use crate::support;
use cola::{app::*, domain::*};
use serde_json::{Value, json};
use std::{
    cell::{Cell, RefCell},
    collections::BTreeMap,
    io,
    num::NonZeroU32,
    rc::Rc,
};

#[derive(Clone, Default)]
struct Counts {
    model: Rc<Cell<usize>>,
    review: Rc<Cell<usize>>,
    save: Rc<Cell<usize>>,
    load: Rc<Cell<usize>>,
    list: Rc<Cell<usize>>,
    tool: Rc<Cell<usize>>,
}
fn text(s: &str) -> NonEmptyText {
    NonEmptyText::new(s).unwrap()
}
fn definition() -> ToolDefinition {
    ToolDefinition::new(
        ToolName::new("echo").unwrap(),
        text("echo"),
        InputSchema::new(json!({"type":"object"})).unwrap(),
    )
}
fn call() -> ToolCall {
    ToolCall::new(
        ToolCallId::new("call-1").unwrap(),
        ToolName::new("echo").unwrap(),
        json!({"n":42}),
    )
    .unwrap()
}
struct Roles {
    counts: Counts,
    mode: String,
}
impl Assistant for Roles {
    fn context(&self, s: &Session) -> Result<Context<AssistantMessage>, ContextError> {
        if self.mode == "context-error" {
            return Err(ContextError::MissingUser);
        }
        let tool = if self.mode == "catalog-mismatch" {
            ToolDefinition::new(
                ToolName::new("echo").unwrap(),
                text("changed description"),
                InputSchema::new(json!({"type":"object"})).unwrap(),
            )
        } else {
            definition()
        };
        Context::new(text("answer"), s.context_exchanges()?, vec![tool])
    }
}
impl ToolCallReviewer for Roles {
    fn context(&self, s: &Session, _: &ToolCall) -> Result<Context<Decision>, ContextError> {
        self.counts.review.set(self.counts.review.get() + 1);
        if self.mode == "review-context-error" {
            return Err(ContextError::MissingUser);
        }
        Context::new(text("review"), s.closed_context_exchanges()?, vec![])
    }
}
struct Model {
    counts: Counts,
    mode: String,
}
impl Model {
    fn assistant_generation(
        &self,
        _: &Context<AssistantMessage>,
        _: &ModelSettings,
    ) -> Result<Generation<AssistantMessage>, ModelError> {
        let n = self.counts.model.get() + 1;
        self.counts.model.set(n);
        match self.mode.as_str() {
            "timeout" => return Err(ModelError::new(ModelErrorKind::Timeout)),
            "model" => return Err(ModelError::new(ModelErrorKind::Transport)),
            "output-rejected" => {
                return Ok(Generation::Complete {
                    output: AssistantMessage::tool_requests(
                        None,
                        ToolCalls::new(vec![
                            ToolCall::new(
                                ToolCallId::new("unknown-call").unwrap(),
                                ToolName::new("unknown").unwrap(),
                                json!({}),
                            )
                            .unwrap(),
                        ])
                        .unwrap(),
                    ),
                });
            }
            "refused" => {
                return Ok(Generation::Refused {
                    reason: Some(text("private refusal")),
                });
            }
            "incomplete" => {
                return Ok(Generation::Incomplete {
                    reason: IncompleteReason::OutputLimit,
                });
            }
            _ => {}
        }
        let output = if (self.mode == "tools"
            || self.mode == "review-timeout"
            || self.mode == "review-context-error"
            || self.mode == "denied")
            && n == 1
            || self.mode == "limit"
        {
            AssistantMessage::tool_requests(
                Some("checking".into()),
                ToolCalls::new(vec![call()]).unwrap(),
            )
        } else {
            AssistantMessage::answer(format!("answer-{n}")).unwrap()
        };
        Ok(Generation::Complete { output })
    }
}
impl Model {
    fn review_generation(
        &self,
        _: &Context<Decision>,
        _: &ModelSettings,
    ) -> Result<Generation<Decision>, ModelError> {
        if self.mode == "review-timeout" {
            return Err(ModelError::new(ModelErrorKind::Timeout));
        }
        Ok(Generation::Complete {
            output: Decision::new(self.mode != "denied", Some(text("policy"))),
        })
    }
}
impl ModelClient<AssistantMessage> for Model {
    fn route(&self) -> String {
        "fixture".into()
    }
    fn invoke(
        &self,
        input: ModelInput<'_, AssistantMessage>,
        recorder: &mut dyn BeforeModelSend,
    ) -> Result<Observation<AssistantMessage>, SessionStoreError> {
        let _permit = recorder.persist(support::prepared(&input, "fixture", true))?;
        Ok(support::observed(
            self.assistant_generation(input.context, input.settings),
        ))
    }
}
impl ModelClient<Decision> for Model {
    fn route(&self) -> String {
        "fixture".into()
    }
    fn invoke(
        &self,
        input: ModelInput<'_, Decision>,
        recorder: &mut dyn BeforeModelSend,
    ) -> Result<Observation<Decision>, SessionStoreError> {
        let _permit = recorder.persist(support::prepared(&input, "fixture", false))?;
        Ok(support::observed(
            self.review_generation(input.context, input.settings),
        ))
    }
}
struct Target(Counts);
impl Tool for Target {
    fn execute(&self, args: &serde_json::Map<String, Value>) -> ToolOutcome {
        assert_eq!(args["n"], 42);
        self.0.tool.set(self.0.tool.get() + 1);
        ToolOutcome::Success("result".into())
    }
}
#[derive(Clone, Default)]
struct Store {
    counts: Counts,
    sessions: Rc<RefCell<BTreeMap<SessionId, SessionDocument>>>,
    fail_save: Option<usize>,
    fail_list: bool,
}
impl Store {
    fn publish(&self, doc: &SessionDocument) -> Result<(), SessionStoreError> {
        let n = self.counts.save.get() + 1;
        self.counts.save.set(n);
        if self.fail_save == Some(n) {
            return Err(SessionStoreError::new(
                SessionStoreErrorKind::Storage,
                io::Error::other("injected store error"),
            ));
        }
        self.sessions
            .borrow_mut()
            .insert(doc.session().id().clone(), doc.clone());
        Ok(())
    }
}
struct Writer {
    store: Store,
    doc: SessionDocument,
    valid: bool,
}
impl SessionWriter for Writer {
    fn committed(&self) -> &SessionDocument {
        &self.doc
    }
    fn commit(&mut self, next: SessionDocument) -> Result<u64, SessionStoreError> {
        assert!(self.valid);
        self.valid = false;
        assert!(self.doc.validates_successor(&next));
        self.store.publish(&next)?;
        self.doc = next;
        self.valid = true;
        Ok(self.doc.revision())
    }
}
impl SessionStore for Store {
    fn create(&self) -> Result<Box<dyn SessionWriter + '_>, SessionStoreError> {
        let doc = SessionDocument::new(SessionId::new(uuid::Uuid::new_v4().to_string()).unwrap());
        self.publish(&doc)?;
        Ok(Box::new(Writer {
            store: self.clone(),
            doc,
            valid: true,
        }))
    }
    fn inspect(&self, id: &SessionId) -> Result<SessionDocument, SessionStoreError> {
        self.counts.load.set(self.counts.load.get() + 1);
        self.sessions.borrow().get(id).cloned().ok_or_else(|| {
            SessionStoreError::new(SessionStoreErrorKind::NotFound, io::Error::other("missing"))
        })
    }
    fn acquire(&self, id: &SessionId) -> Result<Box<dyn SessionWriter + '_>, SessionStoreError> {
        Ok(Box::new(Writer {
            store: self.clone(),
            doc: self.inspect(id)?,
            valid: true,
        }))
    }
    fn list(&self) -> Result<Vec<SessionId>, SessionStoreError> {
        self.counts.list.set(self.counts.list.get() + 1);
        if self.fail_list {
            return Err(SessionStoreError::new(
                SessionStoreErrorKind::Storage,
                io::Error::other("injected list error"),
            ));
        }
        Ok(self.sessions.borrow().keys().cloned().collect())
    }
}
fn service<S: SessionStore>(
    mode: &str,
    store: S,
    counts: Counts,
) -> AgentService<Model, S, Roles, Roles> {
    let settings = ModelSettings {
        model_id: ModelId::new("fake").unwrap(),
        effort: Effort::Default,
        max_output_tokens: NonZeroU32::new(100).unwrap(),
    };
    AgentService::new(
        Model {
            counts: counts.clone(),
            mode: mode.into(),
        },
        store,
        Roles {
            counts: counts.clone(),
            mode: mode.into(),
        },
        Roles {
            counts: counts.clone(),
            mode: mode.into(),
        },
        vec![(definition(), Box::new(Target(counts)))],
        AgentSettings {
            assistant_model_settings: settings.clone(),
            reviewer_model_settings: settings,
            max_iterations: NonZeroU32::new(if mode == "limit" { 1 } else { 5 }).unwrap(),
        },
    )
    .unwrap()
}
use cola::adapters::{api::web::WebApi, stores::JsonlSessionStore};
use std::{
    fs,
    io::{Read, Write},
    net::{Shutdown, TcpListener, TcpStream},
    path::PathBuf,
    process::{Child, Command, Stdio},
    time::{Duration, Instant},
};

