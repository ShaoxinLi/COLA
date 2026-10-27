use super::{anthropic, openai};
use crate::{
    app::{
        Continuation, Effort, Generation, IncompleteReason, ModelError, ModelErrorKind,
        ModelSettings,
    },
    domain::{
        AssistantMessage, Context, ContextExchange, Decision, EffectDisposition, ExecutionNotice,
        NonEmptyText, ResolutionSource, StopReason, ToolCall, ToolCalls, ToolDefinition,
        ToolOutcome, TurnFailure, TurnOutcome,
    },
};
use reqwest::{
    Url,
    blocking::Client,
    header::{HeaderMap, HeaderValue},
};
use serde::Deserialize;
use serde_json::{Value, json};
use std::{fmt, io::Read, num::NonZeroUsize, time::Duration};

#[derive(Clone, Copy, PartialEq, Eq)]
pub(super) enum Provider {
    OpenAI,
    Anthropic,
}
#[derive(Clone, Copy, PartialEq, Eq)]
pub(super) enum OutputKind {
    Assistant,
    Decision,
}
pub(super) fn malformed() -> ModelError {
    ModelError::new(ModelErrorKind::MalformedResponse)
}
pub(super) fn unsupported() -> ModelError {
    ModelError::new(ModelErrorKind::UnsupportedResponse)
}
pub(super) fn string<'a>(value: &'a Value, key: &str) -> Result<&'a str, ModelError> {
    value.get(key).and_then(Value::as_str).ok_or_else(malformed)
}
pub(super) fn array<'a>(value: &'a Value, key: &str) -> Result<&'a Vec<Value>, ModelError> {
    value
        .get(key)
        .and_then(Value::as_array)
        .ok_or_else(malformed)
}

pub(super) enum Decoded {
    Complete {
        text: String,
        calls: Vec<WireCall>,
        raw: Vec<Value>,
    },
    Refused,
    Incomplete(IncompleteReason),
}
pub(super) struct WireCall {
    pub(super) name: String,
    pub(super) arguments: Value,
    pub(super) id: String,
}

