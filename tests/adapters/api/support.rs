pub(super) use cola::{app::*, domain::*};
pub(super) use serde_json::{Value, json};
use std::num::{NonZeroU32, NonZeroU64, NonZeroUsize};
pub(super) use std::{
    cell::{Cell, RefCell},
    collections::BTreeMap,
    io,
    rc::Rc,
};

#[derive(Clone, Default)]
pub(super) struct Counts {
    pub model: Rc<Cell<usize>>,
    pub review: Rc<Cell<usize>>,
    pub save: Rc<Cell<usize>>,
    pub load: Rc<Cell<usize>>,
    pub list: Rc<Cell<usize>>,
    pub tool: Rc<Cell<usize>>,
}
pub(super) fn text(value: &str) -> NonEmptyText {
    NonEmptyText::new(value).unwrap()
}
pub(super) fn definition() -> ToolDefinition {
    ToolDefinition::new(
        ToolName::new("echo").unwrap(),
        text("echo"),
        InputSchema::new(json!({"type":"object"})).unwrap(),
    )
}
pub(super) fn call() -> ToolCall {
    ToolCall::new(
        ToolCallId::new("call-1").unwrap(),
        ToolName::new("echo").unwrap(),
        json!({"n":42}),
    )
    .unwrap()
}
pub(super) struct Model {
    counts: Counts,
    mode: String,
}
fn prepared<O>(
    plan: &ContextPlan<O>,
    settings: &ModelSettings,
) -> Result<PreparedRequest<O>, ModelError> {
    Ok(PreparedRequest::new(
        ModelTarget::new("fake", "memory"),
        settings.clone(),
        plan.fit(|_| json!({"fake":true}))?,
    ))
}
fn observed<O>(result: Result<Generation<O>, ModelError>) -> ObservedReply<O> {
    ObservedReply::new(
        ResponseReceipt {
            capture: ResponseCapture::Complete(b"{}".to_vec()),
            ..ResponseReceipt::default()
        },
        result,
    )
}
impl ModelClient<AssistantMessage> for Model {
    fn prepare(
        &self,
        plan: &ContextPlan<AssistantMessage>,
        settings: &ModelSettings,
    ) -> Result<PreparedRequest<AssistantMessage>, ModelError> {
        prepared(plan, settings)
    }
    fn invoke(&self, _: &PreparedRequest<AssistantMessage>) -> ObservedReply<AssistantMessage> {
        let n = self.counts.model.get() + 1;
        self.counts.model.set(n);
        let result = match self.mode.as_str() {
            "timeout" => Err(ModelError::new(ModelErrorKind::Timeout)),
            "model" => Err(ModelError::new(ModelErrorKind::Transport)),
            "refused" => Ok(Generation::Refused {
                reason: Some(text("private refusal")),
            }),
            "incomplete" => Ok(Generation::Incomplete {
                reason: IncompleteReason::OutputLimit,
            }),
            _ => {
                let output =
                    if (matches!(self.mode.as_str(), "tools" | "review-timeout" | "denied")
                        && n == 1)
                        || self.mode == "limit"
                    {
                        AssistantMessage::tool_requests(
                            Some("checking".into()),
                            ToolCalls::new(vec![call()]).unwrap(),
                        )
                    } else {
                        AssistantMessage::answer(format!("answer-{n}")).unwrap()
                    };
                Ok(Generation::Complete(output))
            }
        };
        observed(result)
    }
}
impl ModelClient<Decision> for Model {
    fn prepare(
        &self,
        plan: &ContextPlan<Decision>,
        settings: &ModelSettings,
    ) -> Result<PreparedRequest<Decision>, ModelError> {
        prepared(plan, settings)
    }
    fn invoke(&self, _: &PreparedRequest<Decision>) -> ObservedReply<Decision> {
        self.counts.review.set(self.counts.review.get() + 1);
        observed(if self.mode == "review-timeout" {
            Err(ModelError::new(ModelErrorKind::Timeout))
        } else {
            Ok(Generation::Complete(Decision::new(
                self.mode != "denied",
                Some(text("policy")),
            )))
        })
    }
}
struct Target(Counts);
impl Tool for Target {
    fn execute(&self, args: &serde_json::Map<String, Value>) -> ToolOutcome {
        assert_eq!(args["n"], 42);
        self.0.tool.set(self.0.tool.get() + 1);
        ToolOutcome::Success("result".into())
    }
}
#[derive(Clone, Default)]
pub(super) struct Store {
    pub counts: Counts,
    pub sessions: Rc<RefCell<BTreeMap<SessionId, SessionArchive>>>,
    pub fail_save: Option<usize>,
    pub fail_list: bool,
}
fn storage_error() -> SessionStoreError {
    SessionStoreError::new(
        SessionStoreErrorKind::Storage,
        io::Error::other("/private/secret storage detail"),
    )
}
impl Store {
    fn publish(&self, change: &ArchiveChange) -> Result<(), CommitFault> {
        let n = self.counts.save.get() + 1;
        self.counts.save.set(n);
        if self.fail_save == Some(n) {
            return Err(CommitFault::NotPublished(storage_error()));
        }
        self.sessions.borrow_mut().insert(
            change.candidate().session().id().clone(),
            change.candidate().clone(),
        );
        Ok(())
    }
}
struct Transaction {
    store: Store,
    current: SessionArchive,
}
impl SessionTransaction for Transaction {
    fn confirmed(&self) -> &SessionArchive {
        &self.current
    }
    fn commit(&mut self, change: &ArchiveChange) -> Result<(), CommitFault> {
        if change
            .base_key()
            .is_none_or(|key| key.revision() != self.current.revision())
        {
            return Err(CommitFault::Conflict);
        }
        self.store.publish(change)?;
        self.current = change.candidate().clone();
        Ok(())
    }
    fn resolve(&mut self, _: &ArchiveChange) -> Result<(), CommitFault> {
        Err(CommitFault::Conflict)
    }
}
impl SessionStore for Store {
    fn create(&self, initial: &ArchiveChange) -> Result<(), CommitFault> {
        if self
            .sessions
            .borrow()
            .contains_key(initial.candidate().session().id())
        {
            return Err(CommitFault::Conflict);
        }
        self.publish(initial)
    }
    fn list(&self) -> Result<Vec<SessionArchive>, SessionStoreError> {
        self.counts.list.set(self.counts.list.get() + 1);
        if self.fail_list {
            return Err(storage_error());
        }
        Ok(self.sessions.borrow().values().cloned().collect())
    }
    fn inspect(&self, id: &SessionId) -> Result<SessionArchive, SessionStoreError> {
        self.counts.load.set(self.counts.load.get() + 1);
        self.sessions.borrow().get(id).cloned().ok_or_else(|| {
            SessionStoreError::new(
                SessionStoreErrorKind::NotFound,
                io::Error::other("/private/secret missing"),
            )
        })
    }
    fn acquire(
        &self,
        id: &SessionId,
    ) -> Result<Box<dyn SessionTransaction + '_>, SessionStoreError> {
        let current = self.inspect(id)?;
        Ok(Box::new(Transaction {
            store: self.clone(),
            current,
        }))
    }
}
pub(super) fn service<S: SessionStore>(
    mode: &str,
    store: S,
    counts: Counts,
) -> AgentService<Model, S> {
    let role = RoleSettings {
        model: ModelSettings {
            model_id: ModelId::new("fake").unwrap(),
            effort: Effort::Default,
            max_output_tokens: NonZeroU32::new(100).unwrap(),
        },
        instructions: text("role instructions"),
        limits: ContextLimits {
            input_tokens: NonZeroU64::new(100000).unwrap(),
            request_bytes: NonZeroUsize::new(1048576).unwrap(),
        },
    };
    AgentService::new(
        Model {
            counts: counts.clone(),
            mode: mode.into(),
        },
        store,
        vec![(definition(), Box::new(Target(counts)))],
        AgentSettings {
            assistant: role.clone(),
            reviewer: role,
            max_iterations: NonZeroU32::new(if mode == "limit" { 1 } else { 5 }).unwrap(),
        },
    )
    .unwrap()
}
pub(super) fn unknown_effect_archive() -> SessionArchive {
    let mut archive = SessionArchive::new(SessionId::new("resolved").unwrap());
    let turn = archive
        .session_mut()
        .start_turn(UserMessage::new(text("work")))
        .unwrap();
    let role = RoleSettings {
        model: ModelSettings {
            model_id: ModelId::new("fake").unwrap(),
            effort: Effort::Default,
            max_output_tokens: NonZeroU32::new(100).unwrap(),
        },
        instructions: text("role"),
        limits: ContextLimits {
            input_tokens: NonZeroU64::new(10000).unwrap(),
            request_bytes: NonZeroUsize::new(100000).unwrap(),
        },
    };
    let plan = ContextPlan::assistant(
        &archive,
        role.instructions.clone(),
        vec![definition()],
        role.limits,
        &[],
    )
    .unwrap();
    let request = prepared(&plan, &role.model).unwrap();
    let source = archive
        .begin_generation(InvocationRole::Assistant { turn }, &request)
        .unwrap();
    archive
        .session_mut()
        .record_assistant(
            turn,
            source,
            AssistantMessage::tool_requests(None, ToolCalls::new(vec![call()]).unwrap()),
        )
        .unwrap();
    archive
        .record_observation(
            source,
            Timestamp::now(),
            observed::<AssistantMessage>(Err(ModelError::new(ModelErrorKind::Transport))).receipt,
            Disposition::Accepted,
        )
        .unwrap();
    let call_ref = CallRef::new(source, call().id().clone());
    let plan = ContextPlan::reviewer(
        &archive,
        &call_ref,
        role.instructions.clone(),
        vec![definition()],
        role.limits,
        &[],
    )
    .unwrap();
    let request = prepared(&plan, &role.model).unwrap();
    let review = archive
        .begin_generation(
            InvocationRole::Reviewer {
                call: call_ref.clone(),
            },
            &request,
        )
        .unwrap();
    archive
        .session_mut()
        .review_tool(turn, &call_ref, review, &Decision::new(true, None))
        .unwrap();
    archive
        .record_observation(
            review,
            Timestamp::now(),
            observed::<Decision>(Err(ModelError::new(ModelErrorKind::Transport))).receipt,
            Disposition::Accepted,
        )
        .unwrap();
    archive
        .session_mut()
        .start_tool(turn, &call_ref, Timestamp::now())
        .unwrap();
    archive
        .session_mut()
        .interrupt(turn, StopReason::ProcessLost)
        .unwrap();
    archive
        .session_mut()
        .resolve_effect(
            turn,
            &call_ref,
            EffectResolution::new(
                EffectDisposition::NotPerformed,
                ResolutionSource::ApplicationAttestation,
                text("checked"),
            ),
        )
        .unwrap();
    ArchiveChange::initial(archive).unwrap().candidate().clone()
}
