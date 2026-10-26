use super::{
    AssistantMessage, Message, NonEmptyText, SessionId, ToolCall, ToolCallId, ToolCalls, ToolName,
    ToolOutcome, ToolResult, TurnId, UserMessage, ValueError,
};
use serde_json::Value;
use std::{error::Error, fmt};

#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum StopReason {
    UserCancelled,
    ProcessLost,
    ContextCapacity,
    IterationLimit,
}
#[derive(Clone, Debug, PartialEq, Eq)]
pub enum TurnFailure {
    ModelFailure(NonEmptyText),
    MalformedResponse(NonEmptyText),
}
#[derive(Clone, Debug, PartialEq, Eq)]
pub enum TurnOutcome {
    Completed,
    Interrupted(StopReason),
    Failed(TurnFailure),
}

/// `Started` records dispatch intent before I/O. After closure its effect is unknown.
#[derive(Clone, Debug, PartialEq, Eq)]
pub enum ExecutionState {
    NotStarted,
    Started,
    Returned(ToolOutcome),
}
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum EffectDisposition {
    Completed,
    NotPerformed,
}
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum ResolutionSource {
    Observation,
    UserAttestation,
    ApplicationAttestation,
}

/// A later claim about an unknown effect, with its provenance and evidence.
/// This never changes the original execution state or creates a tool result.
#[derive(Clone, Debug, PartialEq, Eq)]
pub struct EffectResolution {
    disposition: EffectDisposition,
    source: ResolutionSource,
    evidence: NonEmptyText,
}
impl EffectResolution {
    pub fn new(
        disposition: EffectDisposition,
        source: ResolutionSource,
        evidence: NonEmptyText,
    ) -> Self {
        Self {
            disposition,
            source,
            evidence,
        }
    }
    pub fn disposition(&self) -> EffectDisposition {
        self.disposition
    }
    pub fn source(&self) -> ResolutionSource {
        self.source
    }
    pub fn evidence(&self) -> &NonEmptyText {
        &self.evidence
    }
}

#[derive(Clone, Debug, PartialEq, Eq)]
struct ExecutionRecord {
    state: ExecutionState,
    resolution: Option<EffectResolution>,
}

/// Read-only access to a call and its separate execution facts.
#[derive(Clone, Copy, Debug)]
pub struct ToolExecution<'a> {
    call: &'a ToolCall,
    record: &'a ExecutionRecord,
}
impl<'a> ToolExecution<'a> {
    pub fn call(self) -> &'a ToolCall {
        self.call
    }
    pub fn state(self) -> &'a ExecutionState {
        &self.record.state
    }
    pub fn resolution(self) -> Option<&'a EffectResolution> {
        self.record.resolution.as_ref()
    }
}

#[derive(Clone, Debug, PartialEq, Eq)]
pub struct Step {
    assistant: AssistantMessage,
    executions: Vec<ExecutionRecord>,
}
impl Step {
    pub fn assistant(&self) -> &AssistantMessage {
        &self.assistant
    }
    pub fn executions(&self) -> impl Iterator<Item = ToolExecution<'_>> {
        self.calls()
            .iter()
            .zip(&self.executions)
            .map(|(call, record)| ToolExecution { call, record })
    }
    fn calls(&self) -> &[ToolCall] {
        self.assistant.calls().map_or(&[], ToolCalls::as_slice)
    }
    fn pending_index(&self) -> Option<usize> {
        self.executions
            .iter()
            .position(|e| !matches!(e.state, ExecutionState::Returned(_)))
    }
    pub(crate) fn messages(&self) -> Vec<Message> {
        let mut messages = vec![Message::Assistant(self.assistant.clone())];
        for execution in self.executions() {
            if let ExecutionState::Returned(outcome) = execution.state() {
                messages.push(Message::Tool(ToolResult::new(
                    execution.call().id().clone(),
                    outcome.clone(),
                )));
            }
        }
        messages
    }
    pub(crate) fn all_returned(&self) -> bool {
        self.pending_index().is_none()
    }
}

#[derive(Clone, Debug, PartialEq, Eq)]
struct TurnRecord {
    id: TurnId,
    user: UserMessage,
    steps: Vec<Step>,
}
impl TurnRecord {
    fn messages(&self) -> Vec<Message> {
        let mut messages = vec![Message::User(self.user.clone())];
        messages.extend(self.steps.iter().flat_map(Step::messages));
        messages
    }
}

