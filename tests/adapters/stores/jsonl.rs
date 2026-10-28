use cola::{adapters::stores::JsonlSessionStore, app::*, domain::*};
use serde_json::{Value, json};
use sha2::{Digest, Sha256};
use std::{
    fs,
    num::{NonZeroU32, NonZeroU64, NonZeroUsize},
    path::{Path, PathBuf},
    sync::{
        Arc,
        atomic::{AtomicBool, Ordering},
    },
};

fn text(value: &str) -> NonEmptyText {
    NonEmptyText::new(value).unwrap()
}
fn archive(id: &str) -> SessionArchive {
    SessionArchive::new(SessionId::new(id).unwrap())
}
fn path(root: &Path, id: &str) -> PathBuf {
    root.join(format!("{:x}.jsonl", Sha256::digest(id.as_bytes())))
}
fn settings() -> ModelSettings {
    ModelSettings {
        model_id: ModelId::new("fixture-model").unwrap(),
        effort: Effort::High,
        max_output_tokens: NonZeroU32::new(10).unwrap(),
    }
}
fn limits() -> ContextLimits {
    ContextLimits {
        input_tokens: NonZeroU64::new(100_000).unwrap(),
        request_bytes: NonZeroUsize::new(300_000).unwrap(),
    }
}
fn prepare(archive: &SessionArchive) -> PreparedRequest<AssistantMessage> {
    let plan =
        ContextPlan::assistant(archive, text("Exact instructions"), vec![], limits(), &[]).unwrap();
    PreparedRequest::new(
        ModelTarget::new("fixture", "memory"),
        settings(),
        plan.fit(|_| json!({"prompt":"  exact user  ","future_field":[1,2]}))
            .unwrap(),
    )
}
fn start(archive: &mut SessionArchive) -> TurnId {
    archive
        .session_mut()
        .start_turn(UserMessage::new(text("  exact user  ")))
        .unwrap()
}
fn begin(archive: &mut SessionArchive, turn: TurnId) -> GenerationId {
    let request = prepare(archive);
    archive
        .begin_generation(InvocationRole::Assistant { turn }, &request)
        .unwrap()
}
fn receipt() -> ResponseReceipt {
    ResponseReceipt {
        http_status: Some(200),
        served_model: Some("served".into()),
        response_id: Some("response\0名".into()),
        request_id: Some("request".into()),
        finish: Some("done".into()),
        usage: Some(Usage {
            input_tokens: Some(2),
            output_tokens: None,
        }),
        capture: ResponseCapture::Complete(
            br#"{"future_field":{"opaque":true},"call_id":"same"}"#.to_vec(),
        ),
    }
}
fn accept(archive: &mut SessionArchive, turn: TurnId, message: AssistantMessage) -> GenerationId {
    let generation = begin(archive, turn);
    archive
        .record_observation(
            generation,
            Timestamp::now(),
            receipt(),
            Disposition::Accepted,
        )
        .unwrap();
    archive
        .session_mut()
        .record_assistant(turn, generation, message)
        .unwrap();
    generation
}
fn review(archive: &mut SessionArchive, turn: TurnId, call: &CallRef, allow: bool) -> GenerationId {
    let plan = ContextPlan::<Decision>::reviewer(
        archive,
        call,
        text("Review exactly"),
        vec![],
        limits(),
        &[],
    )
    .unwrap();
    let request: PreparedRequest<Decision> = PreparedRequest::new(
        ModelTarget::new("fixture", "memory"),
        settings(),
        plan.fit(|_| json!({"review":"facts"})).unwrap(),
    );
    let id = archive
        .begin_generation(InvocationRole::Reviewer { call: call.clone() }, &request)
        .unwrap();
    archive
        .record_observation(id, Timestamp::now(), receipt(), Disposition::Accepted)
        .unwrap();
    archive
        .session_mut()
        .review_tool(turn, call, id, &Decision::new(allow, Some(text("reason"))))
        .unwrap();
    id
}
fn batch(ids: &[&str]) -> AssistantMessage {
    AssistantMessage::tool_requests(
        Some("thinking".into()),
        ToolCalls::new(
            ids.iter()
                .map(|id| {
                    ToolCall::new(
                        ToolCallId::new(*id).unwrap(),
                        ToolName::new("work").unwrap(),
                        json!({"tiny":5.757930830206806e-61,"opaque":"会話\u{0000}"}),
                    )
                    .unwrap()
                })
                .collect(),
        )
        .unwrap(),
    )
}
fn lines(bytes: &[u8]) -> Vec<Value> {
    bytes
        .split(|byte| *byte == b'\n')
        .filter(|line| !line.is_empty())
        .map(|line| serde_json::from_slice(line).unwrap())
        .collect()
}
fn bytes(lines: &[Value]) -> Vec<u8> {
    lines
        .iter()
        .flat_map(|line| {
            let mut bytes = serde_json::to_vec(line).unwrap();
            bytes.push(b'\n');
            bytes
        })
        .collect()
}

