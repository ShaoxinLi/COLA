use cola::{adapters::stores::JsonlSessionStore, app::*, domain::*};
use serde_json::{Map, Value, json};
use std::{
    cell::RefCell,
    collections::{BTreeMap, BTreeSet, VecDeque},
    num::{NonZeroU32, NonZeroU64, NonZeroUsize},
    rc::Rc,
};

fn text(value: impl Into<String>) -> NonEmptyText {
    NonEmptyText::new(value).unwrap()
}
fn user(value: &str) -> UserMessage {
    UserMessage::new(text(value))
}
fn tool() -> ToolDefinition {
    ToolDefinition::new(ToolName::new("lookup").unwrap(), text("Look up a value"), InputSchema::new(json!({"type":"object","properties":{"n":{"type":"integer"}},"required":["n"],"additionalProperties":false})).unwrap())
}
fn model_settings() -> ModelSettings {
    ModelSettings {
        model_id: ModelId::new("requested-model").unwrap(),
        effort: Effort::High,
        max_output_tokens: NonZeroU32::new(1024).unwrap(),
    }
}
fn limits(bytes: usize) -> ContextLimits {
    ContextLimits {
        input_tokens: NonZeroU64::new(1_000_000).unwrap(),
        request_bytes: NonZeroUsize::new(bytes).unwrap(),
    }
}
fn settings(max: u32) -> AgentSettings {
    let role = |instructions| RoleSettings {
        model: model_settings(),
        instructions: text(instructions),
        limits: limits(1_000_000),
    };
    AgentSettings {
        assistant: role("Assistant exact instructions"),
        reviewer: role("Reviewer exact instructions"),
        max_iterations: NonZeroU32::new(max).unwrap(),
    }
}
fn call(n: u64, name: &str) -> ToolCall {
    ToolCall::new(
        ToolCallId::new(format!("call-{n}")).unwrap(),
        ToolName::new(name).unwrap(),
        json!({"n":n}),
    )
    .unwrap()
}
fn batch(calls: Vec<ToolCall>) -> Result<Generation<AssistantMessage>, ModelError> {
    Ok(Generation::Complete(AssistantMessage::tool_requests(
        Some("exact preamble".into()),
        ToolCalls::new(calls).unwrap(),
    )))
}
fn answer(value: &str) -> Result<Generation<AssistantMessage>, ModelError> {
    Ok(Generation::Complete(
        AssistantMessage::answer(value).unwrap(),
    ))
}
fn decision(allow: bool) -> Result<Generation<Decision>, ModelError> {
    Ok(Generation::Complete(Decision::new(
        allow,
        Some(text("use read only")),
    )))
}
fn storage_error() -> SessionStoreError {
    SessionStoreError::new(
        SessionStoreErrorKind::Storage,
        std::io::Error::other("injected publication failure"),
    )
}
fn missing_error() -> SessionStoreError {
    SessionStoreError::new(
        SessionStoreErrorKind::NotFound,
        std::io::Error::other("missing"),
    )
}

#[derive(Clone, Copy, Debug)]
enum Fault {
    NotPublished,
    UncertainUnpublished,
    UncertainPublished,
    RepairablePublished,
    Conflict,
}
#[derive(Default)]
struct StoreState {
    bytes: BTreeMap<SessionId, Vec<u8>>,
    owned: BTreeSet<SessionId>,
    attempts: usize,
    resolves: usize,
    fault: Option<(usize, Fault)>,
    list_fault: bool,
    attempted: Vec<SessionArchive>,
}
#[derive(Clone, Default)]
struct Store(Rc<RefCell<StoreState>>);
impl Store {
    fn inject(&self, attempt: usize, fault: Fault) {
        self.0.borrow_mut().fault = Some((attempt, fault));
    }
    fn clear_fault(&self) {
        self.0.borrow_mut().fault = None;
    }
    fn bytes(&self, id: &SessionId) -> Vec<u8> {
        self.0.borrow().bytes[id].clone()
    }
    fn seed(&self, archive: SessionArchive) -> SessionId {
        let change = ArchiveChange::initial(archive).unwrap();
        let id = change.candidate().session().id().clone();
        self.create(&change).unwrap();
        id
    }
    fn publication(&self, change: &ArchiveChange) -> Result<(), CommitFault> {
        let mut state = self.0.borrow_mut();
        state.attempts += 1;
        state.attempted.push(change.candidate().clone());
        let fault = state
            .fault
            .filter(|(n, _)| *n == state.attempts)
            .map(|(_, fault)| fault);
        if !matches!(
            fault,
            Some(Fault::NotPublished | Fault::UncertainUnpublished | Fault::Conflict)
        ) {
            state.bytes.insert(
                change.candidate().session().id().clone(),
                change.encoded().to_vec(),
            );
        }
        match fault {
            Some(Fault::NotPublished) => Err(CommitFault::NotPublished(storage_error())),
            Some(
                Fault::UncertainUnpublished
                | Fault::UncertainPublished
                | Fault::RepairablePublished,
            ) => Err(CommitFault::Uncertain(storage_error())),
            Some(Fault::Conflict) => Err(CommitFault::Conflict),
            None => Ok(()),
        }
    }
}
impl SessionStore for Store {
    fn list(&self) -> Result<Vec<SessionArchive>, SessionStoreError> {
        let state = self.0.borrow();
        if state.list_fault {
            return Err(storage_error());
        }
        state
            .bytes
            .values()
            .map(|bytes| SessionArchive::decode(bytes))
            .collect()
    }
    fn inspect(&self, id: &SessionId) -> Result<SessionArchive, SessionStoreError> {
        SessionArchive::decode(self.0.borrow().bytes.get(id).ok_or_else(missing_error)?)
    }
    fn create(&self, change: &ArchiveChange) -> Result<(), CommitFault> {
        if change.base_key().is_some()
            || self
                .0
                .borrow()
                .bytes
                .contains_key(change.candidate().session().id())
        {
            return Err(CommitFault::Conflict);
        }
        self.publication(change)
    }
    fn acquire(
        &self,
        id: &SessionId,
    ) -> Result<Box<dyn SessionTransaction + '_>, SessionStoreError> {
        if self.0.borrow().owned.contains(id) {
            return Err(SessionStoreError::new(
                SessionStoreErrorKind::Busy,
                std::io::Error::other("owned"),
            ));
        }
        let confirmed = self.inspect(id)?;
        self.0.borrow_mut().owned.insert(id.clone());
        Ok(Box::new(Transaction {
            store: self.clone(),
            confirmed,
        }))
    }
}
struct Transaction {
    store: Store,
    confirmed: SessionArchive,
}
impl Drop for Transaction {
    fn drop(&mut self) {
        self.store
            .0
            .borrow_mut()
            .owned
            .remove(self.confirmed.session().id());
    }
}
impl SessionTransaction for Transaction {
    fn confirmed(&self) -> &SessionArchive {
        &self.confirmed
    }
    fn commit(&mut self, change: &ArchiveChange) -> Result<(), CommitFault> {
        let current = self.store.bytes(self.confirmed.session().id());
        if current != self.confirmed.encode().map_err(CommitFault::NotPublished)?
            || change.base_key().map(|key| key.revision()) != Some(self.confirmed.revision())
        {
            return Err(CommitFault::Conflict);
        }
        self.store.publication(change)?;
        self.confirmed =
            SessionArchive::decode(change.encoded()).map_err(CommitFault::NotPublished)?;
        Ok(())
    }
    fn resolve(&mut self, change: &ArchiveChange) -> Result<(), CommitFault> {
        self.store.0.borrow_mut().resolves += 1;
        let current = self.store.bytes(self.confirmed.session().id());
        if current == change.encoded() {
            if matches!(
                self.store.0.borrow().fault,
                Some((_, Fault::RepairablePublished))
            ) {
                self.confirmed =
                    SessionArchive::decode(&current).map_err(CommitFault::NotPublished)?;
                Ok(())
            } else {
                Err(CommitFault::Uncertain(storage_error()))
            }
        } else if current == self.confirmed.encode().map_err(CommitFault::NotPublished)? {
            Err(CommitFault::NotPublished(storage_error()))
        } else {
            Err(CommitFault::Conflict)
        }
    }
}

