use std::{error::Error, fmt};

#[derive(Clone, Debug, PartialEq, Eq)]
pub enum ValueError {
    BlankText,
    BlankIdentity,
    ZeroTurnId,
    InvalidToolName,
    EmptyToolCalls,
    DuplicateToolCallId(ToolCallId),
    ExpectedObject,
    InvalidInputSchema,
}
impl fmt::Display for ValueError {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        match self {
            Self::BlankText => f.write_str("text must contain a non-whitespace character"),
            Self::BlankIdentity => f.write_str("identity must contain a non-whitespace character"),
            Self::ZeroTurnId => f.write_str("turn IDs start at one"),
            Self::InvalidToolName => f.write_str("tool name must match [A-Za-z_][A-Za-z0-9_-]*"),
            Self::EmptyToolCalls => f.write_str("a tool request batch must contain a call"),
            Self::DuplicateToolCallId(id) => write!(f, "duplicate tool call ID {id}"),
            Self::ExpectedObject => f.write_str("tool arguments must be a JSON object"),
            Self::InvalidInputSchema => {
                f.write_str("input schema must be an object with type set to object")
            }
        }
    }
}
impl Error for ValueError {}

/// Nonblank text. Validation preserves the original text, including whitespace.
#[derive(Clone, Debug, PartialEq, Eq)]
pub struct NonEmptyText(String);
impl NonEmptyText {
    pub fn new(text: impl Into<String>) -> Result<Self, ValueError> {
        let text = text.into();
        if text.trim().is_empty() {
            return Err(ValueError::BlankText);
        }
        Ok(Self(text))
    }
    pub fn as_str(&self) -> &str {
        &self.0
    }
}
impl fmt::Display for NonEmptyText {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        f.write_str(&self.0)
    }
}

macro_rules! identity {
    ($name:ident) => {
        /// An opaque caller-supplied identity. Only blank values are rejected.
        #[derive(Clone, Debug, PartialEq, Eq, PartialOrd, Ord, Hash)]
        pub struct $name(String);
        impl $name {
            pub fn new(value: impl Into<String>) -> Result<Self, ValueError> {
                let value = value.into();
                if value.trim().is_empty() {
                    return Err(ValueError::BlankIdentity);
                }
                Ok(Self(value))
            }
            pub fn as_str(&self) -> &str {
                &self.0
            }
        }
        impl fmt::Display for $name {
            fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
                f.write_str(&self.0)
            }
        }
    };
}
identity!(SessionId);
identity!(ToolCallId);

/// A nonzero turn identifier. `Session` allocates consecutive IDs starting at one.
#[derive(Clone, Copy, Debug, PartialEq, Eq, PartialOrd, Ord, Hash)]
pub struct TurnId(u64);
impl TurnId {
    pub fn new(value: u64) -> Result<Self, ValueError> {
        if value == 0 {
            return Err(ValueError::ZeroTurnId);
        }
        Ok(Self(value))
    }
    pub fn get(self) -> u64 {
        self.0
    }
}

/// A name matching `[A-Za-z_][A-Za-z0-9_-]*`, with no length limit.
#[derive(Clone, Debug, PartialEq, Eq, PartialOrd, Ord, Hash)]
pub struct ToolName(String);
impl ToolName {
    pub fn new(value: impl Into<String>) -> Result<Self, ValueError> {
        let value = value.into();
        let mut bytes = value.bytes();
        if !bytes
            .next()
            .is_some_and(|c| c.is_ascii_alphabetic() || c == b'_')
            || !bytes.all(|c| c.is_ascii_alphanumeric() || c == b'_' || c == b'-')
        {
            return Err(ValueError::InvalidToolName);
        }
        Ok(Self(value))
    }
    pub fn as_str(&self) -> &str {
        &self.0
    }
}
impl fmt::Display for ToolName {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        f.write_str(&self.0)
    }
}
