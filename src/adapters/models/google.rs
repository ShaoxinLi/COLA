use super::{
    Decoded, Event, ModelError, ModelSettings, OutputKind, Record, Snapshot, WireCall, array,
    context, events, malformed, string, unsupported,
};
use crate::app::IncompleteReason;
use serde_json::{Value, json};
fn push(contents: &mut Vec<Value>, role: &str, parts: Vec<Value>) {
    if let Some(last) = contents.last_mut().filter(|last| last["role"] == role) {
        last["parts"]
            .as_array_mut()
            .expect("constructed parts")
            .extend(parts);
    } else {
        contents.push(json!({"role":role,"parts":parts}));
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
    let mut contents = Vec::new();
    if decision {
        for text in context::review_history(snapshot) {
            push(&mut contents, "user", vec![json!({"text":text})]);
        }
    } else {
        for event in events(snapshot, records)? {
            match event {
                Event::User(text) | Event::Fact(text) => {
                    push(&mut contents, "user", vec![json!({"text":text})])
                }
                Event::Answer(text) => {
                    push(&mut contents, "model", vec![json!({"text":text.as_str()})])
                }
                Event::Native(record) => push(&mut contents, "model", record.raw.clone()),
                Event::Result(call, original, result) => {
                    // Calls without native IDs correlate by the original part order and name.
                    let original_call = &original["functionCall"];
                    let mut response = json!({"name":original_call["name"],"response":context::outcome(result.outcome())});
                    if let Some(id) = &call.id {
                        response["id"] = json!(id);
                    }
                    push(
                        &mut contents,
                        "user",
                        vec![json!({"functionResponse":response})],
                    );
                }
            }
        }
    }
    let mut body = json!({"systemInstruction":{"parts":[{"text":context::instructions(snapshot,decision)}]},"contents":contents,"generationConfig":{"maxOutputTokens":settings.max_output_tokens.get()}});
    if let Some(effort) = effort {
        body["generationConfig"]["thinkingConfig"] =
            json!({"thinkingLevel":effort.to_ascii_uppercase()});
    }
    if decision {
        body["generationConfig"]["responseMimeType"] = json!("application/json");
        body["generationConfig"]["responseJsonSchema"] = context::decision_schema();
    } else if !snapshot.tools.is_empty() {
        body["tools"] = json!([{"functionDeclarations":snapshot.tools.iter().map(|tool|json!({"name":tool.name().as_str(),"description":tool.description().as_str(),"parametersJsonSchema":tool.input_schema().document()})).collect::<Vec<_>>()}]);
    }
    Ok(body)
}
pub(super) fn decode(value: Value) -> Result<Decoded, ModelError> {
    if value
        .pointer("/promptFeedback/blockReason")
        .and_then(Value::as_str)
        .is_some_and(|v| v != "BLOCK_REASON_UNSPECIFIED")
    {
        return Ok(Decoded::Blocked);
    }
    let candidates = array(&value, "candidates")?;
    let [candidate] = candidates.as_slice() else {
        return Err(malformed());
    };
    match string(candidate, "finishReason")? {
        "STOP" => {}
        "MAX_TOKENS" => return Ok(Decoded::Incomplete(IncompleteReason::OutputLimit)),
        "SAFETY" | "RECITATION" | "BLOCKLIST" | "PROHIBITED_CONTENT" | "SPII" | "IMAGE_SAFETY" => {
            return Ok(Decoded::Blocked);
        }
        "OTHER" => return Ok(Decoded::Incomplete(IncompleteReason::Other)),
        _ => return Err(unsupported()),
    }
    let content = candidate.get("content").ok_or_else(malformed)?;
    if string(content, "role")? != "model" {
        return Err(malformed());
    }
    let raw = array(content, "parts")?.clone();
    let mut text = String::new();
    let mut calls = Vec::new();
    for (position, part) in raw.iter().enumerate() {
        let map = part.as_object().ok_or_else(malformed)?;
        if map.keys().any(|key| {
            !matches!(
                key.as_str(),
                "text" | "thought" | "thoughtSignature" | "functionCall"
            )
        }) {
            return Err(unsupported());
        }
        if part.get("thoughtSignature").is_some_and(|v| !v.is_string())
            || part.get("thought").is_some_and(|v| !v.is_boolean())
        {
            return Err(malformed());
        }
        match (part.get("text"), part.get("functionCall")) {
            (Some(value), None) => {
                let value = value.as_str().ok_or_else(malformed)?;
                if part.get("thought").and_then(Value::as_bool) != Some(true) {
                    text.push_str(value);
                }
            }
            (None, Some(call)) => {
                if calls.is_empty()
                    && part
                        .get("thoughtSignature")
                        .and_then(Value::as_str)
                        .is_none_or(|s| s.trim().is_empty())
                {
                    return Err(malformed());
                }
                calls.push(WireCall {
                    name: string(call, "name")?.to_owned(),
                    arguments: call.get("args").cloned().unwrap_or_else(|| json!({})),
                    id: call
                        .get("id")
                        .map(|v| v.as_str().map(str::to_owned).ok_or_else(malformed))
                        .transpose()?,
                    position,
                });
            }
            _ => return Err(unsupported()),
        }
    }
    Ok(Decoded::Complete { text, calls, raw })
}