#[derive(Clone, Debug, Default, PartialEq, Eq)]
struct Io {
    assistant: usize,
    reviewer: usize,
    targets: Vec<u64>,
}
type Script<O> = VecDeque<Result<Generation<O>, ModelError>>;
#[derive(Clone)]
struct FakeModel {
    assistant: Rc<RefCell<Script<AssistantMessage>>>,
    reviewer: Rc<RefCell<Script<Decision>>>,
    assistant_inputs: Rc<RefCell<Vec<Value>>>,
    reviewer_inputs: Rc<RefCell<Vec<Value>>>,
    io: Rc<RefCell<Io>>,
    store: Store,
}
fn prepare<O>(
    plan: &ContextPlan<O>,
    settings: &ModelSettings,
) -> Result<PreparedRequest<O>, ModelError> {
    let mandatory = match plan.mandatory() {
        Mandatory::Assistant { user, steps } => {
            json!({"user":user,"native":steps.iter().map(|step| json!({"source":step.artifact().source().get(),"capture":format!("{:?}",step.artifact().capture()),"results":step.results().iter().map(|result| json!({"id":result.call_id().as_str(),"outcome":format!("{:?}",result.outcome())})).collect::<Vec<_>>()})).collect::<Vec<_>>()})
        }
        Mandatory::Reviewer { facts } => json!({"facts":facts}),
    };
    let catalog: Vec<_> = plan.tools().iter().map(|tool| json!({"name":tool.name().as_str(),"description":tool.description().as_str(),"schema":tool.input_schema().document()})).collect();
    let fitted = plan.fit(|selection| json!({"instructions":plan.instructions().as_str(),"catalog":catalog,"callable_tools":[],"history":selection.history.iter().map(|group| json!({"user":group.user(),"record":group.record()})).collect::<Vec<_>>(),"notice":selection.notice,"mandatory":mandatory}))?;
    Ok(PreparedRequest::new(
        ModelTarget::new("fixture-protocol", "memory://credential-free"),
        settings.clone(),
        fitted,
    ))
}
impl FakeModel {
    fn assert_committed_intent<O>(&self, request: &PreparedRequest<O>, assistant: bool) {
        let archives = self.store.list().unwrap();
        let pending: Vec<_> = archives
            .iter()
            .flat_map(SessionArchive::generations)
            .filter(|record| matches!(record.observation(), Observation::Pending))
            .collect();
        assert_eq!(
            pending.len(),
            1,
            "exactly one intent must be durable before invocation"
        );
        let intent = pending[0].intent();
        assert_eq!(intent.request(), request.body());
        assert_eq!(intent.settings(), request.settings());
        assert_eq!(intent.target(), request.target());
        assert_eq!(intent.budget(), request.budget());
        assert_eq!(
            matches!(intent.role(), InvocationRole::Assistant { .. }),
            assistant
        );
    }
    fn receipt(&self, role: &str) -> ResponseReceipt {
        ResponseReceipt {
            http_status: Some(200),
            served_model: Some("served-model".into()),
            response_id: Some(format!("{role}-response")),
            request_id: Some(format!("{role}-request")),
            finish: Some("finished".into()),
            usage: Some(Usage {
                input_tokens: Some(17),
                output_tokens: Some(9),
            }),
            capture: ResponseCapture::Complete(format!("raw {role} response").into_bytes()),
        }
    }
}
impl ModelClient<AssistantMessage> for FakeModel {
    fn prepare(
        &self,
        plan: &ContextPlan<AssistantMessage>,
        settings: &ModelSettings,
    ) -> Result<PreparedRequest<AssistantMessage>, ModelError> {
        prepare(plan, settings)
    }
    fn invoke(
        &self,
        request: &PreparedRequest<AssistantMessage>,
    ) -> ObservedReply<AssistantMessage> {
        self.assert_committed_intent(request, true);
        self.io.borrow_mut().assistant += 1;
        self.assistant_inputs
            .borrow_mut()
            .push(request.body().clone());
        ObservedReply::new(
            self.receipt("assistant"),
            self.assistant
                .borrow_mut()
                .pop_front()
                .expect("assistant script exhausted"),
        )
    }
}
impl ModelClient<Decision> for FakeModel {
    fn prepare(
        &self,
        plan: &ContextPlan<Decision>,
        settings: &ModelSettings,
    ) -> Result<PreparedRequest<Decision>, ModelError> {
        prepare(plan, settings)
    }
    fn invoke(&self, request: &PreparedRequest<Decision>) -> ObservedReply<Decision> {
        self.assert_committed_intent(request, false);
        self.io.borrow_mut().reviewer += 1;
        self.reviewer_inputs
            .borrow_mut()
            .push(request.body().clone());
        ObservedReply::new(
            self.receipt("reviewer"),
            self.reviewer
                .borrow_mut()
                .pop_front()
                .expect("reviewer script exhausted"),
        )
    }
}
struct Target {
    io: Rc<RefCell<Io>>,
    fail: bool,
    store: Store,
}
impl Tool for Target {
    fn execute(&self, arguments: &Map<String, Value>) -> ToolOutcome {
        let n = arguments["n"].as_u64().unwrap();
        let archive = self
            .store
            .list()
            .unwrap()
            .into_iter()
            .find(|archive| archive.session().active_turn().is_some())
            .unwrap();
        let started: Vec<_> = archive
            .session()
            .active_turn()
            .unwrap()
            .steps()
            .iter()
            .flat_map(|step| step.executions())
            .filter(|execution| matches!(execution.state(), ExecutionState::Started { .. }))
            .collect();
        assert_eq!(
            started.len(),
            1,
            "dispatch intent must be committed before the effect"
        );
        assert_eq!(started[0].call().arguments(), arguments);
        self.io.borrow_mut().targets.push(n);
        if self.fail {
            ToolOutcome::Failure(ToolError::new(text("actual target failure")))
        } else {
            ToolOutcome::Success(format!("actual lookup result {n}"))
        }
    }
}
struct Fixture {
    store: Store,
    model: FakeModel,
}
impl Fixture {
    fn new(
        assistant: Vec<Result<Generation<AssistantMessage>, ModelError>>,
        reviewer: Vec<Result<Generation<Decision>, ModelError>>,
    ) -> Self {
        let store = Store::default();
        Self {
            model: FakeModel {
                assistant: Rc::new(RefCell::new(assistant.into())),
                reviewer: Rc::new(RefCell::new(reviewer.into())),
                assistant_inputs: Default::default(),
                reviewer_inputs: Default::default(),
                io: Default::default(),
                store: store.clone(),
            },
            store,
        }
    }
    fn service(&self, settings: AgentSettings, fail: bool) -> AgentService<FakeModel, Store> {
        AgentService::new(
            self.model.clone(),
            self.store.clone(),
            vec![(
                tool(),
                Box::new(Target {
                    io: self.model.io.clone(),
                    fail,
                    store: self.store.clone(),
                }),
            )],
            settings,
        )
        .unwrap()
    }
    fn basic(&self, max: u32) -> AgentService<FakeModel, Store> {
        self.service(settings(max), false)
    }
}
fn results(messages: &[Message]) -> Vec<ToolResult> {
    messages
        .iter()
        .filter_map(|message| match message {
            Message::Tool(result) => Some(result.clone()),
            _ => None,
        })
        .collect()
}
fn facts(body: &Value) -> Value {
    let value = body["mandatory"]["facts"]
        .as_str()
        .unwrap()
        .split_once('\n')
        .unwrap()
        .1;
    serde_json::from_str(value).unwrap()
}

