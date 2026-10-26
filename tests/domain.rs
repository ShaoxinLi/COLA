use cola::domain::*;
use serde_json::{Map, Value, json};

fn text(value: &str) -> NonEmptyText {
    NonEmptyText::new(value).unwrap()
}
fn user(value: &str) -> UserMessage {
    UserMessage::new(text(value))
}
fn id(value: &str) -> ToolCallId {
    ToolCallId::new(value).unwrap()
}
fn session() -> Session {
    Session::new(SessionId::new("session 1").unwrap())
}
fn call(value: &str) -> ToolCall {
    ToolCall::new(
        id(value),
        ToolName::new("read_file").unwrap(),
        json!({"path":"notes.txt"}),
    )
    .unwrap()
}
fn batch(ids: &[&str]) -> AssistantMessage {
    AssistantMessage::tool_requests(
        None,
        ToolCalls::new(ids.iter().map(|id| call(id)).collect()).unwrap(),
    )
}
fn result(value: &str) -> ToolResult {
    ToolResult::new(
        id(value),
        ToolOutcome::Success(format!("result of {value}")),
    )
}
fn finish_tool(session: &mut Session, turn: TurnId, value: &str) {
    session.start_tool(turn, &id(value)).unwrap();
    session.record_tool_result(turn, result(value)).unwrap();
}
fn reject_unchanged(
    session: &mut Session,
    op: impl FnOnce(&mut Session) -> Result<(), SessionError>,
) -> SessionError {
    let before = session.snapshot();
    let error = op(session).unwrap_err();
    assert_eq!(session.snapshot(), before);
    error
}
fn partial_turn() -> (Session, TurnId) {
    let mut session = session();
    let turn = session.start_turn(user("read three files")).unwrap();
    session
        .record_assistant(turn, batch(&["a", "b", "c"]))
        .unwrap();
    finish_tool(&mut session, turn, "a");
    session.start_tool(turn, &id("b")).unwrap();
    (session, turn)
}
fn resolution(source: ResolutionSource) -> EffectResolution {
    EffectResolution::new(
        EffectDisposition::Completed,
        source,
        text("operation log confirms completion"),
    )
}
fn definition(name: &str) -> ToolDefinition {
    ToolDefinition::new(
        ToolName::new(name).unwrap(),
        text("Read a file"),
        InputSchema::new(json!({"type":"object"})).unwrap(),
    )
}

#[test]
fn external_tool_executes_arguments_and_records_success_or_failure() {
    struct Echo;
    impl Tool for Echo {
        fn execute(&self, arguments: &Map<String, Value>) -> ToolOutcome {
            match arguments.get("text").and_then(Value::as_str) {
                Some(text) => ToolOutcome::Success(text.to_owned()),
                None => ToolOutcome::Failure(ToolError::new(text("text must be a string"))),
            }
        }
    }

    let tool: &dyn Tool = &Echo;
    for (arguments, expected) in [
        (
            json!({"text": "hello"}),
            ToolOutcome::Success("hello".into()),
        ),
        (json!({"text": ""}), ToolOutcome::Success(String::new())),
        (
            json!({}),
            ToolOutcome::Failure(ToolError::new(text("text must be a string"))),
        ),
        (
            json!({"text": 42}),
            ToolOutcome::Failure(ToolError::new(text("text must be a string"))),
        ),
    ] {
        let mut session = session();
        let turn = session.start_turn(user("echo the text")).unwrap();
        let call =
            ToolCall::new(id("echo-call"), ToolName::new("echo").unwrap(), arguments).unwrap();
        session
            .record_assistant(
                turn,
                AssistantMessage::tool_requests(None, ToolCalls::new(vec![call]).unwrap()),
            )
            .unwrap();
        let call = session.start_tool(turn, &id("echo-call")).unwrap();
        let result = ToolResult::new(call.id().clone(), tool.execute(call.arguments()));
        session.record_tool_result(turn, result).unwrap();

        assert_eq!(
            session.messages().last(),
            Some(&Message::Tool(ToolResult::new(
                id("echo-call"),
                expected.clone()
            )))
        );
        assert_eq!(
            session.active_turn().unwrap().steps()[0]
                .executions()
                .next()
                .unwrap()
                .state(),
            &ExecutionState::Returned(expected)
        );
        assert_eq!(session.next_action(), NextAction::AskModel);
    }
}

