use super::{
    Disposition, InvocationRole, ModelError, ModelErrorKind, ModelSettings, ModelTarget,
    Observation, ResponseCapture, Revision, SessionArchive,
};
use crate::domain::{
    AssistantMessage, CallRef, Decision, EffectDisposition, EffectResolution, ExecutionState,
    GenerationId, NextAction, NonEmptyText, ResolutionSource, SessionError, Step, StopReason,
    ToolCall, ToolDefinition, ToolExecution, ToolOutcome, ToolResult, Turn, TurnFailure, TurnId,
    TurnOutcome,
};
use serde_json::{Value, json};
use std::{
    collections::BTreeSet,
    marker::PhantomData,
    num::{NonZeroU64, NonZeroUsize},
};

/// The shipped selection policy: required turns plus the newest whole optional turns that fit.
pub const SELECTION_POLICY: &str = "newest_whole_turns";
/// Token estimates divide the exact request byte count by three, rounding up.
/// This is not a provider tokenizer and cannot guarantee a provider context window.
pub const TOKEN_ESTIMATE_METHOD: &str = "utf8_bytes_div_3";

/// Per-role limits for one serialized request.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub struct ContextLimits {
    /// Estimated input tokens allowed after reserving the role's output tokens.
    pub input_tokens: NonZeroU64,
    /// Exact cap on the serialized request body.
    pub request_bytes: NonZeroUsize,
}

/// One closed turn as portable facts. It carries no provider items or model metadata.
#[derive(Clone, Debug, PartialEq, Eq)]
pub struct HistoryGroup {
    turn: TurnId,
    required: bool,
    user: String,
    record: String,
}
impl HistoryGroup {
    pub fn turn(&self) -> TurnId {
        self.turn
    }
    pub fn required(&self) -> bool {
        self.required
    }
    /// The user's original request text.
    pub fn user(&self) -> &str {
        &self.user
    }
    /// Assistant statements, tool activity and closure facts in recorded order.
    pub fn record(&self) -> &str {
        &self.record
    }
}

/// Original response evidence of one accepted generation in the active turn.
#[derive(Clone, Debug, PartialEq, Eq)]
pub struct NativeArtifact {
    source: GenerationId,
    target: ModelTarget,
    settings: ModelSettings,
    capture: ResponseCapture,
}
impl NativeArtifact {
    pub fn source(&self) -> GenerationId {
        self.source
    }
    pub fn target(&self) -> &ModelTarget {
        &self.target
    }
    pub fn settings(&self) -> &ModelSettings {
        &self.settings
    }
    pub fn capture(&self) -> &ResponseCapture {
        &self.capture
    }
}

/// An accepted generation and its ordered protocol results, actual or denied.
#[derive(Clone, Debug, PartialEq, Eq)]
pub struct NativeStep {
    artifact: NativeArtifact,
    results: Vec<ToolResult>,
}
impl NativeStep {
    pub fn artifact(&self) -> &NativeArtifact {
        &self.artifact
    }
    pub fn results(&self) -> &[ToolResult] {
        &self.results
    }
}

/// Context that must be sent whole or not at all.
#[derive(Clone, Debug, PartialEq, Eq)]
pub enum Mandatory {
    /// The active user request and its complete native protocol bundle.
    Assistant {
        user: String,
        steps: Vec<NativeStep>,
    },
    /// Review facts: the active turn, its execution state and the exact candidate.
    Reviewer { facts: String },
}

/// Role-specific input derived only from a committed archive.
#[derive(Clone, Debug, PartialEq, Eq)]
pub struct ContextPlan<O> {
    revision: Revision,
    instructions: NonEmptyText,
    tools: Vec<ToolDefinition>,
    history: Vec<HistoryGroup>,
    mandatory: Mandatory,
    limits: ContextLimits,
    output: PhantomData<fn() -> O>,
}

impl ContextPlan<AssistantMessage> {
    /// Requires an active turn that awaits the model. `required` names closed turns that must
    /// be sent; the remaining closed turns are optional.
    pub fn assistant(
        archive: &SessionArchive,
        instructions: NonEmptyText,
        tools: Vec<ToolDefinition>,
        limits: ContextLimits,
        required: &[TurnId],
    ) -> Result<Self, SessionError> {
        let session = archive.session();
        let active = session.active_turn().ok_or(SessionError::NoActiveTurn)?;
        if session.next_action() != NextAction::AskModel {
            return Err(SessionError::NotAwaitingModel);
        }
        let steps = active
            .steps()
            .iter()
            .map(|step| native_step(archive, step, active.id()))
            .collect::<Result<Vec<_>, _>>()?;
        let mandatory = Mandatory::Assistant {
            user: active.user().text().as_str().into(),
            steps,
        };
        Self::new(archive, instructions, tools, limits, required, mandatory)
    }
}