#[test]
fn selective_review_preserves_exact_batch_and_each_siblings_facts() {
    for allowed in [[true, false], [false, true], [false, false], [true, true]] {
        let proposal = AssistantMessage::tool_requests(
            Some("exact preamble".into()),
            ToolCalls::new(vec![call(1, "lookup"), call(2, "lookup")]).unwrap(),
        );
        let f = Fixture::new(
            vec![
                Ok(Generation::Complete(proposal.clone())),
                answer("adapted"),
                answer("second turn"),
            ],
            allowed.into_iter().map(decision).collect(),
        );
        let agent = f.basic(2);
        let archive = agent.new_session().unwrap();
        let id = archive.session().id();
        assert_eq!(f.store.inspect(id).unwrap(), archive);
        let completed = agent.chat(id, user("first request")).unwrap();
        assert_eq!(completed.messages().len(), 5);
        assert_eq!(completed.messages()[1], Message::Assistant(proposal));
        let outcomes = results(completed.messages());
        for (index, result) in outcomes.iter().enumerate() {
            assert_eq!(result.call_id(), call(index as u64 + 1, "lookup").id());
            let expected = if allowed[index] {
                ToolOutcome::Success(format!("actual lookup result {}", index + 1))
            } else {
                ToolOutcome::Failure(ToolError::new(text(
                    "Not executed: tool call denied by review. Reason: use read only",
                )))
            };
            assert_eq!(result.outcome(), &expected);
        }
        assert_eq!(
            f.model.io.borrow().targets,
            allowed
                .iter()
                .enumerate()
                .filter_map(|(i, allow)| allow.then_some(i as u64 + 1))
                .collect::<Vec<_>>()
        );
        let reviewer = f.model.reviewer_inputs.borrow();
        assert_eq!(reviewer.len(), 2);
        let first = facts(&reviewer[0]);
        let second = facts(&reviewer[1]);
        assert_eq!(
            first["candidate"],
            json!({"id":"call-1","name":"lookup","arguments":{"n":1}})
        );
        assert_eq!(
            second["candidate"],
            json!({"id":"call-2","name":"lookup","arguments":{"n":2}})
        );
        assert_eq!(second["active_turn"]["user"], "first request");
        assert_eq!(
            second["active_turn"]["steps"][0]["executions"][1]["state"],
            json!({"kind":"requested"})
        );
        assert_eq!(
            second["active_turn"]["steps"][0]["executions"][0]["state"]["kind"],
            if allowed[0] { "returned" } else { "denied" }
        );
        assert_eq!(reviewer[1]["callable_tools"], json!([]));
        drop(reviewer);
        let next = agent.chat(id, user("next request")).unwrap();
        assert_eq!(
            next.messages(),
            &[
                Message::User(user("next request")),
                Message::Assistant(AssistantMessage::answer("second turn").unwrap())
            ]
        );
        assert_eq!(agent.list_sessions().unwrap()[0].session().turns().len(), 2);
        assert_eq!(f.model.io.borrow().assistant, 3);
    }
}

#[test]
fn no_reason_denial_and_genuine_tool_failure_have_distinct_durable_states() {
    let f = Fixture::new(
        vec![
            batch(vec![call(1, "lookup"), call(2, "lookup")]),
            answer("done"),
        ],
        vec![
            Ok(Generation::Complete(Decision::new(false, None))),
            decision(true),
        ],
    );
    let agent = f.service(settings(2), true);
    let id = agent.new_session().unwrap().session().id().clone();
    let completed = agent.chat(&id, user("request")).unwrap();
    let outcomes = results(completed.messages());
    assert_eq!(
        outcomes[0].outcome(),
        &ToolOutcome::Failure(ToolError::new(text(
            "Not executed: tool call denied by review. Reason: Review denied the call without providing a reason."
        )))
    );
    assert_eq!(
        outcomes[1].outcome(),
        &ToolOutcome::Failure(ToolError::new(text("actual target failure")))
    );
    let archive = f.store.inspect(&id).unwrap();
    let states: Vec<_> = archive.session().turns()[0].steps()[0]
        .executions()
        .map(|e| e.state())
        .collect();
    assert!(matches!(states[0], ExecutionState::Denied(_)));
    assert!(matches!(
        states[1],
        ExecutionState::Returned {
            outcome: ToolOutcome::Failure(_),
            ..
        }
    ));
    assert_eq!(f.model.io.borrow().targets, vec![2]);
    assert_eq!(
        archive.session().turns()[0].outcome(),
        &TurnOutcome::Completed
    );
}

#[test]
fn assistant_attempt_limit_drains_each_accepted_call_before_interruption() {
    let f = Fixture::new(
        vec![
            batch(vec![call(1, "lookup"), call(2, "lookup")]),
            answer("next turn"),
        ],
        vec![decision(false), decision(true)],
    );
    let agent = f.basic(1);
    let id = agent.new_session().unwrap().session().id().clone();
    let error = agent.chat(&id, user("request")).unwrap_err();
    assert!(matches!(error.kind(), AgentErrorKind::IterationLimit));
    let durable = error.durable().unwrap();
    assert_eq!(
        durable.session().turns()[0].outcome(),
        &TurnOutcome::Interrupted(StopReason::IterationLimit)
    );
    assert_eq!(results(&durable.session().messages()).len(), 2);
    assert_eq!(f.store.inspect(&id).unwrap(), *durable);
    assert_eq!(
        *f.model.io.borrow(),
        Io {
            assistant: 1,
            reviewer: 2,
            targets: vec![2]
        }
    );
    assert!(agent.chat(&id, user("follow up")).is_ok());
}

#[test]
fn publication_faults_at_each_effect_gate_expose_only_confirmed_progress() {
    for attempt in 2..=10 {
        for fault in [
            Fault::NotPublished,
            Fault::UncertainUnpublished,
            Fault::UncertainPublished,
            Fault::Conflict,
        ] {
            let f = Fixture::new(
                vec![batch(vec![call(1, "lookup")]), answer("done")],
                vec![decision(true)],
            );
            let agent = f.basic(2);
            let id = agent.new_session().unwrap().session().id().clone();
            f.store.inject(attempt, fault);
            let error = agent.chat(&id, user("request")).unwrap_err();
            match fault {
                Fault::NotPublished | Fault::UncertainUnpublished => assert!(matches!(
                    error.kind(),
                    AgentErrorKind::Commit(CommitFault::NotPublished(_))
                )),
                Fault::UncertainPublished => assert!(matches!(
                    error.kind(),
                    AgentErrorKind::Commit(CommitFault::Uncertain(_))
                )),
                Fault::Conflict => assert!(matches!(
                    error.kind(),
                    AgentErrorKind::Commit(CommitFault::Conflict)
                )),
                Fault::RepairablePublished => unreachable!(),
            }
            let committed = error.durable().unwrap();
            let candidate = error.uncommitted().unwrap();
            assert_eq!(committed.revision().get(), attempt as u64 - 1);
            assert_eq!(candidate.revision().get(), attempt as u64);
            let destination = agent.inspect(&id).unwrap();
            assert_eq!(
                destination,
                if matches!(fault, Fault::UncertainPublished) {
                    candidate.clone()
                } else {
                    committed.clone()
                }
            );
            assert_eq!(agent.list_sessions().unwrap(), vec![destination]);
            let expected_assistant = usize::from(attempt > 3) + usize::from(attempt > 9);
            assert_eq!(
                f.model.io.borrow().assistant,
                expected_assistant,
                "attempt {attempt} {fault:?}"
            );
            assert_eq!(f.model.io.borrow().reviewer, usize::from(attempt > 5));
            assert_eq!(
                f.model.io.borrow().targets,
                if attempt > 7 { vec![1] } else { vec![] }
            );
            assert_eq!(
                f.store.0.borrow().resolves,
                usize::from(matches!(
                    fault,
                    Fault::UncertainPublished | Fault::UncertainUnpublished
                ))
            );
        }
    }
}

