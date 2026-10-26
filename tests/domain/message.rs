use super::call;
use cola::domain::{AssistantMessage, ToolCalls};

#[test]
fn message_constructors_normalize_preambles_and_validate_answers() {
    let request = AssistantMessage::tool_requests(
        Some(" \n".into()),
        ToolCalls::new(vec![call("a")]).unwrap(),
    );
    assert!(matches!(
        request,
        AssistantMessage::ToolRequests { text: None, .. }
    ));
    assert!(AssistantMessage::answer("  ").is_err());
}
