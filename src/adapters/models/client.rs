use super::{anthropic, openai};
use crate::domain::{
    ContextPlan, Effort, IncompleteReason, Mandatory, ModelError, ModelErrorKind, ModelSettings,
    ModelTarget, PreparedRequest, ResponseCapture, ResponseReceipt, Usage,
};
use crate::{
    app::{Generation, ObservedReply},
    domain::{
        AssistantMessage, Decision, NonEmptyText, ToolCall, ToolCallId, ToolCalls, ToolName,
        ToolOutcome, ToolResult,
    },
};
use reqwest::{
    Url,
    blocking::Client,
    header::{HeaderMap, HeaderValue},
};
use serde::Deserialize;
use serde_json::{Value, json};
use std::{collections::BTreeSet, fmt, io::Read, num::NonZeroUsize, time::Duration};

#[derive(Clone, Copy, PartialEq, Eq)]
pub(super) enum Provider {
    OpenAI,
    Anthropic,
}
impl Provider {
    fn name(self) -> &'static str {
        match self {
            Self::OpenAI => "openai",
            Self::Anthropic => "anthropic",
        }
    }
    fn response_text(self, value: &Value) -> String {
        let mut text = String::new();
        match self {
            Self::OpenAI => {
                for item in value
                    .get("output")
                    .and_then(Value::as_array)
                    .into_iter()
                    .flatten()
                {
                    for block in item
                        .get("content")
                        .and_then(Value::as_array)
                        .into_iter()
                        .flatten()
                    {
                        if block.get("type").and_then(Value::as_str) == Some("output_text")
                            && let Some(part) = block.get("text").and_then(Value::as_str)
                        {
                            text.push_str(part);
                        }
                    }
                }
            }
            Self::Anthropic => {
                for block in value
                    .get("content")
                    .and_then(Value::as_array)
                    .into_iter()
                    .flatten()
                {
                    if block.get("type").and_then(Value::as_str) == Some("text")
                        && let Some(part) = block.get("text").and_then(Value::as_str)
                    {
                        text.push_str(part);
                    }
                }
            }
        }
        text
    }
    fn decode(self, value: Value) -> Result<Decoded, ModelError> {
        match self {
            Self::OpenAI => openai::decode(value),
            Self::Anthropic => anthropic::decode(value),
        }
    }
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
    pub name: String,
    pub arguments: Value,
    pub id: String,
}
pub(super) enum Event {
    User(String),
    Answer(String),
    Native(Vec<Value>),
    Result(ToolResult),
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
    fn target(&self) -> ModelTarget {
        ModelTarget::new(
            self.provider.name(),
            self.transport.endpoint(self.provider).as_str(),
        )
    }
    pub(super) fn prepare<O: Output>(
        &self,
        plan: &ContextPlan<O>,
        settings: &ModelSettings,
    ) -> Result<PreparedRequest<O>, ModelError> {
        let target = self.target();
        let mut mandatory = Vec::new();
        match plan.mandatory() {
            Mandatory::Assistant { user, steps } => {
                mandatory.push(Event::User(user.clone()));
                for step in steps {
                    let artifact = step.artifact();
                    if artifact.target() != &target
                        || artifact.settings().model_id != settings.model_id
                        || artifact.settings().effort != settings.effort
                    {
                        return Err(ModelError::new(ModelErrorKind::UnsupportedInput));
                    }
                    let ResponseCapture::Complete(bytes) = artifact.capture() else {
                        return Err(ModelError::new(ModelErrorKind::UnsupportedInput));
                    };
                    let value = serde_json::from_slice(bytes).map_err(|_| malformed())?;
                    let Decoded::Complete { raw, .. } = self.provider.decode(value)? else {
                        return Err(malformed());
                    };
                    mandatory.push(Event::Native(raw));
                    mandatory.extend(step.results().iter().cloned().map(Event::Result));
                }
            }
            Mandatory::Reviewer { facts } => mandatory.push(Event::User(facts.clone())),
        }
        let effort = match settings.effort {
            Effort::Default => None,
            Effort::Off => Some("none"),
            Effort::Low => Some("low"),
            Effort::Medium => Some("medium"),
            Effort::High => Some("high"),
            Effort::Xhigh => Some("xhigh"),
            Effort::Max => Some("max"),
        };
        let fitted = plan.fit(|selected| {
            let mut events = Vec::new();
            if let Some(notice) = &selected.notice {
                events.push(Event::User(notice.clone()));
            }
            for group in &selected.history {
                events.push(Event::User(group.user().to_owned()));
                events.push(Event::Answer(group.record().to_owned()));
            }
            let events = events.iter().chain(&mandatory);
            match self.provider {
                Provider::OpenAI => openai::request(plan, settings, effort, O::KIND, events),
                Provider::Anthropic => anthropic::request(plan, settings, effort, O::KIND, events),
            }
        })?;
        Ok(PreparedRequest::new(target, settings.clone(), fitted))
    }
    pub(super) fn invoke<O: Output>(&self, request: &PreparedRequest<O>) -> ObservedReply<O> {
        if request.target() != &self.target() {
            return ObservedReply::new(
                ResponseReceipt::default(),
                Err(ModelError::new(ModelErrorKind::UnsupportedInput)),
            );
        }
        let (mut receipt, response) = self.transport.send(self.provider, request.body());
        let result = response.and_then(|value| {
            metadata(&mut receipt, self.provider, &value);
            let decoded = self.provider.decode(value)?;
            let key = self.transport.config.key.to_str().expect("validated key");
            if let Decoded::Complete { text, calls, .. } = &decoded
                && (contains_text(text, key)
                    || calls.iter().any(|call| {
                        contains_text(&call.name, key)
                            || contains_text(&call.id, key)
                            || contains_credential(&call.arguments, key)
                    }))
            {
                if let ResponseCapture::Complete(bytes) = &mut receipt.capture {
                    bytes.fill(b'*');
                }
                return Err(malformed());
            }
            complete(request.body(), decoded, O::KIND)
        });
        self.transport.redact_receipt(&mut receipt);
        ObservedReply::new(receipt, result)
    }
}
fn complete<O: Output>(
    body: &Value,
    decoded: Decoded,
    kind: OutputKind,
) -> Result<Generation<O>, ModelError> {
    let (text, wire) = match decoded {
        Decoded::Refused => return Ok(Generation::Refused { reason: None }),
        Decoded::Incomplete(reason) => return Ok(Generation::Incomplete { reason }),
        Decoded::Complete { text, calls, .. } => (text, calls),
    };
    if kind == OutputKind::Decision && !wire.is_empty() {
        return Err(malformed());
    }
    let mut ids = BTreeSet::new();
    let mut calls = Vec::new();
    for call in wire {
        if !ids.insert(call.id.clone()) {
            return Err(malformed());
        }
        let name = ToolName::new(call.name).map_err(|_| malformed())?;
        if !body
            .get("tools")
            .and_then(Value::as_array)
            .is_some_and(|tools| {
                tools
                    .iter()
                    .any(|tool| tool["name"].as_str() == Some(name.as_str()))
            })
        {
            return Err(malformed());
        }
        calls.push(
            ToolCall::new(
                ToolCallId::new(call.id).map_err(|_| malformed())?,
                name,
                call.arguments,
            )
            .map_err(|_| malformed())?,
        );
    }
    Ok(Generation::Complete(O::decode(text, calls)?))
}
fn metadata(receipt: &mut ResponseReceipt, provider: Provider, value: &Value) {
    receipt.served_model = value
        .get("model")
        .and_then(Value::as_str)
        .map(str::to_owned);
    receipt.response_id = value.get("id").and_then(Value::as_str).map(str::to_owned);
    receipt.finish = value
        .get(match provider {
            Provider::OpenAI => "status",
            Provider::Anthropic => "stop_reason",
        })
        .and_then(Value::as_str)
        .map(str::to_owned);
    receipt.usage = value
        .get("usage")
        .and_then(Value::as_object)
        .map(|usage| Usage {
            input_tokens: usage.get("input_tokens").and_then(Value::as_u64),
            output_tokens: usage.get("output_tokens").and_then(Value::as_u64),
        });
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
        segments.push(match provider {
            Provider::OpenAI => "responses",
            Provider::Anthropic => "messages",
        });
        drop(segments);
        url
    }
    fn send(
        &self,
        provider: Provider,
        body: &Value,
    ) -> (ResponseReceipt, Result<Value, ModelError>) {
        let mut receipt = ResponseReceipt::default();
        let mut headers = HeaderMap::new();
        match provider {
            Provider::OpenAI => {
                let mut bearer = HeaderValue::from_str(&format!(
                    "Bearer {}",
                    self.config.key.to_str().expect("validated key")
                ))
                .expect("validated key");
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
            .post(self.endpoint(provider))
            .headers(headers)
            .json(body)
            .send();
        let mut response = match response {
            Ok(response) => response,
            Err(error) => {
                return (
                    receipt,
                    Err(ModelError::new(if error.is_timeout() {
                        ModelErrorKind::Timeout
                    } else {
                        ModelErrorKind::Transport
                    })),
                );
            }
        };
        let status = response.status();
        receipt.http_status = Some(status.as_u16());
        receipt.request_id = ["x-request-id", "request-id"].into_iter().find_map(|key| {
            response
                .headers()
                .get(key)
                .and_then(|v| v.to_str().ok())
                .map(str::to_owned)
        });
        let max = self.config.limit.get();
        let mut bytes = Vec::new();
        let read = response
            .by_ref()
            .take(max as u64 + 1)
            .read_to_end(&mut bytes);
        if let Err(error) = read {
            receipt.capture = ResponseCapture::Interrupted { prefix: bytes };
            let timeout = error.kind() == std::io::ErrorKind::TimedOut
                || error
                    .get_ref()
                    .and_then(|source| source.downcast_ref::<reqwest::Error>())
                    .is_some_and(reqwest::Error::is_timeout);
            return (
                receipt,
                Err(ModelError::new(if timeout {
                    ModelErrorKind::Timeout
                } else {
                    ModelErrorKind::Transport
                })),
            );
        }
        if bytes.len() > max {
            bytes.truncate(max);
            receipt.capture = ResponseCapture::Truncated {
                prefix: bytes,
                limit: max,
            };
            return (
                receipt,
                Err(ModelError::new(ModelErrorKind::ResponseTooLarge)),
            );
        }
        receipt.capture = ResponseCapture::Complete(bytes.clone());
        let value = serde_json::from_slice(&bytes).map_err(|_| malformed());
        if let Ok(value) = &value {
            metadata(&mut receipt, provider, value);
        }
        let key = self.config.key.to_str().expect("validated key");
        let credential_echo = value
            .as_ref()
            .is_ok_and(|value| contains_credential(value, key));
        let assembled_echo = value
            .as_ref()
            .is_ok_and(|value| contains_text(&provider.response_text(value), key));
        if (assembled_echo || credential_echo)
            && let ResponseCapture::Complete(bytes) = &mut receipt.capture
        {
            bytes.fill(b'*');
        }
        if !status.is_success() {
            return (
                receipt,
                Err(ModelError::http(
                    match status.as_u16() {
                        400 => ModelErrorKind::InvalidSettings,
                        401 | 403 => ModelErrorKind::Authentication,
                        429 => ModelErrorKind::RateLimited,
                        500..=599 => ModelErrorKind::ProviderFailure,
                        _ => ModelErrorKind::Http,
                    },
                    status.as_u16(),
                )),
            );
        }
        if credential_echo {
            return (receipt, Err(malformed()));
        }
        (receipt, value)
    }
    fn redact_receipt(&self, receipt: &mut ResponseReceipt) {
        let key = self.config.key.as_bytes();
        match &mut receipt.capture {
            ResponseCapture::Complete(bytes) => scrub_capture(bytes, key),
            ResponseCapture::Truncated { prefix, .. } | ResponseCapture::Interrupted { prefix } => {
                scrub_capture(prefix, key)
            }
            ResponseCapture::NotReceived => {}
        }
        for value in [
            &mut receipt.served_model,
            &mut receipt.response_id,
            &mut receipt.request_id,
            &mut receipt.finish,
        ]
        .into_iter()
        .flatten()
        {
            let mut bytes = value.as_bytes().to_vec();
            scrub_capture(&mut bytes, key);
            *value = String::from_utf8(bytes).expect("redaction preserves UTF-8");
        }
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
}

pub(super) fn decision_schema() -> Value {
    json!({"type":"object","properties":{"should_allow":{"type":"boolean"},"reason":{"type":["string","null"]}},"required":["should_allow","reason"],"additionalProperties":false})
}
pub(super) fn instructions<O>(plan: &ContextPlan<O>, decision: bool) -> String {
    let mut text = plan.instructions().as_str().to_owned();
    if decision && !plan.tools().is_empty() {
        text.push_str("\nReview tool catalog (descriptive only, not callable):\n");
        for tool in plan.tools() {
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

fn contains_credential(value: &Value, key: &str) -> bool {
    match value {
        Value::String(text) => contains_text(text, key),
        Value::Array(items) => items.iter().any(|item| contains_credential(item, key)),
        Value::Object(fields) => fields
            .iter()
            .any(|(name, value)| contains_text(name, key) || contains_credential(value, key)),
        _ => false,
    }
}

fn contains_text(text: &str, key: &str) -> bool {
    text.contains(key)
        || serde_json::from_str::<Value>(text).is_ok_and(|value| contains_credential(&value, key))
}

fn decoded_string(bytes: &[u8]) -> Vec<u8> {
    let mut decoded = Vec::with_capacity(bytes.len());
    let mut index = 0;
    while index < bytes.len() {
        if bytes[index] == b'\\' {
            let length = if bytes.get(index + 1) == Some(&b'u') {
                if bytes.get(index + 6..index + 8) == Some(b"\\u") {
                    12
                } else {
                    6
                }
            } else {
                2
            };
            let mut consumed = false;
            for length in [length, 6, 2] {
                if let Some(escape) = bytes.get(index..index + length) {
                    let mut token = vec![b'"'];
                    token.extend(escape);
                    token.push(b'"');
                    if let Ok(value) = serde_json::from_slice::<String>(&token) {
                        decoded.extend(value.as_bytes());
                        index += length;
                        consumed = true;
                        break;
                    }
                }
            }
            if consumed {
                continue;
            }
        }
        decoded.push(bytes[index]);
        index += 1;
    }
    decoded
}

fn scrub_capture(bytes: &mut [u8], key: &[u8]) {
    let mut index = 0;
    while index < bytes.len() {
        if bytes[index] != b'"' {
            index += 1;
            continue;
        }
        index += 1;
        let start = index;
        let mut closed = false;
        while index < bytes.len() {
            match bytes[index] {
                b'\\' => index = (index + 2).min(bytes.len()),
                b'"' => {
                    closed = true;
                    break;
                }
                _ => index += 1,
            }
        }
        let mut decoded = decoded_string(&bytes[start..index]);
        let original = decoded.clone();
        scrub_capture(&mut decoded, key);
        if decoded != original {
            bytes[start..index].fill(b'*');
        }
        index += usize::from(closed);
    }
    let mut index = 0;
    while index + key.len() <= bytes.len() {
        if bytes[index..].starts_with(key) {
            bytes[index..index + key.len()].fill(b'*');
            index += key.len();
        } else {
            index += 1;
        }
    }
}
