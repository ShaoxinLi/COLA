use super::{model_support::*, session_store::faults};
use cola::{adapters::stores::JsonlSessionStore, app::*, domain::*};
use std::sync::{
    Arc,
    atomic::{AtomicUsize, Ordering},
};

#[test]
fn every_checkpoint_failure_stops_external_io_even_after_publication() {
    for provider in PROVIDERS {
        for published in [false, true] {
            for revision in 1..=12 {
                let server = Server::new(vec![
                    provider.tool(1).into(),
                    decision(provider, true).into(),
                    provider.answer("done").into(),
                ]);
                let root = tempfile::tempdir().unwrap();
                let targets = Arc::new(AtomicUsize::new(0));
                let mut svc = service_with_store(
                    provider,
                    &server,
                    faults(root.path(), revision, published),
                    targets.clone(),
                );
                let id = svc.new_session().unwrap().id().clone();
                let error = svc.chat(&id, user("ask")).unwrap_err();
                assert!(
                    matches!(error.kind(), AgentErrorKind::Store { .. }),
                    "revision {revision}: {error:?}"
                );
                assert_eq!(
                    server.requests().len(),
                    if revision <= 3 {
                        0
                    } else if revision <= 7 {
                        1
                    } else if revision <= 11 {
                        2
                    } else {
                        3
                    },
                    "revision {revision}"
                );
                assert_eq!(targets.load(Ordering::SeqCst), usize::from(revision >= 9));
                let doc = JsonlSessionStore::new(root.path()).inspect(&id).unwrap();
                assert_eq!(
                    doc.revision(),
                    if published { revision } else { revision - 1 }
                );
            }
        }
    }
}
#[test]
fn accepted_review_is_committed_before_target_and_real_denial_is_not_a_block() {
    for provider in PROVIDERS {
        let server = Server::new(vec![
            provider.tool(1).into(),
            decision(provider, false).into(),
            provider.answer("denied").into(),
        ]);
        let root = tempfile::tempdir().unwrap();
        let targets = Arc::new(AtomicUsize::new(0));
        let mut svc = service_with_store(
            provider,
            &server,
            JsonlSessionStore::new(root.path()),
            targets.clone(),
        );
        let id = svc.new_session().unwrap().id().clone();
        svc.chat(&id, user("ask")).unwrap();
        let doc = svc.inspect_session(&id).unwrap();
        assert_eq!(doc.review_block(), None);
        assert_eq!(targets.load(Ordering::SeqCst), 0);
        assert!(matches!(
            doc.invocations()[1].state,
            InvocationState::Finished {
                outcome: RecordedOutcome::Review { allow: false, .. },
                ..
            }
        ));
        assert!(server.request(2).body.to_string().contains("Not executed"));
    }
}
struct BadPolicy;
impl ContextPolicy for BadPolicy {
    fn select(&self, _: &Session) -> ContextSelection {
        ContextSelection {
            closed_turns: vec![],
            mandatory_turns: vec![],
            max_request_bytes: 65536,
        }
    }
}
#[test]
fn replacement_policy_cannot_unpin_unknown_effects() {
    let p = Provider::OpenAI;
    let server = Server::new(vec![p.tool(1).into(), decision(p, true).into()]);
    let root = tempfile::tempdir().unwrap();
    let mut svc = service_with_store(
        p,
        &server,
        faults(root.path(), 9, false),
        Arc::new(AtomicUsize::new(0)),
    );
    let id = svc.new_session().unwrap().id().clone();
    svc.chat(&id, user("unresolved")).unwrap_err();
    drop(svc);
    let mut svc = service(p, &server, root.path()).with_context_policy(BadPolicy);
    let error = svc
        .chat(&id, user("must preserve unknown effect"))
        .unwrap_err();
    assert!(
        matches!(error.kind(),AgentErrorKind::Model{source,..} if source.kind()==ModelErrorKind::InvalidSelection)
    );
    assert_eq!(server.requests().len(), 2);
    assert!(
        svc.inspect_session(&id)
            .unwrap()
            .session()
            .active_turn()
            .is_none()
    );
}
struct SelectTurn(TurnId);
impl ContextPolicy for SelectTurn {
    fn select(&self, _: &Session) -> ContextSelection {
        ContextSelection {
            closed_turns: vec![self.0],
            mandatory_turns: vec![],
            max_request_bytes: 65536,
        }
    }
}
#[test]
fn policy_rejects_current_future_and_unknown_turns_before_network_io() {
    for p in PROVIDERS {
        for invalid_id in [2, 3, u64::MAX] {
            let server = Server::new(vec![
                p.answer("closed").into(),
                p.answer("unexpected").into(),
            ]);
            let root = tempfile::tempdir().unwrap();
            let mut svc = service(p, &server, root.path());
            let id = svc.new_session().unwrap().id().clone();
            svc.chat(&id, user("first")).unwrap();
            let mut svc = svc.with_context_policy(SelectTurn(TurnId::new(invalid_id).unwrap()));
            let error = svc.chat(&id, user("invalid selection")).unwrap_err();
            assert!(
                matches!(error.kind(), AgentErrorKind::Model { source, .. }
                    if source.kind() == ModelErrorKind::InvalidSelection),
                "policy turn {invalid_id}: {error:?}"
            );
            assert_eq!(server.requests().len(), 1);
            drop(svc);
            let doc = JsonlSessionStore::new(root.path()).inspect(&id).unwrap();
            assert!(matches!(
                last_outcome(&doc),
                RecordedOutcome::PreparationFailed(error)
                    if error.kind() == ModelErrorKind::InvalidSelection
            ));
            assert!(doc.session().active_turn().is_none());
            assert!(matches!(
                doc.session().turns()[1].outcome(),
                TurnOutcome::Failed(_)
            ));
        }
    }
}
struct CurrentTurnOnly;
impl Assistant for CurrentTurnOnly {
    fn context(&self, s: &Session) -> Result<Context<AssistantMessage>, ContextError> {
        Context::new(
            text("Be useful"),
            s.context_exchanges()?
                .into_iter()
                .filter(|e| e.outcome().is_none())
                .collect(),
            vec![],
        )
    }
}
#[test]
fn role_may_omit_known_optional_turns_selected_by_policy() {
    for p in PROVIDERS {
        let server = Server::new(vec![
            p.answer("first answer").into(),
            p.answer("second answer").into(),
        ]);
        let root = tempfile::tempdir().unwrap();
        let mut svc = AgentService::new(
            Pair {
                assistant: p.client(&server),
                reviewer: p.reviewer(&server),
            },
            JsonlSessionStore::new(root.path()),
            CurrentTurnOnly,
            Roles,
            vec![],
            AgentSettings {
                assistant_model_settings: p.settings(),
                reviewer_model_settings: p.settings(),
                max_iterations: std::num::NonZeroU32::new(8).unwrap(),
            },
        )
        .unwrap();
        let id = svc.new_session().unwrap().id().clone();
        svc.chat(&id, user("optional history")).unwrap();
        let mut svc = svc.with_context_policy(SelectTurn(TurnId::new(1).unwrap()));
        svc.chat(&id, user("current request")).unwrap();
        assert_eq!(server.requests().len(), 2);
        assert!(
            !server
                .request(1)
                .body
                .to_string()
                .contains("optional history")
        );
        assert!(!server.request(1).body.to_string().contains("first answer"));
        let doc = svc.inspect_session(&id).unwrap();
        assert!(doc.session().active_turn().is_none());
        assert!(matches!(
            doc.session().turns()[1].outcome(),
            TurnOutcome::Completed
        ));
    }
}
struct LargeOutput;
impl Tool for LargeOutput {
    fn execute(&self, _: &serde_json::Map<String, serde_json::Value>) -> ToolOutcome {
        ToolOutcome::Success("x".repeat(16000))
    }
}
#[test]
fn growing_native_chain_hits_exact_body_budget_without_truncating_results() {
    for p in PROVIDERS {
        let server = Server::new(vec![p.tool(1).into(), decision(p, true).into()]);
        let root = tempfile::tempdir().unwrap();
        let mut svc = AgentService::new(
            Pair {
                assistant: p.client(&server),
                reviewer: p.reviewer(&server),
            },
            JsonlSessionStore::new(root.path()),
            Roles,
            Roles,
            vec![(tool(), Box::new(LargeOutput))],
            AgentSettings {
                assistant_model_settings: p.settings(),
                reviewer_model_settings: p.settings(),
                max_iterations: std::num::NonZeroU32::new(8).unwrap(),
            },
        )
        .unwrap()
        .with_context_policy(RecentContextPolicy {
            optional_turns: 8,
            max_request_bytes: 8000,
        });
        let id = svc.new_session().unwrap().id().clone();
        let error = svc.chat(&id, user("large output")).unwrap_err();
        assert!(
            matches!(error.kind(),AgentErrorKind::Model{source,..} if source.kind()==ModelErrorKind::ContextSelectionTooLarge)
        );
        assert_eq!(server.requests().len(), 2);
        let doc = svc.inspect_session(&id).unwrap();
        assert!(
            matches!(doc.session().turns()[0].steps()[0].executions().next().unwrap().state(),ExecutionState::Returned(ToolOutcome::Success(s)) if s.len()==16000)
        );
        assert!(matches!(
            last_outcome(&doc),
            RecordedOutcome::PreparationFailed(_)
        ));
    }
}
struct ChangedInstructions;
impl Assistant for ChangedInstructions {
    fn context(&self, s: &Session) -> Result<Context<AssistantMessage>, ContextError> {
        Context::new(
            text(if s.active_turn().unwrap().steps().is_empty() {
                "initial"
            } else {
                "edited"
            }),
            s.context_exchanges()?,
            vec![tool()],
        )
    }
}
#[test]
fn same_turn_instruction_edits_fail_before_next_network_request() {
    let p = Provider::Anthropic;
    let server = Server::new(vec![p.tool(1).into(), decision(p, true).into()]);
    let root = tempfile::tempdir().unwrap();
    let mut svc = AgentService::new(
        Pair {
            assistant: p.client(&server),
            reviewer: p.reviewer(&server),
        },
        JsonlSessionStore::new(root.path()),
        ChangedInstructions,
        Roles,
        vec![(tool(), Box::new(Target(Arc::new(AtomicUsize::new(0)))))],
        AgentSettings {
            assistant_model_settings: p.settings(),
            reviewer_model_settings: p.settings(),
            max_iterations: std::num::NonZeroU32::new(8).unwrap(),
        },
    )
    .unwrap();
    let id = svc.new_session().unwrap().id().clone();
    let error = svc.chat(&id, user("ask")).unwrap_err();
    assert!(
        matches!(error.kind(),AgentErrorKind::Model{source,..} if source.kind()==ModelErrorKind::NativeHistoryIncompatible)
    );
    assert_eq!(server.requests().len(), 2);
}

