//! Validated messages, conversation state, recovery, and model input values.
mod context;
mod conversation;
mod message;
mod roles;
mod tool;
mod value;

pub use context::{Context, ContextError, ContextExchange, ExecutionNotice};
pub use conversation::{
    ActiveTurn, AssistantSnapshot, EffectDisposition, EffectResolution, ExecutionSnapshot,
    ExecutionState, NextAction, ResolutionSource, RestoreError, Session, SessionError,
    SessionSnapshot, Step, StepSnapshot, StopReason, ToolCallSnapshot, ToolExecution, Turn,
    TurnFailure, TurnOutcome, TurnSnapshot,
};
pub use message::{AssistantMessage, Message, UserMessage};
pub use roles::{Assistant, Decision, ToolCallReviewer};
pub use tool::{
    InputSchema, Tool, ToolCall, ToolCalls, ToolDefinition, ToolError, ToolOutcome, ToolResult,
};
pub use value::{CallAddress, NonEmptyText, SessionId, ToolCallId, ToolName, TurnId, ValueError};
