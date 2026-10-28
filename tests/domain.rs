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
    AssistantMessage, CallRef, Decision, EffectDisposition, EffectResolution, GenerationId,
    InputSchema, NonEmptyText, ResolutionSource, Session, SessionError, SessionId, Timestamp,
    ToolCall, ToolCallId, ToolCalls, ToolDefinition, ToolName, ToolOutcome, ToolResult, TurnId,
    UserMessage,
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
fn generation(value: u64) -> GenerationId {
    GenerationId::new(value).unwrap()
}
fn at(value: u64) -> Timestamp {
    Timestamp::from_unix_millis(value)
}
fn next_generation(session: &Session) -> GenerationId {
    let snapshot = session.snapshot();
    let largest = snapshot
        .turns
        .iter()
        .chain(snapshot.active.iter())
        .flat_map(|turn| &turn.steps)
        .flat_map(|step| {
            std::iter::once(step.source_generation).chain(step.executions.iter().filter_map(
                |execution| {
                    execution
                        .state
                        .review()
                        .map(|review| review.reviewer().get())
                },
            ))
        })
        .max()
        .unwrap_or(0);
    generation(largest + 1)
}
fn submit(
    session: &mut Session,
    turn: TurnId,
    message: AssistantMessage,
) -> Result<(), SessionError> {
    session.record_assistant(turn, next_generation(session), message)
}
fn reference(session: &Session, value: &str) -> CallRef {
    let step = session.active_turn().unwrap().steps().last().unwrap();
    CallRef::new(step.source_generation(), id(value))
}
fn approve(session: &mut Session, turn: TurnId, value: &str) {
    let call = reference(session, value);
    session
        .review_tool(
            turn,
            &call,
            next_generation(session),
            &Decision::new(true, None),
        )
        .unwrap();
}
fn finish_tool(session: &mut Session, turn: TurnId, value: &str) {
    approve(session, turn, value);
    let call = reference(session, value);
    session.start_tool(turn, &call, at(10)).unwrap();
    session
        .record_tool_result(turn, &call, result(value).outcome().clone(), at(11))
        .unwrap();
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
        .record_assistant(turn, generation(1), batch(&["a", "b", "c"]))
        .unwrap();
    finish_tool(&mut session, turn, "a");
    approve(&mut session, turn, "b");
    let call = reference(&session, "b");
    session.start_tool(turn, &call, at(20)).unwrap();
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
