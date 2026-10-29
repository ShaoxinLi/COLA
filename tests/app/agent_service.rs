#[allow(dead_code)]
#[path = "../adapters/models/support.rs"]
mod support;
use cola::{adapters::stores::JsonlSessionStore, app::*, domain::*};
use std::{
    path::Path,
    sync::{
        Arc,
        atomic::{AtomicUsize, Ordering},
    },
};
use support::*;

struct FaultStore {
    inner: JsonlSessionStore,
    revision: u64,
    published: bool,
}
struct FaultWriter<'a> {
    inner: Box<dyn SessionWriter + 'a>,
    revision: u64,
    published: bool,
    valid: bool,
}
impl SessionWriter for FaultWriter<'_> {
    fn committed(&self) -> &SessionDocument {
        self.inner.committed()
    }
    fn commit(&mut self, next: SessionDocument) -> Result<u64, SessionStoreError> {
        assert!(self.valid, "no further commit after storage failure");
        if next.revision() == self.revision {
            self.valid = false;
            if self.published {
                self.inner.commit(next)?;
            }
            return Err(SessionStoreError::new(
                if self.published {
                    SessionStoreErrorKind::PublishedDurabilityUncertain
                } else {
                    SessionStoreErrorKind::Storage
                },
                std::io::Error::other("injected write failure"),
            ));
        }
        self.inner.commit(next)
    }
}
impl SessionStore for FaultStore {
    fn list(&self) -> Result<Vec<SessionId>, SessionStoreError> {
        self.inner.list()
    }
    fn inspect(&self, id: &SessionId) -> Result<SessionDocument, SessionStoreError> {
        self.inner.inspect(id)
    }
    fn create(&self) -> Result<Box<dyn SessionWriter + '_>, SessionStoreError> {
        self.inner.create()
    }
    fn acquire(&self, id: &SessionId) -> Result<Box<dyn SessionWriter + '_>, SessionStoreError> {
        Ok(Box::new(FaultWriter {
            inner: self.inner.acquire(id)?,
            revision: self.revision,
            published: self.published,
            valid: true,
        }))
    }
}
fn faults(root: &Path, revision: u64, published: bool) -> FaultStore {
    FaultStore {
        inner: JsonlSessionStore::new(root),
        revision,
        published,
    }
}

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
        let AgentErrorKind::ReviewBlocked { invocation } = error.kind() else {
            panic!("{error:?}")
        };
        let invocation = *invocation;
        drop(svc);
        let mut reopened = service(provider, &server, root.path());
        assert!(matches!(
            reopened.chat(&id, user("blocked")).unwrap_err().kind(),
            AgentErrorKind::ReviewBlocked { .. }
        ));
        assert_eq!(server.requests().len(), 2);
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
        assert_eq!(
            doc.session().turns()[0].outcome(),
            &TurnOutcome::Interrupted(StopReason::ProcessLost)
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
        assert!(
            reopened
                .acknowledge_review_failure(&id, invocation, text("different"), None)
                .is_err()
        );
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
#[test]
fn context_selection_pins_unresolved_effect_turns_outside_optional_window() {
    let mut session = Session::new(SessionId::new("selection").unwrap());
    let turn = session.start_turn(user("mandatory")).unwrap();
    session
        .record_assistant(
            turn,
            AssistantMessage::tool_requests(
                None,
                ToolCalls::new(vec![
                    ToolCall::new(
                        ToolCallId::new("native").unwrap(),
                        tool().name().clone(),
                        serde_json::json!({}),
                    )
                    .unwrap(),
                ])
                .unwrap(),
            ),
        )
        .unwrap();
    let address = session
        .call_address(ToolCallId::new("native").unwrap())
        .unwrap();
    session.start_tool(&address).unwrap();
    session.interrupt(turn, StopReason::ProcessLost).unwrap();
    for _ in 0..12 {
        let t = session.start_turn(user("optional")).unwrap();
        session
            .record_assistant(t, AssistantMessage::answer("done").unwrap())
            .unwrap();
    }
    let selection = RecentContextPolicy::default().select(&session);
    assert_eq!(selection.mandatory_turns, vec![turn]);
    assert_eq!(selection.closed_turns.len(), 9);
    assert_eq!(selection.closed_turns[0], turn);
    let fitted = selection.fit(|ids| Ok(ids.len() * 10000)).unwrap();
    assert!(fitted.contains(&turn));
    assert_eq!(fitted.len(), 6);
}
