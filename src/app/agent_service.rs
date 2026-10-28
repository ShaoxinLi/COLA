use super::{
    ArchiveChange, CommitFault, ContextLimits, ContextPlan, Disposition, Generation,
    IncompleteReason, InvocationRole, ModelClient, ModelError, ModelErrorKind, ModelSettings,
    ObservedReply, SessionArchive, SessionStore, SessionStoreError, SessionTransaction,
};
use crate::domain::{
    AssistantMessage, Decision, Message, NextAction, NonEmptyText, SessionError, SessionId,
    StopReason, Timestamp, Tool, ToolCall, ToolDefinition, ToolName, TurnFailure, TurnId,
    UserMessage,
};
use std::{collections::BTreeMap, error::Error, fmt, num::NonZeroU32};

#[derive(Clone, Debug)]
pub struct RoleSettings {
    pub model: ModelSettings,
    pub instructions: NonEmptyText,
    pub limits: ContextLimits,
}

#[derive(Clone, Debug)]
pub struct AgentSettings {
    pub assistant: RoleSettings,
    pub reviewer: RoleSettings,
    pub max_iterations: NonZeroU32,
}

/// Synchronous orchestration from committed archives. No session facts survive only in this service.
pub struct AgentService<M, S> {
    model: M,
    store: S,
    settings: AgentSettings,
    tools: BTreeMap<ToolName, (ToolDefinition, Box<dyn Tool>)>,
}

#[derive(Clone, Debug)]
pub struct CommittedTurn {
    archive: SessionArchive,
    messages: Vec<Message>,
}
impl CommittedTurn {
    pub fn archive(&self) -> &SessionArchive {
        &self.archive
    }
    pub fn messages(&self) -> &[Message] {
        &self.messages
    }
}

#[derive(Clone, Debug)]
pub struct RecoveryReport {
    archive: SessionArchive,
}
impl RecoveryReport {
    pub fn archive(&self) -> &SessionArchive {
        &self.archive
    }
}

