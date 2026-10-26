use cola::{
    adapters::stores::JsonlSessionStore,
    app::SessionStore,
    domain::{AssistantMessage, NonEmptyText, Session, SessionId, UserMessage},
};

fn main() -> Result<(), Box<dyn std::error::Error>> {
    let root = std::env::args()
        .nth(1)
        .unwrap_or_else(|| ".cola/sessions".into());
    let store = JsonlSessionStore::new(root);
    let mut session = Session::new(SessionId::new("example/会话")?);
    let turn = session.start_turn(UserMessage::new(NonEmptyText::new("Hello")?))?;
    session.record_assistant(turn, AssistantMessage::answer("Hello from COLA")?)?;
    store.save(&session)?;
    let loaded = store.load(session.id())?;
    assert_eq!(loaded, session);
    println!(
        "Recovered {} with {} closed turn(s)",
        loaded.id(),
        loaded.turns().len()
    );
    Ok(())
}