#[test]
fn closed_roundtrips_every_fact_and_variant() {
    let dir = tempfile::tempdir().unwrap();
    let store = JsonlSessionStore::new(dir.path());
    let mut a = archive("all-facts");
    let t = start(&mut a);
    let source = accept(
        &mut a,
        t,
        batch(&["denied", "success", "failure", "unknown", "later"]),
    );
    let refs: Vec<_> = ["denied", "success", "failure", "unknown"]
        .iter()
        .map(|id| CallRef::new(source, ToolCallId::new(*id).unwrap()))
        .collect();
    review(&mut a, t, &refs[0], false);
    for (index, call) in refs.iter().enumerate().skip(1) {
        review(&mut a, t, call, true);
        let at = Timestamp::now();
        a.session_mut().start_tool(t, call, at).unwrap();
        if index != 3 {
            let outcome = if index == 1 {
                ToolOutcome::Success(String::new())
            } else {
                ToolOutcome::Failure(ToolError::new(text("failed full description")))
            };
            a.session_mut()
                .record_tool_result(t, call, outcome, Timestamp::now())
                .unwrap();
        }
    }
    a.session_mut()
        .interrupt(t, StopReason::ProcessLost)
        .unwrap();
    a.session_mut()
        .resolve_effect(
            t,
            &refs[3],
            EffectResolution::new(
                EffectDisposition::Completed,
                ResolutionSource::Observation,
                text("observed file"),
            ),
        )
        .unwrap();
    for disposition in [
        Disposition::Refused {
            reason: Some(text("refused")),
        },
        Disposition::Refused { reason: None },
        Disposition::Incomplete(IncompleteReason::OutputLimit),
        Disposition::Incomplete(IncompleteReason::Other),
        Disposition::Rejected(text("unoffered call")),
        Disposition::Failed(ModelError::http(ModelErrorKind::RateLimited, 429)),
        Disposition::Failed(ModelError::new(ModelErrorKind::Timeout)),
    ] {
        let t = start(&mut a);
        let id = begin(&mut a, t);
        a.record_observation(id, Timestamp::now(), receipt(), disposition)
            .unwrap();
        a.session_mut()
            .fail(t, TurnFailure::ModelFailure(text("stopped")))
            .unwrap();
    }
    let t = start(&mut a);
    begin(&mut a, t);
    a.abandon_pending(Timestamp::now());
    a.session_mut()
        .interrupt(t, StopReason::ProcessLost)
        .unwrap();
    let t = start(&mut a);
    let source = accept(&mut a, t, batch(&["denied"]));
    let call = CallRef::new(source, ToolCallId::new("denied").unwrap());
    review(&mut a, t, &call, false);
    accept(&mut a, t, AssistantMessage::answer("answer").unwrap());
    let change = ArchiveChange::initial(a).unwrap();
    store.create(&change).unwrap();
    assert_eq!(
        store.inspect(change.candidate().session().id()).unwrap(),
        *change.candidate()
    );
    assert_eq!(store.list().unwrap(), vec![change.candidate().clone()]);
    assert_eq!(
        SessionArchive::decode(change.encoded())
            .unwrap()
            .encode()
            .unwrap(),
        change.encoded()
    );
    let header = &lines(change.encoded())[0];
    assert_eq!(header["record"], "session");
    assert!(header.get("version").is_none());
    assert!(header.get("format").is_none());
}

