//! Native blocking model adapters. Provider JSON and immutable replay records stay private.
mod anthropic;
mod client;
mod context;
mod openai;
mod output;
mod replay;
mod transport;

use crate::{
    app::{IncompleteReason, ModelError, ModelErrorKind, ModelSettings},
    domain::{AssistantMessage, Message, NonEmptyText},
};
pub use anthropic::AnthropicClient;
pub use openai::OpenAIClient;
use replay::{NativeCall, Record, Snapshot};
use serde_json::Value;
pub use transport::ClientConfig;

#[derive(Clone, Copy, PartialEq, Eq)]
pub(super) enum Provider {
    OpenAI,
    Anthropic,
}
#[derive(Clone, Copy, PartialEq, Eq)]
pub(super) enum OutputKind {
    Assistant,
    Decision,
}
pub(super) fn error(kind: ModelErrorKind) -> ModelError {
    ModelError::new(kind)
}
pub(super) fn malformed() -> ModelError {
    error(ModelErrorKind::MalformedResponse)
}
pub(super) fn unsupported() -> ModelError {
    error(ModelErrorKind::UnsupportedResponse)
}
pub(super) fn string<'a>(value: &'a Value, key: &str) -> Result<&'a str, ModelError> {
    value.get(key).and_then(Value::as_str).ok_or_else(malformed)
}
pub(super) fn array<'a>(value: &'a Value, key: &str) -> Result<&'a Vec<Value>, ModelError> {
    value
        .get(key)
        .and_then(Value::as_array)
        .ok_or_else(malformed)
}

pub(super) enum Decoded {
    Complete {
        text: String,
        calls: Vec<WireCall>,
        raw: Vec<Value>,
    },
    Refused,
    Incomplete(IncompleteReason),
}
pub(super) struct WireCall {
    name: String,
    arguments: Value,
    id: String,
}

enum Event<'a> {
    User(String),
    Answer(&'a NonEmptyText),
    Native(&'a Record),
    Result(&'a NativeCall, &'a crate::domain::ToolResult),
    Fact(String),
}
fn events<'a>(snapshot: &'a Snapshot, records: &'a [Record]) -> Result<Vec<Event<'a>>, ModelError> {
    let mut events = Vec::new();
    for exchange in &snapshot.exchanges {
        for (index, message) in exchange.messages().iter().enumerate() {
            match message {
                Message::User(user) => events.push(Event::User(format!(
                    "Turn {}\n{}",
                    context::boundary(exchange),
                    user.text().as_str()
                ))),
                Message::Assistant(assistant) => {
                    if let Some(record) = records
                        .iter()
                        .find(|r| r.turn == exchange.turn_id() && r.index == index)
                    {
                        events.push(Event::Native(record));
                    } else if let AssistantMessage::Answer(text) = assistant {
                        events.push(Event::Answer(text));
                    } else {
                        return Err(error(ModelErrorKind::MissingContinuation));
                    }
                }
                Message::Tool(result) => {
                    let call = records
                        .iter()
                        .find_map(|record| {
                            record
                                .calls
                                .iter()
                                .find(|call| call.call.id() == result.call_id())
                        })
                        .ok_or_else(|| error(ModelErrorKind::MissingContinuation))?;
                    events.push(Event::Result(call, result));
                }
            }
        }
        if let Some(fact) = context::notices(exchange.notices()) {
            events.push(Event::Fact(fact));
        }
    }
    Ok(events)
}
