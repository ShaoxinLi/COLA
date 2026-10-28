use crate::{
    app::{SessionStoreError, SessionStoreErrorKind},
    domain::*,
};
use serde::{Deserialize, Serialize};
use serde_json::Value;
use std::{error::Error, fmt, io, num::NonZeroU32};

/// Canonical JSONL representation of checked committed archive facts.
pub struct JsonlArchiveCodec;
impl JsonlArchiveCodec {
    pub fn encode(archive: &SessionArchive) -> Result<Vec<u8>, SessionStoreError> {
        archive.validate_committed()?;
        encode(archive)
    }
    pub fn decode(bytes: &[u8]) -> Result<SessionArchive, SessionStoreError> {
        decode(bytes)
    }
}

#[derive(Serialize, Deserialize)]
#[serde(deny_unknown_fields)]
struct Turn {
    id: u64,
    user: String,
    steps: Vec<Step>,
    outcome: Option<Outcome>,
}
#[derive(Serialize, Deserialize)]
#[serde(deny_unknown_fields)]
struct Step {
    source_generation: u64,
    assistant: Assistant,
    executions: Vec<Execution>,
}
#[derive(Serialize, Deserialize)]
#[serde(tag = "kind", rename_all = "snake_case", deny_unknown_fields)]
enum Assistant {
    Answer {
        text: String,
    },
    ToolRequests {
        text: Option<String>,
        calls: Vec<Call>,
    },
}
#[derive(Serialize, Deserialize)]
#[serde(deny_unknown_fields)]
struct Call {
    id: String,
    name: String,
    arguments: Value,
}
#[derive(Serialize, Deserialize)]
#[serde(deny_unknown_fields)]
struct Execution {
    call_id: String,
    state: State,
    resolution: Option<Resolution>,
}
#[derive(Serialize, Deserialize)]
#[serde(tag = "kind", rename_all = "snake_case", deny_unknown_fields)]
enum State {
    Requested {},
    Denied {
        review: WireReview,
    },
    Approved {
        review: WireReview,
    },
    Started {
        review: WireReview,
        at: u64,
    },
    Returned {
        review: WireReview,
        started_at: u64,
        at: u64,
        outcome: ToolReturn,
    },
}
#[derive(Serialize, Deserialize)]
#[serde(deny_unknown_fields)]
struct WireReview {
    reviewer: u64,
    reason: Option<String>,
}
impl From<Review> for WireReview {
    fn from(value: Review) -> Self {
        Self {
            reviewer: value.reviewer().get(),
            reason: value.reason().map(|text| text.as_str().into()),
        }
    }
}
impl TryFrom<WireReview> for Review {
    type Error = SessionStoreError;
    fn try_from(value: WireReview) -> Result<Self, Self::Error> {
        Ok(Review::new(
            GenerationId::new(value.reviewer).map_err(data_error)?,
            value
                .reason
                .map(NonEmptyText::new)
                .transpose()
                .map_err(data_error)?,
        ))
    }
}
#[derive(Serialize, Deserialize)]
#[serde(tag = "kind", rename_all = "snake_case", deny_unknown_fields)]
enum ToolReturn {
    Success { output: String },
    Failure { description: String },
}
#[derive(Serialize, Deserialize)]
#[serde(deny_unknown_fields)]
struct Resolution {
    disposition: ResolutionDisposition,
    source: Source,
    evidence: String,
}
#[derive(Serialize, Deserialize)]
#[serde(tag = "kind", rename_all = "snake_case", deny_unknown_fields)]
enum ResolutionDisposition {
    Completed {},
    NotPerformed {},
}
#[derive(Serialize, Deserialize)]
#[serde(tag = "kind", rename_all = "snake_case", deny_unknown_fields)]
enum Source {
    Observation {},
    UserAttestation {},
    ApplicationAttestation {},
}
#[derive(Serialize, Deserialize)]
#[serde(tag = "kind", rename_all = "snake_case", deny_unknown_fields)]
enum Outcome {
    Completed {},
    Interrupted { reason: Reason },
    Failed { failure: Failure },
}
#[derive(Serialize, Deserialize)]
#[serde(tag = "kind", rename_all = "snake_case", deny_unknown_fields)]
enum Reason {
    UserCancelled {},
    ProcessLost {},
    ContextCapacity {},
    IterationLimit {},
}
#[derive(Serialize, Deserialize)]
#[serde(tag = "kind", rename_all = "snake_case", deny_unknown_fields)]
enum Failure {
    ModelFailure { description: String },
    MalformedResponse { description: String },
}

