use super::{protocols::interrupted_history, support::*};
use cola::{adapters::stores::JsonlSessionStore, app::*, domain::*};
use serde_json::json;

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
                Provider::Google => json!({"candidates":[{"finishReason":"MAX_TOKENS"}]}),
            };
            let refused = match p {
                Provider::OpenAI => {
                    json!({"status":"completed","output":[{"type":"message","role":"assistant","content":[{"type":"refusal","refusal":"no"}]}]})
                }
                Provider::Anthropic => json!({"stop_reason":"refusal"}),
                Provider::Google => json!({"promptFeedback":{"blockReason":"SAFETY"}}),
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
                Generation::Refused { .. } | Generation::Blocked { .. }
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
        let first = p.tool(1);
        let second = p.tool(2);
        let terminal = p.answer("first turn done");
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
            headers: vec![],
        };
        let broken = Reply {
            status: 200,
            body: "{broken".into(),
            delay: std::time::Duration::ZERO,
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
fn settings_endpoint_provider_instructions_catalog_and_receipt_binding_fail_without_network() {
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
        let mut invalid = vec![
            Context::new(
                text("different instructions"),
                valid.exchanges().to_vec(),
                valid.tools().to_vec(),
            )
            .unwrap(),
            Context::new(
                valid.instructions().clone(),
                valid.exchanges().to_vec(),
                vec![],
            )
            .unwrap(),
        ];
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
        changed.effort = Effort::Low;
        assert_eq!(
            client
                .generate(&valid, &changed, Some(&token))
                .unwrap_err()
                .kind(),
            ModelErrorKind::ContinuationMismatch
        );
        changed = settings.clone();
        changed.max_output_tokens = std::num::NonZeroU32::new(1025).unwrap();
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
fn catalog_order_descriptions_schemas_model_and_new_turn_identity_are_bound() {
    for p in PROVIDERS {
        let server = Server::new(vec![p.answer("done").into(), p.answer("next").into()]);
        let client = p.client(&server);
        let mut s = session();
        let second = ToolDefinition::new(
            ToolName::new("other").unwrap(),
            text("other description"),
            InputSchema::new(json!({"type":"object"})).unwrap(),
        );
        let tools = vec![tool(), second.clone()];
        let before = Context::new(
            text("Be useful"),
            s.context_exchanges().unwrap(),
            tools.clone(),
        )
        .unwrap();
        let (output, token) = complete(client.generate(&before, &p.settings(), None).unwrap());
        accept(&mut s, output);
        s.start_turn(user("continue")).unwrap();
        let valid = Context::new(
            text("Be useful"),
            s.context_exchanges().unwrap(),
            tools.clone(),
        )
        .unwrap();
        for altered in [
            vec![second.clone(), tool()],
            vec![
                ToolDefinition::new(
                    tool().name().clone(),
                    text("edited"),
                    tool().input_schema().clone(),
                ),
                second.clone(),
            ],
            vec![
                ToolDefinition::new(
                    tool().name().clone(),
                    tool().description().clone(),
                    InputSchema::new(json!({"type":"object"})).unwrap(),
                ),
                second.clone(),
            ],
        ] {
            let ctx = Context::new(text("Be useful"), valid.exchanges().to_vec(), altered).unwrap();
            assert_eq!(
                client
                    .generate(&ctx, &p.settings(), token.as_ref())
                    .unwrap_err()
                    .kind(),
                ModelErrorKind::ContinuationMismatch
            );
        }
        let mut exchanges = valid.exchanges().to_vec();
        exchanges[1] = ContextExchange::new(
            TurnId::new(3).unwrap(),
            exchanges[1].messages().to_vec(),
            None,
        )
        .unwrap();
        assert_eq!(
            client
                .generate(&rebuild(&valid, exchanges), &p.settings(), token.as_ref())
                .unwrap_err()
                .kind(),
            ModelErrorKind::ContinuationMismatch
        );
        let mut settings = p.settings();
        settings.model_id = ModelId::new("different-model").unwrap();
        assert_eq!(
            client
                .generate(&valid, &settings, token.as_ref())
                .unwrap_err()
                .kind(),
            ModelErrorKind::UnsupportedModel
        );
        let equivalent = cola::adapters::models::ClientConfig::new(
            &format!("{}/", server.url),
            "rotated-key",
            std::time::Duration::from_secs(2),
            std::num::NonZeroUsize::new(1_000_000).unwrap(),
        )
        .unwrap();
        assert_eq!(server.requests().len(), 1);
        p.configured(equivalent)
            .generate(&valid, &p.settings(), token.as_ref())
            .unwrap();
        assert_eq!(server.requests().len(), 2);
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
