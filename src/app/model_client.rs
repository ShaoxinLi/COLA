use super::{
    ContextSelection, ModelArtifact, RequestEvidence, ResponseEvidence, SessionStoreError, TurnView,
};
use crate::domain::{Context, NonEmptyText};
use serde::{Deserialize, Serialize};
use std::{error::Error, fmt, num::NonZeroU32};

/// A nonblank model identifier. Its contents are never included in diagnostics.
#[derive(Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct ModelId(String);
impl ModelId {
    pub fn new(value: impl Into<String>) -> Result<Self, ModelError> {
        let value = value.into();
        if value.trim().is_empty() {
            return Err(ModelError::new(ModelErrorKind::InvalidSettings));
        }
        Ok(Self(value))
    }
    pub fn as_str(&self) -> &str {
        &self.0
    }
}
impl fmt::Debug for ModelId {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        f.write_str("ModelId([redacted])")
    }
}

#[derive(Clone, Copy, Debug, Default, PartialEq, Eq, Serialize, Deserialize)]
pub enum Effort {
    #[default]
    Default,
    Off,
    Low,
    Medium,
    High,
    Xhigh,
    Max,
}
#[derive(Clone, Debug, PartialEq, Eq, Serialize, Deserialize)]
pub struct ModelSettings {
    pub model_id: ModelId,
    pub effort: Effort,
    pub max_output_tokens: NonZeroU32,
}

pub trait ModelClient<O> {
    fn route(&self) -> String;
    fn invoke(
        &self,
        input: ModelInput<'_, O>,
        recorder: &mut dyn BeforeModelSend,
    ) -> Result<Observation<O>, SessionStoreError>;
}

pub struct ModelInput<'a, O> {
    pub context: &'a Context<O>,
    pub settings: &'a ModelSettings,
    pub selection: &'a ContextSelection,
    pub view: Option<&'a TurnView>,
    pub native: &'a [NativeStep],
}
#[derive(Clone, Debug)]
pub struct NativeStep {
    pub response: ModelArtifact,
    pub results: Vec<crate::domain::ToolResult>,
}
pub trait BeforeModelSend {
    fn persist(&mut self, prepared: PreparedInvocation) -> Result<SendPermit, SessionStoreError>;
}
pub struct SendPermit {
    pub(crate) _revision: u64,
}
#[derive(Clone, Debug)]
pub struct PreparedInvocation {
    pub request: RequestEvidence,
    pub view: Option<TurnView>,
}
#[derive(Clone, Debug)]
pub enum Generation<O> {
    Complete { output: O },
    Refused { reason: Option<NonEmptyText> },
    Incomplete { reason: IncompleteReason },
    Failed(ModelError),
}
#[derive(Clone, Debug)]
pub struct Observation<O> {
    pub generation: Generation<O>,
    pub evidence: ResponseEvidence,
}
impl<O> Observation<O> {
    pub fn failed(error: ModelError) -> Self {
        Self {
            generation: Generation::Failed(error),
            evidence: ResponseEvidence::not_returned(),
        }
    }
}
#[derive(Clone, Copy, Debug, PartialEq, Eq, Serialize, Deserialize)]
pub enum IncompleteReason {
    OutputLimit,
    Other,
}
#[derive(Clone, Copy, Debug, PartialEq, Eq, Serialize, Deserialize)]
pub enum ModelErrorKind {
    InvalidConfig,
    InvalidSettings,
    UnsupportedInput,
    MissingNativeHistory,
    NativeHistoryIncompatible,
    NativeIdCollision,
    ContextSelectionTooLarge,
    InvalidSelection,
    Transport,
    Timeout,
    Http,
    Authentication,
    RateLimited,
    ProviderFailure,
    ResponseTooLarge,
    MalformedResponse,
    UnsupportedResponse,
}
/// Safe diagnostics with no raw provider data, URL, credential, body, or error source.
#[derive(Clone, Debug, PartialEq, Eq, Serialize, Deserialize)]
pub struct ModelError {
    kind: ModelErrorKind,
    http_status: Option<u16>,
}
impl ModelError {
    pub fn new(kind: ModelErrorKind) -> Self {
        Self {
            kind,
            http_status: None,
        }
    }
    pub(crate) fn http(kind: ModelErrorKind, status: u16) -> Self {
        Self {
            kind,
            http_status: Some(status),
        }
    }
    pub fn http_status(&self) -> Option<u16> {
        self.http_status
    }
    pub fn kind(&self) -> ModelErrorKind {
        self.kind
    }
}
impl fmt::Display for ModelError {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        write!(f, "model client {:?}", self.kind)?;
        if let Some(status) = self.http_status {
            write!(f, " (HTTP {status})")?;
        }
        Ok(())
    }
}
impl Error for ModelError {}
