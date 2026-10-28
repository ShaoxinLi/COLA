use crate::domain::SessionId;
use crate::domain::{ArchiveChange, SessionArchive};
use std::{error::Error, fmt};

/// Read-only inspection and exclusive transactional ownership of durable sessions.
pub trait SessionStore {
    fn list(&self) -> Result<Vec<SessionArchive>, SessionStoreError>;
    fn inspect(&self, id: &SessionId) -> Result<SessionArchive, SessionStoreError>;
    fn create(&self, initial: &ArchiveChange) -> Result<(), CommitFault>;
    fn acquire(
        &self,
        id: &SessionId,
    ) -> Result<Box<dyn SessionTransaction + '_>, SessionStoreError>;
}

/// Exclusive ownership remains held through publication and any uncertainty repair.
pub trait SessionTransaction {
    fn confirmed(&self) -> &SessionArchive;
    fn commit(&mut self, change: &ArchiveChange) -> Result<(), CommitFault>;
    /// Reconciles exactly this candidate, including a file and directory sync barrier.
    fn resolve(&mut self, change: &ArchiveChange) -> Result<(), CommitFault>;
}

#[derive(Debug)]
pub enum CommitFault {
    NotPublished(SessionStoreError),
    Uncertain(SessionStoreError),
    Conflict,
}
impl fmt::Display for CommitFault {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        match self {
            Self::NotPublished(error) => write!(f, "snapshot not published: {error}"),
            Self::Uncertain(error) => write!(f, "snapshot publication uncertain: {error}"),
            Self::Conflict => f.write_str("snapshot publication conflict"),
        }
    }
}
impl Error for CommitFault {
    fn source(&self) -> Option<&(dyn Error + 'static)> {
        match self {
            Self::NotPublished(error) | Self::Uncertain(error) => Some(error),
            Self::Conflict => None,
        }
    }
}

#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum SessionStoreErrorKind {
    NotFound,
    InvalidData,
    Storage,
    Busy,
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

impl From<crate::domain::ArchiveError> for SessionStoreError {
    fn from(error: crate::domain::ArchiveError) -> Self {
        Self::new(SessionStoreErrorKind::InvalidData, error)
    }
}