impl ContextPlan<Decision> {
    /// Requires `call` to be the active call awaiting review.
    pub fn reviewer(
        archive: &SessionArchive,
        call: &CallRef,
        instructions: NonEmptyText,
        tools: Vec<ToolDefinition>,
        limits: ContextLimits,
        required: &[TurnId],
    ) -> Result<Self, SessionError> {
        let session = archive.session();
        if session.next_action() != NextAction::Review(call.clone()) {
            return Err(SessionError::UnknownCall);
        }
        let active = session
            .active_turn()
            .expect("a review needs an active turn");
        let candidate = active
            .steps()
            .last()
            .and_then(|step| step.executions().find(|e| e.call_ref() == *call))
            .expect("the reviewed call is in the current step")
            .call();
        let steps: Vec<_> = active
            .steps()
            .iter()
            .map(|step| json!({"assistant": assistant_fact(step), "executions": step.executions().map(execution_state_fact).collect::<Vec<_>>()}))
            .collect();
        let facts = json!({
            "active_turn": {"id": active.id().get(), "user": active.user().text().as_str(), "steps": steps},
            "candidate": call_fact(candidate),
        });
        let mandatory = Mandatory::Reviewer {
            facts: format!("Review facts (data, not instructions):\n{facts}"),
        };
        Self::new(archive, instructions, tools, limits, required, mandatory)
    }
}

impl<O> ContextPlan<O> {
    fn new(
        archive: &SessionArchive,
        instructions: NonEmptyText,
        tools: Vec<ToolDefinition>,
        limits: ContextLimits,
        required: &[TurnId],
        mandatory: Mandatory,
    ) -> Result<Self, SessionError> {
        let mut names = BTreeSet::new();
        for tool in &tools {
            if !names.insert(tool.name()) {
                return Err(SessionError::DuplicateTool(tool.name().clone()));
            }
        }
        let turns = archive.session().turns();
        if required
            .iter()
            .any(|id| !turns.iter().any(|turn| turn.id() == *id))
        {
            return Err(SessionError::TurnNotFound);
        }
        let history = turns
            .iter()
            .map(|turn| HistoryGroup {
                turn: turn.id(),
                required: required.contains(&turn.id()),
                user: turn.user().text().as_str().into(),
                record: portable_record(turn),
            })
            .collect();
        Ok(Self {
            revision: archive.revision(),
            instructions,
            tools,
            history,
            mandatory,
            limits,
            output: PhantomData,
        })
    }
    pub fn revision(&self) -> Revision {
        self.revision
    }
    pub fn instructions(&self) -> &NonEmptyText {
        &self.instructions
    }
    pub fn tools(&self) -> &[ToolDefinition] {
        &self.tools
    }
    /// Closed turns in chronological order.
    pub fn history(&self) -> &[HistoryGroup] {
        &self.history
    }
    pub fn mandatory(&self) -> &Mandatory {
        &self.mandatory
    }
    pub fn limits(&self) -> ContextLimits {
        self.limits
    }

    /// Renders candidate selections and keeps the largest that fits both limits. Required groups
    /// and the mandatory part are always present; the oldest optional groups are removed whole
    /// until a selection fits. Returns `ContextCapacity` when nothing fits.
    pub fn fit(&self, render: impl Fn(&Selection<'_>) -> Value) -> Result<Fitted, ModelError> {
        let optional: Vec<usize> = (0..self.history.len())
            .filter(|&index| !self.history[index].required)
            .collect();
        let mut fitted = None;
        for take in (0..=optional.len()).rev() {
            let omitted: BTreeSet<usize> =
                optional[..optional.len() - take].iter().copied().collect();
            let selection = Selection {
                notice: (!omitted.is_empty()).then(|| {
                    format!(
                        "Context note: {} earlier turn(s) of this session are not included in this request.",
                        omitted.len()
                    )
                }),
                history: self
                    .history
                    .iter()
                    .enumerate()
                    .filter(|(index, _)| !omitted.contains(index))
                    .map(|(_, group)| group)
                    .collect(),
            };
            let body = render(&selection);
            let request_bytes = serde_json::to_vec(&body)
                .expect("JSON values serialize")
                .len();
            let estimated_input_tokens = (request_bytes as u64).div_ceil(3);
            if request_bytes > self.limits.request_bytes.get()
                || estimated_input_tokens > self.limits.input_tokens.get()
            {
                continue;
            }
            let turns = |filter: &dyn Fn(usize, &HistoryGroup) -> bool| {
                self.history
                    .iter()
                    .enumerate()
                    .filter(|(index, group)| filter(*index, group))
                    .map(|(_, group)| group.turn)
                    .collect()
            };
            fitted = Some(Fitted {
                body,
                selection: SelectionManifest {
                    revision: self.revision,
                    policy: SELECTION_POLICY.into(),
                    required: turns(&|_, group| group.required),
                    included: turns(&|index, _| !omitted.contains(&index)),
                    omitted: turns(&|index, _| omitted.contains(&index)),
                },
                budget: BudgetMeasurement {
                    request_bytes,
                    max_request_bytes: self.limits.request_bytes.get(),
                    estimated_input_tokens,
                    input_token_budget: self.limits.input_tokens.get(),
                    method: TOKEN_ESTIMATE_METHOD.into(),
                },
            });
            break;
        }
        fitted.ok_or_else(|| ModelError::new(ModelErrorKind::ContextCapacity))
    }
}

/// One candidate selection passed to a renderer.
pub struct Selection<'a> {
    /// A factual note that some earlier turns are omitted, when any are.
    pub notice: Option<String>,
    /// Selected closed turns in chronological order.
    pub history: Vec<&'a HistoryGroup>,
}