#[test]
fn owned_uncertainty_repair_allows_exact_candidate_once_without_repeating_effects() {
    for attempt in 2..=10 {
        let f = Fixture::new(
            vec![batch(vec![call(1, "lookup")]), answer("done")],
            vec![decision(true)],
        );
        let agent = f.basic(2);
        let id = agent.new_session().unwrap().session().id().clone();
        f.store.inject(attempt, Fault::RepairablePublished);
        let completed = agent.chat(&id, user("request")).unwrap();
        assert_eq!(
            completed.archive().session().turns()[0].outcome(),
            &TurnOutcome::Completed
        );
        assert_eq!(
            *f.model.io.borrow(),
            Io {
                assistant: 2,
                reviewer: 1,
                targets: vec![1]
            }
        );
        assert_eq!(f.store.0.borrow().resolves, 1);
        assert_eq!(agent.inspect(&id).unwrap(), *completed.archive());
    }
}

#[test]
fn unsaved_answer_and_tool_return_remain_separate_from_disk_and_never_reexecute() {
    for (attempt, returns) in [(8, true), (10, false)] {
        let f = Fixture::new(
            vec![
                batch(vec![call(1, "lookup")]),
                answer("done"),
                answer("after recovery"),
            ],
            vec![decision(true)],
        );
        let agent = f.basic(2);
        let id = agent.new_session().unwrap().session().id().clone();
        f.store.inject(attempt, Fault::NotPublished);
        let error = agent.chat(&id, user("request")).unwrap_err();
        let disk = f.store.inspect(&id).unwrap();
        assert_eq!(error.durable(), Some(&disk));
        if returns {
            assert!(matches!(
                disk.session().active_turn().unwrap().steps()[0]
                    .executions()
                    .next()
                    .unwrap()
                    .state(),
                ExecutionState::Started { .. }
            ));
            assert!(matches!(
                error
                    .uncommitted()
                    .unwrap()
                    .session()
                    .active_turn()
                    .unwrap()
                    .steps()[0]
                    .executions()
                    .next()
                    .unwrap()
                    .state(),
                ExecutionState::Returned { .. }
            ));
        } else {
            assert!(disk.session().active_turn().is_some());
            assert!(
                error
                    .uncommitted()
                    .unwrap()
                    .session()
                    .active_turn()
                    .is_none()
            );
            assert_eq!(
                error.uncommitted().unwrap().session().turns()[0]
                    .steps()
                    .last()
                    .unwrap()
                    .assistant(),
                &AssistantMessage::answer("done").unwrap()
            );
        }
        let io = f.model.io.borrow().clone();
        assert!(matches!(
            agent.chat(&id, user("must recover")).unwrap_err().kind(),
            AgentErrorKind::RecoveryRequired
        ));
        assert_eq!(*f.model.io.borrow(), io);
        f.store.clear_fault();
        let recovered = agent.recover(&id).unwrap();
        assert_eq!(*f.model.io.borrow(), io);
        assert!(!recovered.archive().needs_recovery());
        assert_eq!(agent.recover(&id).unwrap().archive(), recovered.archive());
        agent.chat(&id, user("new request")).unwrap();
        assert_eq!(f.model.io.borrow().targets, vec![1]);
    }
}

#[test]
fn failed_denial_commit_never_dispatches_denied_target_or_later_sibling() {
    let f = Fixture::new(
        vec![batch(vec![call(1, "lookup"), call(2, "lookup")])],
        vec![decision(false)],
    );
    let agent = f.basic(2);
    let id = agent.new_session().unwrap().session().id().clone();
    f.store.inject(6, Fault::NotPublished);
    let error = agent.chat(&id, user("request")).unwrap_err();
    assert!(matches!(
        error.kind(),
        AgentErrorKind::Commit(CommitFault::NotPublished(_))
    ));
    assert!(f.model.io.borrow().targets.is_empty());
    let states: Vec<_> = error
        .durable()
        .unwrap()
        .session()
        .active_turn()
        .unwrap()
        .steps()[0]
        .executions()
        .map(|e| e.state())
        .collect();
    assert!(
        states
            .iter()
            .all(|s| matches!(s, ExecutionState::Requested))
    );
    assert!(matches!(
        error
            .uncommitted()
            .unwrap()
            .session()
            .active_turn()
            .unwrap()
            .steps()[0]
            .executions()
            .next()
            .unwrap()
            .state(),
        ExecutionState::Denied(_)
    ));
}

#[test]
fn reviewer_stopped_and_failed_attempts_record_evidence_without_dispatch_intent() {
    for stopped in [
        Err(ModelError::new(ModelErrorKind::Transport)),
        Err(ModelError::new(ModelErrorKind::MalformedResponse)),
        Ok(Generation::Refused {
            reason: Some(text("refused")),
        }),
        Ok(Generation::Incomplete {
            reason: IncompleteReason::OutputLimit,
        }),
    ] {
        let f = Fixture::new(
            vec![
                batch(vec![call(1, "lookup"), call(2, "lookup")]),
                answer("next"),
            ],
            vec![stopped],
        );
        let agent = f.basic(2);
        let id = agent.new_session().unwrap().session().id().clone();
        let error = agent.chat(&id, user("request")).unwrap_err();
        assert!(matches!(
            error.kind(),
            AgentErrorKind::Model {
                role: ModelRole::Reviewer,
                ..
            } | AgentErrorKind::Refused {
                role: ModelRole::Reviewer,
                ..
            } | AgentErrorKind::Incomplete {
                role: ModelRole::Reviewer,
                ..
            }
        ));
        let archive = f.store.inspect(&id).unwrap();
        assert!(archive.session().active_turn().is_none());
        assert_eq!(archive.generations().len(), 2);
        assert!(matches!(
            archive.generations()[1].intent().role(),
            InvocationRole::Reviewer { .. }
        ));
        assert!(
            matches!(archive.generations()[1].observation(),Observation::Observed{receipt:ResponseReceipt{capture:ResponseCapture::Complete(bytes),..},disposition:Disposition::Failed(_)|Disposition::Refused{..}|Disposition::Incomplete(_),..} if bytes == b"raw reviewer response")
        );
        assert!(
            archive.session().turns()[0].steps()[0]
                .executions()
                .all(|e| matches!(e.state(), ExecutionState::Requested))
        );
        assert_eq!(
            *f.model.io.borrow(),
            Io {
                assistant: 1,
                reviewer: 1,
                targets: vec![]
            }
        );
        assert_eq!(agent.recover(&id).unwrap().archive(), &archive);
        agent.chat(&id, user("next request")).unwrap();
        assert_eq!(f.model.io.borrow().targets, Vec::<u64>::new());
    }
}

#[test]
fn assistant_failures_close_and_save_then_allow_another_chat() {
    for stopped in [
        Err(ModelError::http(ModelErrorKind::Authentication, 401)),
        Err(ModelError::new(ModelErrorKind::Timeout)),
        Err(ModelError::new(ModelErrorKind::MalformedResponse)),
        Ok(Generation::Refused { reason: None }),
        Ok(Generation::Incomplete {
            reason: IncompleteReason::Other,
        }),
    ] {
        let f = Fixture::new(vec![stopped, answer("recovered")], vec![]);
        let agent = f.basic(2);
        let id = agent.new_session().unwrap().session().id().clone();
        let error = agent.chat(&id, user("request")).unwrap_err();
        let archive = error.durable().unwrap();
        assert!(archive.session().active_turn().is_none());
        assert_eq!(archive.generations().len(), 1);
        assert_eq!(f.store.inspect(&id).unwrap(), *archive);
        assert_eq!(f.model.io.borrow().assistant, 1);
        assert_eq!(
            agent
                .chat(&id, user("try another turn"))
                .unwrap()
                .messages()
                .last(),
            Some(&Message::Assistant(
                AssistantMessage::answer("recovered").unwrap()
            ))
        );
        assert_eq!(f.model.io.borrow().assistant, 2);
    }
}

