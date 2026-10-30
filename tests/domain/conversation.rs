use super::{
    batch, finish_tool, id, partial_turn, reject_unchanged, resolution, result, session, text, user,
};
use cola::domain::{
    AssistantMessage, AssistantSnapshot, Context, ContextError, EffectDisposition, ExecutionNotice,
    ExecutionState, Message, NextAction, ResolutionSource, Session, SessionError, SessionSnapshot,
    StepSnapshot, StopReason, ToolCallSnapshot, ToolError, ToolOutcome, ToolResult, TurnFailure,
    TurnId, TurnOutcome, TurnSnapshot,
};
use serde_json::json;

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
    let request = session
        .start_tool(&super::address(&session, turn, &id("a")))
        .unwrap();
    assert_eq!(request.arguments().get("path"), Some(&json!("notes.txt")));
    assert_eq!(session.next_action(), NextAction::AwaitTool);
    let error = ToolError::new(text("file not found"));
    session
        .record_tool_result(
            &super::address(
                &session,
                turn,
                (ToolResult::new(id("a"), ToolOutcome::Failure(error.clone()))).call_id(),
            ),
            (ToolResult::new(id("a"), ToolOutcome::Failure(error.clone())))
                .outcome()
                .clone(),
        )
        .unwrap();
    assert_eq!(session.next_action(), NextAction::Execute(id("b")));
    finish_tool(&mut session, turn, "b");
    assert_eq!(session.next_action(), NextAction::AskModel);
    session.record_assistant(turn, batch(&["retry"])).unwrap();
    session
        .start_tool(&super::address(&session, turn, &id("retry")))
        .unwrap();
    session
        .record_tool_result(
            &super::address(
                &session,
                turn,
                (ToolResult::new(id("retry"), ToolOutcome::Success(String::new()))).call_id(),
            ),
            (ToolResult::new(id("retry"), ToolOutcome::Success(String::new())))
                .outcome()
                .clone(),
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
        reject_unchanged(&mut session, |s| s
            .start_tool(&super::address(s, turn, &id("missing")))
            .map(|_| ())),
        SessionError::UnknownCall
    );
    assert_eq!(
        reject_unchanged(&mut session, |s| s
            .start_tool(&super::address(s, turn, &id("b")))
            .map(|_| ())),
        SessionError::OutOfOrder
    );
    assert_eq!(
        reject_unchanged(&mut session, |s| s.record_tool_result(
            &super::address(s, turn, (result("b")).call_id()),
            (result("b")).outcome().clone()
        )),
        SessionError::NotStarted
    );
    assert_eq!(
        reject_unchanged(&mut session, |s| s.record_tool_result(
            &super::address(s, turn, (result("missing")).call_id()),
            (result("missing")).outcome().clone()
        )),
        SessionError::UnknownCall
    );
    assert_eq!(
        reject_unchanged(&mut session, |s| s
            .record_assistant(turn, AssistantMessage::answer("early").unwrap())),
        SessionError::NotAwaitingModel
    );
    session
        .start_tool(&super::address(&session, turn, &id("a")))
        .unwrap();
    assert_eq!(
        reject_unchanged(&mut session, |s| s
            .start_tool(&super::address(s, turn, &id("b")))
            .map(|_| ())),
        SessionError::OutOfOrder
    );
    assert_eq!(
        reject_unchanged(&mut session, |s| s.record_tool_result(
            &super::address(s, turn, (result("b")).call_id()),
            (result("b")).outcome().clone()
        )),
        SessionError::NotStarted
    );
    assert_eq!(
        reject_unchanged(&mut session, |s| s
            .start_tool(&super::address(s, turn, &id("a")))
            .map(|_| ())),
        SessionError::AlreadyStarted
    );
    assert_eq!(
        reject_unchanged(&mut session, |s| s.record_tool_result(
            &super::address(s, wrong, (result("a")).call_id()),
            (result("a")).outcome().clone()
        )),
        SessionError::WrongTurn
    );
    session
        .record_tool_result(
            &super::address(&session, turn, (result("a")).call_id()),
            (result("a")).outcome().clone(),
        )
        .unwrap();
    assert_eq!(
        reject_unchanged(&mut session, |s| s.record_tool_result(
            &super::address(s, turn, (result("a")).call_id()),
            (result("a")).outcome().clone()
        )),
        SessionError::AlreadyReturned
    );
    assert_eq!(
        reject_unchanged(&mut session, |s| s
            .start_tool(&super::address(s, turn, &id("a")))
            .map(|_| ())),
        SessionError::AlreadyReturned
    );
    session.interrupt(turn, StopReason::UserCancelled).unwrap();
    assert_eq!(
        reject_unchanged(&mut session, |s| s.record_tool_result(
            &super::address(s, turn, (result("b")).call_id()),
            (result("b")).outcome().clone()
        )),
        SessionError::TurnClosed
    );
    assert_eq!(
        reject_unchanged(&mut session, |s| s
            .start_tool(&super::address(s, turn, &id("b")))
            .map(|_| ())),
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
fn repeated_native_ids_are_scoped_and_stale_receipts_are_rejected() {
    let mut session = session();
    let turn = session.start_turn(user("read")).unwrap();
    session.record_assistant(turn, batch(&["same"])).unwrap();
    let old = session.call_address(id("same")).unwrap();
    session.start_tool(&old).unwrap();
    session
        .record_tool_result(&old, ToolOutcome::Success("one".into()))
        .unwrap();
    session.record_assistant(turn, batch(&["same"])).unwrap();
    let current = session.call_address(id("same")).unwrap();
    assert_ne!(old, current);
    session.start_tool(&current).unwrap();
    assert_eq!(
        session.record_tool_result(&old, ToolOutcome::Success("stale".into())),
        Err(SessionError::UnknownCall)
    );
    session
        .record_tool_result(&current, ToolOutcome::Success("two".into()))
        .unwrap();
    session
        .record_assistant(turn, AssistantMessage::answer("done").unwrap())
        .unwrap();
    let next = session.start_turn(user("again")).unwrap();
    session.record_assistant(next, batch(&["same"])).unwrap();
    assert_eq!(
        session.record_tool_result(&current, ToolOutcome::Success("stale".into())),
        Err(SessionError::TurnClosed)
    );
    assert!(Session::restore(session.snapshot()).is_ok());
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
    let context = Context::<AssistantMessage>::new(
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
            &super::address(s, turn, &id("b")),
            resolution(ResolutionSource::Observation)
        )),
        SessionError::EffectNotUnknown
    );
    session.interrupt(turn, StopReason::ProcessLost).unwrap();
    for call_id in ["a", "c"] {
        assert_eq!(
            reject_unchanged(&mut session, |s| s.resolve_effect(
                &super::address(s, turn, &id(call_id)),
                resolution(ResolutionSource::Observation)
            )),
            SessionError::EffectNotUnknown
        );
    }
    assert_eq!(
        reject_unchanged(&mut session, |s| s.resolve_effect(
            &super::address(s, turn, &id("absent")),
            resolution(ResolutionSource::Observation)
        )),
        SessionError::UnknownCall
    );
    let original_messages = session.messages();
    let fact = resolution(ResolutionSource::UserAttestation);
    session
        .resolve_effect(&super::address(&session, turn, &id("b")), fact.clone())
        .unwrap();
    assert_eq!(session.messages(), original_messages);
    assert_eq!(session.next_action(), NextAction::AwaitUser);
    let before = session.snapshot();
    session
        .resolve_effect(&super::address(&session, turn, &id("b")), fact.clone())
        .unwrap();
    assert_eq!(session.snapshot(), before);
    assert_eq!(
        reject_unchanged(&mut session, |s| s.resolve_effect(
            &super::address(s, turn, &id("b")),
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
    let context = Context::<AssistantMessage>::new(
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
fn restoration_preserves_active_facts_without_implicit_recovery() {
    let (session, _) = partial_turn();
    let restored = Session::restore(session.snapshot()).unwrap();
    assert_eq!(restored, session);
    assert_eq!(restored.next_action(), NextAction::AwaitTool);
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
fn snapshot_accepts_repeated_ids_across_turns_but_rejects_steps_after_an_answer() {
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
    assert!(Session::restore(duplicate).is_ok());
    let mut after_answer = session.snapshot();
    let duplicate = after_answer.turns[0].steps[0].clone();
    after_answer.turns[0].steps.push(duplicate);
    assert!(Session::restore(after_answer).is_err());
}

#[test]
fn effect_resolution_uses_full_address_when_native_ids_repeat() {
    let mut s = session();
    let turn = s.start_turn(user("repeat")).unwrap();
    s.record_assistant(turn, batch(&["same"])).unwrap();
    let first = s.call_address(id("same")).unwrap();
    s.start_tool(&first).unwrap();
    s.record_tool_result(&first, ToolOutcome::Success("known".into()))
        .unwrap();
    s.record_assistant(turn, batch(&["same"])).unwrap();
    let unknown = s.call_address(id("same")).unwrap();
    s.start_tool(&unknown).unwrap();
    s.interrupt(turn, StopReason::ProcessLost).unwrap();
    let proof = resolution(ResolutionSource::Observation);
    assert_eq!(
        s.resolve_effect(&first, proof.clone()),
        Err(SessionError::EffectNotUnknown)
    );
    s.resolve_effect(&unknown, proof.clone()).unwrap();
    let next = s.start_turn(user("new turn")).unwrap();
    s.record_assistant(next, batch(&["same"])).unwrap();
    let newer = s.call_address(id("same")).unwrap();
    s.start_tool(&newer).unwrap();
    s.interrupt(next, StopReason::ProcessLost).unwrap();
    s.resolve_effect(&unknown, proof).unwrap();
    assert!(
        s.turns()[1].steps()[0]
            .executions()
            .next()
            .unwrap()
            .resolution()
            .is_none()
    );
    assert_eq!(Session::restore(s.snapshot()).unwrap(), s);
}
