use super::{
    AssistantMessage, CallRef, Decision, GenerationId, Message, NonEmptyText, SessionId, Timestamp,
    ToolCall, ToolCallId, ToolCalls, ToolError, ToolName, ToolOutcome, ToolResult, TurnId,
    UserMessage, ValueError,
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

/// A reviewer generation and its stated reason. The enclosing state says whether it allowed the call.
#[derive(Clone, Debug, PartialEq, Eq)]
pub struct Review {
    reviewer: GenerationId,
    reason: Option<NonEmptyText>,
}
impl Review {
    pub fn new(reviewer: GenerationId, reason: Option<NonEmptyText>) -> Self {
        Self { reviewer, reason }
    }
    pub fn reviewer(&self) -> GenerationId {
        self.reviewer
    }
    pub fn reason(&self) -> Option<&NonEmptyText> {
        self.reason.as_ref()
    }
}

/// Review is separate from dispatch. `Started` is recorded immediately before the tool runs,
/// so a `Started` call without a return has an unknown effect. Earlier states prove the
/// target was not dispatched.
#[derive(Clone, Debug, PartialEq, Eq)]
pub enum ExecutionState {
    Requested,
    Denied(Review),
    Approved(Review),
    Started {
        review: Review,
        at: Timestamp,
    },
    Returned {
        review: Review,
        started_at: Timestamp,
        at: Timestamp,
        outcome: ToolOutcome,
    },
}
impl ExecutionState {
    pub fn review(&self) -> Option<&Review> {
        match self {
            Self::Requested => None,
            Self::Denied(review)
            | Self::Approved(review)
            | Self::Started { review, .. }
            | Self::Returned { review, .. } => Some(review),
        }
    }
    /// The result reported to the model: the actual return, or the controller's denial.
    pub fn protocol_outcome(&self) -> Option<ToolOutcome> {
        match self {
            Self::Returned { outcome, .. } => Some(outcome.clone()),
            Self::Denied(review) => {
                let reason = review
                    .reason()
                    .map(NonEmptyText::as_str)
                    .unwrap_or("Review denied the call without providing a reason.");
                Some(ToolOutcome::Failure(ToolError::new(
                    NonEmptyText::new(format!(
                        "Not executed: tool call denied by review. Reason: {reason}"
                    ))
                    .expect("nonblank denial"),
                )))
            }
            _ => None,
        }
    }
    fn settled(&self) -> bool {
        matches!(self, Self::Denied(_) | Self::Returned { .. })
    }
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
    generation: GenerationId,
    call: &'a ToolCall,
    record: &'a ExecutionRecord,
}
impl<'a> ToolExecution<'a> {
    pub fn call(self) -> &'a ToolCall {
        self.call
    }
    pub fn call_ref(self) -> CallRef {
        CallRef::new(self.generation, self.call.id().clone())
    }
    pub fn state(self) -> &'a ExecutionState {
        &self.record.state
    }
    pub fn resolution(self) -> Option<&'a EffectResolution> {
        self.record.resolution.as_ref()
    }
}

