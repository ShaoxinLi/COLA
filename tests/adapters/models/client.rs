use super::support::*;
use cola::adapters::stores::JsonlSessionStore;
use cola::{
    adapters::models::{ClientConfig, OpenAIClient},
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
                .generate(&context(&session()), &p.settings(), None)
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
            .generate(&context(&session()), &p.settings(), None)
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
            .generate(&context(&session()), &p.settings(), None)
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
            .generate(&context(&session()), &p.settings(), None)
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
            .generate(&context(&session()), &p.settings(), None)
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
            .generate(&context(&session()), &p.settings(), None)
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
            .generate(&context(&session()), &p.settings(), None)
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
                    .generate(&context(&session()), &p.settings(), None)
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
                .generate(&context(&session()), &p.settings(), None)
                .unwrap_err()
                .kind(),
            ModelErrorKind::MalformedResponse
        );
        let server = Server::new(vec![p.answer("  ").into()]);
        assert_eq!(
            p.client(&server)
                .generate(&context(&session()), &p.settings(), None)
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
            .generate(&context(&session()), &p.settings(), None)
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
            .generate(&context(&session()), &p.settings(), None)
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
            .generate(&context(&session()), &p.settings(), None)
            .unwrap_err();
        assert_eq!(error.kind(), ModelErrorKind::Timeout);
        assert_eq!(error.http_status(), None);
        assert_safe(&error);
    }
}

#[test]
fn assistant_requires_an_active_last_exchange_before_http() {
    for p in PROVIDERS {
        let server = Server::new(vec![]);
        let empty = cola::domain::Context::new(text("instructions"), vec![], vec![]).unwrap();
        assert_eq!(
            p.client(&server)
                .generate(&empty, &p.settings(), None)
                .unwrap_err()
                .kind(),
            ModelErrorKind::UnsupportedInput
        );
        let mut s = session();
        accept(
            &mut s,
            cola::domain::AssistantMessage::answer("historical answer").unwrap(),
        );
        assert_eq!(
            p.client(&server)
                .generate(&context(&s), &p.settings(), None)
                .unwrap_err()
                .kind(),
            ModelErrorKind::UnsupportedInput
        );
        assert!(server.requests().is_empty());
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
            let (answer, _) = complete(
                client
                    .generate(&context(&session()), &settings, None)
                    .unwrap(),
            );
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
        let (d, continuation) = complete(
            client
                .generate(&context::<Decision>(&session()), &p.settings(), None)
                .unwrap(),
        );
        assert!(d.should_allow());
        assert!(d.reason().is_none());
        assert!(continuation.is_none());
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
                    .generate(&context::<Decision>(&session()), &p.settings(), None)
                    .unwrap_err()
                    .kind(),
                ModelErrorKind::MalformedResponse
            );
        }
        let server = Server::new(vec![p.tool(1).into()]);
        assert_eq!(
            p.reviewer(&server)
                .generate(&context::<Decision>(&session()), &p.settings(), None)
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
            let ctx = Context::new(
                text("instructions"),
                s.context_exchanges().unwrap(),
                vec![ToolDefinition::new(
                    ToolName::new("lookup").unwrap(),
                    text("tool"),
                    InputSchema::new(schema.clone()).unwrap(),
                )],
            )
            .unwrap();
            p.client(&server)
                .generate(&ctx, &p.settings(), None)
                .unwrap();
            let r = server.request(0);
            let actual = match p {
                Provider::OpenAI => &r.body["tools"][0]["parameters"],
                Provider::Anthropic => &r.body["tools"][0]["input_schema"],
            };
            assert_eq!(actual, &schema);
        }
    }
}