#[test]
fn constructors_preserve_text_validate_identities_and_normalize_preambles() {
    assert_eq!(text("  hello\n").as_str(), "  hello\n");
    assert_eq!(NonEmptyText::new("\n \t"), Err(ValueError::BlankText));
    assert_eq!(SessionId::new("  "), Err(ValueError::BlankIdentity));
    assert_eq!(ToolCallId::new("\t"), Err(ValueError::BlankIdentity));
    assert_eq!(
        SessionId::new("any / opaque id\n").unwrap().as_str(),
        "any / opaque id\n"
    );
    assert_eq!(TurnId::new(0), Err(ValueError::ZeroTurnId));
    for name in ["read_file", "_read-2", "A"] {
        assert!(ToolName::new(name).is_ok());
    }
    for name in ["", "2read", "read.file", "read file", "读取"] {
        assert!(ToolName::new(name).is_err());
    }
    assert_eq!(ToolCalls::new(vec![]), Err(ValueError::EmptyToolCalls));
    assert!(matches!(
        ToolCalls::new(vec![call("a"), call("a")]),
        Err(ValueError::DuplicateToolCallId(_))
    ));
    assert!(matches!(
        ToolCall::new(id("a"), ToolName::new("read").unwrap(), json!([])),
        Err(ValueError::ExpectedObject)
    ));
    let request = AssistantMessage::tool_requests(
        Some(" \n".into()),
        ToolCalls::new(vec![call("a")]).unwrap(),
    );
    assert!(matches!(
        request,
        AssistantMessage::ToolRequests { text: None, .. }
    ));
    assert!(AssistantMessage::answer("  ").is_err());
    assert!(InputSchema::new(json!({"type":"object", "properties":false})).is_ok());
    for schema in [json!(true), json!({}), json!({"type":"array"})] {
        assert!(InputSchema::new(schema).is_err());
    }
    let tool = definition("read_file");
    assert_eq!(tool.description().as_str(), "Read a file");
    assert_eq!(
        tool.input_schema().document().get("type"),
        Some(&json!("object"))
    );
}

#[test]
fn direct_answer_closes_turn_and_allocates_the_next_id() {
    let mut session = session();
    assert_eq!(session.next_action(), NextAction::AwaitUser);
    let turn = session.start_turn(user("hello")).unwrap();
    assert_eq!(turn.get(), 1);
    assert_eq!(session.next_action(), NextAction::AskModel);
    session
        .record_assistant(turn, AssistantMessage::answer("Hello.").unwrap())
        .unwrap();
    assert_eq!(session.next_action(), NextAction::AwaitUser);
    assert!(session.active_turn().is_none());
    assert_eq!(session.turns()[0].outcome(), &TurnOutcome::Completed);
    assert_eq!(
        session.messages(),
        vec![
            Message::User(user("hello")),
            Message::Assistant(AssistantMessage::answer("Hello.").unwrap())
        ]
    );
    assert_eq!(session.start_turn(user("again")).unwrap().get(), 2);
}

#[test]
fn multiple_batches_and_known_tool_error_recover_in_one_turn() {
    let mut session = session();
    let turn = session.start_turn(user("find the file")).unwrap();
    session.record_assistant(turn, batch(&["a", "b"])).unwrap();
    assert_eq!(session.next_action(), NextAction::Execute(id("a")));
    let request = session.start_tool(turn, &id("a")).unwrap();
    assert_eq!(request.arguments().get("path"), Some(&json!("notes.txt")));
    assert_eq!(session.next_action(), NextAction::AwaitTool);
    let error = ToolError::new(text("file not found"));
    session
        .record_tool_result(
            turn,
            ToolResult::new(id("a"), ToolOutcome::Failure(error.clone())),
        )
        .unwrap();
    assert_eq!(session.next_action(), NextAction::Execute(id("b")));
    finish_tool(&mut session, turn, "b");
    assert_eq!(session.next_action(), NextAction::AskModel);
    session.record_assistant(turn, batch(&["retry"])).unwrap();
    session.start_tool(turn, &id("retry")).unwrap();
    session
        .record_tool_result(
            turn,
            ToolResult::new(id("retry"), ToolOutcome::Success(String::new())),
        )
        .unwrap();
    session
        .record_assistant(turn, AssistantMessage::answer("Recovered.").unwrap())
        .unwrap();
    let messages = session.messages();
    assert_eq!(messages.len(), 7);
    assert_eq!(
        messages[2],
        Message::Tool(ToolResult::new(id("a"), ToolOutcome::Failure(error)))
    );
    assert_eq!(
        messages[5],
        Message::Tool(ToolResult::new(
            id("retry"),
            ToolOutcome::Success(String::new())
        ))
    );
    assert_eq!(session.turns()[0].steps().len(), 3);
    assert_eq!(session.turns()[0].outcome(), &TurnOutcome::Completed);
}