pub(super) enum Event<'a> {
    User(String),
    Answer(&'a NonEmptyText),
    Native(&'a [Value]),
    Result(&'a str, &'a crate::domain::ToolResult),
    Fact(String),
}

#[derive(Clone)]
pub(super) struct Engine {
    provider: Provider,
    transport: Transport,
}
impl Engine {
    pub(super) fn new(provider: Provider, config: ClientConfig) -> Result<Self, ModelError> {
        Ok(Self {
            provider,
            transport: Transport::new(config)?,
        })
    }
    pub(super) fn generate<O: Output>(
        &self,
        context: &Context<O>,
        settings: &ModelSettings,
        continuation: Option<&Continuation<O>>,
    ) -> Result<Generation<O>, ModelError> {
        let effort = match settings.effort {
            Effort::Default => None,
            Effort::Off => Some("none"),
            Effort::Low => Some("low"),
            Effort::Medium => Some("medium"),
            Effort::High => Some("high"),
            Effort::Xhigh => Some("xhigh"),
            Effort::Max => Some("max"),
        };
        let endpoint = self.transport.endpoint(self.provider);
        let snapshot = Snapshot::of(context);
        let old = replay::resume(
            continuation,
            self.provider,
            endpoint.as_str(),
            &settings.model_id,
            &snapshot,
        )?;
        let events = replay::events(O::KIND, &snapshot, old)?;
        let body = match self.provider {
            Provider::OpenAI => openai::request(&snapshot, settings, effort, O::KIND, events),
            Provider::Anthropic => anthropic::request(&snapshot, settings, effort, O::KIND, events),
        };
        let value = self
            .transport
            .send(self.provider, endpoint.clone(), &body)?;
        let decoded = match self.provider {
            Provider::OpenAI => openai::decode(value)?,
            Provider::Anthropic => anthropic::decode(value)?,
        };
        replay::complete(
            old,
            self.provider,
            endpoint.as_str(),
            &settings.model_id,
            snapshot,
            decoded,
        )
    }
}

/// Explicit HTTP connection settings with no environment credentials.
/// Each adapter appends its native route to the configured base path.
/// Redirects, retries, and environment proxies are disabled. Keys are sent in headers.
#[derive(Clone)]
pub struct ClientConfig {
    base: Url,
    key: HeaderValue,
    timeout: Duration,
    limit: NonZeroUsize,
}
impl fmt::Debug for ClientConfig {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        f.write_str("ClientConfig([redacted])")
    }
}
impl ClientConfig {
    pub fn new(
        base_url: &str,
        api_key: &str,
        timeout: Duration,
        max_response_bytes: NonZeroUsize,
    ) -> Result<Self, ModelError> {
        let base =
            Url::parse(base_url).map_err(|_| ModelError::new(ModelErrorKind::InvalidConfig))?;
        let has_userinfo = base_url.split_once("://").is_some_and(|(_, rest)| {
            rest.split(['/', '?', '#'])
                .next()
                .is_some_and(|authority| authority.contains('@'))
        });
        if has_userinfo
            || !matches!(base.scheme(), "http" | "https")
            || base.host_str().is_none()
            || !base.username().is_empty()
            || base.password().is_some()
            || base.query().is_some()
            || base.fragment().is_some()
            || timeout.is_zero()
            || timeout.as_millis() > u64::MAX as u128
            || api_key.trim().is_empty()
            || max_response_bytes.get() == usize::MAX
        {
            return Err(ModelError::new(ModelErrorKind::InvalidConfig));
        }
        let mut key = HeaderValue::from_str(api_key)
            .map_err(|_| ModelError::new(ModelErrorKind::InvalidConfig))?;
        key.set_sensitive(true);
        Ok(Self {
            base,
            key,
            timeout,
            limit: max_response_bytes,
        })
    }
}
#[derive(Clone)]
struct Transport {
    config: ClientConfig,
    client: Client,
}
impl Transport {
    fn new(config: ClientConfig) -> Result<Self, ModelError> {
        let client = Client::builder()
            .timeout(config.timeout)
            .redirect(reqwest::redirect::Policy::none())
            .retry(reqwest::retry::never())
            .no_proxy()
            .build()
            .map_err(|_| ModelError::new(ModelErrorKind::InvalidConfig))?;
        Ok(Self { config, client })
    }
    fn endpoint(&self, provider: Provider) -> Url {
        let mut url = self.config.base.clone();
        let mut segments = url
            .path_segments_mut()
            .expect("validated hierarchical HTTP URL");
        segments.pop_if_empty();
        match provider {
            Provider::OpenAI => {
                segments.push("responses");
            }
            Provider::Anthropic => {
                segments.push("messages");
            }
        }
        drop(segments);
        url
    }
    fn send(&self, provider: Provider, endpoint: Url, body: &Value) -> Result<Value, ModelError> {
        let mut headers = HeaderMap::new();
        match provider {
            Provider::OpenAI => {
                let key = self
                    .config
                    .key
                    .to_str()
                    .map_err(|_| ModelError::new(ModelErrorKind::InvalidConfig))?;
                let mut bearer = HeaderValue::from_str(&format!("Bearer {key}"))
                    .map_err(|_| ModelError::new(ModelErrorKind::InvalidConfig))?;
                bearer.set_sensitive(true);
                headers.insert("authorization", bearer);
            }
            Provider::Anthropic => {
                headers.insert("x-api-key", self.config.key.clone());
                headers.insert("anthropic-version", HeaderValue::from_static("2026-10-21"));
            }
        }
        let response = self
            .client
            .post(endpoint)
            .headers(headers)
            .json(body)
            .send()
            .map_err(|e| {
                ModelError::new(if e.is_timeout() {
                    ModelErrorKind::Timeout
                } else {
                    ModelErrorKind::Transport
                })
            })?;
        if !response.status().is_success() {
            let status = response.status().as_u16();
            return Err(ModelError::http(
                match status {
                    400 => ModelErrorKind::InvalidSettings,
                    401 | 403 => ModelErrorKind::Authentication,
                    429 => ModelErrorKind::RateLimited,
                    500..=599 => ModelErrorKind::ProviderFailure,
                    _ => ModelErrorKind::Http,
                },
                status,
            ));
        }
        let max = self.config.limit.get();
        if response.content_length().is_some_and(|n| n > max as u64) {
            return Err(ModelError::new(ModelErrorKind::ResponseTooLarge));
        }
        let mut bytes = Vec::new();
        response
            .take(max as u64 + 1)
            .read_to_end(&mut bytes)
            .map_err(|e| {
                ModelError::new(
                    if e.kind() == std::io::ErrorKind::TimedOut
                        || e.get_ref()
                            .and_then(|source| source.downcast_ref::<reqwest::Error>())
                            .is_some_and(reqwest::Error::is_timeout)
                    {
                        ModelErrorKind::Timeout
                    } else {
                        ModelErrorKind::Transport
                    },
                )
            })?;
        if bytes.len() > max {
            return Err(ModelError::new(ModelErrorKind::ResponseTooLarge));
        }
        serde_json::from_slice(&bytes)
            .map_err(|_| ModelError::new(ModelErrorKind::MalformedResponse))
    }
}

