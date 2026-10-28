use super::{
    BudgetMeasurement, CallRef, ExecutionState, GenerationId, IncompleteReason, ModelError,
    ModelSettings, ModelTarget, NonEmptyText, PreparedRequest, ResponseCapture, ResponseReceipt,
    RestoreError, SelectionManifest, Session, SessionId, SessionSnapshot, StepSnapshot, Timestamp,
    ToolCallId, TurnId, ValueError,
};
use serde_json::Value;
use std::{collections::BTreeSet, error::Error, fmt};

/// Committed snapshot sequence. Zero is reserved for a new, unpublished draft.
#[derive(Clone, Copy, Debug, PartialEq, Eq, PartialOrd, Ord)]
pub struct Revision(u64);
impl Revision {
    pub fn get(self) -> u64 {
        self.0
    }
}

#[derive(Clone, Debug, PartialEq, Eq)]
pub enum InvocationRole {
    Assistant { turn: TurnId },
    Reviewer { call: CallRef },
}

#[derive(Clone, Debug, PartialEq, Eq)]
pub struct InvocationIntent {
    role: InvocationRole,
    target: ModelTarget,
    settings: ModelSettings,
    at: Timestamp,
    request: Value,
    selection: SelectionManifest,
    budget: BudgetMeasurement,
}
impl InvocationIntent {
    pub fn role(&self) -> &InvocationRole {
        &self.role
    }
    pub fn target(&self) -> &ModelTarget {
        &self.target
    }
    pub fn settings(&self) -> &ModelSettings {
        &self.settings
    }
    pub fn at(&self) -> Timestamp {
        self.at
    }
    pub fn request(&self) -> &Value {
        &self.request
    }
    pub fn selection(&self) -> &SelectionManifest {
        &self.selection
    }
    pub fn budget(&self) -> &BudgetMeasurement {
        &self.budget
    }
}

#[derive(Clone, Debug, PartialEq, Eq)]
pub enum Disposition {
    Accepted,
    Refused { reason: Option<NonEmptyText> },
    Incomplete(IncompleteReason),
    Rejected(NonEmptyText),
    Failed(ModelError),
}
#[derive(Clone, Debug, PartialEq, Eq)]
pub enum Observation {
    Pending,
    Observed {
        at: Timestamp,
        receipt: ResponseReceipt,
        disposition: Disposition,
    },
    Abandoned {
        at: Timestamp,
    },
}
#[derive(Clone, Debug, PartialEq, Eq)]
pub struct GenerationRecord {
    id: GenerationId,
    intent: InvocationIntent,
    observation: Observation,
}
impl GenerationRecord {
    pub fn id(&self) -> GenerationId {
        self.id
    }
    pub fn intent(&self) -> &InvocationIntent {
        &self.intent
    }
    pub fn observation(&self) -> &Observation {
        &self.observation
    }
}

