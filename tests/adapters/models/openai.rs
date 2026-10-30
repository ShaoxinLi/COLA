use super::support::*;
use cola::app::*;
use serde_json::json;
#[test]
fn openai_failed_response_is_audited_as_provider_failure() {
    let server = Server::new(vec![
        json!({"status":"failed","model":"actual-model","usage":{"input_tokens":9},"output":[]})
            .into(),
    ]);
    let root = tempfile::tempdir().unwrap();
    let mut service = service(Provider::OpenAI, &server, root.path());
    let id = service.new_session().unwrap().id().clone();
    service.chat(&id, user("ask")).unwrap_err();
    let doc = service.inspect_session(&id).unwrap();
    let InvocationState::Finished {
        response, outcome, ..
    } = &doc.invocations()[0].state
    else {
        panic!()
    };
    assert_eq!(response.reported_model.as_deref(), Some("actual-model"));
    assert!(response.usage.is_some());
    assert!(
        matches!(outcome,RecordedOutcome::Failed(e) if e.kind()==ModelErrorKind::ProviderFailure)
    );
}
