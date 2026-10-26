use cola::domain::*;
use serde_json::json;

fn text(value: impl Into<String>) -> NonEmptyText {
    NonEmptyText::new(value).unwrap()
}

struct FactAssistant;
impl Assistant for FactAssistant {
    fn context(&self, session: &Session) -> Result<Context<AssistantMessage>, ContextError> {
        Context::new(
            text("Answer from recorded facts."),
            session.context_exchanges()?,
            vec![],
        )
    }
}

struct FactReviewer;
impl ToolCallReviewer for FactReviewer {
    fn context(
        &self,
        session: &Session,
        call: &ToolCall,
    ) -> Result<Context<Decision>, ContextError> {
        let mut instructions = format!(
            "Review call {} to {} with arguments {}. Session {}.",
            call.id(),
            call.name(),
            serde_json::to_string(call.arguments()).unwrap(),
            session.id(),
        );
        if let Some(turn) = session.active_turn() {
            instructions.push_str(&format!(" User request: {}.", turn.user().text()));
        }
        Context::new(
            text(instructions),
            session.closed_context_exchanges()?,
            vec![],
        )
    }
}

#[test]
fn external_roles_assemble_typed_context_through_trait_objects() {
    let mut session = Session::new(SessionId::new("review-session").unwrap());
    let prior = session
        .start_turn(UserMessage::new(text("Read prior.txt")))
        .unwrap();
    let prior_call = ToolCall::new(
        ToolCallId::new("prior-read").unwrap(),
        ToolName::new("read_file").unwrap(),
        json!({"path":"prior.txt"}),
    )
    .unwrap();
    session
        .record_assistant(
            prior,
            AssistantMessage::tool_requests(
                None,
                ToolCalls::new(vec![prior_call.clone()]).unwrap(),
            ),
        )
        .unwrap();
    session.start_tool(prior, prior_call.id()).unwrap();
    session.interrupt(prior, StopReason::ProcessLost).unwrap();
    let closed = session.closed_context_exchanges().unwrap();
    let turn = session
        .start_turn(UserMessage::new(text("Read secret.txt")))
        .unwrap();
    let assistant: &dyn Assistant = &FactAssistant;
    let input: Context<AssistantMessage> = assistant.context(&session).unwrap();
    assert_eq!(input.messages().count(), 2);
    let target = ToolCall::new(
        ToolCallId::new("read-secret").unwrap(),
        ToolName::new("read_file").unwrap(),
        json!({"path":"secret.txt"}),
    )
    .unwrap();
    session
        .record_assistant(
            turn,
            AssistantMessage::tool_requests(None, ToolCalls::new(vec![target.clone()]).unwrap()),
        )
        .unwrap();
    let before = session.snapshot();
    assert_eq!(
        assistant.context(&session),
        Err(ContextError::UnresolvedToolBatch)
    );
    let reviewer: &dyn ToolCallReviewer = &FactReviewer;
    let input: Context<Decision> = reviewer.context(&session, &target).unwrap();
    let instructions = input.instructions().as_str();
    for fact in ["read-secret", "read_file", "secret.txt", "Read secret.txt"] {
        assert!(instructions.contains(fact), "missing {fact}");
    }
    assert_eq!(input.exchanges(), closed);
    assert_eq!(input.messages().count(), 1);
    assert!(input.execution_notices().any(|notice| matches!(
        notice,
        ExecutionNotice::UnknownEffect { call, resolution: None, .. } if call == &prior_call
    )));
    assert_eq!(session.snapshot(), before);
}

#[test]
fn decisions_preserve_allowance_and_optional_checked_reasons() {
    let allow = Decision::new(true, None);
    assert!(allow.should_allow());
    assert_eq!(allow.reason(), None);
    let reason = text("  secret files need explicit approval  ");
    let deny = Decision::new(false, Some(reason.clone()));
    assert!(!deny.should_allow());
    assert_eq!(deny.reason(), Some(&reason));
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