#[derive(Clone, Debug, PartialEq, Eq)]
pub struct ActiveTurn {
    record: TurnRecord,
}
#[derive(Clone, Debug, PartialEq, Eq)]
pub struct Turn {
    record: TurnRecord,
    outcome: TurnOutcome,
}
macro_rules! turn_accessors {
    () => {
        pub fn id(&self) -> TurnId {
            self.record.id
        }
        pub fn user(&self) -> &UserMessage {
            &self.record.user
        }
        pub fn steps(&self) -> &[Step] {
            &self.record.steps
        }
        /// All recorded messages, including a partial tool batch when present.
        pub fn messages(&self) -> Vec<Message> {
            self.record.messages()
        }
    };
}
impl ActiveTurn {
    turn_accessors!();
}
impl Turn {
    turn_accessors!();
    pub fn outcome(&self) -> &TurnOutcome {
        &self.outcome
    }
}

#[derive(Clone, Debug, PartialEq, Eq)]
pub enum NextAction {
    AskModel,
    Execute(ToolCallId),
    AwaitTool,
    AwaitUser,
}
#[derive(Clone, Debug, PartialEq, Eq)]
pub enum SessionError {
    ActiveTurnExists,
    NoActiveTurn,
    WrongTurn,
    TurnClosed,
    TurnNotFound,
    TurnIdExhausted,
    NotAwaitingModel,
    UnknownCall,
    OutOfOrder,
    AlreadyStarted,
    NotStarted,
    AlreadyReturned,
    DuplicateCallId(ToolCallId),
    EffectNotUnknown,
    ConflictingResolution,
}
impl fmt::Display for SessionError {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        match self {
            Self::ActiveTurnExists => f.write_str("a turn is already active"),
            Self::NoActiveTurn => f.write_str("there is no active turn"),
            Self::WrongTurn => f.write_str("the supplied turn is not active"),
            Self::TurnClosed => f.write_str("the supplied turn is closed"),
            Self::TurnNotFound => f.write_str("the supplied turn does not exist"),
            Self::TurnIdExhausted => f.write_str("the turn ID sequence is exhausted"),
            Self::NotAwaitingModel => f.write_str("the active turn is waiting for tools"),
            Self::UnknownCall => {
                f.write_str("the call does not belong to the current batch or selected turn")
            }
            Self::OutOfOrder => f.write_str("calls must execute in declaration order"),
            Self::AlreadyStarted => f.write_str("the call has already started"),
            Self::NotStarted => f.write_str("the call has not started"),
            Self::AlreadyReturned => f.write_str("the call already has a result"),
            Self::DuplicateCallId(id) => {
                write!(f, "tool call ID {id} was already used in this session")
            }
            Self::EffectNotUnknown => {
                f.write_str("only a closed call with unknown effect can be resolved")
            }
            Self::ConflictingResolution => {
                f.write_str("the call already has a different resolution")
            }
        }
    }
}
impl Error for SessionError {}