impl<M, S> AgentService<M, S>
where
    M: ModelClient<AssistantMessage> + ModelClient<Decision>,
    S: SessionStore,
{
    pub fn new(
        model: M,
        store: S,
        tools: Vec<(ToolDefinition, Box<dyn Tool>)>,
        settings: AgentSettings,
    ) -> Result<Self, AgentError> {
        let mut registered = BTreeMap::new();
        for (definition, tool) in tools {
            let name = definition.name().clone();
            if registered
                .insert(name.clone(), (definition, tool))
                .is_some()
            {
                return Err(AgentError::bare(AgentErrorKind::DuplicateTool(name)));
            }
        }
        Ok(Self {
            model,
            store,
            tools: registered,
            settings,
        })
    }

    pub fn new_session(&self) -> Result<SessionArchive, AgentError> {
        let archive =
            SessionArchive::new(SessionId::new(uuid::Uuid::new_v4().to_string()).unwrap());
        let change = ArchiveChange::initial(archive).map_err(AgentError::store)?;
        self.store.create(&change).map_err(|fault| AgentError {
            kind: AgentErrorKind::Commit(fault),
            durable: None,
            uncommitted: Some(Box::new(change.candidate().clone())),
        })?;
        Ok(change.candidate().clone())
    }

    pub fn inspect(&self, id: &SessionId) -> Result<SessionArchive, AgentError> {
        self.store.inspect(id).map_err(AgentError::store)
    }

    pub fn list_sessions(&self) -> Result<Vec<SessionArchive>, AgentError> {
        self.store.list().map_err(AgentError::store)
    }

    /// Closes abandoned work under exclusive ownership, without model or tool calls.
    pub fn recover(&self, id: &SessionId) -> Result<RecoveryReport, AgentError> {
        let mut transaction = self.store.acquire(id).map_err(AgentError::store)?;
        if !transaction.confirmed().needs_recovery() {
            return Ok(RecoveryReport {
                archive: transaction.confirmed().clone(),
            });
        }
        let mut candidate = transaction.confirmed().clone();
        candidate.abandon_pending(Timestamp::now());
        if let Some(turn) = candidate.session().active_turn().map(|turn| turn.id()) {
            candidate
                .session_mut()
                .interrupt(turn, StopReason::ProcessLost)
                .map_err(|source| transition_error(transaction.as_ref(), source))?;
        }
        let archive = commit(transaction.as_mut(), candidate)?;
        Ok(RecoveryReport { archive })
    }

    /// Runs one new user turn. An interrupted active turn requires explicit recovery first.
    pub fn chat(&self, id: &SessionId, user: UserMessage) -> Result<CommittedTurn, AgentError> {
        self.chat_with_history(id, user, &[])
    }

    /// Keeps the named older closed turns in addition to the default newest-turn selection.
    pub fn chat_with_history(
        &self,
        id: &SessionId,
        user: UserMessage,
        required: &[TurnId],
    ) -> Result<CommittedTurn, AgentError> {
        let mut transaction = self.store.acquire(id).map_err(AgentError::store)?;
        if transaction.confirmed().needs_recovery() {
            return Err(AgentError::at(
                AgentErrorKind::RecoveryRequired,
                transaction.confirmed(),
            ));
        }
        let mut candidate = transaction.confirmed().clone();
        let turn = candidate
            .session_mut()
            .start_turn(user)
            .map_err(|source| transition_error(transaction.as_ref(), source))?;
        commit(transaction.as_mut(), candidate)?;
        let catalog: Vec<_> = self
            .tools
            .values()
            .map(|(definition, _)| definition.clone())
            .collect();
        let mut attempts = 0;
        loop {
            match transaction.confirmed().session().next_action() {
                NextAction::AskModel => {
                    if attempts == self.settings.max_iterations.get() {
                        return Err(close_error(
                            transaction.as_mut(),
                            turn,
                            AgentErrorKind::IterationLimit,
                        )?);
                    }
                    let role = &self.settings.assistant;
                    let plan = match ContextPlan::assistant(
                        transaction.confirmed(),
                        role.instructions.clone(),
                        catalog.clone(),
                        role.limits,
                        required,
                    ) {
                        Ok(plan) => plan,
                        Err(source) => {
                            return Err(close_error(
                                transaction.as_mut(),
                                turn,
                                AgentErrorKind::RejectedOutput(source),
                            )?);
                        }
                    };
                    let request = match ModelClient::<AssistantMessage>::prepare(
                        &self.model,
                        &plan,
                        &role.model,
                    ) {
                        Ok(request) => request,
                        Err(source) => {
                            return Err(close_error(
                                transaction.as_mut(),
                                turn,
                                model_error(ModelRole::Assistant, source),
                            )?);
                        }
                    };
                    let mut candidate = transaction.confirmed().clone();
                    let generation = candidate
                        .begin_generation(InvocationRole::Assistant { turn }, &request)
                        .map_err(AgentError::store)?;
                    commit(transaction.as_mut(), candidate)?;
                    attempts += 1;
                    let ObservedReply {
                        at,
                        receipt,
                        result,
                    } = ModelClient::<AssistantMessage>::invoke(&self.model, &request);
                    let mut candidate = transaction.confirmed().clone();
                    let (disposition, failure) = match result {
                        Ok(Generation::Complete(output)) => {
                            let unknown = output.calls().is_some_and(|calls| {
                                calls
                                    .as_slice()
                                    .iter()
                                    .any(|call| !self.tools.contains_key(call.name()))
                            });
                            let accepted = if unknown {
                                Err(SessionError::UnknownCall)
                            } else {
                                candidate
                                    .session_mut()
                                    .record_assistant(turn, generation, output)
                            };
                            match accepted {
                                Ok(()) => (Disposition::Accepted, None),
                                Err(source) => (
                                    Disposition::Rejected(text("Invalid assistant output")),
                                    Some(AgentErrorKind::RejectedOutput(source)),
                                ),
                            }
                        }
                        other => stopped(ModelRole::Assistant, other),
                    };
                    candidate
                        .record_observation(generation, at, receipt, disposition)
                        .map_err(AgentError::store)?;
                    if let Some(kind) = failure {
                        end_failed_turn(&mut candidate, turn, &kind)
                            .map_err(|source| transition_error(transaction.as_ref(), source))?;
                        let archive = commit(transaction.as_mut(), candidate)?;
                        return Err(AgentError::at(kind, &archive));
                    }
                    commit(transaction.as_mut(), candidate)?;
                }
                NextAction::Review(call) => {
                    let role = &self.settings.reviewer;
                    let plan = match ContextPlan::reviewer(
                        transaction.confirmed(),
                        &call,
                        role.instructions.clone(),
                        catalog.clone(),
                        role.limits,
                        required,
                    ) {
                        Ok(plan) => plan,
                        Err(source) => {
                            return Err(close_error(
                                transaction.as_mut(),
                                turn,
                                AgentErrorKind::RejectedOutput(source),
                            )?);
                        }
                    };
                    let request =
                        match ModelClient::<Decision>::prepare(&self.model, &plan, &role.model) {
                            Ok(request) => request,
                            Err(source) => {
                                return Err(close_error(
                                    transaction.as_mut(),
                                    turn,
                                    model_error(ModelRole::Reviewer, source),
                                )?);
                            }
                        };
                    let mut candidate = transaction.confirmed().clone();
                    let generation = candidate
                        .begin_generation(InvocationRole::Reviewer { call: call.clone() }, &request)
                        .map_err(AgentError::store)?;
                    commit(transaction.as_mut(), candidate)?;
                    let ObservedReply {
                        at,
                        receipt,
                        result,
                    } = ModelClient::<Decision>::invoke(&self.model, &request);
                    let mut candidate = transaction.confirmed().clone();
                    let (disposition, failure) = match result {
                        Ok(Generation::Complete(decision)) => {
                            candidate
                                .session_mut()
                                .review_tool(turn, &call, generation, &decision)
                                .map_err(|source| transition_error(transaction.as_ref(), source))?;
                            (Disposition::Accepted, None)
                        }
                        other => stopped(ModelRole::Reviewer, other),
                    };
                    candidate
                        .record_observation(generation, at, receipt, disposition)
                        .map_err(AgentError::store)?;
                    if let Some(kind) = failure {
                        end_failed_turn(&mut candidate, turn, &kind)
                            .map_err(|source| transition_error(transaction.as_ref(), source))?;
                        let archive = commit(transaction.as_mut(), candidate)?;
                        return Err(AgentError::at(kind, &archive));
                    }
                    commit(transaction.as_mut(), candidate)?;
                }
                NextAction::Execute(call) => {
                    let mut candidate = transaction.confirmed().clone();
                    let target: ToolCall = candidate
                        .session_mut()
                        .start_tool(turn, &call, Timestamp::now())
                        .map_err(|source| transition_error(transaction.as_ref(), source))?
                        .clone();
                    commit(transaction.as_mut(), candidate)?;
                    let outcome = self
                        .tools
                        .get(target.name())
                        .expect("accepted registered call")
                        .1
                        .execute(target.arguments());
                    let mut candidate = transaction.confirmed().clone();
                    candidate
                        .session_mut()
                        .record_tool_result(turn, &call, outcome, Timestamp::now())
                        .map_err(|source| transition_error(transaction.as_ref(), source))?;
                    commit(transaction.as_mut(), candidate)?;
                }
                NextAction::AwaitUser => {
                    let archive = transaction.confirmed().clone();
                    let messages = archive
                        .session()
                        .turns()
                        .last()
                        .expect("completed current turn")
                        .messages();
                    return Ok(CommittedTurn { archive, messages });
                }
                NextAction::AwaitTool => {
                    return Err(AgentError::at(
                        AgentErrorKind::RecoveryRequired,
                        transaction.confirmed(),
                    ));
                }
            }
        }
    }
}

