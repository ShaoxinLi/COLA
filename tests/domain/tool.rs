use super::support::{call, definition, id, session, text, user};
use cola::domain::{
    AssistantMessage, ExecutionState, InputSchema, Message, NextAction, Tool, ToolCall, ToolCalls,
    ToolError, ToolName, ToolOutcome, ToolResult, ValueError,
};
use serde_json::{Map, Value, json};

#[test]
fn external_tool_executes_arguments_and_records_success_or_failure() {
    struct Echo;
    impl Tool for Echo {
        fn execute(&self, arguments: &Map<String, Value>) -> ToolOutcome {
            match arguments.get("text").and_then(Value::as_str) {
                Some(text) => ToolOutcome::Success(text.to_owned()),
                None => ToolOutcome::Failure(ToolError::new(text("text must be a string"))),
            }
        }
    }

    let tool: &dyn Tool = &Echo;
    for (arguments, expected) in [
        (
            json!({"text": "hello"}),
            ToolOutcome::Success("hello".into()),
        ),
        (json!({"text": ""}), ToolOutcome::Success(String::new())),
        (
            json!({}),
            ToolOutcome::Failure(ToolError::new(text("text must be a string"))),
        ),
        (
            json!({"text": 42}),
            ToolOutcome::Failure(ToolError::new(text("text must be a string"))),
        ),
    ] {
        let mut session = session();
        let turn = session.start_turn(user("echo the text")).unwrap();
        let call =
            ToolCall::new(id("echo-call"), ToolName::new("echo").unwrap(), arguments).unwrap();
        session
            .record_assistant(
                turn,
                AssistantMessage::tool_requests(None, ToolCalls::new(vec![call]).unwrap()),
            )
            .unwrap();
        let call = session.start_tool(turn, &id("echo-call")).unwrap();
        let result = ToolResult::new(call.id().clone(), tool.execute(call.arguments()));
        session.record_tool_result(turn, result).unwrap();

        assert_eq!(
            session.messages().last(),
            Some(&Message::Tool(ToolResult::new(
                id("echo-call"),
                expected.clone()
            )))
        );
        assert_eq!(
            session.active_turn().unwrap().steps()[0]
                .executions()
                .next()
                .unwrap()
                .state(),
            &ExecutionState::Returned(expected)
        );
        assert_eq!(session.next_action(), NextAction::AskModel);
    }
}

#[test]
fn tool_constructors_validate_calls_and_schema() {
    assert_eq!(ToolCalls::new(vec![]), Err(ValueError::EmptyToolCalls));
    assert!(matches!(
        ToolCalls::new(vec![call("a"), call("a")]),
        Err(ValueError::DuplicateToolCallId(_))
    ));
    assert!(matches!(
        ToolCall::new(id("a"), ToolName::new("read").unwrap(), json!([])),
        Err(ValueError::ExpectedObject)
    ));
    assert!(InputSchema::new(json!({"type":"object", "properties":false})).is_ok());
    for schema in [json!(true), json!({}), json!({"type":"array"})] {
        assert!(InputSchema::new(schema).is_err());
    }
    let tool = definition("read_file");
    assert_eq!(tool.description().as_str(), "Read a file");
    assert_eq!(
        tool.input_schema().document().get("type"),
        Some(&json!("object"))
    );
}