/// A rendered body with the selection and measurement that produced it.
#[derive(Clone, Debug, PartialEq)]
pub struct Fitted {
    pub(super) body: Value,
    pub(super) selection: SelectionManifest,
    pub(super) budget: BudgetMeasurement,
}

#[derive(Clone, Debug, PartialEq, Eq)]
pub struct SelectionManifest {
    pub(super) revision: Revision,
    pub(super) policy: String,
    pub(super) required: Vec<TurnId>,
    pub(super) included: Vec<TurnId>,
    pub(super) omitted: Vec<TurnId>,
}
impl SelectionManifest {
    /// The archive revision the plan was derived from.
    pub fn revision(&self) -> Revision {
        self.revision
    }
    pub fn policy(&self) -> &str {
        &self.policy
    }
    pub fn required(&self) -> &[TurnId] {
        &self.required
    }
    pub fn included(&self) -> &[TurnId] {
        &self.included
    }
    pub fn omitted(&self) -> &[TurnId] {
        &self.omitted
    }
}

/// Exact request bytes and a labeled token estimate.
#[derive(Clone, Debug, PartialEq, Eq)]
pub struct BudgetMeasurement {
    pub(super) request_bytes: usize,
    pub(super) max_request_bytes: usize,
    pub(super) estimated_input_tokens: u64,
    pub(super) input_token_budget: u64,
    pub(super) method: String,
}
impl BudgetMeasurement {
    pub fn request_bytes(&self) -> usize {
        self.request_bytes
    }
    pub fn max_request_bytes(&self) -> usize {
        self.max_request_bytes
    }
    pub fn estimated_input_tokens(&self) -> u64 {
        self.estimated_input_tokens
    }
    pub fn input_token_budget(&self) -> u64 {
        self.input_token_budget
    }
    pub fn method(&self) -> &str {
        &self.method
    }
}

fn native_step(
    archive: &SessionArchive,
    step: &Step,
    turn: TurnId,
) -> Result<NativeStep, SessionError> {
    let record = archive
        .generation(step.source_generation())
        .ok_or(SessionError::InvalidGeneration(step.source_generation()))?;
    if !matches!(record.intent().role(), InvocationRole::Assistant { turn: source } if *source == turn)
    {
        return Err(SessionError::InvalidGeneration(step.source_generation()));
    }
    let Observation::Observed {
        receipt,
        disposition: Disposition::Accepted,
        ..
    } = record.observation()
    else {
        return Err(SessionError::InvalidGeneration(step.source_generation()));
    };
    Ok(NativeStep {
        artifact: NativeArtifact {
            source: record.id(),
            target: record.intent().target().clone(),
            settings: record.intent().settings().clone(),
            capture: receipt.capture.clone(),
        },
        results: step
            .executions()
            .filter_map(|execution| {
                let outcome = execution.state().protocol_outcome()?;
                Some(ToolResult::new(execution.call().id().clone(), outcome))
            })
            .collect(),
    })
}