#[test]
fn active_reads_preserve_bytes_and_explicit_recovery_closes_once() {
    let dir = tempfile::tempdir().unwrap();
    let store = JsonlSessionStore::new(dir.path());
    let mut a = archive("active");
    let t = start(&mut a);
    let source = accept(&mut a, t, batch(&["unknown", "later"]));
    let call = CallRef::new(source, ToolCallId::new("unknown").unwrap());
    review(&mut a, t, &call, true);
    a.session_mut()
        .start_tool(t, &call, Timestamp::now())
        .unwrap();
    let change = ArchiveChange::initial(a).unwrap();
    store.create(&change).unwrap();
    let p = path(dir.path(), "active");
    let original = fs::read(&p).unwrap();
    for _ in 0..3 {
        assert_eq!(
            store.inspect(change.candidate().session().id()).unwrap(),
            *change.candidate()
        );
        assert_eq!(store.list().unwrap(), vec![change.candidate().clone()]);
        assert_eq!(fs::read(&p).unwrap(), original);
    }
    let mut tx = store.acquire(change.candidate().session().id()).unwrap();
    let base = tx.confirmed().clone();
    let mut recovered = base.clone();
    recovered.abandon_pending(Timestamp::now());
    recovered
        .session_mut()
        .interrupt(t, StopReason::ProcessLost)
        .unwrap();
    let recovery = ArchiveChange::next(&base, recovered).unwrap();
    tx.commit(&recovery).unwrap();
    assert!(!tx.confirmed().needs_recovery());
    assert_eq!(
        tx.confirmed().session().turns()[0].steps()[0]
            .executions()
            .next()
            .unwrap()
            .state(),
        change.candidate().session().active_turn().unwrap().steps()[0]
            .executions()
            .next()
            .unwrap()
            .state()
    );
}

#[test]
fn opaque_ids_sort_distinctly_and_unrelated_files_are_ignored() {
    let dir = tempfile::tempdir().unwrap();
    let root = dir.path().join("missing");
    let store = JsonlSessionStore::new(&root);
    assert!(store.list().unwrap().is_empty());
    assert!(!root.exists());
    assert_eq!(
        store
            .inspect(archive("x").session().id())
            .unwrap_err()
            .kind(),
        SessionStoreErrorKind::NotFound
    );
    let mut ids = vec![
        "../a/会話\0".to_string(),
        "x".repeat(10000),
        "A".into(),
        "a".into(),
        "é".into(),
        "e\u{301}".into(),
    ];
    for id in &ids {
        let change = ArchiveChange::initial(archive(id)).unwrap();
        store.create(&change).unwrap();
        assert_eq!(
            store.inspect(change.candidate().session().id()).unwrap(),
            *change.candidate()
        );
    }
    for name in [
        ".cola-writing.tmp",
        "notes.jsonl",
        &format!("{}.jsonl", "A".repeat(64)),
    ] {
        fs::write(root.join(name), b"junk").unwrap();
    }
    ids.sort();
    assert_eq!(
        store
            .list()
            .unwrap()
            .iter()
            .map(|a| a.session().id().as_str().to_string())
            .collect::<Vec<_>>(),
        ids
    );
    let file = dir.path().join("file");
    fs::write(&file, b"x").unwrap();
    let broken = JsonlSessionStore::new(file);
    assert_eq!(
        broken.list().unwrap_err().kind(),
        SessionStoreErrorKind::Storage
    );
    assert!(matches!(
        broken.create(&ArchiveChange::initial(archive("x")).unwrap()),
        Err(CommitFault::NotPublished(_))
    ));
}

