use super::{
    Decoded, Event, ModelError, ModelSettings, OutputKind, Record, Snapshot, WireCall, array,
    context, events, malformed, string, unsupported,
};
use crate::app::{IncompleteReason, ModelErrorKind};
use serde_json::{Value, json};

pub(super) fn request(
    snapshot: &Snapshot,
    settings: &ModelSettings,
    effort: Option<&str>,
    kind: OutputKind,
    records: &[Record],
) -> Result<Value, ModelError> {
    let decision = kind == OutputKind::Decision;
    let mut input = Vec::new();
    if decision {
        for text in context::review_history(snapshot) {
            input.push(json!({"role":"user","content":text}));
        }
    } else {
        for event in events(snapshot, records)? {
            match event {
                Event::User(text) | Event::Fact(text) => {
                    input.push(json!({"role": "user", "content": text}));
                }
                Event::Answer(text) => {
                    input.push(json!({"role": "assistant", "content": text.as_str()}));
                }
                Event::Native(record) => input.extend(record.raw.clone()),
                Event::Result(call, result) => input.push(json!({
                    "type": "function_call_output",
                    "call_id": &call.id,
                    "output": context::outcome(result.outcome()).to_string(),
                })),
            }
        }
    }
    let mut body = json!({
        "model": settings.model_id.as_str(),
        "instructions": context::instructions(snapshot, decision),
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
            "schema": context::decision_schema(),
        }});
    } else if !snapshot.tools.is_empty() {
        body["tools"] = json!(
            snapshot
                .tools
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
    Ok(body)
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
        if let Some(id) = item.get("id") {
            let id = id.as_str().ok_or_else(malformed)?;
            if id.trim().is_empty() || !item_ids.insert(id) {
                return Err(malformed());
            }
        }
        if item
            .get("status")
            .is_some_and(|v| v.as_str() != Some("completed"))
        {
            return Ok(Decoded::Incomplete(IncompleteReason::Other));
        }
        match string(item, "type")? {
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
            super::Provider::OpenAI,
            config,
        )?))
    }
}
impl std::fmt::Debug for OpenAIClient {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        f.write_str("OpenAIClient([redacted])")
    }
}
impl<O: super::output::Output> crate::app::ModelClient<O> for OpenAIClient {
    fn generate(
        &self,
        context: &crate::domain::Context<O>,
        settings: &ModelSettings,
        continuation: Option<&crate::app::Continuation<O>>,
    ) -> Result<crate::app::Generation<O>, ModelError> {
        self.0.generate(context, settings, continuation)
    }
}
