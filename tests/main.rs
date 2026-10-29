use cola::{adapters::stores::JsonlSessionStore, app::SessionStore, domain::SessionId};
use serde_json::{Value, json};
use std::{
    fs,
    io::{BufRead, BufReader, Read, Write},
    net::TcpListener,
    path::{Path, PathBuf},
    process::{Child, Command, Output, Stdio},
    sync::{
        Arc, Mutex,
        atomic::{AtomicBool, Ordering},
        mpsc,
    },
    thread,
    time::{Duration, Instant},
};

const WAIT: Duration = Duration::from_secs(15);

#[derive(Clone, Copy)]
enum Protocol {
    OpenAi,
    Anthropic,
}
impl Protocol {
    fn name(self) -> &'static str {
        match self {
            Self::OpenAi => "openai-responses",
            Self::Anthropic => "anthropic-messages",
        }
    }
    fn answer(self, text: &str) -> Value {
        match self {
            Self::OpenAi => {
                json!({"status":"completed","output":[{"type":"message","id":"answer","role":"assistant","content":[{"type":"output_text","text":text}]}]})
            }
            Self::Anthropic => {
                json!({"type":"message","role":"assistant","stop_reason":"end_turn","content":[{"type":"text","text":text}]})
            }
        }
    }
    fn calls(self) -> Value {
        let calls = [
            ("read_file", json!({"path":"seed.txt"})),
            (
                "write_file",
                json!({"path":"created.txt","content":"fixture-output"}),
            ),
            ("bash", json!({"command":"cat created.txt"})),
            (
                "write_file",
                json!({"path":"denied.txt","content":"must not exist"}),
            ),
        ];
        let native: Vec<_> = calls.into_iter().enumerate().map(|(index, (name, arguments))| {
            match self {
                Self::OpenAi => json!({"type":"function_call","id":format!("item-{index}"),"call_id":format!("native-{index}"),"name":name,"arguments":arguments.to_string()}),
                Self::Anthropic => json!({"type":"tool_use","id":format!("native-{index}"),"name":name,"input":arguments}),
            }
        }).collect();
        match self {
            Self::OpenAi => json!({"status":"completed","output":native}),
            Self::Anthropic => {
                json!({"type":"message","role":"assistant","stop_reason":"tool_use","content":native})
            }
        }
    }
}

struct Provider {
    url: String,
    requests: Arc<Mutex<Vec<(String, Value)>>>,
    stop: Arc<AtomicBool>,
    worker: Option<thread::JoinHandle<()>>,
}
impl Provider {
    fn new(protocol: Protocol) -> Self {
        let listener = TcpListener::bind("127.0.0.1:0").unwrap();
        listener.set_nonblocking(true).unwrap();
        let url = format!("http://{}/v1", listener.local_addr().unwrap());
        let requests = Arc::new(Mutex::new(Vec::new()));
        let received = requests.clone();
        let stop = Arc::new(AtomicBool::new(false));
        let stopped = stop.clone();
        let worker = thread::spawn(move || {
            let mut assistant_calls = 0;
            let mut reviews = 0;
            while !stopped.load(Ordering::SeqCst) {
                let (mut stream, _) = match listener.accept() {
                    Ok(pair) => pair,
                    Err(error) if error.kind() == std::io::ErrorKind::WouldBlock => {
                        thread::sleep(Duration::from_millis(2));
                        continue;
                    }
                    Err(error) => panic!("fixture accept: {error}"),
                };
                stream.set_nonblocking(false).unwrap();
                stream
                    .set_read_timeout(Some(Duration::from_secs(2)))
                    .unwrap();
                let mut reader = BufReader::new(stream.try_clone().unwrap());
                let mut line = String::new();
                reader.read_line(&mut line).unwrap();
                let route = line.trim().to_owned();
                let mut length = 0;
                loop {
                    line.clear();
                    assert!(
                        reader.read_line(&mut line).unwrap() > 0,
                        "provider request ended before its headers"
                    );
                    if line == "\r\n" {
                        break;
                    }
                    if let Some((name, value)) = line.split_once(':')
                        && name.eq_ignore_ascii_case("content-length")
                    {
                        length = value.trim().parse().unwrap();
                    }
                }
                let mut bytes = vec![0; length];
                reader.read_exact(&mut bytes).unwrap();
                let request: Value = serde_json::from_slice(&bytes).unwrap();
                let review = request["text"].get("format").is_some()
                    || request["output_config"].get("format").is_some();
                received.lock().unwrap().push((route, request));
                let response = if review {
                    reviews += 1;
                    protocol.answer(
                        &json!({"should_allow": reviews != 4,"reason":"fixture scope decision"})
                            .to_string(),
                    )
                } else {
                    assistant_calls += 1;
                    match assistant_calls {
                        1 => protocol.calls(),
                        2 => protocol.answer("first turn complete"),
                        _ => protocol.answer("second turn complete"),
                    }
                }
                .to_string();
                write!(stream, "HTTP/1.1 200 OK\r\nContent-Type: application/json\r\nContent-Length: {}\r\nConnection: close\r\n\r\n{response}", response.len()).unwrap();
            }
        });
        Self {
            url,
            requests,
            stop,
            worker: Some(worker),
        }
    }
    fn requests(&self) -> Vec<(String, Value)> {
        self.requests.lock().unwrap().clone()
    }
}
impl Drop for Provider {
    fn drop(&mut self) {
        self.stop.store(true, Ordering::SeqCst);
        let result = self.worker.take().unwrap().join();
        if !thread::panicking() {
            result.unwrap();
        }
    }
}