#[test]
fn unknown_and_reused_active_calls_keep_raw_rejected_output_without_effects() {
    for duplicate in [false, true] {
        let rejected = if duplicate {
            call(1, "lookup")
        } else {
            call(99, "unadvertised")
        };
        let f = Fixture::new(
            vec![
                batch(vec![call(1, "lookup")]),
                batch(vec![rejected]),
                answer("new turn"),
            ],
            vec![decision(true)],
        );
        let agent = f.basic(3);
        let id = agent.new_session().unwrap().session().id().clone();
        let error = agent.chat(&id, user("request")).unwrap_err();
        assert!(matches!(error.kind(), AgentErrorKind::RejectedOutput(_)));
        let archive = error.durable().unwrap();
        assert_eq!(archive.session().turns()[0].steps().len(), 1);
        assert_eq!(archive.generations().len(), 3);
        assert!(
            matches!(archive.generations()[2].observation(),Observation::Observed{disposition:Disposition::Rejected(_),receipt:ResponseReceipt{capture:ResponseCapture::Complete(bytes),..},..} if bytes == b"raw assistant response")
        );
        assert_eq!(f.model.io.borrow().targets, vec![1]);
        agent.chat(&id, user("new request")).unwrap();
        assert_eq!(f.model.io.borrow().targets, vec![1]);
        let last = f.model.assistant_inputs.borrow().last().unwrap().clone();
        assert_eq!(last["mandatory"]["native"], json!([]));
        assert_eq!(last["history"].as_array().unwrap().len(), 1);
    }
}

#[test]
fn complete_invocation_provenance_is_monotonic_and_roundtrips_public_store_codec() {
    let f = Fixture::new(
        vec![batch(vec![call(1, "lookup")]), answer("done")],
        vec![decision(false)],
    );
    let agent = f.basic(2);
    let id = agent.new_session().unwrap().session().id().clone();
    let completed = agent.chat(&id, user("request exact whitespace  ")).unwrap();
    let archive = SessionArchive::decode(&f.store.bytes(&id)).unwrap();
    assert_eq!(&archive, completed.archive());
    assert_eq!(
        archive
            .generations()
            .iter()
            .map(|r| r.id().get())
            .collect::<Vec<_>>(),
        vec![1, 2, 3]
    );
    for record in archive.generations() {
        let intent = record.intent();
        assert_eq!(
            intent.target(),
            &ModelTarget::new("fixture-protocol", "memory://credential-free")
        );
        assert_eq!(intent.settings(), &model_settings());
        assert_eq!(
            intent.budget().request_bytes(),
            serde_json::to_vec(intent.request()).unwrap().len()
        );
        assert_eq!(intent.budget().method(), TOKEN_ESTIMATE_METHOD);
        let Observation::Observed {
            at,
            receipt,
            disposition,
        } = record.observation()
        else {
            panic!("missing observation")
        };
        assert!(*at >= intent.at());
        assert_eq!(*disposition, Disposition::Accepted);
        assert_eq!(receipt.served_model.as_deref(), Some("served-model"));
        assert_eq!(receipt.http_status, Some(200));
        assert_eq!(receipt.finish.as_deref(), Some("finished"));
        assert_eq!(
            receipt.usage,
            Some(Usage {
                input_tokens: Some(17),
                output_tokens: Some(9)
            })
        );
    }
    assert_eq!(
        archive.session().turns()[0].steps()[0].source_generation(),
        archive.generations()[0].id()
    );
    assert_eq!(
        archive.session().turns()[0].steps()[0]
            .executions()
            .next()
            .unwrap()
            .state()
            .review()
            .unwrap()
            .reviewer(),
        archive.generations()[1].id()
    );
    let encoded = String::from_utf8(f.store.bytes(&id)).unwrap();
    assert!(!encoded.contains("\"version\""));
    assert!(!encoded.contains("\"format\""));
}

#[test]
fn per_role_capacity_checks_prevent_http_and_leave_exact_typed_closure() {
    for role in [ModelRole::Assistant, ModelRole::Reviewer] {
        let f = Fixture::new(vec![batch(vec![call(1, "lookup")])], vec![]);
        let mut config = settings(2);
        match role {
            ModelRole::Assistant => config.assistant.limits = limits(1),
            ModelRole::Reviewer => config.reviewer.limits = limits(1),
        }
        let agent = f.service(config, false);
        let id = agent.new_session().unwrap().session().id().clone();
        let error = agent.chat(&id, user("request")).unwrap_err();
        assert!(matches!(error.kind(),AgentErrorKind::ContextCapacity{role:r} if *r==role));
        let durable = error.durable().unwrap();
        assert_eq!(
            durable.session().turns()[0].outcome(),
            &TurnOutcome::Interrupted(StopReason::ContextCapacity)
        );
        assert_eq!(
            f.model.io.borrow().assistant,
            usize::from(role == ModelRole::Reviewer)
        );
        assert_eq!(f.model.io.borrow().reviewer, 0);
        assert!(f.model.io.borrow().targets.is_empty());
        assert_eq!(
            durable.generations().len(),
            usize::from(role == ModelRole::Reviewer)
        );
    }
}

#[test]
fn duplicate_registration_listing_fault_and_missing_inspection_do_not_poison_other_sessions() {
    let f = Fixture::new(vec![answer("usable")], vec![]);
    let duplicate = AgentService::new(
        f.model.clone(),
        f.store.clone(),
        vec![
            (
                tool(),
                Box::new(Target {
                    io: f.model.io.clone(),
                    fail: false,
                    store: f.store.clone(),
                }) as Box<dyn Tool>,
            ),
            (
                tool(),
                Box::new(Target {
                    io: f.model.io.clone(),
                    fail: false,
                    store: f.store.clone(),
                }) as Box<dyn Tool>,
            ),
        ],
        settings(2),
    );
    assert!(matches!(
        duplicate.err().unwrap().kind(),
        AgentErrorKind::DuplicateTool(_)
    ));
    for id in ["z", "a"] {
        f.store
            .seed(SessionArchive::new(SessionId::new(id).unwrap()));
    }
    let agent = f.basic(2);
    assert_eq!(
        agent
            .list_sessions()
            .unwrap()
            .iter()
            .map(|s| s.session().id().as_str())
            .collect::<Vec<_>>(),
        vec!["a", "z"]
    );
    let missing = SessionId::new("missing").unwrap();
    assert!(
        matches!(agent.inspect(&missing).unwrap_err().kind(),AgentErrorKind::Store(source)if source.kind()==SessionStoreErrorKind::NotFound)
    );
    f.store.0.borrow_mut().list_fault = true;
    assert!(matches!(
        agent.list_sessions().unwrap_err().kind(),
        AgentErrorKind::Store(_)
    ));
    f.store.0.borrow_mut().list_fault = false;
    agent
        .chat(&SessionId::new("a").unwrap(), user("request"))
        .unwrap();
    assert_eq!(f.model.io.borrow().assistant, 1);
}

