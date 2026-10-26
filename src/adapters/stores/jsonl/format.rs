use super::{context, invalid};
use crate::{
    app::{SessionStoreError, SessionStoreErrorKind},
    domain::*,
};
use serde::{Deserialize, Serialize};
use serde_json::Value;
use std::io;

#[derive(Deserialize)]
struct VersionProbe {
    format: String,
    version: u64,
}
#[derive(Serialize, Deserialize)]
#[serde(deny_unknown_fields)]
struct Header {
    format: String,
    version: u64,
    session_id: String,
    closed_turn_count: usize,
    active: bool,
}
#[derive(Serialize, Deserialize)]
#[serde(tag = "record", rename_all = "snake_case", deny_unknown_fields)]
enum Record {
    Closed { turn: Turn },
    Active { turn: Turn },
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
    NotStarted {},
    Started {},
    Returned { outcome: ToolReturn },
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
    disposition: Disposition,
    source: Source,
    evidence: String,
}
#[derive(Serialize, Deserialize)]
#[serde(tag = "kind", rename_all = "snake_case", deny_unknown_fields)]
enum Disposition {
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

fn json_error(e: serde_json::Error) -> SessionStoreError {
    SessionStoreError::new(SessionStoreErrorKind::InvalidData, e)
}
fn value_error(e: ValueError) -> SessionStoreError {
    SessionStoreError::new(SessionStoreErrorKind::InvalidData, e)
}
pub(super) fn encode(snapshot: SessionSnapshot) -> Result<Vec<u8>, SessionStoreError> {
    let mut bytes = vec![];
    let header = Header {
        format: "cola.session".into(),
        version: 1,
        session_id: snapshot.session_id,
        closed_turn_count: snapshot.turns.len(),
        active: snapshot.active.is_some(),
    };
    line(&mut bytes, &header)?;
    for turn in snapshot.turns {
        line(&mut bytes, &Record::Closed { turn: turn.into() })?;
    }
    if let Some(turn) = snapshot.active {
        line(&mut bytes, &Record::Active { turn: turn.into() })?;
    }
    Ok(bytes)
}
fn line(bytes: &mut Vec<u8>, value: &impl Serialize) -> Result<(), SessionStoreError> {
    serde_json::to_writer(&mut *bytes, value).map_err(json_error)?;
    bytes.push(b'\n');
    Ok(())
}
pub(super) fn decode(bytes: &[u8]) -> Result<SessionSnapshot, SessionStoreError> {
    if !bytes.ends_with(b"\n") {
        return Err(invalid("missing final LF"));
    }
    let mut lines = bytes[..bytes.len() - 1].split(|b| *b == b'\n');
    let header_bytes = lines.next().ok_or_else(|| invalid("missing header"))?;
    let header = (|| {
        let probe: VersionProbe = serde_json::from_slice(header_bytes).map_err(json_error)?;
        if probe.format != "cola.session" {
            return Err(invalid("unknown session format"));
        }
        if probe.version != 1 {
            return Err(SessionStoreError::new(
                SessionStoreErrorKind::UnsupportedVersion,
                io::Error::new(
                    io::ErrorKind::InvalidData,
                    format!("unsupported session version {}", probe.version),
                ),
            ));
        }
        serde_json::from_slice::<Header>(header_bytes).map_err(json_error)
    })()
    .map_err(|e| context(e, "record 1".into()))?;
    let mut turns = vec![];
    let mut active = None;
    for (index, bytes) in lines.enumerate() {
        (|| {
            let record: Record = serde_json::from_slice(bytes).map_err(json_error)?;
            match record {
                Record::Closed { turn }
                    if active.is_none() && turns.len() < header.closed_turn_count =>
                {
                    turns.push(turn.try_into()?)
                }
                Record::Active { turn }
                    if active.is_none()
                        && header.active
                        && turns.len() == header.closed_turn_count =>
                {
                    active = Some(turn.try_into()?)
                }
                _ => return Err(invalid("unexpected session record order or count")),
            }
            Ok(())
        })()
        .map_err(|e| context(e, format!("record {}", index + 2)))?;
    }
    if turns.len() != header.closed_turn_count || active.is_some() != header.active {
        return Err(invalid("session record count mismatch"));
    }
    Ok(SessionSnapshot {
        session_id: header.session_id,
        turns,
        active,
    })
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
                ExecutionState::NotStarted => State::NotStarted {},
                ExecutionState::Started => State::Started {},
                ExecutionState::Returned(outcome) => State::Returned {
                    outcome: match outcome {
                        ToolOutcome::Success(output) => ToolReturn::Success { output },
                        ToolOutcome::Failure(e) => ToolReturn::Failure {
                            description: e.description().as_str().into(),
                        },
                    },
                },
            },
            resolution: e.resolution.map(|r| Resolution {
                disposition: match r.disposition() {
                    EffectDisposition::Completed => Disposition::Completed {},
                    EffectDisposition::NotPerformed => Disposition::NotPerformed {},
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
                State::NotStarted {} => ExecutionState::NotStarted,
                State::Started {} => ExecutionState::Started,
                State::Returned { outcome } => ExecutionState::Returned(match outcome {
                    ToolReturn::Success { output } => ToolOutcome::Success(output),
                    ToolReturn::Failure { description } => ToolOutcome::Failure(ToolError::new(
                        NonEmptyText::new(description).map_err(value_error)?,
                    )),
                }),
            },
            resolution: e
                .resolution
                .map(|r| {
                    Ok(EffectResolution::new(
                        match r.disposition {
                            Disposition::Completed {} => EffectDisposition::Completed,
                            Disposition::NotPerformed {} => EffectDisposition::NotPerformed,
                        },
                        match r.source {
                            Source::Observation {} => ResolutionSource::Observation,
                            Source::UserAttestation {} => ResolutionSource::UserAttestation,
                            Source::ApplicationAttestation {} => {
                                ResolutionSource::ApplicationAttestation
                            }
                        },
                        NonEmptyText::new(r.evidence).map_err(value_error)?,
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
                    TurnFailure::ModelFailure(NonEmptyText::new(description).map_err(value_error)?)
                }
                Failure::MalformedResponse { description } => TurnFailure::MalformedResponse(
                    NonEmptyText::new(description).map_err(value_error)?,
                ),
            }),
        })
    }
}
