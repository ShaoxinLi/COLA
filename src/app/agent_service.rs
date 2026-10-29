use super::*;
use crate::domain::*;
use std::{collections::BTreeMap, error::Error, fmt, num::NonZeroU32};

#[derive(Clone, Debug)]
pub struct AgentSettings {
    pub assistant_model_settings: ModelSettings,
    pub reviewer_model_settings: ModelSettings,
    pub max_iterations: NonZeroU32,
}
pub struct AgentService<M, S, A, R> {
    model_client: M,
    session_store: S,
    assistant: A,
    reviewer: R,
    settings: AgentSettings,
    tools: BTreeMap<ToolName, (ToolDefinition, Box<dyn Tool>)>,
    context_policy: Box<dyn ContextPolicy>,
}
fn error(doc: Option<&SessionDocument>, kind: AgentErrorKind) -> AgentError {
    AgentError {
        kind,
        progress: doc.map(|d| Box::new(d.session().clone())),
    }
}
fn document_error(doc: &SessionDocument, source: DocumentError) -> AgentError {
    error(Some(doc), AgentErrorKind::Document(source))
}
fn commit(
    writer: &mut dyn SessionWriter,
    next: SessionDocument,
    at: Checkpoint,
) -> Result<(), AgentError> {
    writer.commit(next).map(|_| ()).map_err(|source| {
        error(
            Some(writer.committed()),
            AgentErrorKind::Store { at, source },
        )
    })
}
struct Recorder<'a> {
    writer: &'a mut dyn SessionWriter,
    invocation: u64,
}
impl BeforeModelSend for Recorder<'_> {
    fn persist(&mut self, prepared: PreparedInvocation) -> Result<SendPermit, SessionStoreError> {
        let next = self
            .writer
            .committed()
            .prepared(self.invocation, prepared)
            .map_err(|e| SessionStoreError::new(SessionStoreErrorKind::InvalidData, e))?;
        let revision = self.writer.commit(next)?;
        Ok(SendPermit {
            _revision: revision,
        })
    }
}
impl<M, S, A, R> AgentService<M, S, A, R>
where
    M: ModelClient<AssistantMessage> + ModelClient<Decision>,
    S: SessionStore,
    A: Assistant,
    R: ToolCallReviewer,
{
    pub fn new(
        model_client: M,
        session_store: S,
        assistant: A,
        reviewer: R,
        tools: Vec<(ToolDefinition, Box<dyn Tool>)>,
        settings: AgentSettings,
    ) -> Result<Self, AgentError> {
        let mut registry = BTreeMap::new();
        for (definition, tool) in tools {
            let name = definition.name().clone();
            if registry.insert(name.clone(), (definition, tool)).is_some() {
                return Err(error(None, AgentErrorKind::DuplicateTool(name)));
            }
        }
        Ok(Self {
            model_client,
            session_store,
            assistant,
            reviewer,
            settings,
            tools: registry,
            context_policy: Box::new(RecentContextPolicy::default()),
        })
    }
    pub fn with_context_policy(mut self, policy: impl ContextPolicy + 'static) -> Self {
        self.context_policy = Box::new(policy);
        self
    }
    pub fn new_session(&mut self) -> Result<Session, AgentError> {
        self.session_store
            .create()
            .map(|w| w.committed().session().clone())
            .map_err(|source| {
                error(
                    None,
                    AgentErrorKind::Store {
                        at: Checkpoint::NewSession,
                        source,
                    },
                )
            })
    }
    pub fn inspect_session(&self, id: &SessionId) -> Result<SessionDocument, AgentError> {
        self.session_store.inspect(id).map_err(|source| {
            error(
                None,
                AgentErrorKind::Store {
                    at: Checkpoint::LoadSession,
                    source,
                },
            )
        })
    }
    pub fn list_session(&self) -> Result<Vec<Session>, AgentError> {
        self.session_store
            .list()
            .map_err(|source| {
                error(
                    None,
                    AgentErrorKind::Store {
                        at: Checkpoint::ListSessions,
                        source,
                    },
                )
            })?
            .iter()
            .map(|id| self.inspect_session(id).map(|d| d.session().clone()))
            .collect()
    }
    pub fn acknowledge_review_failure(
        &mut self,
        id: &SessionId,
        invocation: u64,
        evidence: NonEmptyText,
        actor: Option<NonEmptyText>,
    ) -> Result<(), AgentError> {
        let mut writer = self.acquire(id)?;
        if writer.committed().invocations().iter().any(|i| {
            matches!(
                i.state,
                InvocationState::Planned | InvocationState::Prepared(_)
            )
        }) {
            recover(&mut *writer)?;
        }
        let next = writer
            .committed()
            .acknowledge(invocation, evidence, actor)
            .map_err(|e| document_error(writer.committed(), e))?;
        if let Some(next) = next {
            commit(&mut *writer, next, Checkpoint::ReviewAcknowledgement)?;
        }
        Ok(())
    }
    pub fn resolve_effect(
        &mut self,
        address: CallAddress,
        resolution: EffectResolution,
    ) -> Result<(), AgentError> {
        let mut writer = self.acquire(&address.session)?;
        recover(&mut *writer)?;
        let next = writer
            .committed()
            .mutate_session(|s| s.resolve_effect(&address, resolution))
            .map_err(|e| error(Some(writer.committed()), AgentErrorKind::RejectedOutput(e)))?;
        commit(&mut *writer, next, Checkpoint::EffectResolution)
    }
    fn acquire(&self, id: &SessionId) -> Result<Box<dyn SessionWriter + '_>, AgentError> {
        self.session_store.acquire(id).map_err(|source| {
            error(
                None,
                AgentErrorKind::Store {
                    at: Checkpoint::LoadSession,
                    source,
                },
            )
        })
    }
    pub fn chat(&mut self, id: &SessionId, user: UserMessage) -> Result<Vec<Message>, AgentError> {
        let mut writer = self.acquire(id)?;
        recover(&mut *writer)?;
        if let Some(invocation) = writer.committed().review_block() {
            return Err(error(
                Some(writer.committed()),
                AgentErrorKind::ReviewBlocked { invocation },
            ));
        }
        let next = writer
            .committed()
            .mutate_session(|s| s.start_turn(user).map(|_| ()))
            .map_err(|e| error(Some(writer.committed()), AgentErrorKind::RejectedOutput(e)))?;
        commit(&mut *writer, next, Checkpoint::StartedTurn)?;
        let turn = writer.committed().session().active_turn().unwrap().id();
        let mut attempts = 0;
        loop {
            match writer.committed().session().next_action() {
                NextAction::AskModel => {
                    if attempts == self.settings.max_iterations.get() {
                        let next = writer
                            .committed()
                            .mutate_session(|s| s.interrupt(turn, StopReason::IterationLimit))
                            .unwrap();
                        commit(&mut *writer, next, Checkpoint::ClosedTurn)?;
                        return Err(error(
                            Some(writer.committed()),
                            AgentErrorKind::IterationLimit,
                        ));
                    }
                    attempts += 1;
                    let step = writer
                        .committed()
                        .session()
                        .active_turn()
                        .unwrap()
                        .steps()
                        .len() as u64
                        + 1;
                    let requested = RequestedModel {
                        route: ModelClient::<AssistantMessage>::route(&self.model_client),
                        settings: self.settings.assistant_model_settings.clone(),
                    };
                    let next = writer
                        .committed()
                        .plan(
                            InvocationTarget::Assistant {
                                turn: turn.get(),
                                step,
                            },
                            requested,
                        )
                        .map_err(|e| document_error(writer.committed(), e))?;
                    commit(&mut *writer, next, Checkpoint::PlannedInvocation)?;
                    let invocation = writer.committed().invocations().last().unwrap().id;
                    let context = self.assistant.context(writer.committed().session());
                    let mut advertised = vec![];
                    let observed = match context {
                        Err(_) => {
                            Observation::failed(ModelError::new(ModelErrorKind::InvalidSelection))
                        }
                        Ok(context) => {
                            let current = writer
                                .committed()
                                .session()
                                .context_exchanges()
                                .ok()
                                .and_then(|e| e.last().cloned());
                            advertised = context.tools().to_vec();
                            if context.exchanges().last() != current.as_ref() {
                                Observation::failed(ModelError::new(
                                    ModelErrorKind::InvalidSelection,
                                ))
                            } else if advertised.iter().any(|t| {
                                self.tools
                                    .get(t.name())
                                    .is_none_or(|(registered, _)| registered != t)
                            }) {
                                Observation::failed(ModelError::new(
                                    ModelErrorKind::UnsupportedInput,
                                ))
                            } else {
                                let selection = selection_for(
                                    &*self.context_policy,
                                    writer.committed().session(),
                                    &context,
                                );
                                match selection {
                                    Err(e) => Observation::failed(e),
                                    Ok(selection) => {
                                        let view = writer
                                            .committed()
                                            .views()
                                            .iter()
                                            .find(|v| v.turn == turn.get())
                                            .cloned();
                                        let native = writer
                                            .committed()
                                            .native_steps(turn)
                                            .map_err(|e| document_error(writer.committed(), e))?;
                                        let input = ModelInput {
                                            context: &context,
                                            settings: &self.settings.assistant_model_settings,
                                            selection: &selection,
                                            view: view.as_ref(),
                                            native: &native,
                                        };
                                        ModelClient::<AssistantMessage>::invoke(
                                            &self.model_client,
                                            input,
                                            &mut Recorder {
                                                writer: &mut *writer,
                                                invocation,
                                            },
                                        )
                                        .map_err(
                                            |source| {
                                                error(
                                                    Some(writer.committed()),
                                                    AgentErrorKind::Store {
                                                        at: Checkpoint::PreparedInvocation,
                                                        source,
                                                    },
                                                )
                                            },
                                        )?
                                    }
                                }
                            }
                        }
                    };
                    let next = writer
                        .committed()
                        .accept_assistant(invocation, &observed, &advertised)
                        .map_err(|e| document_error(writer.committed(), e))?;
                    commit(&mut *writer, next, Checkpoint::AcceptedAssistant)?;
                    if let Some(kind) = generation_error(&observed.generation, ModelRole::Assistant)
                    {
                        return Err(error(Some(writer.committed()), kind));
                    }
                    if matches!(
                        writer.committed().invocations().last().unwrap().state,
                        InvocationState::Finished {
                            outcome: RecordedOutcome::OutputRejected,
                            ..
                        }
                    ) {
                        return Err(error(
                            Some(writer.committed()),
                            AgentErrorKind::UnexpectedState,
                        ));
                    }
                }
                NextAction::Execute(native) => {
                    let address = writer.committed().session().call_address(native).unwrap();
                    let next = writer
                        .committed()
                        .mutate_session(|s| s.start_tool(&address).map(|_| ()))
                        .unwrap();
                    commit(&mut *writer, next, Checkpoint::StartedTool)?;
                    let call = writer
                        .committed()
                        .session()
                        .active_turn()
                        .unwrap()
                        .steps()
                        .last()
                        .unwrap()
                        .executions()
                        .find(|e| e.call().id() == &address.native)
                        .unwrap()
                        .call()
                        .clone();
                    let requested = RequestedModel {
                        route: ModelClient::<Decision>::route(&self.model_client),
                        settings: self.settings.reviewer_model_settings.clone(),
                    };
                    let next = writer
                        .committed()
                        .plan(
                            InvocationTarget::Reviewer {
                                call: (&address).into(),
                            },
                            requested,
                        )
                        .map_err(|e| document_error(writer.committed(), e))?;
                    commit(&mut *writer, next, Checkpoint::PlannedInvocation)?;
                    let invocation = writer.committed().invocations().last().unwrap().id;
                    let context = self.reviewer.context(writer.committed().session(), &call);
                    let observed = match context {
                        Err(_) => {
                            Observation::failed(ModelError::new(ModelErrorKind::InvalidSelection))
                        }
                        Ok(context) => {
                            let facts = format!(
                                "{}{}",
                                context.instructions(),
                                super::context_policy::review_facts(
                                    writer.committed().session(),
                                    &call,
                                    &address
                                )
                            );
                            let context = Context::new(
                                NonEmptyText::new(facts).unwrap(),
                                context.exchanges().to_vec(),
                                context.tools().to_vec(),
                            )
                            .unwrap();
                            match selection_for(
                                &*self.context_policy,
                                writer.committed().session(),
                                &context,
                            ) {
                                Err(e) => Observation::failed(e),
                                Ok(selection) => ModelClient::<Decision>::invoke(
                                    &self.model_client,
                                    ModelInput {
                                        context: &context,
                                        settings: &self.settings.reviewer_model_settings,
                                        selection: &selection,
                                        view: None,
                                        native: &[],
                                    },
                                    &mut Recorder {
                                        writer: &mut *writer,
                                        invocation,
                                    },
                                )
                                .map_err(|source| {
                                    error(
                                        Some(writer.committed()),
                                        AgentErrorKind::Store {
                                            at: Checkpoint::PreparedInvocation,
                                            source,
                                        },
                                    )
                                })?,
                            }
                        }
                    };
                    let next = writer
                        .committed()
                        .accept_review(invocation, &observed)
                        .map_err(|e| document_error(writer.committed(), e))?;
                    commit(&mut *writer, next, Checkpoint::AcceptedReview)?;
                    let Generation::Complete { output: decision } = observed.generation else {
                        return Err(error(
                            Some(writer.committed()),
                            AgentErrorKind::ReviewBlocked { invocation },
                        ));
                    };
                    let outcome = if decision.should_allow() {
                        self.tools
                            .get(call.name())
                            .expect("accepted advertised tool")
                            .1
                            .execute(call.arguments())
                    } else {
                        ToolOutcome::Failure(ToolError::new(
                            NonEmptyText::new(format!(
                                "Not executed: tool call denied by review. Reason: {}",
                                decision
                                    .reason()
                                    .map(NonEmptyText::as_str)
                                    .unwrap_or("No reason provided")
                            ))
                            .unwrap(),
                        ))
                    };
                    let next = writer
                        .committed()
                        .mutate_session(|s| s.record_tool_result(&address, outcome))
                        .unwrap();
                    commit(&mut *writer, next, Checkpoint::ReturnedTool)?;
                }
                NextAction::AwaitUser => {
                    return Ok(writer
                        .committed()
                        .session()
                        .turns()
                        .last()
                        .unwrap()
                        .messages());
                }
                NextAction::AwaitTool => {
                    return Err(error(
                        Some(writer.committed()),
                        AgentErrorKind::UnexpectedState,
                    ));
                }
            }
        }
    }
}
fn recover(writer: &mut dyn SessionWriter) -> Result<(), AgentError> {
    if let Some(next) = writer
        .committed()
        .recover()
        .map_err(|e| document_error(writer.committed(), e))?
    {
        commit(writer, next, Checkpoint::Recovery)?;
    }
    Ok(())
}
fn selection_for<O>(
    policy: &dyn ContextPolicy,
    session: &Session,
    context: &Context<O>,
) -> Result<ContextSelection, ModelError> {
    let mut selection = policy.select(session);
    let required = RecentContextPolicy::default()
        .select(session)
        .mandatory_turns;
    if required
        .iter()
        .any(|id| !selection.closed_turns.contains(id))
    {
        return Err(ModelError::new(ModelErrorKind::InvalidSelection));
    }
    for id in required {
        if !selection.mandatory_turns.contains(&id) {
            selection.mandatory_turns.push(id);
        }
    }
    let closed = session
        .closed_context_exchanges()
        .map_err(|_| ModelError::new(ModelErrorKind::InvalidSelection))?;
    if context
        .exchanges()
        .iter()
        .any(|e| e.outcome().is_some() && !closed.contains(e))
    {
        return Err(ModelError::new(ModelErrorKind::InvalidSelection));
    }
    let available: Vec<_> = context
        .exchanges()
        .iter()
        .filter(|e| e.outcome().is_some())
        .map(|e| e.turn_id())
        .collect();
    if selection
        .mandatory_turns
        .iter()
        .any(|id| !available.contains(id))
        || available
            .iter()
            .any(|id| !session.turns().iter().any(|t| t.id() == *id))
    {
        return Err(ModelError::new(ModelErrorKind::InvalidSelection));
    }
    selection.closed_turns.retain(|id| available.contains(id));
    Ok(selection)
}
fn generation_error<O>(generation: &Generation<O>, role: ModelRole) -> Option<AgentErrorKind> {
    match generation {
        Generation::Failed(source) => Some(AgentErrorKind::Model {
            role,
            source: source.clone(),
        }),
        Generation::Refused { reason } => Some(AgentErrorKind::Refused {
            role,
            reason: reason.clone(),
        }),
        Generation::Incomplete { reason } => Some(AgentErrorKind::Incomplete {
            role,
            reason: *reason,
        }),
        Generation::Complete { .. } => None,
    }
}
#[derive(Debug)]
pub struct AgentError {
    kind: AgentErrorKind,
    progress: Option<Box<Session>>,
}
impl AgentError {
    pub fn kind(&self) -> &AgentErrorKind {
        &self.kind
    }
    /// Last confirmed committed facts. A failed publication may have changed disk state.
    pub fn progress(&self) -> Option<&Session> {
        self.progress.as_deref()
    }
}
impl fmt::Display for AgentError {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        self.kind.fmt(f)
    }
}
impl Error for AgentError {
    fn source(&self) -> Option<&(dyn Error + 'static)> {
        Some(&self.kind)
    }
}

