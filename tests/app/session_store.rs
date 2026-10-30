use cola::{adapters::stores::JsonlSessionStore, app::*, domain::SessionId};
use std::path::Path;

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

pub(super) struct FaultStore {
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
    fn committed(&self) -> &SessionRecord {
        self.inner.committed()
    }
    fn commit(&mut self, next: SessionRecord) -> Result<u64, SessionStoreError> {
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
    fn inspect(&self, id: &SessionId) -> Result<SessionRecord, SessionStoreError> {
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
pub(super) fn faults(root: &Path, revision: u64, published: bool) -> FaultStore {
    FaultStore {
        inner: JsonlSessionStore::new(root),
        revision,
        published,
    }
}
