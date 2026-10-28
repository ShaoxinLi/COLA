use crate::{app::*, domain::*};
use serde::Serialize;
use serde_json::Value;

#[derive(Serialize)]
pub(super) struct SessionDto {
    id: String,
    revision: u64,
    turns: Vec<TurnDto>,
    active: Option<TurnDto>,
}

#[derive(Serialize)]
pub(super) struct TurnDto {
    id: u64,
    user: String,
    steps: Vec<StepDto>,
    outcome: Option<OutcomeDto>,
}

#[derive(Serialize)]
pub(super) struct StepDto {
    source_generation: u64,
    assistant: AssistantDto,
    executions: Vec<ExecutionDto>,
}

#[derive(Serialize)]
#[serde(tag = "kind", rename_all = "snake_case")]
pub(super) enum AssistantDto {
    Answer {
        text: String,
    },
    ToolRequests {
        text: Option<String>,
        calls: Vec<CallDto>,
    },
}

#[derive(Serialize)]
pub(super) struct CallDto {
    id: String,
    name: String,
    arguments: Value,
}

#[derive(Serialize)]
pub(super) struct ExecutionDto {
    generation: u64,
    call_id: String,
    state: ExecutionStateDto,
    resolution: Option<ResolutionDto>,
}

#[derive(Serialize)]
#[serde(tag = "kind", rename_all = "snake_case")]
pub(super) enum ExecutionStateDto {
    Requested,
    Denied {
        reviewer: u64,
        reason: Option<String>,
    },
    Approved {
        reviewer: u64,
        reason: Option<String>,
    },
    Started {
        reviewer: u64,
        at: u64,
    },
    Returned {
        reviewer: u64,
        started_at: u64,
        at: u64,
        outcome: ToolOutcomeDto,
    },
}

#[derive(Serialize)]
pub(super) struct ResolutionDto {
    disposition: DispositionDto,
    source: ResolutionSourceDto,
    evidence: String,
}

#[derive(Serialize)]
#[serde(rename_all = "snake_case")]
pub(super) enum DispositionDto {
    Completed,
    NotPerformed,
}

#[derive(Serialize)]
#[serde(rename_all = "snake_case")]
pub(super) enum ResolutionSourceDto {
    Observation,
    UserAttestation,
    ApplicationAttestation,
}

#[derive(Serialize)]
#[serde(tag = "kind", rename_all = "snake_case")]
pub(super) enum OutcomeDto {
    Completed,
    Interrupted { reason: StopDto },
    Failed { failure: TurnFailureDto },
}

#[derive(Serialize)]
#[serde(rename_all = "snake_case")]
pub(super) enum StopDto {
    UserCancelled,
    ProcessLost,
    ContextCapacity,
    IterationLimit,
}

#[derive(Serialize)]
#[serde(tag = "kind", rename_all = "snake_case")]
pub(super) enum TurnFailureDto {
    ModelFailure { text: String },
    MalformedResponse { text: String },
}

#[derive(Serialize)]
#[serde(tag = "kind", rename_all = "snake_case")]
pub(super) enum ToolOutcomeDto {
    Success { text: String },
    Failure { text: String },
}

#[derive(Serialize)]
#[serde(tag = "kind", rename_all = "snake_case")]
pub(super) enum MessageDto {
    User {
        text: String,
    },
    AssistantAnswer {
        text: String,
    },
    AssistantToolRequests {
        text: Option<String>,
        calls: Vec<CallDto>,
    },
    ToolResult {
        call_id: String,
        outcome: ToolOutcomeDto,
    },
}

#[derive(Serialize)]
pub(super) struct FailureDto {
    code: &'static str,
    message: &'static str,
    role: Option<ModelRoleDto>,
    progress: Option<SessionDto>,
    uncommitted: Option<SessionDto>,
    durability: DurabilityDto,
}

#[derive(Serialize)]
#[serde(rename_all = "snake_case")]
pub(super) enum ModelRoleDto {
    Assistant,
    Reviewer,
}

#[derive(Serialize)]
#[serde(rename_all = "snake_case")]
pub(super) enum DurabilityDto {
    Committed,
    NotPublished,
    Uncertain,
}

pub(super) enum FailureClass {
    NotFound,
    Conflict,
    Capacity,
    Refused,
    IterationLimit,
    Incomplete,
    Timeout,
    Model,
    Store,
    Internal,
}