struct Process(Option<Child>);
impl Process {
    fn spawn(cwd: &Path, args: &[&std::ffi::OsStr]) -> Self {
        Self(Some(
            Command::new(env!("CARGO_BIN_EXE_cola"))
                .args(args)
                .current_dir(cwd)
                .stdin(Stdio::piped())
                .stdout(Stdio::piped())
                .stderr(Stdio::piped())
                .spawn()
                .unwrap(),
        ))
    }
    fn input(&mut self, input: &str) {
        self.0
            .as_mut()
            .unwrap()
            .stdin
            .take()
            .unwrap()
            .write_all(input.as_bytes())
            .unwrap();
    }
    fn finish(mut self) -> Output {
        let deadline = Instant::now() + WAIT;
        while self.0.as_mut().unwrap().try_wait().unwrap().is_none() {
            assert!(Instant::now() < deadline, "child process timed out");
            thread::sleep(Duration::from_millis(5));
        }
        self.0.take().unwrap().wait_with_output().unwrap()
    }
    fn address(&mut self) -> String {
        let stderr = self.0.as_mut().unwrap().stderr.take().unwrap();
        let (send, receive) = mpsc::channel();
        thread::spawn(move || {
            for line in BufReader::new(stderr).lines() {
                if send.send(line.unwrap()).is_err() {
                    break;
                }
            }
        });
        let line = receive
            .recv_timeout(WAIT)
            .expect("HTTP listener announcement");
        line.strip_prefix("Listening on ").expect(&line).to_owned()
    }
}
impl Drop for Process {
    fn drop(&mut self) {
        if let Some(child) = &mut self.0 {
            let _ = child.kill();
            let _ = child.wait();
        }
    }
}