fn interrupted_history() -> Session {
    let mut s = session();
    let turn = s.active_turn().unwrap().id();
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
    s.record_assistant(turn, AssistantMessage::tool_requests(None, calls.clone()))
        .unwrap();
    let calls = calls.as_slice();
    s.start_tool(turn, calls[0].id()).unwrap();
    s.record_tool_result(
        turn,
        ToolResult::new(calls[0].id().clone(), ToolOutcome::Success(String::new())),
    )
    .unwrap();
    s.start_tool(turn, calls[1].id()).unwrap();
    s.interrupt(turn, StopReason::ProcessLost).unwrap();
    s.resolve_effect(
        turn,
        calls[1].id(),
        EffectResolution::new(
            EffectDisposition::NotPerformed,
            ResolutionSource::Observation,
            text("confirmed absent"),
        ),
    )
    .unwrap();
    s.start_turn(user("continue from facts")).unwrap();
    s
}

#[test]
fn notices_and_review_history_are_facts_at_their_exchange_position() {
    for p in PROVIDERS {
        for decision in [false, true] {
            let mut s = interrupted_history();
            if decision {
                let turn = s.active_turn().unwrap().id();
                s.record_assistant(turn, AssistantMessage::answer("previous answer").unwrap())
                    .unwrap();
                let turn = s.start_turn(user("another")).unwrap();
                let calls = ToolCalls::new(vec![
                    ToolCall::new(
                        ToolCallId::new("closed-call").unwrap(),
                        ToolName::new("lookup").unwrap(),
                        json!({}),
                    )
                    .unwrap(),
                ])
                .unwrap();
                s.record_assistant(turn, AssistantMessage::tool_requests(None, calls))
                    .unwrap();
                let call = ToolCallId::new("closed-call").unwrap();
                s.start_tool(turn, &call).unwrap();
                s.record_tool_result(
                    turn,
                    ToolResult::new(
                        call,
                        ToolOutcome::Failure(ToolError::new(text("ordinary failure"))),
                    ),
                )
                .unwrap();
                s.record_assistant(turn, AssistantMessage::answer("finished").unwrap())
                    .unwrap();
            }
            let response = p.answer(if decision {
                r#"{"should_allow":false,"reason":"reviewed"}"#
            } else {
                "acknowledged"
            });
            let server = Server::new(vec![response.into()]);
            if decision {
                p.reviewer(&server)
                    .generate(&context::<Decision>(&s), &p.settings(), None)
                    .unwrap();
            } else {
                p.client(&server)
                    .generate(&context(&s), &p.settings(), None)
                    .unwrap();
            }
            let body = server.request(0).body.to_string();
            for fact in [
                "turn_closed",
                "confirmed_result",
                "unknown_effect",
                "undispatched",
                "not_performed",
                "observation",
                "confirmed absent",
            ] {
                assert!(body.contains(fact), "missing {fact} in {body}");
            }
            assert!(!body.contains("tool_result"));
            assert!(!body.contains("functionResponse"));
            assert!(!body.contains("function_call_output"));
            assert!(
                body.find("unknown_effect").unwrap() < body.find("continue from facts").unwrap()
            );
            if decision {
                assert!(body.contains("ordinary failure"));
                assert!(body.contains("requested_calls"));
            }
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
            .generate(&context::<Decision>(&session()), &p.settings(), None)
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
        let (output, token) = complete(client.generate(&context(&s), &p.settings(), None).unwrap());
        assert_eq!(output.calls().unwrap().as_slice().len(), 2);
        accept(&mut s, output);
        client
            .generate(&context(&s), &p.settings(), token.as_ref())
            .unwrap();
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
            client
                .generate(&context(&session()), &settings, None)
                .unwrap();
            let error = client
                .generate(&context(&session()), &settings, None)
                .unwrap_err();
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
fn empty_review_history_still_has_native_user_input_and_descriptive_catalog() {
    for p in PROVIDERS {
        let server = Server::new(vec![
            p.answer(r#"{"should_allow":false,"reason":"denied"}"#)
                .into(),
        ]);
        let ctx = Context::<Decision>::new(
            text("Review the request in these instructions"),
            vec![],
            vec![tool()],
        )
        .unwrap();
        let mut settings = p.settings();
        settings.effort = Effort::Off;
        let (decision, token) =
            complete(p.reviewer(&server).generate(&ctx, &settings, None).unwrap());
        assert!(!decision.should_allow());
        assert_eq!(decision.reason().unwrap().as_str(), "denied");
        assert!(token.is_none());
        let body = server.request(0).body;
        assert!(body.get("tools").is_none());
        assert!(
            body.to_string()
                .contains("Review the request in these instructions")
        );
        assert!(body.to_string().contains("Review tool catalog"));
        let input = match p {
            Provider::OpenAI => &body["input"],
            Provider::Anthropic => &body["messages"],
        };
        assert_eq!(input.as_array().unwrap().len(), 1);
        assert_eq!(input[0]["role"], "user");
        if matches!(p, Provider::Anthropic) {
            assert_eq!(body["thinking"], json!({"type":"disabled"}));
            assert!(body["output_config"].get("effort").is_none());
            assert!(body["output_config"].get("format").is_some());
        }
    }
}

#[test]
fn every_resolution_disposition_and_provenance_is_rendered_as_a_fact() {
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
                let mut snapshot = interrupted_history().snapshot();
                snapshot.turns[0].steps[0].executions[1].resolution = Some(EffectResolution::new(
                    disposition,
                    source,
                    text("resolution evidence"),
                ));
                let mut s = Session::restore(snapshot).unwrap();
                s.start_turn(user("review recovery facts")).unwrap();
                let server = Server::new(vec![p.answer("acknowledged").into()]);
                p.client(&server)
                    .generate(&context(&s), &p.settings(), None)
                    .unwrap();
                let body = server.request(0).body.to_string();
                assert!(body.contains(label));
                assert!(body.contains(provenance));
                assert!(body.contains("resolution evidence"));
                assert!(!body.contains("function_call_output"));
                assert!(!body.contains("tool_result"));
            }
        }
    }
}

#[test]
fn stopped_generations_resume_after_confirmed_batches_and_output_free_turns() {
    let closures = [
        TurnOutcome::Interrupted(StopReason::IterationLimit),
        TurnOutcome::Interrupted(StopReason::UserCancelled),
        TurnOutcome::Interrupted(StopReason::ContextCapacity),
        TurnOutcome::Interrupted(StopReason::ProcessLost),
        TurnOutcome::Failed(TurnFailure::ModelFailure(text("provider stopped"))),
        TurnOutcome::Failed(TurnFailure::MalformedResponse(text("bad reply"))),
    ];
    for p in PROVIDERS {
        for closure in &closures {
            let first = p.tool(1);
            let incomplete = match p {
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
                first.clone().into(),
                incomplete.into(),
                refused.into(),
                p.answer("resumed").into(),
            ]);
            let client = p.client(&server);
            let mut s = session();
            let (output, token) =
                complete(client.generate(&context(&s), &p.settings(), None).unwrap());
            accept(&mut s, output);
            assert!(matches!(
                client
                    .generate(&context(&s), &p.settings(), token.as_ref())
                    .unwrap(),
                Generation::Incomplete {
                    reason: IncompleteReason::OutputLimit
                }
            ));
            let turn = s.active_turn().unwrap().id();
            match closure {
                TurnOutcome::Interrupted(reason) => s.interrupt(turn, *reason).unwrap(),
                TurnOutcome::Failed(reason) => s.fail(turn, reason.clone()).unwrap(),
                TurnOutcome::Completed => unreachable!(),
            }
            s.start_turn(user("try once more")).unwrap();
            assert!(matches!(
                client
                    .generate(&context(&s), &p.settings(), token.as_ref())
                    .unwrap(),
                Generation::Refused { .. }
            ));
            let turn = s.active_turn().unwrap().id();
            s.fail(
                turn,
                TurnFailure::ModelFailure(text("stopped without output")),
            )
            .unwrap();
            s.start_turn(user("continue with known facts")).unwrap();
            let (output, next) = complete(
                client
                    .generate(&context(&s), &p.settings(), token.as_ref())
                    .unwrap(),
            );
            assert_eq!(
                p.request_native(&server.request(3).body),
                p.native(&first).as_array().unwrap().clone()
            );
            assert!(server.request(3).body.to_string().contains("turn_closed"));
            accept(&mut s, output);
            s.start_turn(user("next input")).unwrap();
            let valid = context::<AssistantMessage>(&s);
            let mut exchanges = valid.exchanges().to_vec();
            let closed = &exchanges[1];
            exchanges[1] = ContextExchange::new(
                closed.turn_id(),
                closed.messages().to_vec(),
                Some(TurnOutcome::Failed(TurnFailure::ModelFailure(text(
                    "rewritten closure",
                )))),
            )
            .unwrap();
            assert_eq!(
                client
                    .generate(&rebuild(&valid, exchanges), &p.settings(), next.as_ref())
                    .unwrap_err()
                    .kind(),
                ModelErrorKind::ContinuationMismatch
            );
            assert_eq!(server.requests().len(), 4);
        }
    }
}