#[derive(Clone, PartialEq, Eq)]
pub(super) struct Snapshot {
    pub instructions: NonEmptyText,
    pub exchanges: Vec<ContextExchange>,
    pub tools: Vec<ToolDefinition>,
}
impl Snapshot {
    pub fn of<O>(context: &Context<O>) -> Self {
        Self {
            instructions: context.instructions().clone(),
            exchanges: context.exchanges().to_vec(),
            tools: context.tools().to_vec(),
        }
    }
}
mod replay {
    use super::{
        Decoded, Event, Output, OutputKind, Provider, Snapshot, boundary, malformed, notices,
    };
    use crate::{
        app::{Continuation, Generation, ModelError, ModelErrorKind, ModelId},
        domain::{
            AssistantMessage, ContextExchange, ExecutionNotice, Message, ToolCall, ToolCallId,
            ToolName, TurnId, TurnOutcome,
        },
    };
    use serde_json::Value;

    #[derive(Clone)]
    struct NativeCall {
        call: ToolCall,
        id: String,
    }
    #[derive(Clone)]
    struct Record {
        turn: TurnId,
        index: usize,
        raw: Vec<Value>,
        calls: Vec<NativeCall>,
    }
    #[derive(Clone)]
    pub(super) struct Replay {
        provider: Provider,
        endpoint: String,
        kind: OutputKind,
        model_id: ModelId,
        snapshot: Snapshot,
        receipt: AssistantMessage,
        records: Vec<Record>,
    }
    impl Replay {
        fn check(
            &self,
            provider: Provider,
            endpoint: &str,
            kind: OutputKind,
            model_id: &ModelId,
            current: &Snapshot,
        ) -> Result<(), ModelError> {
            if self.provider != provider
                || self.endpoint != endpoint
                || self.kind != kind
                || &self.model_id != model_id
            {
                return Err(ModelError::new(ModelErrorKind::ContinuationMismatch));
            }
            if !self.accepts_exact_output_and_next_input(current) {
                return Err(ModelError::new(ModelErrorKind::ContinuationMismatch));
            }
            Ok(())
        }
        fn accepts_exact_output_and_next_input(&self, current: &Snapshot) -> bool {
            let output = &self.receipt;
            let old = &self.snapshot.exchanges;
            let Some(last) = old.last() else {
                return false;
            };
            if last.outcome().is_some()
                || current.exchanges.len() < old.len()
                || old[..old.len() - 1] != current.exchanges[..old.len() - 1]
            {
                return false;
            }
            let accepted = &current.exchanges[old.len() - 1];
            if accepted.turn_id() != last.turn_id()
                || !accepted.messages().starts_with(last.messages())
            {
                return false;
            }
            let suffix = &accepted.messages()[last.messages().len()..];
            if suffix.first() != Some(&Message::Assistant(output.clone())) {
                return false;
            }
            match output {
                AssistantMessage::Answer(_) => {
                    if suffix.len() != 1
                        || accepted.outcome() != Some(&TurnOutcome::Completed)
                        || accepted.notices() != last.notices()
                    {
                        return false;
                    }
                }
                AssistantMessage::ToolRequests { calls, .. } => {
                    if suffix.len() != calls.as_slice().len() + 1 {
                        return false;
                    }
                    for (message, call) in suffix[1..].iter().zip(calls.as_slice()) {
                        if !matches!(message, Message::Tool(result) if result.call_id() == call.id())
                        {
                            return false;
                        }
                    }
                    if accepted.outcome().is_none() {
                        if accepted.notices() != last.notices() {
                            return false;
                        }
                    } else if !has_exact_closure_notice(accepted) {
                        return false;
                    }
                }
            }
            let newly_admitted_turns = &current.exchanges[old.len()..];
            if !newly_admitted_turns.is_empty() && accepted.outcome().is_none() {
                return false;
            }
            let mut previous = accepted.turn_id();
            for (index, exchange) in newly_admitted_turns.iter().enumerate() {
                if previous.get().checked_add(1) != Some(exchange.turn_id().get())
                    || !matches!(exchange.messages(), [Message::User(_)])
                {
                    return false;
                }
                if exchange.outcome().is_none() {
                    if index + 1 != newly_admitted_turns.len() || !exchange.notices().is_empty() {
                        return false;
                    }
                } else if !has_exact_closure_notice(exchange) {
                    return false;
                }
                previous = exchange.turn_id();
            }
            true
        }
    }