fn text(value: &str) -> NonEmptyText {
    NonEmptyText::new(value).expect("nonblank internal text")
}

fn stopped<O>(
    role: ModelRole,
    result: Result<Generation<O>, ModelError>,
) -> (Disposition, Option<AgentErrorKind>) {
    match result {
        Err(source) => (
            Disposition::Failed(source.clone()),
            Some(model_error(role, source)),
        ),
        Ok(Generation::Refused { reason }) => (
            Disposition::Refused {
                reason: reason.clone(),
            },
            Some(AgentErrorKind::Refused { role, reason }),
        ),
        Ok(Generation::Incomplete { reason }) => (
            Disposition::Incomplete(reason),
            Some(AgentErrorKind::Incomplete { role, reason }),
        ),
        Ok(Generation::Complete(_)) => unreachable!("complete generation handled by caller"),
    }
}
fn model_error(role: ModelRole, source: ModelError) -> AgentErrorKind {
    if source.kind() == ModelErrorKind::ContextCapacity {
        AgentErrorKind::ContextCapacity { role }
    } else {
        AgentErrorKind::Model { role, source }
    }
}
fn transition_error(transaction: &dyn SessionTransaction, source: SessionError) -> AgentError {
    AgentError::at(
        AgentErrorKind::RejectedOutput(source),
        transaction.confirmed(),
    )
}
fn commit(
    transaction: &mut dyn SessionTransaction,
    candidate: SessionArchive,
) -> Result<SessionArchive, AgentError> {
    let before = transaction.confirmed().clone();
    let change = ArchiveChange::next(&before, candidate).map_err(AgentError::store)?;
    let result = match transaction.commit(&change) {
        Err(CommitFault::Uncertain(_)) => transaction.resolve(&change),
        other => other,
    };
    result.map_err(|fault| AgentError {
        kind: AgentErrorKind::Commit(fault),
        durable: Some(Box::new(before)),
        uncommitted: Some(Box::new(change.candidate().clone())),
    })?;
    Ok(transaction.confirmed().clone())
}
fn end_failed_turn(
    archive: &mut SessionArchive,
    turn: TurnId,
    kind: &AgentErrorKind,
) -> Result<(), SessionError> {
    if matches!(kind, AgentErrorKind::IterationLimit) {
        return archive
            .session_mut()
            .interrupt(turn, StopReason::IterationLimit);
    }
    if matches!(kind, AgentErrorKind::ContextCapacity { .. }) {
        return archive
            .session_mut()
            .interrupt(turn, StopReason::ContextCapacity);
    }
    let failure = if matches!(kind, AgentErrorKind::RejectedOutput(_))
        || matches!(kind, AgentErrorKind::Model { source, .. } if matches!(source.kind(), ModelErrorKind::MalformedResponse | ModelErrorKind::UnsupportedResponse))
    {
        TurnFailure::MalformedResponse(text("Invalid assistant or reviewer response"))
    } else {
        TurnFailure::ModelFailure(text("Model request failed"))
    };
    archive.session_mut().fail(turn, failure)
}
fn close_error(
    transaction: &mut dyn SessionTransaction,
    turn: TurnId,
    kind: AgentErrorKind,
) -> Result<AgentError, AgentError> {
    let mut candidate = transaction.confirmed().clone();
    end_failed_turn(&mut candidate, turn, &kind)
        .map_err(|source| transition_error(transaction, source))?;
    let archive = commit(transaction, candidate)?;
    Ok(AgentError::at(kind, &archive))
}

