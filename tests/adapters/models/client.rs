use super::support::*;
use cola::{
    adapters::{
        models::{ClientConfig, OpenAIClient},
        stores::JsonlSessionStore,
    },
    app::*,
    domain::*,
};
use serde_json::{Value, json};
use std::{num::NonZeroUsize, time::Duration};

#[test]
fn config_rejects_unsafe_urls_secrets_and_limits_with_redacted_diagnostics() {
    for url in [
        "file:///tmp/key",
        "http://@localhost/v1",
        "ftp://example.com",
        "http://user:pass@localhost/v1",
        "http://localhost/v1?secret=URL_SENTINEL",
        "http://localhost/v1#URL_SENTINEL",
        "http://[invalid",
    ] {
        let error = ClientConfig::new(
            url,
            "API_KEY_SENTINEL",
            Duration::from_secs(1),
            NonZeroUsize::new(1024).unwrap(),
        )
        .unwrap_err();
        assert_eq!(error.kind(), ModelErrorKind::InvalidConfig);
        assert_safe(&error);
    }
    for key in ["", "  ", "API_KEY_SENTINEL\r\nx-leak: true"] {
        assert_eq!(
            ClientConfig::new(
                "https://example.com/v1",
                key,
                Duration::from_secs(1),
                NonZeroUsize::new(1024).unwrap()
            )
            .unwrap_err()
            .kind(),
            ModelErrorKind::InvalidConfig
        );
    }
    assert!(
        ClientConfig::new(
            "https://example.com",
            "key",
            Duration::ZERO,
            NonZeroUsize::new(1024).unwrap()
        )
        .is_err()
    );
    assert!(
        ClientConfig::new(
            "https://example.com",
            "key",
            Duration::from_secs(1),
            NonZeroUsize::new(usize::MAX).unwrap()
        )
        .is_err()
    );
    let config = ClientConfig::new(
        "https://URL_SENTINEL.example/v1",
        "API_KEY_SENTINEL",
        Duration::from_secs(1),
        NonZeroUsize::new(1024).unwrap(),
    )
    .unwrap();
    let printed = format!(
        "{config:?} {:?}",
        OpenAIClient::new(config.clone()).unwrap()
    );
    assert!(!printed.contains("SENTINEL"));
}

#[test]
fn http_statuses_are_safe_typed_and_never_automatically_retried() {
    for p in PROVIDERS {
        for (status, expected) in [
            (401, ModelErrorKind::Authentication),
            (403, ModelErrorKind::Authentication),
            (429, ModelErrorKind::RateLimited),
            (500, ModelErrorKind::ProviderFailure),
            (400, ModelErrorKind::InvalidSettings),
        ] {
            let server = Server::new(vec![Reply {
                status,
                body: "BODY_SENTINEL API_KEY_SENTINEL URL_SENTINEL".into(),
                delay: Duration::ZERO,
                body_delay: std::time::Duration::ZERO,
                headers: vec![],
            }]);
            let error = p
                .client(&server)
                .request(&context(&session()), &p.settings())
                .unwrap_err();
            assert_eq!(error.kind(), expected);
            assert_eq!(error.http_status(), Some(status));
            assert_safe(&error);
            assert_eq!(server.requests().len(), 1);
        }
    }
}

#[test]
fn redirects_are_not_followed_or_allowed_to_leak_credentials() {
    let target = Server::new(vec![]);
    for p in PROVIDERS {
        let server = Server::new(vec![Reply {
            status: 307,
            body: "BODY_SENTINEL".into(),
            delay: Duration::ZERO,
            body_delay: std::time::Duration::ZERO,
            headers: vec![("Location".into(), target.url.clone())],
        }]);
        let error = p
            .client(&server)
            .request(&context(&session()), &p.settings())
            .unwrap_err();
        assert_eq!(error.kind(), ModelErrorKind::Http);
        assert_safe(&error);
        assert!(target.requests().is_empty());
        assert_eq!(server.requests().len(), 1);
    }
}

#[test]
fn deadlines_and_bounded_response_reads_are_enforced() {
    for p in PROVIDERS {
        let mut reply: Reply = p.answer("late").into();
        reply.delay = Duration::from_millis(100);
        let server = Server::new(vec![reply]);
        let config = ClientConfig::new(
            &server.url,
            "key",
            Duration::from_millis(20),
            NonZeroUsize::new(1024).unwrap(),
        )
        .unwrap();
        let error = p
            .configured(config)
            .request(&context(&session()), &p.settings())
            .unwrap_err();
        assert_eq!(error.kind(), ModelErrorKind::Timeout);
        assert_safe(&error);
        let server = Server::new(vec![p.answer("BODY_SENTINEL").into()]);
        let config = ClientConfig::new(
            &server.url,
            "key",
            Duration::from_secs(1),
            NonZeroUsize::new(10).unwrap(),
        )
        .unwrap();
        let error = p
            .configured(config)
            .request(&context(&session()), &p.settings())
            .unwrap_err();
        assert_eq!(error.kind(), ModelErrorKind::ResponseTooLarge);
        assert_safe(&error);
    }
}

#[test]
fn malformed_http_json_is_redacted() {
    for p in PROVIDERS {
        let server = Server::new(vec![Reply {
            status: 200,
            body: "not json BODY_SENTINEL".into(),
            delay: Duration::ZERO,
            body_delay: std::time::Duration::ZERO,
            headers: vec![],
        }]);
        let error = p
            .client(&server)
            .request(&context(&session()), &p.settings())
            .unwrap_err();
        assert_eq!(error.kind(), ModelErrorKind::MalformedResponse);
        assert_safe(&error);
    }
}

#[test]
fn truncated_and_refused_responses_never_expose_partial_executable_calls() {
    let cases = [
        (
            Provider::OpenAI,
            json!({"status":"incomplete","incomplete_details":{"reason":"max_output_tokens"},"output":[{"type":"function_call","call_id":"dangerous","name":"lookup","arguments":"{}"}]}),
            "incomplete",
        ),
        (
            Provider::Anthropic,
            json!({"stop_reason":"max_tokens","content":[{"type":"tool_use","id":"dangerous","name":"lookup","input":{}}]}),
            "incomplete",
        ),
        (
            Provider::OpenAI,
            json!({"status":"completed","output":[{"type":"message","role":"assistant","content":[{"type":"refusal","refusal":"API_KEY_SENTINEL"}]}]}),
            "refused",
        ),
        (
            Provider::Anthropic,
            json!({"stop_reason":"refusal","content":[]}),
            "refused",
        ),
    ];
    for (p, value, kind) in cases {
        let server = Server::new(vec![value.into()]);
        let result = p
            .client(&server)
            .request(&context(&session()), &p.settings())
            .unwrap();
        match kind {
            "incomplete" => assert!(matches!(
                result,
                Generation::Incomplete {
                    reason: IncompleteReason::OutputLimit
                }
            )),
            "refused" => assert!(matches!(result, Generation::Refused { reason: None })),
            _ => unreachable!(),
        };
    }
}

#[test]
fn unknown_native_items_and_missing_completion_metadata_fail_closed() {
    let cases = [
        (
            Provider::OpenAI,
            json!({"status":"completed","output":[{"type":"web_search_call"}]}),
            ModelErrorKind::UnsupportedResponse,
        ),
        (
            Provider::OpenAI,
            json!({"output":[]}),
            ModelErrorKind::MalformedResponse,
        ),
        (
            Provider::OpenAI,
            json!({"status":"completed","output":[{"type":"message","role":"assistant","content":[{"type":"audio","data":"secret"}]}]}),
            ModelErrorKind::UnsupportedResponse,
        ),
        (
            Provider::Anthropic,
            json!({"type":"message","role":"assistant","stop_reason":"end_turn","content":[{"type":"server_tool_use","name":"unknown"}]}),
            ModelErrorKind::UnsupportedResponse,
        ),
        (
            Provider::Anthropic,
            json!({"type":"message","role":"assistant","content":[]}),
            ModelErrorKind::MalformedResponse,
        ),
    ];
    for (p, value, expected) in cases {
        let server = Server::new(vec![value.into()]);
        let error = p
            .client(&server)
            .request(&context(&session()), &p.settings())
            .unwrap_err();
        assert_eq!(error.kind(), expected);
        assert_safe(&error);
    }
}

