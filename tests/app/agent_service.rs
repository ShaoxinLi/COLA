#[allow(dead_code)]
#[path = "../adapters/models/support.rs"]
mod support;

use cola::{adapters::stores::JsonlSessionStore, app::*, domain::*};
use serde_json::{Map, Value, json};
use std::{
    cell::RefCell,
    collections::{BTreeMap, VecDeque},
    num::NonZeroU32,
    rc::Rc,
};
use support::{PROVIDERS, Provider, Server, text, tool, user};

type Script<O> = VecDeque<Result<Generation<O>, ModelError>>;

#[derive(Clone, Default, Debug, PartialEq, Eq)]
struct Io {
    assistant: usize,
    review: usize,
    context: usize,
    review_context: usize,
    save: usize,
    load: usize,
    list: usize,
    targets: Vec<u64>,
}

#[derive(Clone, Default)]
struct Store {
    io: Rc<RefCell<Io>>,
    saved: Rc<RefCell<BTreeMap<SessionId, Session>>>,
    attempted: Rc<RefCell<Vec<Session>>>,
    fault: Rc<RefCell<Option<(usize, bool)>>>,
    list_fault: Rc<RefCell<bool>>,
}
fn storage_error() -> SessionStoreError {
    SessionStoreError::new(
        SessionStoreErrorKind::Storage,
        std::io::Error::other("injected save failure"),
    )
}
impl SessionStore for Store {
    fn save(&self, session: &Session) -> Result<(), SessionStoreError> {
        self.io.borrow_mut().save += 1;
        self.attempted.borrow_mut().push(session.clone());
        let fault = self
            .fault
            .borrow()
            .filter(|(n, _)| *n == self.io.borrow().save);
        if fault != Some((self.io.borrow().save, false)) {
            self.saved
                .borrow_mut()
                .insert(session.id().clone(), session.clone());
        }
        if fault.is_some() {
            Err(storage_error())
        } else {
            Ok(())
        }
    }
    fn load(&self, id: &SessionId) -> Result<Session, SessionStoreError> {
        self.io.borrow_mut().load += 1;
        self.saved
            .borrow()
            .get(id)
            .map(|s| Session::restore(s.snapshot()).unwrap())
            .ok_or_else(|| {
                SessionStoreError::new(
                    SessionStoreErrorKind::NotFound,
                    std::io::Error::other("missing"),
                )
            })
    }
    fn list(&self) -> Result<Vec<Session>, SessionStoreError> {
        self.io.borrow_mut().list += 1;
        if *self.list_fault.borrow() {
            return Err(storage_error());
        }
        Ok(self
            .saved
            .borrow()
            .values()
            .map(|s| Session::restore(s.snapshot()).unwrap())
            .collect())
    }
}

#[derive(Clone)]
struct FakeModel {
    assistant: Rc<RefCell<Script<AssistantMessage>>>,
    reviewer: Rc<RefCell<Script<Decision>>>,
    inputs: Rc<RefCell<Vec<Context<AssistantMessage>>>>,
    io: Rc<RefCell<Io>>,
}
impl ModelClient<AssistantMessage> for FakeModel {
    fn generate(
        &self,
        context: &Context<AssistantMessage>,
        _: &ModelSettings,
        _: Option<&Continuation<AssistantMessage>>,
    ) -> Result<Generation<AssistantMessage>, ModelError> {
        self.io.borrow_mut().assistant += 1;
        self.inputs.borrow_mut().push(context.clone());
        self.assistant
            .borrow_mut()
            .pop_front()
            .expect("assistant script exhausted")
    }
}
impl ModelClient<Decision> for FakeModel {
    fn generate(
        &self,
        _: &Context<Decision>,
        _: &ModelSettings,
        continuation: Option<&Continuation<Decision>>,
    ) -> Result<Generation<Decision>, ModelError> {
        assert!(continuation.is_none());
        self.io.borrow_mut().review += 1;
        self.reviewer
            .borrow_mut()
            .pop_front()
            .expect("review script exhausted")
    }
}

