use super::NonEmptyText;

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