#[derive(Debug)]
pub enum AgentErrorKind {
    Document(DocumentError),
    ReviewBlocked {
        invocation: u64,
    },
    DuplicateTool(ToolName),
    CatalogMismatch(ToolName),
    UnknownTool(ToolCall),
    Store {
        at: Checkpoint,
        source: SessionStoreError,
    },
    Blocked {
        at: Checkpoint,
    },
    Context {
        role: ModelRole,
        source: ContextError,
    },
    Model {
        role: ModelRole,
        source: ModelError,
    },
    Refused {
        role: ModelRole,
        reason: Option<NonEmptyText>,
    },
    Incomplete {
        role: ModelRole,
        reason: IncompleteReason,
    },
    IterationLimit,
    RejectedOutput(SessionError),
    UnexpectedState,
}
impl fmt::Display for AgentErrorKind {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        match self {
            Self::Document(source) => write!(f, "{source}"),
            Self::ReviewBlocked { invocation } => write!(
                f,
                "review failure requires acknowledgement for invocation {invocation}"
            ),
            Self::DuplicateTool(name) => write!(f, "duplicate tool {name}"),
            Self::CatalogMismatch(name) => write!(f, "catalog mismatch for {name}"),
            Self::UnknownTool(call) => write!(f, "unadvertised tool {}", call.name()),
            Self::Store { at, source } => write!(f, "agent save/load at {at:?}: {source}"),
            Self::Blocked { at } => write!(f, "agent session blocked at {at:?}"),
            Self::Context { role, source } => write!(f, "{role:?} context: {source}"),
            Self::Model { role, source } => write!(f, "{role:?}: {source}"),
            Self::Refused { role, .. } => write!(f, "{role:?} refused generation"),
            Self::Incomplete { role, reason } => {
                write!(f, "{role:?} incomplete generation ({reason:?})")
            }
            Self::IterationLimit => f.write_str("assistant iteration limit reached"),
            Self::RejectedOutput(source) => write!(f, "session transition rejected: {source}"),
            Self::UnexpectedState => f.write_str("unexpected agent session state"),
        }
    }
}
impl Error for AgentErrorKind {
    fn source(&self) -> Option<&(dyn Error + 'static)> {
        match self {
            Self::Store { source, .. } => Some(source),
            Self::Context { source, .. } => Some(source),
            Self::Model { source, .. } => Some(source),
            Self::RejectedOutput(source) => Some(source),
            _ => None,
        }
    }
}

#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum ModelRole {
    Assistant,
    Reviewer,
}
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum Checkpoint {
    Recovery,
    PlannedInvocation,
    PreparedInvocation,
    AcceptedReview,
    ReviewAcknowledgement,
    EffectResolution,
    NewSession,
    ListSessions,
    LoadSession,
    StartedTurn,
    AcceptedAssistant,
    StartedTool,
    ReturnedTool,
    ClosedTurn,
}