/// Ordered turns with at most one active turn. Every rejected transition is atomic.
#[derive(Clone, Debug, PartialEq, Eq)]
pub struct Session {
    id: SessionId,
    turns: Vec<Turn>,
    active: Option<ActiveTurn>,
}
impl Session {
    pub fn new(id: SessionId) -> Self {
        Self {
            id,
            turns: vec![],
            active: None,
        }
    }
    pub fn id(&self) -> &SessionId {
        &self.id
    }
    pub fn turns(&self) -> &[Turn] {
        &self.turns
    }
    pub fn active_turn(&self) -> Option<&ActiveTurn> {
        self.active.as_ref()
    }
    /// A factual history projection. It can contain unresolved tool requests.
    /// Use `context_exchanges` when constructing model input.
    pub fn messages(&self) -> Vec<Message> {
        self.records().flat_map(TurnRecord::messages).collect()
    }
    fn records(&self) -> impl Iterator<Item = &TurnRecord> {
        self.turns
            .iter()
            .map(|turn| &turn.record)
            .chain(self.active.iter().map(|turn| &turn.record))
    }
    fn active(&mut self, id: TurnId) -> Result<&mut ActiveTurn, SessionError> {
        if self.turns.iter().any(|turn| turn.id() == id) {
            return Err(SessionError::TurnClosed);
        }
        match &mut self.active {
            Some(turn) if turn.id() == id => Ok(turn),
            Some(_) => Err(SessionError::WrongTurn),
            None => Err(SessionError::NoActiveTurn),
        }
    }
    pub fn start_turn(&mut self, user: UserMessage) -> Result<TurnId, SessionError> {
        if self.active.is_some() {
            return Err(SessionError::ActiveTurnExists);
        }
        let next = self
            .turns
            .last()
            .map_or(Some(1), |turn| turn.id().get().checked_add(1))
            .ok_or(SessionError::TurnIdExhausted)?;
        let id = TurnId::new(next).expect("sequence starts at one");
        self.active = Some(ActiveTurn {
            record: TurnRecord {
                id,
                user,
                steps: vec![],
            },
        });
        Ok(id)
    }
    pub fn next_action(&self) -> NextAction {
        let Some(active) = &self.active else {
            return NextAction::AwaitUser;
        };
        let Some(step) = active.steps().last() else {
            return NextAction::AskModel;
        };
        match step.pending_index() {
            None => NextAction::AskModel,
            Some(index) => match step.executions[index].state {
                ExecutionState::NotStarted => NextAction::Execute(step.calls()[index].id().clone()),
                ExecutionState::Started => NextAction::AwaitTool,
                ExecutionState::Returned(_) => unreachable!("pending_index excludes returns"),
            },
        }
    }
    pub fn record_assistant(
        &mut self,
        id: TurnId,
        message: AssistantMessage,
    ) -> Result<(), SessionError> {
        self.active(id)?;
        if self.next_action() != NextAction::AskModel {
            return Err(SessionError::NotAwaitingModel);
        }
        if let Some(calls) = message.calls() {
            for call in calls.as_slice() {
                if self
                    .records()
                    .flat_map(|r| &r.steps)
                    .flat_map(Step::calls)
                    .any(|prior| prior.id() == call.id())
                {
                    return Err(SessionError::DuplicateCallId(call.id().clone()));
                }
            }
        }
        let executions = message.calls().map_or_else(Vec::new, |calls| {
            calls
                .as_slice()
                .iter()
                .map(|_| ExecutionRecord {
                    state: ExecutionState::NotStarted,
                    resolution: None,
                })
                .collect()
        });
        let completed = matches!(message, AssistantMessage::Answer(_));
        self.active
            .as_mut()
            .expect("validated active turn")
            .record
            .steps
            .push(Step {
                assistant: message,
                executions,
            });
        if completed {
            self.close(TurnOutcome::Completed);
        }
        Ok(())
    }
    /// Records dispatch intent before returning call data. No tool is executed here.
    /// A crash after this transition can leave an unknown effect even before I/O.
    pub fn start_tool(
        &mut self,
        id: TurnId,
        call_id: &ToolCallId,
    ) -> Result<&ToolCall, SessionError> {
        let active = self.active(id)?;
        let step = active
            .record
            .steps
            .last_mut()
            .ok_or(SessionError::UnknownCall)?;
        let index = step
            .calls()
            .iter()
            .position(|call| call.id() == call_id)
            .ok_or(SessionError::UnknownCall)?;
        match step.executions[index].state {
            ExecutionState::Started => return Err(SessionError::AlreadyStarted),
            ExecutionState::Returned(_) => return Err(SessionError::AlreadyReturned),
            ExecutionState::NotStarted => {}
        }
        if step.pending_index() != Some(index) {
            return Err(SessionError::OutOfOrder);
        }
        step.executions[index].state = ExecutionState::Started;
        Ok(&step.calls()[index])
    }
    pub fn record_tool_result(
        &mut self,
        id: TurnId,
        result: ToolResult,
    ) -> Result<(), SessionError> {
        let active = self.active(id)?;
        let step = active
            .record
            .steps
            .last_mut()
            .ok_or(SessionError::UnknownCall)?;
        let index = step
            .calls()
            .iter()
            .position(|call| call.id() == result.call_id())
            .ok_or(SessionError::UnknownCall)?;
        match step.executions[index].state {
            ExecutionState::NotStarted => return Err(SessionError::NotStarted),
            ExecutionState::Returned(_) => return Err(SessionError::AlreadyReturned),
            ExecutionState::Started => {}
        }
        if step.pending_index() != Some(index) {
            return Err(SessionError::OutOfOrder);
        }
        step.executions[index].state = ExecutionState::Returned(result.outcome().clone());
        Ok(())
    }
    pub fn interrupt(&mut self, id: TurnId, reason: StopReason) -> Result<(), SessionError> {
        self.active(id)?;
        self.close(TurnOutcome::Interrupted(reason));
        Ok(())
    }
    pub fn fail(&mut self, id: TurnId, failure: TurnFailure) -> Result<(), SessionError> {
        self.active(id)?;
        self.close(TurnOutcome::Failed(failure));
        Ok(())
    }
    fn close(&mut self, outcome: TurnOutcome) {
        let active = self.active.take().expect("closure requires an active turn");
        self.turns.push(Turn {
            record: active.record,
            outcome,
        });
    }
    /// Repeating the identical resolution succeeds without changing state.
    /// A different resolution is rejected, preserving the original evidence.
    pub fn resolve_effect(
        &mut self,
        id: TurnId,
        call_id: &ToolCallId,
        resolution: EffectResolution,
    ) -> Result<(), SessionError> {
        if self.active.as_ref().is_some_and(|turn| turn.id() == id) {
            return Err(SessionError::EffectNotUnknown);
        }
        let turn = self
            .turns
            .iter_mut()
            .find(|turn| turn.id() == id)
            .ok_or(SessionError::TurnNotFound)?;
        for step in &mut turn.record.steps {
            if let Some(index) = step.calls().iter().position(|call| call.id() == call_id) {
                let execution = &mut step.executions[index];
                if execution.state != ExecutionState::Started {
                    return Err(SessionError::EffectNotUnknown);
                }
                match &execution.resolution {
                    Some(prior) if prior != &resolution => {
                        return Err(SessionError::ConflictingResolution);
                    }
                    Some(_) => {}
                    None => execution.resolution = Some(resolution),
                }
                return Ok(());
            }
        }
        Err(SessionError::UnknownCall)
    }
}

