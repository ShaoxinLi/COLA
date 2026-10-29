// Design-stage sketch. Canonical implemented contracts live in src/app and src/domain.
//! Architecture sketch for review. This file is not compiled into COLA.
//! All operations intentionally have unimplemented bodies.
#![allow(dead_code, unused_variables)]
use cola::{app::{ModelError, ModelSettings}, domain::{
    AssistantMessage, Context, Decision, EffectResolution, Session, SessionId,
    ToolCallId, ToolOutcome, TurnId, UserMessage,
}};
use std::num::NonZeroU32;

// Proposed domain values. Native IDs are unique within a batch, not a session.
// Across steps, this address owns identity. Provider request limits stay in adapters.
#[derive(Clone, Debug, PartialEq, Eq)]
pub struct CallAddress {
    pub session: SessionId,
    pub turn: TurnId,
    pub step: StepId,
    pub native: ToolCallId,
}
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub struct StepId(NonZeroU32);
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub struct InvocationId(u64);
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub struct Revision(u64);

// Application record. The store publishes this as one complete revision.
pub struct SessionDocument {
    revision: Revision,
    session: Session,
    invocations: Vec<Invocation>,
    views: Vec<TurnView>,
    failure_acknowledgements: Vec<ReviewFailureAcknowledgement>,
}
pub enum SessionGate {
    Ready,
    ReviewBlocked { call: CallAddress, invocation: InvocationId },
}
pub struct ReviewFailureAcknowledgement {
    invocation: InvocationId,
    evidence: cola::domain::NonEmptyText,
    acknowledged_at: String,
    actor: Option<cola::domain::NonEmptyText>,
}
pub enum InvocationTarget {
    Assistant { turn: TurnId, step: StepId },
    Reviewer { call: CallAddress },
}
pub struct Invocation {
    id: InvocationId,
    target: InvocationTarget,
    requested: RequestedModel,
    state: InvocationState,
}
pub struct RequestedModel {
    route: String, // Nonsecret configured route identity, never a credential/hash.
    settings: ModelSettings,
}
pub enum InvocationState {
    Planned,
    Prepared(RequestEvidence),
    Finished { request: Option<RequestEvidence>, observation: RecordedObservation },
    Interrupted { request: Option<RequestEvidence>, reason: InterruptedInvocation },
}
pub enum InterruptedInvocation { ProcessLostNotSent, ProcessLostResponseUnknown }
pub struct RequestEvidence {
    view: Option<TurnId>,
    selection: Vec<TurnId>,
    policy_version: u32,
    renderer_version: u32,
    body: ModelArtifact,
    started_at: String,
}
pub struct RecordedObservation {
    reported_model: Option<String>,
    provider_request_id: Option<String>,
    http_status: Option<u16>,
    usage: Option<ModelArtifact>,
    response: Option<ModelArtifact>,
    capture: CaptureCompleteness,
    outcome: RecordedOutcome,
    finished_at: String,
}
pub enum CaptureCompleteness { Complete, NotReturned, Partial { captured_bytes: usize } }
pub enum RecordedOutcome {
    AssistantAccepted { turn: TurnId, step: StepId },
    Review { call: CallAddress, decision: Decision },
    Refused,
    Incomplete,
    Failed(ModelError),
    PreparationFailed(PreparationFailure),
    OutputRejected { reason: String },
}

// Opaque app-owned artifact envelope, private provider codecs own its bytes.
// It archives model payloads, not HTTP/config/authentication objects.
pub struct ModelArtifact {
    format: String,
    version: u32,
    bytes: Vec<u8>,
}
pub struct TurnView {
    turn: TurnId,
    selected_closed_turns: Vec<TurnId>,
    mandatory_effects: Vec<CallAddress>,
    policy_version: u32,
    frozen_prefix: ModelArtifact,
    requested: RequestedModel,
}

pub trait ContextPolicy {
    fn select(&self, session: &Session) -> Result<ContextSelection, ContextFailure>;
}
pub struct ContextSelection {
    closed_turns: Vec<TurnId>,
    mandatory_closed_turns: Vec<TurnId>,
    max_request_bytes: usize,
}
impl ContextSelection {
    // Application policy chooses whole turns; the adapter supplies encoded size.
    fn fit(&self, encoded_size: impl FnMut(&[TurnId]) -> usize)
        -> Result<Vec<TurnId>, ContextFailure> { unimplemented!() }
}
pub enum ContextFailure { RequiredContextTooLarge, InvalidSelection }
pub struct ModelInput<'a, O> {
    context: &'a Context<O>,
    view: Option<&'a TurnView>,
    selection: &'a ContextSelection,
    active_native: &'a [NativeStep<'a>],
    settings: &'a ModelSettings,
}
pub struct NativeStep<'a> {
    step: StepId,
    response: &'a ModelArtifact,
    results: &'a [cola::domain::ToolResult],
}
pub enum GenerationOutcome<O> {
    Complete(O),
    Refused,
    Incomplete,
    Failed(ModelError),
    PreparationFailed(PreparationFailure),
}
pub enum PreparationFailure {
    Context(ContextFailure),
    MissingNativeHistory,
    NativeHistoryIncompatible,
    NativeIdCollision,
}
pub struct Observation<O> {
    result: GenerationOutcome<O>,
    reported_model: Option<String>,
    response: Option<ModelArtifact>,
    capture: CaptureCompleteness,
    usage: Option<ModelArtifact>,
    http_status: Option<u16>,
    provider_request_id: Option<String>,
    finished_at: String,
}