fn mutate_call(p: Provider, value: &mut Value, field: &str, replacement: Value) {
    match p {
        Provider::OpenAI => {
            let key = match field {
                "input" => "arguments",
                other => other,
            };
            value["output"][2][key] = if field == "input" {
                json!(replacement.to_string())
            } else {
                replacement
            };
        }
        Provider::Anthropic => value["content"][3][field] = replacement,
    }
}

#[test]
fn invalid_arguments_tool_names_ambiguous_native_ids_and_empty_output_are_rejected() {
    for p in PROVIDERS {
        for (field, replacement) in [("name", json!("not_offered")), ("input", json!([]))] {
            let mut response = p.tool(1);
            mutate_call(p, &mut response, field, replacement);
            let server = Server::new(vec![response.into()]);
            assert_eq!(
                p.client(&server)
                    .request(&context(&session()), &p.settings())
                    .unwrap_err()
                    .kind(),
                ModelErrorKind::MalformedResponse
            );
        }
        let mut response = p.tool(1);
        match p {
            Provider::OpenAI => {
                let call = response["output"][2].clone();
                response["output"].as_array_mut().unwrap().push(call);
            }
            Provider::Anthropic => {
                let call = response["content"][3].clone();
                response["content"].as_array_mut().unwrap().push(call);
            }
        }
        let server = Server::new(vec![response.into()]);
        assert_eq!(
            p.client(&server)
                .request(&context(&session()), &p.settings())
                .unwrap_err()
                .kind(),
            ModelErrorKind::MalformedResponse
        );
        let server = Server::new(vec![p.answer("  ").into()]);
        assert_eq!(
            p.client(&server)
                .request(&context(&session()), &p.settings())
                .unwrap_err()
                .kind(),
            ModelErrorKind::MalformedResponse
        );
    }
}

#[test]
fn chunked_responses_cannot_bypass_the_body_limit() {
    for p in PROVIDERS {
        let body = "BODY_SENTINEL".repeat(20);
        let server = Server::new(vec![Reply {
            status: 200,
            body: format!("{:X}\r\n{}\r\n0\r\n\r\n", body.len(), body),
            delay: Duration::ZERO,
            body_delay: std::time::Duration::ZERO,
            headers: vec![("Transfer-Encoding".into(), "chunked".into())],
        }]);
        let config = ClientConfig::new(
            &server.url,
            "key",
            Duration::from_secs(1),
            NonZeroUsize::new(32).unwrap(),
        )
        .unwrap();
        let error = p
            .configured(config)
            .request(&context(&session()), &p.settings())
            .unwrap_err();
        assert_eq!(error.kind(), ModelErrorKind::ResponseTooLarge);
        assert_safe(&error);
    }
}

#[test]
fn refused_connection_is_a_safe_transport_error() {
    let url = "http://127.0.0.1:0/URL_SENTINEL";
    for p in PROVIDERS {
        let config = ClientConfig::new(
            url,
            "API_KEY_SENTINEL",
            Duration::from_secs(1),
            NonZeroUsize::new(1024).unwrap(),
        )
        .unwrap();
        let error = p
            .configured(config)
            .request(&context(&session()), &p.settings())
            .unwrap_err();
        assert_eq!(error.kind(), ModelErrorKind::Transport);
        assert_safe(&error);
    }
}

#[test]
fn response_body_reads_share_the_total_deadline() {
    for p in PROVIDERS {
        let mut reply: Reply = p.answer("late body").into();
        reply.body_delay = Duration::from_millis(100);
        let server = Server::new(vec![reply]);
        let config = ClientConfig::new(
            &server.url,
            "API_KEY_SENTINEL",
            Duration::from_millis(20),
            NonZeroUsize::new(4096).unwrap(),
        )
        .unwrap();
        let error = p
            .configured(config)
            .request(&context(&session()), &p.settings())
            .unwrap_err();
        assert_eq!(error.kind(), ModelErrorKind::Timeout);
        assert_eq!(error.http_status(), None);
        assert_safe(&error);
    }
}

#[test]
fn paths_headers_native_schemas_effort_and_defaults() {
    for p in PROVIDERS {
        for effort in [Effort::Default, Effort::High] {
            let server = Server::new(vec![p.answer("hello back").into()]);
            let client = p.client(&server);
            let mut settings = p.settings();
            settings.effort = effort;
            let answer = complete(client.request(&context(&session()), &settings).unwrap());
            assert_eq!(answer, AssistantMessage::answer("hello back").unwrap());
            let r = server.request(0);
            let headers = r.headers.to_ascii_lowercase();
            assert!(headers.contains("content-type: application/json"));
            assert!(!r.line.contains("secret"));
            match p {
                Provider::OpenAI => {
                    assert_eq!(r.line, "POST /custom/v1/responses HTTP/1.1");
                    assert!(headers.contains("authorization: bearer secret-key"));
                    assert_eq!(r.body["max_output_tokens"], 1024);
                    assert_eq!(
                        r.body["tools"][0]["parameters"],
                        json!({"type":"object","properties":{"n":{"type":"integer","minimum":0}},"required":["n"],"additionalProperties":false})
                    );
                    assert_eq!(
                        r.body.pointer("/reasoning/effort").cloned(),
                        if effort == Effort::Default {
                            None
                        } else {
                            Some(json!("high"))
                        }
                    );
                }
                Provider::Anthropic => {
                    assert_eq!(r.line, "POST /custom/v1/messages HTTP/1.1");
                    assert!(headers.contains("x-api-key: secret-key"));
                    assert!(headers.contains("anthropic-version: 2026-10-21"));
                    assert_eq!(r.body["max_tokens"], 1024);
                    assert!(r.body.get("thinking").is_none());
                    assert_eq!(
                        r.body["tools"][0]["input_schema"],
                        json!({"type":"object","properties":{"n":{"type":"integer","minimum":0}},"required":["n"],"additionalProperties":false})
                    );
                    assert_eq!(
                        r.body.pointer("/output_config/effort").cloned(),
                        if effort == Effort::Default {
                            None
                        } else {
                            Some(json!("high"))
                        }
                    );
                }
            }
            assert!(r.body.to_string().contains("Be useful"));
            assert!(r.body.to_string().contains("hello"));
        }
    }
}