fn config(root: &Path, protocol: Protocol, url: &str) -> PathBuf {
    let value = json!({
        "protocol":protocol.name(),"base_url":url,"api_key":"FAKE_KEY_SENTINEL",
        "assistant":{"model":"fixture-assistant"},"reviewer":{"model":"fixture-reviewer","effort":"low","max_output_tokens":1000},
        "tool_cwd":"tools","store_root":"sessions","timeout_ms":2000
    });
    fs::create_dir(root.join("tools")).unwrap();
    fs::create_dir(root.join("configuration")).unwrap();
    let path = root.join("configuration/config.json");
    fs::write(&path, value.to_string()).unwrap();
    path
}
fn cli(root: &Path, config: &Path, input: &str) -> Output {
    let mut process = Process::spawn(
        root,
        &["cli".as_ref(), "--config".as_ref(), config.as_os_str()],
    );
    process.input(input);
    process.finish()
}
fn lines(output: &Output) -> Vec<Value> {
    String::from_utf8(output.stdout.clone())
        .unwrap()
        .lines()
        .map(|line| serde_json::from_str(line).unwrap())
        .collect()
}
fn assert_code(output: &Output, code: i32) {
    assert_eq!(
        output.status.code(),
        Some(code),
        "stdout={} stderr={}",
        String::from_utf8_lossy(&output.stdout),
        String::from_utf8_lossy(&output.stderr)
    );
}
fn http(
    client: &reqwest::blocking::Client,
    url: &str,
    data: Option<Value>,
    expected: u16,
) -> Value {
    let request = match data {
        Some(data) => client.post(url).json(&data),
        None => client.get(url),
    };
    let response = request.send().unwrap();
    assert_eq!(response.status().as_u16(), expected);
    response.json().unwrap()
}
fn facts(request: &Value) -> Value {
    let instructions = request["instructions"]
        .as_str()
        .or_else(|| request["system"].as_str())
        .unwrap();
    let json = instructions
        .split("Review facts:\n")
        .nth(1)
        .unwrap()
        .lines()
        .next()
        .unwrap();
    serde_json::from_str(json).unwrap()
}
fn exercise(protocol: Protocol, web: bool) {
    let directory = tempfile::tempdir().unwrap();
    let root = directory.path();
    let provider = Provider::new(protocol);
    let path = config(root, protocol, &provider.url);
    fs::write(root.join("tools/seed.txt"), "fixture-input").unwrap();
    let mut server = None;
    let (id, first, second) = if web {
        let mut process = Process::spawn(
            root,
            &[
                "web".as_ref(),
                "--config".as_ref(),
                path.as_os_str(),
                "--bind".as_ref(),
                "127.0.0.1:0".as_ref(),
            ],
        );
        let address = process.address();
        let client = reqwest::blocking::Client::builder()
            .timeout(WAIT)
            .no_proxy()
            .build()
            .unwrap();
        let created = http(
            &client,
            &format!("{address}/sessions"),
            Some(json!({})),
            201,
        );
        let id = created["session"]["id"].as_str().unwrap().to_owned();
        let route = format!("{address}/sessions/{id}/messages");
        let first = http(
            &client,
            &route,
            Some(
                json!({"text":"Read seed, write created, inspect it with bash. Do not write denied.txt."}),
            ),
            200,
        );
        let second = http(
            &client,
            &route,
            Some(json!({"text":"Continue this session."})),
            200,
        );
        let listed = http(&client, &format!("{address}/sessions"), None, 200);
        assert_eq!(listed["sessions"].as_array().unwrap().len(), 1);
        server = Some(process);
        (id, first, second)
    } else {
        let output = cli(
            root,
            &path,
            "/new\nRead seed, write created, inspect it with bash. Do not write denied.txt.\nContinue this session.\n/exit\n",
        );
        assert_code(&output, 0);
        assert!(output.stderr.is_empty());
        let replies = lines(&output);
        assert_eq!(replies.len(), 3);
        assert_eq!(replies[0]["kind"], "created");
        assert_eq!(replies[1]["kind"], "chatted");
        (
            replies[0]["session"]["id"].as_str().unwrap().to_owned(),
            replies[1].clone(),
            replies[2].clone(),
        )
    };
    assert_eq!(
        first["messages"].as_array().unwrap().last().unwrap()["text"],
        "first turn complete"
    );
    assert_eq!(
        second["messages"].as_array().unwrap().last().unwrap()["text"],
        "second turn complete"
    );
    assert_eq!(
        fs::read_to_string(root.join("tools/created.txt")).unwrap(),
        "fixture-output"
    );
    assert!(!root.join("tools/denied.txt").exists());
    let store = JsonlSessionStore::new(root.join("sessions"));
    let document = store.inspect(&SessionId::new(&id).unwrap()).unwrap();
    let session = document.session();
    assert_eq!(session.turns().len(), 2);
    let persisted = fs::read_dir(root.join("sessions"))
        .unwrap()
        .map(|entry| entry.unwrap().path())
        .find(|path| {
            path.extension()
                .is_some_and(|extension| extension == "jsonl")
        })
        .unwrap();
    let content = fs::read_to_string(persisted).unwrap();
    for line in content.lines() {
        let _: Value = serde_json::from_str(line).unwrap();
    }
    assert!(content.contains("fixture-output") && content.contains("second turn complete"));
    let requests = provider.requests();
    assert_eq!(requests.len(), 7);
    let route = match protocol {
        Protocol::OpenAi => "POST /v1/responses HTTP/1.1",
        Protocol::Anthropic => "POST /v1/messages HTTP/1.1",
    };
    assert!(requests.iter().all(|(line, _)| line == route));
    assert_eq!(requests[0].1["model"], "fixture-assistant");
    assert_eq!(
        requests[0].1["max_output_tokens"]
            .as_u64()
            .or_else(|| requests[0].1["max_tokens"].as_u64()),
        Some(4096)
    );
    for (index, (_, request)) in requests[1..5].iter().enumerate() {
        assert_eq!(request["model"], "fixture-reviewer");
        assert!(request.get("tools").is_none());
        let facts = facts(request);
        assert_eq!(facts["active_turn"]["id"], 1);
        assert_eq!(
            facts["active_turn"]["user"],
            "Read seed, write created, inspect it with bash. Do not write denied.txt."
        );
        let step = &facts["active_turn"]["steps"][0];
        let calls = step["assistant"]["calls"].as_array().unwrap();
        assert_eq!(calls.len(), 4);
        assert_eq!(
            calls[3]["arguments"],
            json!({"path":"denied.txt","content":"must not exist"})
        );
        assert_eq!(facts["candidate"], calls[index]);
        let executions = step["executions"].as_array().unwrap();
        assert_eq!(executions[index]["state"]["kind"], "started");
        assert!(executions[index]["resolution"].is_null());
        for execution in &executions[index + 1..] {
            assert_eq!(execution["state"]["kind"], "not_started");
        }
        if index > 0 {
            assert_eq!(
                executions[0]["state"]["outcome"],
                json!({"kind":"success","text":"fixture-input"})
            );
        }
        if index > 1 {
            assert_eq!(executions[1]["state"]["kind"], "returned");
        }
        if index > 2 {
            assert!(
                executions[2]["state"]["outcome"]["text"]
                    .as_str()
                    .unwrap()
                    .contains("fixture-output")
            );
        }
    }
    let continued = requests[6].1.to_string();
    for expected in [
        "native-0",
        "native-3",
        "fixture-input",
        "fixture-output",
        "first turn complete",
        "Continue this session.",
        "denied by review",
    ] {
        assert!(
            continued.contains(expected),
            "missing {expected}: {continued}"
        );
    }
    drop(server);
    let restarted = cli(root, &path, &format!("/use {id}\nAfter restart\n/exit\n"));
    assert_code(&restarted, 0);
    assert_eq!(lines(&restarted)[1]["kind"], "chatted");
    assert_eq!(provider.requests().len(), 8);
    assert_eq!(
        store
            .inspect(&SessionId::new(&id).unwrap())
            .unwrap()
            .session()
            .turns()
            .len(),
        3
    );
    if web {
        let mut process = Process::spawn(
            root,
            &[
                "web".as_ref(),
                "--config".as_ref(),
                path.as_os_str(),
                "--bind".as_ref(),
                "127.0.0.1:0".as_ref(),
            ],
        );
        let address = process.address();
        let client = reqwest::blocking::Client::builder()
            .timeout(WAIT)
            .no_proxy()
            .build()
            .unwrap();
        let response = http(
            &client,
            &format!("{address}/sessions/{id}/messages"),
            Some(json!({"text":"After Web restart"})),
            200,
        );
        assert_eq!(
            response["messages"].as_array().unwrap().last().unwrap()["text"],
            "second turn complete"
        );
        assert_eq!(
            store
                .inspect(&SessionId::new(&id).unwrap())
                .unwrap()
                .session()
                .turns()
                .len(),
            4
        );
    }
}

