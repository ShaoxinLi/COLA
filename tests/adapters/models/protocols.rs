use super::support::*;
use cola::{app::*, domain::*};
use serde_json::json;

#[test]
fn google_omitted_args_are_empty_without_rewriting_signed_native_parts() {
    let p = Provider::Google;
    let reply = json!({"candidates":[{"finishReason":"STOP","content":{"role":"model","parts":[{"functionCall":{"name":"ping"},"thoughtSignature":"signed-no-args"}]}}]});
    let server = Server::new(vec![reply.clone().into(), p.answer("done").into()]);
    let client = p.client(&server);
    let input = |s: &Session| {
        Context::<AssistantMessage>::new(
            text("Be useful"),
            s.context_exchanges().unwrap(),
            vec![ToolDefinition::new(
                ToolName::new("ping").unwrap(),
                text("Check availability"),
                InputSchema::new(
                    json!({"type":"object","properties":{},"additionalProperties":false}),
                )
                .unwrap(),
            )],
        )
        .unwrap()
    };
    let mut s = session();
    let (output, token) = complete(client.generate(&input(&s), &p.settings(), None).unwrap());
    let call = &output.calls().unwrap().as_slice()[0];
    assert_eq!(call.name().as_str(), "ping");
    assert!(call.arguments().is_empty());
    accept(&mut s, output);
    client
        .generate(&input(&s), &p.settings(), token.as_ref())
        .unwrap();
    let request = server.request(1);
    assert_eq!(
        p.request_native(&request.body),
        p.native(&reply).as_array().unwrap().clone()
    );
    assert_eq!(
        request.body["contents"][2]["parts"][0]["functionResponse"]["name"],
        "ping"
    );
    assert!(
        request.body["contents"][1]["parts"][0]["functionCall"]
            .get("args")
            .is_none()
    );
    for invalid in [json!(null), json!("text"), json!([]), json!(true)] {
        let mut invalid_reply = reply.clone();
        invalid_reply["candidates"][0]["content"]["parts"][0]["functionCall"]["args"] = invalid;
        let server = Server::new(vec![invalid_reply.into()]);
        assert_eq!(
            p.client(&server)
                .generate(&input(&session()), &p.settings(), None)
                .unwrap_err()
                .kind(),
            ModelErrorKind::MalformedResponse
        );
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
                    assert!(headers.contains("anthropic-version: 2023-06-01"));
                    assert_eq!(r.body["max_tokens"], 1024);
                    assert_eq!(r.body["thinking"], json!({"type":"adaptive"}));
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
                Provider::Google => {
                    assert_eq!(
                        r.line,
                        "POST /custom/v1/models/gemini-3.8-flash:generateContent HTTP/1.1"
                    );
                    assert!(headers.contains("x-goog-api-key: secret-key"));
                    assert_eq!(r.body["generationConfig"]["maxOutputTokens"], 1024);
                    assert_eq!(
                        r.body["tools"][0]["functionDeclarations"][0]["parametersJsonSchema"],
                        json!({"type":"object","properties":{"n":{"type":"integer","minimum":0}},"required":["n"],"additionalProperties":false})
                    );
                    assert_eq!(
                        r.body
                            .pointer("/generationConfig/thinkingConfig/thinkingLevel")
                            .cloned(),
                        if effort == Effort::Default {
                            None
                        } else {
                            Some(json!("HIGH"))
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
fn exact_profiles_reject_unsupported_before_network() {
    for p in PROVIDERS {
        let server = Server::new(vec![]);
        let client = p.client(&server);
        let ctx = context(&session());
        let mut settings = p.settings();
        settings.model_id = ModelId::new(format!("{}-latest", p.model())).unwrap();
        assert_eq!(
            client.generate(&ctx, &settings, None).unwrap_err().kind(),
            ModelErrorKind::UnsupportedModel
        );
        settings = p.settings();
        settings.effort = Effort::Off;
        assert_eq!(
            client.generate(&ctx, &settings, None).unwrap_err().kind(),
            ModelErrorKind::UnsupportedEffort
        );
        if matches!(p, Provider::Google) {
            for effort in [Effort::Xhigh, Effort::Max] {
                settings.effort = effort;
                assert_eq!(
                    client.generate(&ctx, &settings, None).unwrap_err().kind(),
                    ModelErrorKind::UnsupportedEffort
                );
            }
        }
        assert!(server.requests().is_empty());
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
            Provider::Google => {
                assert_eq!(
                    r.body["generationConfig"]["responseMimeType"],
                    "application/json"
                );
                assert_eq!(r.body["generationConfig"]["responseJsonSchema"], expected);
            }
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
fn portable_schemas_preserve_constraints_or_fail_before_send() {
    for p in PROVIDERS {
        for schema in [
            json!({"type":"object","properties":{"path":{"type":"string","minLength":1}}}),
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
                Provider::Google => {
                    &r.body["tools"][0]["functionDeclarations"][0]["parametersJsonSchema"]
                }
            };
            assert_eq!(actual, &schema);
        }
        let server = Server::new(vec![]);
        let ctx = Context::new(
            text("instructions"),
            session().context_exchanges().unwrap(),
            vec![ToolDefinition::new(
                ToolName::new("lookup").unwrap(),
                text("tool"),
                InputSchema::new(json!({"type":"object","$ref":"https://secret.example/schema"}))
                    .unwrap(),
            )],
        )
        .unwrap();
        assert_eq!(
            p.client(&server)
                .generate(&ctx, &p.settings(), None)
                .unwrap_err()
                .kind(),
            ModelErrorKind::UnsupportedInput
        );
        assert!(server.requests().is_empty());
    }
}

pub fn interrupted_history() -> Session {
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
fn google_same_name_parallel_calls_preserve_order_and_optional_native_ids() {
    let p = Provider::Google;
    let response = json!({"candidates":[{"finishReason":"STOP","content":{"role":"model","parts":[{"functionCall":{"name":"lookup","args":{"n":1}},"thoughtSignature":"sig-first"},{"functionCall":{"id":"native-second","name":"lookup","args":{"n":2}},"thoughtSignature":"sig-second"}]}}]});
    let server = Server::new(vec![response.clone().into(), p.answer("done").into()]);
    let client = p.client(&server);
    let mut s = session();
    let (output, token) = complete(client.generate(&context(&s), &p.settings(), None).unwrap());
    assert_eq!(output.calls().unwrap().as_slice().len(), 2);
    let ids: Vec<_> = output
        .calls()
        .unwrap()
        .as_slice()
        .iter()
        .map(|c| c.id().clone())
        .collect();
    assert_ne!(ids[0], ids[1]);
    accept(&mut s, output);
    client
        .generate(&context(&s), &p.settings(), token.as_ref())
        .unwrap();
    let request = server.request(1).body;
    assert_eq!(
        request["contents"][1]["parts"],
        response["candidates"][0]["content"]["parts"]
    );
    assert_eq!(
        request["contents"][2]["parts"],
        json!([{"functionResponse":{"name":"lookup","response":{"success":true,"output":""}}},{"functionResponse":{"id":"native-second","name":"lookup","response":{"success":false,"error":"tool failed"}}}])
    );
    assert!(ids.iter().all(|id| !id.as_str().contains("native-second")));
}
#[test]
fn every_supported_effort_level_has_its_exact_native_spelling() {
    for p in PROVIDERS {
        for (effort, label) in [
            (Effort::Low, "low"),
            (Effort::Medium, "medium"),
            (Effort::High, "high"),
            (Effort::Xhigh, "xhigh"),
            (Effort::Max, "max"),
        ] {
            if matches!(p, Provider::Google) && matches!(effort, Effort::Xhigh | Effort::Max) {
                continue;
            }
            let server = Server::new(vec![p.answer("ok").into()]);
            let mut settings = p.settings();
            settings.effort = effort;
            p.client(&server)
                .generate(&context(&session()), &settings, None)
                .unwrap();
            let body = server.request(0).body;
            match p {
                Provider::OpenAI => assert_eq!(body["reasoning"]["effort"], label),
                Provider::Anthropic => assert_eq!(body["output_config"]["effort"], label),
                Provider::Google => assert_eq!(
                    body["generationConfig"]["thinkingConfig"]["thinkingLevel"],
                    label.to_ascii_uppercase()
                ),
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
        (
            Provider::Google,
            json!({"promptFeedback":{"blockReason":"SAFETY"}}),
        ),
    ] {
        let server = Server::new(vec![response.into()]);
        let result = p
            .reviewer(&server)
            .generate(&context::<Decision>(&session()), &p.settings(), None)
            .unwrap();
        assert!(matches!(
            result,
            Generation::Refused { .. } | Generation::Blocked { .. }
        ));
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
            _ => unreachable!(),
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
            _ => unreachable!(),
        }
    }
}
