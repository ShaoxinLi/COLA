use super::{BudgetMeasurement, Fitted, SelectionManifest};
use serde_json::Value;
use std::{error::Error, fmt, marker::PhantomData, num::NonZeroU32};

/// A nonblank model identifier. Its contents are never included in diagnostics.
#[derive(Clone, PartialEq, Eq)]
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

#[derive(Clone, Copy, Debug, Default, PartialEq, Eq)]
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
#[derive(Clone, Debug, PartialEq, Eq)]
pub struct ModelSettings {
    pub model_id: ModelId,
    pub effort: Effort,
    pub max_output_tokens: NonZeroU32,
}

/// Credential-free identity of the protocol and endpoint a request is prepared for.
#[derive(Clone, Debug, PartialEq, Eq)]
pub struct ModelTarget {
    provider: String,
    endpoint: String,
}
impl ModelTarget {
    pub fn new(provider: impl Into<String>, endpoint: impl Into<String>) -> Self {
        Self {
            provider: provider.into(),
            endpoint: endpoint.into(),
        }
    }
    pub fn provider(&self) -> &str {
        &self.provider
    }
    pub fn endpoint(&self) -> &str {
        &self.endpoint
    }
}

/// The exact request a client will send, with its selection and measurement.
pub struct PreparedRequest<O> {
    target: ModelTarget,
    settings: ModelSettings,
    fitted: Fitted,
    output: PhantomData<fn() -> O>,
}
impl<O> PreparedRequest<O> {
    pub fn new(target: ModelTarget, settings: ModelSettings, fitted: Fitted) -> Self {
        Self {
            target,
            settings,
            fitted,
            output: PhantomData,
        }
    }
    pub fn target(&self) -> &ModelTarget {
        &self.target
    }
    pub fn settings(&self) -> &ModelSettings {
        &self.settings
    }
    pub fn body(&self) -> &Value {
        &self.fitted.body
    }
    pub fn selection(&self) -> &SelectionManifest {
        &self.fitted.selection
    }
    pub fn budget(&self) -> &BudgetMeasurement {
        &self.fitted.budget
    }
}
impl<O> fmt::Debug for PreparedRequest<O> {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        f.write_str("PreparedRequest([redacted])")
    }
}

#[derive(Clone, Copy, Debug, Default, PartialEq, Eq)]
pub struct Usage {
    pub input_tokens: Option<u64>,
    pub output_tokens: Option<u64>,
}

/// Bounded original response bytes. The archive never claims bytes that were not received.
#[derive(Clone, Debug, Default, PartialEq, Eq)]
pub enum ResponseCapture {
    #[default]
    NotReceived,
    Complete(Vec<u8>),
    Interrupted {
        prefix: Vec<u8>,
    },
    Truncated {
        prefix: Vec<u8>,
        limit: usize,
    },
}

/// Safe response metadata. Missing fields remain unknown rather than guessed.
#[derive(Clone, Debug, Default, PartialEq, Eq)]
pub struct ResponseReceipt {
    pub http_status: Option<u16>,
    pub served_model: Option<String>,
    pub response_id: Option<String>,
    pub request_id: Option<String>,
    pub finish: Option<String>,
    pub usage: Option<Usage>,
    pub capture: ResponseCapture,
}

#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum IncompleteReason {
    OutputLimit,
    Other,
}
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum ModelErrorKind {
    InvalidConfig,
    InvalidSettings,
    UnsupportedInput,
    ContextCapacity,
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
#[derive(Clone, Debug, PartialEq, Eq)]
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
    pub fn http(kind: ModelErrorKind, status: u16) -> Self {
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
