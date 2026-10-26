mod wire;

use crate::{
    app::{SessionStore, SessionStoreError, SessionStoreErrorKind},
    domain::{Session, SessionId},
};
use sha2::{Digest, Sha256};
use std::{
    error::Error,
    fmt, fs,
    io::{self, Write},
    path::{Path, PathBuf},
};

/// Complete per-session snapshots published by same-directory replacement.
/// Concurrent saves use the last rename; there is no stale-write detection.
#[derive(Clone, Debug)]
pub struct JsonlSessionStore {
    root: PathBuf,
}
impl JsonlSessionStore {
    /// Chooses the root without accessing the filesystem.
    pub fn new(root: impl Into<PathBuf>) -> Self {
        Self { root: root.into() }
    }
    fn read(
        &self,
        path: &Path,
        requested: Option<&SessionId>,
    ) -> Result<Session, SessionStoreError> {
        let bytes = fs::read(path)
            .map_err(read_error)
            .map_err(|e| context(e, path.display().to_string()))?;
        let snapshot = wire::decode(&bytes).map_err(|e| context(e, path.display().to_string()))?;
        if path.file_name().and_then(|s| s.to_str())
            != Some(filename(&snapshot.session_id).as_str())
            || requested.is_some_and(|id| id.as_str() != snapshot.session_id)
        {
            return Err(context(
                invalid("session identity does not match its file"),
                path.display().to_string(),
            ));
        }
        Session::restore(snapshot).map_err(|e| {
            context(
                SessionStoreError::new(SessionStoreErrorKind::InvalidData, e),
                path.display().to_string(),
            )
        })
    }
}
impl SessionStore for JsonlSessionStore {
    fn list(&self) -> Result<Vec<Session>, SessionStoreError> {
        let entries = match fs::read_dir(&self.root) {
            Ok(entries) => entries,
            Err(e) if e.kind() == io::ErrorKind::NotFound => return Ok(vec![]),
            Err(e) => return Err(storage(e)),
        };
        let mut paths = vec![];
        for entry in entries {
            let entry = entry.map_err(storage)?;
            if entry.file_name().to_str().is_some_and(recognized) {
                paths.push(entry.path());
            }
        }
        paths.sort();
        let mut sessions = paths
            .iter()
            .map(|p| self.read(p, None))
            .collect::<Result<Vec<_>, _>>()?;
        sessions.sort_by(|a, b| a.id().cmp(b.id()));
        Ok(sessions)
    }
    fn save(&self, session: &Session) -> Result<(), SessionStoreError> {
        fs::create_dir_all(&self.root).map_err(storage)?;
        let path = self.root.join(filename(session.id().as_str()));
        match self.read(&path, Some(session.id())) {
            Ok(_) => {}
            Err(e) if e.kind() == SessionStoreErrorKind::NotFound => {}
            Err(e) => return Err(e),
        }
        let bytes = wire::encode(session.snapshot())?;
        wire::decode(&bytes)?;
        let mut staged = tempfile::Builder::new()
            .prefix(".cola-")
            .suffix(".tmp")
            .tempfile_in(&self.root)
            .map_err(storage)?;
        staged.write_all(&bytes).map_err(storage)?;
        staged.as_file().sync_all().map_err(storage)?;
        staged.persist(&path).map_err(|e| storage(e.error))?;
        #[cfg(unix)]
        fs::File::open(&self.root)
            .and_then(|f| f.sync_all())
            .map_err(storage)?;
        Ok(())
    }
    fn load(&self, id: &SessionId) -> Result<Session, SessionStoreError> {
        self.read(&self.root.join(filename(id.as_str())), Some(id))
    }
}
fn filename(id: &str) -> String {
    format!("{:x}.jsonl", Sha256::digest(id.as_bytes()))
}
fn recognized(name: &str) -> bool {
    name.len() == 70
        && name.ends_with(".jsonl")
        && name.as_bytes()[..64]
            .iter()
            .all(|b| b.is_ascii_digit() || (b'a'..=b'f').contains(b))
}
fn read_error(e: io::Error) -> SessionStoreError {
    SessionStoreError::new(
        if e.kind() == io::ErrorKind::NotFound {
            SessionStoreErrorKind::NotFound
        } else {
            SessionStoreErrorKind::Storage
        },
        e,
    )
}
fn storage(e: io::Error) -> SessionStoreError {
    SessionStoreError::new(SessionStoreErrorKind::Storage, e)
}
fn invalid(message: &'static str) -> SessionStoreError {
    SessionStoreError::new(
        SessionStoreErrorKind::InvalidData,
        io::Error::new(io::ErrorKind::InvalidData, message),
    )
}

#[derive(Debug)]
struct ReadContext {
    location: String,
    source: SessionStoreError,
}
impl fmt::Display for ReadContext {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        write!(f, "{}: {}", self.location, self.source)
    }
}
impl Error for ReadContext {
    fn source(&self) -> Option<&(dyn Error + 'static)> {
        Some(&self.source)
    }
}
fn context(source: SessionStoreError, location: String) -> SessionStoreError {
    SessionStoreError::new(source.kind(), ReadContext { location, source })
}