    fn has_exact_closure_notice(exchange: &ContextExchange) -> bool {
        let Some(outcome @ (TurnOutcome::Interrupted(_) | TurnOutcome::Failed(_))) =
            exchange.outcome()
        else {
            return false;
        };
        matches!(exchange.notices(), [ExecutionNotice::TurnClosed { turn_id, outcome: notice_outcome }]
            if *turn_id == exchange.turn_id() && notice_outcome == outcome)
    }

    fn require_history(snapshot: &Snapshot, replay: Option<&Replay>) -> Result<(), ModelError> {
        if replay.is_none()
            && snapshot.exchanges.iter().any(|exchange| {
                exchange
                    .messages()
                    .iter()
                    .any(|message| matches!(message, Message::Assistant(a) if a.calls().is_some()))
            })
        {
            return Err(ModelError::new(ModelErrorKind::MissingContinuation));
        }
        Ok(())
    }

    pub(super) fn resume<'a, O: Output>(
        continuation: Option<&'a Continuation<O>>,
        provider: Provider,
        endpoint: &str,
        model_id: &ModelId,
        current: &Snapshot,
    ) -> Result<Option<&'a Replay>, ModelError> {
        let old = continuation
            .map(|c| {
                c.downcast::<Replay>()
                    .ok_or_else(|| ModelError::new(ModelErrorKind::ContinuationMismatch))
            })
            .transpose()?;
        if let Some(old) = old {
            old.check(provider, endpoint, O::KIND, model_id, current)?;
        }
        if O::KIND == OutputKind::Assistant {
            require_history(current, old)?;
            if current
                .exchanges
                .last()
                .is_none_or(|e| e.outcome().is_some())
            {
                return Err(ModelError::new(ModelErrorKind::UnsupportedInput));
            }
        }
        Ok(old)
    }

    pub(super) fn events<'a>(
        kind: OutputKind,
        snapshot: &'a Snapshot,
        previous: Option<&'a Replay>,
    ) -> Result<Vec<Event<'a>>, ModelError> {
        if kind == OutputKind::Decision {
            return Ok(Vec::new());
        }
        let records = previous.map_or(&[][..], |replay| replay.records.as_slice());
        let mut events = Vec::new();
        for exchange in &snapshot.exchanges {
            for (index, message) in exchange.messages().iter().enumerate() {
                match message {
                    Message::User(user) => events.push(Event::User(format!(
                        "Turn {}\n{}",
                        boundary(exchange),
                        user.text().as_str()
                    ))),
                    Message::Assistant(assistant) => {
                        if let Some(record) = records
                            .iter()
                            .find(|r| r.turn == exchange.turn_id() && r.index == index)
                        {
                            events.push(Event::Native(&record.raw));
                        } else if let AssistantMessage::Answer(text) = assistant {
                            events.push(Event::Answer(text));
                        } else {
                            return Err(ModelError::new(ModelErrorKind::MissingContinuation));
                        }
                    }
                    Message::Tool(result) => {
                        let call = records
                            .iter()
                            .find_map(|record| {
                                record
                                    .calls
                                    .iter()
                                    .find(|call| call.call.id() == result.call_id())
                            })
                            .ok_or_else(|| ModelError::new(ModelErrorKind::MissingContinuation))?;
                        events.push(Event::Result(&call.id, result));
                    }
                }
            }
            if let Some(fact) = notices(exchange.notices()) {
                events.push(Event::Fact(fact));
            }
        }
        Ok(events)
    }

    pub(super) fn complete<O: Output>(
        previous: Option<&Replay>,
        provider: Provider,
        endpoint: &str,
        model_id: &ModelId,
        snapshot: Snapshot,
        decoded: Decoded,
    ) -> Result<Generation<O>, ModelError> {
        let records = previous.map_or(&[][..], |replay| replay.records.as_slice());
        let (text, wire, raw) = match decoded {
            Decoded::Refused => return Ok(Generation::Refused { reason: None }),
            Decoded::Incomplete(reason) => return Ok(Generation::Incomplete { reason }),
            Decoded::Complete { text, calls, raw } => (text, calls, raw),
        };
        if O::KIND == OutputKind::Decision && !wire.is_empty() {
            return Err(malformed());
        }
        let nonce = uuid::Uuid::new_v4();
        let mut native = Vec::new();
        let mut ids: std::collections::BTreeSet<String> = records
            .iter()
            .flat_map(|record| &record.calls)
            .map(|call| call.id.clone())
            .collect();
        for (index, call) in wire.into_iter().enumerate() {
            if call.id.trim().is_empty() || !ids.insert(call.id.clone()) {
                return Err(malformed());
            }
            let name = ToolName::new(call.name).map_err(|_| malformed())?;
            if !snapshot.tools.iter().any(|tool| tool.name() == &name) {
                return Err(malformed());
            }
            let domain = ToolCall::new(
                ToolCallId::new(format!("{nonce}:{index}")).map_err(|_| malformed())?,
                name,
                call.arguments,
            )
            .map_err(|_| malformed())?;
            native.push(NativeCall {
                call: domain,
                id: call.id,
            });
        }
        let output = O::decode(text, native.iter().map(|c| c.call.clone()).collect())?;
        if O::KIND == OutputKind::Decision {
            return Ok(Generation::Complete {
                output,
                continuation: None,
            });
        }
        let receipt = output.receipt().ok_or_else(malformed)?;
        let mut records = records.to_vec();
        let last = snapshot.exchanges.last().ok_or_else(malformed)?;
        records.push(Record {
            turn: last.turn_id(),
            index: last.messages().len(),
            raw,
            calls: native,
        });
        let state = Replay {
            provider,
            endpoint: endpoint.to_owned(),
            kind: O::KIND,
            model_id: model_id.clone(),
            snapshot,
            receipt,
            records,
        };
        Ok(Generation::Complete {
            output,
            continuation: Some(Continuation::new(state)),
        })
    }
}

