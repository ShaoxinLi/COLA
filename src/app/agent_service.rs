use super::{
    Continuation, Generation, IncompleteReason, ModelClient, ModelError, ModelErrorKind,
    ModelSettings, SessionStore, SessionStoreError,
};
use crate::domain::{
    Assistant, AssistantMessage, ContextError, Decision, Message, NextAction, NonEmptyText,
    Session, SessionError, SessionId, StopReason, Tool, ToolCall, ToolCallReviewer, ToolDefinition,
    ToolError, ToolName, ToolOutcome, ToolResult, TurnFailure, TurnId, UserMessage,
};
use std::{collections::BTreeMap, error::Error, fmt, num::NonZeroU32};

#[derive(Clone, Debug)]
pub struct AgentSettings {
    pub assistant_model_settings: ModelSettings,
    pub reviewer_model_settings: ModelSettings,
    pub max_iterations: NonZeroU32,
}

/// Synchronous reviewed dispatch with resident, memory-only native continuations.
/// A dispatch attempt includes review. `Started` never proves target execution.
pub struct AgentService<M, S, A, R> {
    model_client: M,
    session_store: S,
    assistant: A,
    reviewer: R,
    settings: AgentSettings,
    tools: BTreeMap<ToolName, (ToolDefinition, Box<dyn Tool>)>,
    sessions: BTreeMap<SessionId, Entry>,
}

struct Entry {
    session: Session,
    continuation: Option<Continuation<AssistantMessage>>,
    state: RuntimeState,
}

enum RuntimeState {
    Ready,
    Blocked(Checkpoint),
}

impl Entry {
    fn new(session: Session) -> Self {
        Self {
            session,
            continuation: None,
            state: RuntimeState::Ready,
        }
    }

    fn error(&self, kind: AgentErrorKind) -> AgentError {
        AgentError {
            kind,
            progress: Some(Box::new(self.session.clone())),
        }
    }

    fn save(&mut self, store: &impl SessionStore, at: Checkpoint) -> Result<(), AgentError> {
        if let Err(source) = store.save(&self.session) {
            self.state = RuntimeState::Blocked(at);
            return Err(self.error(AgentErrorKind::Store { at, source }));
        }
        Ok(())
    }

    fn close_error(
        &mut self,
        store: &impl SessionStore,
        turn: TurnId,
        kind: AgentErrorKind,
    ) -> AgentError {
        let closed = if matches!(kind, AgentErrorKind::IterationLimit) {
            self.session.interrupt(turn, StopReason::IterationLimit)
        } else {
            let failure = match &kind {
                AgentErrorKind::RejectedOutput(_) | AgentErrorKind::UnknownTool(_) => {
                    TurnFailure::MalformedResponse(
                        NonEmptyText::new("Invalid assistant output").unwrap(),
                    )
                }
                AgentErrorKind::Model { source, .. }
                    if matches!(
                        source.kind(),
                        ModelErrorKind::MalformedResponse | ModelErrorKind::UnsupportedResponse
                    ) =>
                {
                    TurnFailure::MalformedResponse(
                        NonEmptyText::new("Malformed or unsupported assistant response").unwrap(),
                    )
                }
                AgentErrorKind::Context { .. } => TurnFailure::ModelFailure(
                    NonEmptyText::new("Assistant context assembly failed").unwrap(),
                ),
                AgentErrorKind::CatalogMismatch(_) => TurnFailure::ModelFailure(
                    NonEmptyText::new(
                        "Assistant tool catalog does not match registered definitions",
                    )
                    .unwrap(),
                ),
                AgentErrorKind::Refused { .. } => TurnFailure::ModelFailure(
                    NonEmptyText::new("Assistant refused generation").unwrap(),
                ),
                AgentErrorKind::Incomplete { reason, .. } => TurnFailure::ModelFailure(
                    NonEmptyText::new(format!("Assistant generation incomplete ({reason:?})"))
                        .unwrap(),
                ),
                _ => TurnFailure::ModelFailure(
                    NonEmptyText::new("Assistant model request failed").unwrap(),
                ),
            };
            self.session.fail(turn, failure)
        };
        if closed.is_err() {
            self.state = RuntimeState::Blocked(Checkpoint::ClosedTurn);
            return self.error(AgentErrorKind::UnexpectedState);
        }
        match self.save(store, Checkpoint::ClosedTurn) {
            Ok(()) => self.error(kind),
            Err(error) => error,
        }
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
        for (definition, executor) in tools {
            let name = definition.name().clone();
            if registry
                .insert(name.clone(), (definition, executor))
                .is_some()
            {
                return Err(AgentError {
                    kind: AgentErrorKind::DuplicateTool(name),
                    progress: None,
                });
            }
        }
        Ok(Self {
            model_client,
            session_store,
            assistant,
            reviewer,
            settings,
            tools: registry,
            sessions: BTreeMap::new(),
        })
    }

