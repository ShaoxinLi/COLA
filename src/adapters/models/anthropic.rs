use super::{
    Decoded, Event, ModelError, ModelSettings, OutputKind, Record, Snapshot, WireCall, array,
    context, events, malformed, string, unsupported,
};
use crate::{app::IncompleteReason, domain::ToolOutcome};
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
pub(super) fn request(
    snapshot: &Snapshot,
    settings: &ModelSettings,
    effort: Option<&str>,
    kind: OutputKind,
    records: &[Record],
) -> Result<Value, ModelError> {
    let decision = kind == OutputKind::Decision;
    let mut messages = Vec::new();
    if decision {
        for text in context::review_history(snapshot) {
            push(
                &mut messages,
                "user",
                vec![json!({"type":"text","text":text})],
            );
        }
    } else {
        for event in events(snapshot, records)? {
            match event {
                Event::User(text) | Event::Fact(text) => push(
                    &mut messages,
                    "user",
                    vec![json!({"type":"text","text":text})],
                ),
                Event::Answer(text) => push(
                    &mut messages,
                    "assistant",
                    vec![json!({"type":"text","text":text.as_str()})],
                ),
                Event::Native(record) => push(&mut messages, "assistant", record.raw.clone()),
                Event::Result(call, _, result) => push(
                    &mut messages,
                    "user",
                    vec![
                        json!({"type":"tool_result","tool_use_id":call.id.as_ref().ok_or_else(malformed)?,"is_error":matches!(result.outcome(),ToolOutcome::Failure(_)),"content":match result.outcome(){ToolOutcome::Success(text)=>text.as_str(),ToolOutcome::Failure(error)=>error.description().as_str()}}),
                    ],
                ),
            }
        }
    }
    let mut body = json!({"model":settings.model_id.as_str(),"system":context::instructions(snapshot,decision),"messages":messages,"max_tokens":settings.max_output_tokens.get(),"thinking":{"type":"adaptive"}});
    if let Some(effort) = effort {
        body["output_config"] = json!({"effort":effort});
    }
    if decision {
        if body.get("output_config").is_none() {
            body["output_config"] = json!({});
        }
        body["output_config"]["format"] =
            json!({"type":"json_schema","schema":context::decision_schema()});
    } else if !snapshot.tools.is_empty() {
        body["tools"]=json!(snapshot.tools.iter().map(|tool|json!({"name":tool.name().as_str(),"description":tool.description().as_str(),"input_schema":tool.input_schema().document()})).collect::<Vec<_>>());
    }
    Ok(body)
}
pub(super) fn decode(value: Value) -> Result<Decoded, ModelError> {
    let stop = string(&value, "stop_reason")?;
    match stop {
        "max_tokens" => return Ok(Decoded::Incomplete(IncompleteReason::OutputLimit)),
        "refusal" => return Ok(Decoded::Refused),
        "end_turn" | "tool_use" => {}
        "pause_turn" | "stop_sequence" => return Ok(Decoded::Incomplete(IncompleteReason::Other)),
        _ => return Err(unsupported()),
    }
    if string(&value, "type")? != "message" || string(&value, "role")? != "assistant" {
        return Err(malformed());
    }
    let raw = array(&value, "content")?.clone();
    let mut text = String::new();
    let mut calls = Vec::new();
    for (position, item) in raw.iter().enumerate() {
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
                id: Some(string(item, "id")?.to_owned()),
                position,
            }),
            _ => return Err(unsupported()),
        }
    }
    if (stop == "tool_use") != !calls.is_empty() {
        return Err(malformed());
    }
    Ok(Decoded::Complete { text, calls, raw })
}