pub trait BeforeModelSend {
    fn persist(&mut self, prepared: PreparedInvocation) -> Result<SendPermit, CommitFailure>;
}
pub struct PreparedInvocation {
    request: RequestEvidence,
    view: ViewBinding,
}
pub enum ViewBinding { Create(TurnView), Reuse(TurnId), Reviewer }
// Constructible only by the application recorder after a confirmed commit.
pub struct SendPermit { invocation: InvocationId, revision: Revision }
pub trait ModelClient<O> {
    // Private compiler uses selection.fit(|ids| render(ids).encoded_body_len()).
    // First assistant request creates a frozen view inside PreparedInvocation.
    // The app commits that view and request together before issuing a permit.
    // Further assistant requests reuse it. Reviewer requests have no native view.
    fn invoke(&self, input: ModelInput<'_, O>, recorder: &mut dyn BeforeModelSend)
        -> Result<Observation<O>, InvocationHalt>;
}
pub enum InvocationHalt { Persistence(CommitFailure) }

pub trait SessionStore {
    fn list(&self) -> Result<Vec<SessionId>, StoreFailure>;
    fn create(&self) -> Result<Box<dyn SessionWriter + '_>, StoreFailure>;
    fn acquire(&self, id: &SessionId) -> Result<Box<dyn SessionWriter + '_>, StoreFailure>;
    fn inspect(&self, id: &SessionId) -> Result<SessionDocument, StoreFailure>;
}
pub trait SessionWriter {
    // Acquire syncs file+directory and reads exact state without implicit recovery.
    fn committed(&self) -> &SessionDocument;
    // The writer requires matching identity and next.revision == head + 1.
    fn commit(&mut self, next: SessionDocument) -> Result<Revision, CommitFailure>;
}
pub enum StoreFailure { Busy, NotFound, InvalidData, UnsupportedVersion, Storage }
pub enum CommitFailure { BeforePublish, PublishedDurabilityUncertain }

impl SessionDocument {
    pub(crate) fn gate(&self) -> SessionGate { unimplemented!() }
    pub(crate) fn begin_turn(&self, user: UserMessage) -> Result<Self, DocumentFailure> { unimplemented!() }
    pub(crate) fn plan(&self, target: InvocationTarget, requested: RequestedModel)
        -> Result<Self, DocumentFailure> { unimplemented!() }
    pub(crate) fn record_prepared(&self, id: InvocationId, prepared: PreparedInvocation)
        -> Result<Self, DocumentFailure> { unimplemented!() }
    // Expected output rejections return Ok(next) with audited OutputRejected.
    // Err is reserved for invalid application state, stale addresses or evidence.
    pub(crate) fn accept_assistant(&self, id: InvocationId, observed: Observation<AssistantMessage>)
        -> Result<Self, DocumentFailure> { unimplemented!() }
    pub(crate) fn accept_review(&self, id: InvocationId, observed: Observation<Decision>)
        -> Result<Self, DocumentFailure> { unimplemented!() }
    pub(crate) fn recover_process_loss(&self) -> Result<Self, DocumentFailure> { unimplemented!() }
    pub(crate) fn start_tool(&self, call: CallAddress) -> Result<Self, DocumentFailure> { unimplemented!() }
    pub(crate) fn record_tool_result(&self, call: CallAddress, outcome: ToolOutcome)
        -> Result<Self, DocumentFailure> { unimplemented!() }
    pub(crate) fn resolve_effect(&self, call: CallAddress, resolution: EffectResolution)
        -> Result<Self, DocumentFailure> { unimplemented!() }
    pub(crate) fn acknowledge_review_failure(&self, id: InvocationId, evidence: cola::domain::NonEmptyText,
        actor: Option<cola::domain::NonEmptyText>)
        -> Result<Self, DocumentFailure> { unimplemented!() }
}
pub enum DocumentFailure { WrongState, StaleAddress, InvalidEvidence }

// Consumer API stays focused on use cases. No continuation objects or wire bodies.
pub struct AgentService;
impl AgentService {
    pub fn new_session(&mut self) -> Result<Session, ServiceFailure> { unimplemented!() }
    pub fn chat(&mut self, id: &SessionId, user: UserMessage)
        -> Result<Vec<cola::domain::Message>, ServiceFailure> { unimplemented!() }
    pub fn resolve_effect(&mut self, call: CallAddress, resolution: EffectResolution)
        -> Result<(), ServiceFailure> { unimplemented!() }
    pub fn acknowledge_review_failure(&mut self, id: &SessionId, invocation: InvocationId,
        evidence: cola::domain::NonEmptyText, actor: Option<cola::domain::NonEmptyText>)
        -> Result<(), ServiceFailure> { unimplemented!() }
}
pub enum ServiceFailure { Store(StoreFailure), Context(ContextFailure), Failed }