struct BrokenContext {
    review_only: bool,
}
impl Assistant for BrokenContext {
    fn context(&self, session: &Session) -> Result<Context<AssistantMessage>, ContextError> {
        if self.review_only {
            Assistant::context(&Roles, session)
        } else {
            Err(ContextError::MissingUser)
        }
    }
}
impl ToolCallReviewer for BrokenContext {
    fn context(&self, _: &Session, _: &ToolCall) -> Result<Context<Decision>, ContextError> {
        Err(ContextError::UnresolvedToolBatch)
    }
}
#[test]
fn role_context_errors_retain_their_domain_source_and_review_block() {
    for review_only in [false, true] {
        let p = Provider::OpenAI;
        let server = Server::new(vec![p.tool(1).into()]);
        let root = tempfile::tempdir().unwrap();
        let mut svc = AgentService::new(
            Pair {
                assistant: p.client(&server),
                reviewer: p.reviewer(&server),
            },
            JsonlSessionStore::new(root.path()),
            BrokenContext { review_only },
            BrokenContext { review_only },
            vec![(tool(), Box::new(Target(Arc::new(AtomicUsize::new(0)))))],
            AgentSettings {
                assistant_model_settings: p.settings(),
                reviewer_model_settings: p.settings(),
                max_iterations: std::num::NonZeroU32::new(8).unwrap(),
            },
        )
        .unwrap();
        let id = svc.new_session().unwrap().id().clone();
        let error = svc.chat(&id, user("ask")).unwrap_err();
        let kind = if review_only {
            let AgentErrorKind::ReviewFailure {
                invocation: 2,
                source,
            } = error.kind()
            else {
                panic!("{error:?}")
            };
            assert_eq!(svc.inspect_session(&id).unwrap().review_block(), Some(2));
            source.as_ref()
        } else {
            error.kind()
        };
        let AgentErrorKind::Context { role, source } = kind else {
            panic!("{kind:?}")
        };
        assert_eq!(
            *role,
            if review_only {
                ModelRole::Reviewer
            } else {
                ModelRole::Assistant
            }
        );
        assert_eq!(
            *source,
            if review_only {
                ContextError::UnresolvedToolBatch
            } else {
                ContextError::MissingUser
            }
        );
        assert_eq!(server.requests().len(), usize::from(review_only));
    }
}