/// Domain facts and one noncumulative evidence record per model invocation.
#[derive(Clone, Debug, PartialEq, Eq)]
pub struct SessionArchive {
    revision: Revision,
    session: Session,
    generations: Vec<GenerationRecord>,
}
impl SessionArchive {
    pub fn new(id: SessionId) -> Self {
        Self {
            revision: Revision(0),
            session: Session::new(id),
            generations: vec![],
        }
    }
    pub fn revision(&self) -> Revision {
        self.revision
    }
    pub fn session(&self) -> &Session {
        &self.session
    }
    /// Mutations remain drafts until checked by `ArchiveChange`.
    pub fn session_mut(&mut self) -> &mut Session {
        &mut self.session
    }
    pub fn generations(&self) -> &[GenerationRecord] {
        &self.generations
    }
    pub fn generation(&self, id: GenerationId) -> Option<&GenerationRecord> {
        usize::try_from(id.get() - 1)
            .ok()
            .and_then(|index| self.generations.get(index))
    }
    pub fn needs_recovery(&self) -> bool {
        self.session.active_turn().is_some()
    }
    pub fn next_generation_id(&self) -> Result<GenerationId, ArchiveError> {
        let id = self
            .generations
            .last()
            .map_or(Some(1), |record| record.id.get().checked_add(1))
            .ok_or_else(|| invalid("generation sequence exhausted"))?;
        GenerationId::new(id).map_err(data_error)
    }
    pub fn begin_generation<O>(
        &mut self,
        role: InvocationRole,
        request: &PreparedRequest<O>,
    ) -> Result<GenerationId, ArchiveError> {
        if request.selection().revision() != self.revision {
            return Err(invalid("prepared request has a stale source revision"));
        }
        let action_matches = match (
            &role,
            self.session.next_action(),
            self.session.active_turn(),
        ) {
            (
                InvocationRole::Assistant { turn },
                crate::domain::NextAction::AskModel,
                Some(active),
            ) => *turn == active.id(),
            (InvocationRole::Reviewer { call }, crate::domain::NextAction::Review(expected), _) => {
                *call == expected
            }
            _ => false,
        };
        if !action_matches {
            return Err(invalid(
                "invocation role does not match the next domain action",
            ));
        }
        let id = self.next_generation_id()?;
        let intent = InvocationIntent {
            role,
            target: request.target().clone(),
            settings: request.settings().clone(),
            at: Timestamp::now(),
            request: request.body().clone(),
            selection: request.selection().clone(),
            budget: request.budget().clone(),
        };
        self.generations.push(GenerationRecord {
            id,
            intent,
            observation: Observation::Pending,
        });
        if let Err(error) = validate(self) {
            self.generations.pop();
            return Err(error);
        }
        Ok(id)
    }
    pub fn record_observation(
        &mut self,
        id: GenerationId,
        at: Timestamp,
        receipt: ResponseReceipt,
        disposition: Disposition,
    ) -> Result<(), ArchiveError> {
        validate_capture(&receipt)?;
        let record = self
            .generations
            .iter_mut()
            .find(|record| record.id == id)
            .ok_or_else(|| invalid("unknown generation"))?;
        if !matches!(record.observation, Observation::Pending) || at < record.intent.at {
            return Err(invalid(
                "observation does not finalize a pending invocation",
            ));
        }
        record.observation = Observation::Observed {
            at,
            receipt,
            disposition,
        };
        Ok(())
    }
    pub fn abandon_pending(&mut self, at: Timestamp) {
        for record in &mut self.generations {
            if matches!(record.observation, Observation::Pending) {
                record.observation = Observation::Abandoned {
                    at: at.max(record.intent.at),
                };
            }
        }
    }
    /// Checks facts admitted by a committed archive, including invocation source revisions.
    pub fn validate_committed(&self) -> Result<(), ArchiveError> {
        validate(self)?;
        if self.revision.0 == 0
            || self
                .generations
                .iter()
                .any(|record| record.intent.selection.revision >= self.revision)
        {
            return Err(invalid(
                "archive revision does not follow invocation source revision",
            ));
        }
        Ok(())
    }
}

/// Validated immutable base-to-candidate transition, usable by independent stores.
#[derive(Clone, Debug)]
pub struct ArchiveChange {
    base: Option<SessionArchive>,
    candidate: SessionArchive,
}
impl ArchiveChange {
    pub fn initial(mut candidate: SessionArchive) -> Result<Self, ArchiveError> {
        if candidate.revision != Revision(0) {
            return Err(invalid("initial snapshot is already committed"));
        }
        candidate.revision = Revision(1);
        Self::checked(None, candidate)
    }
    pub fn next(
        base: &SessionArchive,
        mut candidate: SessionArchive,
    ) -> Result<Self, ArchiveError> {
        if base.revision.0 == 0 || candidate.revision != base.revision {
            return Err(invalid("candidate source revision mismatch"));
        }
        base.validate_committed()?;
        validate_extension(base, &candidate)?;
        candidate.revision = Revision(
            base.revision
                .0
                .checked_add(1)
                .ok_or_else(|| invalid("revision exhausted"))?,
        );
        Self::checked(Some(base.clone()), candidate)
    }
    fn checked(
        base: Option<SessionArchive>,
        candidate: SessionArchive,
    ) -> Result<Self, ArchiveError> {
        candidate.validate_committed()?;
        Ok(Self { base, candidate })
    }
    pub fn base(&self) -> Option<&SessionArchive> {
        self.base.as_ref()
    }
    pub fn candidate(&self) -> &SessionArchive {
        &self.candidate
    }
}