/// Accepted assistant output and the execution facts of its calls. `source_generation`
/// is the only link between the accepted output and the invocation that produced it.
#[derive(Clone, Debug, PartialEq, Eq)]
pub struct Step {
    source: GenerationId,
    assistant: AssistantMessage,
    executions: Vec<ExecutionRecord>,
}
impl Step {
    fn new(source: GenerationId, assistant: AssistantMessage) -> Self {
        let executions = assistant.calls().map_or_else(Vec::new, |calls| {
            calls
                .as_slice()
                .iter()
                .map(|_| ExecutionRecord {
                    state: ExecutionState::Requested,
                    resolution: None,
                })
                .collect()
        });
        Self {
            source,
            assistant,
            executions,
        }
    }
    pub fn source_generation(&self) -> GenerationId {
        self.source
    }
    pub fn assistant(&self) -> &AssistantMessage {
        &self.assistant
    }
    pub fn executions(&self) -> impl Iterator<Item = ToolExecution<'_>> {
        self.calls()
            .iter()
            .zip(&self.executions)
            .map(|(call, record)| ToolExecution {
                generation: self.source,
                call,
                record,
            })
    }
    fn calls(&self) -> &[ToolCall] {
        self.assistant.calls().map_or(&[], ToolCalls::as_slice)
    }
    fn call_index(&self, call: &CallRef) -> Result<usize, SessionError> {
        if call.generation() != self.source {
            return Err(SessionError::UnknownCall);
        }
        self.calls()
            .iter()
            .position(|c| c.id() == call.call_id())
            .ok_or(SessionError::UnknownCall)
    }
    fn pending_index(&self) -> Option<usize> {
        self.executions.iter().position(|e| !e.state.settled())
    }
    fn next_action(&self) -> NextAction {
        let Some(index) = self.pending_index() else {
            return NextAction::AskModel;
        };
        let call = CallRef::new(self.source, self.calls()[index].id().clone());
        match self.executions[index].state {
            ExecutionState::Requested => NextAction::Review(call),
            ExecutionState::Approved(_) => NextAction::Execute(call),
            _ => NextAction::AwaitTool,
        }
    }
    fn next_index(&self, call: &CallRef) -> Result<usize, SessionError> {
        let index = self.call_index(call)?;
        if self.pending_index() != Some(index) && !self.executions[index].state.settled() {
            return Err(SessionError::OutOfOrder);
        }
        Ok(index)
    }
    fn review(&mut self, call: &CallRef, review: Review, allow: bool) -> Result<(), SessionError> {
        let index = self.next_index(call)?;
        let state = &mut self.executions[index].state;
        if *state != ExecutionState::Requested {
            return Err(SessionError::AlreadyReviewed);
        }
        *state = if allow {
            ExecutionState::Approved(review)
        } else {
            ExecutionState::Denied(review)
        };
        Ok(())
    }
    fn start_tool(&mut self, call: &CallRef, at: Timestamp) -> Result<&ToolCall, SessionError> {
        let index = self.next_index(call)?;
        let state = &mut self.executions[index].state;
        let review = match state {
            ExecutionState::Approved(review) => review.clone(),
            ExecutionState::Requested | ExecutionState::Denied(_) => {
                return Err(SessionError::NotApproved);
            }
            ExecutionState::Started { .. } => return Err(SessionError::AlreadyStarted),
            ExecutionState::Returned { .. } => return Err(SessionError::AlreadyReturned),
        };
        *state = ExecutionState::Started { review, at };
        Ok(&self.calls()[index])
    }
    fn record_tool_result(
        &mut self,
        call: &CallRef,
        outcome: ToolOutcome,
        at: Timestamp,
    ) -> Result<(), SessionError> {
        let index = self.call_index(call)?;
        let state = &mut self.executions[index].state;
        let (review, started_at) = match state {
            ExecutionState::Started { review, at } => (review.clone(), *at),
            ExecutionState::Returned { .. } => return Err(SessionError::AlreadyReturned),
            _ => return Err(SessionError::NotStarted),
        };
        *state = ExecutionState::Returned {
            review,
            started_at,
            at,
            outcome,
        };
        Ok(())
    }
    fn resolve_effect(
        &mut self,
        call: &CallRef,
        resolution: &EffectResolution,
    ) -> Result<(), SessionError> {
        let index = self.call_index(call)?;
        let execution = &mut self.executions[index];
        if !matches!(execution.state, ExecutionState::Started { .. }) {
            return Err(SessionError::EffectNotUnknown);
        }
        match &execution.resolution {
            Some(prior) if prior != resolution => Err(SessionError::ConflictingResolution),
            Some(_) => Ok(()),
            None => {
                execution.resolution = Some(resolution.clone());
                Ok(())
            }
        }
    }
    /// The assistant message followed by each call's protocol result, actual or denial.
    pub fn messages(&self) -> Vec<Message> {
        let mut messages = vec![Message::Assistant(self.assistant.clone())];
        for execution in self.executions() {
            if let Some(outcome) = execution.state().protocol_outcome() {
                messages.push(Message::Tool(ToolResult::new(
                    execution.call().id().clone(),
                    outcome,
                )));
            }
        }
        messages
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
    Review(CallRef),
    Execute(CallRef),
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
    AlreadyReviewed,
    NotApproved,
    AlreadyStarted,
    NotStarted,
    AlreadyReturned,
    DuplicateCallId(ToolCallId),
    DuplicateGeneration(GenerationId),
    InvalidGeneration(GenerationId),
    DuplicateTool(ToolName),
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
            Self::OutOfOrder => f.write_str("calls must be reviewed and run in declaration order"),
            Self::AlreadyReviewed => f.write_str("the call has already been reviewed"),
            Self::NotApproved => f.write_str("the call has not been approved"),
            Self::AlreadyStarted => f.write_str("the call has already started"),
            Self::NotStarted => f.write_str("the call has not started"),
            Self::AlreadyReturned => f.write_str("the call already has a result"),
            Self::DuplicateCallId(id) => {
                write!(f, "tool call ID {id} was already used in the active turn")
            }
            Self::DuplicateGeneration(id) => {
                write!(f, "generation {} already produced a step", id.get())
            }
            Self::DuplicateTool(name) => write!(f, "duplicate tool definition {name}"),
            Self::InvalidGeneration(id) => {
                write!(f, "generation {} is not an accepted source", id.get())
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
    fn current_step(&mut self, id: TurnId) -> Result<&mut Step, SessionError> {
        self.active(id)?
            .record
            .steps
            .last_mut()
            .ok_or(SessionError::UnknownCall)
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
        step.next_action()
    }
    /// Call IDs must be new within the active turn, whose calls form one native protocol
    /// bundle. Closed turns may have used the same IDs.
    pub fn record_assistant(
        &mut self,
        id: TurnId,
        source: GenerationId,
        message: AssistantMessage,
    ) -> Result<(), SessionError> {
        let active = self.active(id)?;
        if active
            .steps()
            .last()
            .is_some_and(|step| step.pending_index().is_some())
        {
            return Err(SessionError::NotAwaitingModel);
        }
        if let Some(calls) = message.calls() {
            for call in calls.as_slice() {
                if active
                    .steps()
                    .iter()
                    .flat_map(Step::calls)
                    .any(|prior| prior.id() == call.id())
                {
                    return Err(SessionError::DuplicateCallId(call.id().clone()));
                }
            }
        }
        if self
            .records()
            .flat_map(|record| &record.steps)
            .any(|step| step.source == source)
        {
            return Err(SessionError::DuplicateGeneration(source));
        }
        let completed = matches!(message, AssistantMessage::Answer(_));
        self.active
            .as_mut()
            .expect("validated active turn")
            .record
            .steps
            .push(Step::new(source, message));
        if completed {
            self.close(TurnOutcome::Completed);
        }
        Ok(())
    }
    /// Records the reviewer's decision. Neither approval nor denial dispatches the target.
    pub fn review_tool(
        &mut self,
        id: TurnId,
        call: &CallRef,
        reviewer: GenerationId,
        decision: &Decision,
    ) -> Result<(), SessionError> {
        self.current_step(id)?.review(
            call,
            Review::new(reviewer, decision.reason().cloned()),
            decision.should_allow(),
        )
    }
    /// Records dispatch intent before returning call data. No tool is executed here.
    /// A crash after this transition leaves an unknown effect even before I/O.
    pub fn start_tool(
        &mut self,
        id: TurnId,
        call: &CallRef,
        at: Timestamp,
    ) -> Result<&ToolCall, SessionError> {
        self.current_step(id)?.start_tool(call, at)
    }
    pub fn record_tool_result(
        &mut self,
        id: TurnId,
        call: &CallRef,
        outcome: ToolOutcome,
        at: Timestamp,
    ) -> Result<(), SessionError> {
        self.current_step(id)?.record_tool_result(call, outcome, at)
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
        call: &CallRef,
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
        turn.record
            .steps
            .iter_mut()
            .find(|step| step.source == call.generation())
            .ok_or(SessionError::UnknownCall)?
            .resolve_effect(call, &resolution)
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
    pub source_generation: u64,
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
    /// An active turn stays active; only explicit recovery closes it.
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
    fn restore_turn(&mut self, snapshot: TurnSnapshot, active: bool) -> Result<(), RestoreError> {
        if active != snapshot.outcome.is_none() {
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
            if self.active.is_none() {
                return Err(RestoreError::InvalidSnapshot(
                    "an answer must be the last step",
                ));
            }
            let source = GenerationId::new(step.source_generation)?;
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
            let calls: Vec<_> = message
                .calls()
                .map_or(&[][..], ToolCalls::as_slice)
                .iter()
                .map(|call| CallRef::new(source, call.id().clone()))
                .collect();
            if calls.len() != step.executions.len() {
                return Err(RestoreError::InvalidSnapshot(
                    "call and execution counts differ",
                ));
            }
            for (call, execution) in calls.iter().zip(&step.executions) {
                if call.call_id().as_str() != execution.call_id {
                    return Err(RestoreError::InvalidSnapshot(
                        "call and execution IDs differ",
                    ));
                }
                if execution.resolution.is_some()
                    && (active || !matches!(execution.state, ExecutionState::Started { .. }))
                {
                    return Err(RestoreError::InvalidSnapshot(
                        "resolution requires a closed unknown effect",
                    ));
                }
            }
            self.record_assistant(id, source, message)?;
            for (call, execution) in calls.into_iter().zip(step.executions) {
                self.restore_execution(id, &call, execution.state)?;
                if let Some(resolution) = execution.resolution {
                    resolutions.push((call, resolution));
                }
            }
        }
        match snapshot.outcome {
            Some(TurnOutcome::Completed) | None => {
                if self.active.is_some() == snapshot.outcome.is_some() {
                    return Err(RestoreError::InvalidSnapshot(
                        "only a completed turn ends with an answer",
                    ));
                }
            }
            Some(TurnOutcome::Interrupted(reason)) => self.interrupt(id, reason)?,
            Some(TurnOutcome::Failed(failure)) => self.fail(id, failure)?,
        }
        for (call, resolution) in resolutions {
            self.resolve_effect(id, &call, resolution)?;
        }
        Ok(())
    }
    fn restore_execution(
        &mut self,
        id: TurnId,
        call: &CallRef,
        state: ExecutionState,
    ) -> Result<(), SessionError> {
        let step = self.current_step(id)?;
        match state {
            ExecutionState::Requested => Ok(()),
            ExecutionState::Denied(review) => step.review(call, review, false),
            ExecutionState::Approved(review) => step.review(call, review, true),
            ExecutionState::Started { review, at } => {
                step.review(call, review, true)?;
                step.start_tool(call, at).map(|_| ())
            }
            ExecutionState::Returned {
                review,
                started_at,
                at,
                outcome,
            } => {
                step.review(call, review, true)?;
                step.start_tool(call, started_at)?;
                step.record_tool_result(call, outcome, at)
            }
        }
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
                source_generation: step.source.get(),
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