#[test]
fn owned_session_rejects_chat_and_recovery_before_any_effect_and_releases_normally() {
    let f = Fixture::new(vec![answer("after release")], vec![]);
    let agent = f.basic(2);
    let id = agent.new_session().unwrap().session().id().clone();
    let transaction = f.store.acquire(&id).unwrap();
    for error in [
        agent.chat(&id, user("busy")).unwrap_err(),
        agent.recover(&id).unwrap_err(),
    ] {
        assert!(
            matches!(error.kind(),AgentErrorKind::Store(source)if source.kind()==SessionStoreErrorKind::Busy)
        );
    }
    assert_eq!(*f.model.io.borrow(), Io::default());
    drop(transaction);
    agent.chat(&id, user("released")).unwrap();
    assert_eq!(f.model.io.borrow().assistant, 1);
}

#[test]
fn new_session_publication_failure_is_typed_and_has_no_effect() {
    for fault in [
        Fault::NotPublished,
        Fault::UncertainPublished,
        Fault::Conflict,
    ] {
        let f = Fixture::new(vec![], vec![]);
        f.store.inject(1, fault);
        let agent = f.basic(1);
        let error = agent.new_session().unwrap_err();
        assert!(matches!(error.kind(), AgentErrorKind::Commit(_)));
        assert!(error.durable().is_none());
        assert_eq!(error.uncommitted().unwrap().revision().get(), 1);
        assert_eq!(*f.model.io.borrow(), Io::default());
        assert_eq!(
            agent.list_sessions().unwrap().len(),
            usize::from(matches!(fault, Fault::UncertainPublished))
        );
    }
}

#[allow(dead_code)]
#[path = "../adapters/models/support.rs"]
mod support;
use support::{PROVIDERS, Provider, Server};

struct ProviderClients {
    assistant: Box<dyn ModelClient<AssistantMessage>>,
    reviewer: Box<dyn ModelClient<Decision>>,
}
impl ModelClient<AssistantMessage> for ProviderClients {
    fn prepare(
        &self,
        plan: &ContextPlan<AssistantMessage>,
        settings: &ModelSettings,
    ) -> Result<PreparedRequest<AssistantMessage>, ModelError> {
        self.assistant.prepare(plan, settings)
    }
    fn invoke(
        &self,
        request: &PreparedRequest<AssistantMessage>,
    ) -> ObservedReply<AssistantMessage> {
        self.assistant.invoke(request)
    }
}
impl ModelClient<Decision> for ProviderClients {
    fn prepare(
        &self,
        plan: &ContextPlan<Decision>,
        settings: &ModelSettings,
    ) -> Result<PreparedRequest<Decision>, ModelError> {
        self.reviewer.prepare(plan, settings)
    }
    fn invoke(&self, request: &PreparedRequest<Decision>) -> ObservedReply<Decision> {
        self.reviewer.invoke(request)
    }
}
fn native_batch(provider: Provider) -> Value {
    let mut response = provider.tool(1);
    let second = provider.tool(2);
    let key = match provider {
        Provider::OpenAI => "output",
        Provider::Anthropic => "content",
    };
    response[key]
        .as_array_mut()
        .unwrap()
        .push(second[key].as_array().unwrap().last().unwrap().clone());
    response
}
struct NativeTarget {
    io: Rc<RefCell<Io>>,
    store: JsonlSessionStore,
}
impl Tool for NativeTarget {
    fn execute(&self, arguments: &Map<String, Value>) -> ToolOutcome {
        let n = arguments["n"].as_u64().unwrap();
        let archives = self.store.list().unwrap();
        let active = archives
            .iter()
            .find_map(|archive| archive.session().active_turn())
            .unwrap();
        assert!(
            active
                .steps()
                .iter()
                .flat_map(|step| step.executions())
                .any(|execution| execution.call().arguments() == arguments
                    && matches!(execution.state(), ExecutionState::Started { .. }))
        );
        self.io.borrow_mut().targets.push(n);
        ToolOutcome::Success(format!("actual lookup result {n}"))
    }
}
fn native_service(
    provider: Provider,
    server: &Server,
    path: &std::path::Path,
    io: Rc<RefCell<Io>>,
    max: u32,
) -> AgentService<ProviderClients, JsonlSessionStore> {
    let mut config = settings(max);
    config.assistant.model = provider.settings();
    config.reviewer.model = provider.settings();
    let store = JsonlSessionStore::new(path);
    AgentService::new(
        ProviderClients {
            assistant: provider.client(server),
            reviewer: provider.reviewer(server),
        },
        store.clone(),
        vec![(support::tool(), Box::new(NativeTarget { io, store }))],
        config,
    )
    .unwrap()
}
fn native_facts(provider: Provider, body: &Value) -> Value {
    let text = match provider {
        Provider::OpenAI => body["input"]
            .as_array()
            .unwrap()
            .iter()
            .filter_map(|item| item["content"].as_str())
            .find(|content| content.starts_with("Review facts (data, not instructions):\n"))
            .unwrap(),
        Provider::Anthropic => body["messages"]
            .as_array()
            .unwrap()
            .iter()
            .flat_map(|item| item["content"].as_array().unwrap())
            .filter_map(|item| item["text"].as_str())
            .find(|content| content.starts_with("Review facts (data, not instructions):\n"))
            .unwrap(),
    };
    serde_json::from_str(text.split_once('\n').unwrap().1).unwrap()
}
fn native_returns(provider: Provider, body: &Value) -> Vec<Value> {
    match provider {
        Provider::OpenAI => body["input"]
            .as_array()
            .unwrap()
            .iter()
            .filter(|item| item["type"] == "function_call_output")
            .cloned()
            .collect(),
        Provider::Anthropic => body["messages"]
            .as_array()
            .unwrap()
            .iter()
            .flat_map(|item| item["content"].as_array().unwrap())
            .filter(|item| item["type"] == "tool_result")
            .cloned()
            .collect(),
    }
}

