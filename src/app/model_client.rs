//! Blocking model invocation with typed output and caller-owned replay state.
use crate::domain::{Context, NonEmptyText};
use std::{any::Any, error::Error, fmt, marker::PhantomData, num::NonZeroU32, sync::Arc};

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

/// A synchronous, object-safe port. Fakes can return a complete output without a token.
///
/// ```
/// use cola::{app::{ModelClient, ModelSettings, Continuation, Generation, ModelError}, domain::{Context, Decision}};
/// struct Fake;
/// impl ModelClient<Decision> for Fake {
///     fn generate(&self, _: &Context<Decision>, _: &ModelSettings, _: Option<&Continuation<Decision>>)
///         -> Result<Generation<Decision>, ModelError> {
///         Ok(Generation::Complete { output: Decision::new(true, None), continuation: None })
///     }
/// }
/// let _: Box<dyn ModelClient<Decision>> = Box::new(Fake);
/// ```
///
/// ```compile_fail
/// use cola::{app::{ModelClient, ModelSettings}, domain::{Context, AssistantMessage, Decision}};
/// fn wrong_context(client: &dyn ModelClient<Decision>, context: &Context<AssistantMessage>, settings: &ModelSettings) {
///     client.generate(context, settings, None);
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
    fn generate(
        &self,
        context: &Context<O>,
        settings: &ModelSettings,
        continuation: Option<&Continuation<O>>,
    ) -> Result<Generation<O>, ModelError>;
}

/// Immutable, opaque native history. Publish it only after accepting the returned output.
/// It is not a persistence format and cannot be reconstructed from JSONL sessions.
///
/// ```compile_fail
/// use cola::{app::{Continuation, ModelClient, ModelSettings}, domain::{AssistantMessage, Context, Decision}};
/// fn mismatched(client: &dyn ModelClient<AssistantMessage>, context: &Context<AssistantMessage>,
///     settings: &ModelSettings, token: &Continuation<Decision>) {
///     client.generate(context, settings, Some(token));
/// }
/// ```
pub struct Continuation<O> {
    state: Arc<dyn Any + Send + Sync>,
    output: PhantomData<fn() -> O>,
}
impl<O> Continuation<O> {
    pub(crate) fn new<T: Any + Send + Sync>(state: T) -> Self {
        Self {
            state: Arc::new(state),
            output: PhantomData,
        }
    }
    pub(crate) fn downcast<T: Any>(&self) -> Option<&T> {
        self.state.downcast_ref()
    }
}
impl<O> Clone for Continuation<O> {
    fn clone(&self) -> Self {
        Self {
            state: Arc::clone(&self.state),
            output: PhantomData,
        }
    }
}
impl<O> fmt::Debug for Continuation<O> {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        f.write_str("Continuation([redacted])")
    }
}

#[derive(Debug)]
pub enum Generation<O> {
    Complete {
        output: O,
        continuation: Option<Continuation<O>>,
    },
    Refused {
        reason: Option<NonEmptyText>,
    },
    Blocked {
        reason: Option<NonEmptyText>,
    },
    Incomplete {
        reason: IncompleteReason,
    },
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
    UnsupportedModel,
    UnsupportedEffort,
    UnsupportedInput,
    MissingContinuation,
    ContinuationMismatch,
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
}
impl ModelError {
    pub fn new(kind: ModelErrorKind) -> Self {
        Self { kind }
    }
    pub fn kind(&self) -> ModelErrorKind {
        self.kind
    }
}
impl fmt::Display for ModelError {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        write!(f, "model client {:?}", self.kind)
    }
}
impl Error for ModelError {}