#[test]
fn partial_receipted_batches_cannot_be_resumed_as_complete_native_history() {
    for p in PROVIDERS {
        let server = Server::new(vec![p.tool(1).into()]);
        let client = p.client(&server);
        let mut s = session();
        let (output, token) = complete(client.generate(&context(&s), &p.settings(), None).unwrap());
        let turn = s.active_turn().unwrap().id();
        s.record_assistant(turn, output).unwrap();
        s.interrupt(turn, StopReason::UserCancelled).unwrap();
        s.start_turn(user("new attempt")).unwrap();
        assert_eq!(
            client
                .generate(&context(&s), &p.settings(), token.as_ref())
                .unwrap_err()
                .kind(),
            ModelErrorKind::ContinuationMismatch
        );
        assert_eq!(server.requests().len(), 1);
    }
}

#[test]
fn cumulative_native_replay_survives_four_generations_and_completed_turns() {
    for p in PROVIDERS {
        let mut first = p.tool(1);
        let mut second = p.tool(2);
        let mut terminal = p.answer("first turn done");
        for response in [&mut first, &mut second, &mut terminal] {
            let blocks = match p {
                Provider::OpenAI => &mut response["output"],
                Provider::Anthropic => &mut response["content"],
            };
            for block in blocks.as_array_mut().unwrap() {
                block["future_native_field"] = json!({"opaque":["preserve exactly",null,3]});
            }
        }
        let server = Server::new(vec![
            first.clone().into(),
            second.clone().into(),
            terminal.clone().into(),
            p.answer("second turn done").into(),
        ]);
        let client = p.client(&server);
        let mut s = session();
        let settings = p.settings();
        let (output, one) = complete(client.generate(&context(&s), &settings, None).unwrap());
        accept(&mut s, output);
        let one = one.unwrap();
        let one_debug = format!("{one:?}");
        let (output, two) = complete(
            client
                .generate(&context(&s), &settings, Some(&one))
                .unwrap(),
        );
        accept(&mut s, output);
        let (output, three) = complete(
            client
                .generate(&context(&s), &settings, two.as_ref())
                .unwrap(),
        );
        accept(&mut s, output);
        s.start_turn(user("new user turn")).unwrap();
        let cross_client = p.client(&server);
        cross_client
            .generate(&context(&s), &settings, three.as_ref())
            .unwrap();
        let expected: Vec<_> = [first, second, terminal]
            .iter()
            .flat_map(|r| p.native(r).as_array().unwrap().clone())
            .collect();
        assert_eq!(p.request_native(&server.request(3).body), expected);
        assert_eq!(format!("{one:?}"), one_debug);
        assert!(!one_debug.contains("signature"));
        let r = server.request(1).body.to_string();
        assert!(r.contains("signature-1"));
        assert!(!r.contains("signature-2"));
    }
}