fn validate_capture(receipt: &ResponseReceipt) -> Result<(), ArchiveError> {
    if receipt
        .http_status
        .is_some_and(|status| !(100..=599).contains(&status))
    {
        return Err(invalid("invalid HTTP status in response receipt"));
    }
    if let ResponseCapture::Truncated { prefix, limit } = &receipt.capture
        && (*limit == 0 || prefix.len() != *limit)
    {
        return Err(invalid("invalid truncated response capture"));
    }
    Ok(())
}

fn validate(archive: &SessionArchive) -> Result<(), ArchiveError> {
    Session::restore(archive.session.snapshot()).map_err(data_error)?;
    let snapshot = archive.session.snapshot();
    let mut links = BTreeSet::new();
    for turn in snapshot.turns.iter().chain(snapshot.active.iter()) {
        let mut last_link = 0;
        for step in &turn.steps {
            let id = GenerationId::new(step.source_generation).map_err(data_error)?;
            if id.get() <= last_link {
                return Err(invalid("accepted sources are out of invocation order"));
            }
            last_link = id.get();
            let record = archive
                .generation(id)
                .ok_or_else(|| invalid("dangling step source generation"))?;
            if record.intent.role
                != (InvocationRole::Assistant {
                    turn: TurnId::new(turn.id).map_err(data_error)?,
                })
                || !is_accepted(record)
                || !links.insert(id)
            {
                return Err(invalid("step source role, turn or acceptance mismatch"));
            }
            for execution in &step.executions {
                if let Some(review) = execution.state.review() {
                    let reviewer = archive
                        .generation(review.reviewer())
                        .ok_or_else(|| invalid("dangling reviewer generation"))?;
                    let call = CallRef::new(
                        id,
                        ToolCallId::new(execution.call_id.clone()).map_err(data_error)?,
                    );
                    if reviewer.intent.role != (InvocationRole::Reviewer { call })
                        || !is_accepted(reviewer)
                        || reviewer.id.get() <= last_link
                        || !links.insert(reviewer.id)
                    {
                        return Err(invalid("review source role, call or acceptance mismatch"));
                    }
                    last_link = reviewer.id.get();
                }
            }
        }
    }
    let mut prior_turn = 0;
    for (index, record) in archive.generations.iter().enumerate() {
        if record.id.get() != index as u64 + 1 {
            return Err(invalid("generation IDs must be consecutive"));
        }
        let turn = role_turn(&snapshot, &record.intent.role)?;
        if turn < prior_turn {
            return Err(invalid("invocations are out of turn order"));
        }
        prior_turn = turn;
        if let InvocationRole::Reviewer { call } = &record.intent.role
            && call.generation() >= record.id
        {
            return Err(invalid("review predates its proposal"));
        }
        if record.intent.selection.revision.0 > archive.revision.0
            || record.intent.target.provider().trim().is_empty()
            || record.intent.target.endpoint().trim().is_empty()
            || !record.intent.request.is_object()
        {
            return Err(invalid("invalid invocation provenance"));
        }
        let bytes = serde_json::to_vec(&record.intent.request)
            .map_err(data_error)?
            .len();
        let budget = &record.intent.budget;
        if budget.request_bytes != bytes
            || bytes > budget.max_request_bytes
            || budget.max_request_bytes == 0
            || budget.estimated_input_tokens != (bytes as u64).div_ceil(3)
            || budget.estimated_input_tokens > budget.input_token_budget
            || budget.method != super::TOKEN_ESTIMATE_METHOD
        {
            return Err(invalid(
                "invocation budget does not describe its exact request",
            ));
        }
        let selection = &record.intent.selection;
        if selection.policy != super::SELECTION_POLICY
            || !strictly_sorted(&selection.included)
            || !strictly_sorted(&selection.omitted)
            || !strictly_sorted(&selection.required)
            || selection
                .required
                .iter()
                .any(|id| !selection.included.contains(id))
        {
            return Err(invalid("invalid invocation selection manifest"));
        }
        let all: BTreeSet<_> = selection
            .included
            .iter()
            .chain(&selection.omitted)
            .copied()
            .collect();
        if all.len() != selection.included.len() + selection.omitted.len()
            || all.iter().map(|id| id.get()).ne(1..turn)
        {
            return Err(invalid("selection does not partition prior whole turns"));
        }
        match &record.observation {
            Observation::Pending => {
                if index + 1 != archive.generations.len()
                    || snapshot
                        .active
                        .as_ref()
                        .is_none_or(|active| active.id != turn)
                {
                    return Err(invalid("pending invocation has no active turn"));
                }
            }
            Observation::Abandoned { at } => {
                if *at < record.intent.at {
                    return Err(invalid("abandonment predates invocation"));
                }
            }
            Observation::Observed {
                at,
                receipt,
                disposition,
            } => {
                if *at < record.intent.at {
                    return Err(invalid("observation predates invocation"));
                }
                validate_capture(receipt)?;
                if let Disposition::Failed(error) = disposition
                    && error
                        .http_status()
                        .is_some_and(|status| !(100..=599).contains(&status))
                {
                    return Err(invalid("invalid failure status"));
                }
                if is_accepted(record) && !matches!(receipt.capture, ResponseCapture::Complete(_)) {
                    return Err(invalid(
                        "accepted output requires complete response evidence",
                    ));
                }
            }
        }
        if is_accepted(record) != links.contains(&record.id) {
            return Err(invalid("accepted generation has no unique accepted fact"));
        }
    }
    Ok(())
}
fn strictly_sorted(ids: &[TurnId]) -> bool {
    ids.windows(2).all(|pair| pair[0] < pair[1])
}
fn is_accepted(record: &GenerationRecord) -> bool {
    matches!(
        record.observation,
        Observation::Observed {
            disposition: Disposition::Accepted,
            ..
        }
    )
}
fn role_turn(snapshot: &SessionSnapshot, role: &InvocationRole) -> Result<u64, ArchiveError> {
    match role {
        InvocationRole::Assistant { turn } => {
            if snapshot
                .turns
                .iter()
                .chain(snapshot.active.iter())
                .any(|value| value.id == turn.get())
            {
                Ok(turn.get())
            } else {
                Err(invalid("invocation names an unknown turn"))
            }
        }
        InvocationRole::Reviewer { call } => snapshot
            .turns
            .iter()
            .chain(snapshot.active.iter())
            .find_map(|turn| {
                turn.steps
                    .iter()
                    .find(|step| {
                        step.source_generation == call.generation().get()
                            && step
                                .executions
                                .iter()
                                .any(|execution| execution.call_id == call.call_id().as_str())
                    })
                    .map(|_| turn.id)
            })
            .ok_or_else(|| invalid("review invocation names an unknown call")),
    }
}

