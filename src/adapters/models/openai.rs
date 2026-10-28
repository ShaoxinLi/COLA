use super::client::{
    self, Decoded, Event, OutputKind, WireCall, array, malformed, string, unsupported,
};
use crate::domain::{IncompleteReason, ModelError, ModelErrorKind, ModelSettings};
use serde_json::{Value, json};

pub(super) fn request<'a, O>(
    plan: &crate::domain::ContextPlan<O>,
    settings: &ModelSettings,
    effort: Option<&str>,
    kind: OutputKind,
    events: impl Iterator<Item = &'a Event>,
) -> Value {
    let decision = kind == OutputKind::Decision;
    let mut input = Vec::new();
    for event in events {
        match event {
            Event::User(text) => {
                input.push(json!({"role": "user", "content": text}));
            }
            Event::Answer(text) => {
                input.push(json!({"role": "assistant", "content": text.as_str()}));
            }
            Event::Native(raw) => input.extend(raw.iter().cloned()),
            Event::Result(result) => input.push(json!({
                "type": "function_call_output",
                "call_id": result.call_id().as_str(),
                "output": client::outcome(result.outcome()).to_string(),
            })),
        }
    }
    let mut body = json!({
        "model": settings.model_id.as_str(),
        "instructions": client::instructions(plan, decision),
        "input": input,
        "max_output_tokens": settings.max_output_tokens.get(),
        "store": false,
        "include": ["reasoning.encrypted_content"],
    });
    if let Some(effort) = effort {
        body["reasoning"] = json!({"effort":effort});
    }
    if decision {
        body["text"] = json!({"format": {
            "type": "json_schema", "name": "decision", "strict": true,
            "schema": client::decision_schema(),
        }});
    } else if !plan.tools().is_empty() {
        body["tools"] = json!(
            plan.tools()
                .iter()
                .map(|tool| json!({
                    "type": "function",
                    "name": tool.name().as_str(),
                    "description": tool.description().as_str(),
                    "parameters": tool.input_schema().document(),
                    "strict": false,
                }))
                .collect::<Vec<_>>()
        );
    }
    body
}
pub(super) fn decode(value: Value) -> Result<Decoded, ModelError> {
    match string(&value, "status")? {
        "incomplete" => {
            return Ok(Decoded::Incomplete(
                if value
                    .pointer("/incomplete_details/reason")
                    .and_then(Value::as_str)
                    == Some("max_output_tokens")
                {
                    IncompleteReason::OutputLimit
                } else {
                    IncompleteReason::Other
                },
            ));
        }
        "completed" => {}
        "failed" => return Err(ModelError::new(ModelErrorKind::ProviderFailure)),
        "cancelled" | "queued" | "in_progress" => {
            return Ok(Decoded::Incomplete(IncompleteReason::Other));
        }
        _ => return Err(unsupported()),
    }
    let raw = array(&value, "output")?.clone();
    let mut text = String::new();
    let mut calls = Vec::new();
    let mut item_ids = std::collections::BTreeSet::new();
    for item in &raw {
        let kind = string(item, "type")?;
        if let Some(id) = item
            .get("id")
            .filter(|id| !id.is_null() || kind != "function_call")
        {
            let id = id.as_str().ok_or_else(malformed)?;
            if id.trim().is_empty() || !item_ids.insert(id) {
                return Err(malformed());
            }
        }
        if item.get("status").is_some_and(|v| {
            v.as_str() != Some("completed")
                && !(v.is_null() && matches!(kind, "reasoning" | "function_call"))
        }) {
            return Ok(Decoded::Incomplete(IncompleteReason::Other));
        }
        match kind {
            "reasoning" => {
                for part in array(item, "summary")? {
                    if string(part, "type")? != "summary_text" {
                        return Err(unsupported());
                    }
                    string(part, "text")?;
                }
                if item
                    .get("encrypted_content")
                    .is_some_and(|v| !v.is_string() && !v.is_null())
                {
                    return Err(malformed());
                }
            }
            "message" => {
                if string(item, "role")? != "assistant" {
                    return Err(malformed());
                }
                for content in array(item, "content")? {
                    match string(content, "type")? {
                        "output_text" => text.push_str(string(content, "text")?),
                        "refusal" => return Ok(Decoded::Refused),
                        _ => return Err(unsupported()),
                    }
                }
            }
            "function_call" => calls.push(WireCall {
                name: string(item, "name")?.to_owned(),
                arguments: serde_json::from_str(string(item, "arguments")?)
                    .map_err(|_| malformed())?,
                id: string(item, "call_id")?.to_owned(),
            }),
            _ => return Err(unsupported()),
        }
    }
    Ok(Decoded::Complete { text, calls, raw })
}

#[derive(Clone)]
pub struct OpenAIClient(super::client::Engine);
impl OpenAIClient {
    pub fn new(config: super::ClientConfig) -> Result<Self, ModelError> {
        Ok(Self(super::client::Engine::new(
            super::client::Provider::OpenAI,
            config,
        )?))
    }
}
impl std::fmt::Debug for OpenAIClient {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        f.write_str("OpenAIClient([redacted])")
    }
}
impl<O: super::client::Output> crate::app::ModelClient<O> for OpenAIClient {
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