#[test]
fn cloned_token_is_reusable_after_http_and_decode_failures() {
    for p in PROVIDERS {
        let bad = Reply {
            status: 503,
            body: "secret body".into(),
            delay: std::time::Duration::ZERO,
            body_delay: std::time::Duration::ZERO,
            headers: vec![],
        };
        let broken = Reply {
            status: 200,
            body: "{broken".into(),
            delay: std::time::Duration::ZERO,
            body_delay: std::time::Duration::ZERO,
            headers: vec![],
        };
        let server = Server::new(vec![
            p.tool(1).into(),
            bad,
            broken,
            p.answer("recovered").into(),
        ]);
        let client = p.client(&server);
        let mut s = session();
        let (output, token) = complete(client.generate(&context(&s), &p.settings(), None).unwrap());
        accept(&mut s, output);
        let token = token.unwrap();
        let saved = token.clone();
        let ctx = context(&s);
        assert_eq!(
            client
                .generate(&ctx, &p.settings(), Some(&token))
                .unwrap_err()
                .kind(),
            ModelErrorKind::ProviderFailure
        );
        assert_eq!(
            client
                .generate(&ctx, &p.settings(), Some(&token))
                .unwrap_err()
                .kind(),
            ModelErrorKind::MalformedResponse
        );
        client.generate(&ctx, &p.settings(), Some(&saved)).unwrap();
        let requests = server.requests();
        assert_eq!(requests[1].body, requests[2].body);
        assert_eq!(requests[2].body, requests[3].body);
    }
}

