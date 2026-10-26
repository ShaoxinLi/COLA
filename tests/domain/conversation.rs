use super::support::{
    batch, finish_tool, id, partial_turn, reject_unchanged, resolution, result, session, text, user,
};
use cola::domain::{
    AssistantMessage, Context, ContextError, EffectDisposition, ExecutionNotice, ExecutionState,
    Message, NextAction, ResolutionSource, Session, SessionError, StopReason, ToolError,
    ToolOutcome, ToolResult, TurnFailure, TurnId, TurnOutcome,
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
