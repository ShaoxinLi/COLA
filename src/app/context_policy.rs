use super::{ModelError, ModelErrorKind};
use crate::domain::*;

pub trait ContextPolicy {
    fn select(&self, session: &Session) -> ContextSelection;
}
#[derive(Clone, Debug)]
pub struct RecentContextPolicy {
    pub optional_turns: usize,
    pub max_request_bytes: usize,
}
impl Default for RecentContextPolicy {
    fn default() -> Self {
        Self {
            optional_turns: 8,
            max_request_bytes: 64 * 1024,
        }
    }
}
#[derive(Clone, Debug)]
pub struct ContextSelection {
    pub closed_turns: Vec<TurnId>,
    pub mandatory_turns: Vec<TurnId>,
    pub max_request_bytes: usize,
}
impl ContextPolicy for RecentContextPolicy {
    fn select(&self, session: &Session) -> ContextSelection {
        let mandatory_turns: Vec<_> = session
            .turns()
            .iter()
            .filter(|t| {
                t.steps().iter().any(|s| {
                    s.executions()
                        .any(|e| *e.state() == ExecutionState::Started && e.resolution().is_none())
                })
            })
            .map(|t| t.id())
            .collect();
        let optional: Vec<_> = session
            .turns()
            .iter()
            .rev()
            .filter(|t| !mandatory_turns.contains(&t.id()))
            .take(self.optional_turns)
            .map(|t| t.id())
            .collect();
        let closed_turns = session
            .turns()
            .iter()
            .map(|t| t.id())
            .filter(|id| mandatory_turns.contains(id) || optional.contains(id))
            .collect();
        ContextSelection {
            closed_turns,
            mandatory_turns,
            max_request_bytes: self.max_request_bytes,
        }
    }
}
impl ContextSelection {
    pub fn fit(
        &self,
        mut size: impl FnMut(&[TurnId]) -> Result<usize, ModelError>,
    ) -> Result<Vec<TurnId>, ModelError> {
        if !self
            .mandatory_turns
            .iter()
            .all(|id| self.closed_turns.contains(id))
            || self.closed_turns.windows(2).any(|w| w[0] >= w[1])
        {
            return Err(ModelError::new(ModelErrorKind::InvalidSelection));
        }
        let mut selected = self.closed_turns.clone();
        loop {
            if size(&selected)? <= self.max_request_bytes {
                return Ok(selected);
            }
            let Some(index) = selected
                .iter()
                .position(|id| !self.mandatory_turns.contains(id))
            else {
                return Err(ModelError::new(ModelErrorKind::ContextSelectionTooLarge));
            };
            selected.remove(index);
        }
    }
}