#[test]
fn guard_rejections_are_atomic_and_started_calls_cannot_be_redispatched() {
    let mut session = session();
    let turn = session.start_turn(user("read")).unwrap();
    let wrong = TurnId::new(99).unwrap();
    assert_eq!(
        reject_unchanged(&mut session, |s| s.start_turn(user("other")).map(|_| ())),
        SessionError::ActiveTurnExists
    );
    assert_eq!(
        reject_unchanged(&mut session, |s| s.record_assistant(wrong, batch(&["a"]))),
        SessionError::WrongTurn
    );
    session.record_assistant(turn, batch(&["a", "b"])).unwrap();
    assert_eq!(
        reject_unchanged(&mut session, |s| s.start_tool(turn, &id("b")).map(|_| ())),
        SessionError::OutOfOrder
    );
    assert_eq!(
        reject_unchanged(&mut session, |s| s.record_tool_result(turn, result("b"))),
        SessionError::NotStarted
    );
    assert_eq!(
        reject_unchanged(&mut session, |s| s
            .record_tool_result(turn, result("missing"))),
        SessionError::UnknownCall
    );
    assert_eq!(
        reject_unchanged(&mut session, |s| s
            .record_assistant(turn, AssistantMessage::answer("early").unwrap())),
        SessionError::NotAwaitingModel
    );
    session.start_tool(turn, &id("a")).unwrap();
    assert_eq!(
        reject_unchanged(&mut session, |s| s.start_tool(turn, &id("a")).map(|_| ())),
        SessionError::AlreadyStarted
    );
    assert_eq!(
        reject_unchanged(&mut session, |s| s.record_tool_result(wrong, result("a"))),
        SessionError::WrongTurn
    );
    session.record_tool_result(turn, result("a")).unwrap();
    assert_eq!(
        reject_unchanged(&mut session, |s| s.record_tool_result(turn, result("a"))),
        SessionError::AlreadyReturned
    );
    assert_eq!(
        reject_unchanged(&mut session, |s| s.start_tool(turn, &id("a")).map(|_| ())),
        SessionError::AlreadyReturned
    );
    session.interrupt(turn, StopReason::UserCancelled).unwrap();
    assert_eq!(
        reject_unchanged(&mut session, |s| s.record_tool_result(turn, result("b"))),
        SessionError::TurnClosed
    );
    assert_eq!(
        reject_unchanged(&mut session, |s| s.start_tool(turn, &id("b")).map(|_| ())),
        SessionError::TurnClosed
    );
    assert_eq!(
        reject_unchanged(&mut session, |s| s.record_assistant(turn, batch(&["c"]))),
        SessionError::TurnClosed
    );
    assert_eq!(
        reject_unchanged(&mut session, |s| s
            .interrupt(turn, StopReason::UserCancelled)),
        SessionError::TurnClosed
    );
    assert_eq!(
        reject_unchanged(&mut session, |s| s
            .fail(turn, TurnFailure::ModelFailure(text("offline")))),
        SessionError::TurnClosed
    );
}

#[test]
fn duplicate_call_ids_across_steps_and_turns_reject_the_whole_reply() {
    let mut session = session();
    let first = session.start_turn(user("read")).unwrap();
    session.record_assistant(first, batch(&["used"])).unwrap();
    finish_tool(&mut session, first, "used");
    assert_eq!(
        reject_unchanged(&mut session, |s| s
            .record_assistant(first, batch(&["new", "used"]))),
        SessionError::DuplicateCallId(id("used"))
    );
    session
        .record_assistant(first, AssistantMessage::answer("done").unwrap())
        .unwrap();
    let second = session.start_turn(user("again")).unwrap();
    assert_eq!(
        reject_unchanged(&mut session, |s| s
            .record_assistant(second, batch(&["new", "used"]))),
        SessionError::DuplicateCallId(id("used"))
    );
    assert_eq!(session.next_action(), NextAction::AskModel);
    session.record_assistant(second, batch(&["new"])).unwrap();
}