fn rebuild(
    ctx: &Context<AssistantMessage>,
    exchanges: Vec<ContextExchange>,
) -> Context<AssistantMessage> {
    Context::new(ctx.instructions().clone(), exchanges, ctx.tools().to_vec()).unwrap()
}

#[test]
fn model_endpoint_provider_history_and_receipt_binding_fail_without_network() {
    for p in PROVIDERS {
        let server = Server::new(vec![p.tool(1).into(), p.answer("good").into()]);
        let client = p.client(&server);
        let mut s = session();
        let before = context(&s);
        let settings = p.settings();
        let (output, token) = complete(client.generate(&before, &settings, None).unwrap());
        accept(&mut s, output.clone());
        let valid = context(&s);
        let token = token.unwrap();
        assert_eq!(
            client
                .generate(&before, &settings, Some(&token))
                .unwrap_err()
                .kind(),
            ModelErrorKind::ContinuationMismatch
        );
        let mut invalid = vec![];
        let exchange = &valid.exchanges()[0];
        let mut messages = exchange.messages().to_vec();
        if let Message::User(user) = &mut messages[0] {
            *user = super::support::user("rewritten user");
        }
        invalid.push(rebuild(
            &valid,
            vec![ContextExchange::new(exchange.turn_id(), messages, None).unwrap()],
        ));
        invalid.push(rebuild(
            &valid,
            vec![
                ContextExchange::new(TurnId::new(2).unwrap(), exchange.messages().to_vec(), None)
                    .unwrap(),
            ],
        ));
        let mut messages = exchange.messages().to_vec();
        let call = ToolCall::new(
            ToolCallId::new("hidden-call").unwrap(),
            ToolName::new("lookup").unwrap(),
            json!({}),
        )
        .unwrap();
        messages.push(Message::Assistant(AssistantMessage::tool_requests(
            None,
            ToolCalls::new(vec![call.clone()]).unwrap(),
        )));
        messages.push(Message::Tool(ToolResult::new(
            call.id().clone(),
            ToolOutcome::Success("hidden".into()),
        )));
        invalid.push(rebuild(
            &valid,
            vec![ContextExchange::new(exchange.turn_id(), messages, None).unwrap()],
        ));
        let closed = rebuild(
            &valid,
            vec![
                ContextExchange::new(
                    exchange.turn_id(),
                    exchange.messages().to_vec(),
                    Some(TurnOutcome::Interrupted(StopReason::IterationLimit)),
                )
                .unwrap(),
            ],
        );
        assert_eq!(
            client
                .generate(&closed, &settings, Some(&token))
                .unwrap_err()
                .kind(),
            ModelErrorKind::UnsupportedInput
        );
        let mut messages = exchange.messages().to_vec();
        let calls = output.calls().unwrap().clone();
        messages[1] = Message::Assistant(AssistantMessage::tool_requests(
            Some("forged".into()),
            calls,
        ));
        invalid.push(rebuild(
            &valid,
            vec![ContextExchange::new(exchange.turn_id(), messages, None).unwrap()],
        ));
        for ctx in invalid {
            assert_eq!(
                client
                    .generate(&ctx, &settings, Some(&token))
                    .unwrap_err()
                    .kind(),
                ModelErrorKind::ContinuationMismatch
            );
        }
        let mut changed = settings.clone();
        changed.model_id = ModelId::new("different-model").unwrap();
        assert_eq!(
            client
                .generate(&valid, &changed, Some(&token))
                .unwrap_err()
                .kind(),
            ModelErrorKind::ContinuationMismatch
        );
        let other = Server::new(vec![]);
        assert_eq!(
            p.client(&other)
                .generate(&valid, &settings, Some(&token))
                .unwrap_err()
                .kind(),
            ModelErrorKind::ContinuationMismatch
        );
        assert!(other.requests().is_empty());
        let q = match p {
            Provider::OpenAI => Provider::Anthropic,
            _ => Provider::OpenAI,
        };
        assert_eq!(
            q.client(&server)
                .generate(&valid, &q.settings(), Some(&token))
                .unwrap_err()
                .kind(),
            ModelErrorKind::ContinuationMismatch
        );
        assert_eq!(server.requests().len(), 1);
        client.generate(&valid, &settings, Some(&token)).unwrap();
        assert_eq!(server.requests().len(), 2);
    }
}

