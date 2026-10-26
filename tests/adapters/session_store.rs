use cola::{
    adapters::stores::JsonlSessionStore,
    app::{SessionStore, SessionStoreErrorKind},
    domain::*,
};
use serde_json::{Value, json};
use std::{
    fs,
    path::{Path, PathBuf},
    sync::{
        Arc,
        atomic::{AtomicBool, Ordering},
    },
};
fn text(s: &str) -> NonEmptyText {
    NonEmptyText::new(s).unwrap()
}
fn session(id: &str) -> Session {
    Session::new(SessionId::new(id).unwrap())
}
fn user() -> UserMessage {
    UserMessage::new(text(" \nuser\t "))
}
fn call(id: &str) -> ToolCall {
    ToolCall::new(
        ToolCallId::new(id).unwrap(),
        ToolName::new("work").unwrap(),
        json!({"nested":[null,true,{"float":5.757930830206806e-61,"text":" \n\t "}],"integer":u64::MAX}),
    )
    .unwrap()
}
fn batch(ids: &[&str]) -> AssistantMessage {
    AssistantMessage::tool_requests(
        Some(" \npreamble ".into()),
        ToolCalls::new(ids.iter().map(|id| call(id)).collect()).unwrap(),
    )
}
fn finish(s: &mut Session, t: TurnId, id: &str, outcome: ToolOutcome) {
    let id = ToolCallId::new(id).unwrap();
    s.start_tool(t, &id).unwrap();
    s.record_tool_result(t, ToolResult::new(id, outcome))
        .unwrap();
}
fn path(root: &Path) -> PathBuf {
    fs::read_dir(root)
        .unwrap()
        .map(|e| e.unwrap().path())
        .find(|p| p.extension().is_some_and(|e| e == "jsonl"))
        .unwrap()
}
fn lines(bytes: &[u8]) -> Vec<Value> {
    String::from_utf8(bytes.into())
        .unwrap()
        .lines()
        .map(|l| serde_json::from_str(l).unwrap())
        .collect()
}
fn bytes(lines: &[Value]) -> Vec<u8> {
    lines
        .iter()
        .map(|l| format!("{l}\n"))
        .collect::<String>()
        .into_bytes()
}
#[test]
fn closed_roundtrips_every_fact_and_variant() {
    let dir = tempfile::tempdir().unwrap();
    let store = JsonlSessionStore::new(dir.path());
    let port: &dyn SessionStore = &store;
    let mut s = session("full");
    let t = s.start_turn(user()).unwrap();
    for (id, outcome) in [
        ("a", ToolOutcome::Success(String::new())),
        (
            "b",
            ToolOutcome::Failure(ToolError::new(text(" \nfailed "))),
        ),
    ] {
        s.record_assistant(t, batch(&[id])).unwrap();
        finish(&mut s, t, id, outcome);
    }
    s.record_assistant(t, AssistantMessage::answer(" \nanswer ").unwrap())
        .unwrap();
    for (i, reason) in [
        StopReason::UserCancelled,
        StopReason::ProcessLost,
        StopReason::ContextCapacity,
        StopReason::IterationLimit,
    ]
    .into_iter()
    .enumerate()
    {
        let t = s.start_turn(user()).unwrap();
        let ids = [
            format!("{i}-returned"),
            format!("{i}-started"),
            format!("{i}-pending"),
        ];
        s.record_assistant(t, batch(&[&ids[0], &ids[1], &ids[2]]))
            .unwrap();
        finish(&mut s, t, &ids[0], ToolOutcome::Success("\noutput".into()));
        s.start_tool(t, &ToolCallId::new(&ids[1]).unwrap()).unwrap();
        s.interrupt(t, reason).unwrap();
        if i < 3 {
            s.resolve_effect(
                t,
                &ToolCallId::new(&ids[1]).unwrap(),
                EffectResolution::new(
                    if i == 1 {
                        EffectDisposition::NotPerformed
                    } else {
                        EffectDisposition::Completed
                    },
                    [
                        ResolutionSource::Observation,
                        ResolutionSource::UserAttestation,
                        ResolutionSource::ApplicationAttestation,
                    ][i],
                    text(" \nevidence "),
                ),
            )
            .unwrap();
        }
    }
    for failure in [
        TurnFailure::ModelFailure(text("model")),
        TurnFailure::MalformedResponse(text("malformed")),
    ] {
        let t = s.start_turn(user()).unwrap();
        s.fail(t, failure).unwrap();
    }
    port.save(&s).unwrap();
    let first = fs::read(path(dir.path())).unwrap();
    port.save(&s).unwrap();
    assert_eq!(first, fs::read(path(dir.path())).unwrap());
    assert_eq!(port.load(s.id()).unwrap(), s);
    assert_eq!(port.list().unwrap(), vec![s]);
}
#[test]
fn active_recovery_is_read_only_and_retains_dispatch_intent() {
    let dir = tempfile::tempdir().unwrap();
    let store = JsonlSessionStore::new(dir.path());
    let mut s = session("active");
    let t = s.start_turn(user()).unwrap();
    s.record_assistant(t, batch(&["started", "pending"]))
        .unwrap();
    s.start_tool(t, &ToolCallId::new("started").unwrap())
        .unwrap();
    let live = s.snapshot();
    store.save(&s).unwrap();
    let original = fs::read(path(dir.path())).unwrap();
    let recovered = store.load(s.id()).unwrap();
    assert_eq!(s.snapshot(), live);
    assert_eq!(recovered, Session::restore(live).unwrap());
    assert_eq!(
        recovered.turns()[0].outcome(),
        &TurnOutcome::Interrupted(StopReason::ProcessLost)
    );
    assert_eq!(
        recovered.turns()[0].steps()[0]
            .executions()
            .next()
            .unwrap()
            .state(),
        &ExecutionState::Started
    );
    assert_eq!(store.list().unwrap(), vec![recovered.clone()]);
    assert_eq!(fs::read(path(dir.path())).unwrap(), original);
    store.save(&recovered).unwrap();
    assert_eq!(store.load(s.id()).unwrap(), recovered);
}
#[test]
fn opaque_ids_sort_distinctly_and_unrelated_files_are_ignored() {
    let dir = tempfile::tempdir().unwrap();
    let root = dir.path().join("missing");
    let store = JsonlSessionStore::new(&root);
    assert!(store.list().unwrap().is_empty());
    assert!(!root.exists());
    assert_eq!(
        store.load(session("x").id()).unwrap_err().kind(),
        SessionStoreErrorKind::NotFound
    );
    let mut ids = vec![
        "../a/会话\0".to_string(),
        "x".repeat(10000),
        "A".into(),
        "a".into(),
        "é".into(),
        "e\u{301}".into(),
    ];
    for id in &ids {
        let s = session(id);
        store.save(&s).unwrap();
        assert_eq!(store.load(s.id()).unwrap(), s);
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
            .map(|s| s.id().as_str().to_string())
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
    assert_eq!(
        broken.save(&session("x")).unwrap_err().kind(),
        SessionStoreErrorKind::Storage
    );
}
#[test]
fn rejects_corruption_and_preserves_bytes_on_refused_save() {
    let dir = tempfile::tempdir().unwrap();
    let store = JsonlSessionStore::new(dir.path());
    let mut s = session("corrupt");
    let t = s.start_turn(user()).unwrap();
    s.record_assistant(t, batch(&["a"])).unwrap();
    s.interrupt(t, StopReason::UserCancelled).unwrap();
    store.save(&s).unwrap();
    let p = path(dir.path());
    let original = fs::read(&p).unwrap();
    let base = lines(&original);
    let mut cases: Vec<(Vec<u8>, SessionStoreErrorKind)> = vec![
        (b"garbage\n".to_vec(), SessionStoreErrorKind::InvalidData),
        (
            original[..original.len() - 1].into(),
            SessionStoreErrorKind::InvalidData,
        ),
        (bytes(&base[..1]), SessionStoreErrorKind::InvalidData),
    ];
    let mutations: Vec<fn(&mut Vec<Value>)> = vec![
        |v| v[0]["extra"] = json!(0),
        |v| v[1]["turn"]["extra"] = json!(0),
        |v| v[1]["turn"]["steps"][0]["assistant"]["extra"] = json!(0),
        |v| v[1]["turn"]["steps"][0]["executions"][0]["state"]["extra"] = json!(0),
        |v| v[1]["turn"]["steps"][0]["executions"][0]["call_id"] = json!("wrong"),
        |v| v[0]["session_id"] = json!("other"),
        |v| v[0]["active"] = json!(true),
        |v| v[0]["closed_turn_count"] = json!(0),
        |v| v[1]["record"] = json!("active"),
        |v| v[1]["turn"]["id"] = json!(0),
        |v| v[1]["turn"]["user"] = json!(" "),
        |v| v[1]["turn"]["outcome"] = Value::Null,
        |v| v[1]["turn"]["steps"][0]["executions"][0]["state"]["kind"] = json!("bogus"),
        |v| v[1]["turn"]["steps"][0]["assistant"]["calls"][0]["name"] = json!("bad name"),
        |v| v.push(v[1].clone()),
    ];
    for mutate in mutations {
        let mut v = base.clone();
        mutate(&mut v);
        cases.push((bytes(&v), SessionStoreErrorKind::InvalidData));
    }
    let mut v = base.clone();
    v[0]["version"] = json!(2);
    cases.push((bytes(&v), SessionStoreErrorKind::UnsupportedVersion));
    let mut blank = original.clone();
    blank.push(b'\n');
    cases.push((blank, SessionStoreErrorKind::InvalidData));
    cases.push((
        String::from_utf8(original.clone())
            .unwrap()
            .replace("5.757930830206806e-61", "1e999")
            .into_bytes(),
        SessionStoreErrorKind::InvalidData,
    ));
    for (corrupt, kind) in cases {
        fs::write(&p, &corrupt).unwrap();
        assert_eq!(store.load(s.id()).unwrap_err().kind(), kind);
        assert_eq!(store.list().unwrap_err().kind(), kind);
        assert_eq!(store.save(&s).unwrap_err().kind(), kind);
        assert_eq!(fs::read(&p).unwrap(), corrupt);
    }
    fs::write(&p, &original).unwrap();
    let other = dir.path().join(format!("{}.jsonl", "0".repeat(64)));
    fs::rename(&p, &other).unwrap();
    assert_eq!(
        store.list().unwrap_err().kind(),
        SessionStoreErrorKind::InvalidData
    );
}
#[test]
fn unreadable_encoding_is_refused_before_publication() {
    for existing in [false, true] {
        let dir = tempfile::tempdir().unwrap();
        let store = JsonlSessionStore::new(dir.path());
        let previous = session("deep-arguments");
        let original = if existing {
            store.save(&previous).unwrap();
            Some(fs::read(path(dir.path())).unwrap())
        } else {
            None
        };
        let mut arguments = json!(null);
        for _ in 0..140 {
            arguments = json!({"next": arguments});
        }
        let call = ToolCall::new(
            ToolCallId::new("deep").unwrap(),
            ToolName::new("work").unwrap(),
            arguments,
        )
        .unwrap();
        let mut deep = previous.clone();
        let turn = deep.start_turn(user()).unwrap();
        deep.record_assistant(
            turn,
            AssistantMessage::tool_requests(None, ToolCalls::new(vec![call]).unwrap()),
        )
        .unwrap();
        assert_eq!(
            store.save(&deep).unwrap_err().kind(),
            SessionStoreErrorKind::InvalidData,
            "existing target: {existing}"
        );
        if let Some(original) = original {
            assert_eq!(fs::read(path(dir.path())).unwrap(), original);
            assert_eq!(store.load(previous.id()).unwrap(), previous);
            assert_eq!(store.list().unwrap(), vec![previous]);
        } else {
            assert_eq!(
                store.load(previous.id()).unwrap_err().kind(),
                SessionStoreErrorKind::NotFound
            );
            assert!(store.list().unwrap().is_empty());
            assert_eq!(fs::read_dir(dir.path()).unwrap().count(), 0);
        }
    }
}
#[test]
fn concurrent_publication_exposes_complete_sessions() {
    let dir = tempfile::tempdir().unwrap();
    let store = Arc::new(JsonlSessionStore::new(dir.path()));
    let a = session("shared");
    let mut b = a.clone();
    let t = b.start_turn(user()).unwrap();
    b.record_assistant(t, AssistantMessage::answer("complete").unwrap())
        .unwrap();
    store.save(&a).unwrap();
    let running = Arc::new(AtomicBool::new(true));
    std::thread::scope(|scope| {
        let reader_store = store.clone();
        let reader_running = running.clone();
        let a_ref = &a;
        let b_ref = &b;
        scope.spawn(move || {
            while reader_running.load(Ordering::Acquire) {
                let loaded = reader_store.load(a_ref.id()).unwrap();
                assert!(loaded == *a_ref || loaded == *b_ref);
                reader_store.list().unwrap();
            }
        });
        let writers: Vec<_> = (0..4)
            .map(|n| {
                let store = store.clone();
                let s = if n % 2 == 0 { a.clone() } else { b.clone() };
                scope.spawn(move || {
                    for _ in 0..20 {
                        store.save(&s).unwrap();
                        store.save(&session(&format!("separate-{n}"))).unwrap();
                    }
                })
            })
            .collect();
        let results: Vec<_> = writers.into_iter().map(|w| w.join()).collect();
        running.store(false, Ordering::Release);
        for result in results {
            result.unwrap();
        }
    });
    assert_eq!(store.list().unwrap().len(), 5);
}

#[test]
fn port_errors_preserve_the_original_source() {
    use cola::app::SessionStoreError;
    use std::{error::Error, io};
    let error = SessionStoreError::new(
        SessionStoreErrorKind::Storage,
        io::Error::new(io::ErrorKind::PermissionDenied, "original"),
    );
    let source = error.source().unwrap().downcast_ref::<io::Error>().unwrap();
    assert_eq!(source.kind(), io::ErrorKind::PermissionDenied);
    assert_eq!(source.to_string(), "original");
}

#[test]
fn future_headers_are_classified_before_v1_fields() {
    let root = tempfile::tempdir().unwrap();
    let store = JsonlSessionStore::new(root.path());
    let s = session("future");
    store.save(&s).unwrap();
    let p = path(root.path());
    for header in [
        json!({"format":"cola.session","version":2,"session_id":"future","closed_turn_count":0,"active":false,"checksum":"new"}),
        json!({"format":"cola.session","version":2,"closed_turn_count":"changed"}),
        json!({"format":"cola.session","version":2}),
    ] {
        let original = bytes(&[header]);
        fs::write(&p, &original).unwrap();
        for error in [
            store.load(s.id()).unwrap_err(),
            store.list().unwrap_err(),
            store.save(&s).unwrap_err(),
        ] {
            assert_eq!(error.kind(), SessionStoreErrorKind::UnsupportedVersion);
        }
        assert_eq!(fs::read(&p).unwrap(), original);
    }
}

#[test]
fn corrupt_records_report_path_and_ordinal_with_json_source() {
    use std::error::Error;
    let root = tempfile::tempdir().unwrap();
    let store = JsonlSessionStore::new(root.path());
    let s = session("context");
    store.save(&s).unwrap();
    let p = path(root.path());
    fs::write(&p, b"{\"format\":\"cola.session\",\"version\":1,\"session_id\":\"context\",\"closed_turn_count\":1,\"active\":false}\n{broken}\n").unwrap();
    for error in [
        store.load(s.id()).unwrap_err(),
        store.list().unwrap_err(),
        store.save(&s).unwrap_err(),
    ] {
        assert_eq!(error.kind(), SessionStoreErrorKind::InvalidData);
        let display = error.to_string();
        assert!(display.contains(p.to_str().unwrap()), "{display}");
        assert!(display.contains("record 2"), "{display}");
        let mut source = error.source();
        let mut found_json = false;
        while let Some(cause) = source {
            found_json |= cause.downcast_ref::<serde_json::Error>().is_some();
            source = cause.source();
        }
        assert!(found_json);
    }
}