#[test]
fn rejects_corruption_and_preserves_bytes_on_refused_commit() {
    let dir = tempfile::tempdir().unwrap();
    let store = JsonlSessionStore::new(dir.path());
    let mut a = archive("corrupt");
    let t = start(&mut a);
    accept(&mut a, t, batch(&["a"]));
    a.session_mut()
        .interrupt(t, StopReason::UserCancelled)
        .unwrap();
    let original_change = ArchiveChange::initial(a).unwrap();
    store.create(&original_change).unwrap();
    let p = path(dir.path(), "corrupt");
    let original = fs::read(&p).unwrap();
    let base = lines(&original);
    let mut cases = vec![
        b"garbage\n".to_vec(),
        original[..original.len() - 1].to_vec(),
        bytes(&base[..1]),
        b"{\"format\":\"cola.session\",\"version\":1}\n".to_vec(),
    ];
    let mutations: Vec<fn(&mut Vec<Value>)> = vec![
        |v| v[0]["extra"] = json!(0),
        |v| v[0]["revision"] = json!(0),
        |v| v[0]["closed_turns"] = json!(0),
        |v| v[0]["active"] = json!(true),
        |v| v[1]["turn"]["extra"] = json!(0),
        |v| v[1]["turn"]["steps"][0]["assistant"]["extra"] = json!(0),
        |v| v[1]["turn"]["steps"][0]["executions"][0]["state"]["extra"] = json!(0),
        |v| v[1]["turn"]["steps"][0]["source_generation"] = json!(99),
        |v| v[1]["turn"]["steps"][0]["executions"][0]["call_id"] = json!("wrong"),
        |v| v[1]["turn"]["id"] = json!(0),
        |v| v[1]["turn"]["user"] = json!(" "),
        |v| v[1]["turn"]["outcome"] = Value::Null,
        |v| v[1]["turn"]["steps"][0]["assistant"]["calls"][0]["name"] = json!("bad name"),
        |v| v[2]["generation"]["intent"]["role"]["turn"] = json!(2),
        |v| v[2]["generation"]["id"] = json!(2),
        |v| v[2]["generation"]["observation"]["disposition"]["kind"] = json!("rejected"),
        |v| {
            v[2]["generation"]["observation"]["receipt"]["capture"] =
                json!({"kind":"truncated","prefix":[1],"limit":2})
        },
        |v| v[2]["generation"]["intent"]["budget"]["request_bytes"] = json!(1),
        |v| v.push(v[1].clone()),
    ];
    for mutate in mutations {
        let mut values = base.clone();
        mutate(&mut values);
        cases.push(bytes(&values));
    }
    let mut blank = original.clone();
    blank.push(b'\n');
    cases.push(blank);
    cases.push(
        String::from_utf8(original.clone())
            .unwrap()
            .replace("5.757930830206806e-61", "1e999")
            .into_bytes(),
    );
    let mut tx = store
        .acquire(original_change.candidate().session().id())
        .unwrap();
    let candidate = ArchiveChange::next(tx.confirmed(), tx.confirmed().clone()).unwrap();
    for corrupt in cases {
        fs::write(&p, &corrupt).unwrap();
        assert_eq!(
            store
                .inspect(original_change.candidate().session().id())
                .unwrap_err()
                .kind(),
            SessionStoreErrorKind::InvalidData
        );
        assert_eq!(
            store.list().unwrap_err().kind(),
            SessionStoreErrorKind::InvalidData
        );
        assert!(matches!(
            tx.commit(&candidate),
            Err(CommitFault::NotPublished(_))
        ));
        assert_eq!(fs::read(&p).unwrap(), corrupt);
    }
    fs::write(&p, &original).unwrap();
    fs::rename(&p, dir.path().join(format!("{}.jsonl", "0".repeat(64)))).unwrap();
    assert_eq!(
        store.list().unwrap_err().kind(),
        SessionStoreErrorKind::InvalidData
    );
}

