use super::support::*;
use cola::{adapters::models::ClientConfig, app::*};
use serde_json::{Value, json};
use std::{num::NonZeroUsize, time::Duration};

#[test]
fn config_rejects_unsafe_urls_secrets_and_limits_with_redacted_diagnostics() {
    for url in [
        "file:///tmp/test",
        "http://user:secret@localhost",
        "http://localhost/?secret=x",
        "http://localhost/#secret",
    ] {
        let error = ClientConfig::new(
            url,
            "secret",
            Duration::from_secs(1),
            NonZeroUsize::new(100).unwrap(),
        )
        .unwrap_err();
        assert_eq!(error.kind(), ModelErrorKind::InvalidConfig);
        assert_safe(&error);
    }
    assert!(
        ClientConfig::new(
            "http://localhost",
            "bad\nheader",
            Duration::from_secs(1),
            NonZeroUsize::new(100).unwrap()
        )
        .is_err()
    );
}
#[test]
fn http_failures_are_audited_without_retry_and_do_not_leak_to_errors() {
    for provider in PROVIDERS {
        for (status, expected) in [
            (400, ModelErrorKind::InvalidSettings),
            (401, ModelErrorKind::Authentication),
            (403, ModelErrorKind::Authentication),
            (429, ModelErrorKind::RateLimited),
            (500, ModelErrorKind::ProviderFailure),
            (302, ModelErrorKind::Http),
        ] {
            let mut reply: Reply = json!({"error":"BODY_SENTINEL"}).into();
            reply.status = status;
            let server = Server::new(vec![reply]);
            let root = tempfile::tempdir().unwrap();
            let mut service = service(provider, &server, root.path());
            let id = service.new_session().unwrap().id().clone();
            let error = service.chat(&id, user("ask")).unwrap_err();
            let AgentErrorKind::Model { source, .. } = error.kind() else {
                panic!("{error:?}")
            };
            assert_eq!(source.kind(), expected);
            assert_safe(source);
            assert_eq!(server.requests().len(), 1);
            let doc = service.inspect_session(&id).unwrap();
            let InvocationState::Finished {
                request, response, ..
            } = &doc.invocations()[0].state
            else {
                panic!()
            };
            assert!(request.is_some());
            assert_eq!(response.http_status, Some(status));
            assert!(
                String::from_utf8_lossy(&response.response.as_ref().unwrap().bytes)
                    .contains("BODY_SENTINEL")
            );
            assert!(doc.session().active_turn().is_none());
        }
    }
}
#[test]
fn decoding_failures_refusal_and_incomplete_responses_are_archived_without_steps() {
    for provider in PROVIDERS {
        let bad = match provider {
            Provider::OpenAI => vec![
                json!({"status":"completed","output":[{"type":"unknown"}]}),
                json!({"status":"incomplete","output":[],"incomplete_details":{"reason":"max_output_tokens"}}),
                json!({"status":"completed","output":[{"type":"message","role":"assistant","content":[{"type":"refusal","refusal":"no"}]}]}),
            ],
            Provider::Anthropic => vec![
                json!({"type":"message","role":"assistant","stop_reason":"end_turn","content":[{"type":"unknown"}]}),
                json!({"stop_reason":"max_tokens","content":[]}),
                json!({"stop_reason":"refusal","content":[]}),
            ],
        };
        for (index, response) in bad.into_iter().enumerate() {
            let server = Server::new(vec![response.clone().into()]);
            let root = tempfile::tempdir().unwrap();
            let mut service = service(provider, &server, root.path());
            let id = service.new_session().unwrap().id().clone();
            assert!(service.chat(&id, user("ask")).is_err());
            let doc = service.inspect_session(&id).unwrap();
            assert!(doc.session().turns()[0].steps().is_empty());
            let InvocationState::Finished {
                response: evidence,
                outcome,
                ..
            } = &doc.invocations()[0].state
            else {
                panic!()
            };
            assert_eq!(
                serde_json::from_slice::<Value>(&evidence.response.as_ref().unwrap().bytes)
                    .unwrap(),
                response
            );
            match index {
                0 => assert!(matches!(outcome, RecordedOutcome::Failed(_))),
                1 => assert!(matches!(outcome, RecordedOutcome::Incomplete(_))),
                _ => assert!(matches!(outcome, RecordedOutcome::Refused)),
            }
        }
    }
}
#[test]
fn invalid_native_ids_arguments_names_and_unknown_tools_never_dispatch() {
    for provider in PROVIDERS {
        for mutation in 0..5 {
            let mut response = provider.tool(1);
            let call = match provider {
                Provider::OpenAI => response["output"]
                    .as_array_mut()
                    .unwrap()
                    .last_mut()
                    .unwrap(),
                Provider::Anthropic => response["content"]
                    .as_array_mut()
                    .unwrap()
                    .last_mut()
                    .unwrap(),
            };
            match mutation {
                0 => {
                    call[match provider {
                        Provider::OpenAI => "call_id",
                        Provider::Anthropic => "id",
                    }] = json!("");
                }
                1 => {
                    call[match provider {
                        Provider::OpenAI => "arguments",
                        Provider::Anthropic => "input",
                    }] = match provider {
                        Provider::OpenAI => json!("[]"),
                        Provider::Anthropic => json!([]),
                    };
                }
                2 => call["name"] = json!("bad name"),
                3 => call["name"] = json!("unknown"),
                _ => {
                    let duplicate = call.clone();
                    match provider {
                        Provider::OpenAI => {
                            response["output"].as_array_mut().unwrap().push(duplicate)
                        }
                        Provider::Anthropic => {
                            response["content"].as_array_mut().unwrap().push(duplicate)
                        }
                    }
                }
            }
            let server = Server::new(vec![response.into()]);
            let root = tempfile::tempdir().unwrap();
            let mut service = service(provider, &server, root.path());
            let id = service.new_session().unwrap().id().clone();
            assert!(service.chat(&id, user("ask")).is_err());
            assert_eq!(server.requests().len(), 1);
            assert!(
                service.inspect_session(&id).unwrap().session().turns()[0]
                    .steps()
                    .is_empty()
            );
        }
    }
}
#[test]
fn native_chains_preserve_ids_and_raw_evidence_while_new_turns_quote_history() {
    for provider in PROVIDERS {
        let tool_response = provider.tool(1);
        let server = Server::new(vec![
            tool_response.clone().into(),
            decision(provider, true).into(),
            provider.answer("done").into(),
            tool_response.clone().into(),
            decision(provider, false).into(),
            provider.answer("next").into(),
        ]);
        let root = tempfile::tempdir().unwrap();
        let id;
        {
            let mut service = service(provider, &server, root.path());
            id = service.new_session().unwrap().id().clone();
            service.chat(&id, user("first")).unwrap();
        }
        let mut reloaded = service(provider, &server, root.path());
        reloaded.chat(&id, user("second")).unwrap();
        let doc = reloaded.inspect_session(&id).unwrap();
        assert_eq!(doc.session().turns().len(), 2);
        assert_eq!(doc.invocations().len(), 6);
        for turn in doc.session().turns() {
            assert_eq!(
                turn.steps()[0].assistant().calls().unwrap().as_slice()[0]
                    .id()
                    .as_str(),
                "native-1"
            );
        }
        let continuation = server.request(2).body;
        let new_turn = server.request(3).body;
        match provider {
            Provider::OpenAI => {
                assert!(
                    continuation["input"]
                        .as_array()
                        .unwrap()
                        .iter()
                        .any(|v| v == &tool_response["output"][0])
                );
                assert!(
                    !new_turn["input"]
                        .as_array()
                        .unwrap()
                        .iter()
                        .any(|v| v["type"] == "function_call")
                );
            }
            Provider::Anthropic => {
                assert_eq!(
                    continuation["messages"][1]["content"],
                    tool_response["content"]
                );
                assert!(!new_turn.to_string().contains("signature-1"));
            }
        }
        assert!(new_turn.to_string().contains("Quoted closed-turn history"));
        assert!(new_turn.to_string().contains("observed target result"));
        for (invocation, request) in doc.invocations().iter().zip(server.requests()) {
            let InvocationState::Finished {
                request: Some(saved),
                response,
                ..
            } = &invocation.state
            else {
                panic!()
            };
            assert_eq!(
                serde_json::from_slice::<Value>(&saved.body.bytes).unwrap(),
                request.body
            );
            assert_eq!(response.reported_model, None);
        }
    }
}
#[test]
fn full_body_budget_drops_whole_optional_turns_and_rejects_growing_native_chain() {
    for provider in PROVIDERS {
        let server = Server::new(vec![
            provider.answer("x").into(),
            provider.answer("y").into(),
            provider.answer("z").into(),
        ]);
        let root = tempfile::tempdir().unwrap();
        let mut service =
            service(provider, &server, root.path()).with_context_policy(RecentContextPolicy {
                optional_turns: 1,
                max_request_bytes: 4096,
            });
        let id = service.new_session().unwrap().id().clone();
        for text in ["FIRST_SENTINEL", "SECOND_SENTINEL", "THIRD_SENTINEL"] {
            service.chat(&id, user(text)).unwrap();
        }
        assert!(
            !server
                .request(2)
                .body
                .to_string()
                .contains("FIRST_SENTINEL")
        );
        assert!(
            server
                .request(2)
                .body
                .to_string()
                .contains("SECOND_SENTINEL")
        );
        let doc = service.inspect_session(&id).unwrap();
        assert_eq!(doc.views()[2].selection, vec![2]);
        let count = server.requests().len();
        let mut tiny = service.with_context_policy(RecentContextPolicy {
            optional_turns: 8,
            max_request_bytes: 1,
        });
        assert!(tiny.chat(&id, user("too large")).is_err());
        assert_eq!(server.requests().len(), count);
        assert!(
            matches!(last_outcome(&tiny.inspect_session(&id).unwrap()),RecordedOutcome::PreparationFailed(e) if e.kind()==ModelErrorKind::ContextSelectionTooLarge)
        );
    }
}
#[test]
fn repeated_ids_within_one_native_chain_are_adapter_rejections() {
    for provider in PROVIDERS {
        let server = Server::new(vec![
            provider.tool(1).into(),
            decision(provider, true).into(),
            provider.tool(1).into(),
        ]);
        let root = tempfile::tempdir().unwrap();
        let mut service = service(provider, &server, root.path());
        let id = service.new_session().unwrap().id().clone();
        service.chat(&id, user("repeat")).unwrap_err();
        let doc = service.inspect_session(&id).unwrap();
        assert_eq!(doc.session().turns()[0].steps().len(), 1);
        assert!(
            matches!(last_outcome(&doc),RecordedOutcome::Failed(e) if e.kind()==ModelErrorKind::NativeIdCollision)
        );
    }
}
