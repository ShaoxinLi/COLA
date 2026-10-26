use cola::domain::*;
use serde_json::json;

fn main() -> Result<(), Box<dyn std::error::Error>> {
    let mut session = Session::new(SessionId::new("read-file-example")?);
    let turn = session.start_turn(UserMessage::new(NonEmptyText::new("Read notes.txt.")?))?;
    let call_id = ToolCallId::new("read-1")?;
    let call = ToolCall::new(
        call_id.clone(),
        ToolName::new("read_file")?,
        json!({"path":"notes.txt"}),
    )?;
    session.record_assistant(
        turn,
        AssistantMessage::tool_requests(None, ToolCalls::new(vec![call])?),
    )?;
    assert_eq!(session.next_action(), NextAction::Execute(call_id.clone()));
    session.start_tool(turn, &call_id)?;
    session.record_tool_result(
        turn,
        ToolResult::new(call_id, ToolOutcome::Success("Meeting at 10.".into())),
    )?;
    assert_eq!(session.next_action(), NextAction::AskModel);
    session.record_assistant(
        turn,
        AssistantMessage::answer("notes.txt says: Meeting at 10.")?,
    )?;
    let context = Context::<AssistantMessage>::new(
        NonEmptyText::new("Answer using recorded facts.")?,
        session.context_exchanges()?,
        vec![],
    )?;
    assert_eq!(context.messages().count(), 4);
    assert_eq!(session.turns()[0].outcome(), &TurnOutcome::Completed);
    println!("Recorded 4 messages. Turn 1 is Completed. No file was read.");
    Ok(())
}