struct AssistantContext {
    catalog: Vec<ToolDefinition>,
    fail: bool,
    io: Rc<RefCell<Io>>,
}
impl Assistant for AssistantContext {
    fn context(&self, session: &Session) -> Result<Context<AssistantMessage>, ContextError> {
        self.io.borrow_mut().context += 1;
        if self.fail {
            return Err(ContextError::MissingUser);
        }
        Context::new(
            text("Be useful"),
            session.context_exchanges()?,
            self.catalog.clone(),
        )
    }
}
struct ReviewContext {
    fail: bool,
    observed: Rc<RefCell<Vec<(Session, ToolCall)>>>,
    io: Rc<RefCell<Io>>,
}
impl ToolCallReviewer for ReviewContext {
    fn context(
        &self,
        session: &Session,
        call: &ToolCall,
    ) -> Result<Context<Decision>, ContextError> {
        self.io.borrow_mut().review_context += 1;
        self.observed
            .borrow_mut()
            .push((session.clone(), call.clone()));
        if self.fail {
            return Err(ContextError::UnresolvedToolBatch);
        }
        Context::new(
            text(&format!(
                "Review exact call {call:?}; active facts {:?}",
                session.active_turn()
            )),
            session.closed_context_exchanges()?,
            vec![],
        )
    }
}
struct Target {
    io: Rc<RefCell<Io>>,
    fail: bool,
}
impl Tool for Target {
    fn execute(&self, arguments: &Map<String, Value>) -> ToolOutcome {
        let n = arguments["n"].as_u64().unwrap();
        self.io.borrow_mut().targets.push(n);
        if self.fail {
            ToolOutcome::Failure(ToolError::new(text("actual target failure")))
        } else {
            ToolOutcome::Success(format!("actual lookup result {n}"))
        }
    }
}
fn settings(max: u32) -> AgentSettings {
    AgentSettings {
        assistant_model_settings: PROVIDERS[0].settings(),
        reviewer_model_settings: PROVIDERS[1].settings(),
        max_iterations: NonZeroU32::new(max).unwrap(),
    }
}
fn complete<O>(output: O) -> Result<Generation<O>, ModelError> {
    Ok(Generation::Complete {
        output,
        continuation: None,
    })
}
fn answer(value: &str) -> Result<Generation<AssistantMessage>, ModelError> {
    complete(AssistantMessage::answer(value).unwrap())
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
    complete(AssistantMessage::tool_requests(
        Some("exact preamble".into()),
        ToolCalls::new(calls).unwrap(),
    ))
}

type Service = AgentService<FakeModel, Store, AssistantContext, ReviewContext>;
struct Fixture {
    store: Store,
    model: FakeModel,
    observed: Rc<RefCell<Vec<(Session, ToolCall)>>>,
}
impl Fixture {
    fn new(
        assistant: Vec<Result<Generation<AssistantMessage>, ModelError>>,
        reviewer: Vec<Result<Generation<Decision>, ModelError>>,
    ) -> Self {
        let store = Store::default();
        let model = FakeModel {
            assistant: Rc::new(RefCell::new(assistant.into())),
            reviewer: Rc::new(RefCell::new(reviewer.into())),
            inputs: Default::default(),
            io: store.io.clone(),
        };
        Self {
            store,
            model,
            observed: Default::default(),
        }
    }
    fn service(
        &self,
        max: u32,
        assistant_fail: bool,
        review_fail: bool,
        target_fail: bool,
        catalog: Vec<ToolDefinition>,
    ) -> Service {
        AgentService::new(
            self.model.clone(),
            self.store.clone(),
            AssistantContext {
                catalog,
                fail: assistant_fail,
                io: self.store.io.clone(),
            },
            ReviewContext {
                fail: review_fail,
                observed: self.observed.clone(),
                io: self.store.io.clone(),
            },
            vec![(
                tool(),
                Box::new(Target {
                    io: self.store.io.clone(),
                    fail: target_fail,
                }),
            )],
            settings(max),
        )
        .unwrap()
    }
    fn basic(&self, max: u32) -> Service {
        self.service(max, false, false, false, vec![tool()])
    }
}
fn results(messages: &[Message]) -> Vec<ToolResult> {
    messages
        .iter()
        .filter_map(|m| {
            if let Message::Tool(result) = m {
                Some(result.clone())
            } else {
                None
            }
        })
        .collect()
}
fn assert_blocked(
    service: &mut Service,
    id: &SessionId,
    io: &Rc<RefCell<Io>>,
    checkpoint: Checkpoint,
    progress: &Session,
) {
    let before = io.borrow().clone();
    let error = service
        .chat(id, user("must not perform any I/O"))
        .unwrap_err();
    assert!(matches!(error.kind(), AgentErrorKind::Blocked { at } if *at == checkpoint));
    assert_eq!(error.progress(), Some(progress));
    assert_eq!(*io.borrow(), before);
}