#[test]
fn every_resolution_is_a_labelled_closed_turn_fact_without_native_return() {
    use serde_json::{Value, json};
    for provider in PROVIDERS {
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
                let server = Server::new(vec![
                    provider.tool(1).into(),
                    decision(provider, true).into(),
                    provider.answer("recovered").into(),
                ]);
                let root = tempfile::tempdir().unwrap();
                let targets = Arc::new(AtomicUsize::new(0));
                let mut svc = service_with_store(
                    provider,
                    &server,
                    faults(root.path(), 8, false),
                    targets.clone(),
                );
                let id = svc.new_session().unwrap().id().clone();
                assert!(matches!(
                    svc.chat(&id, user("ask")).unwrap_err().kind(),
                    AgentErrorKind::Store { .. }
                ));
                let doc = svc.inspect_session(&id).unwrap();
                assert_eq!(doc.revision(), 7);
                assert!(matches!(
                    doc.invocations()[1].state,
                    InvocationState::Prepared { .. }
                ));
                assert_eq!(server.requests().len(), 2);
                let address = doc
                    .session()
                    .call_address(ToolCallId::new("native-1").unwrap())
                    .unwrap();
                drop(svc);
                let mut reopened = service(provider, &server, root.path());
                reopened
                    .acknowledge_review_failure(&id, 2, text("review incident inspected"), None)
                    .unwrap();
                let recovered = reopened.inspect_session(&id).unwrap();
                assert_eq!(
                    recovered.session().turns()[0].outcome(),
                    &TurnOutcome::Interrupted(StopReason::ProcessLost)
                );
                reopened
                    .resolve_effect(
                        address,
                        EffectResolution::new(disposition, source, text("resolution evidence")),
                    )
                    .unwrap();
                reopened.chat(&id, user("continue from facts")).unwrap();
                assert_eq!(targets.load(Ordering::SeqCst), 0);
                assert_eq!(server.requests().len(), 3);
                let request = server.request(2).body;
                assert!(!request.to_string().contains("function_call_output"));
                assert!(!request.to_string().contains("tool_result"));
                let events = match provider {
                    Provider::OpenAI => &request["input"],
                    Provider::Anthropic => &request["messages"],
                };
                let mut facts = vec![];
                for event in events.as_array().unwrap() {
                    assert_eq!(event["role"], "user");
                    let texts: Vec<&str> = match &event["content"] {
                        Value::String(s) => vec![s],
                        Value::Array(parts) => {
                            parts.iter().filter_map(|p| p["text"].as_str()).collect()
                        }
                        _ => panic!("unexpected event"),
                    };
                    for content in texts {
                        if let Some((quoted, raw)) =
                            content.split_once("Execution facts (not new tool results): ")
                        {
                            assert!(quoted.starts_with("Quoted closed-turn history."));
                            facts.extend(serde_json::from_str::<Vec<Value>>(raw).unwrap());
                        }
                    }
                }
                assert!(facts.contains(&json!({"fact":"turn_closed","turn":1,"outcome":{"status":"interrupted","reason":"process_lost"}})), "request {request}");
                let effects: Vec<_> = facts
                    .iter()
                    .filter(|f| f["fact"] == "unknown_effect")
                    .collect();
                assert_eq!(
                    effects,
                    vec![
                        &json!({"fact":"unknown_effect","session":id.as_str(),"turn":1,"step":1,"call_id":"native-1","name":"lookup","arguments":{"n":1},"resolution":{"disposition":label,"source":provenance,"evidence":"resolution evidence"}})
                    ]
                );
            }
        }
    }
}
