use super::{
    approve, at, batch, finish_tool, generation, id, next_generation, partial_turn, reference,
    reject_unchanged, resolution, result, session, submit, text, user,
};
use cola::domain::{
    AssistantMessage, AssistantSnapshot, CallRef, Decision, EffectDisposition, ExecutionState,
    Message, NextAction, ResolutionSource, Review, Session, SessionError, SessionId,
    SessionSnapshot, StepSnapshot, StopReason, ToolCallSnapshot, ToolError, ToolOutcome,
    ToolResult, TurnFailure, TurnId, TurnOutcome, TurnSnapshot,
};
use serde_json::json;

#[test]
fn direct_answer_closes_turn_and_allocates_the_next_id() {
    let mut session = session();
    assert_eq!(session.next_action(), NextAction::AwaitUser);
    let turn = session.start_turn(user("hello")).unwrap();
    assert_eq!(turn.get(), 1);
    assert_eq!(session.next_action(), NextAction::AskModel);
    submit(
        &mut session,
        turn,
        AssistantMessage::answer("Hello.").unwrap(),
    )
    .unwrap();
    assert_eq!(session.next_action(), NextAction::AwaitUser);
    assert!(session.active_turn().is_none());
    assert_eq!(session.turns()[0].outcome(), &TurnOutcome::Completed);
    assert_eq!(
        session.turns()[0].steps()[0].source_generation(),
        generation(1)
    );
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
fn multiple_batches_preserve_empty_success_and_genuine_tool_failure() {
    let mut session = session();
    let turn = session.start_turn(user("find the file")).unwrap();
    submit(&mut session, turn, batch(&["a", "b"])).unwrap();
    assert_eq!(
        session.next_action(),
        NextAction::Review(reference(&session, "a"))
    );
    approve(&mut session, turn, "a");
    let a = reference(&session, "a");
    assert_eq!(session.next_action(), NextAction::Execute(a.clone()));
    let request = session.start_tool(turn, &a, at(10)).unwrap();
    assert_eq!(request.arguments().get("path"), Some(&json!("notes.txt")));
    assert_eq!(session.next_action(), NextAction::AwaitTool);
    let error = ToolError::new(text("file not found"));
    session
        .record_tool_result(turn, &a, ToolOutcome::Failure(error.clone()), at(11))
        .unwrap();
    assert_eq!(
        session.next_action(),
        NextAction::Review(reference(&session, "b"))
    );
    finish_tool(&mut session, turn, "b");
    assert_eq!(session.next_action(), NextAction::AskModel);
    submit(&mut session, turn, batch(&["retry"])).unwrap();
    approve(&mut session, turn, "retry");
    let retry = reference(&session, "retry");
    session.start_tool(turn, &retry, at(20)).unwrap();
    session
        .record_tool_result(turn, &retry, ToolOutcome::Success(String::new()), at(21))
        .unwrap();
    submit(
        &mut session,
        turn,
        AssistantMessage::answer("Recovered.").unwrap(),
    )
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
fn review_decision_matrix_keeps_dispatch_and_protocol_results_separate() {
    for first in [false, true] {
        for second in [false, true] {
            let mut session = session();
            let turn = session.start_turn(user("review both files")).unwrap();
            submit(&mut session, turn, batch(&["a", "b"])).unwrap();
            for (name, allow) in [("a", first), ("b", second)] {
                let call = reference(&session, name);
                let reviewer = next_generation(&session);
                let reason = Some(text(format!("decision for {name}")));
                session
                    .review_tool(turn, &call, reviewer, &Decision::new(allow, reason.clone()))
                    .unwrap();
                let execution = session.active_turn().unwrap().steps()[0]
                    .executions()
                    .find(|e| e.call().id() == &id(name))
                    .unwrap();
                assert_eq!(execution.state().review().unwrap().reviewer(), reviewer);
                assert_eq!(
                    execution.state().review().unwrap().reason(),
                    reason.as_ref()
                );
                if allow {
                    assert!(matches!(execution.state(), ExecutionState::Approved(_)));
                    assert_eq!(execution.state().protocol_outcome(), None);
                    session.start_tool(turn, &call, at(30)).unwrap();
                    session
                        .record_tool_result(turn, &call, ToolOutcome::Success(name.into()), at(31))
                        .unwrap();
                } else {
                    assert!(matches!(execution.state(), ExecutionState::Denied(_)));
                    assert_eq!(
                        reject_unchanged(&mut session, |s| s
                            .start_tool(turn, &call, at(30))
                            .map(|_| ())),
                        SessionError::NotApproved
                    );
                    let outcome = session.active_turn().unwrap().steps()[0]
                        .executions()
                        .find(|e| e.call().id() == &id(name))
                        .unwrap()
                        .state()
                        .protocol_outcome()
                        .unwrap();
                    let ToolOutcome::Failure(error) = outcome else {
                        panic!("denial must be a protocol failure")
                    };
                    assert_eq!(
                        error.description().as_str(),
                        format!(
                            "Not executed: tool call denied by review. Reason: decision for {name}"
                        )
                    );
                }
            }
            assert_eq!(session.next_action(), NextAction::AskModel);
            let states: Vec<_> = session.active_turn().unwrap().steps()[0]
                .executions()
                .map(|e| e.state().clone())
                .collect();
            assert_eq!(
                states
                    .iter()
                    .filter(|s| matches!(s, ExecutionState::Returned { .. }))
                    .count(),
                usize::from(first) + usize::from(second)
            );
            assert_eq!(
                session
                    .messages()
                    .iter()
                    .filter(|m| matches!(m, Message::Tool(_)))
                    .count(),
                2
            );
        }
    }
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
        reject_unchanged(&mut session, |s| s.record_assistant(
            wrong,
            generation(1),
            batch(&["a"])
        )),
        SessionError::WrongTurn
    );
    submit(&mut session, turn, batch(&["a", "b"])).unwrap();
    let a = reference(&session, "a");
    let b = reference(&session, "b");
    let missing = reference(&session, "missing");
    let wrong_source = CallRef::new(generation(999), id("a"));
    for missing in [&missing, &wrong_source] {
        assert_eq!(
            reject_unchanged(&mut session, |s| s
                .start_tool(turn, missing, at(1))
                .map(|_| ())),
            SessionError::UnknownCall
        );
        assert_eq!(
            reject_unchanged(&mut session, |s| s.record_tool_result(
                turn,
                missing,
                ToolOutcome::Success("x".into()),
                at(2)
            )),
            SessionError::UnknownCall
        );
    }
    assert_eq!(
        reject_unchanged(&mut session, |s| s.start_tool(turn, &a, at(1)).map(|_| ())),
        SessionError::NotApproved
    );
    assert_eq!(
        reject_unchanged(&mut session, |s| s.review_tool(
            turn,
            &b,
            generation(2),
            &Decision::new(true, None)
        )),
        SessionError::OutOfOrder
    );
    assert_eq!(
        reject_unchanged(&mut session, |s| s.start_tool(turn, &b, at(1)).map(|_| ())),
        SessionError::OutOfOrder
    );
    assert_eq!(
        reject_unchanged(&mut session, |s| s.record_tool_result(
            turn,
            &b,
            ToolOutcome::Success("x".into()),
            at(2)
        )),
        SessionError::NotStarted
    );
    assert_eq!(
        reject_unchanged(&mut session, |s| s.record_assistant(
            turn,
            generation(2),
            AssistantMessage::answer("early").unwrap()
        )),
        SessionError::NotAwaitingModel
    );
    approve(&mut session, turn, "a");
    assert_eq!(
        reject_unchanged(&mut session, |s| s.review_tool(
            turn,
            &a,
            generation(3),
            &Decision::new(false, None)
        )),
        SessionError::AlreadyReviewed
    );
    session.start_tool(turn, &a, at(10)).unwrap();
    assert_eq!(
        reject_unchanged(&mut session, |s| s.start_tool(turn, &a, at(11)).map(|_| ())),
        SessionError::AlreadyStarted
    );
    assert_eq!(
        reject_unchanged(&mut session, |s| s.record_tool_result(
            wrong,
            &a,
            ToolOutcome::Success("x".into()),
            at(12)
        )),
        SessionError::WrongTurn
    );
    session
        .record_tool_result(turn, &a, result("a").outcome().clone(), at(12))
        .unwrap();
    assert_eq!(
        reject_unchanged(&mut session, |s| s.record_tool_result(
            turn,
            &a,
            ToolOutcome::Success("again".into()),
            at(13)
        )),
        SessionError::AlreadyReturned
    );
    assert_eq!(
        reject_unchanged(&mut session, |s| s.start_tool(turn, &a, at(13)).map(|_| ())),
        SessionError::AlreadyReturned
    );
    session.interrupt(turn, StopReason::UserCancelled).unwrap();
    for op in [0, 1, 2, 3, 4] {
        assert_eq!(
            reject_unchanged(&mut session, |s| match op {
                0 => s.start_tool(turn, &b, at(20)).map(|_| ()),
                1 => s.record_tool_result(turn, &b, ToolOutcome::Success("late".into()), at(20)),
                2 => s.record_assistant(turn, generation(20), batch(&["c"])),
                3 => s.interrupt(turn, StopReason::UserCancelled),
                _ => s.fail(turn, TurnFailure::ModelFailure(text("offline"))),
            }),
            SessionError::TurnClosed
        );
    }
}

#[test]
fn duplicate_active_ids_reject_whole_reply_but_closed_turns_and_sessions_reuse_original_ids() {
    let mut session = session();
    let first = session.start_turn(user("read")).unwrap();
    submit(&mut session, first, batch(&["原始 call / \n"])).unwrap();
    let first_ref = reference(&session, "原始 call / \n");
    finish_tool(&mut session, first, "原始 call / \n");
    let source = next_generation(&session);
    assert_eq!(
        reject_unchanged(&mut session, |s| s.record_assistant(
            first,
            source,
            batch(&["new", "原始 call / \n"])
        )),
        SessionError::DuplicateCallId(id("原始 call / \n"))
    );
    submit(
        &mut session,
        first,
        AssistantMessage::answer("done").unwrap(),
    )
    .unwrap();
    let second = session.start_turn(user("again")).unwrap();
    submit(&mut session, second, batch(&["原始 call / \n"])).unwrap();
    let second_ref = reference(&session, "原始 call / \n");
    assert_ne!(first_ref, second_ref);
    assert_eq!(first_ref.call_id(), second_ref.call_id());
    assert_eq!(
        reject_unchanged(&mut session, |s| s.review_tool(
            second,
            &first_ref,
            generation(99),
            &Decision::new(true, None)
        )),
        SessionError::UnknownCall
    );
    finish_tool(&mut session, second, "原始 call / \n");
    submit(
        &mut session,
        second,
        AssistantMessage::answer("done again").unwrap(),
    )
    .unwrap();
    assert_eq!(Session::restore(session.snapshot()).unwrap(), session);
    let mut other = Session::new(SessionId::new("other").unwrap());
    let turn = other.start_turn(user("separate session")).unwrap();
    other
        .record_assistant(turn, generation(1), batch(&["原始 call / \n"]))
        .unwrap();
}

#[test]
fn source_generation_cannot_be_reused_by_a_second_accepted_step() {
    let mut session = session();
    let turn = session.start_turn(user("read")).unwrap();
    submit(&mut session, turn, batch(&["a"])).unwrap();
    finish_tool(&mut session, turn, "a");
    assert_eq!(
        reject_unchanged(&mut session, |s| s.record_assistant(
            turn,
            generation(1),
            AssistantMessage::answer("done").unwrap()
        )),
        SessionError::DuplicateGeneration(generation(1))
    );
    submit(
        &mut session,
        turn,
        AssistantMessage::answer("done").unwrap(),
    )
    .unwrap();
    let second = session.start_turn(user("again")).unwrap();
    assert_eq!(
        reject_unchanged(&mut session, |s| s.record_assistant(
            second,
            generation(1),
            batch(&["b"])
        )),
        SessionError::DuplicateGeneration(generation(1))
    );
}

#[test]
fn cancellation_preserves_returned_started_and_undispatched_facts() {
    let (mut session, turn) = partial_turn();
    let before = session.messages();
    session.interrupt(turn, StopReason::UserCancelled).unwrap();
    let executions: Vec<_> = session.turns()[0].steps()[0].executions().collect();
    assert!(
        matches!(executions[0].state(), ExecutionState::Returned { outcome: ToolOutcome::Success(output), .. } if output == "result of a")
    );
    assert!(matches!(
        executions[1].state(),
        ExecutionState::Started { .. }
    ));
    assert_eq!(executions[2].state(), &ExecutionState::Requested);
    assert_eq!(session.messages(), before);
    assert_eq!(
        session
            .messages()
            .iter()
            .filter(|m| matches!(m, Message::Tool(_)))
            .count(),
        1
    );
}

#[test]
fn reconciliation_stores_source_and_evidence_without_reopening_or_fake_results() {
    for source in [
        ResolutionSource::Observation,
        ResolutionSource::UserAttestation,
        ResolutionSource::ApplicationAttestation,
    ] {
        let (mut session, turn) = partial_turn();
        let b = reference(&session, "b");
        assert_eq!(
            reject_unchanged(&mut session, |s| s.resolve_effect(
                turn,
                &b,
                resolution(source)
            )),
            SessionError::EffectNotUnknown
        );
        let a = reference(&session, "a");
        let c = reference(&session, "c");
        let missing = reference(&session, "absent");
        session.interrupt(turn, StopReason::ProcessLost).unwrap();
        for call in [&a, &c] {
            assert_eq!(
                reject_unchanged(&mut session, |s| s.resolve_effect(
                    turn,
                    call,
                    resolution(source)
                )),
                SessionError::EffectNotUnknown
            );
        }
        assert_eq!(
            reject_unchanged(&mut session, |s| s.resolve_effect(
                turn,
                &missing,
                resolution(source)
            )),
            SessionError::UnknownCall
        );
        let original = session.messages();
        let fact = resolution(source);
        session.resolve_effect(turn, &b, fact.clone()).unwrap();
        let before = session.snapshot();
        session.resolve_effect(turn, &b, fact.clone()).unwrap();
        assert_eq!(session.snapshot(), before);
        assert_eq!(session.messages(), original);
        assert_eq!(session.next_action(), NextAction::AwaitUser);
        assert_eq!(
            reject_unchanged(&mut session, |s| s.resolve_effect(
                turn,
                &b,
                cola::domain::EffectResolution::new(
                    EffectDisposition::NotPerformed,
                    source,
                    text("conflicting evidence")
                )
            )),
            SessionError::ConflictingResolution
        );
        let execution = session.turns()[0].steps()[0].executions().nth(1).unwrap();
        assert!(matches!(execution.state(), ExecutionState::Started { .. }));
        assert_eq!(execution.resolution(), Some(&fact));
        assert_eq!(fact.source(), source);
        assert_eq!(
            fact.evidence().as_str(),
            "operation log confirms completion"
        );
        assert_eq!(fact.disposition(), EffectDisposition::Completed);
        assert_eq!(Session::restore(session.snapshot()).unwrap(), session);
    }
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
    assert_eq!(session.messages().len(), 6);
    assert_eq!(Session::restore(session.snapshot()).unwrap(), session);
}

#[test]
fn closed_snapshot_roundtrip_preserves_complete_multi_batch_history() {
    let mut session = session();
    let turn = session.start_turn(user("read files")).unwrap();
    for call_id in ["one", "two"] {
        submit(&mut session, turn, batch(&[call_id])).unwrap();
        finish_tool(&mut session, turn, call_id);
    }
    submit(
        &mut session,
        turn,
        AssistantMessage::answer("contents").unwrap(),
    )
    .unwrap();
    let snapshot = session.snapshot();
    assert_eq!(
        Session::restore(snapshot.clone()).unwrap().snapshot(),
        snapshot
    );
}

#[test]
fn restoration_faithfully_retains_every_active_state_without_implicit_process_lost() {
    for phase in 0..5 {
        let mut session = session();
        let turn = session.start_turn(user("question")).unwrap();
        if phase > 0 {
            submit(&mut session, turn, batch(&["a", "b"])).unwrap();
        }
        if phase > 1 {
            approve(&mut session, turn, "a");
        }
        let call = (phase > 0).then(|| reference(&session, "a"));
        if phase > 2 {
            session
                .start_tool(turn, call.as_ref().unwrap(), at(50))
                .unwrap();
        }
        if phase > 3 {
            session
                .record_tool_result(
                    turn,
                    call.as_ref().unwrap(),
                    ToolOutcome::Success("known".into()),
                    at(51),
                )
                .unwrap();
        }
        let snapshot = session.snapshot();
        let restored = Session::restore(snapshot.clone()).unwrap();
        assert_eq!(restored.snapshot(), snapshot);
        assert_eq!(restored.next_action(), session.next_action());
        assert_eq!(restored.active_turn().unwrap().id(), turn);
        assert!(restored.turns().is_empty());
    }
}

#[test]
fn corrupt_snapshots_reject_invalid_values_alignment_order_and_terminal_states() {
    let (mut session, turn) = partial_turn();
    session.interrupt(turn, StopReason::UserCancelled).unwrap();
    let base = session.snapshot();
    let review = || Review::new(generation(90), None);
    type Corruption = Box<dyn Fn(&mut SessionSnapshot)>;
    let corruptions: Vec<Corruption> = vec![
        Box::new(|s| s.session_id = " ".into()),
        Box::new(|s| s.turns[0].id = 0),
        Box::new(|s| s.turns[0].id = 2),
        Box::new(|s| s.turns[0].user = "\t".into()),
        Box::new(|s| s.turns[0].outcome = None),
        Box::new(|s| s.turns[0].outcome = Some(TurnOutcome::Completed)),
        Box::new(|s| {
            s.turns[0].steps[0].executions.pop();
        }),
        Box::new(|s| s.turns[0].steps[0].executions[1].call_id = "other".into()),
        Box::new(|s| s.turns[0].steps[0].executions[0].state = ExecutionState::Requested),
        Box::new(move |s| {
            s.turns[0].steps[0].executions[2].state = ExecutionState::Started {
                review: review(),
                at: at(10),
            }
        }),
        Box::new(move |s| {
            s.turns[0].steps[0].executions[2].state = ExecutionState::Returned {
                review: review(),
                started_at: at(10),
                at: at(11),
                outcome: ToolOutcome::Success("out of order".into()),
            }
        }),
        Box::new(|s| {
            s.turns[0].steps[0].executions[0].resolution =
                Some(resolution(ResolutionSource::Observation))
        }),
        Box::new(|s| {
            let duplicate = s.turns[0].steps[0].clone();
            s.turns[0].steps.push(duplicate);
        }),
        Box::new(|s| s.turns.push(s.turns[0].clone())),
        Box::new(|s| s.active = Some(s.turns[0].clone())),
        Box::new(|s| s.turns[0].steps[0].source_generation = 0),
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
                source_generation: 1,
                assistant: AssistantSnapshot::Answer("done".into()),
                executions: vec![],
            }],
            outcome: None,
        }),
    };
    assert!(Session::restore(snapshot).is_err());
}

#[test]
fn snapshot_rejects_answer_followed_by_steps_and_reused_source_generation() {
    let mut session = session();
    for call_id in ["a", "a"] {
        let turn = session.start_turn(user("read")).unwrap();
        submit(&mut session, turn, batch(&[call_id])).unwrap();
        finish_tool(&mut session, turn, call_id);
        submit(
            &mut session,
            turn,
            AssistantMessage::answer("done").unwrap(),
        )
        .unwrap();
    }
    assert_eq!(Session::restore(session.snapshot()).unwrap(), session);
    let mut duplicate = session.snapshot();
    duplicate.turns[1].steps[0].source_generation = duplicate.turns[0].steps[0].source_generation;
    assert!(Session::restore(duplicate).is_err());
    let mut after_answer = session.snapshot();
    let duplicate = after_answer.turns[0].steps[0].clone();
    after_answer.turns[0].steps.push(duplicate);
    assert!(Session::restore(after_answer).is_err());
}