#[test]
fn unreadable_encoding_is_refused_before_publication() {
    let mut a = archive("deep");
    let t = start(&mut a);
    let generation = begin(&mut a, t);
    let mut arguments = json!(null);
    for _ in 0..140 {
        arguments = json!({"next":arguments});
    }
    let call = ToolCall::new(
        ToolCallId::new("deep").unwrap(),
        ToolName::new("work").unwrap(),
        arguments,
    )
    .unwrap();
    a.record_observation(
        generation,
        Timestamp::now(),
        receipt(),
        Disposition::Accepted,
    )
    .unwrap();
    a.session_mut()
        .record_assistant(
            t,
            generation,
            AssistantMessage::tool_requests(None, ToolCalls::new(vec![call]).unwrap()),
        )
        .unwrap();
    assert_eq!(
        ArchiveChange::initial(a).unwrap_err().kind(),
        SessionStoreErrorKind::InvalidData
    );
}

#[test]
fn stable_lock_excludes_writers_creation_never_replaces_and_stale_changes_conflict() {
    let dir = tempfile::tempdir().unwrap();
    let store = JsonlSessionStore::new(dir.path());
    let init = ArchiveChange::initial(archive("owned")).unwrap();
    store.create(&init).unwrap();
    assert!(matches!(store.create(&init), Err(CommitFault::Conflict)));
    let mut tx = store.acquire(init.candidate().session().id()).unwrap();
    assert_eq!(
        store
            .acquire(init.candidate().session().id())
            .err()
            .unwrap()
            .kind(),
        SessionStoreErrorKind::Busy
    );
    let other = ArchiveChange::initial(archive("other")).unwrap();
    store.create(&other).unwrap();
    assert!(store.acquire(other.candidate().session().id()).is_ok());
    let base = tx.confirmed().clone();
    let a = ArchiveChange::next(&base, base.clone()).unwrap();
    let mut different = base.clone();
    start(&mut different);
    let b = ArchiveChange::next(&base, different).unwrap();
    tx.commit(&a).unwrap();
    assert!(matches!(tx.commit(&b), Err(CommitFault::Conflict)));
    assert_eq!(tx.confirmed(), a.candidate());
    drop(tx);
    assert!(store.acquire(init.candidate().session().id()).is_ok());
}

#[test]
fn reconcile_syncs_exact_candidate_preserves_base_and_rejects_unexpected_publication() {
    let dir = tempfile::tempdir().unwrap();
    let store = JsonlSessionStore::new(dir.path());
    let init = ArchiveChange::initial(archive("repair")).unwrap();
    store.create(&init).unwrap();
    let mut tx = store.acquire(init.candidate().session().id()).unwrap();
    let base = tx.confirmed().clone();
    let mut candidate = base.clone();
    start(&mut candidate);
    let change = ArchiveChange::next(&base, candidate).unwrap();
    assert!(matches!(
        tx.resolve(&change),
        Err(CommitFault::NotPublished(_))
    ));
    assert_eq!(tx.confirmed(), &base);
    fs::write(path(dir.path(), "repair"), change.encoded()).unwrap();
    tx.resolve(&change).unwrap();
    assert_eq!(tx.confirmed(), change.candidate());
    drop(tx);
    let mut tx = store.acquire(init.candidate().session().id()).unwrap();
    let next = ArchiveChange::next(tx.confirmed(), tx.confirmed().clone()).unwrap();
    fs::write(path(dir.path(), "repair"), init.encoded()).unwrap();
    assert!(matches!(tx.resolve(&next), Err(CommitFault::Conflict)));
    assert!(matches!(tx.commit(&next), Err(CommitFault::Conflict)));
    assert_eq!(
        fs::read(path(dir.path(), "repair")).unwrap(),
        init.encoded()
    );
}