#[derive(Deserialize)]
#[serde(deny_unknown_fields)]
struct DecisionWire {
    should_allow: bool,
    reason: Value,
}

pub(super) trait Output: Sized {
    const KIND: OutputKind;
    fn decode(text: String, calls: Vec<ToolCall>) -> Result<Self, ModelError>;
    fn receipt(&self) -> Option<AssistantMessage>;
}
impl Output for AssistantMessage {
    const KIND: OutputKind = OutputKind::Assistant;
    fn decode(text: String, calls: Vec<ToolCall>) -> Result<Self, ModelError> {
        if calls.is_empty() {
            AssistantMessage::answer(text).map_err(|_| malformed())
        } else {
            Ok(AssistantMessage::tool_requests(
                Some(text),
                ToolCalls::new(calls).map_err(|_| malformed())?,
            ))
        }
    }
    fn receipt(&self) -> Option<AssistantMessage> {
        Some(self.clone())
    }
}
impl Output for Decision {
    const KIND: OutputKind = OutputKind::Decision;
    fn decode(text: String, _: Vec<ToolCall>) -> Result<Self, ModelError> {
        let value: DecisionWire = serde_json::from_str(&text).map_err(|_| malformed())?;
        let reason = match value.reason {
            Value::Null => None,
            Value::String(text) => Some(NonEmptyText::new(text).map_err(|_| malformed())?),
            _ => return Err(malformed()),
        };
        Ok(Decision::new(value.should_allow, reason))
    }
    fn receipt(&self) -> Option<AssistantMessage> {
        None
    }
}

