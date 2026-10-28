use cola::app::SessionStoreErrorKind;

#[test]
fn port_errors_preserve_the_original_source() {
    use cola::app::SessionStoreError;
    use std::{error::Error, io};
    let error = SessionStoreError::new(
        SessionStoreErrorKind::Storage,
        io::Error::new(io::ErrorKind::PermissionDenied, "original"),
    );
    let source = error.source().unwrap().downcast_ref::<io::Error>().unwrap();
    assert_eq!(source.kind(), io::ErrorKind::PermissionDenied);
    assert_eq!(source.to_string(), "original");
}

use cola::{app::*, domain::*};
use serde_json::json;
use std::{
    cell::{Cell, RefCell},
    collections::BTreeMap,
    io,
    num::{NonZeroU32, NonZeroU64, NonZeroUsize},
};

#[derive(Default)]
struct IndependentStore {
    entries: RefCell<BTreeMap<SessionId, StoredArchive>>,
    owned: Cell<bool>,
}
fn error(kind: SessionStoreErrorKind) -> SessionStoreError {
    SessionStoreError::new(kind, io::Error::other("independent store"))
}
impl SessionStore for IndependentStore {
    fn list(&self) -> Result<Vec<SessionArchive>, SessionStoreError> {
        self.entries
            .borrow()
            .values()
            .map(|stored| stored.restore())
            .collect()
    }
    fn inspect(&self, id: &SessionId) -> Result<SessionArchive, SessionStoreError> {
        StoredArchive::restore(
            self.entries
                .borrow()
                .get(id)
                .ok_or_else(|| error(SessionStoreErrorKind::NotFound))?,
        )
    }
    fn create(&self, change: &ArchiveChange) -> Result<(), CommitFault> {
        let mut entries = self.entries.borrow_mut();
        let id = change.candidate().session().id();
        if change.base().is_some() || entries.contains_key(id) {
            return Err(CommitFault::Conflict);
        }
        entries.insert(id.clone(), StoredArchive::capture(change.candidate()));
        Ok(())
    }
    fn acquire(
        &self,
        id: &SessionId,
    ) -> Result<Box<dyn SessionTransaction + '_>, SessionStoreError> {
        if self.owned.get() {
            return Err(error(SessionStoreErrorKind::Busy));
        }
        let confirmed = self.inspect(id)?;
        self.owned.set(true);
        Ok(Box::new(IndependentTransaction {
            store: self,
            confirmed,
        }))
    }
}
struct IndependentTransaction<'a> {
    store: &'a IndependentStore,
    confirmed: SessionArchive,
}
impl Drop for IndependentTransaction<'_> {
    fn drop(&mut self) {
        self.store.owned.set(false);
    }
}
impl SessionTransaction for IndependentTransaction<'_> {
    fn confirmed(&self) -> &SessionArchive {
        &self.confirmed
    }
    fn commit(&mut self, change: &ArchiveChange) -> Result<(), CommitFault> {
        if change.base() != Some(&self.confirmed) {
            return Err(CommitFault::Conflict);
        }
        if self
            .store
            .inspect(self.confirmed.session().id())
            .map_err(CommitFault::NotPublished)?
            != self.confirmed
        {
            return Err(CommitFault::Conflict);
        }
        self.store.entries.borrow_mut().insert(
            self.confirmed.session().id().clone(),
            StoredArchive::capture(change.candidate()),
        );
        self.confirmed = change.candidate().clone();
        Ok(())
    }
    fn resolve(&mut self, change: &ArchiveChange) -> Result<(), CommitFault> {
        let stored = self
            .store
            .inspect(self.confirmed.session().id())
            .map_err(CommitFault::Uncertain)?;
        if stored == *change.candidate() {
            self.confirmed = stored;
            Ok(())
        } else {
            Err(CommitFault::Conflict)
        }
    }
}
fn text(value: &str) -> NonEmptyText {
    NonEmptyText::new(value).unwrap()
}
fn request(archive: &SessionArchive) -> PreparedRequest<AssistantMessage> {
    let limits = ContextLimits {
        input_tokens: NonZeroU64::new(1000).unwrap(),
        request_bytes: NonZeroUsize::new(3000).unwrap(),
    };
    let plan = ContextPlan::assistant(archive, text("instructions"), vec![], limits, &[]).unwrap();
    PreparedRequest::new(
        ModelTarget::new("custom", "memory"),
        ModelSettings {
            model_id: ModelId::new("custom").unwrap(),
            effort: Effort::Off,
            max_output_tokens: NonZeroU32::new(1).unwrap(),
        },
        plan.fit(|_| json!({"exact":"body"})).unwrap(),
    )
}
#[test]
fn independent_store_reconstructs_public_snapshot_facts_without_jsonl() {
    let store = IndependentStore::default();
    let init =
        ArchiveChange::initial(SessionArchive::new(SessionId::new("port").unwrap())).unwrap();
    assert_eq!(init.candidate().revision().get(), 1);
    assert!(init.base().is_none());
    store.create(&init).unwrap();
    assert!(matches!(store.create(&init), Err(CommitFault::Conflict)));
    assert_eq!(
        store.inspect(init.candidate().session().id()).unwrap(),
        *init.candidate()
    );
    let mut tx = store.acquire(init.candidate().session().id()).unwrap();
    assert!(
        matches!(store.acquire(init.candidate().session().id()), Err(error) if error.kind() == SessionStoreErrorKind::Busy)
    );
    let mut draft = tx.confirmed().clone();
    draft
        .session_mut()
        .start_turn(UserMessage::new(text("pending")))
        .unwrap();
    let prepared = request(&draft);
    let turn = draft.session().active_turn().unwrap().id();
    draft
        .begin_generation(InvocationRole::Assistant { turn }, &prepared)
        .unwrap();
    let change = ArchiveChange::next(tx.confirmed(), draft).unwrap();
    tx.commit(&change).unwrap();
    assert!(matches!(tx.commit(&change), Err(CommitFault::Conflict)));
    tx.resolve(&change).unwrap();
    assert_eq!(
        tx.confirmed().generations()[0].observation(),
        &Observation::Pending
    );
    drop(tx);
    assert_eq!(
        store.inspect(change.candidate().session().id()).unwrap(),
        *change.candidate()
    );
    let later = ArchiveChange::initial(SessionArchive::new(SessionId::new("z").unwrap())).unwrap();
    store.create(&later).unwrap();
    assert_eq!(
        store.list().unwrap(),
        vec![change.candidate().clone(), later.candidate().clone()]
    );
}

