use cola::{app::*, domain::*};

fn user(s: &str) -> UserMessage {
    UserMessage::new(NonEmptyText::new(s).unwrap())
}

#[test]
fn context_selection_pins_unresolved_effect_turns_outside_optional_window() {
    let mut session = Session::new(SessionId::new("selection").unwrap());
    let turn = session.start_turn(user("mandatory")).unwrap();
    session
        .record_assistant(
            turn,
            AssistantMessage::tool_requests(
                None,
                ToolCalls::new(vec![
                    ToolCall::new(
                        ToolCallId::new("native").unwrap(),
                        ToolName::new("lookup").unwrap(),
                        serde_json::json!({}),
                    )
                    .unwrap(),
                ])
                .unwrap(),
            ),
        )
        .unwrap();
    let address = session
        .call_address(ToolCallId::new("native").unwrap())
        .unwrap();
    session.start_tool(&address).unwrap();
    session.interrupt(turn, StopReason::ProcessLost).unwrap();
    for _ in 0..12 {
        let t = session.start_turn(user("optional")).unwrap();
        session
            .record_assistant(t, AssistantMessage::answer("done").unwrap())
            .unwrap();
    }
    let selection = RecentContextPolicy::default().select(&session);
    assert_eq!(selection.mandatory_turns, vec![turn]);
    assert_eq!(selection.closed_turns.len(), 9);
    assert_eq!(selection.closed_turns[0], turn);
    let fitted = selection.fit(|ids| Ok(ids.len() * 10000)).unwrap();
    assert!(fitted.contains(&turn));
    assert_eq!(fitted.len(), 6);
}

fn closed_turn(session: &mut Session, prompt: &str) -> TurnId {
    let turn = session.start_turn(user(prompt)).unwrap();
    session
        .record_assistant(turn, AssistantMessage::answer("done").unwrap())
        .unwrap();
    turn
}

#[test]
fn defaults_bound_optional_history_and_request_bytes() {
    let policy = RecentContextPolicy::default();
    assert_eq!(policy.optional_turns, 8);
    assert_eq!(policy.max_request_bytes, 64 * 1024);
    let session = Session::new(SessionId::new("empty").unwrap());
    let selection = policy.select(&session);
    assert!(selection.closed_turns.is_empty());
    assert!(selection.mandatory_turns.is_empty());
    assert_eq!(selection.max_request_bytes, 64 * 1024);
}

#[test]
fn selection_keeps_recent_closed_turns_in_order_and_excludes_active_turn() {
    let mut session = Session::new(SessionId::new("recent").unwrap());
    closed_turn(&mut session, "oldest");
    let second = closed_turn(&mut session, "second");
    let third = closed_turn(&mut session, "third");
    session.start_turn(user("active")).unwrap();
    let policy = RecentContextPolicy {
        optional_turns: 2,
        max_request_bytes: 512,
    };
    let selection = policy.select(&session);
    assert_eq!(selection.closed_turns, vec![second, third]);
    assert!(selection.mandatory_turns.is_empty());
    assert_eq!(selection.max_request_bytes, 512);
    assert!(
        RecentContextPolicy {
            optional_turns: 0,
            ..policy
        }
        .select(&session)
        .closed_turns
        .is_empty()
    );
}

#[test]
fn resolved_effects_leave_the_mandatory_selection() {
    let mut session = Session::new(SessionId::new("resolution").unwrap());
    let turn = session.start_turn(user("effect")).unwrap();
    session
        .record_assistant(
            turn,
            AssistantMessage::tool_requests(
                None,
                ToolCalls::new(vec![
                    ToolCall::new(
                        ToolCallId::new("call").unwrap(),
                        ToolName::new("lookup").unwrap(),
                        serde_json::json!({}),
                    )
                    .unwrap(),
                ])
                .unwrap(),
            ),
        )
        .unwrap();
    let address = session
        .call_address(ToolCallId::new("call").unwrap())
        .unwrap();
    session.start_tool(&address).unwrap();
    session.interrupt(turn, StopReason::ProcessLost).unwrap();
    let policy = RecentContextPolicy {
        optional_turns: 0,
        max_request_bytes: 1024,
    };
    assert_eq!(policy.select(&session).mandatory_turns, vec![turn]);
    session
        .resolve_effect(
            &address,
            EffectResolution::new(
                EffectDisposition::NotPerformed,
                ResolutionSource::UserAttestation,
                NonEmptyText::new("operator checked").unwrap(),
            ),
        )
        .unwrap();
    let selection = policy.select(&session);
    assert!(selection.mandatory_turns.is_empty());
    assert!(selection.closed_turns.is_empty());
}

#[test]
fn fitting_drops_oldest_optional_whole_turns_and_keeps_mandatory_turns() {
    let ids: Vec<_> = (1..=4).map(|n| TurnId::new(n).unwrap()).collect();
    let selection = ContextSelection {
        closed_turns: ids.clone(),
        mandatory_turns: vec![ids[0], ids[2]],
        max_request_bytes: 30,
    };
    let fitted = selection.fit(|turns| Ok(turns.len() * 10)).unwrap();
    assert_eq!(fitted, vec![ids[0], ids[2], ids[3]]);
    assert_eq!(selection.closed_turns, ids);
}

#[test]
fn fitting_rejects_missing_mandatory_duplicate_and_unordered_selections() {
    let first = TurnId::new(1).unwrap();
    let second = TurnId::new(2).unwrap();
    for (closed_turns, mandatory_turns) in [
        (vec![first], vec![second]),
        (vec![first, first], vec![]),
        (vec![second, first], vec![]),
    ] {
        let selection = ContextSelection {
            closed_turns,
            mandatory_turns,
            max_request_bytes: 100,
        };
        let error = selection
            .fit(|_| panic!("invalid selection reached renderer"))
            .unwrap_err();
        assert_eq!(error.kind(), ModelErrorKind::InvalidSelection);
    }
}

#[test]
fn fitting_reports_required_content_overflow_and_propagates_renderer_errors() {
    let first = TurnId::new(1).unwrap();
    for (closed_turns, mandatory_turns) in [(vec![first], vec![first]), (vec![], vec![])] {
        let selection = ContextSelection {
            closed_turns,
            mandatory_turns,
            max_request_bytes: 100,
        };
        assert_eq!(
            selection.fit(|_| Ok(101)).unwrap_err().kind(),
            ModelErrorKind::ContextSelectionTooLarge
        );
        assert_eq!(selection.fit(|_| Ok(100)).unwrap(), selection.closed_turns);
        assert_eq!(
            selection
                .fit(|_| Err(ModelError::new(ModelErrorKind::UnsupportedInput)))
                .unwrap_err()
                .kind(),
            ModelErrorKind::UnsupportedInput
        );
    }
}
