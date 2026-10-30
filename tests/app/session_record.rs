use super::{model_support::*, session_store::faults};
use cola::{adapters::stores::JsonlSessionStore, app::*, domain::*};
use std::sync::{
    Arc,
    atomic::{AtomicUsize, Ordering},
};

#[test]
fn reviewer_failure_is_durable_and_acknowledgement_is_idempotent_without_tool_io() {
    for provider in PROVIDERS {
        let mut failed: Reply = provider.answer("invalid decision").into();
        failed.status = 503;
        let server = Server::new(vec![
            provider.tool(1).into(),
            failed,
            provider.answer("after acknowledgement").into(),
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
        let error = svc.chat(&id, user("ask")).unwrap_err();
        let AgentErrorKind::ReviewFailure { invocation, .. } = error.kind() else {
            panic!("{error:?}")
        };
        let invocation = *invocation;
        let before_retry = svc.inspect_session(&id).unwrap();
        assert!(matches!(
            svc.chat(&id, user("retry live service"))
                .unwrap_err()
                .kind(),
            AgentErrorKind::ReviewBlocked { .. }
        ));
        let address = before_retry
            .session()
            .call_address(ToolCallId::new("native-1").unwrap())
            .unwrap();
        assert!(matches!(
            svc.resolve_effect(
                address,
                EffectResolution::new(
                    EffectDisposition::NotPerformed,
                    ResolutionSource::UserAttestation,
                    text("not executed")
                )
            )
            .unwrap_err()
            .kind(),
            AgentErrorKind::RejectedOutput(SessionError::EffectNotUnknown)
        ));
        assert_eq!(svc.inspect_session(&id).unwrap(), before_retry);
        drop(svc);
        let mut reopened = service(provider, &server, root.path());
        assert!(matches!(
            reopened.chat(&id, user("blocked")).unwrap_err().kind(),
            AgentErrorKind::ReviewBlocked { .. }
        ));
        assert_eq!(server.requests().len(), 2);
        assert_eq!(reopened.inspect_session(&id).unwrap(), before_retry);
        reopened
            .acknowledge_review_failure(
                &id,
                invocation,
                text("operator inspected the incident"),
                None,
            )
            .unwrap();
        let doc = reopened.inspect_session(&id).unwrap();
        let revision = doc.revision();
        assert_eq!(doc.review_block(), None);
        assert_eq!(doc.acknowledgements().len(), 1);
        let acknowledgement = &doc.acknowledgements()[0];
        assert_eq!(acknowledgement.invocation, invocation);
        assert_eq!(acknowledgement.evidence, "operator inspected the incident");
        assert_eq!(acknowledgement.actor, None);
        assert_eq!(
            doc.session().turns()[0].outcome(),
            &TurnOutcome::Failed(TurnFailure::ModelFailure(text(
                "Model request failed (ProviderFailure)"
            )))
        );
        assert_eq!(
            doc.session().turns()[0].steps()[0]
                .executions()
                .next()
                .unwrap()
                .state(),
            &ExecutionState::Started
        );
        reopened
            .acknowledge_review_failure(
                &id,
                invocation,
                text("operator inspected the incident"),
                None,
            )
            .unwrap();
        assert_eq!(reopened.inspect_session(&id).unwrap().revision(), revision);
        for (evidence, actor) in [
            (text("different"), None),
            (
                text("operator inspected the incident"),
                Some(text("another actor")),
            ),
        ] {
            assert!(matches!(
                reopened
                    .acknowledge_review_failure(&id, invocation, evidence, actor)
                    .unwrap_err()
                    .kind(),
                AgentErrorKind::Record(RecordError::ConflictingAcknowledgement)
            ));
        }
        assert!(matches!(
            reopened
                .acknowledge_review_failure(&id, 1, text("assistant has no review failure"), None)
                .unwrap_err()
                .kind(),
            AgentErrorKind::Record(RecordError::NoReviewFailure)
        ));
        assert_eq!(reopened.inspect_session(&id).unwrap(), doc);
        assert_eq!(targets.load(Ordering::SeqCst), 0);
        assert_eq!(server.requests().len(), 2);
        reopened.chat(&id, user("continue")).unwrap();
        assert!(
            server
                .request(2)
                .body
                .to_string()
                .contains("unknown_effect")
        );
    }
}

#[test]
fn acknowledgement_preserves_original_finished_failure_when_turn_is_still_active() {
    let p = Provider::OpenAI;
    let server = Server::new(vec![p.tool(1).into(), p.answer("bad review").into()]);
    let root = tempfile::tempdir().unwrap();
    let mut svc = service(p, &server, root.path());
    let id = svc.new_session().unwrap().id().clone();
    svc.chat(&id, user("ask")).unwrap_err();
    drop(svc);
    let mut svc = service(p, &server, root.path());
    svc.acknowledge_review_failure(&id, 2, text("accepted failure"), None)
        .unwrap();
    let doc = svc.inspect_session(&id).unwrap();
    assert!(matches!(
        doc.session().turns()[0].outcome(),
        TurnOutcome::Failed(_)
    ));
    assert_eq!(server.requests().len(), 2);
}

#[test]
fn planned_only_recovery_is_not_sent_and_prepared_review_can_be_acknowledged_directly() {
    for revision in [6, 7] {
        let p = Provider::OpenAI;
        let server = Server::new(vec![p.tool(1).into(), p.answer("new turn").into()]);
        let root = tempfile::tempdir().unwrap();
        let mut svc = service_with_store(
            p,
            &server,
            faults(root.path(), revision, true),
            Arc::new(AtomicUsize::new(0)),
        );
        let id = svc.new_session().unwrap().id().clone();
        svc.chat(&id, user("ask")).unwrap_err();
        drop(svc);
        let original = JsonlSessionStore::new(root.path()).inspect(&id).unwrap();
        assert!(original.session().active_turn().is_some());
        assert_eq!(original.revision(), revision);
        let mut svc = service(p, &server, root.path());
        if revision == 7 {
            svc.acknowledge_review_failure(&id, 2, text("unknown review inspected"), None)
                .unwrap();
        } else {
            svc.chat(&id, user("new request")).unwrap();
        }
        let doc = svc.inspect_session(&id).unwrap();
        assert!(
            matches!(&doc.invocations()[1].state,InvocationState::Interrupted{response_unknown,..} if *response_unknown==(revision==7))
        );
        assert_eq!(
            doc.session().turns()[0].outcome(),
            &TurnOutcome::Interrupted(StopReason::ProcessLost)
        );
        assert_eq!(doc.review_block(), None);
    }
}

#[test]
fn accepted_steps_keep_exact_audit_evidence_addresses_and_frozen_view_after_reopen() {
    for provider in PROVIDERS {
        let replies = [
            provider.tool(1),
            decision(provider, true),
            provider.answer("done"),
        ];
        let server = Server::new(replies.iter().cloned().map(Into::into).collect());
        let root = tempfile::tempdir().unwrap();
        let mut svc = service(provider, &server, root.path());
        let id = svc.new_session().unwrap().id().clone();
        svc.chat(&id, user("ask")).unwrap();
        let record = svc.inspect_session(&id).unwrap();
        assert!(record.session().active_turn().is_none());
        assert_eq!(record.session().turns().len(), 1);
        let turn = &record.session().turns()[0];
        assert_eq!(turn.outcome(), &TurnOutcome::Completed);
        assert_eq!(turn.steps().len(), 2);
        assert_eq!(record.invocations().len(), 3);
        assert_eq!(record.views().len(), 1);
        assert!(record.acknowledgements().is_empty());
        assert_eq!(record.review_block(), None);
        for (index, invocation) in record.invocations().iter().enumerate() {
            assert_eq!(invocation.id, index as u64 + 1);
            let InvocationState::Finished {
                request: Some(request),
                response,
                outcome,
            } = &invocation.state
            else {
                panic!("unfinished invocation: {invocation:?}");
            };
            assert_eq!(request.body.bytes, server.request(index).bytes);
            assert_eq!(
                response.response.as_ref().unwrap().bytes,
                replies[index].to_string().as_bytes()
            );
            assert_eq!(response.capture, CaptureCompleteness::Complete);
            assert_eq!(response.http_status, Some(200));
            assert_eq!(request.policy_version, 1);
            assert!(request.selection.is_empty());
            if index == 1 {
                let InvocationTarget::Reviewer { call } = &invocation.target else {
                    panic!("{invocation:?}")
                };
                assert_eq!(
                    call.address().unwrap(),
                    CallAddress {
                        session: id.clone(),
                        turn: turn.id(),
                        step: 1,
                        native: ToolCallId::new("native-1").unwrap(),
                    }
                );
                assert_eq!(
                    outcome,
                    &RecordedOutcome::Review {
                        allow: true,
                        reason: None
                    }
                );
            } else {
                assert_eq!(
                    invocation.target,
                    InvocationTarget::Assistant {
                        turn: turn.id().get(),
                        step: if index == 0 { 1 } else { 2 }
                    }
                );
                assert_eq!(outcome, &RecordedOutcome::AssistantAccepted);
            }
        }
        let view = &record.views()[0];
        let InvocationState::Finished {
            request: Some(first),
            ..
        } = &record.invocations()[0].state
        else {
            unreachable!()
        };
        assert_eq!(view.turn, turn.id().get());
        assert_eq!(view.frozen_prefix, first.body);
        assert_eq!(view.requested, record.invocations()[0].requested);
        assert_eq!(view.selection, first.selection);
        assert_eq!(view.policy_version, first.policy_version);
        assert!(matches!(
            turn.steps()[0].executions().next().unwrap().state(),
            ExecutionState::Returned(ToolOutcome::Success(_))
        ));
        drop(svc);
        assert_eq!(
            service(provider, &server, root.path())
                .inspect_session(&id)
                .unwrap(),
            record
        );
        assert_eq!(server.requests().len(), 3);
    }
}

#[test]
fn recovery_preserves_planned_and_prepared_assistant_evidence_without_inventing_steps() {
    for revision in [2, 3] {
        let provider = Provider::OpenAI;
        let server = Server::new(vec![provider.answer("new turn").into()]);
        let root = tempfile::tempdir().unwrap();
        let mut svc = service_with_store(
            provider,
            &server,
            faults(root.path(), revision, true),
            Arc::new(AtomicUsize::new(0)),
        );
        let id = svc.new_session().unwrap().id().clone();
        svc.chat(&id, user("abandoned")).unwrap_err();
        drop(svc);
        let before = JsonlSessionStore::new(root.path()).inspect(&id).unwrap();
        assert_eq!(before.revision(), revision);
        assert!(before.session().active_turn().is_some());
        assert_eq!(server.requests().len(), 0);
        let mut svc = service(provider, &server, root.path());
        assert_eq!(svc.inspect_session(&id).unwrap(), before);
        svc.chat(&id, user("continue")).unwrap();
        let record = svc.inspect_session(&id).unwrap();
        let InvocationState::Interrupted {
            request,
            response_unknown,
        } = &record.invocations()[0].state
        else {
            panic!("{:?}", record.invocations()[0])
        };
        assert_eq!(*response_unknown, revision == 3);
        let original_request = match &before.invocations()[0].state {
            InvocationState::Planned => None,
            InvocationState::Prepared(request) => Some(request.clone()),
            other => panic!("{other:?}"),
        };
        assert_eq!(request, &original_request);
        assert_eq!(
            record.session().turns()[0].outcome(),
            &TurnOutcome::Interrupted(StopReason::ProcessLost)
        );
        assert!(record.session().turns()[0].steps().is_empty());
        assert_eq!(
            record.session().turns()[1].outcome(),
            &TurnOutcome::Completed
        );
        assert_eq!(last_outcome(&record), &RecordedOutcome::AssistantAccepted);
        assert_eq!(record.review_block(), None);
        assert_eq!(server.requests().len(), 1);
    }
}