/// Freely constructible boundary data. Exporting it does not persist anything.
#[derive(Clone, Debug, PartialEq, Eq)]
pub struct SessionSnapshot {
    pub session_id: String,
    pub turns: Vec<TurnSnapshot>,
    pub active: Option<TurnSnapshot>,
}
/// Closed entries require an outcome. The optional active entry must have none.
#[derive(Clone, Debug, PartialEq, Eq)]
pub struct TurnSnapshot {
    pub id: u64,
    pub user: String,
    pub steps: Vec<StepSnapshot>,
    pub outcome: Option<TurnOutcome>,
}
#[derive(Clone, Debug, PartialEq, Eq)]
pub struct StepSnapshot {
    pub assistant: AssistantSnapshot,
    pub executions: Vec<ExecutionSnapshot>,
}
#[derive(Clone, Debug, PartialEq, Eq)]
pub enum AssistantSnapshot {
    Answer(String),
    ToolRequests {
        text: Option<String>,
        calls: Vec<ToolCallSnapshot>,
    },
}
#[derive(Clone, Debug, PartialEq, Eq)]
pub struct ToolCallSnapshot {
    pub id: String,
    pub name: String,
    pub arguments: Value,
}
#[derive(Clone, Debug, PartialEq, Eq)]
pub struct ExecutionSnapshot {
    pub call_id: String,
    pub state: ExecutionState,
    pub resolution: Option<EffectResolution>,
}

#[derive(Clone, Debug, PartialEq, Eq)]
pub enum RestoreError {
    InvalidValue(ValueError),
    InvalidTransition(SessionError),
    InvalidSnapshot(&'static str),
}
impl fmt::Display for RestoreError {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        match self {
            Self::InvalidValue(error) => write!(f, "invalid snapshot value: {error}"),
            Self::InvalidTransition(error) => write!(f, "invalid snapshot transition: {error}"),
            Self::InvalidSnapshot(reason) => write!(f, "invalid snapshot: {reason}"),
        }
    }
}
impl Error for RestoreError {
    fn source(&self) -> Option<&(dyn Error + 'static)> {
        match self {
            Self::InvalidValue(error) => Some(error),
            Self::InvalidTransition(error) => Some(error),
            Self::InvalidSnapshot(_) => None,
        }
    }
}
impl From<ValueError> for RestoreError {
    fn from(value: ValueError) -> Self {
        Self::InvalidValue(value)
    }
}
impl From<SessionError> for RestoreError {
    fn from(value: SessionError) -> Self {
        Self::InvalidTransition(value)
    }
}