#[test]
fn concurrent_publication_exposes_only_complete_snapshots() {
    let dir = tempfile::tempdir().unwrap();
    let store = Arc::new(JsonlSessionStore::new(dir.path()));
    let initial = ArchiveChange::initial(archive("shared")).unwrap();
    store.create(&initial).unwrap();
    let running = Arc::new(AtomicBool::new(true));
    std::thread::scope(|scope| {
        let read_store = store.clone();
        let flag = running.clone();
        scope.spawn(move || {
            while flag.load(Ordering::Acquire) {
                let loaded = read_store
                    .inspect(&SessionId::new("shared").unwrap())
                    .unwrap();
                assert!(loaded.revision().get() >= 1);
                read_store.list().unwrap();
            }
        });
        let mut tx = store.acquire(&SessionId::new("shared").unwrap()).unwrap();
        for _ in 0..30 {
            let change = ArchiveChange::next(tx.confirmed(), tx.confirmed().clone()).unwrap();
            tx.commit(&change).unwrap();
        }
        running.store(false, Ordering::Release);
    });
    assert_eq!(
        store
            .inspect(initial.candidate().session().id())
            .unwrap()
            .revision()
            .get(),
        31
    );
}

#[test]
fn corrupt_records_report_path_ordinal_and_original_json_error() {
    use std::error::Error;
    let dir = tempfile::tempdir().unwrap();
    let store = JsonlSessionStore::new(dir.path());
    let change = ArchiveChange::initial(archive("context")).unwrap();
    store.create(&change).unwrap();
    let p = path(dir.path(), "context");
    fs::write(&p,b"{\"record\":\"session\",\"session_id\":\"context\",\"revision\":1,\"closed_turns\":1,\"active\":false,\"generations\":0}\n{broken}\n").unwrap();
    for error in [
        store
            .inspect(change.candidate().session().id())
            .unwrap_err(),
        store.list().unwrap_err(),
    ] {
        assert!(error.to_string().contains(p.to_str().unwrap()));
        assert!(error.to_string().contains("record 2"));
        let mut source = error.source();
        let mut found = false;
        while let Some(error) = source {
            found |= error.downcast_ref::<serde_json::Error>().is_some();
            source = error.source();
        }
        assert!(found);
    }
}

#[test]
fn killed_owner_releases_os_lock_for_fresh_acquire() {
    use std::{
        process::{Command, Stdio},
        time::{Duration, Instant},
    };
    const STATE: &str = "COLA_ARCHIVE_LOCK_CHILD";
    if let Ok(root) = std::env::var(STATE) {
        let store = JsonlSessionStore::new(&root);
        let _tx = store.acquire(&SessionId::new("process").unwrap()).unwrap();
        fs::write(Path::new(&root).join("ready"), b"locked").unwrap();
        loop {
            std::thread::park();
        }
    }
    let dir = tempfile::tempdir().unwrap();
    let store = JsonlSessionStore::new(dir.path());
    let change = ArchiveChange::initial(archive("process")).unwrap();
    store.create(&change).unwrap();
    let mut child = Command::new(std::env::current_exe().unwrap())
        .args([
            "killed_owner_releases_os_lock_for_fresh_acquire",
            "--nocapture",
        ])
        .env(STATE, dir.path())
        .stdout(Stdio::null())
        .stderr(Stdio::null())
        .spawn()
        .unwrap();
    let deadline = Instant::now() + Duration::from_secs(10);
    while !dir.path().join("ready").exists() {
        assert!(Instant::now() < deadline);
        assert!(child.try_wait().unwrap().is_none());
        std::thread::sleep(Duration::from_millis(5));
    }
    assert_eq!(
        store
            .acquire(change.candidate().session().id())
            .err()
            .unwrap()
            .kind(),
        SessionStoreErrorKind::Busy
    );
    child.kill().unwrap();
    child.wait().unwrap();
    assert_eq!(
        store
            .acquire(change.candidate().session().id())
            .unwrap()
            .confirmed(),
        change.candidate()
    );
}
