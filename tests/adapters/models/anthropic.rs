use super::support::*;
use cola::app::{Generation, IncompleteReason};
use serde_json::json;

#[test]
fn anthropic_context_window_stop_never_returns_partial_output() {
    let p = Provider::Anthropic;
    let response = json!({
        "stop_reason": "model_context_window_exceeded",
        "content": [{"type":"tool_use","id":"partial","name":"lookup","input":{}}],
    });
    let server = Server::new(vec![response.clone().into(), response.into()]);
    let assistant = p
        .client(&server)
        .request(&context(&session()), &p.settings())
        .unwrap();
    let reviewer = p
        .reviewer(&server)
        .request(
            &context::<cola::domain::Decision>(&session()),
            &p.settings(),
        )
        .unwrap();
    assert!(matches!(
        assistant,
        Generation::Incomplete {
            reason: IncompleteReason::Other
        }
    ));
    assert!(matches!(
        reviewer,
        Generation::Incomplete {
            reason: IncompleteReason::Other
        }
    ));
}