pub(super) fn session_dto(archive: &SessionArchive) -> SessionDto {
    let session = archive.session();
    SessionDto {
        id: session.id().as_str().into(),
        revision: archive.revision().get(),
        turns: session
            .turns()
            .iter()
            .map(|t| TurnDto {
                id: t.id().get(),
                user: t.user().text().as_str().into(),
                steps: t.steps().iter().map(step_dto).collect(),
                outcome: Some(outcome_dto(t.outcome())),
            })
            .collect(),
        active: session.active_turn().map(|t| TurnDto {
            id: t.id().get(),
            user: t.user().text().as_str().into(),
            steps: t.steps().iter().map(step_dto).collect(),
            outcome: None,
        }),
    }
}
fn step_dto(step: &Step) -> StepDto {
    StepDto {
        source_generation: step.source_generation().get(),
        assistant: assistant_dto(step.assistant()),
        executions: step
            .executions()
            .map(|e| ExecutionDto {
                generation: e.call_ref().generation().get(),
                call_id: e.call().id().as_str().into(),
                state: match e.state() {
                    ExecutionState::Requested => ExecutionStateDto::Requested,
                    ExecutionState::Denied(review) => ExecutionStateDto::Denied {
                        reviewer: review.reviewer().get(),
                        reason: review.reason().map(|r| r.as_str().into()),
                    },
                    ExecutionState::Approved(review) => ExecutionStateDto::Approved {
                        reviewer: review.reviewer().get(),
                        reason: review.reason().map(|r| r.as_str().into()),
                    },
                    ExecutionState::Started { review, at } => ExecutionStateDto::Started {
                        reviewer: review.reviewer().get(),
                        at: at.unix_millis(),
                    },
                    ExecutionState::Returned {
                        review,
                        started_at,
                        at,
                        outcome,
                    } => ExecutionStateDto::Returned {
                        reviewer: review.reviewer().get(),
                        started_at: started_at.unix_millis(),
                        at: at.unix_millis(),
                        outcome: tool_outcome_dto(outcome),
                    },
                },
                resolution: e.resolution().map(|r| ResolutionDto {
                    disposition: match r.disposition() {
                        EffectDisposition::Completed => DispositionDto::Completed,
                        EffectDisposition::NotPerformed => DispositionDto::NotPerformed,
                    },
                    source: match r.source() {
                        ResolutionSource::Observation => ResolutionSourceDto::Observation,
                        ResolutionSource::UserAttestation => ResolutionSourceDto::UserAttestation,
                        ResolutionSource::ApplicationAttestation => {
                            ResolutionSourceDto::ApplicationAttestation
                        }
                    },
                    evidence: r.evidence().as_str().into(),
                }),
            })
            .collect(),
    }
}
fn assistant_dto(message: &AssistantMessage) -> AssistantDto {
    match message {
        AssistantMessage::Answer(t) => AssistantDto::Answer {
            text: t.as_str().into(),
        },
        AssistantMessage::ToolRequests { text, calls } => AssistantDto::ToolRequests {
            text: text.as_ref().map(|t| t.as_str().into()),
            calls: calls.as_slice().iter().map(call_dto).collect(),
        },
    }
}
fn call_dto(call: &ToolCall) -> CallDto {
    CallDto {
        id: call.id().as_str().into(),
        name: call.name().as_str().into(),
        arguments: Value::Object(call.arguments().clone()),
    }
}
fn tool_outcome_dto(outcome: &ToolOutcome) -> ToolOutcomeDto {
    match outcome {
        ToolOutcome::Success(t) => ToolOutcomeDto::Success { text: t.clone() },
        ToolOutcome::Failure(e) => ToolOutcomeDto::Failure {
            text: e.description().as_str().into(),
        },
    }
}
fn outcome_dto(outcome: &TurnOutcome) -> OutcomeDto {
    match outcome {
        TurnOutcome::Completed => OutcomeDto::Completed,
        TurnOutcome::Interrupted(reason) => OutcomeDto::Interrupted {
            reason: match reason {
                StopReason::UserCancelled => StopDto::UserCancelled,
                StopReason::ProcessLost => StopDto::ProcessLost,
                StopReason::ContextCapacity => StopDto::ContextCapacity,
                StopReason::IterationLimit => StopDto::IterationLimit,
            },
        },
        TurnOutcome::Failed(failure) => OutcomeDto::Failed {
            failure: match failure {
                TurnFailure::ModelFailure(t) => TurnFailureDto::ModelFailure {
                    text: t.as_str().into(),
                },
                TurnFailure::MalformedResponse(t) => TurnFailureDto::MalformedResponse {
                    text: t.as_str().into(),
                },
            },
        },
    }
}
pub(super) fn message_dtos(messages: &[Message]) -> Vec<MessageDto> {
    messages
        .iter()
        .map(|message| match message {
            Message::User(t) => MessageDto::User {
                text: t.text().as_str().into(),
            },
            Message::Assistant(a) => match assistant_dto(a) {
                AssistantDto::Answer { text } => MessageDto::AssistantAnswer { text },
                AssistantDto::ToolRequests { text, calls } => {
                    MessageDto::AssistantToolRequests { text, calls }
                }
            },
            Message::Tool(t) => MessageDto::ToolResult {
                call_id: t.call_id().as_str().into(),
                outcome: tool_outcome_dto(t.outcome()),
            },
        })
        .collect()
}