struct StoredArchive {
    revision: u64,
    session: SessionSnapshot,
    generations: Vec<StoredGeneration>,
}
struct StoredGeneration {
    id: u64,
    role: StoredRole,
    target: (String, String),
    settings: (String, Effort, NonZeroU32),
    at: u64,
    request: serde_json::Value,
    selection: (u64, String, Vec<u64>, Vec<u64>, Vec<u64>),
    budget: (usize, usize, u64, u64, String),
    observation: StoredObservation,
}
enum StoredRole {
    Assistant(u64),
    Reviewer(u64, String),
}
enum StoredObservation {
    Pending,
    Abandoned(u64),
    Observed(u64, StoredReceipt, StoredDisposition),
}
struct StoredReceipt {
    status: Option<u16>,
    served_model: Option<String>,
    response_id: Option<String>,
    request_id: Option<String>,
    finish: Option<String>,
    usage: Option<(Option<u64>, Option<u64>)>,
    capture: StoredCapture,
}
enum StoredCapture {
    None,
    Complete(Vec<u8>),
    Interrupted(Vec<u8>),
    Truncated(Vec<u8>, usize),
}
enum StoredDisposition {
    Accepted,
    Refused(Option<String>),
    Incomplete(IncompleteReason),
    Rejected(String),
    Failed(ModelErrorKind, Option<u16>),
}
impl StoredArchive {
    fn capture(archive: &SessionArchive) -> Self {
        let snapshot = archive.snapshot();
        Self {
            revision: snapshot.revision,
            session: snapshot.session,
            generations: snapshot
                .generations
                .into_iter()
                .map(|g| StoredGeneration {
                    id: g.id,
                    role: match g.role {
                        InvocationRole::Assistant { turn } => StoredRole::Assistant(turn.get()),
                        InvocationRole::Reviewer { call } => StoredRole::Reviewer(
                            call.generation().get(),
                            call.call_id().as_str().into(),
                        ),
                    },
                    target: (g.target.provider().into(), g.target.endpoint().into()),
                    settings: (
                        g.settings.model_id.as_str().into(),
                        g.settings.effort,
                        g.settings.max_output_tokens,
                    ),
                    at: g.at.unix_millis(),
                    request: g.request,
                    selection: (
                        g.selection.revision,
                        g.selection.policy,
                        g.selection.required,
                        g.selection.included,
                        g.selection.omitted,
                    ),
                    budget: (
                        g.budget.request_bytes,
                        g.budget.max_request_bytes,
                        g.budget.estimated_input_tokens,
                        g.budget.input_token_budget,
                        g.budget.method,
                    ),
                    observation: match g.observation {
                        Observation::Pending => StoredObservation::Pending,
                        Observation::Abandoned { at } => {
                            StoredObservation::Abandoned(at.unix_millis())
                        }
                        Observation::Observed {
                            at,
                            receipt: r,
                            disposition,
                        } => StoredObservation::Observed(
                            at.unix_millis(),
                            StoredReceipt {
                                status: r.http_status,
                                served_model: r.served_model,
                                response_id: r.response_id,
                                request_id: r.request_id,
                                finish: r.finish,
                                usage: r.usage.map(|u| (u.input_tokens, u.output_tokens)),
                                capture: match r.capture {
                                    ResponseCapture::NotReceived => StoredCapture::None,
                                    ResponseCapture::Complete(b) => StoredCapture::Complete(b),
                                    ResponseCapture::Interrupted { prefix } => {
                                        StoredCapture::Interrupted(prefix)
                                    }
                                    ResponseCapture::Truncated { prefix, limit } => {
                                        StoredCapture::Truncated(prefix, limit)
                                    }
                                },
                            },
                            match disposition {
                                Disposition::Accepted => StoredDisposition::Accepted,
                                Disposition::Refused { reason } => {
                                    StoredDisposition::Refused(reason.map(|t| t.as_str().into()))
                                }
                                Disposition::Incomplete(r) => StoredDisposition::Incomplete(r),
                                Disposition::Rejected(r) => {
                                    StoredDisposition::Rejected(r.as_str().into())
                                }
                                Disposition::Failed(e) => {
                                    StoredDisposition::Failed(e.kind(), e.http_status())
                                }
                            },
                        ),
                    },
                })
                .collect(),
        }
    }
    fn restore(&self) -> Result<SessionArchive, SessionStoreError> {
        let generations = self
            .generations
            .iter()
            .map(|g| GenerationSnapshot {
                id: g.id,
                role: match &g.role {
                    StoredRole::Assistant(turn) => InvocationRole::Assistant {
                        turn: TurnId::new(*turn).unwrap(),
                    },
                    StoredRole::Reviewer(generation, call) => InvocationRole::Reviewer {
                        call: CallRef::new(
                            GenerationId::new(*generation).unwrap(),
                            ToolCallId::new(call.clone()).unwrap(),
                        ),
                    },
                },
                target: ModelTarget::new(g.target.0.clone(), g.target.1.clone()),
                settings: ModelSettings {
                    model_id: ModelId::new(g.settings.0.clone()).unwrap(),
                    effort: g.settings.1,
                    max_output_tokens: g.settings.2,
                },
                at: Timestamp::from_unix_millis(g.at),
                request: g.request.clone(),
                selection: SelectionSnapshot {
                    revision: g.selection.0,
                    policy: g.selection.1.clone(),
                    required: g.selection.2.clone(),
                    included: g.selection.3.clone(),
                    omitted: g.selection.4.clone(),
                },
                budget: BudgetSnapshot {
                    request_bytes: g.budget.0,
                    max_request_bytes: g.budget.1,
                    estimated_input_tokens: g.budget.2,
                    input_token_budget: g.budget.3,
                    method: g.budget.4.clone(),
                },
                observation: match &g.observation {
                    StoredObservation::Pending => Observation::Pending,
                    StoredObservation::Abandoned(at) => Observation::Abandoned {
                        at: Timestamp::from_unix_millis(*at),
                    },
                    StoredObservation::Observed(at, r, d) => Observation::Observed {
                        at: Timestamp::from_unix_millis(*at),
                        receipt: ResponseReceipt {
                            http_status: r.status,
                            served_model: r.served_model.clone(),
                            response_id: r.response_id.clone(),
                            request_id: r.request_id.clone(),
                            finish: r.finish.clone(),
                            usage: r.usage.map(|u| Usage {
                                input_tokens: u.0,
                                output_tokens: u.1,
                            }),
                            capture: match &r.capture {
                                StoredCapture::None => ResponseCapture::NotReceived,
                                StoredCapture::Complete(b) => ResponseCapture::Complete(b.clone()),
                                StoredCapture::Interrupted(b) => {
                                    ResponseCapture::Interrupted { prefix: b.clone() }
                                }
                                StoredCapture::Truncated(b, limit) => ResponseCapture::Truncated {
                                    prefix: b.clone(),
                                    limit: *limit,
                                },
                            },
                        },
                        disposition: match d {
                            StoredDisposition::Accepted => Disposition::Accepted,
                            StoredDisposition::Refused(r) => Disposition::Refused {
                                reason: r.as_ref().map(|t| text(t)),
                            },
                            StoredDisposition::Incomplete(r) => Disposition::Incomplete(*r),
                            StoredDisposition::Rejected(r) => Disposition::Rejected(text(r)),
                            StoredDisposition::Failed(k, status) => {
                                Disposition::Failed(status.map_or_else(
                                    || ModelError::new(*k),
                                    |s| ModelError::http(*k, s),
                                ))
                            }
                        },
                    },
                },
            })
            .collect();
        SessionArchive::restore(ArchiveSnapshot {
            revision: self.revision,
            session: self.session.clone(),
            generations,
        })
        .map_err(Into::into)
    }
}