#[test]
fn selective_review_preserves_exact_batch_and_continues_the_same_turn() {
    for allowed in [[true, false], [false, true], [false, false], [true, true]] {
        let proposal = AssistantMessage::tool_requests(
            Some("exact preamble".into()),
            ToolCalls::new(vec![call(1, "lookup"), call(2, "lookup")]).unwrap(),
        );
        let f = Fixture::new(
            vec![
                complete(proposal.clone()),
                answer("adapted"),
                answer("second turn"),
            ],
            allowed
                .into_iter()
                .map(|allow| complete(Decision::new(allow, Some(text("use read only")))))
                .collect(),
        );
        let mut agent = f.basic(2);
        let session = agent.new_session().unwrap();
        assert_eq!(f.store.load(session.id()).unwrap(), session);
        let messages = agent.chat(session.id(), user("first request")).unwrap();
        assert_eq!(messages.len(), 5);
        assert_eq!(messages[1], Message::Assistant(proposal));
        let outcomes = results(&messages);
        for (index, result) in outcomes.iter().enumerate() {
            assert_eq!(result.call_id(), call(index as u64 + 1, "lookup").id());
            match result.outcome() {
                ToolOutcome::Success(value) => {
                    assert!(allowed[index]);
                    assert_eq!(value, &format!("actual lookup result {}", index + 1));
                }
                ToolOutcome::Failure(error) => {
                    assert!(!allowed[index]);
                    assert_eq!(
                        error.description().as_str(),
                        "Not executed: tool call denied by review. Reason: use read only"
                    );
                }
            }
        }
        assert_eq!(
            f.store.io.borrow().targets,
            allowed
                .iter()
                .enumerate()
                .filter_map(|(i, allow)| allow.then_some(i as u64 + 1))
                .collect::<Vec<_>>()
        );
        assert_eq!(
            f.model.inputs.borrow()[1]
                .messages()
                .cloned()
                .collect::<Vec<_>>(),
            messages[..4]
        );
        let observed = f.observed.borrow();
        assert_eq!(observed.len(), 2);
        assert_eq!(observed[1].1, call(2, "lookup"));
        let active = observed[1].0.active_turn().unwrap();
        assert_eq!(active.id().get(), 1);
        assert_eq!(active.messages(), messages[..3]);
        assert!(matches!(
            active.steps()[0].executions().nth(1).unwrap().state(),
            ExecutionState::Started
        ));
        drop(observed);
        let next = agent.chat(session.id(), user("next request")).unwrap();
        assert_eq!(
            next,
            vec![
                Message::User(user("next request")),
                Message::Assistant(AssistantMessage::answer("second turn").unwrap())
            ]
        );
        assert_eq!(f.store.io.borrow().load, 1);
        assert_eq!(agent.list_session().unwrap()[0].turns().len(), 2);
    }
}

#[test]
fn no_reason_denial_and_genuine_tool_failure_are_normal_results() {
    let f = Fixture::new(
        vec![
            batch(vec![call(1, "lookup"), call(2, "lookup")]),
            answer("done"),
        ],
        vec![
            complete(Decision::new(false, None)),
            complete(Decision::new(true, None)),
        ],
    );
    let mut agent = f.service(2, false, false, true, vec![tool()]);
    let session = agent.new_session().unwrap();
    let messages = agent.chat(session.id(), user("request")).unwrap();
    let outcomes = results(&messages);
    assert!(
        matches!(outcomes[0].outcome(), ToolOutcome::Failure(error) if error.description().as_str().contains("without providing a reason"))
    );
    assert_eq!(
        outcomes[1].outcome(),
        &ToolOutcome::Failure(ToolError::new(text("actual target failure")))
    );
    assert_eq!(f.store.io.borrow().targets, vec![2]);
    assert_eq!(
        f.store.load(session.id()).unwrap().turns()[0].outcome(),
        &TurnOutcome::Completed
    );
}

#[test]
fn assistant_attempt_limit_drains_each_tool_and_saves_interruption() {
    let f = Fixture::new(
        vec![
            batch(vec![call(1, "lookup"), call(2, "lookup")]),
            answer("next turn"),
        ],
        vec![
            complete(Decision::new(false, None)),
            complete(Decision::new(true, None)),
        ],
    );
    let mut agent = f.basic(1);
    let session = agent.new_session().unwrap();
    let error = agent.chat(session.id(), user("request")).unwrap_err();
    assert!(matches!(error.kind(), AgentErrorKind::IterationLimit));
    let progress = error.progress().unwrap();
    assert!(progress.active_turn().is_none());
    assert_eq!(
        progress.turns()[0].outcome(),
        &TurnOutcome::Interrupted(StopReason::IterationLimit)
    );
    assert_eq!(results(&progress.messages()).len(), 2);
    assert_eq!(f.store.load(session.id()).unwrap(), *progress);
    assert_eq!(f.store.io.borrow().assistant, 1);
    assert_eq!(f.store.io.borrow().review, 2);
    assert_eq!(f.store.io.borrow().targets, vec![2]);
    assert!(agent.chat(session.id(), user("follow up")).is_ok());
}

