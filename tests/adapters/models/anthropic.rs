use super::support::*;
use cola::app::*;
use serde_json::json;
#[test]
fn anthropic_context_window_stop_is_archived_without_partial_output() {
    let server = Server::new(vec![
        json!({"stop_reason":"model_context_window_exceeded","content":[]}).into(),
    ]);
    let root = tempfile::tempdir().unwrap();
    let mut service = service(Provider::Anthropic, &server, root.path());
    let id = service.new_session().unwrap().id().clone();
    service.chat(&id, user("ask")).unwrap_err();
    let doc = service.inspect_session(&id).unwrap();
    assert!(matches!(
        last_outcome(&doc),
        RecordedOutcome::Incomplete(IncompleteReason::Other)
    ));
    assert!(doc.session().turns()[0].steps().is_empty());
}
