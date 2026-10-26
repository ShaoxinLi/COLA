use super::{Context, ContextError, NonEmptyText, Session, ToolCall};

/// Review output data. It does not authorize execution or prove call freshness.
#[derive(Clone, Debug, PartialEq, Eq)]
pub struct Decision {
    should_allow: bool,
    reason: Option<NonEmptyText>,
}
impl Decision {
    pub fn new(should_allow: bool, reason: Option<NonEmptyText>) -> Self {
        Self {
            should_allow,
            reason,
        }
    }
    pub fn should_allow(&self) -> bool {
        self.should_allow
    }
    pub fn reason(&self) -> Option<&NonEmptyText> {
        self.reason.as_ref()
    }
}

/// Synchronous review context assembly implemented outside the domain library.
/// Implementations select history and render active-turn facts and the explicit call.
/// This contract does not check call membership or invoke models or tools.
pub trait ToolCallReviewer {
    fn context(
        &self,
        session: &Session,
        call: &ToolCall,
    ) -> Result<Context<Decision>, ContextError>;
}