#[test]
fn completed_receipts_bind_history_boundaries_and_allow_one_new_user_only() {
    for p in PROVIDERS {
        let server = Server::new(vec![p.answer("accepted").into(), p.answer("next").into()]);
        let client = p.client(&server);
        let mut s = session();
        let (output, token) = complete(client.generate(&context(&s), &p.settings(), None).unwrap());
        accept(&mut s, output);
        s.start_turn(user("new input")).unwrap();
        let valid = context(&s);
        let mut exchanges = valid.exchanges().to_vec();
        assert_eq!(
            client
                .generate(
                    &rebuild(&valid, vec![exchanges[1].clone()]),
                    &p.settings(),
                    token.as_ref()
                )
                .unwrap_err()
                .kind(),
            ModelErrorKind::ContinuationMismatch
        );
        exchanges[1] = ContextExchange::new(
            exchanges[1].turn_id(),
            vec![
                Message::User(user("new input")),
                Message::Assistant(AssistantMessage::answer("hidden").unwrap()),
            ],
            Some(TurnOutcome::Completed),
        )
        .unwrap();
        exchanges.push(
            ContextExchange::new(
                TurnId::new(3).unwrap(),
                vec![Message::User(user("third"))],
                None,
            )
            .unwrap(),
        );
        assert_eq!(
            client
                .generate(&rebuild(&valid, exchanges), &p.settings(), token.as_ref())
                .unwrap_err()
                .kind(),
            ModelErrorKind::ContinuationMismatch
        );
        assert_eq!(server.requests().len(), 1);
        client
            .generate(&valid, &p.settings(), token.as_ref())
            .unwrap();
    }
}

#[test]
fn preexisting_notices_resolutions_and_closed_outcomes_are_fully_bound() {
    for p in PROVIDERS {
        let server = Server::new(vec![p.tool(1).into(), p.answer("continue").into()]);
        let client = p.client(&server);
        let mut s = interrupted_history();
        let (output, token) = complete(client.generate(&context(&s), &p.settings(), None).unwrap());
        accept(&mut s, output);
        let valid = context(&s);
        let mut exchanges = valid.exchanges().to_vec();
        exchanges[0] = ContextExchange::new(
            exchanges[0].turn_id(),
            exchanges[0].messages().to_vec(),
            Some(TurnOutcome::Interrupted(StopReason::ProcessLost)),
        )
        .unwrap();
        assert_eq!(
            client
                .generate(&rebuild(&valid, exchanges), &p.settings(), token.as_ref())
                .unwrap_err()
                .kind(),
            ModelErrorKind::ContinuationMismatch
        );
        let mut changed = interrupted_history().snapshot();
        changed.turns[0].outcome = Some(TurnOutcome::Interrupted(StopReason::UserCancelled));
        let altered = Session::restore(changed).unwrap();
        let closed = altered.closed_context_exchanges().unwrap().remove(0);
        let mut exchanges = valid.exchanges().to_vec();
        exchanges[0] = closed;
        assert_eq!(
            client
                .generate(&rebuild(&valid, exchanges), &p.settings(), token.as_ref())
                .unwrap_err()
                .kind(),
            ModelErrorKind::ContinuationMismatch
        );
        assert_eq!(server.requests().len(), 1);
        client
            .generate(&valid, &p.settings(), token.as_ref())
            .unwrap();
    }
}

