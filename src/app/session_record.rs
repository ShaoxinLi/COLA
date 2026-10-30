use super::{
    Generation, ModelErrorKind, ModelSettings, NativeStep, Observation, PreparedInvocation,
};
use crate::domain::*;
use serde::{Deserialize, Serialize};

pub fn now_millis() -> u64 {
    std::time::SystemTime::now()
        .duration_since(std::time::UNIX_EPOCH)
        .unwrap_or_default()
        .as_millis() as u64
}
#[derive(Clone, Debug, PartialEq, Eq, Serialize, Deserialize)]
pub struct ModelArtifact {
    pub format: String,
    pub version: u32,
    pub bytes: Vec<u8>,
}
#[derive(Clone, Debug, PartialEq, Eq, Serialize, Deserialize)]
pub struct RequestedModel {
    pub route: String,
    pub settings: ModelSettings,
}
#[derive(Clone, Debug, PartialEq, Eq, Serialize, Deserialize)]
pub struct AuditAddress {
    pub session: String,
    pub turn: u64,
    pub step: u64,
    pub native: String,
}
impl From<&CallAddress> for AuditAddress {
    fn from(a: &CallAddress) -> Self {
        Self {
            session: a.session.as_str().into(),
            turn: a.turn.get(),
            step: a.step,
            native: a.native.as_str().into(),
        }
    }
}
impl AuditAddress {
    pub fn address(&self) -> Result<CallAddress, ValueError> {
        Ok(CallAddress {
            session: SessionId::new(self.session.clone())?,
            turn: TurnId::new(self.turn)?,
            step: self.step,
            native: ToolCallId::new(self.native.clone())?,
        })
    }
}
#[derive(Clone, Debug, PartialEq, Eq, Serialize, Deserialize)]
pub enum InvocationTarget {
    Assistant { turn: u64, step: u64 },
    Reviewer { call: AuditAddress },
}
impl InvocationTarget {
    pub fn turn(&self) -> u64 {
        match self {
            Self::Assistant { turn, .. } => *turn,
            Self::Reviewer { call } => call.turn,
        }
    }
    pub fn is_reviewer(&self) -> bool {
        matches!(self, Self::Reviewer { .. })
    }
}
#[derive(Clone, Debug, PartialEq, Eq, Serialize, Deserialize)]
pub struct TurnView {
    pub turn: u64,
    pub selection: Vec<u64>,
    pub policy_version: u32,
    pub frozen_prefix: ModelArtifact,
    pub requested: RequestedModel,
}
#[derive(Clone, Debug, PartialEq, Eq, Serialize, Deserialize)]
pub struct RequestEvidence {
    pub selection: Vec<u64>,
    pub policy_version: u32,
    pub renderer_version: u32,
    pub body: ModelArtifact,
    pub started_at: u64,
}
#[derive(Clone, Debug, PartialEq, Eq, Serialize, Deserialize)]
pub enum CaptureCompleteness {
    Complete,
    NotReturned,
    Partial { captured_bytes: usize },
}
#[derive(Clone, Debug, PartialEq, Eq, Serialize, Deserialize)]
pub struct ResponseEvidence {
    pub reported_model: Option<String>,
    /// The provider response body ID, not an HTTP request header ID.
    pub provider_request_id: Option<String>,
    pub http_status: Option<u16>,
    pub usage: Option<ModelArtifact>,
    pub response: Option<ModelArtifact>,
    pub capture: CaptureCompleteness,
    pub finished_at: u64,
}
impl ResponseEvidence {
    pub fn not_returned() -> Self {
        Self {
            reported_model: None,
            provider_request_id: None,
            http_status: None,
            usage: None,
            response: None,
            capture: CaptureCompleteness::NotReturned,
            finished_at: now_millis(),
        }
    }
}
#[derive(Clone, Debug, PartialEq, Eq, Serialize, Deserialize)]
pub enum RecordedOutcome {
    AssistantAccepted,
    Review { allow: bool, reason: Option<String> },
    Refused,
    Incomplete(super::IncompleteReason),
    Failed(super::ModelError),
    PreparationFailed(super::ModelError),
    OutputRejected { reason: String },
}
#[derive(Clone, Debug, PartialEq, Eq, Serialize, Deserialize)]
pub enum InvocationState {
    Planned,
    Prepared(RequestEvidence),
    Finished {
        request: Option<RequestEvidence>,
        response: Box<ResponseEvidence>,
        outcome: RecordedOutcome,
    },
    Interrupted {
        request: Option<RequestEvidence>,
        response_unknown: bool,
    },
}
#[derive(Clone, Debug, PartialEq, Eq, Serialize, Deserialize)]
pub struct Invocation {
    pub id: u64,
    pub target: InvocationTarget,
    pub requested: RequestedModel,
    pub state: InvocationState,
}
impl Invocation {
    pub fn blocks_review(&self) -> bool {
        self.target.is_reviewer()
            && match &self.state {
                InvocationState::Finished { outcome, .. } => {
                    !matches!(outcome, RecordedOutcome::Review { .. })
                }
                InvocationState::Interrupted {
                    response_unknown, ..
                } => *response_unknown,
                _ => false,
            }
    }
}
#[derive(Clone, Debug, PartialEq, Eq, Serialize, Deserialize)]
pub struct ReviewFailureAcknowledgement {
    pub invocation: u64,
    pub evidence: String,
    pub acknowledged_at: u64,
    pub actor: Option<String>,
}
#[derive(Clone, Debug, PartialEq, Eq)]
pub struct SessionRecord {
    revision: u64,
    session: Session,
    invocations: Vec<Invocation>,
    views: Vec<TurnView>,
    acknowledgements: Vec<ReviewFailureAcknowledgement>,
}
#[derive(Clone, Debug, PartialEq, Eq)]
pub enum RecordError {
    InvalidState,
    InvalidEvidence,
    ConflictingAcknowledgement,
    NoReviewFailure,
}
impl std::fmt::Display for RecordError {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        write!(f, "document {self:?}")
    }
}
impl std::error::Error for RecordError {}
impl SessionRecord {
    pub fn new(id: SessionId) -> Self {
        Self {
            revision: 0,
            session: Session::new(id),
            invocations: vec![],
            views: vec![],
            acknowledgements: vec![],
        }
    }
    pub fn revision(&self) -> u64 {
        self.revision
    }
    pub fn session(&self) -> &Session {
        &self.session
    }
    pub fn invocations(&self) -> &[Invocation] {
        &self.invocations
    }
    pub fn views(&self) -> &[TurnView] {
        &self.views
    }
    pub fn acknowledgements(&self) -> &[ReviewFailureAcknowledgement] {
        &self.acknowledgements
    }
    pub fn review_block(&self) -> Option<u64> {
        self.invocations
            .iter()
            .find(|i| {
                i.blocks_review() && !self.acknowledgements.iter().any(|a| a.invocation == i.id)
            })
            .map(|i| i.id)
    }
    fn next(&self) -> Self {
        let mut next = self.clone();
        next.revision = self.revision.checked_add(1).expect("revision exhausted");
        next
    }
    pub(crate) fn mutate_session(
        &self,
        operation: impl FnOnce(&mut Session) -> Result<(), SessionError>,
    ) -> Result<Self, SessionError> {
        let mut next = self.next();
        operation(&mut next.session)?;
        Ok(next)
    }
    pub(crate) fn plan(
        &self,
        target: InvocationTarget,
        requested: RequestedModel,
    ) -> Result<Self, RecordError> {
        if self
            .session
            .active_turn()
            .is_none_or(|t| t.id().get() != target.turn())
            || self.invocations.last().is_some_and(|i| {
                matches!(
                    i.state,
                    InvocationState::Planned | InvocationState::Prepared(_)
                )
            })
        {
            return Err(RecordError::InvalidState);
        }
        let active = self
            .session
            .active_turn()
            .ok_or(RecordError::InvalidState)?;
        match &target {
            InvocationTarget::Assistant { step, .. }
                if *step == active.steps().len() as u64 + 1
                    && self.session.next_action() == NextAction::AskModel => {}
            InvocationTarget::Reviewer { call } => {
                let address = call.address().map_err(|_| RecordError::InvalidEvidence)?;
                if address.session != *self.session.id()
                    || address.step != active.steps().len() as u64
                    || address.step == 0
                    || active.steps().last().is_none_or(|s| {
                        !s.executions().any(|e| {
                            e.call().id() == &address.native
                                && *e.state() == ExecutionState::Started
                        })
                    })
                    || self.invocations.iter().any(|i| i.target == target)
                {
                    return Err(RecordError::InvalidState);
                }
            }
            _ => return Err(RecordError::InvalidState),
        }
        let mut next = self.next();
        next.invocations.push(Invocation {
            id: self.invocations.len() as u64 + 1,
            target,
            requested,
            state: InvocationState::Planned,
        });
        Ok(next)
    }
    pub(crate) fn prepared(
        &self,
        id: u64,
        prepared: PreparedInvocation,
    ) -> Result<Self, RecordError> {
        let mut next = self.next();
        let invocation = next
            .invocations
            .last_mut()
            .filter(|i| i.id == id && matches!(i.state, InvocationState::Planned))
            .ok_or(RecordError::InvalidState)?;
        if let Some(view) = prepared.view {
            if invocation.target.is_reviewer()
                || view.turn != invocation.target.turn()
                || view.requested != invocation.requested
                || view.selection != prepared.request.selection
                || next.views.iter().any(|v| v.turn == view.turn)
            {
                return Err(RecordError::InvalidEvidence);
            }
            next.views.push(view);
        } else if !invocation.target.is_reviewer()
            && !next
                .views
                .iter()
                .any(|v| v.turn == invocation.target.turn())
        {
            return Err(RecordError::InvalidEvidence);
        }
        invocation.state = InvocationState::Prepared(prepared.request);
        Ok(next)
    }
    fn finish<O>(
        &self,
        id: u64,
        observation: &Observation<O>,
        outcome: RecordedOutcome,
    ) -> Result<Self, RecordError> {
        let mut next = self.next();
        let invocation = next
            .invocations
            .last_mut()
            .filter(|i| i.id == id)
            .ok_or(RecordError::InvalidState)?;
        let request = match &invocation.state {
            InvocationState::Planned => None,
            InvocationState::Prepared(r) => Some(r.clone()),
            _ => return Err(RecordError::InvalidState),
        };
        if request.is_none() && !matches!(observation.generation, Generation::Failed(_)) {
            return Err(RecordError::InvalidEvidence);
        }
        invocation.state = InvocationState::Finished {
            request,
            response: Box::new(observation.evidence.clone()),
            outcome,
        };
        Ok(next)
    }
    pub(crate) fn accept_assistant(
        &self,
        id: u64,
        observation: &Observation<AssistantMessage>,
        advertised: &[ToolDefinition],
    ) -> Result<Self, RecordError> {
        let invocation = self
            .invocations
            .last()
            .filter(|i| i.id == id && !i.target.is_reviewer())
            .ok_or(RecordError::InvalidState)?;
        let turn = TurnId::new(invocation.target.turn()).map_err(|_| RecordError::InvalidState)?;
        let mut session = self.session.clone();
        let outcome = match &observation.generation {
            Generation::Complete { output } => {
                let known = output.calls().is_none_or(|calls| {
                    calls
                        .as_slice()
                        .iter()
                        .all(|c| advertised.iter().any(|t| t.name() == c.name()))
                });
                if known && session.record_assistant(turn, output.clone()).is_ok() {
                    RecordedOutcome::AssistantAccepted
                } else {
                    RecordedOutcome::OutputRejected {
                        reason:
                            "Assistant output violates the advertised catalog or session transition"
                                .into(),
                    }
                }
            }
            other => recorded(other, matches!(invocation.state, InvocationState::Planned)),
        };
        if !matches!(outcome, RecordedOutcome::AssistantAccepted) {
            session
                .fail(turn, failure(&outcome))
                .map_err(|_| RecordError::InvalidState)?;
        }
        let mut next = self.finish(id, observation, outcome)?;
        next.session = session;
        Ok(next)
    }
    pub(crate) fn accept_review(
        &self,
        id: u64,
        observation: &Observation<Decision>,
    ) -> Result<Self, RecordError> {
        let invocation = self
            .invocations
            .last()
            .filter(|i| i.id == id && i.target.is_reviewer())
            .ok_or(RecordError::InvalidState)?;
        let outcome = match &observation.generation {
            Generation::Complete { output } => RecordedOutcome::Review {
                allow: output.should_allow(),
                reason: output.reason().map(|r| r.as_str().into()),
            },
            other => recorded(other, matches!(invocation.state, InvocationState::Planned)),
        };
        self.finish(id, observation, outcome)
    }
    pub(crate) fn recover(&self) -> Result<Option<Self>, RecordError> {
        let pending = self.invocations.iter().any(|i| {
            matches!(
                i.state,
                InvocationState::Planned | InvocationState::Prepared(_)
            )
        });
        let known_review_failure = self.session.active_turn().is_some_and(|turn| {
            self.invocations.iter().any(|i| {
                i.target.turn() == turn.id().get()
                    && i.blocks_review()
                    && matches!(i.state, InvocationState::Finished { .. })
            })
        });
        if !pending && (self.session.active_turn().is_none() || known_review_failure) {
            return Ok(None);
        }
        let mut next = self.next();
        for invocation in &mut next.invocations {
            invocation.state = match &invocation.state {
                InvocationState::Planned => InvocationState::Interrupted {
                    request: None,
                    response_unknown: false,
                },
                InvocationState::Prepared(r) => InvocationState::Interrupted {
                    request: Some(r.clone()),
                    response_unknown: true,
                },
                _ => continue,
            };
        }
        if let Some(turn) = next.session.active_turn() {
            next.session
                .interrupt(turn.id(), StopReason::ProcessLost)
                .map_err(|_| RecordError::InvalidState)?;
        }
        Ok(Some(next))
    }
    pub(crate) fn acknowledge(
        &self,
        id: u64,
        evidence: NonEmptyText,
        actor: Option<NonEmptyText>,
    ) -> Result<Option<Self>, RecordError> {
        let actor = actor.map(|a| a.as_str().to_owned());
        if let Some(prior) = self.acknowledgements.iter().find(|a| a.invocation == id) {
            return if prior.evidence == evidence.as_str() && prior.actor == actor {
                Ok(None)
            } else {
                Err(RecordError::ConflictingAcknowledgement)
            };
        }
        let invocation = self
            .invocations
            .iter()
            .find(|i| i.id == id && i.blocks_review())
            .ok_or(RecordError::NoReviewFailure)?;
        let mut next = self.next();
        if let Some(turn) = next
            .session
            .active_turn()
            .filter(|t| t.id().get() == invocation.target.turn())
        {
            let cause = match &invocation.state {
                InvocationState::Finished { outcome, .. } => failure(outcome),
                _ => TurnFailure::ModelFailure(
                    NonEmptyText::new("Reviewer response unknown").unwrap(),
                ),
            };
            next.session
                .fail(turn.id(), cause)
                .map_err(|_| RecordError::InvalidState)?;
        }
        next.acknowledgements.push(ReviewFailureAcknowledgement {
            invocation: id,
            evidence: evidence.as_str().into(),
            acknowledged_at: now_millis(),
            actor,
        });
        Ok(Some(next))
    }
    pub(crate) fn native_steps(&self, turn: TurnId) -> Result<Vec<NativeStep>, RecordError> {
        let active = self
            .session
            .active_turn()
            .filter(|t| t.id() == turn)
            .ok_or(RecordError::InvalidState)?;
        active
            .steps()
            .iter()
            .enumerate()
            .map(|(index, step)| {
                let response = self
                    .invocations
                    .iter()
                    .find_map(|i| match (&i.target, &i.state) {
                        (
                            InvocationTarget::Assistant { turn: t, step: s },
                            InvocationState::Finished {
                                response,
                                outcome: RecordedOutcome::AssistantAccepted,
                                ..
                            },
                        ) if *t == turn.get() && *s == index as u64 + 1 => {
                            response.response.clone()
                        }
                        _ => None,
                    })
                    .ok_or(RecordError::InvalidEvidence)?;
                let results = step
                    .executions()
                    .filter_map(|e| match e.state() {
                        ExecutionState::Returned(outcome) => {
                            Some(ToolResult::new(e.call().id().clone(), outcome.clone()))
                        }
                        _ => None,
                    })
                    .collect();
                Ok(NativeStep { response, results })
            })
            .collect()
    }
    pub(crate) fn restore(
        revision: u64,
        session: Session,
        invocations: Vec<Invocation>,
        views: Vec<TurnView>,
        acknowledgements: Vec<ReviewFailureAcknowledgement>,
    ) -> Result<Self, RecordError> {
        let record = Self {
            revision,
            session,
            invocations,
            views,
            acknowledgements,
        };
        record.validate()?;
        Ok(record)
    }
    pub fn validates_successor(&self, next: &Self) -> bool {
        next.session.id() == self.session.id()
            && self.revision.checked_add(1) == Some(next.revision)
            && next.validate().is_ok()
    }
    fn validate(&self) -> Result<(), RecordError> {
        let snapshot = self.session.snapshot();
        let records: Vec<_> = snapshot
            .turns
            .iter()
            .chain(snapshot.active.iter())
            .collect();
        let turn_count = records.len();
        let mut accepted = std::collections::BTreeMap::<(u64, u64), u64>::new();
        let mut counts = std::collections::BTreeMap::<u64, u64>::new();
        let mut reviews = std::collections::BTreeSet::new();
        for (index, i) in self.invocations.iter().enumerate() {
            if i.id != index as u64 + 1
                || i.target.turn() == 0
                || i.target.turn() > turn_count as u64
                || i.requested.route.trim().is_empty()
                || i.requested.settings.model_id.as_str().trim().is_empty()
            {
                return Err(RecordError::InvalidEvidence);
            }
            let turn = records
                .iter()
                .find(|t| t.id == i.target.turn())
                .ok_or(RecordError::InvalidEvidence)?;
            let (request, successful) = match &i.state {
                InvocationState::Planned => (None, false),
                InvocationState::Prepared(r) => (Some(r), false),
                InvocationState::Interrupted {
                    request,
                    response_unknown,
                } => {
                    if request.is_some() != *response_unknown {
                        return Err(RecordError::InvalidEvidence);
                    }
                    (request.as_ref(), false)
                }
                InvocationState::Finished {
                    request,
                    response,
                    outcome,
                } => {
                    if matches!(outcome, RecordedOutcome::PreparationFailed(_)) != request.is_none()
                    {
                        return Err(RecordError::InvalidEvidence);
                    }
                    let successful = matches!(
                        outcome,
                        RecordedOutcome::AssistantAccepted | RecordedOutcome::Review { .. }
                    );
                    if successful
                        && (response.response.is_none()
                            || response.capture != CaptureCompleteness::Complete)
                    {
                        return Err(RecordError::InvalidEvidence);
                    }
                    if i.target.is_reviewer() != matches!(outcome, RecordedOutcome::Review { .. })
                        && successful
                    {
                        return Err(RecordError::InvalidEvidence);
                    }
                    if matches!(&response.capture, CaptureCompleteness::NotReturned)
                        && response.response.is_some()
                    {
                        return Err(RecordError::InvalidEvidence);
                    }
                    (request.as_ref(), successful)
                }
            };
            if matches!(
                i.state,
                InvocationState::Planned | InvocationState::Prepared(_)
            ) && (index + 1 != self.invocations.len()
                || self
                    .session
                    .active_turn()
                    .is_none_or(|t| t.id().get() != i.target.turn()))
            {
                return Err(RecordError::InvalidEvidence);
            }
            if let Some(r) = request
                && (r.body.format.trim().is_empty()
                    || r.body.version == 0
                    || r.body.bytes.is_empty()
                    || r.selection.windows(2).any(|w| w[0] >= w[1])
                    || r.selection
                        .iter()
                        .any(|id| *id == 0 || *id >= i.target.turn()))
            {
                return Err(RecordError::InvalidEvidence);
            }
            match &i.target {
                InvocationTarget::Assistant { turn, step } => {
                    let count = counts.entry(*turn).or_default();
                    if *step != *count + 1 {
                        return Err(RecordError::InvalidEvidence);
                    }
                    if successful {
                        if *step > records[*turn as usize - 1].steps.len() as u64
                            || accepted.insert((*turn, *step), i.id).is_some()
                        {
                            return Err(RecordError::InvalidEvidence);
                        }
                        *count += 1;
                    }
                    if let Some(r) = request {
                        let view = self
                            .views
                            .iter()
                            .find(|v| v.turn == *turn)
                            .ok_or(RecordError::InvalidEvidence)?;
                        if view.requested != i.requested
                            || r.selection != view.selection
                            || (*step == 1 && view.frozen_prefix != r.body)
                        {
                            return Err(RecordError::InvalidEvidence);
                        }
                    }
                }
                InvocationTarget::Reviewer { call } => {
                    let address = call.address().map_err(|_| RecordError::InvalidEvidence)?;
                    if call.session != self.session.id().as_str()
                        || call.step == 0
                        || !reviews.insert(address)
                        || !accepted.contains_key(&(call.turn, call.step))
                        || turn
                            .steps
                            .get(
                                usize::try_from(call.step - 1)
                                    .map_err(|_| RecordError::InvalidEvidence)?,
                            )
                            .is_none_or(|s| {
                                !s.executions.iter().any(|e| {
                                    e.call_id == call.native
                                        && !matches!(e.state, ExecutionState::NotStarted)
                                })
                            })
                    {
                        return Err(RecordError::InvalidEvidence);
                    }
                }
            }
        }
        for turn in &records {
            for step in 1..=turn.steps.len() as u64 {
                if !accepted.contains_key(&(turn.id, step)) {
                    return Err(RecordError::InvalidEvidence);
                }
            }
        }
        for turn in &records {
            for (index, step) in turn.steps.iter().enumerate() {
                for execution in &step.executions {
                    if let ExecutionState::Returned(result) = &execution.state {
                        let review = self.invocations.iter().find(|i| {
                            matches!(&i.target, InvocationTarget::Reviewer { call }
                                if call.turn == turn.id && call.step == index as u64 + 1 && call.native == execution.call_id)
                        });
                        let Some(Invocation {
                            state:
                                InvocationState::Finished {
                                    outcome: RecordedOutcome::Review { allow, .. },
                                    ..
                                },
                            ..
                        }) = review
                        else {
                            return Err(RecordError::InvalidEvidence);
                        };
                        if !allow && matches!(result, ToolOutcome::Success(_)) {
                            return Err(RecordError::InvalidEvidence);
                        }
                    }
                }
            }
        }
        let mut turns = std::collections::BTreeSet::new();
        for view in &self.views {
            if view.turn == 0
                || view.turn > turn_count as u64
                || !turns.insert(view.turn)
                || view.selection.windows(2).any(|w| w[0] >= w[1])
                || view.selection.iter().any(|id| *id == 0 || *id >= view.turn)
                || !self.invocations.iter().any(|i| {
                    matches!(i.target, InvocationTarget::Assistant {turn,step:1} if turn==view.turn)
                        && !matches!(
                            i.state,
                            InvocationState::Planned
                                | InvocationState::Interrupted { request: None, .. }
                                | InvocationState::Finished { request: None, .. }
                        )
                })
            {
                return Err(RecordError::InvalidEvidence);
            }
        }
        let mut ids = std::collections::BTreeSet::new();
        for a in &self.acknowledgements {
            if a.evidence.trim().is_empty()
                || a.actor.as_ref().is_some_and(|a| a.trim().is_empty())
                || !ids.insert(a.invocation)
                || !self
                    .invocations
                    .iter()
                    .any(|i| i.id == a.invocation && i.blocks_review())
            {
                return Err(RecordError::InvalidEvidence);
            }
        }
        Ok(())
    }
}
fn recorded<O>(generation: &Generation<O>, planned: bool) -> RecordedOutcome {
    match generation {
        Generation::Refused { .. } => RecordedOutcome::Refused,
        Generation::Incomplete { reason } => RecordedOutcome::Incomplete(*reason),
        Generation::Failed(e) if planned => RecordedOutcome::PreparationFailed(e.clone()),
        Generation::Failed(e) => RecordedOutcome::Failed(e.clone()),
        Generation::Complete { .. } => unreachable!(),
    }
}
fn failure(outcome: &RecordedOutcome) -> TurnFailure {
    let text = match outcome {
        RecordedOutcome::OutputRejected { reason } => format!("Model output rejected: {reason}"),
        RecordedOutcome::Refused => "Model refused generation".into(),
        RecordedOutcome::Incomplete(reason) => format!("Model generation incomplete ({reason:?})"),
        RecordedOutcome::Failed(e) | RecordedOutcome::PreparationFailed(e) => {
            format!("Model request failed ({:?})", e.kind())
        }
        _ => "Reviewer failure".into(),
    };
    if matches!(outcome, RecordedOutcome::OutputRejected { .. })
        || matches!(outcome, RecordedOutcome::Failed(e) if e.kind() == ModelErrorKind::MalformedResponse)
    {
        TurnFailure::MalformedResponse(NonEmptyText::new(text).unwrap())
    } else {
        TurnFailure::ModelFailure(NonEmptyText::new(text).unwrap())
    }
}