pub(super) struct ClassifiedFailure {
    pub class: FailureClass,
    pub dto: FailureDto,
}
pub(super) fn classify(error: &AgentError) -> ClassifiedFailure {
    let (class, code, message, role) = match error.kind() {
        AgentErrorKind::Store(source) => match source.kind() {
            SessionStoreErrorKind::NotFound => (
                FailureClass::NotFound,
                "session_not_found",
                "Session not found",
                None,
            ),
            SessionStoreErrorKind::Busy => (
                FailureClass::Conflict,
                "session_busy",
                "Session is busy",
                None,
            ),
            _ => (
                FailureClass::Store,
                "store_failure",
                "Session storage failed",
                None,
            ),
        },
        AgentErrorKind::Commit(CommitFault::Uncertain(_)) => (
            FailureClass::Store,
            "commit_uncertain",
            "Session publication is uncertain",
            None,
        ),
        AgentErrorKind::Commit(CommitFault::Conflict) => (
            FailureClass::Conflict,
            "commit_conflict",
            "Session commit conflicts with stored data",
            None,
        ),
        AgentErrorKind::Commit(CommitFault::NotPublished(_)) => (
            FailureClass::Store,
            "store_failure",
            "Session commit was not published",
            None,
        ),
        AgentErrorKind::RecoveryRequired => (
            FailureClass::Conflict,
            "recovery_required",
            "Session recovery is required",
            None,
        ),
        AgentErrorKind::ContextCapacity { role } => (
            FailureClass::Capacity,
            "context_capacity",
            "Required context exceeds the configured limits",
            Some(*role),
        ),
        AgentErrorKind::Refused { role, .. } => (
            FailureClass::Refused,
            "generation_refused",
            "Generation was refused",
            Some(*role),
        ),
        AgentErrorKind::Incomplete { role, .. } => (
            FailureClass::Incomplete,
            "generation_incomplete",
            "Generation was incomplete",
            Some(*role),
        ),
        AgentErrorKind::Model { role, source } if source.kind() == ModelErrorKind::Timeout => (
            FailureClass::Timeout,
            "model_timeout",
            "Model request timed out",
            Some(*role),
        ),
        AgentErrorKind::Model { role, .. } => (
            FailureClass::Model,
            "model_failure",
            "Model request failed",
            Some(*role),
        ),
        AgentErrorKind::IterationLimit => (
            FailureClass::IterationLimit,
            "iteration_limit",
            "Assistant iteration limit reached",
            None,
        ),
        AgentErrorKind::DuplicateTool(_) => (
            FailureClass::Internal,
            "duplicate_tool",
            "Duplicate tool registration",
            None,
        ),
        AgentErrorKind::RejectedOutput(_) => (
            FailureClass::Internal,
            "rejected_output",
            "Session transition rejected",
            None,
        ),
    };
    let durability = match error.kind() {
        AgentErrorKind::Commit(CommitFault::Uncertain(_)) => DurabilityDto::Uncertain,
        AgentErrorKind::Commit(_) => DurabilityDto::NotPublished,
        _ => DurabilityDto::Committed,
    };
    ClassifiedFailure {
        class,
        dto: FailureDto {
            code,
            message,
            role: role.map(|r| match r {
                ModelRole::Assistant => ModelRoleDto::Assistant,
                ModelRole::Reviewer => ModelRoleDto::Reviewer,
            }),
            progress: error.durable().map(session_dto),
            uncommitted: error.uncommitted().map(session_dto),
            durability,
        },
    }
}
pub(super) fn input_failure(code: &'static str, message: &'static str) -> FailureDto {
    FailureDto {
        code,
        message,
        role: None,
        progress: None,
        uncommitted: None,
        durability: DurabilityDto::Committed,
    }
}
pub(super) const INPUT_LIMIT: usize = 64 * 1024;
