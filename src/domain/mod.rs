//! Validated messages, conversation state, recovery, and model input values.
mod assistant;
mod context;
mod conversation;
mod message;
mod review;
mod tool;
mod value;

pub use assistant::Assistant;
pub use context::{Context, ContextError, ContextExchange, ExecutionNotice};
pub use conversation::{
    ActiveTurn, AssistantSnapshot, EffectDisposition, EffectResolution, ExecutionSnapshot,
    ExecutionState, NextAction, ResolutionSource, RestoreError, Session, SessionError,
    SessionSnapshot, Step, StepSnapshot, StopReason, ToolCallSnapshot, ToolExecution, Turn,
    TurnFailure, TurnOutcome, TurnSnapshot,
};
pub use message::{AssistantMessage, Message, UserMessage};
pub use review::{Decision, ToolCallReviewer};
pub use tool::{
    InputSchema, Tool, ToolCall, ToolCalls, ToolDefinition, ToolError, ToolOutcome, ToolResult,
};
pub use value::{NonEmptyText, SessionId, ToolCallId, ToolName, TurnId, ValueError};