#[test]
fn every_failed_save_preserves_live_facts_and_blocks_even_if_published() {
    for (save, checkpoint) in [
        (1, Checkpoint::NewSession),
        (2, Checkpoint::StartedTurn),
        (3, Checkpoint::AcceptedAssistant),
        (4, Checkpoint::StartedTool),
        (5, Checkpoint::ReturnedTool),
        (6, Checkpoint::ClosedTurn),
    ] {
        for published in [false, true] {
            let f = Fixture::new(
                vec![batch(vec![call(1, "lookup")])],
                vec![complete(Decision::new(true, None))],
            );
            *f.store.fault.borrow_mut() = Some((save, published));
            let mut agent = f.basic(1);
            let (id, error) = if save == 1 {
                let error = agent.new_session().unwrap_err();
                (error.progress().unwrap().id().clone(), error)
            } else {
                let session = agent.new_session().unwrap();
                (
                    session.id().clone(),
                    agent.chat(session.id(), user("request")).unwrap_err(),
                )
            };
            assert!(matches!(error.kind(), AgentErrorKind::Store { at, .. } if *at == checkpoint));
            let progress = error.progress().unwrap();
            assert_eq!(progress, f.store.attempted.borrow().last().unwrap());
            if published {
                assert_eq!(f.store.saved.borrow().get(&id), Some(progress));
            } else {
                assert_ne!(f.store.saved.borrow().get(&id), Some(progress));
            }
            if save <= 4 {
                assert_eq!(f.store.io.borrow().review, 0);
                assert!(f.store.io.borrow().targets.is_empty());
            } else {
                assert_eq!(f.store.io.borrow().targets, vec![1]);
            }
            assert_blocked(&mut agent, &id, &f.store.io, checkpoint, progress);
            assert_eq!(agent.list_session().unwrap(), vec![progress.clone()]);
            *f.store.list_fault.borrow_mut() = true;
            assert!(matches!(
                agent.list_session().unwrap_err().kind(),
                AgentErrorKind::Store {
                    at: Checkpoint::ListSessions,
                    ..
                }
            ));
            assert_blocked(&mut agent, &id, &f.store.io, checkpoint, progress);
        }
    }
}

#[test]
fn failed_answer_save_retains_the_completed_answer() {
    for published in [false, true] {
        let f = Fixture::new(vec![answer("completed answer")], vec![]);
        *f.store.fault.borrow_mut() = Some((3, published));
        let mut agent = f.basic(1);
        let session = agent.new_session().unwrap();
        let error = agent.chat(session.id(), user("request")).unwrap_err();
        let progress = error.progress().unwrap();
        assert_eq!(progress.turns()[0].outcome(), &TurnOutcome::Completed);
        assert_eq!(
            progress.messages().last(),
            Some(&Message::Assistant(
                AssistantMessage::answer("completed answer").unwrap()
            ))
        );
        assert_blocked(
            &mut agent,
            session.id(),
            &f.store.io,
            Checkpoint::AcceptedAssistant,
            progress,
        );
    }
}

#[test]
fn failed_denial_receipt_save_never_runs_the_denied_target_or_later_sibling() {
    for published in [false, true] {
        let f = Fixture::new(
            vec![batch(vec![call(1, "lookup"), call(2, "lookup")])],
            vec![complete(Decision::new(false, Some(text("denied"))))],
        );
        *f.store.fault.borrow_mut() = Some((5, published));
        let mut agent = f.basic(2);
        let session = agent.new_session().unwrap();
        let error = agent.chat(session.id(), user("request")).unwrap_err();
        let progress = error.progress().unwrap();
        assert_eq!(results(&progress.messages()).len(), 1);
        assert!(f.store.io.borrow().targets.is_empty());
        assert_eq!(f.store.io.borrow().review, 1);
        assert_blocked(
            &mut agent,
            session.id(),
            &f.store.io,
            Checkpoint::ReturnedTool,
            progress,
        );
    }
}

#[test]
fn reviewer_infrastructure_errors_preserve_started_state_and_block() {
    for failure in 0..5 {
        let review = match failure {
            0 | 4 => Err(ModelError::new(ModelErrorKind::Transport)),
            1 => Ok(Generation::Refused {
                reason: Some(text("cannot review")),
            }),
            2 => Ok(Generation::Incomplete {
                reason: IncompleteReason::OutputLimit,
            }),
            _ => Err(ModelError::new(ModelErrorKind::MalformedResponse)),
        };
        let f = Fixture::new(
            vec![batch(vec![call(1, "lookup"), call(2, "lookup")])],
            vec![review],
        );
        let mut agent = f.service(2, false, failure == 4, false, vec![tool()]);
        let session = agent.new_session().unwrap();
        let error = agent.chat(session.id(), user("request")).unwrap_err();
        match failure {
            0 => assert!(
                matches!(error.kind(), AgentErrorKind::Model { role: ModelRole::Reviewer, source } if source.kind() == ModelErrorKind::Transport)
            ),
            1 => assert!(
                matches!(error.kind(), AgentErrorKind::Refused { role: ModelRole::Reviewer, reason: Some(reason) } if reason.as_str() == "cannot review")
            ),
            2 => assert!(matches!(
                error.kind(),
                AgentErrorKind::Incomplete {
                    role: ModelRole::Reviewer,
                    reason: IncompleteReason::OutputLimit
                }
            )),
            3 => assert!(
                matches!(error.kind(), AgentErrorKind::Model { role: ModelRole::Reviewer, source } if source.kind() == ModelErrorKind::MalformedResponse)
            ),
            _ => assert!(matches!(
                error.kind(),
                AgentErrorKind::Context {
                    role: ModelRole::Reviewer,
                    source: ContextError::UnresolvedToolBatch
                }
            )),
        }
        let progress = error.progress().unwrap();
        assert_eq!(progress.next_action(), NextAction::AwaitTool);
        assert!(f.store.io.borrow().targets.is_empty());
        assert_eq!(f.store.io.borrow().save, 4);
        assert_eq!(f.store.saved.borrow().get(session.id()), Some(progress));
        assert_blocked(
            &mut agent,
            session.id(),
            &f.store.io,
            Checkpoint::StartedTool,
            progress,
        );
        let recovered = f.store.load(session.id()).unwrap();
        assert_eq!(
            recovered.turns()[0].outcome(),
            &TurnOutcome::Interrupted(StopReason::ProcessLost)
        );
        assert!(recovered.context_exchanges().unwrap()[0].notices().iter().any(|notice| matches!(notice, ExecutionNotice::UnknownEffect { call, .. } if call.id().as_str() == "call-1")));
    }
}