#[test]
fn cancellation_preserves_returned_started_and_undispatched_facts() {
    let (mut session, turn) = partial_turn();
    assert_eq!(
        session.context_exchanges(),
        Err(ContextError::UnresolvedToolBatch)
    );
    session.interrupt(turn, StopReason::UserCancelled).unwrap();
    let executions: Vec<_> = session.turns()[0].steps()[0].executions().collect();
    assert!(matches!(executions[0].state(), ExecutionState::Returned(_)));
    assert_eq!(executions[1].state(), &ExecutionState::Started);
    assert_eq!(executions[2].state(), &ExecutionState::NotStarted);
    assert_eq!(
        session
            .messages()
            .iter()
            .filter(|m| matches!(m, Message::Tool(_)))
            .count(),
        1
    );
    let context = Context::new(
        text("Be accurate."),
        session.context_exchanges().unwrap(),
        vec![],
    )
    .unwrap();
    assert_eq!(context.messages().count(), 1);
    let notices: Vec<_> = context.execution_notices().collect();
    assert_eq!(notices.len(), 4);
    assert!(matches!(
        notices[0],
        ExecutionNotice::TurnClosed {
            outcome: TurnOutcome::Interrupted(StopReason::UserCancelled),
            ..
        }
    ));
    assert!(
        matches!(notices[1], ExecutionNotice::ConfirmedResult { result: r, .. } if r == &result("a"))
    );
    assert!(
        matches!(notices[2], ExecutionNotice::UnknownEffect { call, resolution: None, .. } if call.id() == &id("b"))
    );
    assert!(
        matches!(notices[3], ExecutionNotice::Undispatched { call, .. } if call.id() == &id("c"))
    );
}

#[test]
fn reconciliation_stores_source_and_evidence_without_reopening_or_fake_results() {
    let (mut session, turn) = partial_turn();
    assert_eq!(
        reject_unchanged(&mut session, |s| s.resolve_effect(
            turn,
            &id("b"),
            resolution(ResolutionSource::Observation)
        )),
        SessionError::EffectNotUnknown
    );
    session.interrupt(turn, StopReason::ProcessLost).unwrap();
    for call_id in ["a", "c"] {
        assert_eq!(
            reject_unchanged(&mut session, |s| s.resolve_effect(
                turn,
                &id(call_id),
                resolution(ResolutionSource::Observation)
            )),
            SessionError::EffectNotUnknown
        );
    }
    assert_eq!(
        reject_unchanged(&mut session, |s| s.resolve_effect(
            turn,
            &id("absent"),
            resolution(ResolutionSource::Observation)
        )),
        SessionError::UnknownCall
    );
    let original_messages = session.messages();
    let fact = resolution(ResolutionSource::UserAttestation);
    session
        .resolve_effect(turn, &id("b"), fact.clone())
        .unwrap();
    assert_eq!(session.messages(), original_messages);
    assert_eq!(session.next_action(), NextAction::AwaitUser);
    let before = session.snapshot();
    session
        .resolve_effect(turn, &id("b"), fact.clone())
        .unwrap();
    assert_eq!(session.snapshot(), before);
    assert_eq!(
        reject_unchanged(&mut session, |s| s.resolve_effect(
            turn,
            &id("b"),
            resolution(ResolutionSource::Observation)
        )),
        SessionError::ConflictingResolution
    );
    let execution = session.turns()[0].steps()[0].executions().nth(1).unwrap();
    assert_eq!(execution.state(), &ExecutionState::Started);
    assert_eq!(execution.resolution(), Some(&fact));
    assert_eq!(fact.source(), ResolutionSource::UserAttestation);
    assert_eq!(
        fact.evidence().as_str(),
        "operation log confirms completion"
    );
    assert_eq!(fact.disposition(), EffectDisposition::Completed);
    assert!(
        matches!(&session.context_exchanges().unwrap()[0].notices()[2], ExecutionNotice::UnknownEffect { resolution: Some(r), .. } if r == &fact)
    );
    assert_eq!(Session::restore(session.snapshot()).unwrap(), session);
}

