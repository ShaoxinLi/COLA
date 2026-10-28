use super::support::*;
use cola::adapters::api::cli::{CliApp, CliExit};
use std::io::{Cursor, Write};
fn run(mode: &str, store: Store, input: &[u8]) -> (CliExit, Vec<Value>) {
    let mut output = vec![];
    let counts = store.counts.clone();
    let exit = CliApp::new(service(mode, store, counts))
        .run(Cursor::new(input), &mut output)
        .unwrap();
    (
        exit,
        output
            .split(|b| *b == b'\n')
            .filter(|b| !b.is_empty())
            .map(|b| serde_json::from_slice(b).unwrap())
            .collect(),
    )
}
#[test]
fn persistent_commands_preserve_selection_text_and_new_turn_only() {
    let store = Store::default();
    let counts = store.counts.clone();
    let (exit, replies) = run(
        "answer",
        store.clone(),
        b"\n/new\r\n  first  \r\n//literal\n/sessions\n/exit\nignored\n",
    );
    assert_eq!(exit.code(), 0);
    assert_eq!(replies.len(), 4);
    assert_eq!(replies[0]["kind"], "created");
    assert_eq!(
        replies[1]["messages"],
        json!([{"kind":"user","text":"  first  "},{"kind":"assistant_answer","text":"answer-1"}])
    );
    assert_eq!(replies[2]["messages"][0]["text"], "/literal");
    assert_eq!(replies[2]["messages"].as_array().unwrap().len(), 2);
    assert_eq!(
        replies[3]["sessions"][0]["turns"].as_array().unwrap().len(),
        2
    );
    assert_eq!(counts.model.get(), 2);
    assert_eq!(counts.list.get(), 1);
    assert_eq!(counts.load.get(), 2);
    let id = SessionId::new(replies[0]["session"]["id"].as_str().unwrap()).unwrap();
    assert_eq!(store.inspect(&id).unwrap().session().turns().len(), 2);
}
#[test]
fn use_is_acknowledged_without_loading_and_accepts_opaque_ids() {
    let store = Store::default();
    let counts = store.counts.clone();
    let (exit, r) = run("answer", store, b"/use opaque id\n/sessions\nhello");
    assert_eq!(exit, CliExit::CommandFailed);
    assert_eq!(r[0], json!({"kind":"selected","session_id":"opaque id"}));
    assert_eq!(r[1]["sessions"], json!([]));
    assert_eq!(r[2]["error"]["code"], "session_not_found");
    assert_eq!(counts.load.get(), 1);
    assert_eq!(counts.model.get(), 0);
}
#[test]
fn invalid_commands_accumulate_precedence_and_failed_new_keeps_selection() {
    let store = Store {
        fail_save: Some(2),
        ..Store::default()
    };
    let (exit, r) = run(
        "answer",
        store,
        b"no selection\n/unknown\n/use    \n/new\n/new\nhello\n/exit\n",
    );
    assert_eq!(exit, CliExit::InvalidInput);
    assert_eq!(exit.code(), 2);
    assert_eq!(r.len(), 6);
    assert_eq!(r[4]["error"]["code"], "store_failure");
    assert_eq!(r[5]["session_id"], r[3]["session"]["id"]);
    assert!(
        !serde_json::to_string(&r)
            .unwrap()
            .contains("/private/secret")
    );
}
#[test]
fn line_limits_drain_utf8_and_eof_are_input_failures() {
    for delimiter in [b"\n".as_slice(), b"\r\n".as_slice()] {
        let mut input = b"/new\n".to_vec();
        input.extend(vec![b'x'; 65536]);
        input.extend(delimiter);
        input.extend(vec![b'y'; 65537]);
        input.extend(delimiter);
        input.extend(b"\xff\nfinal");
        let (exit, r) = run("answer", Store::default(), &input);
        assert_eq!(exit, CliExit::InvalidInput);
        assert_eq!(r.len(), 5);
        assert_eq!(r[1]["messages"][0]["text"].as_str().unwrap().len(), 65536);
        assert_eq!(r[2]["kind"], "error");
        assert_eq!(r[3]["kind"], "error");
        assert_eq!(r[4]["messages"][0]["text"], "final");
    }
    let (_, r) = run("answer", Store::default(), &vec![b'x'; 65537]);
    assert_eq!(r.len(), 1);
    assert_eq!(r[0]["kind"], "error");
    assert_eq!(
        run("answer", Store::default(), b"\n \t\r\n/exit").1.len(),
        0
    );
}
#[test]
fn full_tool_messages_and_session_execution_facts() {
    for (mode, outcome, targets) in [("tools", "success", 1), ("denied", "failure", 0)] {
        let store = Store::default();
        let counts = store.counts.clone();
        let (exit, r) = run(mode, store, b"/new\nquestion\n/sessions\n");
        assert_eq!(exit, CliExit::Success);
        assert_eq!(
            r[1]["messages"][1]["calls"][0],
            json!({"id":"call-1","name":"echo","arguments":{"n":42}})
        );
        assert_eq!(r[1]["messages"][2]["kind"], "tool_result");
        assert_eq!(r[1]["messages"][2]["outcome"]["kind"], outcome);
        assert_eq!(
            r[2]["sessions"][0]["turns"][0]["steps"][0]["executions"][0]["state"]["kind"],
            if mode == "denied" {
                "denied"
            } else {
                "returned"
            }
        );
        assert_eq!(counts.tool.get(), targets);
    }
}
#[test]
fn errors_retain_role_and_committed_progress_without_resident_overlay() {
    for (mode, code, role) in [
        ("timeout", "model_timeout", "assistant"),
        ("review-timeout", "model_timeout", "reviewer"),
        ("refused", "generation_refused", "assistant"),
        ("incomplete", "generation_incomplete", "assistant"),
        ("model", "model_failure", "assistant"),
        ("limit", "iteration_limit", ""),
    ] {
        let (exit, r) = run(mode, Store::default(), b"/new\nhello\nagain\n");
        assert_eq!(exit, CliExit::CommandFailed);
        assert_eq!(exit.code(), 1);
        assert_eq!(r[1]["error"]["code"], code);
        if !role.is_empty() {
            assert_eq!(r[1]["error"]["role"], role);
        }
        assert_eq!(r[1]["error"]["durability"], "committed");
        assert!(r[1]["error"]["progress"]["active"].is_null());
        if mode == "review-timeout" {
            assert_eq!(
                r[1]["error"]["progress"]["turns"][0]["steps"][0]["executions"][0]["state"]["kind"],
                "requested"
            );
            assert_eq!(r[2]["kind"], "chatted");
        }
    }
    let (exit, r) = run(
        "answer",
        Store {
            fail_save: Some(3),
            ..Store::default()
        },
        b"/new\nhello\nagain\n/sessions\n",
    );
    assert_eq!(exit, CliExit::CommandFailed);
    assert_eq!(r[1]["error"]["code"], "store_failure");
    assert_eq!(r[1]["error"]["progress"]["active"]["user"], "hello");
    assert_eq!(r[2]["error"]["code"], "recovery_required");
    assert_eq!(r[3]["sessions"][0]["turns"].as_array().unwrap().len(), 0);
}
#[test]
fn listing_failure_does_not_block_other_session_commands() {
    let (exit, replies) = run(
        "answer",
        Store {
            fail_list: true,
            ..Store::default()
        },
        b"/sessions\n/new\nhello\n",
    );
    assert_eq!(exit, CliExit::CommandFailed);
    assert_eq!(replies[0]["error"]["code"], "store_failure");
    assert!(replies[0]["error"]["progress"].is_null());
    assert_eq!(replies[1]["kind"], "created");
    assert_eq!(replies[2]["messages"][1]["text"], "answer-1");
    assert!(
        !serde_json::to_string(&replies)
            .unwrap()
            .contains("/private/secret")
    );
}
#[test]
fn listing_preserves_unknown_effect_resolution_and_all_outcomes() {
    let store = Store::default();
    let archive = unknown_effect_archive();
    store
        .sessions
        .borrow_mut()
        .insert(archive.session().id().clone(), archive);
    let (_, r) = run("answer", store, b"/sessions\n");
    let turn = &r[0]["sessions"][0]["turns"][0];
    assert_eq!(
        turn["outcome"],
        json!({"kind":"interrupted","reason":"process_lost"})
    );
    let execution = &turn["steps"][0]["executions"][0];
    assert_eq!(execution["call_id"], "call-1");
    assert_eq!(execution["generation"], 1);
    assert_eq!(execution["state"]["kind"], "started");
    assert_eq!(
        execution["resolution"],
        json!({"disposition":"not_performed","source":"application_attestation","evidence":"checked"})
    );
}
#[test]
fn interrupted_input_retries_without_losing_partial_lines() {
    struct Interrupted {
        input: Cursor<&'static [u8]>,
        retried: bool,
    }
    impl io::Read for Interrupted {
        fn read(&mut self, output: &mut [u8]) -> io::Result<usize> {
            io::Read::read(&mut self.input, output)
        }
    }
    impl io::BufRead for Interrupted {
        fn fill_buf(&mut self) -> io::Result<&[u8]> {
            if self.input.position() == 2 && !self.retried {
                self.retried = true;
                return Err(io::Error::from(io::ErrorKind::Interrupted));
            }
            let initial = self.input.position() == 0;
            let bytes = io::BufRead::fill_buf(&mut self.input)?;
            Ok(if initial { &bytes[..2] } else { bytes })
        }
        fn consume(&mut self, count: usize) {
            io::BufRead::consume(&mut self.input, count);
        }
    }
    let store = Store::default();
    let mut output = Vec::new();
    let exit = CliApp::new(service("answer", store.clone(), store.counts.clone()))
        .run(
            Interrupted {
                input: Cursor::new(b"/new\nhello\n/exit\n"),
                retried: false,
            },
            &mut output,
        )
        .unwrap();
    let replies: Vec<Value> = output
        .split(|byte| *byte == b'\n')
        .filter(|line| !line.is_empty())
        .map(|line| serde_json::from_slice(line).unwrap())
        .collect();
    assert_eq!(exit, CliExit::Success);
    assert_eq!(replies.len(), 2);
    assert_eq!(replies[0]["kind"], "created");
    assert_eq!(replies[1]["messages"][0]["text"], "hello");
    assert_eq!(replies[1]["messages"][1]["text"], "answer-1");
}
#[test]
fn output_is_flushed_and_stream_failures_propagate() {
    #[derive(Default)]
    struct Output {
        bytes: Vec<u8>,
        flushes: usize,
        fail: bool,
    }
    impl Write for Output {
        fn write(&mut self, b: &[u8]) -> io::Result<usize> {
            if self.fail {
                Err(io::Error::other("write"))
            } else {
                self.bytes.extend_from_slice(b);
                Ok(b.len())
            }
        }
        fn flush(&mut self) -> io::Result<()> {
            self.flushes += 1;
            Ok(())
        }
    }
    let mut out = Output::default();
    let store = Store::default();
    CliApp::new(service("answer", store.clone(), store.counts.clone()))
        .run(Cursor::new(b"/new\n/sessions\n/exit\n"), &mut out)
        .unwrap();
    assert_eq!(out.flushes, 2);
    let store = Store::default();
    assert!(
        CliApp::new(service("answer", store.clone(), store.counts.clone()))
            .run(
                Cursor::new(b"/new\n"),
                Output {
                    fail: true,
                    ..Output::default()
                }
            )
            .is_err()
    );
    struct Broken;
    impl io::Read for Broken {
        fn read(&mut self, _: &mut [u8]) -> io::Result<usize> {
            Err(io::Error::other("read"))
        }
    }
    impl io::BufRead for Broken {
        fn fill_buf(&mut self) -> io::Result<&[u8]> {
            Err(io::Error::other("read"))
        }
        fn consume(&mut self, _: usize) {}
    }
    let store = Store::default();
    assert!(
        CliApp::new(service("answer", store.clone(), store.counts.clone()))
            .run(Broken, vec![])
            .is_err()
    );
}

#[test]
fn recover_requires_selection_and_closes_only_recorded_active_work() {
    let store = Store::default();
    let counts = store.counts.clone();
    let mut archive = SessionArchive::new(SessionId::new("recover-me").unwrap());
    archive
        .session_mut()
        .start_turn(UserMessage::new(text("interrupted")))
        .unwrap();
    let change = ArchiveChange::initial(archive).unwrap();
    store.create(&change).unwrap();
    let (exit, replies) = run(
        "answer",
        store,
        b"/recover\n/use recover-me\nhello\n/recover\n/recover\nhello\n",
    );
    assert_eq!(exit, CliExit::InvalidInput);
    assert_eq!(replies[0]["error"]["code"], "invalid_input");
    assert_eq!(replies[2]["error"]["code"], "recovery_required");
    assert_eq!(replies[3]["kind"], "recovered");
    assert_eq!(replies[3], replies[4]);
    assert_eq!(
        replies[3]["session"]["turns"][0]["outcome"]["reason"],
        "process_lost"
    );
    assert_eq!(replies[5]["messages"][1]["text"], "answer-1");
    assert_eq!(counts.model.get(), 1);
    assert_eq!(counts.review.get(), 0);
    assert_eq!(counts.tool.get(), 0);
}
