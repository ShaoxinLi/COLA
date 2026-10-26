use cola::{
    adapters::models::{AnthropicClient, ClientConfig, OpenAIClient},
    app::*,
    domain::*,
};
use serde_json::{Value, json};
use std::{
    collections::VecDeque,
    io::{Read, Write},
    net::TcpListener,
    num::{NonZeroU32, NonZeroUsize},
    sync::{
        Arc, Mutex,
        atomic::{AtomicBool, Ordering},
    },
    thread,
    time::{Duration, Instant},
};
#[derive(Clone, Copy, Debug)]
pub enum Provider {
    OpenAI,
    Anthropic,
}
pub const PROVIDERS: [Provider; 2] = [Provider::OpenAI, Provider::Anthropic];
impl Provider {
    pub fn model(self) -> &'static str {
        match self {
            Self::OpenAI => "gpt-6.1-sol",
            Self::Anthropic => "claude-opus-5-5",
        }
    }
    pub fn settings(self) -> ModelSettings {
        ModelSettings {
            model_id: ModelId::new(self.model()).unwrap(),
            effort: Effort::High,
            max_output_tokens: NonZeroU32::new(1024).unwrap(),
        }
    }
    pub fn client(self, server: &Server) -> Box<dyn ModelClient<AssistantMessage>> {
        self.configured(server.config())
    }
    pub fn configured(self, config: ClientConfig) -> Box<dyn ModelClient<AssistantMessage>> {
        match self {
            Self::OpenAI => Box::new(OpenAIClient::new(config).unwrap()),
            Self::Anthropic => Box::new(AnthropicClient::new(config).unwrap()),
        }
    }
    pub fn reviewer(self, server: &Server) -> Box<dyn ModelClient<Decision>> {
        let c = server.config();
        match self {
            Self::OpenAI => Box::new(OpenAIClient::new(c).unwrap()),
            Self::Anthropic => Box::new(AnthropicClient::new(c).unwrap()),
        }
    }
    pub fn answer(self, text: &str) -> Value {
        match self {
            Self::OpenAI => {
                json!({"status":"completed","output":[{"type":"reasoning","id":"reason-final","summary":[],"encrypted_content":"terminal-signature"},{"type":"message","id":"answer","role":"assistant","status":"completed","content":[{"type":"output_text","text":text}]}]})
            }
            Self::Anthropic => {
                json!({"type":"message","role":"assistant","stop_reason":"end_turn","content":[{"type":"thinking","thinking":"private final","signature":"terminal-signature"},{"type":"text","text":text}]})
            }
        }
    }
    pub fn tool(self, n: usize) -> Value {
        match self {
            Self::OpenAI => {
                json!({"status":"completed","output":[{"type":"reasoning","id":format!("reason-{n}"),"summary":[],"encrypted_content":format!("signature-{n}")},{"type":"message","id":format!("message-{n}"),"role":"assistant","content":[{"type":"output_text","text":"working"}]},{"type":"function_call","id":format!("item-{n}"),"call_id":format!("native-{n}"),"name":"lookup","arguments":format!("{{\"n\":{n}}}") }]})
            }
            Self::Anthropic => {
                json!({"type":"message","role":"assistant","stop_reason":"tool_use","content":[{"type":"thinking","thinking":"private","signature":format!("signature-{n}")},{"type":"redacted_thinking","data":"opaque-redacted"},{"type":"text","text":"working"},{"type":"tool_use","id":format!("native-{n}"),"name":"lookup","input":{"n":n}}]})
            }
        }
    }
    pub fn native(self, response: &Value) -> Value {
        match self {
            Self::OpenAI => response["output"].clone(),
            Self::Anthropic => response["content"].clone(),
        }
    }
    pub fn request_native(self, request: &Value) -> Vec<Value> {
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
pub fn text(s: &str) -> NonEmptyText {
    NonEmptyText::new(s).unwrap()
}
pub fn user(s: &str) -> UserMessage {
    UserMessage::new(text(s))
}
pub fn tool() -> ToolDefinition {
    ToolDefinition::new(ToolName::new("lookup").unwrap(),text("Look up a value"),InputSchema::new(json!({"type":"object","properties":{"n":{"type":"integer","minimum":0}},"required":["n"],"additionalProperties":false})).unwrap())
}
pub fn session() -> Session {
    let mut s = Session::new(SessionId::new("fixture").unwrap());
    s.start_turn(user("hello")).unwrap();
    s
}
pub fn context<O>(s: &Session) -> Context<O> {
    Context::new(
        text("Be useful"),
        s.context_exchanges().unwrap(),
        vec![tool()],
    )
    .unwrap()
}
pub fn complete<O>(generation: Generation<O>) -> (O, Option<Continuation<O>>) {
    match generation {
        Generation::Complete {
            output,
            continuation,
        } => (output, continuation),
        other => panic!(
            "expected complete, got {}",
            match other {
                Generation::Refused { .. } => "refused",
                Generation::Incomplete { .. } => "incomplete",
                _ => unreachable!(),
            }
        ),
    }
}
pub fn accept(s: &mut Session, output: AssistantMessage) {
    let turn = s.active_turn().unwrap().id();
    let calls = output
        .calls()
        .map(|c| c.as_slice().to_vec())
        .unwrap_or_default();
    s.record_assistant(turn, output).unwrap();
    for (i, call) in calls.iter().enumerate() {
        s.start_tool(turn, call.id()).unwrap();
        let outcome = if i == 0 {
            ToolOutcome::Success(String::new())
        } else {
            ToolOutcome::Failure(ToolError::new(text("tool failed")))
        };
        s.record_tool_result(turn, ToolResult::new(call.id().clone(), outcome))
            .unwrap();
    }
}
#[derive(Clone)]
pub struct Reply {
    pub status: u16,
    pub body: String,
    pub delay: Duration,
    pub body_delay: Duration,
    pub headers: Vec<(String, String)>,
}
impl From<Value> for Reply {
    fn from(value: Value) -> Self {
        Self {
            status: 200,
            body: value.to_string(),
            delay: Duration::ZERO,
            body_delay: std::time::Duration::ZERO,
            headers: vec![],
        }
    }
}
#[derive(Clone, Debug)]
pub struct Request {
    pub line: String,
    pub headers: String,
    pub body: Value,
}
pub struct Server {
    pub url: String,
    requests: Arc<Mutex<Vec<Request>>>,
    stop: Arc<AtomicBool>,
    worker: Option<thread::JoinHandle<()>>,
}
impl Server {
    pub fn new(responses: Vec<Reply>) -> Self {
        let listener = TcpListener::bind("127.0.0.1:0").unwrap();
        listener.set_nonblocking(true).unwrap();
        let url = format!("http://{}/custom/v1", listener.local_addr().unwrap());
        let requests = Arc::new(Mutex::new(Vec::new()));
        let received = requests.clone();
        let stop = Arc::new(AtomicBool::new(false));
        let stopped = stop.clone();
        let worker = thread::spawn(move || {
            let mut replies = VecDeque::from(responses);
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
                let reply = replies.pop_front().unwrap_or_else(|| Reply {
                    status: 500,
                    body: "unexpected request".into(),
                    delay: Duration::ZERO,
                    body_delay: std::time::Duration::ZERO,
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
    pub fn config(&self) -> ClientConfig {
        ClientConfig::new(
            &self.url,
            "secret-key",
            Duration::from_secs(2),
            NonZeroUsize::new(1_000_000).unwrap(),
        )
        .unwrap()
    }
    pub fn requests(&self) -> Vec<Request> {
        self.requests.lock().unwrap().clone()
    }
    pub fn request(&self, index: usize) -> Request {
        let until = Instant::now() + Duration::from_secs(2);
        loop {
            if let Some(r) = self.requests().get(index) {
                return r.clone();
            }
            assert!(Instant::now() < until, "request missing");
            thread::sleep(Duration::from_millis(1));
        }
    }
}
impl Drop for Server {
    fn drop(&mut self) {
        self.stop.store(true, Ordering::SeqCst);
        self.worker.take().unwrap().join().unwrap();
    }
}