#[test]
fn interruptions_and_failures_are_explicit_terminal_outcomes() {
    let mut session = session();
    for reason in [
        StopReason::UserCancelled,
        StopReason::ProcessLost,
        StopReason::ContextCapacity,
        StopReason::IterationLimit,
    ] {
        let turn = session.start_turn(user("question")).unwrap();
        session.interrupt(turn, reason).unwrap();
        assert_eq!(
            session.turns().last().unwrap().outcome(),
            &TurnOutcome::Interrupted(reason)
        );
    }
    for failure in [
        TurnFailure::ModelFailure(text("unavailable")),
        TurnFailure::MalformedResponse(text("missing content")),
    ] {
        let turn = session.start_turn(user("question")).unwrap();
        session.fail(turn, failure.clone()).unwrap();
        assert_eq!(
            session.turns().last().unwrap().outcome(),
            &TurnOutcome::Failed(failure)
        );
    }
    let context = Context::new(
        text("answer honestly"),
        session.context_exchanges().unwrap(),
        vec![],
    )
    .unwrap();
    assert_eq!(context.messages().count(), 6);
    assert_eq!(context.execution_notices().count(), 6);
    assert_eq!(Session::restore(session.snapshot()).unwrap(), session);
}

#[test]
fn closed_snapshot_roundtrip_preserves_complete_multi_batch_history() {
    let mut session = session();
    let turn = session.start_turn(user("read files")).unwrap();
    for call_id in ["one", "two"] {
        session.record_assistant(turn, batch(&[call_id])).unwrap();
        finish_tool(&mut session, turn, call_id);
    }
    session
        .record_assistant(turn, AssistantMessage::answer("contents").unwrap())
        .unwrap();
    let snapshot = session.snapshot();
    assert_eq!(
        Session::restore(snapshot.clone()).unwrap().snapshot(),
        snapshot
    );
}

#[test]
fn restoration_closes_active_work_as_process_lost_and_never_reexecutes() {
    let (session, turn) = partial_turn();
    let mut restored = Session::restore(session.snapshot()).unwrap();
    assert!(restored.active_turn().is_none());
    assert_eq!(restored.next_action(), NextAction::AwaitUser);
    assert_eq!(
        restored.turns()[0].outcome(),
        &TurnOutcome::Interrupted(StopReason::ProcessLost)
    );
    assert_eq!(restored.messages(), session.messages());
    assert_eq!(
        restored.turns()[0].steps()[0]
            .executions()
            .nth(1)
            .unwrap()
            .state(),
        &ExecutionState::Started
    );
    assert_eq!(
        reject_unchanged(&mut restored, |s| s.start_tool(turn, &id("c")).map(|_| ())),
        SessionError::TurnClosed
    );
    assert_eq!(restored.start_turn(user("new request")).unwrap().get(), 2);
    let mut awaiting_model = Session::new(SessionId::new("awaiting-model").unwrap());
    awaiting_model.start_turn(user("question")).unwrap();
    let recovered = Session::restore(awaiting_model.snapshot()).unwrap();
    assert_eq!(
        recovered.turns()[0].outcome(),
        &TurnOutcome::Interrupted(StopReason::ProcessLost)
    );
}