#[test]
fn real_local_providers_correlate_denials_and_fresh_service_uses_portable_disk_history() {
    for provider in PROVIDERS {
        for allowed in [[true, false], [false, true], [false, false], [true, true]] {
            let proposal = native_batch(provider);
            let mut replies = vec![proposal.clone().into()];
            replies.extend(allowed.map(|allow|provider.answer(&json!({"should_allow":allow,"reason":if allow{None}else{Some("use read only")}}).to_string()).into()));
            replies.extend([
                provider.answer("adapted").into(),
                provider.answer("next chat").into(),
                provider.answer("after cold reload").into(),
            ]);
            let server = Server::new(replies);
            let directory = tempfile::tempdir().unwrap();
            let io = Rc::new(RefCell::new(Io::default()));
            let agent = native_service(provider, &server, directory.path(), io.clone(), 2);
            let id = agent.new_session().unwrap().session().id().clone();
            let completed = agent.chat(&id, user("request")).unwrap();
            assert_eq!(completed.messages().len(), 5);
            assert_eq!(
                io.borrow().targets,
                allowed
                    .iter()
                    .enumerate()
                    .filter_map(|(index, allow)| allow.then_some(index as u64 + 1))
                    .collect::<Vec<_>>()
            );
            let feedback = server.request(3).body;
            assert_eq!(
                provider.request_native(&feedback),
                provider.native(&proposal).as_array().unwrap().clone()
            );
            let expected:Vec<Value>=allowed.iter().enumerate().map(|(index,allow)|{
            let output=if *allow{format!("actual lookup result {}",index+1)}else{"Not executed: tool call denied by review. Reason: use read only".into()};
            match provider{
                Provider::OpenAI=>json!({"type":"function_call_output","call_id":format!("native-{}",index+1),"output":if *allow{json!({"success":true,"output":output})}else{json!({"success":false,"error":output})}.to_string()}),
                Provider::Anthropic=>json!({"type":"tool_result","tool_use_id":format!("native-{}",index+1),"is_error":!*allow,"content":output}),
            }
        }).collect();
            assert_eq!(native_returns(provider, &feedback), expected);
            let review = native_facts(provider, &server.request(2).body);
            assert_eq!(
                review["candidate"],
                json!({"id":"native-2","name":"lookup","arguments":{"n":2}})
            );
            assert_eq!(review["active_turn"]["user"], "request");
            assert_eq!(
                review["active_turn"]["steps"][0]["executions"][1]["state"],
                json!({"kind":"requested"})
            );
            assert_eq!(
                review["active_turn"]["steps"][0]["executions"][0]["state"]["kind"],
                if allowed[0] { "returned" } else { "denied" }
            );
            assert!(server.request(2).body.get("tools").is_none());
            let next = agent.chat(&id, user("follow up")).unwrap();
            assert_eq!(next.messages().len(), 2);
            drop(agent);
            let cold = native_service(provider, &server, directory.path(), io.clone(), 2);
            let last = cold.chat(&id, user("after reload")).unwrap();
            assert_eq!(
                last.messages(),
                &[
                    Message::User(user("after reload")),
                    Message::Assistant(AssistantMessage::answer("after cold reload").unwrap())
                ]
            );
            assert_eq!(server.requests().len(), 6);
            let cold_request = server.request(5).body;
            assert!(native_returns(provider, &cold_request).is_empty());
            match provider {
                Provider::OpenAI => assert!(
                    !cold_request["input"]
                        .as_array()
                        .unwrap()
                        .iter()
                        .any(|item| item["type"] == "function_call" || item["type"] == "reasoning")
                ),
                Provider::Anthropic => assert!(
                    !cold_request["messages"]
                        .as_array()
                        .unwrap()
                        .iter()
                        .flat_map(|item| item["content"].as_array().unwrap())
                        .any(|item| matches!(
                            item["type"].as_str(),
                            Some("tool_use" | "thinking" | "redacted_thinking")
                        ))
                ),
            }
            let persisted = JsonlSessionStore::new(directory.path())
                .inspect(&id)
                .unwrap();
            assert_eq!(persisted.session().turns().len(), 3);
            assert_eq!(
                persisted.session().turns()[0].messages(),
                completed.messages()
            );
            assert_eq!(persisted.generations().len(), 6);
        }
    }
}

#[test]
fn real_local_provider_final_budget_denial_is_durable_and_next_chat_replays_facts() {
    for provider in PROVIDERS {
        let server = Server::new(vec![
            native_batch(provider).into(),
            provider
                .answer(&json!({"should_allow":false,"reason":null}).to_string())
                .into(),
            provider
                .answer(&json!({"should_allow":false,"reason":null}).to_string())
                .into(),
            provider.answer("next chat").into(),
        ]);
        let directory = tempfile::tempdir().unwrap();
        let io = Rc::new(RefCell::new(Io::default()));
        let agent = native_service(provider, &server, directory.path(), io.clone(), 1);
        let id = agent.new_session().unwrap().session().id().clone();
        let error = agent.chat(&id, user("request")).unwrap_err();
        assert!(matches!(error.kind(), AgentErrorKind::IterationLimit));
        assert_eq!(
            error.durable().unwrap().session().turns()[0].outcome(),
            &TurnOutcome::Interrupted(StopReason::IterationLimit)
        );
        assert_eq!(
            results(&error.durable().unwrap().session().messages()).len(),
            2
        );
        assert!(io.borrow().targets.is_empty());
        assert_eq!(server.requests().len(), 3);
        drop(agent);
        let cold = native_service(provider, &server, directory.path(), io.clone(), 1);
        cold.chat(&id, user("follow up")).unwrap();
        let request = server.request(3).body;
        assert!(native_returns(provider, &request).is_empty());
        let encoded = request.to_string();
        assert!(encoded.contains("denied by review"));
        assert!(encoded.contains("iteration_limit"));
        assert_eq!(
            JsonlSessionStore::new(directory.path())
                .inspect(&id)
                .unwrap()
                .session()
                .turns()
                .len(),
            2
        );
    }
}

struct InvalidSecondProposal {
    native: ProviderClients,
    first: RefCell<Option<AssistantMessage>>,
    generations: std::cell::Cell<u32>,
    duplicate: bool,
}
impl ModelClient<AssistantMessage> for InvalidSecondProposal {
    fn prepare(
        &self,
        plan: &ContextPlan<AssistantMessage>,
        settings: &ModelSettings,
    ) -> Result<PreparedRequest<AssistantMessage>, ModelError> {
        self.native.prepare(plan, settings)
    }
    fn invoke(
        &self,
        request: &PreparedRequest<AssistantMessage>,
    ) -> ObservedReply<AssistantMessage> {
        let mut observed = self.native.invoke(request);
        self.generations.set(self.generations.get() + 1);
        if let Ok(Generation::Complete(output)) = &mut observed.result {
            match self.generations.get() {
                1 => *self.first.borrow_mut() = Some(output.clone()),
                2 => {
                    *output = if self.duplicate {
                        self.first.borrow().as_ref().unwrap().clone()
                    } else {
                        AssistantMessage::tool_requests(
                            None,
                            ToolCalls::new(vec![call(99, "unadvertised")]).unwrap(),
                        )
                    }
                }
                _ => {}
            }
        }
        observed
    }
}
impl ModelClient<Decision> for InvalidSecondProposal {
    fn prepare(
        &self,
        plan: &ContextPlan<Decision>,
        settings: &ModelSettings,
    ) -> Result<PreparedRequest<Decision>, ModelError> {
        self.native.prepare(plan, settings)
    }
    fn invoke(&self, request: &PreparedRequest<Decision>) -> ObservedReply<Decision> {
        self.native.invoke(request)
    }
}

#[test]
fn rejected_native_output_stays_audit_only_and_next_chat_uses_accepted_facts() {
    for provider in PROVIDERS {
        for duplicate in [true, false] {
            let first = provider.tool(1);
            let rejected = provider.tool(2);
            let server = Server::new(vec![
                first.clone().into(),
                provider
                    .answer(&json!({"should_allow":true,"reason":null}).to_string())
                    .into(),
                rejected.clone().into(),
                provider.answer("recovered next chat").into(),
            ]);
            let directory = tempfile::tempdir().unwrap();
            let io = Rc::new(RefCell::new(Io::default()));
            let native = ProviderClients {
                assistant: provider.client(&server),
                reviewer: provider.reviewer(&server),
            };
            let mut config = settings(3);
            config.assistant.model = provider.settings();
            config.reviewer.model = provider.settings();
            let store = JsonlSessionStore::new(directory.path());
            let agent = AgentService::new(
                InvalidSecondProposal {
                    native,
                    first: RefCell::new(None),
                    generations: std::cell::Cell::new(0),
                    duplicate,
                },
                store.clone(),
                vec![(
                    support::tool(),
                    Box::new(NativeTarget {
                        io: io.clone(),
                        store: store.clone(),
                    }) as Box<dyn Tool>,
                )],
                config,
            )
            .unwrap();
            let id = agent.new_session().unwrap().session().id().clone();
            let error = agent.chat(&id, user("request")).unwrap_err();
            assert!(matches!(error.kind(), AgentErrorKind::RejectedOutput(_)));
            let archive = error.durable().unwrap();
            assert_eq!(archive.session().turns()[0].steps().len(), 1);
            assert!(matches!(
                archive.session().turns()[0].outcome(),
                TurnOutcome::Failed(TurnFailure::MalformedResponse(_))
            ));
            assert_eq!(io.borrow().targets, vec![1]);
            let Observation::Observed {
                receipt,
                disposition,
                ..
            } = archive.generations()[2].observation()
            else {
                panic!("missing rejected evidence")
            };
            assert!(matches!(disposition, Disposition::Rejected(_)));
            let ResponseCapture::Complete(bytes) = &receipt.capture else {
                panic!("raw response missing")
            };
            assert_eq!(serde_json::from_slice::<Value>(bytes).unwrap(), rejected);
            let next = agent.chat(&id, user("next user request")).unwrap();
            assert_eq!(
                next.messages().last(),
                Some(&Message::Assistant(
                    AssistantMessage::answer("recovered next chat").unwrap()
                ))
            );
            let request = server.request(3).body;
            assert!(native_returns(provider, &request).is_empty());
            assert!(!request.to_string().contains("signature-2"));
            assert!(!request.to_string().contains("native-2"));
            assert!(request.to_string().contains("actual lookup result 1"));
            assert_eq!(io.borrow().targets, vec![1]);
            assert_eq!(store.inspect(&id).unwrap().session().turns().len(), 2);
        }
    }
}

