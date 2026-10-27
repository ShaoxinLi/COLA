use crate::{app::*, domain::*};
use serde::Serialize;
use serde_json::Value;

#[derive(Serialize)]
pub(super) struct SessionDto {
    id: String,
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
    call_id: String,
    state: ExecutionStateDto,
    resolution: Option<ResolutionDto>,
}

#[derive(Serialize)]
#[serde(tag = "kind", rename_all = "snake_case")]
pub(super) enum ExecutionStateDto {
    NotStarted,
    Started,
    Returned { outcome: ToolOutcomeDto },
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
    Unconfirmed,
}

pub(super) enum FailureClass {
    NotFound,
    Blocked,
    Refused,
    IterationLimit,
    Incomplete,
    Timeout,
    Model,
    Store,
    Internal,
}

pub(super) fn session_dto(session: &Session) -> SessionDto {
    SessionDto {
        id: session.id().as_str().into(),
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
        assistant: assistant_dto(step.assistant()),
        executions: step
            .executions()
            .map(|e| ExecutionDto {
                call_id: e.call().id().as_str().into(),
                state: match e.state() {
                    ExecutionState::NotStarted => ExecutionStateDto::NotStarted,
                    ExecutionState::Started => ExecutionStateDto::Started,
                    ExecutionState::Returned(o) => ExecutionStateDto::Returned {
                        outcome: tool_outcome_dto(o),
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
        AgentErrorKind::Store {
            at: Checkpoint::LoadSession,
            source,
        } if source.kind() == SessionStoreErrorKind::NotFound => (
            FailureClass::NotFound,
            "session_not_found",
            "Session not found",
            None,
        ),
        AgentErrorKind::Store { .. } => (
            FailureClass::Store,
            "store_failure",
            "Session storage failed",
            None,
        ),
        AgentErrorKind::Blocked { .. } => (
            FailureClass::Blocked,
            "session_blocked",
            "Session is blocked",
            None,
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
        AgentErrorKind::Model { role, source } => {
            if source.kind() == ModelErrorKind::Timeout {
                (
                    FailureClass::Timeout,
                    "model_timeout",
                    "Model request timed out",
                    Some(*role),
                )
            } else {
                (
                    FailureClass::Model,
                    "model_failure",
                    "Model request failed",
                    Some(*role),
                )
            }
        }
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
        AgentErrorKind::CatalogMismatch(_) => (
            FailureClass::Internal,
            "catalog_mismatch",
            "Tool catalog mismatch",
            None,
        ),
        AgentErrorKind::UnknownTool(_) => (
            FailureClass::Internal,
            "unknown_tool",
            "Assistant requested an unknown tool",
            None,
        ),
        AgentErrorKind::Context { role, .. } => (
            FailureClass::Internal,
            "context_failure",
            "Context assembly failed",
            Some(*role),
        ),
        AgentErrorKind::RejectedOutput(_) => (
            FailureClass::Internal,
            "rejected_output",
            "Session transition rejected",
            None,
        ),
        AgentErrorKind::UnexpectedState => (
            FailureClass::Internal,
            "unexpected_state",
            "Unexpected session state",
            None,
        ),
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
            progress: error.progress().map(session_dto),
            durability: DurabilityDto::Unconfirmed,
        },
    }
}
pub(super) fn input_failure(code: &'static str, message: &'static str) -> FailureDto {
    FailureDto {
        code,
        message,
        role: None,
        progress: None,
        durability: DurabilityDto::Unconfirmed,
    }
}
pub(super) const INPUT_LIMIT: usize = 64 * 1024;
