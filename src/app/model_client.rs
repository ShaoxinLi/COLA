use super::{BudgetMeasurement, ContextPlan, Fitted, SelectionManifest};
use crate::domain::{NonEmptyText, Timestamp};
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

/// A synchronous, object-safe port. `prepare` is pure and never touches the network, so a
/// caller can commit the exact request before `invoke` sends it.
///
/// ```
/// use cola::{app::*, domain::Decision};
/// use serde_json::json;
/// struct Fake;
/// impl ModelClient<Decision> for Fake {
///     fn prepare(&self, plan: &ContextPlan<Decision>, settings: &ModelSettings)
///         -> Result<PreparedRequest<Decision>, ModelError> {
///         let fitted = plan.fit(|_| json!({"fake": true}))?;
///         Ok(PreparedRequest::new(ModelTarget::new("fake", "memory"), settings.clone(), fitted))
///     }
///     fn invoke(&self, _: &PreparedRequest<Decision>) -> ObservedReply<Decision> {
///         let receipt = ResponseReceipt { capture: ResponseCapture::Complete(b"{}".to_vec()), ..ResponseReceipt::default() };
///         ObservedReply::new(receipt, Ok(Generation::Complete(Decision::new(true, None))))
///     }
/// }
/// let _: Box<dyn ModelClient<Decision>> = Box::new(Fake);
/// ```
///
/// ```compile_fail
/// use cola::{app::*, domain::{AssistantMessage, Decision}};
/// fn wrong_plan(client: &dyn ModelClient<Decision>, plan: &ContextPlan<AssistantMessage>, settings: &ModelSettings) {
///     client.prepare(plan, settings);
/// }
/// ```
///
/// ```compile_fail
/// use cola::{app::ModelClient, adapters::models::OpenAIClient};
/// fn unsupported_output(client: OpenAIClient) {
///     let _: Box<dyn ModelClient<String>> = Box::new(client);
/// }
/// ```
pub trait ModelClient<O> {
    /// Renders, fits and measures a credential-free request without network access.
    fn prepare(
        &self,
        plan: &ContextPlan<O>,
        settings: &ModelSettings,
    ) -> Result<PreparedRequest<O>, ModelError>;
    /// Sends exactly the prepared request and reports everything observed, including failures.
    fn invoke(&self, request: &PreparedRequest<O>) -> ObservedReply<O>;
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

pub struct ObservedReply<O> {
    pub at: Timestamp,
    pub receipt: ResponseReceipt,
    pub result: Result<Generation<O>, ModelError>,
}
impl<O> ObservedReply<O> {
    /// Stamps the observation with the current time.
    pub fn new(receipt: ResponseReceipt, result: Result<Generation<O>, ModelError>) -> Self {
        Self {
            at: Timestamp::now(),
            receipt,
            result,
        }
    }
}

#[derive(Debug)]
pub enum Generation<O> {
    Complete(O),
    Refused { reason: Option<NonEmptyText> },
    Incomplete { reason: IncompleteReason },
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
