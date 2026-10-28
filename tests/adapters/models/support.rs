use cola::{
    adapters::models::{AnthropicClient, ClientConfig, OpenAIClient},
    app::*,
    domain::*,
};
use serde_json::{Value, json};
use std::{
    error::Error,
    io::{Read, Write},
    net::TcpListener,
    num::{NonZeroU32, NonZeroU64, NonZeroUsize},
    sync::{
        Arc, Mutex,
        atomic::{AtomicBool, Ordering},
    },
    thread,
    time::Duration,
};

#[derive(Clone, Copy, Debug)]
pub(super) enum Provider {
    OpenAI,
    Anthropic,
}
pub(super) const PROVIDERS: [Provider; 2] = [Provider::OpenAI, Provider::Anthropic];
impl Provider {
    fn model(self) -> &'static str {
        match self {
            Self::OpenAI => "gpt-6.1-sol",
            Self::Anthropic => "claude-opus-5-5",
        }
    }
    pub(super) fn settings(self) -> ModelSettings {
        ModelSettings {
            model_id: ModelId::new(self.model()).unwrap(),
            effort: Effort::High,
            max_output_tokens: NonZeroU32::new(1024).unwrap(),
        }
    }
    pub(super) fn client(self, server: &Server) -> Box<dyn ModelClient<AssistantMessage>> {
        self.configured(server.config())
    }
    pub(super) fn configured(self, config: ClientConfig) -> Box<dyn ModelClient<AssistantMessage>> {
        match self {
            Self::OpenAI => Box::new(OpenAIClient::new(config).unwrap()),
            Self::Anthropic => Box::new(AnthropicClient::new(config).unwrap()),
        }
    }
    pub(super) fn reviewer(self, server: &Server) -> Box<dyn ModelClient<Decision>> {
        let c = server.config();
        match self {
            Self::OpenAI => Box::new(OpenAIClient::new(c).unwrap()),
            Self::Anthropic => Box::new(AnthropicClient::new(c).unwrap()),
        }
    }
    pub(super) fn answer(self, text: &str) -> Value {
        match self {
            Self::OpenAI => {
                json!({"status":"completed","output":[{"type":"reasoning","id":"reason-final","summary":[],"encrypted_content":"terminal-signature"},{"type":"message","id":"answer","role":"assistant","status":"completed","content":[{"type":"output_text","text":text}]}]})
            }
            Self::Anthropic => {
                json!({"type":"message","role":"assistant","stop_reason":"end_turn","content":[{"type":"thinking","thinking":"private final","signature":"terminal-signature"},{"type":"text","text":text}]})
            }
        }
    }
    pub(super) fn tool(self, n: usize) -> Value {
        match self {
            Self::OpenAI => {
                json!({"status":"completed","output":[{"type":"reasoning","id":format!("reason-{n}"),"summary":[],"encrypted_content":format!("signature-{n}")},{"type":"message","id":format!("message-{n}"),"role":"assistant","content":[{"type":"output_text","text":"working"}]},{"type":"function_call","id":format!("item-{n}"),"call_id":format!("native-{n}"),"name":"lookup","arguments":format!("{{\"n\":{n}}}") }]})
            }
            Self::Anthropic => {
                json!({"type":"message","role":"assistant","stop_reason":"tool_use","content":[{"type":"thinking","thinking":"private","signature":format!("signature-{n}")},{"type":"redacted_thinking","data":"opaque-redacted"},{"type":"text","text":"working"},{"type":"tool_use","id":format!("native-{n}"),"name":"lookup","input":{"n":n}}]})
            }
        }
    }
    pub(super) fn native(self, response: &Value) -> Value {
        match self {
            Self::OpenAI => response["output"].clone(),
            Self::Anthropic => response["content"].clone(),
        }
    }
    pub(super) fn request_native(self, request: &Value) -> Vec<Value> {
        match self {
            Self::OpenAI => request["input"]
                .as_array()
                .unwrap()
                .iter()
                .filter(|v| v.get("type").is_some_and(|t| t != "function_call_output"))
                .cloned()
                .collect(),
            Self::Anthropic => request["messages"]
                .as_array()
                .unwrap()
                .iter()
                .filter(|v| v["role"] == "assistant")
                .flat_map(|v| v["content"].as_array().unwrap().clone())
                .collect(),
        }
    }
}
pub(super) fn text(s: &str) -> NonEmptyText {
    NonEmptyText::new(s).unwrap()
}
pub(super) fn user(s: &str) -> UserMessage {
    UserMessage::new(text(s))
}
pub(super) fn tool() -> ToolDefinition {
    ToolDefinition::new(ToolName::new("lookup").unwrap(),text("Look up a value"),InputSchema::new(json!({"type":"object","properties":{"n":{"type":"integer","minimum":0}},"required":["n"],"additionalProperties":false})).unwrap())
}
pub(super) fn session() -> SessionArchive {
    let mut archive = SessionArchive::new(SessionId::new("fixture").unwrap());
    archive.session_mut().start_turn(user("hello")).unwrap();
    archive
}
pub(super) fn limits() -> ContextLimits {
    ContextLimits {
        input_tokens: NonZeroU64::new(1_000_000).unwrap(),
        request_bytes: NonZeroUsize::new(3_000_000).unwrap(),
    }
}
pub(super) trait FixtureOutput: Sized {
    fn context(
        archive: &SessionArchive,
        instructions: NonEmptyText,
        tools: Vec<ToolDefinition>,
        limits: ContextLimits,
        required: &[TurnId],
    ) -> ContextPlan<Self>;
}
impl FixtureOutput for AssistantMessage {
    fn context(
        archive: &SessionArchive,
        instructions: NonEmptyText,
        tools: Vec<ToolDefinition>,
        limits: ContextLimits,
        required: &[TurnId],
    ) -> ContextPlan<Self> {
        ContextPlan::assistant(archive, instructions, tools, limits, required).unwrap()
    }
}
impl FixtureOutput for Decision {
    fn context(
        archive: &SessionArchive,
        instructions: NonEmptyText,
        tools: Vec<ToolDefinition>,
        limits: ContextLimits,
        required: &[TurnId],
    ) -> ContextPlan<Self> {
        let mut archive = archive.clone();
        if archive.session().next_action() == NextAction::AskModel {
            let call = ToolCall::new(
                ToolCallId::new("candidate").unwrap(),
                ToolName::new("lookup").unwrap(),
                json!({"n":9}),
            )
            .unwrap();
            record_output(
                &mut archive,
                AssistantMessage::tool_requests(None, ToolCalls::new(vec![call]).unwrap()),
            );
        }
        let NextAction::Review(call) = archive.session().next_action() else {
            panic!("expected pending review")
        };
        ContextPlan::reviewer(&archive, &call, instructions, tools, limits, required).unwrap()
    }
}
pub(super) fn context<O: FixtureOutput>(archive: &SessionArchive) -> ContextPlan<O> {
    O::context(archive, text("Be useful"), vec![tool()], limits(), &[])
}
pub(super) trait TestRequest<O>: ModelClient<O> {
    fn request(
        &self,
        plan: &ContextPlan<O>,
        settings: &ModelSettings,
    ) -> Result<Generation<O>, ModelError> {
        let prepared = self.prepare(plan, settings)?;
        self.invoke(&prepared).result
    }
}
impl<O, T: ModelClient<O> + ?Sized> TestRequest<O> for T {}
pub(super) fn complete<O>(generation: Generation<O>) -> O {
    match generation {
        Generation::Complete(output) => output,
        _ => panic!("expected complete"),
    }
}
pub(super) fn record_output(
    archive: &mut SessionArchive,
    output: AssistantMessage,
) -> GenerationId {
    let plan = context::<AssistantMessage>(archive);
    let fitted = plan.fit(|_| json!({"fixture":true})).unwrap();
    let prepared = PreparedRequest::<AssistantMessage>::new(
        ModelTarget::new("fixture", "memory"),
        Provider::OpenAI.settings(),
        fitted,
    );
    let turn = archive.session().active_turn().unwrap().id();
    let generation = archive
        .begin_generation(InvocationRole::Assistant { turn }, &prepared)
        .unwrap();
    archive
        .session_mut()
        .record_assistant(turn, generation, output)
        .unwrap();
    archive
        .record_observation(
            generation,
            Timestamp::now(),
            ResponseReceipt {
                capture: ResponseCapture::Complete(b"{}".to_vec()),
                ..ResponseReceipt::default()
            },
            Disposition::Accepted,
        )
        .unwrap();
    generation
}
pub(super) fn invoke_assistant(
    p: Provider,
    client: &dyn ModelClient<AssistantMessage>,
    archive: &mut SessionArchive,
) -> AssistantMessage {
    let prepared = client.prepare(&context(archive), &p.settings()).unwrap();
    let turn = archive.session().active_turn().unwrap().id();
    let generation = archive
        .begin_generation(InvocationRole::Assistant { turn }, &prepared)
        .unwrap();
    let observed = client.invoke(&prepared);
    let output = complete(observed.result.unwrap());
    archive
        .session_mut()
        .record_assistant(turn, generation, output.clone())
        .unwrap();
    archive
        .record_observation(
            generation,
            observed.at,
            observed.receipt,
            Disposition::Accepted,
        )
        .unwrap();
    output
}
pub(super) fn review(archive: &mut SessionArchive, call: &CallRef, allow: bool) {
    let plan = context::<Decision>(archive);
    let prepared = PreparedRequest::<Decision>::new(
        ModelTarget::new("fixture", "memory"),
        Provider::OpenAI.settings(),
        plan.fit(|_| json!({"review":true})).unwrap(),
    );
    let generation = archive
        .begin_generation(InvocationRole::Reviewer { call: call.clone() }, &prepared)
        .unwrap();
    let turn = archive.session().active_turn().unwrap().id();
    archive
        .session_mut()
        .review_tool(turn, call, generation, &Decision::new(allow, None))
        .unwrap();
    archive
        .record_observation(
            generation,
            Timestamp::now(),
            ResponseReceipt {
                capture: ResponseCapture::Complete(b"{}".to_vec()),
                ..ResponseReceipt::default()
            },
            Disposition::Accepted,
        )
        .unwrap();
}
pub(super) fn accept_tools(archive: &mut SessionArchive) {
    let turn = archive.session().active_turn().unwrap().id();
    let calls: Vec<_> = archive
        .session()
        .active_turn()
        .unwrap()
        .steps()
        .last()
        .unwrap()
        .executions()
        .map(|execution| execution.call_ref())
        .collect();
    for (index, call) in calls.iter().enumerate() {
        review(archive, call, true);
        archive
            .session_mut()
            .start_tool(turn, call, Timestamp::now())
            .unwrap();
        let outcome = if index == 0 {
            ToolOutcome::Success(String::new())
        } else {
            ToolOutcome::Failure(ToolError::new(text("tool failed")))
        };
        archive
            .session_mut()
            .record_tool_result(turn, call, outcome, Timestamp::now())
            .unwrap();
    }
}
#[derive(Clone)]
pub(super) struct Reply {
    pub(super) status: u16,
    pub(super) body: String,
    pub(super) delay: Duration,
    pub(super) body_delay: Duration,
    pub(super) headers: Vec<(String, String)>,
}
impl From<Value> for Reply {
    fn from(value: Value) -> Self {
        Self {
            status: 200,
            body: value.to_string(),
            delay: Duration::ZERO,
            body_delay: Duration::ZERO,
            headers: vec![],
        }
    }
}
#[derive(Clone, Debug)]
pub(super) struct Request {
    pub(super) line: String,
    pub(super) headers: String,
    pub(super) body: Value,
}
pub(super) struct Server {
    pub(super) url: String,
    requests: Arc<Mutex<Vec<Request>>>,
    stop: Arc<AtomicBool>,
    worker: Option<thread::JoinHandle<()>>,
}
impl Server {
    pub(super) fn new(responses: Vec<Reply>) -> Self {
        let listener = TcpListener::bind("127.0.0.1:0").unwrap();
        listener.set_nonblocking(true).unwrap();
        let url = format!("http://{}/custom/v1", listener.local_addr().unwrap());
        let requests = Arc::new(Mutex::new(Vec::new()));
        let received = requests.clone();
        let stop = Arc::new(AtomicBool::new(false));
        let stopped = stop.clone();
        let worker = thread::spawn(move || {
            let mut replies = responses.into_iter();
            while !stopped.load(Ordering::SeqCst) {
                let (mut stream, _) = match listener.accept() {
                    Ok(v) => v,
                    Err(e) if e.kind() == std::io::ErrorKind::WouldBlock => {
                        thread::sleep(Duration::from_millis(1));
                        continue;
                    }
                    Err(_) => break,
                };
                stream.set_nonblocking(false).unwrap();
                stream
                    .set_read_timeout(Some(Duration::from_secs(2)))
                    .unwrap();
                let mut bytes = Vec::new();
                let mut chunk = [0; 4096];
                let header_end = loop {
                    let n = stream.read(&mut chunk).unwrap_or(0);
                    if n == 0 {
                        break None;
                    }
                    bytes.extend_from_slice(&chunk[..n]);
                    if let Some(i) = bytes.windows(4).position(|b| b == b"\r\n\r\n") {
                        break Some(i + 4);
                    }
                };
                let Some(header_end) = header_end else {
                    continue;
                };
                let headers = String::from_utf8_lossy(&bytes[..header_end]).to_string();
                let length = headers
                    .lines()
                    .find_map(|l| {
                        l.to_ascii_lowercase()
                            .strip_prefix("content-length:")
                            .map(|n| n.trim().parse::<usize>().unwrap())
                    })
                    .unwrap();
                while bytes.len() < header_end + length {
                    let n = stream.read(&mut chunk).unwrap_or(0);
                    if n == 0 {
                        break;
                    }
                    bytes.extend_from_slice(&chunk[..n]);
                }
                let request = Request {
                    line: headers.lines().next().unwrap().to_string(),
                    headers,
                    body: serde_json::from_slice(&bytes[header_end..]).unwrap(),
                };
                received.lock().unwrap().push(request);
                let reply = replies.next().unwrap_or_else(|| Reply {
                    status: 500,
                    body: "unexpected request".into(),
                    delay: Duration::ZERO,
                    body_delay: Duration::ZERO,
                    headers: vec![],
                });
                thread::sleep(reply.delay);
                let extra = reply
                    .headers
                    .iter()
                    .map(|(k, v)| format!("{k}: {v}\r\n"))
                    .collect::<String>();
                let length = if reply
                    .headers
                    .iter()
                    .any(|(k, _)| k.eq_ignore_ascii_case("transfer-encoding"))
                {
                    String::new()
                } else {
                    format!("Content-Length: {}\r\n", reply.body.len())
                };
                let response = format!(
                    "HTTP/1.1 {} Fixture\r\nContent-Type: application/json\r\n{}Connection: close\r\n{}\r\n",
                    reply.status, length, extra
                );
                let _ = stream.write_all(response.as_bytes());
                thread::sleep(reply.body_delay);
                let _ = stream.write_all(reply.body.as_bytes());
                let _ = stream.shutdown(std::net::Shutdown::Write);
            }
        });
        Self {
            url,
            requests,
            stop,
            worker: Some(worker),
        }
    }
    fn config(&self) -> ClientConfig {
        ClientConfig::new(
            &self.url,
            "secret-key",
            Duration::from_secs(2),
            NonZeroUsize::new(1_000_000).unwrap(),
        )
        .unwrap()
    }
    pub(super) fn requests(&self) -> Vec<Request> {
        self.requests.lock().unwrap().clone()
    }
    pub(super) fn request(&self, index: usize) -> Request {
        self.requests
            .lock()
            .unwrap()
            .get(index)
            .expect("request was recorded before the synchronous response")
            .clone()
    }
}
impl Drop for Server {
    fn drop(&mut self) {
        self.stop.store(true, Ordering::SeqCst);
        self.worker.take().unwrap().join().unwrap();
    }
}

pub(super) fn assert_safe(error: &ModelError) {
    let text = format!("{error:?} {error}");
    for secret in [
        "API_KEY_SENTINEL",
        "BODY_SENTINEL",
        "URL_SENTINEL",
        "secret-key",
        "http://",
    ] {
        assert!(!text.contains(secret), "leak {text}");
    }
    assert!(error.source().is_none());
}