#[test]
fn assistant_failures_close_and_save_then_allow_another_chat() {
    for failure in 0..7 {
        let generation = match failure {
            0 | 3 => Err(ModelError::new(ModelErrorKind::Transport)),
            1 => Ok(Generation::Refused { reason: None }),
            2 => Ok(Generation::Incomplete {
                reason: IncompleteReason::Other,
            }),
            4 => Err(ModelError::new(ModelErrorKind::MalformedResponse)),
            5 => Err(ModelError::new(ModelErrorKind::UnsupportedResponse)),
            _ => Ok(Generation::Incomplete {
                reason: IncompleteReason::OutputLimit,
            }),
        };
        let f = Fixture::new(vec![generation, answer("next")], vec![]);
        let mut agent = f.service(1, failure == 3, false, false, vec![tool()]);
        let session = agent.new_session().unwrap();
        let error = agent.chat(session.id(), user("request")).unwrap_err();
        let progress = error.progress().unwrap();
        assert!(progress.active_turn().is_none());
        if failure == 3 {
            assert!(matches!(
                error.kind(),
                AgentErrorKind::Context {
                    role: ModelRole::Assistant,
                    ..
                }
            ));
            assert!(
                matches!(progress.turns()[0].outcome(), TurnOutcome::Failed(TurnFailure::ModelFailure(reason)) if reason.as_str() == "Assistant context assembly failed")
            );
        } else if failure == 4 || failure == 5 {
            assert!(matches!(
                progress.turns()[0].outcome(),
                TurnOutcome::Failed(TurnFailure::MalformedResponse(_))
            ));
        } else {
            assert!(matches!(
                progress.turns()[0].outcome(),
                TurnOutcome::Failed(TurnFailure::ModelFailure(_))
            ));
        }
        assert_eq!(&f.store.load(session.id()).unwrap(), progress);
        if failure != 3 {
            assert!(agent.chat(session.id(), user("next request")).is_ok());
        }
    }
}

#[test]
fn catalog_mismatch_and_unadvertised_calls_do_not_dispatch_or_record_output() {
    for case in 0..4 {
        let catalog = match case {
            0 => vec![ToolDefinition::new(
                tool().name().clone(),
                text("different definition"),
                tool().input_schema().clone(),
            )],
            1 => vec![ToolDefinition::new(
                ToolName::new("unregistered").unwrap(),
                text("unregistered"),
                tool().input_schema().clone(),
            )],
            2 => vec![],
            _ => vec![tool()],
        };
        let name = if case == 3 { "unknown" } else { "lookup" };
        let f = Fixture::new(vec![batch(vec![call(1, name)]), answer("next")], vec![]);
        let mut agent = f.service(2, false, false, false, catalog);
        let session = agent.new_session().unwrap();
        let error = agent.chat(session.id(), user("request")).unwrap_err();
        assert!(matches!(
            error.kind(),
            AgentErrorKind::CatalogMismatch(_) | AgentErrorKind::UnknownTool(_)
        ));
        assert!(error.progress().unwrap().active_turn().is_none());
        assert!(error.progress().unwrap().turns()[0].steps().is_empty());
        assert_eq!(f.store.io.borrow().assistant, usize::from(case >= 2));
        assert_eq!(f.store.io.borrow().review, 0);
        assert!(f.store.io.borrow().targets.is_empty());
    }
}

#[test]
fn reused_call_id_is_atomic_and_does_not_discard_earlier_progress() {
    let f = Fixture::new(
        vec![
            batch(vec![call(1, "lookup")]),
            batch(vec![call(1, "lookup")]),
            answer("next"),
        ],
        vec![complete(Decision::new(true, None))],
    );
    let mut agent = f.basic(3);
    let session = agent.new_session().unwrap();
    let error = agent.chat(session.id(), user("request")).unwrap_err();
    assert!(
        matches!(error.kind(), AgentErrorKind::RejectedOutput(SessionError::DuplicateCallId(id)) if id.as_str() == "call-1")
    );
    let progress = error.progress().unwrap();
    assert_eq!(progress.turns()[0].steps().len(), 1);
    assert!(matches!(
        progress.turns()[0].outcome(),
        TurnOutcome::Failed(TurnFailure::MalformedResponse(_))
    ));
    assert_eq!(f.store.io.borrow().targets, vec![1]);
    assert_eq!(results(&progress.messages()).len(), 1);
    assert!(agent.chat(session.id(), user("next request")).is_ok());
}