#[test]
fn jsonl_recovery_cannot_fabricate_native_history() {
    for p in PROVIDERS {
        let server = Server::new(vec![p.tool(1).into()]);
        let client = p.client(&server);
        let mut s = session();
        let (output, _) = complete(client.generate(&context(&s), &p.settings(), None).unwrap());
        accept(&mut s, output);
        let dir = tempfile::tempdir().unwrap();
        let store = JsonlSessionStore::new(dir.path());
        store.save(&s).unwrap();
        let mut restored = store.load(s.id()).unwrap();
        restored.start_turn(user("after recovery")).unwrap();
        assert_eq!(
            client
                .generate(&context(&restored), &p.settings(), None)
                .unwrap_err()
                .kind(),
            ModelErrorKind::MissingContinuation
        );
        assert_eq!(server.requests().len(), 1);
    }
}

#[test]
fn previously_received_results_and_resolutions_cannot_be_rewritten() {
    for p in PROVIDERS {
        let server = Server::new(vec![
            p.tool(1).into(),
            p.tool(2).into(),
            p.answer("good").into(),
        ]);
        let client = p.client(&server);
        let mut s = interrupted_history();
        let settings = p.settings();
        let (output, one) = complete(client.generate(&context(&s), &settings, None).unwrap());
        accept(&mut s, output);
        let (output, two) = complete(
            client
                .generate(&context(&s), &settings, one.as_ref())
                .unwrap(),
        );
        accept(&mut s, output);
        let valid = context(&s);
        let mut exchanges = valid.exchanges().to_vec();
        let last = exchanges.last().unwrap();
        let mut messages = last.messages().to_vec();
        let Message::Tool(original) = &messages[2] else {
            panic!("expected result")
        };
        messages[2] = Message::Tool(ToolResult::new(
            original.call_id().clone(),
            ToolOutcome::Failure(ToolError::new(text("rewritten result"))),
        ));
        let changed = ContextExchange::new(last.turn_id(), messages, None).unwrap();
        *exchanges.last_mut().unwrap() = changed;
        assert_eq!(
            client
                .generate(&rebuild(&valid, exchanges), &settings, two.as_ref())
                .unwrap_err()
                .kind(),
            ModelErrorKind::ContinuationMismatch
        );
        let mut snapshot = s.snapshot();
        snapshot.turns[0].steps[0].executions[1].resolution = Some(EffectResolution::new(
            EffectDisposition::Completed,
            ResolutionSource::UserAttestation,
            text("changed proof"),
        ));
        let changed = Session::restore(snapshot).unwrap();
        let mut exchanges = valid.exchanges().to_vec();
        exchanges[0] = changed.closed_context_exchanges().unwrap()[0].clone();
        assert_eq!(
            client
                .generate(&rebuild(&valid, exchanges), &settings, two.as_ref())
                .unwrap_err()
                .kind(),
            ModelErrorKind::ContinuationMismatch
        );
        assert_eq!(server.requests().len(), 2);
        client.generate(&valid, &settings, two.as_ref()).unwrap();
    }
}

#[test]
fn reused_native_call_ids_are_ambiguous_across_generations() {
    for p in [Provider::OpenAI, Provider::Anthropic] {
        let server = Server::new(vec![p.tool(1).into(), p.tool(1).into()]);
        let client = p.client(&server);
        let mut s = session();
        let (output, token) = complete(client.generate(&context(&s), &p.settings(), None).unwrap());
        accept(&mut s, output);
        assert_eq!(
            client
                .generate(&context(&s), &p.settings(), token.as_ref())
                .unwrap_err()
                .kind(),
            ModelErrorKind::MalformedResponse
        );
    }
}

