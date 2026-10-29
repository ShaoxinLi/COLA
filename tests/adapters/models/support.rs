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
    num::{NonZeroU32, NonZeroUsize},
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
pub(super) fn session() -> Session {
    let mut s = Session::new(SessionId::new("fixture").unwrap());
    s.start_turn(user("hello")).unwrap();
    s
}
pub(super) fn context<O>(s: &Session) -> Context<O> {
    Context::new(
        text("Be useful"),
        s.context_exchanges().unwrap(),
        vec![tool()],
    )
    .unwrap()
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
    pub(super) fn config(&self) -> ClientConfig {
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

#[derive(Clone)]
pub(super) struct Roles;
impl Assistant for Roles {
    fn context(&self, s: &Session) -> Result<Context<AssistantMessage>, ContextError> {
        Context::new(text("Be useful"), s.context_exchanges()?, vec![tool()])
    }
}
impl ToolCallReviewer for Roles {
    fn context(&self, s: &Session, _: &ToolCall) -> Result<Context<Decision>, ContextError> {
        Context::new(
            text("Review carefully"),
            s.closed_context_exchanges()?,
            vec![tool()],
        )
    }
}
pub(super) struct Pair {
    pub assistant: Box<dyn ModelClient<AssistantMessage>>,
    pub reviewer: Box<dyn ModelClient<Decision>>,
}
impl ModelClient<AssistantMessage> for Pair {
    fn route(&self) -> String {
        self.assistant.route()
    }
    fn invoke(
        &self,
        input: ModelInput<'_, AssistantMessage>,
        recorder: &mut dyn BeforeModelSend,
    ) -> Result<Observation<AssistantMessage>, SessionStoreError> {
        self.assistant.invoke(input, recorder)
    }
}
impl ModelClient<Decision> for Pair {
    fn route(&self) -> String {
        self.reviewer.route()
    }
    fn invoke(
        &self,
        input: ModelInput<'_, Decision>,
        recorder: &mut dyn BeforeModelSend,
    ) -> Result<Observation<Decision>, SessionStoreError> {
        self.reviewer.invoke(input, recorder)
    }
}
pub(super) struct Target(pub Arc<std::sync::atomic::AtomicUsize>);
impl Tool for Target {
    fn execute(&self, _: &serde_json::Map<String, Value>) -> ToolOutcome {
        self.0.fetch_add(1, Ordering::SeqCst);
        ToolOutcome::Success("observed target result".into())
    }
}
pub(super) type Service<S = cola::adapters::stores::JsonlSessionStore> =
    AgentService<Pair, S, Roles, Roles>;
pub(super) fn service(provider: Provider, server: &Server, root: &std::path::Path) -> Service {
    service_with_store(
        provider,
        server,
        cola::adapters::stores::JsonlSessionStore::new(root),
        Arc::new(std::sync::atomic::AtomicUsize::new(0)),
    )
}
pub(super) fn service_with_store<S: SessionStore>(
    provider: Provider,
    server: &Server,
    store: S,
    count: Arc<std::sync::atomic::AtomicUsize>,
) -> Service<S> {
    AgentService::new(
        Pair {
            assistant: provider.client(server),
            reviewer: provider.reviewer(server),
        },
        store,
        Roles,
        Roles,
        vec![(tool(), Box::new(Target(count)))],
        AgentSettings {
            assistant_model_settings: provider.settings(),
            reviewer_model_settings: provider.settings(),
            max_iterations: NonZeroU32::new(8).unwrap(),
        },
    )
    .unwrap()
}
pub(super) fn decision(provider: Provider, allow: bool) -> Value {
    provider.answer(&json!({"should_allow":allow,"reason":null}).to_string())
}
pub(super) fn last_outcome(doc: &SessionDocument) -> &RecordedOutcome {
    match &doc.invocations().last().unwrap().state {
        InvocationState::Finished { outcome, .. } => outcome,
        state => panic!("unexpected {state:?}"),
    }
}
