use super::support::*;
use cola::{
    app::{Generation, IncompleteReason, ModelErrorKind},
    domain::AssistantMessage,
};
use serde_json::json;

#[test]
fn openai_failed_response_is_a_safe_provider_failure() {
    let p = Provider::OpenAI;
    let server = Server::new(vec![
        json!({"status":"failed","error":{"code":"server_error","message":"BODY_SENTINEL"}}).into(),
    ]);
    let error = p
        .client(&server)
        .generate(&context(&session()), &p.settings(), None)
        .unwrap_err();
    assert_eq!(error.kind(), ModelErrorKind::ProviderFailure);
    assert_safe(&error);
    assert_eq!(server.requests().len(), 1);
}

#[test]
fn openai_optional_native_metadata_survives_checked_output_and_replay() {
    let p = Provider::OpenAI;
    for mode in ["omitted", "null", "completed"] {
        let mut response = p.tool(1);
        let blocks = response["output"].as_array_mut().unwrap();
        for block in blocks {
            if block["type"] == "reasoning" || block["type"] == "function_call" {
                match mode {
                    "omitted" => {
                        block.as_object_mut().unwrap().remove("status");
                    }
                    "null" => block["status"] = serde_json::Value::Null,
                    _ => block["status"] = json!("completed"),
                }
            }
            if block["type"] == "function_call" {
                match mode {
                    "omitted" => {
                        block.as_object_mut().unwrap().remove("id");
                    }
                    "null" => block["id"] = serde_json::Value::Null,
                    _ => {}
                }
            }
        }
        let expected = p.native(&response).as_array().unwrap().clone();
        let server = Server::new(vec![response.into(), p.answer("done").into()]);
        let client = p.client(&server);
        let mut s = session();
        let (output, token) = complete(client.generate(&context(&s), &p.settings(), None).unwrap());
        assert_eq!(output.calls().unwrap().as_slice().len(), 1);
        accept(&mut s, output);
        let (answer, _) = complete(
            client
                .generate(&context(&s), &p.settings(), token.as_ref())
                .unwrap(),
        );
        assert_eq!(answer, AssistantMessage::answer("done").unwrap());
        assert_eq!(p.request_native(&server.request(1).body), expected);
    }
    for status in ["in_progress", "incomplete"] {
        let mut response = p.tool(1);
        response["output"][0]["status"] = json!(status);
        let server = Server::new(vec![response.into()]);
        let result = p
            .client(&server)
            .generate(&context(&session()), &p.settings(), None)
            .unwrap();
        assert!(matches!(
            result,
            Generation::Incomplete {
                reason: IncompleteReason::Other
            }
        ));
    }
}