fn validate_extension(
    base: &SessionArchive,
    candidate: &SessionArchive,
) -> Result<(), ArchiveError> {
    validate(base)?;
    if base.session.id() != candidate.session.id()
        || candidate.generations.len() < base.generations.len()
    {
        return Err(invalid("candidate rewrites archive identity or ledger"));
    }
    for (prior, next) in base.generations.iter().zip(&candidate.generations) {
        if prior.id != next.id
            || prior.intent != next.intent
            || (!matches!(prior.observation, Observation::Pending)
                && prior.observation != next.observation)
        {
            return Err(invalid("candidate rewrites committed invocation evidence"));
        }
    }
    let prior = base.session.snapshot();
    let next = candidate.session.snapshot();
    if next.turns.len() < prior.turns.len() {
        return Err(invalid("candidate removes closed turns"));
    }
    for (old, new) in prior.turns.iter().zip(&next.turns) {
        if old.id != new.id
            || old.user != new.user
            || old.outcome != new.outcome
            || old.steps.len() != new.steps.len()
        {
            return Err(invalid("candidate rewrites closed turn"));
        }
        for (old, new) in old.steps.iter().zip(&new.steps) {
            validate_step_extension(old, new, true)?;
        }
    }
    if let Some(active) = &prior.active {
        let new = next
            .turns
            .iter()
            .chain(next.active.iter())
            .find(|turn| turn.id == active.id)
            .ok_or_else(|| invalid("candidate removes active turn"))?;
        if active.user != new.user || new.steps.len() < active.steps.len() {
            return Err(invalid("candidate rewrites active turn"));
        }
        for (old, new) in active.steps.iter().zip(&new.steps) {
            validate_step_extension(old, new, false)?;
        }
    }
    Ok(())
}
fn validate_step_extension(
    old: &StepSnapshot,
    new: &StepSnapshot,
    closed: bool,
) -> Result<(), ArchiveError> {
    if old.source_generation != new.source_generation
        || old.assistant != new.assistant
        || old.executions.len() != new.executions.len()
    {
        return Err(invalid("candidate rewrites accepted assistant output"));
    }
    for (old, new) in old.executions.iter().zip(&new.executions) {
        let state_extends = old.state == new.state
            || (!closed
                && match (&old.state, &new.state) {
                    (ExecutionState::Requested, _) => true,
                    (
                        ExecutionState::Approved(a),
                        ExecutionState::Started { review: b, .. }
                        | ExecutionState::Returned { review: b, .. },
                    ) => a == b,
                    (
                        ExecutionState::Started {
                            review: a,
                            at: start,
                        },
                        ExecutionState::Returned {
                            review: b,
                            started_at,
                            ..
                        },
                    ) => a == b && start == started_at,
                    _ => false,
                });
        if old.call_id != new.call_id
            || !state_extends
            || old
                .resolution
                .as_ref()
                .is_some_and(|value| Some(value) != new.resolution.as_ref())
        {
            return Err(invalid("candidate rewrites committed execution facts"));
        }
    }
    Ok(())
}