fn interrupted_archive(phase: usize) -> SessionArchive {
    let mut archive =
        SessionArchive::new(SessionId::new(format!("recovery-phase-{phase}")).unwrap());
    let turn = archive
        .session_mut()
        .start_turn(user("original authorization"))
        .unwrap();
    if phase == 0 {
        return archive;
    }
    let plan = ContextPlan::assistant(
        &archive,
        text("assistant"),
        vec![tool()],
        limits(1_000_000),
        &[],
    )
    .unwrap();
    let request = prepare(&plan, &model_settings()).unwrap();
    let source = archive
        .begin_generation(InvocationRole::Assistant { turn }, &request)
        .unwrap();
    if phase == 1 {
        return archive;
    }
    archive
        .record_observation(
            source,
            Timestamp::now(),
            ResponseReceipt {
                capture: ResponseCapture::Complete(b"proposal evidence".to_vec()),
                ..Default::default()
            },
            Disposition::Accepted,
        )
        .unwrap();
    archive
        .session_mut()
        .record_assistant(
            turn,
            source,
            AssistantMessage::tool_requests(
                None,
                ToolCalls::new(vec![call(1, "lookup"), call(2, "lookup")]).unwrap(),
            ),
        )
        .unwrap();
    if phase == 2 {
        return archive;
    }
    let call = CallRef::new(source, call(1, "lookup").id().clone());
    let plan = ContextPlan::reviewer(
        &archive,
        &call,
        text("reviewer"),
        vec![tool()],
        limits(1_000_000),
        &[],
    )
    .unwrap();
    let request = prepare(&plan, &model_settings()).unwrap();
    let reviewer = archive
        .begin_generation(InvocationRole::Reviewer { call: call.clone() }, &request)
        .unwrap();
    if phase == 3 {
        return archive;
    }
    archive
        .record_observation(
            reviewer,
            Timestamp::now(),
            ResponseReceipt {
                capture: ResponseCapture::Complete(b"review evidence".to_vec()),
                ..Default::default()
            },
            Disposition::Accepted,
        )
        .unwrap();
    archive
        .session_mut()
        .review_tool(turn, &call, reviewer, &Decision::new(phase != 4, None))
        .unwrap();
    if phase <= 5 {
        return archive;
    }
    archive
        .session_mut()
        .start_tool(turn, &call, Timestamp::now())
        .unwrap();
    if phase == 6 {
        return archive;
    }
    archive
        .session_mut()
        .record_tool_result(
            turn,
            &call,
            ToolOutcome::Success("actual recorded return".into()),
            Timestamp::now(),
        )
        .unwrap();
    archive
}

#[test]
fn disk_inspection_is_faithful_and_explicit_recovery_closes_once_without_model_or_tool_io() {
    for phase in 0..=7 {
        let directory = tempfile::tempdir().unwrap();
        let store = JsonlSessionStore::new(directory.path());
        let change = ArchiveChange::initial(interrupted_archive(phase)).unwrap();
        let id = change.candidate().session().id().clone();
        store.create(&change).unwrap();
        let snapshot_path = std::fs::read_dir(directory.path())
            .unwrap()
            .map(|entry| entry.unwrap().path())
            .find(|path| {
                path.extension()
                    .is_some_and(|extension| extension == "jsonl")
            })
            .unwrap();
        let original = std::fs::read(&snapshot_path).unwrap();
        let server = Server::new(vec![
            Provider::OpenAI.answer("new turn after recovery").into(),
        ]);
        let io = Rc::new(RefCell::new(Io::default()));
        let agent = native_service(Provider::OpenAI, &server, directory.path(), io.clone(), 2);
        for _ in 0..3 {
            assert_eq!(
                SessionArchive::decode(&original).unwrap(),
                *change.candidate()
            );
            assert_eq!(agent.inspect(&id).unwrap(), *change.candidate());
            assert_eq!(
                agent.list_sessions().unwrap(),
                vec![change.candidate().clone()]
            );
            assert_eq!(std::fs::read(&snapshot_path).unwrap(), original);
        }
        let error = agent.chat(&id, user("must require recovery")).unwrap_err();
        assert!(matches!(error.kind(), AgentErrorKind::RecoveryRequired));
        assert_eq!(error.durable(), Some(change.candidate()));
        assert!(error.uncommitted().is_none());
        assert_eq!(std::fs::read(&snapshot_path).unwrap(), original);
        assert!(server.requests().is_empty());
        assert!(io.borrow().targets.is_empty());
        let recovered = agent.recover(&id).unwrap();
        let archive = recovered.archive();
        assert_eq!(
            archive.revision().get(),
            change.candidate().revision().get() + 1
        );
        assert!(!archive.needs_recovery());
        assert_eq!(
            archive.session().turns()[0].outcome(),
            &TurnOutcome::Interrupted(StopReason::ProcessLost)
        );
        for generation in archive.generations() {
            assert!(!matches!(generation.observation(), Observation::Pending));
        }
        assert_eq!(
            archive
                .generations()
                .iter()
                .filter(|generation| matches!(
                    generation.observation(),
                    Observation::Abandoned { .. }
                ))
                .count(),
            usize::from(phase == 1 || phase == 3)
        );
        if phase >= 2 {
            let states: Vec<_> = archive.session().turns()[0].steps()[0]
                .executions()
                .map(|execution| execution.state())
                .collect();
            assert_eq!(states[1], &ExecutionState::Requested);
            match phase {
                2 | 3 => assert_eq!(states[0], &ExecutionState::Requested),
                4 => assert!(matches!(states[0], ExecutionState::Denied(_))),
                5 => assert!(matches!(states[0], ExecutionState::Approved(_))),
                6 => assert!(matches!(states[0], ExecutionState::Started { .. })),
                7 => assert!(
                    matches!(states[0],ExecutionState::Returned{outcome:ToolOutcome::Success(output),..}if output=="actual recorded return")
                ),
                _ => unreachable!(),
            }
            assert_eq!(
                results(&archive.session().messages()).len(),
                usize::from(phase == 4 || phase == 7)
            );
        }
        assert!(server.requests().is_empty());
        assert!(io.borrow().targets.is_empty());
        let recovered_bytes = std::fs::read(&snapshot_path).unwrap();
        assert_eq!(agent.recover(&id).unwrap().archive(), archive);
        assert_eq!(std::fs::read(&snapshot_path).unwrap(), recovered_bytes);
        drop(agent);
        let cold = native_service(Provider::OpenAI, &server, directory.path(), io.clone(), 2);
        let completed = cold.chat(&id, user("fresh request")).unwrap();
        assert_eq!(
            completed.messages().last(),
            Some(&Message::Assistant(
                AssistantMessage::answer("new turn after recovery").unwrap()
            ))
        );
        assert_eq!(server.requests().len(), 1);
        assert!(io.borrow().targets.is_empty());
        assert!(native_returns(Provider::OpenAI, &server.request(0).body).is_empty());
        assert_eq!(store.inspect(&id).unwrap().session().turns().len(), 2);
    }
}