    /// Saves the empty session before returning it. On failure progress includes its new ID.
    pub fn new_session(&mut self) -> Result<Session, AgentError> {
        let session = Session::new(SessionId::new(uuid::Uuid::new_v4().to_string()).unwrap());
        let entry = self
            .sessions
            .entry(session.id().clone())
            .or_insert_with(|| Entry::new(session));
        entry.save(&self.session_store, Checkpoint::NewSession)?;
        Ok(entry.session.clone())
    }

    /// Sorted session facts, with live resident state taking precedence over stored recovery.
    pub fn list_session(&self) -> Result<Vec<Session>, AgentError> {
        let mut sessions: BTreeMap<_, _> = self
            .session_store
            .list()
            .map_err(|source| AgentError {
                kind: AgentErrorKind::Store {
                    at: Checkpoint::ListSessions,
                    source,
                },
                progress: None,
            })?
            .into_iter()
            .map(|session| (session.id().clone(), session))
            .collect();
        sessions.extend(
            self.sessions
                .iter()
                .map(|(id, entry)| (id.clone(), entry.session.clone())),
        );
        Ok(sessions.into_values().collect())
    }

    /// Returns only the newly completed turn, including its user message.
    /// Failed saves and reviewer infrastructure failures block future chats without I/O.
    pub fn chat(
        &mut self,
        session_id: &SessionId,
        user_message: UserMessage,
    ) -> Result<Vec<Message>, AgentError> {
        let entry = match self.sessions.entry(session_id.clone()) {
            std::collections::btree_map::Entry::Occupied(entry) => entry.into_mut(),
            std::collections::btree_map::Entry::Vacant(entry) => {
                let session = self
                    .session_store
                    .load(session_id)
                    .map_err(|source| AgentError {
                        kind: AgentErrorKind::Store {
                            at: Checkpoint::LoadSession,
                            source,
                        },
                        progress: None,
                    })?;
                entry.insert(Entry::new(session))
            }
        };
        if let RuntimeState::Blocked(at) = entry.state {
            return Err(entry.error(AgentErrorKind::Blocked { at }));
        }
        let turn = match entry.session.start_turn(user_message) {
            Ok(turn) => turn,
            Err(source) => {
                entry.state = RuntimeState::Blocked(Checkpoint::StartedTurn);
                return Err(entry.error(AgentErrorKind::RejectedOutput(source)));
            }
        };
        entry.save(&self.session_store, Checkpoint::StartedTurn)?;
        let mut attempts = 0;
        loop {
            match entry.session.next_action() {
                NextAction::AskModel => {
                    if attempts == self.settings.max_iterations.get() {
                        return Err(entry.close_error(
                            &self.session_store,
                            turn,
                            AgentErrorKind::IterationLimit,
                        ));
                    }
                    let accepted = (|| {
                        let context = self.assistant.context(&entry.session).map_err(|source| {
                            AgentErrorKind::Context {
                                role: ModelRole::Assistant,
                                source,
                            }
                        })?;
                        for definition in context.tools() {
                            if self
                                .tools
                                .get(definition.name())
                                .is_none_or(|(registered, _)| registered != definition)
                            {
                                return Err(AgentErrorKind::CatalogMismatch(
                                    definition.name().clone(),
                                ));
                            }
                        }
                        attempts += 1;
                        let generated = ModelClient::<AssistantMessage>::generate(
                            &self.model_client,
                            &context,
                            &self.settings.assistant_model_settings,
                            entry.continuation.as_ref(),
                        );
                        let (output, continuation) = match generated {
                            Ok(Generation::Complete {
                                output,
                                continuation,
                            }) => (output, continuation),
                            other => return Err(generation_error(other, ModelRole::Assistant)),
                        };
                        if let Some(calls) = output.calls() {
                            for call in calls.as_slice() {
                                if !context
                                    .tools()
                                    .iter()
                                    .any(|definition| definition.name() == call.name())
                                {
                                    return Err(AgentErrorKind::UnknownTool(call.clone()));
                                }
                            }
                        }
                        entry
                            .session
                            .record_assistant(turn, output)
                            .map_err(AgentErrorKind::RejectedOutput)?;
                        entry.continuation = continuation;
                        Ok(())
                    })();
                    if let Err(kind) = accepted {
                        return Err(entry.close_error(&self.session_store, turn, kind));
                    }
                    entry.save(&self.session_store, Checkpoint::AcceptedAssistant)?;
                }
                NextAction::Execute(call_id) => {
                    let call = match entry.session.start_tool(turn, &call_id) {
                        Ok(call) => call.clone(),
                        Err(_) => {
                            entry.state = RuntimeState::Blocked(Checkpoint::StartedTool);
                            return Err(entry.error(AgentErrorKind::UnexpectedState));
                        }
                    };
                    entry.save(&self.session_store, Checkpoint::StartedTool)?;
                    let review = self
                        .reviewer
                        .context(&entry.session, &call)
                        .map_err(|source| AgentErrorKind::Context {
                            role: ModelRole::Reviewer,
                            source,
                        });
                    let decision =
                        review.and_then(|context| {
                            match ModelClient::<Decision>::generate(
                                &self.model_client,
                                &context,
                                &self.settings.reviewer_model_settings,
                                None,
                            ) {
                                Ok(Generation::Complete { output, .. }) => Ok(output),
                                other => Err(generation_error(other, ModelRole::Reviewer)),
                            }
                        });
                    let decision = match decision {
                        Ok(decision) => decision,
                        Err(kind) => {
                            entry.state = RuntimeState::Blocked(Checkpoint::StartedTool);
                            return Err(entry.error(kind));
                        }
                    };
                    let outcome = if decision.should_allow() {
                        self.tools
                            .get(call.name())
                            .expect("accepted registered call")
                            .1
                            .execute(call.arguments())
                    } else {
                        let reason = decision
                            .reason()
                            .map(NonEmptyText::as_str)
                            .unwrap_or("Review denied the call without providing a reason.");
                        ToolOutcome::Failure(ToolError::new(
                            NonEmptyText::new(format!(
                                "Not executed: tool call denied by review. Reason: {reason}"
                            ))
                            .unwrap(),
                        ))
                    };
                    if entry
                        .session
                        .record_tool_result(turn, ToolResult::new(call_id, outcome))
                        .is_err()
                    {
                        entry.state = RuntimeState::Blocked(Checkpoint::ReturnedTool);
                        return Err(entry.error(AgentErrorKind::UnexpectedState));
                    }
                    entry.save(&self.session_store, Checkpoint::ReturnedTool)?;
                }
                NextAction::AwaitUser => {
                    return Ok(entry
                        .session
                        .turns()
                        .last()
                        .expect("completed current turn")
                        .messages());
                }
                NextAction::AwaitTool => {
                    entry.state = RuntimeState::Blocked(Checkpoint::StartedTool);
                    return Err(entry.error(AgentErrorKind::UnexpectedState));
                }
            }
        }
    }
}

fn generation_error<O>(
    generation: Result<Generation<O>, ModelError>,
    role: ModelRole,
) -> AgentErrorKind {
    match generation {
        Err(source) => AgentErrorKind::Model { role, source },
        Ok(Generation::Refused { reason }) => AgentErrorKind::Refused { role, reason },
        Ok(Generation::Incomplete { reason }) => AgentErrorKind::Incomplete { role, reason },
        Ok(Generation::Complete { .. }) => unreachable!("complete output handled by caller"),
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
    /// Actual resident facts. This does not assert that the latest save succeeded.
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
    NewSession,
    ListSessions,
    LoadSession,
    StartedTurn,
    AcceptedAssistant,
    StartedTool,
    ReturnedTool,
    ClosedTurn,
}