/// An archive violates semantic conversation or invocation invariants.
#[derive(Debug)]
pub enum ArchiveError {
    Invalid(&'static str),
    InvalidValue(ValueError),
    InvalidConversation(RestoreError),
    InvalidRequest(serde_json::Error),
}
impl fmt::Display for ArchiveError {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        match self {
            Self::Invalid(message) => f.write_str(message),
            Self::InvalidValue(error) => error.fmt(f),
            Self::InvalidConversation(error) => error.fmt(f),
            Self::InvalidRequest(error) => error.fmt(f),
        }
    }
}
impl Error for ArchiveError {
    fn source(&self) -> Option<&(dyn Error + 'static)> {
        match self {
            Self::Invalid(_) => None,
            Self::InvalidValue(error) => Some(error),
            Self::InvalidConversation(error) => Some(error),
            Self::InvalidRequest(error) => Some(error),
        }
    }
}
impl From<ValueError> for ArchiveError {
    fn from(error: ValueError) -> Self {
        Self::InvalidValue(error)
    }
}
impl From<RestoreError> for ArchiveError {
    fn from(error: RestoreError) -> Self {
        Self::InvalidConversation(error)
    }
}
impl From<serde_json::Error> for ArchiveError {
    fn from(error: serde_json::Error) -> Self {
        Self::InvalidRequest(error)
    }
}
fn invalid(message: &'static str) -> ArchiveError {
    ArchiveError::Invalid(message)
}
fn data_error(error: impl Into<ArchiveError>) -> ArchiveError {
    error.into()
}