#[test]
fn corrupt_snapshots_reject_invalid_values_alignment_order_and_terminal_states() {
    let (mut session, turn) = partial_turn();
    session.interrupt(turn, StopReason::UserCancelled).unwrap();
    let base = session.snapshot();
    let corruptions: Vec<fn(&mut SessionSnapshot)> = vec![
        |s| s.session_id = " ".into(),
        |s| s.turns[0].id = 0,
        |s| s.turns[0].id = 2,
        |s| s.turns[0].user = "\t".into(),
        |s| s.turns[0].outcome = None,
        |s| s.turns[0].outcome = Some(TurnOutcome::Completed),
        |s| {
            s.turns[0].steps[0].executions.pop();
        },
        |s| s.turns[0].steps[0].executions[1].call_id = "other".into(),
        |s| s.turns[0].steps[0].executions[0].state = ExecutionState::NotStarted,
        |s| s.turns[0].steps[0].executions[2].state = ExecutionState::Started,
        |s| {
            s.turns[0].steps[0].executions[2].state =
                ExecutionState::Returned(ToolOutcome::Success("out of order".into()))
        },
        |s| {
            s.turns[0].steps[0].executions[0].resolution =
                Some(resolution(ResolutionSource::Observation))
        },
        |s| {
            let duplicate = s.turns[0].steps[0].clone();
            s.turns[0].steps.push(duplicate);
        },
        |s| {
            s.turns.push(s.turns[0].clone());
        },
        |s| s.active = Some(s.turns[0].clone()),
    ];
    for (index, corrupt) in corruptions.into_iter().enumerate() {
        let mut snapshot = base.clone();
        corrupt(&mut snapshot);
        assert!(
            Session::restore(snapshot).is_err(),
            "corruption {index} accepted"
        );
    }
    let call_corruptions: Vec<fn(&mut Vec<ToolCallSnapshot>)> = vec![
        |calls| calls[0].id = " ".into(),
        |calls| calls[0].name = "invalid.name".into(),
        |calls| calls[0].arguments = json!([]),
        |calls| calls[1].id = calls[0].id.clone(),
        |calls| calls.clear(),
    ];
    for corrupt in call_corruptions {
        let mut snapshot = base.clone();
        let AssistantSnapshot::ToolRequests { calls, .. } =
            &mut snapshot.turns[0].steps[0].assistant
        else {
            panic!("fixture")
        };
        corrupt(calls);
        assert!(Session::restore(snapshot).is_err());
    }
}

#[test]
fn corrupt_active_snapshots_cannot_hide_completion_or_resolutions() {
    let (session, _) = partial_turn();
    let mut snapshot = session.snapshot();
    snapshot.active.as_mut().unwrap().steps[0].executions[1].resolution =
        Some(resolution(ResolutionSource::ApplicationAttestation));
    assert!(Session::restore(snapshot).is_err());
    let mut snapshot = session.snapshot();
    snapshot.active.as_mut().unwrap().outcome =
        Some(TurnOutcome::Interrupted(StopReason::ProcessLost));
    assert!(Session::restore(snapshot).is_err());
    let snapshot = SessionSnapshot {
        session_id: "valid".into(),
        turns: vec![],
        active: Some(TurnSnapshot {
            id: 1,
            user: "question".into(),
            steps: vec![StepSnapshot {
                assistant: AssistantSnapshot::Answer("done".into()),
                executions: vec![],
            }],
            outcome: None,
        }),
    };
    assert!(Session::restore(snapshot).is_err());
}

#[test]
fn snapshot_rejects_duplicate_ids_across_closed_turns_and_answer_followed_by_steps() {
    let mut session = session();
    for call_id in ["a", "b"] {
        let turn = session.start_turn(user("read")).unwrap();
        session.record_assistant(turn, batch(&[call_id])).unwrap();
        finish_tool(&mut session, turn, call_id);
        session
            .record_assistant(turn, AssistantMessage::answer("done").unwrap())
            .unwrap();
    }
    let mut duplicate = session.snapshot();
    let AssistantSnapshot::ToolRequests { calls, .. } = &mut duplicate.turns[1].steps[0].assistant
    else {
        panic!("fixture")
    };
    calls[0].id = "a".into();
    duplicate.turns[1].steps[0].executions[0].call_id = "a".into();
    assert!(matches!(
        Session::restore(duplicate),
        Err(RestoreError::InvalidTransition(
            SessionError::DuplicateCallId(_)
        ))
    ));
    let mut after_answer = session.snapshot();
    let duplicate = after_answer.turns[0].steps[0].clone();
    after_answer.turns[0].steps.push(duplicate);
    assert!(Session::restore(after_answer).is_err());
}

