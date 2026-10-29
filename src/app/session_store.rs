use crate::domain::SessionId;
use std::{error::Error, fmt};

pub trait SessionStore {
    fn list(&self) -> Result<Vec<SessionId>, SessionStoreError>;
    fn create(&self) -> Result<Box<dyn SessionWriter + '_>, SessionStoreError>;
    fn acquire(&self, id: &SessionId) -> Result<Box<dyn SessionWriter + '_>, SessionStoreError>;
    fn inspect(&self, id: &SessionId) -> Result<super::SessionRecord, SessionStoreError>;
}
pub trait SessionWriter {
    fn committed(&self) -> &super::SessionRecord;
    fn commit(&mut self, next: super::SessionRecord) -> Result<u64, SessionStoreError>;
}

#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum SessionStoreErrorKind {
    Busy,
    StaleRevision,
    WriterInvalidated,
    PublishedDurabilityUncertain,
    NotFound,
    InvalidData,
    UnsupportedVersion,
    Storage,
}

#[derive(Debug)]
pub struct SessionStoreError {
    kind: SessionStoreErrorKind,
    source: Box<dyn Error + Send + Sync>,
}
impl SessionStoreError {
    pub fn new(kind: SessionStoreErrorKind, source: impl Error + Send + Sync + 'static) -> Self {
        Self {
            kind,
            source: Box::new(source),
        }
    }
    pub fn kind(&self) -> SessionStoreErrorKind {
        self.kind
    }
}
impl fmt::Display for SessionStoreError {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        write!(f, "session store {:?}: {}", self.kind, self.source)
    }
}
impl Error for SessionStoreError {
    fn source(&self) -> Option<&(dyn Error + 'static)> {
        Some(self.source.as_ref())
    }
}