#[derive(Debug)]
pub struct AgentError {
    kind: AgentErrorKind,
    durable: Option<Box<SessionArchive>>,
    uncommitted: Option<Box<SessionArchive>>,
}
impl AgentError {
    pub fn kind(&self) -> &AgentErrorKind {
        &self.kind
    }
    pub fn durable(&self) -> Option<&SessionArchive> {
        self.durable.as_deref()
    }
    pub fn uncommitted(&self) -> Option<&SessionArchive> {
        self.uncommitted.as_deref()
    }
    fn bare(kind: AgentErrorKind) -> Self {
        Self {
            kind,
            durable: None,
            uncommitted: None,
        }
    }
    fn at(kind: AgentErrorKind, archive: &SessionArchive) -> Self {
        Self {
            kind,
            durable: Some(Box::new(archive.clone())),
            uncommitted: None,
        }
    }
    fn store(source: SessionStoreError) -> Self {
        Self::bare(AgentErrorKind::Store(source))
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
    Store(SessionStoreError),
    Commit(CommitFault),
    RecoveryRequired,
    ContextCapacity {
        role: ModelRole,
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
    RejectedOutput(SessionError),
    IterationLimit,
}
impl fmt::Display for AgentErrorKind {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        match self {
            Self::DuplicateTool(name) => write!(f, "duplicate tool {name}"),
            Self::Store(source) => write!(f, "session storage: {source}"),
            Self::Commit(_) => f.write_str("session commit failed"),
            Self::RecoveryRequired => f.write_str("session recovery required"),
            Self::ContextCapacity { role } => write!(f, "{role:?} context capacity exceeded"),
            Self::Model { role, source } => write!(f, "{role:?}: {source}"),
            Self::Refused { role, .. } => write!(f, "{role:?} refused generation"),
            Self::Incomplete { role, reason } => {
                write!(f, "{role:?} incomplete generation ({reason:?})")
            }
            Self::RejectedOutput(source) => write!(f, "session transition rejected: {source}"),
            Self::IterationLimit => f.write_str("assistant iteration limit reached"),
        }
    }
}
impl Error for AgentErrorKind {
    fn source(&self) -> Option<&(dyn Error + 'static)> {
        match self {
            Self::Store(source) => Some(source),
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