#[test]
fn independent_representation_preserves_observed_and_abandoned_evidence() {
    let captures = [
        ResponseCapture::NotReceived,
        ResponseCapture::Complete(b"{}".to_vec()),
        ResponseCapture::Interrupted {
            prefix: b"{".to_vec(),
        },
        ResponseCapture::Truncated {
            prefix: b"{".to_vec(),
            limit: 1,
        },
    ];
    let dispositions = [
        Disposition::Refused {
            reason: Some(text("refused")),
        },
        Disposition::Incomplete(IncompleteReason::OutputLimit),
        Disposition::Rejected(text("rejected")),
        Disposition::Failed(ModelError::http(ModelErrorKind::RateLimited, 429)),
    ];
    for (capture, disposition) in captures.into_iter().zip(dispositions) {
        let mut draft = SessionArchive::new(SessionId::new("evidence").unwrap());
        let turn = draft
            .session_mut()
            .start_turn(UserMessage::new(text("first")))
            .unwrap();
        let generation = draft
            .begin_generation(InvocationRole::Assistant { turn }, &request(&draft))
            .unwrap();
        draft
            .record_observation(
                generation,
                Timestamp::now(),
                ResponseReceipt {
                    http_status: Some(200),
                    served_model: Some("served".into()),
                    response_id: Some("response".into()),
                    request_id: Some("request".into()),
                    finish: Some("done".into()),
                    usage: Some(Usage {
                        input_tokens: Some(12),
                        output_tokens: Some(3),
                    }),
                    capture,
                },
                disposition,
            )
            .unwrap();
        draft
            .session_mut()
            .interrupt(turn, StopReason::ProcessLost)
            .unwrap();
        let turn = draft
            .session_mut()
            .start_turn(UserMessage::new(text("second")))
            .unwrap();
        let generation = draft
            .begin_generation(InvocationRole::Assistant { turn }, &request(&draft))
            .unwrap();
        draft
            .session_mut()
            .record_assistant(
                turn,
                generation,
                AssistantMessage::answer("answer").unwrap(),
            )
            .unwrap();
        draft
            .record_observation(
                generation,
                Timestamp::now(),
                ResponseReceipt {
                    capture: ResponseCapture::Complete(b"{}".to_vec()),
                    ..ResponseReceipt::default()
                },
                Disposition::Accepted,
            )
            .unwrap();
        let turn = draft
            .session_mut()
            .start_turn(UserMessage::new(text("abandoned")))
            .unwrap();
        draft
            .begin_generation(InvocationRole::Assistant { turn }, &request(&draft))
            .unwrap();
        draft.abandon_pending(Timestamp::now());
        draft
            .session_mut()
            .interrupt(turn, StopReason::ProcessLost)
            .unwrap();
        let archive = ArchiveChange::initial(draft).unwrap();
        let stored = StoredArchive::capture(archive.candidate());
        assert_eq!(stored.restore().unwrap(), *archive.candidate());
        assert_eq!(stored.generations[1].selection.3, vec![1]);
        assert!(matches!(
            stored.generations[2].observation,
            StoredObservation::Abandoned(_)
        ));
    }
}
