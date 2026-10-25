use super::{NonEmptyText, ToolCalls, ToolResult, ValueError};

#[derive(Clone, Debug, PartialEq, Eq)]
pub struct UserMessage(NonEmptyText);
impl UserMessage {
    pub fn new(text: NonEmptyText) -> Self {
        Self(text)
    }
    pub fn text(&self) -> &NonEmptyText {
        &self.0
    }
}

#[derive(Clone, Debug, PartialEq, Eq)]
pub enum AssistantMessage {
    Answer(NonEmptyText),
    ToolRequests {
        text: Option<NonEmptyText>,
        calls: ToolCalls,
    },
}
impl AssistantMessage {
    /// Whitespace-only optional preambles normalize to `None`.
    pub fn tool_requests(text: Option<String>, calls: ToolCalls) -> Self {
        Self::ToolRequests {
            text: text.and_then(|text| NonEmptyText::new(text).ok()),
            calls,
        }
    }
    pub fn answer(text: impl Into<String>) -> Result<Self, ValueError> {
        Ok(Self::Answer(NonEmptyText::new(text)?))
    }
    pub fn calls(&self) -> Option<&ToolCalls> {
        match self {
            Self::Answer(_) => None,
            Self::ToolRequests { calls, .. } => Some(calls),
        }
    }
}

#[derive(Clone, Debug, PartialEq, Eq)]
pub enum Message {
    User(UserMessage),
    Assistant(AssistantMessage),
    Tool(ToolResult),
}
