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
use sha2::{Digest, Sha256};
use std::{
    cell::{Cell, RefCell},
    collections::BTreeMap,
    io,
    num::{NonZeroU32, NonZeroU64, NonZeroUsize},
};

#[derive(Default)]
struct IndependentStore {
    entries: RefCell<BTreeMap<SessionId, Vec<u8>>>,
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
            .map(|bytes| SessionArchive::decode(bytes))
            .collect()
    }
    fn inspect(&self, id: &SessionId) -> Result<SessionArchive, SessionStoreError> {
        SessionArchive::decode(
            self.entries
                .borrow()
                .get(id)
                .ok_or_else(|| error(SessionStoreErrorKind::NotFound))?,
        )
    }
    fn create(&self, change: &ArchiveChange) -> Result<(), CommitFault> {
        let mut entries = self.entries.borrow_mut();
        let id = change.candidate().session().id();
        if change.base_key().is_some() || entries.contains_key(id) {
            return Err(CommitFault::Conflict);
        }
        entries.insert(id.clone(), change.encoded().to_vec());
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
        let bytes = self.confirmed.encode().map_err(CommitFault::NotPublished)?;
        let digest = format!("{:x}", Sha256::digest(&bytes));
        if change.candidate().session().id() != self.confirmed.session().id()
            || !change.base_key().is_some_and(|key| {
                key.revision() == self.confirmed.revision() && key.digest() == digest
            })
        {
            return Err(CommitFault::Conflict);
        }
        self.store.entries.borrow_mut().insert(
            self.confirmed.session().id().clone(),
            change.encoded().to_vec(),
        );
        self.confirmed = change.candidate().clone();
        Ok(())
    }
    fn resolve(&mut self, change: &ArchiveChange) -> Result<(), CommitFault> {
        let stored = self
            .store
            .inspect(self.confirmed.session().id())
            .map_err(CommitFault::Uncertain)?;
        if stored.encode().map_err(CommitFault::Uncertain)? == change.encoded() {
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
fn answered() -> SessionArchive {
    let mut draft = SessionArchive::new(SessionId::new("external").unwrap());
    let turn = draft
        .session_mut()
        .start_turn(UserMessage::new(text("first")))
        .unwrap();
    let request = request(&draft);
    let generation = draft
        .begin_generation(InvocationRole::Assistant { turn }, &request)
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
    draft
        .session_mut()
        .record_assistant(
            turn,
            generation,
            AssistantMessage::answer("first answer").unwrap(),
        )
        .unwrap();
    ArchiveChange::initial(draft).unwrap().candidate().clone()
}
#[test]
fn independent_store_uses_checked_bytes_and_public_candidate_accessors() {
    let store = IndependentStore::default();
    let init =
        ArchiveChange::initial(SessionArchive::new(SessionId::new("port").unwrap())).unwrap();
    assert_eq!(init.key().revision().get(), 1);
    assert_eq!(
        init.key().digest(),
        format!("{:x}", Sha256::digest(init.encoded()))
    );
    store.create(&init).unwrap();
    let mut tx = store.acquire(init.candidate().session().id()).unwrap();
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
    assert_eq!(
        tx.confirmed().generations()[0].observation(),
        &Observation::Pending
    );
    drop(tx);
    assert_eq!(
        store.inspect(change.candidate().session().id()).unwrap(),
        *change.candidate()
    );
    assert_eq!(store.list().unwrap(), vec![change.candidate().clone()]);
}
#[test]
fn candidate_cannot_rewrite_committed_outputs_requests_or_native_evidence() {
    let base = answered();
    let bytes = String::from_utf8(base.encode().unwrap()).unwrap();
    for (from, to) in [
        ("first answer", "rewritten answer"),
        ("\"user\":\"first\"", "\"user\":\"other\""),
        ("[123,125]", "[91,93]"),
        ("\"exact\":\"body\"", "\"exact\":\"else\""),
    ] {
        let changed = bytes.replace(from, to);
        assert_ne!(changed, bytes, "fixture mutation missing {from}");
        let candidate = SessionArchive::decode(changed.as_bytes()).unwrap();
        assert_eq!(
            ArchiveChange::next(&base, candidate).unwrap_err().kind(),
            SessionStoreErrorKind::InvalidData
        );
    }
}
#[test]
fn corrupt_revision_and_source_links_are_rejected_before_persistence() {
    let base = answered();
    let encoded = String::from_utf8(base.encode().unwrap()).unwrap();
    for (from, to) in [
        ("\"revision\":1", "\"revision\":0"),
        ("\"source_generation\":1", "\"source_generation\":2"),
        (
            "\"role\":{\"kind\":\"assistant\",\"turn\":1}",
            "\"role\":{\"kind\":\"assistant\",\"turn\":2}",
        ),
    ] {
        let changed = encoded.replace(from, to);
        assert_ne!(changed, encoded);
        assert_eq!(
            SessionArchive::decode(changed.as_bytes())
                .unwrap_err()
                .kind(),
            SessionStoreErrorKind::InvalidData
        );
    }
    assert!(ArchiveChange::initial(base.clone()).is_err());
    assert!(ArchiveChange::next(&base, SessionArchive::new(base.session().id().clone())).is_err());
}
#[test]
fn noncanonical_jsonl_is_rejected_instead_of_silently_changing_its_digest() {
    let base = answered();
    let bytes = base.encode().unwrap();
    let mut noncanonical = vec![b' '];
    noncanonical.extend(bytes);
    assert_eq!(
        SessionArchive::decode(&noncanonical).unwrap_err().kind(),
        SessionStoreErrorKind::InvalidData
    );
}