#[test]
fn instructions_catalog_effort_and_budget_can_change_with_the_same_token() {
    for p in PROVIDERS {
        let first = p.tool(1);
        let server = Server::new(vec![first.clone().into(), p.answer("next").into()]);
        let client = p.client(&server);
        let mut s = session();
        let (output, token) = complete(client.generate(&context(&s), &p.settings(), None).unwrap());
        accept(&mut s, output);
        let changed_tool = ToolDefinition::new(
            ToolName::new("different").unwrap(),
            text("changed catalog"),
            InputSchema::new(
                json!({"type":"object","properties":{"x":{"type":["number","null"]}}}),
            )
            .unwrap(),
        );
        let changed = Context::new(
            text("changed instructions"),
            s.context_exchanges().unwrap(),
            vec![changed_tool],
        )
        .unwrap();
        let mut settings = p.settings();
        settings.effort = Effort::Low;
        settings.max_output_tokens = std::num::NonZeroU32::new(4097).unwrap();
        let config = cola::adapters::models::ClientConfig::new(
            &format!("{}/", server.url),
            "rotated-key",
            std::time::Duration::from_secs(2),
            std::num::NonZeroUsize::new(1_000_000).unwrap(),
        )
        .unwrap();
        p.configured(config)
            .generate(&changed, &settings, token.as_ref())
            .unwrap();
        let request = server.request(1);
        assert!(request.headers.contains("rotated-key"));
        assert!(request.body.to_string().contains("changed instructions"));
        assert!(request.body.to_string().contains("changed catalog"));
        assert_eq!(
            p.request_native(&request.body),
            p.native(&first).as_array().unwrap().clone()
        );
        match p {
            Provider::OpenAI => {
                assert_eq!(request.body["max_output_tokens"], 4097);
                assert_eq!(request.body["reasoning"]["effort"], "low");
                assert_eq!(request.body["tools"][0]["strict"], false);
            }
            Provider::Anthropic => {
                assert_eq!(request.body["max_tokens"], 4097);
                assert_eq!(request.body["output_config"]["effort"], "low");
                assert!(request.body.get("thinking").is_none());
            }
        }
    }
}

#[test]
fn independent_requests_may_repeat_native_ids_with_fresh_domain_ids() {
    for p in PROVIDERS {
        let server = Server::new(vec![p.tool(1).into(), p.tool(1).into()]);
        let client = p.client(&server);
        let (one, _) = complete(
            client
                .generate(&context(&session()), &p.settings(), None)
                .unwrap(),
        );
        let (two, _) = complete(
            client
                .generate(&context(&session()), &p.settings(), None)
                .unwrap(),
        );
        assert_ne!(
            one.calls().unwrap().as_slice()[0].id(),
            two.calls().unwrap().as_slice()[0].id()
        );
    }
}

#[test]
fn closed_tool_history_without_a_token_is_rejected_before_http() {
    for p in PROVIDERS {
        let server = Server::new(vec![p.tool(1).into(), p.answer("done").into()]);
        let client = p.client(&server);
        let mut s = session();
        let (output, token) = complete(client.generate(&context(&s), &p.settings(), None).unwrap());
        accept(&mut s, output);
        let (output, _) = complete(
            client
                .generate(&context(&s), &p.settings(), token.as_ref())
                .unwrap(),
        );
        accept(&mut s, output);
        s.start_turn(user("next")).unwrap();
        assert_eq!(
            client
                .generate(&context(&s), &p.settings(), None)
                .unwrap_err()
                .kind(),
            ModelErrorKind::MissingContinuation
        );
        assert_eq!(server.requests().len(), 2);
    }
}

#[test]
fn cold_baseline_can_include_historical_plain_answers() {
    for p in PROVIDERS {
        let mut s = session();
        accept(&mut s, AssistantMessage::answer("plain history").unwrap());
        s.start_turn(user("next")).unwrap();
        let server = Server::new(vec![p.answer("done").into()]);
        p.client(&server)
            .generate(&context(&s), &p.settings(), None)
            .unwrap();
        assert!(server.request(0).body.to_string().contains("plain history"));
    }
}
