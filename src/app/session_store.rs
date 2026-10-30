use crate::domain::{Session, SessionId};
use std::{error::Error, fmt};

/// Synchronous persistence of complete session facts.
pub trait SessionStore {
    /// Returns all sessions sorted by ID, recovering active turns as `ProcessLost`.
    /// Recovery retains dispatch intent and does not write back. A missing store
    /// returns an empty list. Any corrupt committed session fails the listing.
    fn list(&self) -> Result<Vec<Session>, SessionStoreError>;
    /// Replaces the saved state for this ID with all current session facts.
    /// Active turns are saved faithfully without changing the live session.
    /// Repeating an identical save is safe. Unreadable representations are
    /// refused with `InvalidData`, preserving any previous saved state.
    /// Corrupt or unsupported existing records are also preserved and refused.
    /// An error after replacement may leave the new state visible; errors do
    /// not imply rollback, and retrying the identical save is safe.
    fn save(&self, session: &Session) -> Result<(), SessionStoreError>;
    /// Loads a session or returns `NotFound` when its record is absent.
    /// Recovery closes active turns as `ProcessLost`, retaining dispatch intent
    /// and typed execution and effect-resolution facts without writing back.
    fn load(&self, id: &SessionId) -> Result<Session, SessionStoreError>;
}

#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum SessionStoreErrorKind {
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
