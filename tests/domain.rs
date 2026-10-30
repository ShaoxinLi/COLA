#[path = "domain/context.rs"]
mod context;
#[path = "domain/conversation.rs"]
mod conversation;
#[path = "domain/message.rs"]
mod message;
#[path = "domain/roles.rs"]
mod roles;
#[path = "domain/tool.rs"]
mod tool;
#[path = "domain/value.rs"]
mod value;

use cola::domain::{
    AssistantMessage, EffectDisposition, EffectResolution, InputSchema, NonEmptyText,
    ResolutionSource, Session, SessionError, SessionId, ToolCall, ToolCallId, ToolCalls,
    ToolDefinition, ToolName, ToolOutcome, ToolResult, TurnId, UserMessage,
};
use serde_json::json;

fn text(value: impl Into<String>) -> NonEmptyText {
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