impl From<TurnSnapshot> for Turn {
    fn from(t: TurnSnapshot) -> Self {
        Self {
            id: t.id,
            user: t.user,
            steps: t.steps.into_iter().map(Into::into).collect(),
            outcome: t.outcome.map(Into::into),
        }
    }
}
impl TryFrom<Turn> for TurnSnapshot {
    type Error = SessionStoreError;
    fn try_from(t: Turn) -> Result<Self, Self::Error> {
        Ok(Self {
            id: t.id,
            user: t.user,
            steps: t
                .steps
                .into_iter()
                .map(TryInto::try_into)
                .collect::<Result<_, _>>()?,
            outcome: t.outcome.map(TryInto::try_into).transpose()?,
        })
    }
}
impl From<StepSnapshot> for Step {
    fn from(s: StepSnapshot) -> Self {
        Self {
            source_generation: s.source_generation,
            assistant: match s.assistant {
                AssistantSnapshot::Answer(text) => Assistant::Answer { text },
                AssistantSnapshot::ToolRequests { text, calls } => Assistant::ToolRequests {
                    text,
                    calls: calls
                        .into_iter()
                        .map(|c| Call {
                            id: c.id,
                            name: c.name,
                            arguments: c.arguments,
                        })
                        .collect(),
                },
            },
            executions: s.executions.into_iter().map(Into::into).collect(),
        }
    }
}
impl TryFrom<Step> for StepSnapshot {
    type Error = SessionStoreError;
    fn try_from(s: Step) -> Result<Self, Self::Error> {
        Ok(Self {
            source_generation: s.source_generation,
            assistant: match s.assistant {
                Assistant::Answer { text } => AssistantSnapshot::Answer(text),
                Assistant::ToolRequests { text, calls } => AssistantSnapshot::ToolRequests {
                    text,
                    calls: calls
                        .into_iter()
                        .map(|c| ToolCallSnapshot {
                            id: c.id,
                            name: c.name,
                            arguments: c.arguments,
                        })
                        .collect(),
                },
            },
            executions: s
                .executions
                .into_iter()
                .map(TryInto::try_into)
                .collect::<Result<_, _>>()?,
        })
    }
}
impl From<ExecutionSnapshot> for Execution {
    fn from(e: ExecutionSnapshot) -> Self {
        Self {
            call_id: e.call_id,
            state: match e.state {
                ExecutionState::Requested => State::Requested {},
                ExecutionState::Denied(review) => State::Denied {
                    review: review.into(),
                },
                ExecutionState::Approved(review) => State::Approved {
                    review: review.into(),
                },
                ExecutionState::Started { review, at } => State::Started {
                    review: review.into(),
                    at: at.unix_millis(),
                },
                ExecutionState::Returned {
                    review,
                    started_at,
                    at,
                    outcome,
                } => State::Returned {
                    review: review.into(),
                    started_at: started_at.unix_millis(),
                    at: at.unix_millis(),
                    outcome: match outcome {
                        ToolOutcome::Success(output) => ToolReturn::Success { output },
                        ToolOutcome::Failure(error) => ToolReturn::Failure {
                            description: error.description().as_str().into(),
                        },
                    },
                },
            },
            resolution: e.resolution.map(|r| Resolution {
                disposition: match r.disposition() {
                    EffectDisposition::Completed => ResolutionDisposition::Completed {},
                    EffectDisposition::NotPerformed => ResolutionDisposition::NotPerformed {},
                },
                source: match r.source() {
                    ResolutionSource::Observation => Source::Observation {},
                    ResolutionSource::UserAttestation => Source::UserAttestation {},
                    ResolutionSource::ApplicationAttestation => Source::ApplicationAttestation {},
                },
                evidence: r.evidence().as_str().into(),
            }),
        }
    }
}
impl TryFrom<Execution> for ExecutionSnapshot {
    type Error = SessionStoreError;
    fn try_from(e: Execution) -> Result<Self, Self::Error> {
        Ok(Self {
            call_id: e.call_id,
            state: match e.state {
                State::Requested {} => ExecutionState::Requested,
                State::Denied { review } => ExecutionState::Denied(review.try_into()?),
                State::Approved { review } => ExecutionState::Approved(review.try_into()?),
                State::Started { review, at } => ExecutionState::Started {
                    review: review.try_into()?,
                    at: Timestamp::from_unix_millis(at),
                },
                State::Returned {
                    review,
                    started_at,
                    at,
                    outcome,
                } => ExecutionState::Returned {
                    review: review.try_into()?,
                    started_at: Timestamp::from_unix_millis(started_at),
                    at: Timestamp::from_unix_millis(at),
                    outcome: match outcome {
                        ToolReturn::Success { output } => ToolOutcome::Success(output),
                        ToolReturn::Failure { description } => ToolOutcome::Failure(
                            ToolError::new(NonEmptyText::new(description).map_err(data_error)?),
                        ),
                    },
                },
            },
            resolution: e
                .resolution
                .map(|r| {
                    Ok::<_, SessionStoreError>(EffectResolution::new(
                        match r.disposition {
                            ResolutionDisposition::Completed {} => EffectDisposition::Completed,
                            ResolutionDisposition::NotPerformed {} => {
                                EffectDisposition::NotPerformed
                            }
                        },
                        match r.source {
                            Source::Observation {} => ResolutionSource::Observation,
                            Source::UserAttestation {} => ResolutionSource::UserAttestation,
                            Source::ApplicationAttestation {} => {
                                ResolutionSource::ApplicationAttestation
                            }
                        },
                        NonEmptyText::new(r.evidence).map_err(data_error)?,
                    ))
                })
                .transpose()?,
        })
    }
}
impl From<TurnOutcome> for Outcome {
    fn from(o: TurnOutcome) -> Self {
        match o {
            TurnOutcome::Completed => Self::Completed {},
            TurnOutcome::Interrupted(r) => Self::Interrupted {
                reason: match r {
                    StopReason::UserCancelled => Reason::UserCancelled {},
                    StopReason::ProcessLost => Reason::ProcessLost {},
                    StopReason::ContextCapacity => Reason::ContextCapacity {},
                    StopReason::IterationLimit => Reason::IterationLimit {},
                },
            },
            TurnOutcome::Failed(f) => Self::Failed {
                failure: match f {
                    TurnFailure::ModelFailure(t) => Failure::ModelFailure {
                        description: t.as_str().into(),
                    },
                    TurnFailure::MalformedResponse(t) => Failure::MalformedResponse {
                        description: t.as_str().into(),
                    },
                },
            },
        }
    }
}
impl TryFrom<Outcome> for TurnOutcome {
    type Error = SessionStoreError;
    fn try_from(o: Outcome) -> Result<Self, Self::Error> {
        Ok(match o {
            Outcome::Completed {} => Self::Completed,
            Outcome::Interrupted { reason } => Self::Interrupted(match reason {
                Reason::UserCancelled {} => StopReason::UserCancelled,
                Reason::ProcessLost {} => StopReason::ProcessLost,
                Reason::ContextCapacity {} => StopReason::ContextCapacity,
                Reason::IterationLimit {} => StopReason::IterationLimit,
            }),
            Outcome::Failed { failure } => Self::Failed(match failure {
                Failure::ModelFailure { description } => {
                    TurnFailure::ModelFailure(NonEmptyText::new(description).map_err(data_error)?)
                }
                Failure::MalformedResponse { description } => TurnFailure::MalformedResponse(
                    NonEmptyText::new(description).map_err(data_error)?,
                ),
            }),
        })
    }
}

