use super::replay::Snapshot;
use crate::domain::{
    EffectDisposition, ExecutionNotice, ResolutionSource, StopReason, ToolOutcome, TurnFailure,
    TurnOutcome,
};
use serde_json::{Value, json};

pub(super) fn decision_schema() -> Value {
    json!({"type":"object","properties":{"should_allow":{"type":"boolean"},"reason":{"type":["string","null"]}},"required":["should_allow","reason"],"additionalProperties":false})
}
pub(super) fn instructions(snapshot: &Snapshot, decision: bool) -> String {
    let mut text = snapshot.instructions.as_str().to_owned();
    if decision && !snapshot.tools.is_empty() {
        text.push_str("\nReview tool catalog (descriptive only, not callable):\n");
        for tool in &snapshot.tools {
            text.push_str(&json!({"name":tool.name().as_str(),"description":tool.description().as_str(),"parameters":tool.input_schema().document()}).to_string());
            text.push('\n');
        }
    }
    text
}
pub(super) fn outcome(value: &ToolOutcome) -> Value {
    match value {
        ToolOutcome::Success(text) => json!({"success":true,"output":text}),
        ToolOutcome::Failure(error) => {
            json!({"success":false,"error":error.description().as_str()})
        }
    }
}
fn turn_outcome(value: &TurnOutcome) -> Value {
    match value {
        TurnOutcome::Completed => json!({"status":"completed"}),
        TurnOutcome::Interrupted(reason) => {
            json!({"status":"interrupted","reason":match reason { StopReason::UserCancelled=>"user_cancelled", StopReason::ProcessLost=>"process_lost", StopReason::ContextCapacity=>"context_capacity", StopReason::IterationLimit=>"iteration_limit" }})
        }
        TurnOutcome::Failed(reason) => match reason {
            TurnFailure::ModelFailure(text) => {
                json!({"status":"failed","kind":"model_failure","reason":text.as_str()})
            }
            TurnFailure::MalformedResponse(text) => {
                json!({"status":"failed","kind":"malformed_response","reason":text.as_str()})
            }
        },
    }
}
fn resolution(value: &crate::domain::EffectResolution) -> Value {
    let disposition = match value.disposition() {
        EffectDisposition::Completed => "completed",
        EffectDisposition::NotPerformed => "not_performed",
    };
    let source = match value.source() {
        ResolutionSource::Observation => "observation",
        ResolutionSource::UserAttestation => "user_attestation",
        ResolutionSource::ApplicationAttestation => "application_attestation",
    };
    json!({"disposition":disposition,"source":source,"evidence":value.evidence().as_str()})
}
fn notice(value: &ExecutionNotice) -> Value {
    match value {
        ExecutionNotice::TurnClosed { turn_id, outcome } => json!({
            "fact":"turn_closed", "turn":turn_id.get(), "outcome":turn_outcome(outcome)
        }),
        ExecutionNotice::Undispatched { turn_id, call } => json!({
            "fact":"undispatched", "turn":turn_id.get(), "call_id":call.id().as_str(),
            "name":call.name().as_str(), "arguments":call.arguments()
        }),
        ExecutionNotice::UnknownEffect {
            turn_id,
            call,
            resolution: proof,
        } => json!({
            "fact":"unknown_effect", "turn":turn_id.get(), "call_id":call.id().as_str(),
            "name":call.name().as_str(), "arguments":call.arguments(),
            "resolution":proof.as_ref().map(resolution)
        }),
        ExecutionNotice::ConfirmedResult {
            turn_id,
            call,
            result,
        } => json!({
            "fact":"confirmed_result", "turn":turn_id.get(), "call_id":call.id().as_str(),
            "name":call.name().as_str(), "arguments":call.arguments(), "result":outcome(result.outcome())
        }),
    }
}
pub(super) fn notices(notices: &[ExecutionNotice]) -> Option<String> {
    if notices.is_empty() {
        return None;
    }
    let facts: Vec<Value> = notices.iter().map(notice).collect();
    Some(format!(
        "Execution facts (not new tool results): {}",
        json!(facts)
    ))
}
pub(super) fn boundary(exchange: &crate::domain::ContextExchange) -> String {
    json!({"turn":exchange.turn_id().get(),"outcome":exchange.outcome().map(turn_outcome)})
        .to_string()
}
fn review_message(message: &crate::domain::Message) -> Value {
    use crate::domain::{AssistantMessage, Message};
    match message {
        Message::User(user) => json!({"role":"user","text":user.text().as_str()}),
        Message::Assistant(AssistantMessage::Answer(text)) => {
            json!({"role":"assistant","text":text.as_str()})
        }
        Message::Assistant(AssistantMessage::ToolRequests { text, calls }) => {
            let calls: Vec<Value> = calls.as_slice().iter().map(|call|json!({
                "id":call.id().as_str(), "name":call.name().as_str(), "arguments":call.arguments()
            })).collect();
            json!({"role":"assistant","text":text.as_ref().map(|t|t.as_str()),"requested_calls":calls})
        }
        Message::Tool(result) => json!({
            "role":"tool", "id":result.call_id().as_str(), "result":outcome(result.outcome())
        }),
    }
}
pub(super) fn review_history(snapshot: &Snapshot) -> Vec<String> {
    if snapshot.exchanges.is_empty() {
        return vec!["Return the decision requested by the review instructions.".into()];
    }
    snapshot
        .exchanges
        .iter()
        .map(|exchange| {
            let messages: Vec<Value> = exchange.messages().iter().map(review_message).collect();
            format!(
                "Review history, descriptive facts only. Turn {}\n{}\n{}",
                boundary(exchange),
                json!(messages),
                notices(exchange.notices()).unwrap_or_default()
            )
        })
        .collect()
}