#[test]
fn duplicate_registration_is_rejected_and_listing_is_sorted_with_live_overlay() {
    let f = Fixture::new(vec![answer("done")], vec![]);
    let duplicate = AgentService::new(
        f.model.clone(),
        f.store.clone(),
        AssistantContext {
            catalog: vec![],
            fail: false,
            io: f.store.io.clone(),
        },
        ReviewContext {
            fail: false,
            observed: f.observed.clone(),
            io: f.store.io.clone(),
        },
        vec![
            (
                tool(),
                Box::new(Target {
                    io: f.store.io.clone(),
                    fail: false,
                }) as Box<dyn Tool>,
            ),
            (
                tool(),
                Box::new(Target {
                    io: f.store.io.clone(),
                    fail: false,
                }) as Box<dyn Tool>,
            ),
        ],
        settings(1),
    );
    assert!(
        matches!(duplicate.err().unwrap().kind(), AgentErrorKind::DuplicateTool(name) if name.as_str() == "lookup")
    );
    assert_eq!(*f.store.io.borrow(), Io::default());
    let stored = Session::new(SessionId::new("zz-stored").unwrap());
    f.store.save(&stored).unwrap();
    let mut agent = f.basic(1);
    let live = agent.new_session().unwrap();
    agent.chat(live.id(), user("request")).unwrap();
    let sessions = agent.list_session().unwrap();
    assert_eq!(
        sessions.iter().map(Session::id).collect::<Vec<_>>(),
        vec![live.id(), stored.id()]
    );
    assert_eq!(sessions[0].turns()[0].outcome(), &TurnOutcome::Completed);
}