#[test]
fn context_rejects_orphans_bad_order_unresolved_batches_and_inconsistent_outcomes() {
    let turn = TurnId::new(1).unwrap();
    let answer = Message::Assistant(AssistantMessage::answer("done").unwrap());
    let invalid = vec![
        (vec![], None),
        (vec![Message::Tool(result("a"))], None),
        (
            vec![Message::User(user("read")), Message::Tool(result("a"))],
            None,
        ),
        (
            vec![
                Message::User(user("read")),
                Message::Assistant(batch(&["a"])),
            ],
            None,
        ),
        (
            vec![
                Message::User(user("read")),
                Message::Assistant(batch(&["a"])),
                Message::Tool(result("b")),
            ],
            None,
        ),
        (
            vec![
                Message::User(user("read")),
                Message::Assistant(batch(&["a", "b"])),
                Message::Tool(result("b")),
                Message::Tool(result("a")),
            ],
            None,
        ),
        (
            vec![Message::User(user("read")), Message::User(user("again"))],
            None,
        ),
        (
            vec![Message::User(user("read")), answer.clone(), answer.clone()],
            Some(TurnOutcome::Completed),
        ),
        (vec![Message::User(user("read")), answer.clone()], None),
        (
            vec![Message::User(user("read"))],
            Some(TurnOutcome::Completed),
        ),
        (
            vec![Message::User(user("read")), answer],
            Some(TurnOutcome::Interrupted(StopReason::UserCancelled)),
        ),
    ];
    for (messages, outcome) in invalid {
        assert!(ContextExchange::new(turn, messages, outcome).is_err());
    }
    let duplicate = vec![
        Message::User(user("read")),
        Message::Assistant(batch(&["a"])),
        Message::Tool(result("a")),
        Message::Assistant(batch(&["a"])),
        Message::Tool(result("a")),
    ];
    assert_eq!(
        ContextExchange::new(turn, duplicate, None),
        Err(ContextError::DuplicateCallId(id("a")))
    );
}

#[test]
fn context_accepts_complete_exchanges_and_active_model_wait_with_unique_catalog() {
    let mut session = session();
    let first = session.start_turn(user("read")).unwrap();
    session.record_assistant(first, batch(&["a", "b"])).unwrap();
    finish_tool(&mut session, first, "a");
    finish_tool(&mut session, first, "b");
    session
        .record_assistant(first, AssistantMessage::answer("done").unwrap())
        .unwrap();
    let second = session.start_turn(user("more")).unwrap();
    session.record_assistant(second, batch(&["c"])).unwrap();
    finish_tool(&mut session, second, "c");
    let exchanges = session.context_exchanges().unwrap();
    let context = Context::new(
        text("Use tools carefully."),
        exchanges.clone(),
        vec![definition("read_file")],
    )
    .unwrap();
    assert_eq!(context.instructions().as_str(), "Use tools carefully.");
    assert_eq!(
        context.messages().cloned().collect::<Vec<_>>(),
        session.messages()
    );
    assert_eq!(context.tools()[0].name().as_str(), "read_file");
    assert_eq!(context.exchanges().len(), 2);
    assert_eq!(context.execution_notices().count(), 0);
    assert!(matches!(
        Context::new(
            text("rules"),
            exchanges,
            vec![definition("read_file"), definition("read_file")]
        ),
        Err(ContextError::DuplicateToolName(_))
    ));
}

#[test]
fn context_rejects_duplicate_ids_across_exchanges_and_invalid_exchange_boundaries() {
    let complete = |turn, call_id| {
        ContextExchange::new(
            TurnId::new(turn).unwrap(),
            vec![
                Message::User(user("read")),
                Message::Assistant(batch(&[call_id])),
                Message::Tool(result(call_id)),
                Message::Assistant(AssistantMessage::answer("done").unwrap()),
            ],
            Some(TurnOutcome::Completed),
        )
        .unwrap()
    };
    assert!(matches!(
        Context::new(
            text("rules"),
            vec![complete(1, "a"), complete(2, "a")],
            vec![]
        ),
        Err(ContextError::DuplicateCallId(_))
    ));
    assert_eq!(
        Context::new(
            text("rules"),
            vec![complete(2, "a"), complete(1, "b")],
            vec![]
        ),
        Err(ContextError::InvalidTurnOrder)
    );
    let active = ContextExchange::new(
        TurnId::new(1).unwrap(),
        vec![Message::User(user("read"))],
        None,
    )
    .unwrap();
    assert_eq!(
        Context::new(text("rules"), vec![active, complete(2, "b")], vec![]),
        Err(ContextError::ActiveExchangeNotLast)
    );
    let (mut cancelled, turn) = partial_turn();
    cancelled
        .interrupt(turn, StopReason::UserCancelled)
        .unwrap();
    let mut exchanges = cancelled.context_exchanges().unwrap();
    exchanges.push(complete(2, "b"));
    assert_eq!(
        Context::new(text("rules"), exchanges, vec![]),
        Err(ContextError::DuplicateCallId(id("b")))
    );
}