#[test]
fn decision_uses_fixed_strict_native_schema_and_no_callable_tools() {
    for p in PROVIDERS {
        let server = Server::new(vec![
            p.answer(r#"{"should_allow":true,"reason":null}"#).into(),
        ]);
        let client = p.reviewer(&server);
        let d = complete(
            client
                .request(&context::<Decision>(&session()), &p.settings())
                .unwrap(),
        );
        assert!(d.should_allow());
        assert!(d.reason().is_none());
        let r = server.request(0);
        assert!(r.body.get("tools").is_none());
        assert!(r.body.to_string().contains("Review tool catalog"));
        let expected = json!({"type":"object","properties":{"should_allow":{"type":"boolean"},"reason":{"type":["string","null"]}},"required":["should_allow","reason"],"additionalProperties":false});
        match p {
            Provider::OpenAI => assert_eq!(
                r.body["text"]["format"],
                json!({"type":"json_schema","name":"decision","strict":true,"schema":expected})
            ),
            Provider::Anthropic => assert_eq!(
                r.body["output_config"]["format"],
                json!({"type":"json_schema","schema":expected})
            ),
        }
    }
}

#[test]
fn decision_rejects_extra_keys_string_boolean_missing_reason_and_tools() {
    for p in PROVIDERS {
        for text in [
            r#"{"should_allow":"false","reason":null}"#,
            r#"{"should_allow":false,"reason":null,"extra":1}"#,
            r#"{"should_allow":true}"#,
        ] {
            let server = Server::new(vec![p.answer(text).into()]);
            assert_eq!(
                p.reviewer(&server)
                    .request(&context::<Decision>(&session()), &p.settings())
                    .unwrap_err()
                    .kind(),
                ModelErrorKind::MalformedResponse
            );
        }
        let server = Server::new(vec![p.tool(1).into()]);
        assert_eq!(
            p.reviewer(&server)
                .request(&context::<Decision>(&session()), &p.settings())
                .unwrap_err()
                .kind(),
            ModelErrorKind::MalformedResponse
        );
    }
}

#[test]
fn arbitrary_schemas_preserve_constraints_without_normalization() {
    for p in PROVIDERS {
        for schema in [
            json!({"type":"object","$defs":{"path":{"type":"string"}},"properties":{"path":{"$ref":"#/$defs/path"},"nullable":{"type":["string","null"]}},"anyOf":[{"required":["path"]},{"required":["nullable"]}]}),
            json!({"type":"object","properties":{"entries":{"type":"array","items":{"type":"string"},"minItems":1}},"required":["entries"]}),
        ] {
            let server = Server::new(vec![p.answer("ok").into()]);
            let s = session();
            let ctx = ContextPlan::assistant(
                &s,
                text("instructions"),
                vec![ToolDefinition::new(
                    ToolName::new("lookup").unwrap(),
                    text("tool"),
                    InputSchema::new(schema.clone()).unwrap(),
                )],
                limits(),
                &[],
            )
            .unwrap();
            p.client(&server).request(&ctx, &p.settings()).unwrap();
            let r = server.request(0);
            let actual = match p {
                Provider::OpenAI => &r.body["tools"][0]["parameters"],
                Provider::Anthropic => &r.body["tools"][0]["input_schema"],
            };
            assert_eq!(actual, &schema);
        }
    }
}

#[test]
fn reviewer_refusals_remain_stopped_outcomes() {
    for (p, response) in [
        (
            Provider::OpenAI,
            json!({"status":"completed","output":[{"type":"message","role":"assistant","content":[{"type":"refusal","refusal":"no"}]}]}),
        ),
        (Provider::Anthropic, json!({"stop_reason":"refusal"})),
    ] {
        let server = Server::new(vec![response.into()]);
        let result = p
            .reviewer(&server)
            .request(&context::<Decision>(&session()), &p.settings())
            .unwrap();
        assert!(matches!(result, Generation::Refused { .. }));
    }
}

#[test]
fn openai_and_anthropic_parallel_returns_distinguish_empty_success_and_failure() {
    for p in [Provider::OpenAI, Provider::Anthropic] {
        let mut response = p.tool(1);
        match p {
            Provider::OpenAI => {
                let mut second = response["output"][2].clone();
                second["id"] = json!("item-second");
                second["call_id"] = json!("native-second");
                second["arguments"] = json!(r#"{"n":2}"#);
                response["output"].as_array_mut().unwrap().push(second);
            }
            Provider::Anthropic => {
                let mut second = response["content"][3].clone();
                second["id"] = json!("native-second");
                second["input"] = json!({"n":2});
                response["content"].as_array_mut().unwrap().push(second);
            }
        }
        let server = Server::new(vec![response.clone().into(), p.answer("done").into()]);
        let client = p.client(&server);
        let mut s = session();
        let output = invoke_assistant(p, client.as_ref(), &mut s);
        assert_eq!(output.calls().unwrap().as_slice().len(), 2);
        accept_tools(&mut s);
        client.request(&context(&s), &p.settings()).unwrap();
        let request = server.request(1).body;
        assert_eq!(
            p.request_native(&request),
            p.native(&response).as_array().unwrap().clone()
        );
        match p {
            Provider::OpenAI => {
                let returns: Vec<_> = request["input"]
                    .as_array()
                    .unwrap()
                    .iter()
                    .filter(|v| v["type"] == "function_call_output")
                    .collect();
                assert_eq!(returns.len(), 2);
                assert_eq!(returns[0]["call_id"], "native-1");
                assert_eq!(
                    serde_json::from_str::<serde_json::Value>(
                        returns[0]["output"].as_str().unwrap()
                    )
                    .unwrap(),
                    json!({"success":true,"output":""})
                );
                assert_eq!(returns[1]["call_id"], "native-second");
                assert_eq!(
                    serde_json::from_str::<serde_json::Value>(
                        returns[1]["output"].as_str().unwrap()
                    )
                    .unwrap(),
                    json!({"success":false,"error":"tool failed"})
                );
            }
            Provider::Anthropic => assert_eq!(
                request["messages"][2]["content"],
                json!([{"type":"tool_result","tool_use_id":"native-1","is_error":false,"content":""},{"type":"tool_result","tool_use_id":"native-second","is_error":true,"content":"tool failed"}])
            ),
        }
    }
}

#[test]
fn arbitrary_models_and_every_effort_reach_provider_without_fallback() {
    for p in PROVIDERS {
        for (effort, label) in [
            (Effort::Default, None),
            (Effort::Off, Some("none")),
            (Effort::Low, Some("low")),
            (Effort::Medium, Some("medium")),
            (Effort::High, Some("high")),
            (Effort::Xhigh, Some("xhigh")),
            (Effort::Max, Some("max")),
        ] {
            let server = Server::new(vec![
                p.answer("ok").into(),
                Reply {
                    status: 400,
                    body: "unsupported effort API_KEY_SENTINEL".into(),
                    delay: std::time::Duration::ZERO,
                    body_delay: std::time::Duration::ZERO,
                    headers: vec![],
                },
            ]);
            let client = p.client(&server);
            let mut settings = p.settings();
            settings.model_id = ModelId::new("arbitrary/vendor:model-2040").unwrap();
            settings.effort = effort;
            client.request(&context(&session()), &settings).unwrap();
            let error = client.request(&context(&session()), &settings).unwrap_err();
            assert_eq!(error.kind(), ModelErrorKind::InvalidSettings);
            assert_eq!(error.http_status(), Some(400));
            assert!(!format!("{error:?} {error}").contains("SENTINEL"));
            let request = server.request(0).body;
            assert_eq!(request["model"], "arbitrary/vendor:model-2040");
            match p {
                Provider::OpenAI => assert_eq!(
                    request
                        .pointer("/reasoning/effort")
                        .and_then(|v| v.as_str()),
                    label
                ),
                Provider::Anthropic => {
                    if effort == Effort::Off {
                        assert_eq!(request["thinking"], json!({"type":"disabled"}));
                        assert!(request.pointer("/output_config/effort").is_none());
                    } else {
                        assert!(request.get("thinking").is_none());
                        assert_eq!(
                            request
                                .pointer("/output_config/effort")
                                .and_then(|v| v.as_str()),
                            label
                        );
                    }
                }
            }
            assert_eq!(server.requests().len(), 2);
            assert_eq!(request, server.request(1).body);
        }
    }
    assert!(ModelId::new(" \n\t").is_err());
}

#[test]
fn assistant_plans_require_a_model_wait_before_any_http() {
    let archive = SessionArchive::new(SessionId::new("empty").unwrap());
    assert!(ContextPlan::assistant(&archive, text("instructions"), vec![], limits(), &[]).is_err());
    let mut archive = session();
    record_output(&mut archive, AssistantMessage::answer("finished").unwrap());
    assert!(ContextPlan::assistant(&archive, text("instructions"), vec![], limits(), &[]).is_err());
}

#[test]
fn active_native_bundles_survive_archive_roundtrip_then_closed_turns_become_portable() {
    for p in PROVIDERS {
        let mut first = p.tool(1);
        let mut second = p.tool(2);
        let terminal = p.answer("first turn done");
        for response in [&mut first, &mut second] {
            let blocks = match p {
                Provider::OpenAI => &mut response["output"],
                Provider::Anthropic => &mut response["content"],
            };
            for block in blocks.as_array_mut().unwrap() {
                block["future_native_field"] = json!({"opaque":["preserve exactly",null,3]});
            }
            response["model"] = json!("SERVED_MODEL_AUDIT_ONLY");
            response["id"] = json!("RESPONSE_ID_AUDIT_ONLY");
            response["usage"] = json!({"input_tokens":31,"output_tokens":7});
        }
        let server = Server::new(vec![
            first.clone().into(),
            second.clone().into(),
            terminal.into(),
            p.answer("second turn done").into(),
        ]);
        let mut archive = session();
        invoke_assistant(p, p.client(&server).as_ref(), &mut archive);
        accept_tools(&mut archive);
        let change = ArchiveChange::initial(archive).unwrap();
        let directory = tempfile::tempdir().unwrap();
        let store = JsonlSessionStore::new(directory.path());
        store.create(&change).unwrap();
        let id = change.candidate().session().id().clone();
        let mut archive = store.inspect(&id).unwrap();
        assert_eq!(&SessionArchive::decode(change.encoded()).unwrap(), &archive);
        let before = archive.clone();
        invoke_assistant(p, p.client(&server).as_ref(), &mut archive);
        assert_eq!(
            p.request_native(&server.request(1).body),
            p.native(&first).as_array().unwrap().clone()
        );
        accept_tools(&mut archive);
        let change = ArchiveChange::next(&before, archive).unwrap();
        let encoded = change.candidate().encode().unwrap();
        assert_eq!(
            SessionArchive::decode(&encoded).unwrap(),
            *change.candidate()
        );
        let mut transaction = store.acquire(&id).unwrap();
        transaction.commit(&change).unwrap();
        drop(transaction);
        let mut archive = store.inspect(&id).unwrap();
        let before = archive.clone();
        invoke_assistant(p, p.client(&server).as_ref(), &mut archive);
        let expected: Vec<_> = [&first, &second]
            .iter()
            .flat_map(|r| p.native(r).as_array().unwrap().clone())
            .collect();
        assert_eq!(p.request_native(&server.request(2).body), expected);
        let captures: Vec<_> = archive
            .generations()
            .iter()
            .filter(|record| matches!(record.intent().role(), InvocationRole::Assistant { .. }))
            .filter_map(|record| match record.observation() {
                Observation::Observed { receipt, .. } => match &receipt.capture {
                    ResponseCapture::Complete(bytes) => Some(bytes),
                    _ => None,
                },
                _ => None,
            })
            .collect();
        assert_eq!(captures.len(), 3);
        assert_eq!(serde_json::from_slice::<Value>(captures[0]).unwrap(), first);
        assert_eq!(
            serde_json::from_slice::<Value>(captures[1]).unwrap(),
            second
        );
        archive
            .session_mut()
            .start_turn(user("new user turn"))
            .unwrap();
        let change = ArchiveChange::next(&before, archive).unwrap();
        let archive = SessionArchive::decode(change.encoded()).unwrap();
        p.client(&server)
            .request(&context(&archive), &p.settings())
            .unwrap();
        let body = server.request(3).body;
        match p {
            Provider::OpenAI => assert!(p.request_native(&body).is_empty()),
            Provider::Anthropic => assert_eq!(
                p.request_native(&body),
                vec![
                    json!({"type":"text","text":context::<AssistantMessage>(&archive).history()[0].record()})
                ]
            ),
        }
        let prose = body.to_string();
        for fact in [
            "hello",
            "working",
            "lookup",
            "tool activity",
            "first turn done",
            "new user turn",
        ] {
            assert!(prose.contains(fact), "missing {fact}");
        }
        for audit in [
            "signature-1",
            "private",
            "opaque-redacted",
            "future_native_field",
            "SERVED_MODEL_AUDIT_ONLY",
            "RESPONSE_ID_AUDIT_ONLY",
            "input_tokens",
            "native-1",
        ] {
            assert!(!prose.contains(audit), "leaked historical metadata {audit}");
        }
    }
}

#[test]
fn active_target_model_and_thinking_changes_fail_before_http_but_closed_history_can_change() {
    for p in PROVIDERS {
        let server = Server::new(vec![p.tool(1).into(), p.answer("completed").into()]);
        let mut archive = session();
        let client = p.client(&server);
        invoke_assistant(p, client.as_ref(), &mut archive);
        accept_tools(&mut archive);
        let plan = context(&archive);
        for change_model in [false, true] {
            let mut settings = p.settings();
            if change_model {
                settings.model_id = ModelId::new("another-model").unwrap();
            } else {
                settings.effort = Effort::Low;
            }
            assert_eq!(
                client.prepare(&plan, &settings).unwrap_err().kind(),
                ModelErrorKind::UnsupportedInput
            );
        }
        let other_endpoint = Server::new(vec![]);
        assert_eq!(
            p.client(&other_endpoint)
                .prepare(&plan, &p.settings())
                .unwrap_err()
                .kind(),
            ModelErrorKind::UnsupportedInput
        );
        let other = match p {
            Provider::OpenAI => Provider::Anthropic,
            Provider::Anthropic => Provider::OpenAI,
        };
        assert_eq!(
            other
                .client(&server)
                .prepare(&plan, &other.settings())
                .unwrap_err()
                .kind(),
            ModelErrorKind::UnsupportedInput
        );
        assert_eq!(server.requests().len(), 1);
        invoke_assistant(p, client.as_ref(), &mut archive);
        archive.session_mut().start_turn(user("new turn")).unwrap();
        let mut settings = other.settings();
        settings.effort = Effort::Off;
        settings.model_id = ModelId::new("new-provider-model").unwrap();
        let prepared = other
            .client(&other_endpoint)
            .prepare(&context(&archive), &settings)
            .unwrap();
        assert_eq!(prepared.body()["model"], "new-provider-model");
        assert!(other_endpoint.requests().is_empty());
    }
}

#[test]
fn original_opaque_ids_are_scoped_to_turns_and_denials_correlate_without_dispatch() {
    for p in PROVIDERS {
        let mut response = p.tool(1);
        match p {
            Provider::OpenAI => {
                response["output"][2]["call_id"] = json!(" arbitrary / 工具 λ \u{0} ")
            }
            Provider::Anthropic => {
                response["content"][3]["id"] = json!(" arbitrary / 工具 λ \u{0} ")
            }
        };
        let server = Server::new(vec![
            response.clone().into(),
            p.answer("done").into(),
            response.into(),
        ]);
        let client = p.client(&server);
        let mut archive = session();
        let output = invoke_assistant(p, client.as_ref(), &mut archive);
        assert_eq!(
            output.calls().unwrap().as_slice()[0].id().as_str(),
            " arbitrary / 工具 λ \u{0} "
        );
        let NextAction::Review(call) = archive.session().next_action() else {
            panic!()
        };
        review(&mut archive, &call, false);
        invoke_assistant(p, client.as_ref(), &mut archive);
        let body = server.request(1).body;
        let result = match p {
            Provider::OpenAI => body["input"]
                .as_array()
                .unwrap()
                .iter()
                .find(|v| v["type"] == "function_call_output")
                .unwrap(),
            Provider::Anthropic => &body["messages"][2]["content"][0],
        };
        assert_eq!(
            result[match p {
                Provider::OpenAI => "call_id",
                Provider::Anthropic => "tool_use_id",
            }],
            call.call_id().as_str()
        );
        assert!(result.to_string().contains("Not executed"));
        archive.session_mut().start_turn(user("again")).unwrap();
        let next = invoke_assistant(p, client.as_ref(), &mut archive);
        assert_eq!(next.calls().unwrap().as_slice()[0].id(), call.call_id());
        let NextAction::Review(next_call) = archive.session().next_action() else {
            panic!()
        };
        assert_ne!(next_call.generation(), call.generation());
    }
}

fn interrupted_history(disposition: EffectDisposition, source: ResolutionSource) -> SessionArchive {
    let mut archive = session();
    let calls = ToolCalls::new(
        (0..3)
            .map(|n| {
                ToolCall::new(
                    ToolCallId::new(format!("external-{n}")).unwrap(),
                    ToolName::new("lookup").unwrap(),
                    json!({"n":n}),
                )
                .unwrap()
            })
            .collect(),
    )
    .unwrap();
    let generation = record_output(
        &mut archive,
        AssistantMessage::tool_requests(None, calls.clone()),
    );
    let turn = archive.session().active_turn().unwrap().id();
    let first = CallRef::new(generation, calls.as_slice()[0].id().clone());
    review(&mut archive, &first, true);
    archive
        .session_mut()
        .start_tool(turn, &first, Timestamp::now())
        .unwrap();
    archive
        .session_mut()
        .record_tool_result(
            turn,
            &first,
            ToolOutcome::Success(String::new()),
            Timestamp::now(),
        )
        .unwrap();
    let second = CallRef::new(generation, calls.as_slice()[1].id().clone());
    review(&mut archive, &second, true);
    archive
        .session_mut()
        .start_tool(turn, &second, Timestamp::now())
        .unwrap();
    archive
        .session_mut()
        .interrupt(turn, StopReason::ProcessLost)
        .unwrap();
    archive
        .session_mut()
        .resolve_effect(
            turn,
            &second,
            EffectResolution::new(disposition, source, text("resolution evidence")),
        )
        .unwrap();
    archive
        .session_mut()
        .start_turn(user("continue from facts"))
        .unwrap();
    archive
}

#[test]
fn closed_recovery_facts_and_every_resolution_source_precede_the_current_request() {
    for p in PROVIDERS {
        for (disposition, label) in [
            (EffectDisposition::Completed, "completed"),
            (EffectDisposition::NotPerformed, "not_performed"),
        ] {
            for (source, provenance) in [
                (ResolutionSource::Observation, "observation"),
                (ResolutionSource::UserAttestation, "user_attestation"),
                (
                    ResolutionSource::ApplicationAttestation,
                    "application_attestation",
                ),
            ] {
                for decision in [false, true] {
                    let archive = interrupted_history(disposition, source);
                    let server = Server::new(vec![
                        p.answer(if decision {
                            r#"{"should_allow":false,"reason":"reviewed"}"#
                        } else {
                            "acknowledged"
                        })
                        .into(),
                    ]);
                    if decision {
                        p.reviewer(&server)
                            .request(&context::<Decision>(&archive), &p.settings())
                            .unwrap();
                    } else {
                        p.client(&server)
                            .request(&context(&archive), &p.settings())
                            .unwrap();
                    }
                    let body = server.request(0).body.to_string();
                    for fact in [
                        "unknown_effect",
                        "not_executed",
                        "returned",
                        label,
                        provenance,
                        "resolution evidence",
                        "process_lost",
                    ] {
                        assert!(body.contains(fact), "missing {fact} in {body}");
                    }
                    assert!(!body.contains("function_call_output"));
                    assert!(!body.contains("tool_result"));
                    assert!(!body.contains("external-"));
                    assert!(
                        body.find("unknown_effect").unwrap()
                            < body.find("continue from facts").unwrap()
                    );
                    if decision {
                        assert!(body.contains("candidate"));
                        assert!(body.contains("active_turn"));
                        assert!(body.contains("executions"));
                    }
                }
            }
        }
    }
}

#[test]
fn fitting_selects_newest_whole_turns_and_preserves_required_groups() {
    for p in PROVIDERS {
        let mut archive = session();
        for n in 0..6 {
            record_output(
                &mut archive,
                AssistantMessage::answer(format!("answer-{n} {}", "f".repeat(400))).unwrap(),
            );
            archive
                .session_mut()
                .start_turn(user(&format!("request-{}", n + 1)))
                .unwrap();
        }
        let server = Server::new(vec![]);
        let client = p.client(&server);
        let turns: Vec<_> = archive.session().turns().iter().map(Turn::id).collect();
        let full = client.prepare(&context(&archive), &p.settings()).unwrap();
        let cap = full.budget().request_bytes() - 3 * 400;
        let constrained = ContextLimits {
            request_bytes: NonZeroUsize::new(cap).unwrap(),
            ..limits()
        };
        let plan = ContextPlan::assistant(
            &archive,
            text("Be useful"),
            vec![tool()],
            constrained,
            &[turns[0]],
        )
        .unwrap();
        let prepared = client.prepare(&plan, &p.settings()).unwrap();
        assert_eq!(prepared.selection().required(), &[turns[0]]);
        assert_eq!(prepared.selection().included().first(), Some(&turns[0]));
        assert_eq!(prepared.selection().included().last(), Some(&turns[5]));
        assert!(prepared.selection().included().len() < 6);
        let optional: Vec<_> = prepared
            .selection()
            .included()
            .iter()
            .copied()
            .filter(|turn| *turn != turns[0])
            .collect();
        assert_eq!(optional, turns[6 - optional.len()..]);
        assert_eq!(prepared.selection().revision(), archive.revision());
        assert_eq!(prepared.selection().policy(), "newest_whole_turns");
        let body = prepared.body().to_string();
        assert_eq!(body.matches("Context note:").count(), 1);
        for (index, turn) in turns.iter().enumerate() {
            assert_eq!(
                body.contains(&format!("answer-{index}")),
                prepared.selection().included().contains(turn)
            );
        }
        assert_eq!(
            prepared.budget().request_bytes(),
            serde_json::to_vec(prepared.body()).unwrap().len()
        );
        assert!(prepared.budget().request_bytes() <= cap);
        assert_eq!(
            prepared.budget().estimated_input_tokens(),
            (prepared.budget().request_bytes() as u64).div_ceil(3)
        );
        assert_eq!(prepared.budget().method(), "utf8_bytes_div_3");
        assert!(server.requests().is_empty());
        let impossible = ContextLimits {
            request_bytes: NonZeroUsize::new(1).unwrap(),
            ..limits()
        };
        let plan = ContextPlan::assistant(
            &archive,
            text("Be useful"),
            vec![tool()],
            impossible,
            &[turns[0]],
        )
        .unwrap();
        assert_eq!(
            client.prepare(&plan, &p.settings()).unwrap_err().kind(),
            ModelErrorKind::ContextCapacity
        );
    }
}

#[test]
fn full_protocol_framing_tools_native_and_reviewer_candidate_must_fit() {
    for p in PROVIDERS {
        let archive = session();
        let server = Server::new(vec![p.tool(1).into()]);
        let client = p.client(&server);
        let plan = context(&archive);
        let baseline = client.prepare(&plan, &p.settings()).unwrap();
        let exact = baseline.budget().request_bytes();
        for (bytes, ok) in [(exact, true), (exact - 1, false)] {
            let plan = ContextPlan::assistant(
                &archive,
                text("Be useful"),
                vec![tool()],
                ContextLimits {
                    request_bytes: NonZeroUsize::new(bytes).unwrap(),
                    ..limits()
                },
                &[],
            )
            .unwrap();
            assert_eq!(client.prepare(&plan, &p.settings()).is_ok(), ok);
        }
        let huge_tool = ToolDefinition::new(
            ToolName::new("lookup").unwrap(),
            text(&"x".repeat(exact)),
            tool().input_schema().clone(),
        );
        let plan = ContextPlan::assistant(
            &archive,
            text("Be useful"),
            vec![huge_tool],
            ContextLimits {
                request_bytes: NonZeroUsize::new(exact).unwrap(),
                ..limits()
            },
            &[],
        )
        .unwrap();
        assert_eq!(
            client.prepare(&plan, &p.settings()).unwrap_err().kind(),
            ModelErrorKind::ContextCapacity
        );
        assert!(server.requests().is_empty());
        let mut archive = archive;
        invoke_assistant(p, client.as_ref(), &mut archive);
        let reviewer = p.reviewer(&server);
        let decision_plan = context::<Decision>(&archive);
        let prepared = reviewer.prepare(&decision_plan, &p.settings()).unwrap();
        let body = prepared.body().to_string();
        assert!(body.contains("candidate"));
        assert!(body.contains("hello"));
        assert!(body.contains("native-1"));
        assert!(prepared.body().get("tools").is_none());
        let constrained = ContextLimits {
            input_tokens: std::num::NonZeroU64::new(1).unwrap(),
            ..limits()
        };
        let NextAction::Review(call) = archive.session().next_action() else {
            panic!()
        };
        let plan = ContextPlan::reviewer(
            &archive,
            &call,
            text("review"),
            vec![tool()],
            constrained,
            &[],
        )
        .unwrap();
        assert_eq!(
            reviewer.prepare(&plan, &p.settings()).unwrap_err().kind(),
            ModelErrorKind::ContextCapacity
        );
        accept_tools(&mut archive);
        let plan = ContextPlan::assistant(
            &archive,
            text("Be useful"),
            vec![tool()],
            ContextLimits {
                request_bytes: NonZeroUsize::new(exact).unwrap(),
                ..limits()
            },
            &[],
        )
        .unwrap();
        assert_eq!(
            client.prepare(&plan, &p.settings()).unwrap_err().kind(),
            ModelErrorKind::ContextCapacity
        );
        assert_eq!(server.requests().len(), 1);
    }
}

#[test]
fn response_receipts_capture_exact_success_failure_bytes_and_only_supplied_metadata() {
    for p in PROVIDERS {
        let mut response = p.answer("ok");
        response["model"] = json!("served-model");
        response["id"] = json!("response-7");
        response["usage"] = json!({"input_tokens":17,"output_tokens":5,"other":99});
        let mut reply: Reply = response.clone().into();
        reply
            .headers
            .push(("X-Request-ID".into(), "request-9".into()));
        let failed = "malformed original response BODY_SENTINEL";
        let server = Server::new(vec![
            reply,
            Reply {
                status: 429,
                body: failed.into(),
                delay: Duration::ZERO,
                body_delay: Duration::ZERO,
                headers: vec![],
            },
            p.answer("bare").into(),
        ]);
        let client = p.client(&server);
        let prepared = client.prepare(&context(&session()), &p.settings()).unwrap();
        assert!(server.requests().is_empty());
        let observed = client.invoke(&prepared);
        assert!(matches!(observed.result, Ok(Generation::Complete(_))));
        assert_eq!(observed.receipt.http_status, Some(200));
        assert_eq!(
            observed.receipt.served_model.as_deref(),
            Some("served-model")
        );
        assert_eq!(observed.receipt.response_id.as_deref(), Some("response-7"));
        assert_eq!(observed.receipt.request_id.as_deref(), Some("request-9"));
        assert_eq!(
            observed.receipt.usage,
            Some(Usage {
                input_tokens: Some(17),
                output_tokens: Some(5)
            })
        );
        assert_eq!(
            observed.receipt.finish.as_deref(),
            Some(match p {
                Provider::OpenAI => "completed",
                Provider::Anthropic => "end_turn",
            })
        );
        assert_eq!(
            observed.receipt.capture,
            ResponseCapture::Complete(response.to_string().into_bytes())
        );
        let observed = client.invoke(&prepared);
        assert_eq!(
            observed.result.unwrap_err().kind(),
            ModelErrorKind::RateLimited
        );
        assert_eq!(observed.receipt.http_status, Some(429));
        assert_eq!(
            observed.receipt.capture,
            ResponseCapture::Complete(failed.as_bytes().to_vec())
        );
        let observed = client.invoke(&prepared);
        assert_eq!(observed.receipt.served_model, None);
        assert_eq!(observed.receipt.response_id, None);
        assert_eq!(observed.receipt.request_id, None);
        assert_eq!(observed.receipt.usage, None);
        assert_eq!(server.requests().len(), 3);
    }
}

#[test]
fn malformed_unsupported_refused_and_incomplete_observations_retain_original_evidence() {
    for p in PROVIDERS {
        let cases = match p {
            Provider::OpenAI => vec![
                json!({"status":"completed","output":[{"type":"future_executable"}]}),
                json!({"status":"completed","output":[{"type":"message","role":"assistant","content":[{"type":"refusal","refusal":"no"}]}]}),
                json!({"status":"incomplete","incomplete_details":{"reason":"max_output_tokens"},"output":[{"type":"function_call","call_id":"partial","name":"lookup","arguments":"{}"}]}),
            ],
            Provider::Anthropic => vec![
                json!({"stop_reason":"tool_use","type":"message","role":"assistant","content":[{"type":"server_tool_use","name":"future"}]}),
                json!({"stop_reason":"refusal"}),
                json!({"stop_reason":"max_tokens","content":[{"type":"tool_use","id":"partial","name":"lookup","input":{}}]}),
            ],
        };
        let mut replies: Vec<_> = cases.iter().cloned().map(Reply::from).collect();
        replies.push(Reply {
            status: 200,
            body: "not json".into(),
            delay: Duration::ZERO,
            body_delay: Duration::ZERO,
            headers: vec![],
        });
        let server = Server::new(replies);
        let client = p.client(&server);
        let prepared = client.prepare(&context(&session()), &p.settings()).unwrap();
        for (index, original) in cases.iter().enumerate() {
            let observed = client.invoke(&prepared);
            assert_eq!(
                observed.receipt.capture,
                ResponseCapture::Complete(original.to_string().into_bytes())
            );
            match index {
                0 => assert_eq!(
                    observed.result.unwrap_err().kind(),
                    ModelErrorKind::UnsupportedResponse
                ),
                1 => assert!(matches!(observed.result, Ok(Generation::Refused { .. }))),
                2 => assert!(matches!(
                    observed.result,
                    Ok(Generation::Incomplete {
                        reason: IncompleteReason::OutputLimit
                    })
                )),
                _ => unreachable!(),
            }
        }
        let observed = client.invoke(&prepared);
        assert_eq!(
            observed.result.unwrap_err().kind(),
            ModelErrorKind::MalformedResponse
        );
        assert_eq!(
            observed.receipt.capture,
            ResponseCapture::Complete(b"not json".to_vec())
        );
    }
}

#[test]
fn receipt_bounds_apply_to_chunked_http_errors_and_echoed_credentials_are_scrubbed() {
    for p in PROVIDERS {
        let body = "prefix secret-key suffix ".repeat(20);
        let server = Server::new(vec![Reply {
            status: 500,
            body: format!("{:X}\r\n{}\r\n0\r\n\r\n", body.len(), body),
            delay: Duration::ZERO,
            body_delay: Duration::ZERO,
            headers: vec![
                ("Transfer-Encoding".into(), "chunked".into()),
                ("X-Request-ID".into(), "echo-secret-key".into()),
            ],
        }]);
        let config = ClientConfig::new(
            &server.url,
            "secret-key",
            Duration::from_secs(1),
            NonZeroUsize::new(32).unwrap(),
        )
        .unwrap();
        let client = p.configured(config);
        let prepared = client.prepare(&context(&session()), &p.settings()).unwrap();
        let observed = client.invoke(&prepared);
        assert_eq!(
            observed.result.unwrap_err().kind(),
            ModelErrorKind::ResponseTooLarge
        );
        assert_eq!(observed.receipt.http_status, Some(500));
        let ResponseCapture::Truncated { prefix, limit } = &observed.receipt.capture else {
            panic!()
        };
        assert_eq!(*limit, 32);
        assert!(prefix.len() <= 32);
        assert!(!format!("{:?}", observed.receipt).contains("secret-key"));
        assert_eq!(server.requests().len(), 1);
    }
}

#[test]
fn rejected_native_output_stays_audit_only_and_never_changes_active_preparation() {
    for p in PROVIDERS {
        let server = Server::new(vec![p.tool(1).into(), p.tool(1).into()]);
        let client = p.client(&server);
        let mut archive = session();
        invoke_assistant(p, client.as_ref(), &mut archive);
        accept_tools(&mut archive);
        let before = client.prepare(&context(&archive), &p.settings()).unwrap();
        let turn = archive.session().active_turn().unwrap().id();
        let generation = archive
            .begin_generation(InvocationRole::Assistant { turn }, &before)
            .unwrap();
        let observed = client.invoke(&before);
        let output = complete(observed.result.unwrap());
        let domain_before = archive.session().clone();
        assert!(
            archive
                .session_mut()
                .record_assistant(turn, generation, output)
                .is_err()
        );
        assert_eq!(archive.session(), &domain_before);
        archive
            .record_observation(
                generation,
                observed.at,
                observed.receipt,
                Disposition::Rejected(text("duplicate call in active native segment")),
            )
            .unwrap();
        let after = client.prepare(&context(&archive), &p.settings()).unwrap();
        assert_eq!(after.body(), before.body());
        assert_eq!(
            p.request_native(after.body()),
            p.native(&p.tool(1)).as_array().unwrap().clone()
        );
        assert_eq!(archive.generations().len(), 3);
        let Observation::Observed {
            disposition,
            receipt,
            ..
        } = archive.generations().last().unwrap().observation()
        else {
            panic!()
        };
        assert!(matches!(disposition, Disposition::Rejected(_)));
        assert_eq!(
            receipt.capture,
            ResponseCapture::Complete(p.tool(1).to_string().into_bytes())
        );
    }
}

#[test]
fn body_timeout_preserves_only_received_prefix_and_never_executes_partial_json() {
    use std::{
        io::{Read, Write},
        net::TcpListener,
        thread,
    };
    for p in PROVIDERS {
        for prefix in [
            b"{\"status\":\"completed\",\"output\":[".as_slice(),
            br#"{"error":"\u0073ecret-key\"#.as_slice(),
        ] {
            let listener = TcpListener::bind("127.0.0.1:0").unwrap();
            let url = format!("http://{}", listener.local_addr().unwrap());
            let worker = thread::spawn(move || {
                let (mut stream, _) = listener.accept().unwrap();
                let mut request = [0u8; 8192];
                assert!(stream.read(&mut request).unwrap() > 0);
                stream.write_all(b"HTTP/1.1 200 OK\r\nContent-Type: application/json\r\nContent-Length: 999\r\nConnection: close\r\n\r\n").unwrap();
                stream.write_all(prefix).unwrap();
                stream.flush().unwrap();
                thread::sleep(Duration::from_millis(100));
            });
            let client = p.configured(
                ClientConfig::new(
                    &url,
                    "secret-key",
                    Duration::from_millis(40),
                    NonZeroUsize::new(4096).unwrap(),
                )
                .unwrap(),
            );
            let prepared = client.prepare(&context(&session()), &p.settings()).unwrap();
            let observed = client.invoke(&prepared);
            assert_eq!(observed.result.unwrap_err().kind(), ModelErrorKind::Timeout);
            assert_eq!(observed.receipt.http_status, Some(200));
            let ResponseCapture::Interrupted { prefix: captured } = observed.receipt.capture else {
                panic!()
            };
            assert_eq!(captured.len(), prefix.len());
            if prefix.windows(6).any(|part| part == br"\u0073") {
                assert!(!String::from_utf8_lossy(&captured).contains(r"\u0073ecret-key"));
                assert!(captured.contains(&b'*'));
            } else {
                assert_eq!(captured, prefix);
            }
            worker.join().unwrap();
        }
    }
}

#[test]
fn credential_echo_cannot_enter_accepted_text_tool_arguments_or_native_evidence() {
    for p in PROVIDERS {
        let mut tool_response = p.tool(1);
        match p {
            Provider::OpenAI => {
                tool_response["output"][2]["arguments"] = json!(r#"{"n":1,"leak":"secret-key"}"#)
            }
            Provider::Anthropic => {
                tool_response["content"][3]["input"] = json!({"n":1,"leak":"secret-key"})
            }
        }
        let server = Server::new(vec![p.answer("secret-key").into(), tool_response.into()]);
        let client = p.client(&server);
        let prepared = client.prepare(&context(&session()), &p.settings()).unwrap();
        for _ in 0..2 {
            let observed = client.invoke(&prepared);
            let error = observed.result.unwrap_err();
            assert_eq!(error.kind(), ModelErrorKind::MalformedResponse);
            assert_safe(&error);
            let ResponseCapture::Complete(bytes) = observed.receipt.capture else {
                panic!()
            };
            assert!(!String::from_utf8_lossy(&bytes).contains("secret-key"));
            assert!(String::from_utf8_lossy(&bytes).contains("**********"));
        }
    }
}

#[test]
fn every_closed_stop_remains_portable_after_tool_facts_and_output_free_failures() {
    for p in PROVIDERS {
        for closure in [
            TurnOutcome::Interrupted(StopReason::IterationLimit),
            TurnOutcome::Interrupted(StopReason::UserCancelled),
            TurnOutcome::Interrupted(StopReason::ContextCapacity),
            TurnOutcome::Interrupted(StopReason::ProcessLost),
            TurnOutcome::Failed(TurnFailure::ModelFailure(text("provider stopped"))),
            TurnOutcome::Failed(TurnFailure::MalformedResponse(text("bad reply"))),
        ] {
            let stopped = match p {
                Provider::OpenAI => {
                    json!({"status":"incomplete","incomplete_details":{"reason":"max_output_tokens"}})
                }
                Provider::Anthropic => json!({"stop_reason":"max_tokens"}),
            };
            let refused = match p {
                Provider::OpenAI => {
                    json!({"status":"completed","output":[{"type":"message","role":"assistant","content":[{"type":"refusal","refusal":"no"}]}]})
                }
                Provider::Anthropic => json!({"stop_reason":"refusal"}),
            };
            let server = Server::new(vec![
                p.tool(1).into(),
                stopped.into(),
                refused.into(),
                p.answer("resumed").into(),
            ]);
            let client = p.client(&server);
            let mut archive = session();
            invoke_assistant(p, client.as_ref(), &mut archive);
            accept_tools(&mut archive);
            let prepared = client.prepare(&context(&archive), &p.settings()).unwrap();
            let turn = archive.session().active_turn().unwrap().id();
            let generation = archive
                .begin_generation(InvocationRole::Assistant { turn }, &prepared)
                .unwrap();
            let observed = client.invoke(&prepared);
            assert!(matches!(
                observed.result,
                Ok(Generation::Incomplete {
                    reason: IncompleteReason::OutputLimit
                })
            ));
            archive
                .record_observation(
                    generation,
                    observed.at,
                    observed.receipt,
                    Disposition::Incomplete(IncompleteReason::OutputLimit),
                )
                .unwrap();
            match closure {
                TurnOutcome::Interrupted(reason) => {
                    archive.session_mut().interrupt(turn, reason).unwrap()
                }
                TurnOutcome::Failed(reason) => archive.session_mut().fail(turn, reason).unwrap(),
                TurnOutcome::Completed => unreachable!(),
            }
            archive
                .session_mut()
                .start_turn(user("try once more"))
                .unwrap();
            let prepared = client.prepare(&context(&archive), &p.settings()).unwrap();
            let turn = archive.session().active_turn().unwrap().id();
            let generation = archive
                .begin_generation(InvocationRole::Assistant { turn }, &prepared)
                .unwrap();
            let observed = client.invoke(&prepared);
            assert!(matches!(observed.result, Ok(Generation::Refused { .. })));
            archive
                .record_observation(
                    generation,
                    observed.at,
                    observed.receipt,
                    Disposition::Refused { reason: None },
                )
                .unwrap();
            archive
                .session_mut()
                .fail(
                    turn,
                    TurnFailure::ModelFailure(text("stopped without output")),
                )
                .unwrap();
            archive
                .session_mut()
                .start_turn(user("continue with known facts"))
                .unwrap();
            let initial = ArchiveChange::initial(archive).unwrap();
            let archive = SessionArchive::decode(initial.encoded()).unwrap();
            let reply = client.request(&context(&archive), &p.settings()).unwrap();
            assert_eq!(
                complete(reply),
                AssistantMessage::answer("resumed").unwrap()
            );
            let body = server.request(3).body.to_string();
            for fact in [
                "lookup",
                "returned",
                "stopped without output",
                "continue with known facts",
            ] {
                assert!(body.contains(fact));
            }
            for native in [
                "signature-1",
                "function_call_output",
                "tool_result",
                "native-1",
            ] {
                assert!(!body.contains(native));
            }
            assert_eq!(server.requests().len(), 4);
        }
    }
}

#[test]
fn escaped_credentials_are_rejected_and_scrubbed_in_nested_json_and_error_captures() {
    for p in PROVIDERS {
        let mut tool_response = p.tool(1);
        match p {
            Provider::OpenAI => {
                tool_response["output"][2]["arguments"] =
                    json!(r#"{"n":1,"leak":"\u0073ecret-key"}"#)
            }
            Provider::Anthropic => {
                tool_response["content"][3]["input"] =
                    json!({"n":1,"leak":r#"{"value":"\u0073ecret-key"}"#})
            }
        }
        let escaped_answer = serde_json::to_string(&p.answer("secret-key"))
            .unwrap()
            .replace("secret-key", r"\u0073ecret-key");
        let error = Reply {
            status: 500,
            body: r#"{"error":"\u0073ecret-key"}"#.into(),
            ..Reply::from(json!({}))
        };
        let server = Server::new(vec![
            tool_response.into(),
            Reply {
                body: escaped_answer,
                ..Reply::from(json!({}))
            },
            error,
        ]);
        let client = p.client(&server);
        let prepared = client.prepare(&context(&session()), &p.settings()).unwrap();
        for expected in [
            ModelErrorKind::MalformedResponse,
            ModelErrorKind::MalformedResponse,
            ModelErrorKind::ProviderFailure,
        ] {
            let observed = client.invoke(&prepared);
            assert_eq!(observed.result.unwrap_err().kind(), expected);
            let ResponseCapture::Complete(bytes) = observed.receipt.capture else {
                panic!()
            };
            let captured = String::from_utf8(bytes).unwrap();
            assert!(!captured.contains("secret-key"));
            assert!(!captured.contains(r"\u0073ecret-key"));
            assert!(captured.contains("**********"));
        }
    }
}

#[test]
fn truncated_escaped_credentials_keep_capture_length_and_can_be_committed() {
    for p in PROVIDERS {
        for trailing in ["", "\\", "\\u0", "\\q"] {
            let encoded_key: String = "secret-key"
                .chars()
                .map(|c| format!("\\u{:04x}", c as u32))
                .collect();
            let prefix = if trailing.is_empty() {
                format!("{{\"error\":\"{encoded_key}\",\"padding\":\"")
            } else {
                format!("{{\"error\":\"{encoded_key}{trailing}")
            };
            let max = prefix.len();
            let server = Server::new(vec![Reply {
                status: 500,
                body: prefix + &"x".repeat(100),
                ..Reply::from(json!({}))
            }]);
            let config = ClientConfig::new(
                &server.url,
                "secret-key",
                Duration::from_secs(1),
                NonZeroUsize::new(max).unwrap(),
            )
            .unwrap();
            let client = p.configured(config);
            let mut archive = session();
            let prepared = client.prepare(&context(&archive), &p.settings()).unwrap();
            let turn = archive.session().active_turn().unwrap().id();
            let generation = archive
                .begin_generation(InvocationRole::Assistant { turn }, &prepared)
                .unwrap();
            let observed = client.invoke(&prepared);
            assert_eq!(
                observed.result.unwrap_err().kind(),
                ModelErrorKind::ResponseTooLarge
            );
            let ResponseCapture::Truncated { prefix, limit } = &observed.receipt.capture else {
                panic!()
            };
            assert_eq!(prefix.len(), max);
            assert_eq!(*limit, max);
            assert!(!String::from_utf8_lossy(prefix).contains(&encoded_key));
            assert!(String::from_utf8_lossy(prefix).contains("**********"));
            archive
                .record_observation(
                    generation,
                    observed.at,
                    observed.receipt,
                    Disposition::Failed(ModelError::new(ModelErrorKind::ResponseTooLarge)),
                )
                .unwrap();
            let committed = ArchiveChange::initial(archive).unwrap();
            assert_eq!(
                SessionArchive::decode(committed.encoded()).unwrap(),
                *committed.candidate()
            );
        }
    }
}

#[test]
fn assembled_text_cannot_join_credential_fragments_in_answers_or_decisions() {
    for p in PROVIDERS {
        for reviewer in [false, true] {
            let first = if reviewer {
                r#"{"should_allow":true,"reason":"secret-"#
            } else {
                "secret-"
            };
            let last = if reviewer { "key\"}" } else { "key" };
            let mut response = p.answer(first);
            match p {
                Provider::OpenAI => response["output"][1]["content"]
                    .as_array_mut()
                    .unwrap()
                    .push(json!({"type":"output_text","text":last})),
                Provider::Anthropic => response["content"]
                    .as_array_mut()
                    .unwrap()
                    .push(json!({"type":"text","text":last})),
            }
            let server = Server::new(vec![response.into()]);
            let receipt = if reviewer {
                let client = p.reviewer(&server);
                let prepared = client
                    .prepare(&context::<Decision>(&session()), &p.settings())
                    .unwrap();
                let observed = client.invoke(&prepared);
                assert_eq!(
                    observed.result.unwrap_err().kind(),
                    ModelErrorKind::MalformedResponse
                );
                observed.receipt
            } else {
                let client = p.client(&server);
                let prepared = client.prepare(&context(&session()), &p.settings()).unwrap();
                let observed = client.invoke(&prepared);
                assert_eq!(
                    observed.result.unwrap_err().kind(),
                    ModelErrorKind::MalformedResponse
                );
                observed.receipt
            };
            let ResponseCapture::Complete(bytes) = receipt.capture else {
                panic!()
            };
            assert!(!bytes.is_empty());
            assert!(bytes.iter().all(|byte| *byte == b'*'));
        }
    }
}

#[test]
fn nested_escaped_receipt_metadata_is_scrubbed_in_all_copies() {
    for p in PROVIDERS {
        let encoded = r#""\u0073ecret-key""#;
        let mut response = p.answer("ordinary");
        for field in [
            "id",
            "model",
            match p {
                Provider::OpenAI => "status",
                Provider::Anthropic => "stop_reason",
            },
        ] {
            response[field] = json!(encoded);
        }
        let server = Server::new(vec![Reply {
            status: 500,
            headers: vec![("X-Request-ID".into(), encoded.into())],
            ..Reply::from(response)
        }]);
        let client = p.client(&server);
        let prepared = client.prepare(&context(&session()), &p.settings()).unwrap();
        let observed = client.invoke(&prepared);
        assert_eq!(
            observed.result.unwrap_err().kind(),
            ModelErrorKind::ProviderFailure
        );
        let mut archive = session();
        let turn = archive.session().active_turn().unwrap().id();
        let generation = archive
            .begin_generation(InvocationRole::Assistant { turn }, &prepared)
            .unwrap();
        archive
            .record_observation(
                generation,
                observed.at,
                observed.receipt.clone(),
                Disposition::Failed(ModelError::new(ModelErrorKind::ProviderFailure)),
            )
            .unwrap();
        let change = ArchiveChange::initial(archive).unwrap();
        assert_eq!(
            SessionArchive::decode(change.encoded()).unwrap(),
            *change.candidate()
        );
        for value in [
            observed.receipt.served_model,
            observed.receipt.response_id,
            observed.receipt.request_id,
            observed.receipt.finish,
        ] {
            let value = value.unwrap();
            assert!(!value.contains("secret-key"));
            assert!(!value.contains(r"\u0073ecret-key"));
            assert!(value.contains("**********"));
        }
    }
}

#[test]
fn stopped_text_fragments_cannot_reconstruct_credentials_from_durable_captures() {
    for p in PROVIDERS {
        let mut response = p.answer("secret-");
        match p {
            Provider::OpenAI => {
                response["status"] = json!("incomplete");
                response["incomplete_details"] = json!({"reason":"max_output_tokens"});
                response["output"][1]["content"]
                    .as_array_mut()
                    .unwrap()
                    .push(json!({"type":"output_text","text":"key"}));
            }
            Provider::Anthropic => {
                response["stop_reason"] = json!("max_tokens");
                response["content"]
                    .as_array_mut()
                    .unwrap()
                    .push(json!({"type":"text","text":"key"}));
            }
        }
        let server = Server::new(vec![response.into()]);
        let client = p.client(&server);
        let mut archive = session();
        let prepared = client.prepare(&context(&archive), &p.settings()).unwrap();
        let turn = archive.session().active_turn().unwrap().id();
        let generation = archive
            .begin_generation(InvocationRole::Assistant { turn }, &prepared)
            .unwrap();
        let observed = client.invoke(&prepared);
        assert!(matches!(
            observed.result,
            Ok(Generation::Incomplete {
                reason: IncompleteReason::OutputLimit
            })
        ));
        let ResponseCapture::Complete(bytes) = &observed.receipt.capture else {
            panic!()
        };
        assert!(!bytes.is_empty());
        assert!(bytes.iter().all(|byte| *byte == b'*'));
        archive
            .record_observation(
                generation,
                observed.at,
                observed.receipt,
                Disposition::Incomplete(IncompleteReason::OutputLimit),
            )
            .unwrap();
        let change = ArchiveChange::initial(archive).unwrap();
        assert_eq!(
            SessionArchive::decode(change.encoded()).unwrap(),
            *change.candidate()
        );
        assert!(
            change
                .candidate()
                .session()
                .active_turn()
                .unwrap()
                .steps()
                .is_empty()
        );
    }
}