struct ProviderClients {
    assistant: Box<dyn ModelClient<AssistantMessage>>,
    reviewer: Box<dyn ModelClient<Decision>>,
}
impl ModelClient<AssistantMessage> for ProviderClients {
    fn generate(
        &self,
        context: &Context<AssistantMessage>,
        settings: &ModelSettings,
        continuation: Option<&Continuation<AssistantMessage>>,
    ) -> Result<Generation<AssistantMessage>, ModelError> {
        self.assistant.generate(context, settings, continuation)
    }
}
impl ModelClient<Decision> for ProviderClients {
    fn generate(
        &self,
        context: &Context<Decision>,
        settings: &ModelSettings,
        continuation: Option<&Continuation<Decision>>,
    ) -> Result<Generation<Decision>, ModelError> {
        self.reviewer.generate(context, settings, continuation)
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
fn native_service(
    provider: Provider,
    server: &Server,
    directory: &std::path::Path,
    io: Rc<RefCell<Io>>,
    max: u32,
) -> AgentService<ProviderClients, JsonlSessionStore, AssistantContext, ReviewContext> {
    let mut settings = settings(max);
    settings.assistant_model_settings = provider.settings();
    settings.reviewer_model_settings = provider.settings();
    AgentService::new(
        ProviderClients {
            assistant: provider.client(server),
            reviewer: provider.reviewer(server),
        },
        JsonlSessionStore::new(directory),
        AssistantContext {
            catalog: vec![tool()],
            fail: false,
            io: io.clone(),
        },
        ReviewContext {
            fail: false,
            observed: Default::default(),
            io: io.clone(),
        },
        vec![(tool(), Box::new(Target { io, fail: false }))],
        settings,
    )
    .unwrap()
}

#[test]
fn real_providers_receive_correlated_denials_and_keep_native_continuation_across_chats() {
    for provider in PROVIDERS {
        for allowed in [[true, false], [false, true], [false, false], [true, true]] {
            let proposal = native_batch(provider);
            let mut replies = vec![proposal.clone().into()];
            replies.extend(allowed.map(|allow| provider.answer(&json!({"should_allow":allow,"reason":if allow {None} else {Some("use read only")}}).to_string()).into()));
            replies.extend([
                provider.answer("adapted").into(),
                provider.answer("next chat").into(),
            ]);
            let server = Server::new(replies);
            let dir = tempfile::tempdir().unwrap();
            let io = Rc::new(RefCell::new(Io::default()));
            let mut agent = native_service(provider, &server, dir.path(), io.clone(), 2);
            let session = agent.new_session().unwrap();
            let messages = agent.chat(session.id(), user("request")).unwrap();
            assert_eq!(messages.len(), 5);
            assert_eq!(
                io.borrow().targets,
                allowed
                    .iter()
                    .enumerate()
                    .filter_map(|(i, allow)| allow.then_some(i as u64 + 1))
                    .collect::<Vec<_>>()
            );
            assert_eq!(
                provider.request_native(&server.request(3).body),
                provider.native(&proposal).as_array().unwrap().clone()
            );
            let feedback = server.request(3).body;
            let sent = feedback.to_string();
            assert!(sent.contains("native-1") && sent.contains("native-2"));
            for (index, allow) in allowed.iter().enumerate() {
                let value = if *allow {
                    format!("actual lookup result {}", index + 1)
                } else {
                    "Not executed: tool call denied by review. Reason: use read only".into()
                };
                assert!(sent.contains(&value));
                if index == 0 {
                    assert!(server.request(2).body.to_string().contains(&value));
                }
            }
            match provider {
                Provider::OpenAI => assert_eq!(
                    feedback["input"]
                        .as_array()
                        .unwrap()
                        .iter()
                        .filter(|item| item["type"] == "function_call_output")
                        .count(),
                    2
                ),
                Provider::Anthropic => {
                    let returns =
                        feedback["messages"].as_array().unwrap().last().unwrap()["content"]
                            .as_array()
                            .unwrap()
                            .clone();
                    assert_eq!(returns.len(), 2);
                    for (index, result) in returns.iter().enumerate() {
                        assert_eq!(result["is_error"], !allowed[index]);
                    }
                }
            }
            let second = agent.chat(session.id(), user("follow up")).unwrap();
            assert_eq!(second.len(), 2);
            assert_eq!(server.requests().len(), 5);
            let persisted = JsonlSessionStore::new(dir.path())
                .load(session.id())
                .unwrap();
            assert_eq!(persisted.turns().len(), 2);
            assert_eq!(persisted.turns()[0].messages(), messages);
            let mut cold = native_service(provider, &server, dir.path(), io.clone(), 2);
            let error = cold.chat(session.id(), user("after reload")).unwrap_err();
            assert!(
                matches!(error.kind(), AgentErrorKind::Model { role: ModelRole::Assistant, source } if source.kind() == ModelErrorKind::MissingContinuation)
            );
            assert!(error.progress().unwrap().active_turn().is_none());
            assert_eq!(server.requests().len(), 5);
        }
    }
}

#[test]
fn real_provider_final_budget_denial_is_durable_and_next_chat_replays_it() {
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
        let dir = tempfile::tempdir().unwrap();
        let io = Rc::new(RefCell::new(Io::default()));
        let mut agent = native_service(provider, &server, dir.path(), io.clone(), 1);
        let session = agent.new_session().unwrap();
        let error = agent.chat(session.id(), user("request")).unwrap_err();
        assert!(matches!(error.kind(), AgentErrorKind::IterationLimit));
        assert_eq!(
            error.progress().unwrap().turns()[0].outcome(),
            &TurnOutcome::Interrupted(StopReason::IterationLimit)
        );
        assert_eq!(results(&error.progress().unwrap().messages()).len(), 2);
        assert!(io.borrow().targets.is_empty());
        assert_eq!(server.requests().len(), 3);
        assert!(agent.chat(session.id(), user("follow up")).is_ok());
        assert!(
            server
                .request(3)
                .body
                .to_string()
                .contains("without providing a reason")
        );
        assert_eq!(
            JsonlSessionStore::new(dir.path())
                .load(session.id())
                .unwrap()
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
    fn generate(
        &self,
        context: &Context<AssistantMessage>,
        settings: &ModelSettings,
        continuation: Option<&Continuation<AssistantMessage>>,
    ) -> Result<Generation<AssistantMessage>, ModelError> {
        let mut generated = self.native.generate(context, settings, continuation)?;
        self.generations.set(self.generations.get() + 1);
        if let Generation::Complete { output, .. } = &mut generated {
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
        Ok(generated)
    }
}
impl ModelClient<Decision> for InvalidSecondProposal {
    fn generate(
        &self,
        context: &Context<Decision>,
        settings: &ModelSettings,
        continuation: Option<&Continuation<Decision>>,
    ) -> Result<Generation<Decision>, ModelError> {
        self.native.generate(context, settings, continuation)
    }
}

#[test]
fn rejected_native_output_never_replaces_the_accepted_continuation() {
    for provider in PROVIDERS {
        for duplicate in [true, false] {
            let first = provider.tool(1);
            let server = Server::new(vec![
                first.clone().into(),
                provider
                    .answer(&json!({"should_allow":true,"reason":null}).to_string())
                    .into(),
                provider.tool(2).into(),
                provider.answer("recovered next chat").into(),
            ]);
            let dir = tempfile::tempdir().unwrap();
            let io = Rc::new(RefCell::new(Io::default()));
            let native = ProviderClients {
                assistant: provider.client(&server),
                reviewer: provider.reviewer(&server),
            };
            let mut model_settings = settings(3);
            model_settings.assistant_model_settings = provider.settings();
            model_settings.reviewer_model_settings = provider.settings();
            let mut agent = AgentService::new(
                InvalidSecondProposal {
                    native,
                    first: RefCell::new(None),
                    generations: std::cell::Cell::new(0),
                    duplicate,
                },
                JsonlSessionStore::new(dir.path()),
                AssistantContext {
                    catalog: vec![tool()],
                    fail: false,
                    io: io.clone(),
                },
                ReviewContext {
                    fail: false,
                    observed: Default::default(),
                    io: io.clone(),
                },
                vec![(
                    tool(),
                    Box::new(Target {
                        io: io.clone(),
                        fail: false,
                    }),
                )],
                model_settings,
            )
            .unwrap();
            let session = agent.new_session().unwrap();
            let error = agent.chat(session.id(), user("request")).unwrap_err();
            if duplicate {
                assert!(matches!(
                    error.kind(),
                    AgentErrorKind::RejectedOutput(SessionError::DuplicateCallId(_))
                ));
            } else {
                assert!(matches!(error.kind(), AgentErrorKind::UnknownTool(_)));
            }
            let progress = error.progress().unwrap();
            assert_eq!(progress.turns()[0].steps().len(), 1);
            assert!(matches!(
                progress.turns()[0].outcome(),
                TurnOutcome::Failed(TurnFailure::MalformedResponse(_))
            ));
            assert_eq!(io.borrow().targets, vec![1]);
            let next = agent.chat(session.id(), user("next user request")).unwrap();
            assert_eq!(
                next.last(),
                Some(&Message::Assistant(
                    AssistantMessage::answer("recovered next chat").unwrap()
                ))
            );
            assert_eq!(
                provider.request_native(&server.request(3).body),
                provider.native(&first).as_array().unwrap().clone()
            );
            assert_eq!(io.borrow().targets, vec![1]);
            assert_eq!(
                JsonlSessionStore::new(dir.path())
                    .load(session.id())
                    .unwrap()
                    .turns()
                    .len(),
                2
            );
        }
    }
}

#[test]
fn missing_load_does_not_poison_the_cache() {
    let f = Fixture::new(vec![answer("found later")], vec![]);
    let mut agent = f.basic(1);
    let session = Session::new(SessionId::new("not-present-yet").unwrap());
    let error = agent.chat(session.id(), user("first attempt")).unwrap_err();
    assert!(
        matches!(error.kind(), AgentErrorKind::Store { at: Checkpoint::LoadSession, source } if source.kind() == SessionStoreErrorKind::NotFound)
    );
    assert!(error.progress().is_none());
    assert_eq!(f.store.io.borrow().assistant, 0);
    f.store.save(&session).unwrap();
    assert_eq!(
        agent.chat(session.id(), user("now present")).unwrap().len(),
        2
    );
    assert_eq!(f.store.io.borrow().load, 2);
}

#[test]
fn rejected_start_preserves_and_blocks_the_resident_session_atomically() {
    struct UnrecoveredStore(Session, Rc<RefCell<Io>>);
    impl SessionStore for UnrecoveredStore {
        fn save(&self, _: &Session) -> Result<(), SessionStoreError> {
            panic!("a rejected start must not save")
        }
        fn load(&self, _: &SessionId) -> Result<Session, SessionStoreError> {
            self.1.borrow_mut().load += 1;
            Ok(self.0.clone())
        }
        fn list(&self) -> Result<Vec<Session>, SessionStoreError> {
            self.1.borrow_mut().list += 1;
            Ok(vec![Session::restore(self.0.snapshot()).unwrap()])
        }
    }
    let f = Fixture::new(vec![], vec![]);
    let mut active = Session::new(SessionId::new("unrecovered").unwrap());
    active.start_turn(user("existing user")).unwrap();
    let mut agent = AgentService::new(
        f.model.clone(),
        UnrecoveredStore(active.clone(), f.store.io.clone()),
        AssistantContext {
            catalog: vec![tool()],
            fail: false,
            io: f.store.io.clone(),
        },
        ReviewContext {
            fail: false,
            observed: f.observed.clone(),
            io: f.store.io.clone(),
        },
        vec![],
        settings(1),
    )
    .unwrap();
    let error = agent
        .chat(active.id(), user("must not replace the existing user"))
        .unwrap_err();
    assert!(matches!(
        error.kind(),
        AgentErrorKind::RejectedOutput(SessionError::ActiveTurnExists)
    ));
    assert_eq!(error.progress(), Some(&active));
    let before = f.store.io.borrow().clone();
    let blocked = agent
        .chat(active.id(), user("must not reload"))
        .unwrap_err();
    assert!(matches!(
        blocked.kind(),
        AgentErrorKind::Blocked {
            at: Checkpoint::StartedTurn
        }
    ));
    assert_eq!(blocked.progress(), Some(&active));
    assert_eq!(*f.store.io.borrow(), before);
    assert_eq!(agent.list_session().unwrap(), vec![active]);
}
