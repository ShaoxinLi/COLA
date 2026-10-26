use super::support::*;
use cola::{app::*, domain::*};
use serde_json::json;

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
fn every_supported_effort_level_has_its_exact_native_spelling() {
    for p in PROVIDERS {
        for (effort, label) in [
            (Effort::Low, "low"),
            (Effort::Medium, "medium"),
            (Effort::High, "high"),
            (Effort::Xhigh, "xhigh"),
            (Effort::Max, "max"),
        ] {
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
