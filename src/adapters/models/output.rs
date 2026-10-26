use super::{OutputKind, malformed};
use crate::{
    app::ModelError,
    domain::{AssistantMessage, Decision, NonEmptyText, ToolCall, ToolCalls},
};
use serde::Deserialize;
use serde_json::Value;

#[derive(Deserialize)]
#[serde(deny_unknown_fields)]
struct DecisionWire {
    should_allow: bool,
    reason: Value,
}

pub(super) trait Output: Sized {
    const KIND: OutputKind;
    fn decode(text: String, calls: Vec<ToolCall>) -> Result<Self, ModelError>;
    fn receipt(&self) -> Option<AssistantMessage>;
}
impl Output for AssistantMessage {
    const KIND: OutputKind = OutputKind::Assistant;
    fn decode(text: String, calls: Vec<ToolCall>) -> Result<Self, ModelError> {
        if calls.is_empty() {
            AssistantMessage::answer(text).map_err(|_| malformed())
        } else {
            Ok(AssistantMessage::tool_requests(
                Some(text),
                ToolCalls::new(calls).map_err(|_| malformed())?,
            ))
        }
    }
    fn receipt(&self) -> Option<AssistantMessage> {
        Some(self.clone())
    }
}
impl Output for Decision {
    const KIND: OutputKind = OutputKind::Decision;
    fn decode(text: String, calls: Vec<ToolCall>) -> Result<Self, ModelError> {
        if !calls.is_empty() {
            return Err(malformed());
        }
        let value: DecisionWire = serde_json::from_str(&text).map_err(|_| malformed())?;
        let should_allow = value.should_allow;
        let reason = match value.reason {
            Value::Null => None,
            Value::String(text) => Some(NonEmptyText::new(text).map_err(|_| malformed())?),
            _ => return Err(malformed()),
        };
        Ok(Decision::new(should_allow, reason))
    }
    fn receipt(&self) -> Option<AssistantMessage> {
        None
    }
}