#[derive(Serialize, Deserialize)]
#[serde(tag = "record", rename_all = "snake_case", deny_unknown_fields)]
enum Header {
    Session {
        session_id: String,
        revision: u64,
        closed_turns: usize,
        active: bool,
        generations: usize,
    },
}
#[derive(Serialize, Deserialize)]
#[serde(tag = "record", rename_all = "snake_case", deny_unknown_fields)]
enum Record {
    ClosedTurn { turn: Turn },
    ActiveTurn { turn: Turn },
    Generation { generation: Box<WireGeneration> },
}
#[derive(Serialize, Deserialize)]
#[serde(deny_unknown_fields)]
struct WireGeneration {
    id: u64,
    intent: WireIntent,
    observation: WireObservation,
}
#[derive(Serialize, Deserialize)]
#[serde(deny_unknown_fields)]
struct WireIntent {
    role: WireRole,
    target: WireTarget,
    settings: WireSettings,
    at: u64,
    request: Value,
    selection: WireSelection,
    budget: WireBudget,
}
#[derive(Serialize, Deserialize)]
#[serde(tag = "kind", rename_all = "snake_case", deny_unknown_fields)]
enum WireRole {
    Assistant { turn: u64 },
    Reviewer { generation: u64, call_id: String },
}
#[derive(Serialize, Deserialize)]
#[serde(deny_unknown_fields)]
struct WireTarget {
    provider: String,
    endpoint: String,
}
#[derive(Serialize, Deserialize)]
#[serde(deny_unknown_fields)]
struct WireSettings {
    model: String,
    effort: String,
    max_output_tokens: u32,
}
#[derive(Serialize, Deserialize)]
#[serde(deny_unknown_fields)]
struct WireSelection {
    revision: u64,
    policy: String,
    required: Vec<u64>,
    included: Vec<u64>,
    omitted: Vec<u64>,
}
#[derive(Serialize, Deserialize)]
#[serde(deny_unknown_fields)]
struct WireBudget {
    request_bytes: usize,
    max_request_bytes: usize,
    estimated_input_tokens: u64,
    input_token_budget: u64,
    method: String,
}
#[derive(Serialize, Deserialize)]
#[serde(tag = "kind", rename_all = "snake_case", deny_unknown_fields)]
enum WireObservation {
    Pending {},
    Observed {
        at: u64,
        receipt: WireReceipt,
        disposition: WireDisposition,
    },
    Abandoned {
        at: u64,
    },
}
#[derive(Serialize, Deserialize)]
#[serde(tag = "kind", rename_all = "snake_case", deny_unknown_fields)]
enum WireDisposition {
    Accepted {},
    Refused {
        reason: Option<String>,
    },
    Incomplete {
        reason: String,
    },
    Rejected {
        reason: String,
    },
    Failed {
        error: String,
        http_status: Option<u16>,
    },
}
#[derive(Serialize, Deserialize)]
#[serde(deny_unknown_fields)]
struct WireReceipt {
    http_status: Option<u16>,
    served_model: Option<String>,
    response_id: Option<String>,
    request_id: Option<String>,
    finish: Option<String>,
    usage: Option<WireUsage>,
    capture: WireCapture,
}
#[derive(Serialize, Deserialize)]
#[serde(deny_unknown_fields)]
struct WireUsage {
    input_tokens: Option<u64>,
    output_tokens: Option<u64>,
}
#[derive(Serialize, Deserialize)]
#[serde(tag = "kind", rename_all = "snake_case", deny_unknown_fields)]
enum WireCapture {
    NotReceived {},
    Complete { bytes: Vec<u8> },
    Interrupted { prefix: Vec<u8> },
    Truncated { prefix: Vec<u8>, limit: usize },
}
fn line(bytes: &mut Vec<u8>, value: &impl Serialize) -> Result<(), SessionStoreError> {
    serde_json::to_writer(&mut *bytes, value).map_err(data_error)?;
    bytes.push(b'\n');
    Ok(())
}
fn encode(archive: &SessionArchive) -> Result<Vec<u8>, SessionStoreError> {
    let snapshot = archive.session().snapshot();
    let mut bytes = vec![];
    line(
        &mut bytes,
        &Header::Session {
            session_id: snapshot.session_id,
            revision: archive.revision().get(),
            closed_turns: snapshot.turns.len(),
            active: snapshot.active.is_some(),
            generations: archive.generations().len(),
        },
    )?;
    for turn in snapshot.turns {
        line(&mut bytes, &Record::ClosedTurn { turn: turn.into() })?;
    }
    if let Some(turn) = snapshot.active {
        line(&mut bytes, &Record::ActiveTurn { turn: turn.into() })?;
    }
    for generation in archive.generations() {
        line(
            &mut bytes,
            &Record::Generation {
                generation: Box::new(generation.into()),
            },
        )?;
    }
    Ok(bytes)
}
fn decode(bytes: &[u8]) -> Result<SessionArchive, SessionStoreError> {
    if !bytes.ends_with(b"\n") {
        return Err(invalid("missing final LF"));
    }
    let mut lines = bytes[..bytes.len() - 1].split(|byte| *byte == b'\n');
    let Header::Session {
        session_id,
        revision,
        closed_turns,
        active: has_active,
        generations: count,
    } = serde_json::from_slice(
        lines
            .next()
            .ok_or_else(|| invalid("missing session header"))?,
    )
    .map_err(data_error)
    .map_err(|error| at_record(1, error))?;
    if revision == 0 {
        return Err(at_record(1, invalid("committed revision must be nonzero")));
    }
    let mut turns = vec![];
    let mut active = None;
    let mut generations = vec![];
    for (index, bytes) in lines.enumerate() {
        (|| {
            match serde_json::from_slice::<Record>(bytes).map_err(data_error)? {
                Record::ClosedTurn { turn }
                    if turns.len() < closed_turns && active.is_none() && generations.is_empty() =>
                {
                    turns.push(turn.try_into()?)
                }
                Record::ActiveTurn { turn }
                    if has_active
                        && turns.len() == closed_turns
                        && active.is_none()
                        && generations.is_empty() =>
                {
                    active = Some(turn.try_into()?)
                }
                Record::Generation { generation }
                    if turns.len() == closed_turns
                        && active.is_some() == has_active
                        && generations.len() < count =>
                {
                    generations.push((*generation).try_into()?)
                }
                _ => return Err(invalid("unexpected archive record order or count")),
            }
            Ok(())
        })()
        .map_err(|error| at_record(index + 2, error))?;
    }
    if turns.len() != closed_turns || active.is_some() != has_active || generations.len() != count {
        return Err(invalid("archive record count mismatch"));
    }
    let archive = SessionArchive::restore(ArchiveSnapshot {
        revision,
        session: SessionSnapshot {
            session_id,
            turns,
            active,
        },
        generations,
    })?;
    if encode(&archive)? != bytes {
        return Err(invalid("archive is not canonically encoded"));
    }
    Ok(archive)
}
impl From<&GenerationRecord> for WireGeneration {
    fn from(value: &GenerationRecord) -> Self {
        let intent = value.intent();
        Self {
            id: value.id().get(),
            intent: WireIntent {
                role: match intent.role() {
                    InvocationRole::Assistant { turn } => WireRole::Assistant { turn: turn.get() },
                    InvocationRole::Reviewer { call } => WireRole::Reviewer {
                        generation: call.generation().get(),
                        call_id: call.call_id().as_str().into(),
                    },
                },
                target: WireTarget {
                    provider: intent.target().provider().into(),
                    endpoint: intent.target().endpoint().into(),
                },
                settings: WireSettings {
                    model: intent.settings().model_id.as_str().into(),
                    effort: effort_name(intent.settings().effort).into(),
                    max_output_tokens: intent.settings().max_output_tokens.get(),
                },
                at: intent.at().unix_millis(),
                request: intent.request().clone(),
                selection: WireSelection {
                    revision: intent.selection().revision().get(),
                    policy: intent.selection().policy().to_owned(),
                    required: intent
                        .selection()
                        .required()
                        .iter()
                        .map(|id| id.get())
                        .collect(),
                    included: intent
                        .selection()
                        .included()
                        .iter()
                        .map(|id| id.get())
                        .collect(),
                    omitted: intent
                        .selection()
                        .omitted()
                        .iter()
                        .map(|id| id.get())
                        .collect(),
                },
                budget: WireBudget {
                    request_bytes: intent.budget().request_bytes(),
                    max_request_bytes: intent.budget().max_request_bytes(),
                    estimated_input_tokens: intent.budget().estimated_input_tokens(),
                    input_token_budget: intent.budget().input_token_budget(),
                    method: intent.budget().method().to_owned(),
                },
            },
            observation: match value.observation() {
                Observation::Pending => WireObservation::Pending {},
                Observation::Abandoned { at } => WireObservation::Abandoned {
                    at: at.unix_millis(),
                },
                Observation::Observed {
                    at,
                    receipt,
                    disposition,
                } => WireObservation::Observed {
                    at: at.unix_millis(),
                    receipt: receipt.into(),
                    disposition: match disposition {
                        Disposition::Accepted => WireDisposition::Accepted {},
                        Disposition::Refused { reason } => WireDisposition::Refused {
                            reason: reason.as_ref().map(|text| text.as_str().into()),
                        },
                        Disposition::Incomplete(reason) => WireDisposition::Incomplete {
                            reason: match reason {
                                IncompleteReason::OutputLimit => "output_limit",
                                IncompleteReason::Other => "other",
                            }
                            .into(),
                        },
                        Disposition::Rejected(reason) => WireDisposition::Rejected {
                            reason: reason.as_str().into(),
                        },
                        Disposition::Failed(error) => WireDisposition::Failed {
                            error: error_name(error.kind()).into(),
                            http_status: error.http_status(),
                        },
                    },
                },
            },
        }
    }
}
impl TryFrom<WireGeneration> for GenerationSnapshot {
    type Error = SessionStoreError;
    fn try_from(value: WireGeneration) -> Result<Self, Self::Error> {
        let intent = value.intent;
        GenerationId::new(value.id).map_err(data_error)?;
        for id in intent
            .selection
            .required
            .iter()
            .chain(&intent.selection.included)
            .chain(&intent.selection.omitted)
        {
            TurnId::new(*id).map_err(data_error)?;
        }
        Ok(Self {
            id: value.id,
            role: match intent.role {
                WireRole::Assistant { turn } => InvocationRole::Assistant {
                    turn: TurnId::new(turn).map_err(data_error)?,
                },
                WireRole::Reviewer {
                    generation,
                    call_id,
                } => InvocationRole::Reviewer {
                    call: CallRef::new(
                        GenerationId::new(generation).map_err(data_error)?,
                        ToolCallId::new(call_id).map_err(data_error)?,
                    ),
                },
            },
            target: ModelTarget::new(intent.target.provider, intent.target.endpoint),
            settings: ModelSettings {
                model_id: ModelId::new(intent.settings.model).map_err(data_error)?,
                effort: parse_effort(&intent.settings.effort)?,
                max_output_tokens: NonZeroU32::new(intent.settings.max_output_tokens)
                    .ok_or_else(|| invalid("zero output token limit"))?,
            },
            at: Timestamp::from_unix_millis(intent.at),
            request: intent.request,
            selection: SelectionSnapshot {
                revision: intent.selection.revision,
                policy: intent.selection.policy,
                required: intent.selection.required,
                included: intent.selection.included,
                omitted: intent.selection.omitted,
            },
            budget: BudgetSnapshot {
                request_bytes: intent.budget.request_bytes,
                max_request_bytes: intent.budget.max_request_bytes,
                estimated_input_tokens: intent.budget.estimated_input_tokens,
                input_token_budget: intent.budget.input_token_budget,
                method: intent.budget.method,
            },
            observation: match value.observation {
                WireObservation::Pending {} => Observation::Pending,
                WireObservation::Abandoned { at } => Observation::Abandoned {
                    at: Timestamp::from_unix_millis(at),
                },
                WireObservation::Observed {
                    at,
                    receipt,
                    disposition,
                } => Observation::Observed {
                    at: Timestamp::from_unix_millis(at),
                    receipt: receipt.into(),
                    disposition: match disposition {
                        WireDisposition::Accepted {} => Disposition::Accepted,
                        WireDisposition::Refused { reason } => Disposition::Refused {
                            reason: reason
                                .map(NonEmptyText::new)
                                .transpose()
                                .map_err(data_error)?,
                        },
                        WireDisposition::Incomplete { reason } => {
                            Disposition::Incomplete(match reason.as_str() {
                                "output_limit" => IncompleteReason::OutputLimit,
                                "other" => IncompleteReason::Other,
                                _ => return Err(invalid("unknown incomplete reason")),
                            })
                        }
                        WireDisposition::Rejected { reason } => {
                            Disposition::Rejected(NonEmptyText::new(reason).map_err(data_error)?)
                        }
                        WireDisposition::Failed { error, http_status } => {
                            let kind = parse_error(&error)?;
                            if http_status.is_some_and(|status| !(100..=599).contains(&status)) {
                                return Err(invalid("invalid failure status"));
                            }
                            Disposition::Failed(http_status.map_or_else(
                                || ModelError::new(kind),
                                |status| ModelError::http(kind, status),
                            ))
                        }
                    },
                },
            },
        })
    }
}
impl From<&ResponseReceipt> for WireReceipt {
    fn from(value: &ResponseReceipt) -> Self {
        Self {
            http_status: value.http_status,
            served_model: value.served_model.clone(),
            response_id: value.response_id.clone(),
            request_id: value.request_id.clone(),
            finish: value.finish.clone(),
            usage: value.usage.map(|usage| WireUsage {
                input_tokens: usage.input_tokens,
                output_tokens: usage.output_tokens,
            }),
            capture: match &value.capture {
                ResponseCapture::NotReceived => WireCapture::NotReceived {},
                ResponseCapture::Complete(bytes) => WireCapture::Complete {
                    bytes: bytes.clone(),
                },
                ResponseCapture::Interrupted { prefix } => WireCapture::Interrupted {
                    prefix: prefix.clone(),
                },
                ResponseCapture::Truncated { prefix, limit } => WireCapture::Truncated {
                    prefix: prefix.clone(),
                    limit: *limit,
                },
            },
        }
    }
}
impl From<WireReceipt> for ResponseReceipt {
    fn from(value: WireReceipt) -> Self {
        Self {
            http_status: value.http_status,
            served_model: value.served_model,
            response_id: value.response_id,
            request_id: value.request_id,
            finish: value.finish,
            usage: value.usage.map(|usage| Usage {
                input_tokens: usage.input_tokens,
                output_tokens: usage.output_tokens,
            }),
            capture: match value.capture {
                WireCapture::NotReceived {} => ResponseCapture::NotReceived,
                WireCapture::Complete { bytes } => ResponseCapture::Complete(bytes),
                WireCapture::Interrupted { prefix } => ResponseCapture::Interrupted { prefix },
                WireCapture::Truncated { prefix, limit } => {
                    ResponseCapture::Truncated { prefix, limit }
                }
            },
        }
    }
}
fn effort_name(value: Effort) -> &'static str {
    match value {
        Effort::Default => "default",
        Effort::Off => "off",
        Effort::Low => "low",
        Effort::Medium => "medium",
        Effort::High => "high",
        Effort::Xhigh => "xhigh",
        Effort::Max => "max",
    }
}
fn parse_effort(value: &str) -> Result<Effort, SessionStoreError> {
    Ok(match value {
        "default" => Effort::Default,
        "off" => Effort::Off,
        "low" => Effort::Low,
        "medium" => Effort::Medium,
        "high" => Effort::High,
        "xhigh" => Effort::Xhigh,
        "max" => Effort::Max,
        _ => return Err(invalid("unknown effort")),
    })
}
fn error_name(value: ModelErrorKind) -> &'static str {
    match value {
        ModelErrorKind::InvalidConfig => "invalid_config",
        ModelErrorKind::InvalidSettings => "invalid_settings",
        ModelErrorKind::UnsupportedInput => "unsupported_input",
        ModelErrorKind::ContextCapacity => "context_capacity",
        ModelErrorKind::Transport => "transport",
        ModelErrorKind::Timeout => "timeout",
        ModelErrorKind::Http => "http",
        ModelErrorKind::Authentication => "authentication",
        ModelErrorKind::RateLimited => "rate_limited",
        ModelErrorKind::ProviderFailure => "provider_failure",
        ModelErrorKind::ResponseTooLarge => "response_too_large",
        ModelErrorKind::MalformedResponse => "malformed_response",
        ModelErrorKind::UnsupportedResponse => "unsupported_response",
    }
}
fn parse_error(value: &str) -> Result<ModelErrorKind, SessionStoreError> {
    [
        ModelErrorKind::InvalidConfig,
        ModelErrorKind::InvalidSettings,
        ModelErrorKind::UnsupportedInput,
        ModelErrorKind::ContextCapacity,
        ModelErrorKind::Transport,
        ModelErrorKind::Timeout,
        ModelErrorKind::Http,
        ModelErrorKind::Authentication,
        ModelErrorKind::RateLimited,
        ModelErrorKind::ProviderFailure,
        ModelErrorKind::ResponseTooLarge,
        ModelErrorKind::MalformedResponse,
        ModelErrorKind::UnsupportedResponse,
    ]
    .into_iter()
    .find(|kind| error_name(*kind) == value)
    .ok_or_else(|| invalid("unknown model error kind"))
}

fn invalid(message: &'static str) -> SessionStoreError {
    SessionStoreError::new(
        SessionStoreErrorKind::InvalidData,
        io::Error::new(io::ErrorKind::InvalidData, message),
    )
}
fn data_error(error: impl Error + Send + Sync + 'static) -> SessionStoreError {
    SessionStoreError::new(SessionStoreErrorKind::InvalidData, error)
}
#[derive(Debug)]
struct RecordError {
    index: usize,
    source: SessionStoreError,
}
impl fmt::Display for RecordError {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        write!(f, "record {}: {}", self.index, self.source)
    }
}
impl Error for RecordError {
    fn source(&self) -> Option<&(dyn Error + 'static)> {
        Some(&self.source)
    }
}
fn at_record(index: usize, source: SessionStoreError) -> SessionStoreError {
    SessionStoreError::new(source.kind(), RecordError { index, source })
}