#[test]
fn openai_cli() {
    exercise(Protocol::OpenAi, false);
}
#[test]
fn openai_http() {
    exercise(Protocol::OpenAi, true);
}
#[test]
fn anthropic_cli() {
    exercise(Protocol::Anthropic, false);
}
#[test]
fn anthropic_http() {
    exercise(Protocol::Anthropic, true);
}

#[test]
fn usage_help_and_exit_codes() {
    let root = tempfile::tempdir().unwrap();
    for args in [vec!["--help"], vec!["cli", "--help"], vec!["web", "--help"]] {
        let args: Vec<_> = args.iter().map(AsRef::as_ref).collect();
        let output = Process::spawn(root.path(), &args).finish();
        assert_code(&output, 0);
        assert!(String::from_utf8_lossy(&output.stdout).contains("--config"));
    }
    for args in [
        vec![],
        vec!["cli"],
        vec!["nope"],
        vec!["cli", "--config"],
        vec!["cli", "--config", "x", "--bind", "127.0.0.1:0"],
        vec!["web", "--config", "x", "--bind", "invalid"],
        vec!["web", "--config", "x", "--config", "y"],
        vec![
            "web",
            "--config",
            "x",
            "--bind",
            "127.0.0.1:0",
            "--bind",
            "127.0.0.1:0",
        ],
    ] {
        let args: Vec<_> = args.iter().map(AsRef::as_ref).collect();
        assert_code(&Process::spawn(root.path(), &args).finish(), 2);
    }
    let path = config(root.path(), Protocol::OpenAi, "http://127.0.0.1:1");
    assert_code(&cli(root.path(), &path, "/invalid\n/exit\n"), 2);
    assert_code(&cli(root.path(), &path, "/new\nhello\n/exit\n"), 1);
    assert_code(&cli(root.path(), &root.path().join("missing"), ""), 1);
}

