use crate::domain::{
    ContextPlan, IncompleteReason, ModelError, ModelSettings, NonEmptyText, PreparedRequest,
    ResponseReceipt, Timestamp,
};

/// A synchronous, object-safe port. `prepare` is pure and never touches the network, so a
/// caller can commit the exact request before `invoke` sends it.
///
/// ```
/// use cola::{app::*, domain::*};
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
/// use cola::{app::*, domain::*};
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