pub(super) fn decision_schema() -> Value {
    json!({"type":"object","properties":{"should_allow":{"type":"boolean"},"reason":{"type":["string","null"]}},"required":["should_allow","reason"],"additionalProperties":false})
}
pub(super) fn instructions(snapshot: &Snapshot, decision: bool) -> String {
    let mut text = snapshot.instructions.as_str().to_owned();
    if decision && !snapshot.tools.is_empty() {
        text.push_str("\nReview tool catalog (descriptive only, not callable):\n");
        for tool in &snapshot.tools {
            text.push_str(&json!({"name":tool.name().as_str(),"description":tool.description().as_str(),"parameters":tool.input_schema().document()}).to_string());
            text.push('\n');
        }
    }
    text
}
pub(super) fn outcome(value: &ToolOutcome) -> Value {
    match value {
        ToolOutcome::Success(text) => json!({"success":true,"output":text}),
        ToolOutcome::Failure(error) => {
            json!({"success":false,"error":error.description().as_str()})
        }
    }
}
fn turn_outcome(value: &TurnOutcome) -> Value {
    match value {
        TurnOutcome::Completed => json!({"status":"completed"}),
        TurnOutcome::Interrupted(reason) => {
            json!({"status":"interrupted","reason":match reason { StopReason::UserCancelled=>"user_cancelled", StopReason::ProcessLost=>"process_lost", StopReason::ContextCapacity=>"context_capacity", StopReason::IterationLimit=>"iteration_limit" }})
        }
        TurnOutcome::Failed(reason) => match reason {
            TurnFailure::ModelFailure(text) => {
                json!({"status":"failed","kind":"model_failure","reason":text.as_str()})
            }
            TurnFailure::MalformedResponse(text) => {
                json!({"status":"failed","kind":"malformed_response","reason":text.as_str()})
            }
        },
    }
}
fn resolution(value: &crate::domain::EffectResolution) -> Value {
    let disposition = match value.disposition() {
        EffectDisposition::Completed => "completed",
        EffectDisposition::NotPerformed => "not_performed",
    };
    let source = match value.source() {
        ResolutionSource::Observation => "observation",
        ResolutionSource::UserAttestation => "user_attestation",
        ResolutionSource::ApplicationAttestation => "application_attestation",
    };
    json!({"disposition":disposition,"source":source,"evidence":value.evidence().as_str()})
}
fn notice(value: &ExecutionNotice) -> Value {
    match value {
        ExecutionNotice::TurnClosed { turn_id, outcome } => json!({
            "fact":"turn_closed", "turn":turn_id.get(), "outcome":turn_outcome(outcome)
        }),
        ExecutionNotice::Undispatched { turn_id, call } => json!({
            "fact":"undispatched", "turn":turn_id.get(), "call_id":call.id().as_str(),
            "name":call.name().as_str(), "arguments":call.arguments()
        }),
        ExecutionNotice::UnknownEffect {
            turn_id,
            call,
            resolution: proof,
        } => json!({
            "fact":"unknown_effect", "turn":turn_id.get(), "call_id":call.id().as_str(),
            "name":call.name().as_str(), "arguments":call.arguments(),
            "resolution":proof.as_ref().map(resolution)
        }),
        ExecutionNotice::ConfirmedResult {
            turn_id,
            call,
            result,
        } => json!({
            "fact":"confirmed_result", "turn":turn_id.get(), "call_id":call.id().as_str(),
            "name":call.name().as_str(), "arguments":call.arguments(), "result":outcome(result.outcome())
        }),
    }
}
pub(super) fn notices(notices: &[ExecutionNotice]) -> Option<String> {
    if notices.is_empty() {
        return None;
    }
    let facts: Vec<Value> = notices.iter().map(notice).collect();
    Some(format!(
        "Execution facts (not new tool results): {}",
        json!(facts)
    ))
}
pub(super) fn boundary(exchange: &crate::domain::ContextExchange) -> String {
    json!({"turn":exchange.turn_id().get(),"outcome":exchange.outcome().map(turn_outcome)})
        .to_string()
}
fn review_message(message: &crate::domain::Message) -> Value {
    use crate::domain::{AssistantMessage, Message};
    match message {
        Message::User(user) => json!({"role":"user","text":user.text().as_str()}),
        Message::Assistant(AssistantMessage::Answer(text)) => {
            json!({"role":"assistant","text":text.as_str()})
        }
        Message::Assistant(AssistantMessage::ToolRequests { text, calls }) => {
            let calls: Vec<Value> = calls.as_slice().iter().map(|call|json!({
                "id":call.id().as_str(), "name":call.name().as_str(), "arguments":call.arguments()
            })).collect();
            json!({"role":"assistant","text":text.as_ref().map(|t|t.as_str()),"requested_calls":calls})
        }
        Message::Tool(result) => json!({
            "role":"tool", "id":result.call_id().as_str(), "result":outcome(result.outcome())
        }),
    }
}
pub(super) fn review_history(snapshot: &Snapshot) -> Vec<String> {
    if snapshot.exchanges.is_empty() {
        return vec!["Return the decision requested by the review instructions.".into()];
    }
    snapshot
        .exchanges
        .iter()
        .map(|exchange| {
            let messages: Vec<Value> = exchange.messages().iter().map(review_message).collect();
            format!(
                "Review history, descriptive facts only. Turn {}\n{}\n{}",
                boundary(exchange),
                json!(messages),
                notices(exchange.notices()).unwrap_or_default()
            )
        })
        .collect()
}
