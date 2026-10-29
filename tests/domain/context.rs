use super::{
    batch, call, definition, finish_tool, id, partial_turn, resolution, result, session, text, user,
};
use cola::domain::{
    AssistantMessage, Context, ContextError, ContextExchange, ExecutionNotice, Message,
    ResolutionSource, StopReason, TurnId, TurnOutcome,
};

#[test]
fn context_rejects_orphans_bad_order_unresolved_batches_and_inconsistent_outcomes() {
    let turn = TurnId::new(1).unwrap();
    let answer = Message::Assistant(AssistantMessage::answer("done").unwrap());
    let invalid = vec![
        (vec![], None, ContextError::MissingUser),
        (
            vec![Message::Tool(result("a"))],
            None,
            ContextError::MissingUser,
        ),
        (
            vec![Message::User(user("read")), Message::Tool(result("a"))],
            None,
            ContextError::InvalidMessageOrder,
        ),
        (
            vec![
                Message::User(user("read")),
                Message::Assistant(batch(&["a"])),
            ],
            None,
            ContextError::UnresolvedToolBatch,
        ),
        (
            vec![
                Message::User(user("read")),
                Message::Assistant(batch(&["a"])),
                Message::Tool(result("b")),
            ],
            None,
            ContextError::MismatchedToolResult,
        ),
        (
            vec![
                Message::User(user("read")),
                Message::Assistant(batch(&["a", "b"])),
                Message::Tool(result("b")),
                Message::Tool(result("a")),
            ],
            None,
            ContextError::MismatchedToolResult,
        ),
        (
            vec![Message::User(user("read")), Message::User(user("again"))],
            None,
            ContextError::InvalidMessageOrder,
        ),
        (
            vec![Message::User(user("read")), answer.clone(), answer.clone()],
            Some(TurnOutcome::Completed),
            ContextError::InvalidMessageOrder,
        ),
        (
            vec![Message::User(user("read")), answer.clone()],
            None,
            ContextError::InvalidOutcome,
        ),
        (
            vec![Message::User(user("read"))],
            Some(TurnOutcome::Completed),
            ContextError::InvalidOutcome,
        ),
        (
            vec![Message::User(user("read")), answer],
            Some(TurnOutcome::Interrupted(StopReason::UserCancelled)),
            ContextError::InvalidOutcome,
        ),
    ];
    for (messages, outcome, expected) in invalid {
        assert_eq!(ContextExchange::new(turn, messages, outcome), Err(expected));
    }
    let duplicate = vec![
        Message::User(user("read")),
        Message::Assistant(batch(&["a"])),
        Message::Tool(result("a")),
        Message::Assistant(batch(&["a"])),
        Message::Tool(result("a")),
    ];
    assert!(ContextExchange::new(turn, duplicate, None).is_ok());
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
    let context = Context::<AssistantMessage>::new(
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
        Context::<AssistantMessage>::new(
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
    assert!(
        Context::<AssistantMessage>::new(
            text("rules"),
            vec![complete(1, "a"), complete(2, "a")],
            vec![]
        )
        .is_ok()
    );
    assert_eq!(
        Context::<AssistantMessage>::new(
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
        Context::<AssistantMessage>::new(text("rules"), vec![active, complete(2, "b")], vec![]),
        Err(ContextError::ActiveExchangeNotLast)
    );
    let (mut cancelled, turn) = partial_turn();
    cancelled
        .interrupt(turn, StopReason::UserCancelled)
        .unwrap();
    let mut exchanges = cancelled.context_exchanges().unwrap();
    exchanges.push(complete(2, "b"));
    assert!(Context::<AssistantMessage>::new(text("rules"), exchanges, vec![]).is_ok());
}

#[test]
fn closed_projection_retains_recovery_facts_while_an_active_batch_is_pending() {
    let (mut session, closed) = partial_turn();
    session.interrupt(closed, StopReason::ProcessLost).unwrap();
    let fact = resolution(ResolutionSource::Observation);
    session
        .resolve_effect(&super::address(&session, closed, &id("b")), fact.clone())
        .unwrap();
    let expected = session.context_exchanges().unwrap();
    let active = session.start_turn(user("read another file")).unwrap();
    session
        .record_assistant(active, batch(&["pending"]))
        .unwrap();
    session
        .start_tool(&super::address(&session, active, &id("pending")))
        .unwrap();
    let before = session.snapshot();
    let exchanges = session.closed_context_exchanges().unwrap();
    assert_eq!(exchanges, expected);
    assert_eq!(exchanges.len(), 1);
    assert_eq!(
        exchanges[0].messages(),
        &[Message::User(user("read three files"))]
    );
    assert!(
        matches!(&exchanges[0].notices()[0], ExecutionNotice::TurnClosed { turn_id, outcome: TurnOutcome::Interrupted(StopReason::ProcessLost) } if *turn_id == closed)
    );
    assert!(
        matches!(&exchanges[0].notices()[1], ExecutionNotice::ConfirmedResult { call: c, result: r, .. } if c == &call("a") && r == &result("a"))
    );
    assert!(
        matches!(&exchanges[0].notices()[2], ExecutionNotice::UnknownEffect { call: c, resolution: Some(r), .. } if c == &call("b") && r == &fact)
    );
    assert!(
        matches!(&exchanges[0].notices()[3], ExecutionNotice::Undispatched { call: c, .. } if c == &call("c"))
    );
    assert_eq!(
        session.context_exchanges(),
        Err(ContextError::UnresolvedToolBatch)
    );
    assert_eq!(session.snapshot(), before);
    session
        .record_tool_result(
            &super::address(&session, active, (result("pending")).call_id()),
            (result("pending")).outcome().clone(),
        )
        .unwrap();
    let all = session.context_exchanges().unwrap();
    assert_eq!(&all[..1], exchanges.as_slice());
    assert_eq!(all[1].turn_id(), active);
    assert_eq!(all[1].messages().len(), 3);
    assert_eq!(session.closed_context_exchanges().unwrap(), exchanges);
}

#[test]
fn context_operations_do_not_require_output_traits() {
    struct Output;
    fn assert_eq_contract<T: Eq>(_: &T) {}
    let context = Context::<Output>::new(text("rules"), vec![], vec![]).unwrap();
    let cloned = context.clone();
    assert_eq!(cloned, context);
    assert_eq_contract(&context);
    assert!(format!("{context:?}").contains("rules"));
    assert_eq!(context.instructions().as_str(), "rules");
    assert!(context.exchanges().is_empty());
    assert!(context.tools().is_empty());
    assert_eq!(context.messages().count(), 0);
    assert_eq!(context.execution_notices().count(), 0);
}
