use super::{AssistantMessage, Context, ContextError, Session};

/// Synchronous context assembly implemented outside the domain library.
/// Model invocation and output decoding belong to the caller.
pub trait Assistant {
    fn context(&self, session: &Session) -> Result<Context<AssistantMessage>, ContextError>;
}