/// Format-neutral committed facts for independently implemented stores.
#[derive(Clone, Debug, PartialEq, Eq)]
pub struct ArchiveSnapshot {
    pub revision: u64,
    pub session: SessionSnapshot,
    pub generations: Vec<GenerationSnapshot>,
}
#[derive(Clone, Debug, PartialEq, Eq)]
pub struct GenerationSnapshot {
    pub id: u64,
    pub role: InvocationRole,
    pub target: ModelTarget,
    pub settings: ModelSettings,
    pub at: Timestamp,
    pub request: Value,
    pub selection: SelectionSnapshot,
    pub budget: BudgetSnapshot,
    pub observation: Observation,
}
#[derive(Clone, Debug, PartialEq, Eq)]
pub struct SelectionSnapshot {
    pub revision: u64,
    pub policy: String,
    pub required: Vec<u64>,
    pub included: Vec<u64>,
    pub omitted: Vec<u64>,
}
#[derive(Clone, Debug, PartialEq, Eq)]
pub struct BudgetSnapshot {
    pub request_bytes: usize,
    pub max_request_bytes: usize,
    pub estimated_input_tokens: u64,
    pub input_token_budget: u64,
    pub method: String,
}
impl SessionArchive {
    pub fn snapshot(&self) -> ArchiveSnapshot {
        ArchiveSnapshot {
            revision: self.revision.get(),
            session: self.session.snapshot(),
            generations: self
                .generations
                .iter()
                .map(|record| {
                    let intent = &record.intent;
                    GenerationSnapshot {
                        id: record.id.get(),
                        role: intent.role.clone(),
                        target: intent.target.clone(),
                        settings: intent.settings.clone(),
                        at: intent.at,
                        request: intent.request.clone(),
                        selection: SelectionSnapshot {
                            revision: intent.selection.revision.get(),
                            policy: intent.selection.policy.clone(),
                            required: intent
                                .selection
                                .required
                                .iter()
                                .map(|id| id.get())
                                .collect(),
                            included: intent
                                .selection
                                .included
                                .iter()
                                .map(|id| id.get())
                                .collect(),
                            omitted: intent.selection.omitted.iter().map(|id| id.get()).collect(),
                        },
                        budget: BudgetSnapshot {
                            request_bytes: intent.budget.request_bytes,
                            max_request_bytes: intent.budget.max_request_bytes,
                            estimated_input_tokens: intent.budget.estimated_input_tokens,
                            input_token_budget: intent.budget.input_token_budget,
                            method: intent.budget.method.clone(),
                        },
                        observation: record.observation.clone(),
                    }
                })
                .collect(),
        }
    }
    pub fn restore(snapshot: ArchiveSnapshot) -> Result<Self, ArchiveError> {
        let ids = |values: Vec<u64>| {
            values
                .into_iter()
                .map(TurnId::new)
                .collect::<Result<Vec<_>, _>>()
                .map_err(data_error)
        };
        let archive = Self {
            revision: Revision(snapshot.revision),
            session: Session::restore(snapshot.session).map_err(data_error)?,
            generations: snapshot
                .generations
                .into_iter()
                .map(|record| {
                    Ok(GenerationRecord {
                        id: GenerationId::new(record.id).map_err(data_error)?,
                        intent: InvocationIntent {
                            role: record.role,
                            target: record.target,
                            settings: record.settings,
                            at: record.at,
                            request: record.request,
                            selection: SelectionManifest {
                                revision: Revision(record.selection.revision),
                                policy: record.selection.policy,
                                required: ids(record.selection.required)?,
                                included: ids(record.selection.included)?,
                                omitted: ids(record.selection.omitted)?,
                            },
                            budget: BudgetMeasurement {
                                request_bytes: record.budget.request_bytes,
                                max_request_bytes: record.budget.max_request_bytes,
                                estimated_input_tokens: record.budget.estimated_input_tokens,
                                input_token_budget: record.budget.input_token_budget,
                                method: record.budget.method,
                            },
                        },
                        observation: record.observation,
                    })
                })
                .collect::<Result<_, ArchiveError>>()?,
        };
        archive.validate_committed()?;
        Ok(archive)
    }
}
