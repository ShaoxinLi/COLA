use super::support::{
    batch, finish_tool, id, partial_turn, reject_unchanged, resolution, session, user,
};
use cola::domain::{
    AssistantMessage, AssistantSnapshot, ExecutionState, NextAction, ResolutionSource,
    RestoreError, Session, SessionError, SessionId, SessionSnapshot, StepSnapshot, StopReason,
    ToolCallSnapshot, ToolOutcome, TurnOutcome, TurnSnapshot,
};
use serde_json::json;

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
