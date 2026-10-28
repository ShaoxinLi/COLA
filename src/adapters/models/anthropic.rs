use super::client::{
    self, Decoded, Event, OutputKind, WireCall, array, malformed, string, unsupported,
};
use crate::domain::ToolOutcome;
use crate::domain::{IncompleteReason, ModelError, ModelSettings};
use serde_json::{Value, json};
fn push(messages: &mut Vec<Value>, role: &str, parts: Vec<Value>) {
    if let Some(last) = messages.last_mut().filter(|last| last["role"] == role) {
        last["content"]
            .as_array_mut()
            .expect("constructed content")
            .extend(parts);
    } else {
        messages.push(json!({"role":role,"content":parts}));
    }
}
pub(super) fn request<'a, O>(
    plan: &crate::domain::ContextPlan<O>,
    settings: &ModelSettings,
    effort: Option<&str>,
    kind: OutputKind,
    events: impl Iterator<Item = &'a Event>,
) -> Value {
    let decision = kind == OutputKind::Decision;
    let mut messages = Vec::new();
    for event in events {
        match event {
            Event::User(text) => push(
                &mut messages,
                "user",
                vec![json!({"type":"text","text":text})],
            ),
            Event::Answer(text) => push(
                &mut messages,
                "assistant",
                vec![json!({"type":"text","text":text.as_str()})],
            ),
            Event::Native(raw) => push(&mut messages, "assistant", raw.to_vec()),
            Event::Result(result) => {
                let content = match result.outcome() {
                    ToolOutcome::Success(text) => text.as_str(),
                    ToolOutcome::Failure(error) => error.description().as_str(),
                };
                push(
                    &mut messages,
                    "user",
                    vec![json!({
                        "type": "tool_result",
                        "tool_use_id": result.call_id().as_str(),
                        "is_error": matches!(result.outcome(), ToolOutcome::Failure(_)),
                        "content": content,
                    })],
                );
            }
        }
    }
    let mut body = json!({
        "model": settings.model_id.as_str(),
        "system": client::instructions(plan, decision),
        "messages": messages,
        "max_tokens": settings.max_output_tokens.get(),
    });
    if settings.effort == crate::domain::Effort::Off {
        body["thinking"] = json!({"type":"disabled"});
    } else if let Some(effort) = effort {
        body["output_config"] = json!({"effort":effort});
    }
    if decision {
        body["output_config"]["format"] =
            json!({"type":"json_schema","schema":client::decision_schema()});
    } else if !plan.tools().is_empty() {
        body["tools"] = json!(
            plan.tools()
                .iter()
                .map(|tool| json!({
                    "name": tool.name().as_str(),
                    "description": tool.description().as_str(),
                    "input_schema": tool.input_schema().document(),
                }))
                .collect::<Vec<_>>()
        );
    }
    body
}
pub(super) fn decode(value: Value) -> Result<Decoded, ModelError> {
    let stop = string(&value, "stop_reason")?;
    match stop {
        "max_tokens" => return Ok(Decoded::Incomplete(IncompleteReason::OutputLimit)),
        "refusal" => return Ok(Decoded::Refused),
        "end_turn" | "tool_use" => {}
        "pause_turn" | "stop_sequence" | "model_context_window_exceeded" => {
            return Ok(Decoded::Incomplete(IncompleteReason::Other));
        }
        _ => return Err(unsupported()),
    }
    if string(&value, "type")? != "message" || string(&value, "role")? != "assistant" {
        return Err(malformed());
    }
    let raw = array(&value, "content")?.clone();
    let mut text = String::new();
    let mut calls = Vec::new();
    for item in &raw {
        match string(item, "type")? {
            "text" => text.push_str(string(item, "text")?),
            "thinking" => {
                string(item, "thinking")?;
                string(item, "signature")?;
            }
            "redacted_thinking" => {
                string(item, "data")?;
            }
            "tool_use" => calls.push(WireCall {
                name: string(item, "name")?.to_owned(),
                arguments: item.get("input").ok_or_else(malformed)?.clone(),
                id: string(item, "id")?.to_owned(),
            }),
            _ => return Err(unsupported()),
        }
    }
    if (stop == "tool_use") != !calls.is_empty() {
        return Err(malformed());
    }
    Ok(Decoded::Complete { text, calls, raw })
}

#[derive(Clone)]
pub struct AnthropicClient(super::client::Engine);
impl AnthropicClient {
    pub fn new(config: super::ClientConfig) -> Result<Self, ModelError> {
        Ok(Self(super::client::Engine::new(
            super::client::Provider::Anthropic,
            config,
        )?))
    }
}
impl std::fmt::Debug for AnthropicClient {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        f.write_str("AnthropicClient([redacted])")
    }
}
impl<O: super::client::Output> crate::app::ModelClient<O> for AnthropicClient {
    fn prepare(
        &self,
        plan: &crate::domain::ContextPlan<O>,
        settings: &ModelSettings,
    ) -> Result<crate::domain::PreparedRequest<O>, ModelError> {
        self.0.prepare(plan, settings)
    }
    fn invoke(&self, request: &crate::domain::PreparedRequest<O>) -> crate::app::ObservedReply<O> {
        self.0.invoke(request)
    }
}