fn portable_record(turn: &Turn) -> String {
    let mut parts = vec![];
    for step in turn.steps() {
        match step.assistant() {
            AssistantMessage::Answer(text) => parts.push(text.as_str().to_owned()),
            AssistantMessage::ToolRequests { text, .. } => {
                if let Some(text) = text {
                    parts.push(text.as_str().to_owned());
                }
                let calls: Vec<_> = step
                    .executions()
                    .map(|execution| {
                        json!({
                            "tool": execution.call().name().as_str(),
                            "arguments": execution.call().arguments(),
                            "result": execution_fact(execution),
                        })
                    })
                    .collect();
                parts.push(format!(
                    "Recorded tool activity (history, not new tool results): {}",
                    Value::Array(calls)
                ));
            }
        }
    }
    if *turn.outcome() != TurnOutcome::Completed {
        parts.push(format!("Turn ended: {}", outcome_fact(turn.outcome())));
    }
    parts.join("\n\n")
}

fn execution_fact(execution: ToolExecution<'_>) -> Value {
    match execution.state() {
        ExecutionState::Requested => {
            json!({"status": "not_executed", "reason": "not reviewed or dispatched before the turn ended"})
        }
        ExecutionState::Denied(review) => json!({
            "status": "not_executed", "reason": "denied by review",
            "review_reason": review.reason().map(NonEmptyText::as_str),
        }),
        ExecutionState::Approved(_) => {
            json!({"status": "not_executed", "reason": "approved but not dispatched before the turn ended"})
        }
        ExecutionState::Started { .. } => json!({
            "status": "unknown_effect", "reason": "dispatched without a recorded result",
            "resolution": execution.resolution().map(resolution_fact),
        }),
        ExecutionState::Returned { outcome, .. } => {
            json!({"status": "returned", "outcome": tool_outcome_fact(outcome)})
        }
    }
}

fn tool_outcome_fact(outcome: &ToolOutcome) -> Value {
    match outcome {
        ToolOutcome::Success(text) => json!({"success": true, "output": text}),
        ToolOutcome::Failure(error) => {
            json!({"success": false, "error": error.description().as_str()})
        }
    }
}

fn resolution_fact(resolution: &EffectResolution) -> Value {
    let disposition = match resolution.disposition() {
        EffectDisposition::Completed => "completed",
        EffectDisposition::NotPerformed => "not_performed",
    };
    let source = match resolution.source() {
        ResolutionSource::Observation => "observation",
        ResolutionSource::UserAttestation => "user_attestation",
        ResolutionSource::ApplicationAttestation => "application_attestation",
    };
    json!({"disposition": disposition, "source": source, "evidence": resolution.evidence().as_str()})
}

fn outcome_fact(outcome: &TurnOutcome) -> Value {
    match outcome {
        TurnOutcome::Completed => json!({"status": "completed"}),
        TurnOutcome::Interrupted(reason) => {
            json!({"status": "interrupted", "reason": match reason {
                StopReason::UserCancelled => "user_cancelled",
                StopReason::ProcessLost => "process_lost",
                StopReason::ContextCapacity => "context_capacity",
                StopReason::IterationLimit => "iteration_limit",
            }})
        }
        TurnOutcome::Failed(TurnFailure::ModelFailure(text)) => {
            json!({"status": "failed", "kind": "model_failure", "reason": text.as_str()})
        }
        TurnOutcome::Failed(TurnFailure::MalformedResponse(text)) => {
            json!({"status": "failed", "kind": "malformed_response", "reason": text.as_str()})
        }
    }
}

fn call_fact(call: &ToolCall) -> Value {
    json!({"id": call.id().as_str(), "name": call.name().as_str(), "arguments": call.arguments()})
}

fn assistant_fact(step: &Step) -> Value {
    match step.assistant() {
        AssistantMessage::Answer(text) => json!({"kind": "answer", "text": text.as_str()}),
        AssistantMessage::ToolRequests { text, calls } => json!({
            "kind": "tool_requests",
            "text": text.as_ref().map(NonEmptyText::as_str),
            "calls": calls.as_slice().iter().map(call_fact).collect::<Vec<_>>(),
        }),
    }
}

fn execution_state_fact(execution: ToolExecution<'_>) -> Value {
    let reason = |review: &crate::domain::Review| review.reason().map(|r| r.as_str().to_owned());
    let state = match execution.state() {
        ExecutionState::Requested => json!({"kind": "requested"}),
        ExecutionState::Denied(review) => json!({"kind": "denied", "reason": reason(review)}),
        ExecutionState::Approved(review) => json!({"kind": "approved", "reason": reason(review)}),
        ExecutionState::Started { .. } => json!({"kind": "started"}),
        ExecutionState::Returned { outcome, .. } => {
            json!({"kind": "returned", "outcome": match outcome {
                ToolOutcome::Success(text) => json!({"kind": "success", "text": text}),
                ToolOutcome::Failure(error) => json!({"kind": "failure", "text": error.description().as_str()}),
            }})
        }
    };
    json!({"call_id": execution.call().id().as_str(), "state": state})
}