#[test]
fn invalid_configuration_is_strict_and_secret_safe() {
    let root = tempfile::tempdir().unwrap();
    let path = config(root.path(), Protocol::OpenAi, "http://127.0.0.1:1");
    let valid: Value = serde_json::from_slice(&fs::read(&path).unwrap()).unwrap();
    let cases = [
        ("protocol", json!("SECRET_PROTOCOL")),
        ("protocol", json!({"openai-responses":null})),
        ("api_key", json!("")),
        ("api_key", json!("SECRET_HEADER\n")),
        (
            "base_url",
            json!("https://SECRET_USER:SECRET_PASSWORD@invalid.example"),
        ),
        ("base_url", json!("not a SECRET_URL")),
        ("timeout_ms", json!(0)),
        ("timeout_ms", json!(18446744073709551616_u128.to_string())),
        ("max_response_bytes", json!(0)),
        ("max_response_bytes", json!(usize::MAX)),
        ("max_iterations", json!(0)),
        ("max_iterations", json!(4294967296_u64)),
        ("assistant", json!({"model":" "})),
        ("reviewer", json!({"model":"x","effort":"SECRET_EFFORT"})),
        ("reviewer", json!({"model":"x","max_output_tokens":0})),
        (
            "assistant",
            json!({"model":"x","max_output_tokens":4294967296_u64}),
        ),
        (
            "reviewer",
            json!({"model":"x","SECRET_FIELD":"SECRET_VALUE"}),
        ),
        ("SECRET_FIELD", json!("SECRET_VALUE")),
    ];
    for (field, value) in cases {
        let mut invalid = valid.clone();
        invalid[field] = value;
        fs::write(&path, invalid.to_string()).unwrap();
        let output = cli(root.path(), &path, "");
        assert_code(&output, 2);
        let stderr = String::from_utf8_lossy(&output.stderr);
        assert!(
            !stderr.contains("SECRET") && !stderr.contains("FAKE_KEY_SENTINEL"),
            "{stderr}"
        );
        assert!(output.stdout.is_empty());
    }
    for field in [
        "protocol",
        "base_url",
        "api_key",
        "assistant",
        "reviewer",
        "tool_cwd",
        "store_root",
    ] {
        let mut invalid = valid.clone();
        invalid.as_object_mut().unwrap().remove(field);
        fs::write(&path, invalid.to_string()).unwrap();
        assert_code(&cli(root.path(), &path, ""), 2);
    }
    for malformed in [
        r#"{"api_key":"SECRET_VALUE", "SECRET_BROKEN" "SECRET_OTHER"}"#.to_owned(),
        valid
            .to_string()
            .replacen("{", r#"{"api_key":"SECRET_DUPLICATE","#, 1),
        valid
            .to_string()
            .replacen("{", r#"{"timeout_ms":18446744073709551616,"#, 1),
        valid
            .to_string()
            .replacen("{", r#"{"timeout_ms":null,"timeout_ms":1,"#, 1),
        valid.to_string().replace(
            r#""model":"fixture-assistant""#,
            r#""model":"fixture-assistant","model":"SECRET_DUPLICATE""#,
        ),
    ] {
        fs::write(&path, malformed).unwrap();
        let output = cli(root.path(), &path, "");
        assert_code(&output, 2);
        assert!(!String::from_utf8_lossy(&output.stderr).contains("SECRET"));
    }
}

#[test]
fn paths_lazy_store_and_setup_failures() {
    let root = tempfile::tempdir().unwrap();
    let path = config(root.path(), Protocol::OpenAi, "http://127.0.0.1:1");
    let relative = Path::new("configuration/config.json");
    assert_code(&cli(root.path(), relative, "/exit\n"), 0);
    assert!(!root.path().join("sessions").exists());
    fs::write(root.path().join("sessions"), "a file").unwrap();
    assert_code(&cli(root.path(), relative, "/exit\n"), 0);
    let output = cli(root.path(), relative, "/new\n/exit\n");
    assert_code(&output, 1);
    assert_eq!(lines(&output)[0]["error"]["code"], "store_failure");
    fs::remove_dir(root.path().join("tools")).unwrap();
    let output = cli(root.path(), &path, "");
    assert_code(&output, 1);
    assert!(String::from_utf8_lossy(&output.stderr).contains("tool cwd"));
    fs::create_dir(root.path().join("tools")).unwrap();
    let listener = TcpListener::bind("127.0.0.1:0").unwrap();
    let bind = listener.local_addr().unwrap().to_string();
    let output = Process::spawn(
        root.path(),
        &[
            "web".as_ref(),
            "--config".as_ref(),
            path.as_os_str(),
            "--bind".as_ref(),
            bind.as_ref(),
        ],
    )
    .finish();
    assert_code(&output, 1);
    assert!(String::from_utf8_lossy(&output.stderr).contains("HTTP bind"));
}

#[cfg(unix)]
#[test]
fn os_config_paths() {
    use std::os::unix::ffi::OsStringExt;
    let root = tempfile::tempdir().unwrap();
    let path = config(root.path(), Protocol::OpenAi, "http://127.0.0.1:1");
    let spaced = root.path().join("config with spaces.json");
    fs::rename(path, &spaced).unwrap();
    assert_code(&cli(root.path(), &spaced, "/exit\n"), 0);
    let non_utf8 = root
        .path()
        .join(std::ffi::OsString::from_vec(b"config-\xff.json".to_vec()));
    #[cfg(not(target_os = "macos"))]
    {
        fs::rename(spaced, &non_utf8).unwrap();
        assert_code(&cli(root.path(), &non_utf8, "/exit\n"), 0);
    }
    #[cfg(target_os = "macos")]
    {
        let output = cli(root.path(), &non_utf8, "");
        assert_code(&output, 1);
        assert!(String::from_utf8_lossy(&output.stderr).contains("config read"));
    }
}