#[test]
fn server_child() {
    let Some(root) = std::env::var_os("COLA_API_CHILD_ROOT") else {
        return;
    };
    let root = PathBuf::from(root);
    let mode = std::env::var("COLA_API_CHILD_MODE").unwrap();
    let listener = TcpListener::bind("127.0.0.1:0").unwrap();
    let counts = Counts::default();
    if matches!(
        mode.as_str(),
        "save-failure" | "create-failure" | "list-failure"
    ) {
        let store = Store {
            counts: counts.clone(),
            fail_save: match mode.as_str() {
                "save-failure" => Some(3),
                "create-failure" => Some(1),
                _ => None,
            },
            fail_list: mode == "list-failure",
            ..Store::default()
        };
        let api = WebApi::new(service("answer", store, counts));
        fs::write(
            root.join("ready.tmp"),
            listener.local_addr().unwrap().to_string(),
        )
        .unwrap();
        fs::rename(root.join("ready.tmp"), root.join("ready")).unwrap();
        api.run(listener).unwrap();
    } else {
        let store = JsonlSessionStore::new(root.join("sessions"));
        {
            use sha2::{Digest, Sha256};
            fs::create_dir_all(root.join("sessions")).unwrap();
            let header = json!({"format":"cola.session","version":2,"session_id":"opaque id:世界","closed_turn_count":0,"active":false,"revision":0,"invocations":[],"views":[],"acknowledgements":[]});
            fs::write(
                root.join("sessions").join(format!(
                    "{:x}.jsonl",
                    Sha256::digest("opaque id:世界".as_bytes())
                )),
                format!("{header}\n"),
            )
            .unwrap();
        }
        let api = WebApi::new(service(&mode, store, counts));
        fs::write(
            root.join("ready.tmp"),
            listener.local_addr().unwrap().to_string(),
        )
        .unwrap();
        fs::rename(root.join("ready.tmp"), root.join("ready")).unwrap();
        api.run(listener).unwrap();
    }
}
struct Server {
    child: Child,
    root: tempfile::TempDir,
    address: String,
}
impl Drop for Server {
    fn drop(&mut self) {
        let _ = self.child.kill();
        let _ = self.child.wait();
    }
}
impl Server {
    fn new(mode: &str) -> Self {
        let root = tempfile::tempdir().unwrap();
        let child = Command::new(std::env::current_exe().unwrap())
            .args(["--exact", "api::web::server_child", "--nocapture"])
            .env("COLA_API_CHILD_ROOT", root.path())
            .env("COLA_API_CHILD_MODE", mode)
            .stdout(Stdio::null())
            .stderr(Stdio::inherit())
            .spawn()
            .unwrap();
        let mut server = Self {
            child,
            root,
            address: String::new(),
        };
        let deadline = Instant::now() + Duration::from_secs(10);
        loop {
            if let Ok(address) = fs::read_to_string(server.root.path().join("ready")) {
                server.address = address;
                break;
            }
            assert!(
                server.child.try_wait().unwrap().is_none(),
                "child exited before ready"
            );
            assert!(Instant::now() < deadline, "server readiness timeout");
            std::thread::sleep(Duration::from_millis(10));
        }
        server
    }
    fn wire_response(&self, request: &[u8]) -> (u16, String, Vec<u8>) {
        let mut stream = TcpStream::connect(&self.address).unwrap();
        stream
            .set_read_timeout(Some(Duration::from_secs(5)))
            .unwrap();
        stream.write_all(request).unwrap();
        let mut bytes = vec![];
        stream.read_to_end(&mut bytes).unwrap();
        let _ = stream.shutdown(Shutdown::Both);
        let boundary = bytes.windows(4).position(|w| w == b"\r\n\r\n").unwrap();
        let headers = String::from_utf8(bytes[..boundary].to_vec()).unwrap();
        let status = headers.split_whitespace().nth(1).unwrap().parse().unwrap();
        let mut body = bytes[boundary + 4..].to_vec();
        if headers
            .to_ascii_lowercase()
            .contains("transfer-encoding: chunked")
        {
            let mut remaining = body.as_slice();
            let mut decoded = Vec::new();
            loop {
                let end = remaining.windows(2).position(|w| w == b"\r\n").unwrap();
                let size =
                    usize::from_str_radix(std::str::from_utf8(&remaining[..end]).unwrap(), 16)
                        .unwrap();
                remaining = &remaining[end + 2..];
                if size == 0 {
                    break;
                }
                decoded.extend_from_slice(&remaining[..size]);
                remaining = &remaining[size + 2..];
            }
            body = decoded;
        }
        (status, headers, body)
    }
    fn raw(&self, request: &[u8]) -> (u16, String, Value) {
        let (status, headers, body) = self.wire_response(request);
        assert!(
            headers
                .to_ascii_lowercase()
                .contains("content-type: application/json")
        );
        (status, headers, serde_json::from_slice(&body).unwrap())
    }
    fn request(
        &self,
        method: &str,
        path: &str,
        body: &[u8],
        media: Option<&str>,
    ) -> (u16, String, Value) {
        let mut request = format!(
            "{method} {path} HTTP/1.1\r\nHost: localhost\r\nConnection: close\r\nContent-Length: {}\r\n",
            body.len()
        );
        if let Some(media) = media {
            request.push_str(&format!("Content-Type: {media}\r\n"));
        }
        request.push_str("\r\n");
        let mut request = request.into_bytes();
        request.extend(body);
        self.raw(&request)
    }
    fn create(&self) -> String {
        let (status, _, r) = self.request("POST", "/sessions", b"", None);
        assert_eq!(status, 201);
        r["session"]["id"].as_str().unwrap().into()
    }
    fn chat(&self, id: &str) -> (u16, Value) {
        let (s, _, v) = self.request(
            "POST",
            &format!("/sessions/{id}/messages"),
            br#"{"text":" question "}"#,
            Some("application/json"),
        );
        (s, v)
    }
}
#[test]
fn loopback_persistent_service_and_disk_readback() {
    let server = Server::new("answer");
    let id = server.create();
    for n in 1..=2 {
        let (s, v) = server.chat(&id);
        assert_eq!(s, 200);
        assert_eq!(v["session_id"], id);
        assert_eq!(
            v["messages"],
            json!([{"kind":"user","text":" question "},{"kind":"assistant_answer","text":format!("answer-{n}")}])
        );
    }
    let (s, _, v) = server.request("GET", "/sessions?limit=ignored", b"", None);
    assert_eq!(s, 200);
    let s = v["sessions"]
        .as_array()
        .unwrap()
        .iter()
        .find(|s| s["id"] == id)
        .unwrap();
    assert_eq!(s["turns"].as_array().unwrap().len(), 2);
    assert!(s["active"].is_null());
    let store = JsonlSessionStore::new(server.root.path().join("sessions"));
    let session = store
        .inspect(&SessionId::new(id).unwrap())
        .map(|d| d.session().clone())
        .unwrap();
    assert_eq!(session.turns().len(), 2);
    let bytes: Vec<_> = fs::read_dir(server.root.path().join("sessions"))
        .unwrap()
        .map(|p| fs::read(p.unwrap().path()).unwrap())
        .collect();
    assert!(
        bytes
            .iter()
            .any(|b| String::from_utf8_lossy(b).contains("answer-2"))
    );
    let (status, _, v) = server.request(
        "POST",
        "/sessions/opaque%20id%3A%E4%B8%96%E7%95%8C/messages",
        br#"{"text":"hello"}"#,
        Some("Application/JSON; charset=utf-8"),
    );
    assert_eq!(status, 200);
    assert_eq!(v["session_id"], "opaque id:世界");
}
#[test]
fn strict_http_boundaries_do_not_dispatch_work() {
    let server = Server::new("answer");
    let id = server.create();
    let path = format!("/sessions/{id}/messages");
    for body in [
        b"[]".as_slice(),
        b"null",
        b"{}",
        br#"{"text":1}"#,
        br#"{"text":" "}"#,
        br#"{"text":"ok","extra":1}"#,
        br#"{"text":"a","text":"b"}"#,
        b"{",
        b"{\"text\":\"\xff\"}",
    ] {
        assert_eq!(
            server
                .request("POST", &path, body, Some("application/json"))
                .0,
            400,
            "{body:?}"
        );
    }
    for body in [
        b"[]".as_slice(),
        br#"{"extra":1}"#,
        br#"{"extra":1,"extra":2}"#,
        b"null",
    ] {
        assert_eq!(
            server
                .request("POST", "/sessions", body, Some("application/json"))
                .0,
            400
        );
    }
    assert_eq!(
        server.request("POST", &path, br#"{"text":"ok"}"#, None).0,
        415
    );
    assert_eq!(
        server
            .request("POST", &path, br#"{"text":"ok"}"#, Some("text/plain"))
            .0,
        415
    );
    assert_eq!(
        server
            .request("POST", &path, &vec![b'x'; 65537], Some("application/json"))
            .0,
        413
    );
    for path in [
        "/sessions/%/messages",
        "/sessions/%GG/messages",
        "/sessions/%FF/messages",
        "/sessions/%2F/messages",
        "/sessions/%5c/messages",
        "/sessions/%20/messages",
    ] {
        assert_eq!(server.request("POST", path, b"", None).0, 400, "{path}");
    }
    for path in [
        "/unknown",
        "/sessions/a/b/messages",
        "/sessions//messages",
        "/sessions/a/messages/extra",
    ] {
        assert_eq!(server.request("POST", path, b"", None).0, 404);
    }
    let (s, h, _) = server.request("PUT", "/sessions", b"", None);
    assert_eq!(s, 405);
    assert!(h.to_ascii_lowercase().contains("allow: get, post"));
    let (s, h, _) = server.request("GET", &path, b"", None);
    assert_eq!(s, 405);
    assert!(h.to_ascii_lowercase().contains("allow: post"));
    assert_eq!(server.chat("missing").0, 404);
    let (_, _, v) = server.request("GET", "/sessions", b"", None);
    assert_eq!(v["sessions"].as_array().unwrap().len(), 2);
    assert!(
        v["sessions"]
            .as_array()
            .unwrap()
            .iter()
            .all(|s| s["turns"] == json!([]))
    );
    let (s, v) = server.chat(&id);
    assert_eq!(s, 200);
    assert_eq!(v["messages"][1]["text"], "answer-1");
    assert_eq!(
        server
            .request("POST", "/sessions", b"{}", Some("application/json"))
            .0,
        201
    );
}
#[test]
fn chunked_body_limit_and_exact_payload() {
    let server = Server::new("answer");
    let id = server.create();
    for (size, expected) in [(65525, 200), (65526, 413)] {
        let body = serde_json::to_vec(&json!({"text":"x".repeat(size)})).unwrap();
        assert_eq!(body.len(), size + 11);
        let mut request=format!("POST /sessions/{id}/messages HTTP/1.1\r\nHost: localhost\r\nConnection: close\r\nContent-Type: application/json\r\nTransfer-Encoding: chunked\r\n\r\n{:X}\r\n",body.len()).into_bytes();
        request.extend(body);
        request.extend(b"\r\n0\r\n\r\n");
        assert_eq!(server.raw(&request).0, expected);
    }
}
#[test]
fn early_body_rejection_delivers_complete_response_without_reset() {
    let mut server = Server::new("answer");
    let id = server.create();
    let mut stream = TcpStream::connect(&server.address).unwrap();
    stream.set_nodelay(true).unwrap();
    stream
        .set_read_timeout(Some(Duration::from_secs(5)))
        .unwrap();
    stream
        .set_write_timeout(Some(Duration::from_secs(5)))
        .unwrap();
    write!(
        stream,
        "POST /sessions/{id}/messages HTTP/1.1\r\nHost: localhost\r\nConnection: close\r\nContent-Type: application/json\r\nContent-Length: 65537\r\n\r\n"
    )
    .unwrap();
    stream.write_all(&vec![b'x'; 65537]).unwrap();
    let mut response = Vec::new();
    stream.read_to_end(&mut response).unwrap();
    let boundary = response.windows(4).position(|w| w == b"\r\n\r\n").unwrap();
    let headers = std::str::from_utf8(&response[..boundary]).unwrap();
    assert!(headers.starts_with("HTTP/1.1 413 "));
    assert!(
        headers
            .to_ascii_lowercase()
            .contains("content-type: application/json")
    );
    let failure: Value = serde_json::from_slice(&response[boundary + 4..]).unwrap();
    assert_eq!(failure["error"]["code"], "body_too_large");
    let response = reqwest::blocking::Client::builder()
        .timeout(Duration::from_secs(5))
        .build()
        .unwrap()
        .post(format!("http://{}/sessions/{id}/messages", server.address))
        .header("Content-Type", "application/json")
        .body(vec![b'x'; 65537])
        .send()
        .unwrap();
    assert_eq!(response.status(), 413);
    let failure: Value = response.json().unwrap();
    assert_eq!(failure["error"]["code"], "body_too_large");
    assert!(server.child.try_wait().unwrap().is_none());
    let (status, chat) = server.chat(&id);
    assert_eq!(status, 200);
    assert_eq!(chat["messages"][1]["text"], "answer-1");
    drop(stream);
}
#[test]
fn oversized_declared_bodies_do_not_kill_the_resident_service() {
    let mut server = Server::new("answer");
    let id = server.create();
    for (index, (length, method, path, expected)) in [
        (usize::MAX, "POST", "/sessions", 413),
        (usize::MAX, "POST", "/unknown", 404),
        (usize::MAX, "PUT", "/sessions", 405),
        (usize::MAX / 2, "POST", "/sessions", 413),
        (usize::MAX / 2, "POST", "/unknown", 404),
        (usize::MAX / 2, "PUT", "/sessions", 405),
    ]
    .into_iter()
    .enumerate()
    {
        let request = format!(
            "{method} {path} HTTP/1.1\r\nHost: localhost\r\nConnection: close\r\nContent-Type: application/json\r\nContent-Length: {}\r\n\r\n",
            length
        );
        let (status, _, _) = server.wire_response(request.as_bytes());
        if length == usize::MAX {
            assert!(
                status == expected || matches!(status, 400 | 413 | 431),
                "unexpected status {status}"
            );
        } else {
            assert_eq!(status, expected);
        }
        assert!(server.child.try_wait().unwrap().is_none());
        let (status, chat) = server.chat(&id);
        assert_eq!(status, 200);
        assert_eq!(chat["messages"][1]["text"], format!("answer-{}", index + 1));
    }
    let stored = JsonlSessionStore::new(server.root.path().join("sessions"))
        .inspect(&SessionId::new(id).unwrap())
        .map(|d| d.session().clone())
        .unwrap();
    assert_eq!(stored.turns().len(), 6);
}
#[test]
fn create_and_list_failures_keep_safe_progress_and_service_residency() {
    let server = Server::new("create-failure");
    let (status, _, failure) = server.request("POST", "/sessions", b"", None);
    assert_eq!(status, 500);
    assert_eq!(failure["error"]["code"], "store_failure");
    assert_eq!(failure["error"]["durability"], "unconfirmed");
    assert!(failure["error"]["progress"].is_null());
    assert!(!failure.to_string().contains("/private/secret"));
    let (status, _, listing) = server.request("GET", "/sessions", b"", None);
    assert_eq!(status, 200);
    assert_eq!(listing["sessions"], json!([]));

    let server = Server::new("list-failure");
    let (status, _, failure) = server.request("GET", "/sessions", b"", None);
    assert_eq!(status, 500);
    assert_eq!(failure["error"]["code"], "store_failure");
    assert!(failure["error"]["progress"].is_null());
    assert!(!failure.to_string().contains("/private/secret"));
    let id = server.create();
    let (status, chat) = server.chat(&id);
    assert_eq!(status, 200);
    assert_eq!(chat["messages"][1]["text"], "answer-1");
}
#[test]
fn http_tool_results_and_failure_progress_are_full_and_safe() {
    for (mode, status, code, role) in [
        ("timeout", 504, "model_timeout", "assistant"),
        ("review-timeout", 504, "model_timeout", "reviewer"),
        ("refused", 422, "generation_refused", "assistant"),
        ("incomplete", 502, "generation_incomplete", "assistant"),
        ("model", 502, "model_failure", "assistant"),
        ("catalog-mismatch", 500, "catalog_mismatch", "assistant"),
        ("context-error", 500, "invalid_context", "assistant"),
        ("review-context-error", 500, "invalid_context", "reviewer"),
        ("output-rejected", 502, "output_rejected", "assistant"),
        ("limit", 422, "iteration_limit", ""),
        ("save-failure", 500, "store_failure", ""),
    ] {
        let server = Server::new(mode);
        let id = server.create();
        let (s, v) = server.chat(&id);
        assert_eq!(s, status, "{mode}");
        assert_eq!(v["error"]["code"], code);
        assert_eq!(v["error"]["durability"], "unconfirmed");
        if !role.is_empty() {
            assert_eq!(v["error"]["role"], role);
        }
        assert!(!v.to_string().contains("/private/secret"));
        assert!(!v.to_string().contains("private refusal"));
        if mode == "review-timeout" || mode == "review-context-error" {
            assert_eq!(
                v["error"]["progress"]["active"]["steps"][0]["executions"][0]["state"]["kind"],
                "started"
            );
            assert_eq!(server.chat(&id).0, 409);
        } else if mode != "save-failure" {
            assert!(v["error"]["progress"]["active"].is_null());
        }
        if mode == "save-failure" {
            assert!(v["error"]["progress"]["active"].is_object());
            assert_eq!(server.chat(&id).0, 200);
        }
    }
    for mode in ["tools", "denied"] {
        let server = Server::new(mode);
        let id = server.create();
        let (s, v) = server.chat(&id);
        assert_eq!(s, 200);
        assert_eq!(v["messages"][1]["kind"], "assistant_tool_requests");
        assert_eq!(v["messages"][1]["calls"][0]["arguments"], json!({"n":42}));
        assert_eq!(
            v["messages"][2]["outcome"]["kind"],
            if mode == "tools" {
                "success"
            } else {
                "failure"
            }
        );
        assert_eq!(v["messages"][3]["kind"], "assistant_answer");
        let (_, _, v) = server.request("GET", "/sessions", b"", None);
        let s = v["sessions"]
            .as_array()
            .unwrap()
            .iter()
            .find(|s| s["id"] == id)
            .unwrap();
        assert_eq!(
            s["turns"][0]["steps"][0]["executions"][0]["state"]["kind"],
            "returned"
        );
    }
}
#[test]
fn disconnect_does_not_undo_admitted_chat() {
    let server = Server::new("answer");
    let id = server.create();
    let body = br#"{"text":"disconnected"}"#;
    let mut stream = TcpStream::connect(&server.address).unwrap();
    write!(stream,"POST /sessions/{id}/messages HTTP/1.1\r\nHost: localhost\r\nContent-Type: application/json\r\nContent-Length: {}\r\nConnection: close\r\n\r\n",body.len()).unwrap();
    stream.write_all(body).unwrap();
    drop(stream);
    let deadline = Instant::now() + Duration::from_secs(5);
    loop {
        let (_, _, v) = server.request("GET", "/sessions", b"", None);
        if v["sessions"]
            .as_array()
            .unwrap()
            .iter()
            .any(|s| s["id"] == id && s["turns"].as_array().unwrap().len() == 1)
        {
            break;
        }
        assert!(Instant::now() < deadline);
    }
    let store = JsonlSessionStore::new(server.root.path().join("sessions"));
    assert_eq!(
        store
            .inspect(&SessionId::new(id).unwrap())
            .map(|d| d.session().clone())
            .unwrap()
            .turns()
            .len(),
        1
    );
}

#[test]
fn review_failure_acknowledgement_http_contract() {
    let server = Server::new("review-timeout");
    let id = server.create();
    let (status, failure) = server.chat(&id);
    assert_eq!(status, 504);
    assert_eq!(failure["error"]["invocation_id"], 2);
    let route = format!("/sessions/{id}/review-failure-acknowledgements");
    for (body, status) in [
        (json!({"invocation_id":0,"evidence":"checked"}), 400),
        (json!({"invocation_id":2,"evidence":" "}), 400),
        (json!({"invocation_id":1,"evidence":"checked"}), 409),
        (json!({"invocation_id":2,"evidence":"checked"}), 200),
        (json!({"invocation_id":2,"evidence":"checked"}), 200),
        (json!({"invocation_id":2,"evidence":"different"}), 409),
    ] {
        assert_eq!(
            server
                .request(
                    "POST",
                    &route,
                    body.to_string().as_bytes(),
                    Some("application/json")
                )
                .0,
            status
        );
    }
    assert_eq!(server.chat(&id).0, 200);
    let doc = JsonlSessionStore::new(server.root.path().join("sessions"))
        .inspect(&SessionId::new(id).unwrap())
        .unwrap();
    assert_eq!(doc.acknowledgements().len(), 1);
    assert_eq!(doc.acknowledgements()[0].actor, None);
    assert!(matches!(
        doc.session().turns()[0].outcome(),
        TurnOutcome::Failed(_)
    ));
    assert_eq!(
        doc.session().turns()[0].steps()[0]
            .executions()
            .next()
            .unwrap()
            .state(),
        &ExecutionState::Started
    );
}