impl Session {
    pub fn snapshot(&self) -> SessionSnapshot {
        SessionSnapshot {
            session_id: self.id.as_str().into(),
            turns: self
                .turns
                .iter()
                .map(|turn| snapshot_turn(&turn.record, Some(turn.outcome.clone())))
                .collect(),
            active: self
                .active
                .as_ref()
                .map(|turn| snapshot_turn(&turn.record, None)),
        }
    }
    /// Validates snapshots by replaying the same public state transitions.
    /// Formerly active work is closed as `Interrupted(ProcessLost)`, never resumed.
    pub fn restore(snapshot: SessionSnapshot) -> Result<Self, RestoreError> {
        let mut session = Self::new(SessionId::new(snapshot.session_id)?);
        for turn in snapshot.turns {
            session.restore_turn(turn, false)?;
        }
        if let Some(turn) = snapshot.active {
            session.restore_turn(turn, true)?;
        }
        Ok(session)
    }
    fn restore_turn(
        &mut self,
        snapshot: TurnSnapshot,
        formerly_active: bool,
    ) -> Result<(), RestoreError> {
        if formerly_active != snapshot.outcome.is_none() {
            return Err(RestoreError::InvalidSnapshot(
                "closed and active outcome mismatch",
            ));
        }
        let expected_id = TurnId::new(snapshot.id)?;
        let id = self.start_turn(UserMessage::new(NonEmptyText::new(snapshot.user)?))?;
        if id != expected_id {
            return Err(RestoreError::InvalidSnapshot(
                "turn IDs must be consecutive starting at one",
            ));
        }
        let mut resolutions = vec![];
        for step in snapshot.steps {
            let message = match step.assistant {
                AssistantSnapshot::Answer(text) => AssistantMessage::answer(text)?,
                AssistantSnapshot::ToolRequests { text, calls } => {
                    let calls = calls
                        .into_iter()
                        .map(|call| {
                            ToolCall::new(
                                ToolCallId::new(call.id)?,
                                ToolName::new(call.name)?,
                                call.arguments,
                            )
                        })
                        .collect::<Result<Vec<_>, ValueError>>()?;
                    AssistantMessage::tool_requests(text, ToolCalls::new(calls)?)
                }
            };
            let call_ids: Vec<_> = message
                .calls()
                .map_or(&[][..], ToolCalls::as_slice)
                .iter()
                .map(|call| call.id().clone())
                .collect();
            if call_ids.len() != step.executions.len() {
                return Err(RestoreError::InvalidSnapshot(
                    "call and execution counts differ",
                ));
            }
            for (call_id, execution) in call_ids.iter().zip(&step.executions) {
                if call_id.as_str() != execution.call_id {
                    return Err(RestoreError::InvalidSnapshot(
                        "call and execution IDs differ",
                    ));
                }
                if execution.resolution.is_some()
                    && (formerly_active || execution.state != ExecutionState::Started)
                {
                    return Err(RestoreError::InvalidSnapshot(
                        "resolution requires a closed unknown effect",
                    ));
                }
            }
            self.record_assistant(id, message)?;
            for (call_id, execution) in call_ids.into_iter().zip(step.executions) {
                match execution.state {
                    ExecutionState::NotStarted => {}
                    ExecutionState::Started => {
                        self.start_tool(id, &call_id)?;
                    }
                    ExecutionState::Returned(outcome) => {
                        self.start_tool(id, &call_id)?;
                        self.record_tool_result(id, ToolResult::new(call_id.clone(), outcome))?;
                    }
                }
                if let Some(resolution) = execution.resolution {
                    resolutions.push((call_id, resolution));
                }
            }
        }
        match snapshot.outcome {
            Some(TurnOutcome::Completed) => {
                if self.active.is_some() || self.turns.last().map(Turn::id) != Some(id) {
                    return Err(RestoreError::InvalidSnapshot(
                        "completed turn must end with an answer",
                    ));
                }
            }
            Some(TurnOutcome::Interrupted(reason)) => self.interrupt(id, reason)?,
            Some(TurnOutcome::Failed(failure)) => self.fail(id, failure)?,
            None => self.interrupt(id, StopReason::ProcessLost)?,
        }
        for (call_id, resolution) in resolutions {
            self.resolve_effect(id, &call_id, resolution)?;
        }
        Ok(())
    }
}
fn snapshot_turn(record: &TurnRecord, outcome: Option<TurnOutcome>) -> TurnSnapshot {
    TurnSnapshot {
        id: record.id.get(),
        user: record.user.text().as_str().into(),
        outcome,
        steps: record
            .steps
            .iter()
            .map(|step| StepSnapshot {
                assistant: match step.assistant() {
                    AssistantMessage::Answer(text) => {
                        AssistantSnapshot::Answer(text.as_str().into())
                    }
                    AssistantMessage::ToolRequests { text, calls } => {
                        AssistantSnapshot::ToolRequests {
                            text: text.as_ref().map(|text| text.as_str().into()),
                            calls: calls
                                .as_slice()
                                .iter()
                                .map(|call| ToolCallSnapshot {
                                    id: call.id().as_str().into(),
                                    name: call.name().as_str().into(),
                                    arguments: Value::Object(call.arguments().clone()),
                                })
                                .collect(),
                        }
                    }
                },
                executions: step
                    .executions()
                    .map(|execution| ExecutionSnapshot {
                        call_id: execution.call().id().as_str().into(),
                        state: execution.state().clone(),
                        resolution: execution.resolution().cloned(),
                    })
                    .collect(),
            })
            .collect(),
    }
}
