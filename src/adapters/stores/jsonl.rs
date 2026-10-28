use crate::{
    app::{
        ArchiveChange, CommitFault, SessionArchive, SessionStore, SessionStoreError,
        SessionStoreErrorKind, SessionTransaction,
    },
    domain::SessionId,
};
use sha2::{Digest, Sha256};
use std::{
    error::Error,
    fmt,
    fs::{self, File, OpenOptions, TryLockError},
    io::{self, Write},
    path::{Path, PathBuf},
};

/// Atomic snapshots with stable sibling OS locks and checked stale-writer guards.
#[derive(Clone, Debug)]
pub struct JsonlSessionStore {
    root: PathBuf,
}
impl JsonlSessionStore {
    pub fn new(root: impl Into<PathBuf>) -> Self {
        Self { root: root.into() }
    }
    fn path(&self, id: &SessionId) -> PathBuf {
        self.root.join(filename(id.as_str()))
    }
    fn lock(&self, id: &SessionId) -> Result<File, SessionStoreError> {
        fs::create_dir_all(&self.root).map_err(storage)?;
        let path = self
            .root
            .join(format!("{:x}.lock", Sha256::digest(id.as_str().as_bytes())));
        let lock = OpenOptions::new()
            .read(true)
            .write(true)
            .create(true)
            .truncate(false)
            .open(&path)
            .map_err(storage)
            .map_err(|error| context(error, &path))?;
        lock.try_lock().map_err(|error| match error {
            TryLockError::WouldBlock => SessionStoreError::new(
                SessionStoreErrorKind::Busy,
                io::Error::new(
                    io::ErrorKind::WouldBlock,
                    "session is owned by another transaction",
                ),
            ),
            TryLockError::Error(error) => storage(error),
        })?;
        Ok(lock)
    }
    fn read(
        &self,
        path: &Path,
        requested: Option<&SessionId>,
    ) -> Result<SessionArchive, SessionStoreError> {
        let bytes = fs::read(path)
            .map_err(read_error)
            .map_err(|error| context(error, path))?;
        let archive = SessionArchive::decode(&bytes).map_err(|error| context(error, path))?;
        if path.file_name().and_then(|name| name.to_str())
            != Some(filename(archive.session().id().as_str()).as_str())
            || requested.is_some_and(|id| id != archive.session().id())
        {
            return Err(context(
                invalid("session identity does not match its file"),
                path,
            ));
        }
        Ok(archive)
    }
    fn sync(&self, path: &Path) -> Result<(), SessionStoreError> {
        // A fresh owner cannot treat a predecessor's visible rename as durable without this barrier.
        File::open(path)
            .and_then(|file| file.sync_all())
            .map_err(storage)
            .map_err(|error| context(error, path))?;
        #[cfg(test)]
        fault(FaultPoint::DirectorySync).map_err(storage)?;
        File::open(&self.root)
            .and_then(|directory| directory.sync_all())
            .map_err(storage)
            .map_err(|error| context(error, &self.root))
    }
    fn publish(
        &self,
        path: &Path,
        change: &ArchiveChange,
        initial: bool,
    ) -> Result<(), CommitFault> {
        #[cfg(test)]
        fault(FaultPoint::BeforeTempfile)
            .map_err(storage)
            .map_err(CommitFault::NotPublished)?;
        let mut staged = tempfile::Builder::new()
            .prefix(".cola-")
            .suffix(".tmp")
            .tempfile_in(&self.root)
            .map_err(storage)
            .map_err(CommitFault::NotPublished)?;
        #[cfg(test)]
        fault(FaultPoint::BeforeWrite)
            .map_err(storage)
            .map_err(CommitFault::NotPublished)?;
        staged
            .write_all(change.encoded())
            .map_err(storage)
            .map_err(CommitFault::NotPublished)?;
        #[cfg(test)]
        fault(FaultPoint::BeforeFileSync)
            .map_err(storage)
            .map_err(CommitFault::NotPublished)?;
        staged
            .as_file()
            .sync_all()
            .map_err(storage)
            .map_err(CommitFault::NotPublished)?;
        #[cfg(test)]
        fault(FaultPoint::BeforeRename)
            .map_err(storage)
            .map_err(CommitFault::NotPublished)?;
        #[cfg(test)]
        fault(FaultPoint::RenameError)
            .map_err(storage)
            .map_err(CommitFault::Uncertain)?;
        if initial {
            staged.persist_noclobber(path).map_err(|error| {
                if error.error.kind() == io::ErrorKind::AlreadyExists {
                    CommitFault::Conflict
                } else {
                    CommitFault::Uncertain(context(storage(error.error), path))
                }
            })?;
        } else {
            staged
                .persist(path)
                .map_err(|error| CommitFault::Uncertain(context(storage(error.error), path)))?;
        }
        #[cfg(test)]
        fault(FaultPoint::AfterRename)
            .map_err(storage)
            .map_err(CommitFault::Uncertain)?;
        self.sync(path).map_err(CommitFault::Uncertain)
    }
}
impl SessionStore for JsonlSessionStore {
    fn list(&self) -> Result<Vec<SessionArchive>, SessionStoreError> {
        let entries = match fs::read_dir(&self.root) {
            Ok(entries) => entries,
            Err(error) if error.kind() == io::ErrorKind::NotFound => return Ok(vec![]),
            Err(error) => return Err(storage(error)),
        };
        let mut paths = vec![];
        for entry in entries {
            let entry = entry.map_err(storage)?;
            if entry.file_name().to_str().is_some_and(recognized) {
                paths.push(entry.path());
            }
        }
        paths.sort();
        let mut archives = paths
            .iter()
            .map(|path| self.read(path, None))
            .collect::<Result<Vec<_>, _>>()?;
        archives.sort_by(|left, right| left.session().id().cmp(right.session().id()));
        Ok(archives)
    }
    fn inspect(&self, id: &SessionId) -> Result<SessionArchive, SessionStoreError> {
        self.read(&self.path(id), Some(id))
    }
    fn create(&self, initial: &ArchiveChange) -> Result<(), CommitFault> {
        if initial.base_key().is_some() {
            return Err(CommitFault::Conflict);
        }
        let id = initial.candidate().session().id();
        let _lock = self.lock(id).map_err(CommitFault::NotPublished)?;
        let path = self.path(id);
        match fs::symlink_metadata(&path) {
            Ok(_) => return Err(CommitFault::Conflict),
            Err(error) if error.kind() == io::ErrorKind::NotFound => {}
            Err(error) => return Err(CommitFault::NotPublished(context(storage(error), &path))),
        }
        self.publish(&path, initial, true)
    }
    fn acquire(
        &self,
        id: &SessionId,
    ) -> Result<Box<dyn SessionTransaction + '_>, SessionStoreError> {
        let lock = self.lock(id)?;
        let path = self.path(id);
        let confirmed = self.read(&path, Some(id))?;
        // This resolves an earlier owner's uncertain publication before any effect is admitted.
        self.sync(&path)?;
        Ok(Box::new(Transaction {
            store: self,
            path,
            _lock: lock,
            confirmed,
            unresolved: None,
        }))
    }
}
struct Transaction<'a> {
    store: &'a JsonlSessionStore,
    path: PathBuf,
    _lock: File,
    confirmed: SessionArchive,
    unresolved: Option<String>,
}
impl Transaction<'_> {
    fn current_digest(&self) -> Result<String, SessionStoreError> {
        let bytes = fs::read(&self.path)
            .map_err(read_error)
            .map_err(|error| context(error, &self.path))?;
        let archive = SessionArchive::decode(&bytes).map_err(|error| context(error, &self.path))?;
        if archive.session().id() != self.confirmed.session().id() {
            return Err(context(
                invalid("session identity does not match its file"),
                &self.path,
            ));
        }
        Ok(format!("{:x}", Sha256::digest(bytes)))
    }
    fn matches_base(&self, change: &ArchiveChange) -> bool {
        change.candidate().session().id() == self.confirmed.session().id()
            && change.base_key().is_some_and(|base| {
                base.revision() == self.confirmed.revision()
                    && self
                        .confirmed
                        .encode()
                        .is_ok_and(|bytes| base.digest() == format!("{:x}", Sha256::digest(bytes)))
            })
    }
}
impl SessionTransaction for Transaction<'_> {
    fn confirmed(&self) -> &SessionArchive {
        &self.confirmed
    }
    fn commit(&mut self, change: &ArchiveChange) -> Result<(), CommitFault> {
        if self.unresolved.is_some() || !self.matches_base(change) {
            return Err(CommitFault::Conflict);
        }
        let digest = self.current_digest().map_err(CommitFault::NotPublished)?;
        if !change
            .base_key()
            .is_some_and(|base| base.digest() == digest)
        {
            return Err(CommitFault::Conflict);
        }
        match self.store.publish(&self.path, change, false) {
            Ok(()) => {
                self.confirmed = change.candidate().clone();
                Ok(())
            }
            Err(CommitFault::Uncertain(error)) => {
                self.unresolved = Some(change.key().digest().into());
                Err(CommitFault::Uncertain(error))
            }
            Err(error) => Err(error),
        }
    }
    fn resolve(&mut self, change: &ArchiveChange) -> Result<(), CommitFault> {
        if !self.matches_base(change)
            || self
                .unresolved
                .as_ref()
                .is_some_and(|digest| digest != change.key().digest())
        {
            return Err(CommitFault::Conflict);
        }
        let digest = self.current_digest().map_err(CommitFault::Uncertain)?;
        if digest == change.key().digest() {
            self.store
                .sync(&self.path)
                .map_err(CommitFault::Uncertain)?;
            self.confirmed = change.candidate().clone();
            self.unresolved = None;
            Ok(())
        } else if change
            .base_key()
            .is_some_and(|base| base.digest() == digest)
        {
            self.unresolved = None;
            Err(CommitFault::NotPublished(SessionStoreError::new(
                SessionStoreErrorKind::Storage,
                io::Error::other("candidate is absent; confirmed base remains published"),
            )))
        } else {
            Err(CommitFault::Conflict)
        }
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
            .all(|byte| byte.is_ascii_digit() || (b'a'..=b'f').contains(byte))
}
fn read_error(error: io::Error) -> SessionStoreError {
    SessionStoreError::new(
        if error.kind() == io::ErrorKind::NotFound {
            SessionStoreErrorKind::NotFound
        } else {
            SessionStoreErrorKind::Storage
        },
        error,
    )
}
fn storage(error: io::Error) -> SessionStoreError {
    SessionStoreError::new(SessionStoreErrorKind::Storage, error)
}
fn invalid(message: &'static str) -> SessionStoreError {
    SessionStoreError::new(
        SessionStoreErrorKind::InvalidData,
        io::Error::new(io::ErrorKind::InvalidData, message),
    )
}
#[derive(Debug)]
struct PathError {
    path: PathBuf,
    source: SessionStoreError,
}
impl fmt::Display for PathError {
    fn fmt(&self, formatter: &mut fmt::Formatter<'_>) -> fmt::Result {
        write!(formatter, "{}: {}", self.path.display(), self.source)
    }
}
impl Error for PathError {
    fn source(&self) -> Option<&(dyn Error + 'static)> {
        Some(&self.source)
    }
}
fn context(source: SessionStoreError, path: &Path) -> SessionStoreError {
    SessionStoreError::new(
        source.kind(),
        PathError {
            path: path.into(),
            source,
        },
    )
}

#[cfg(test)]
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
enum FaultPoint {
    BeforeTempfile,
    BeforeWrite,
    BeforeFileSync,
    BeforeRename,
    RenameError,
    AfterRename,
    DirectorySync,
}
#[cfg(test)]
thread_local! { static FAULT: std::cell::Cell<Option<FaultPoint>> = const { std::cell::Cell::new(None) }; }
#[cfg(test)]
fn fault(point: FaultPoint) -> io::Result<()> {
    if std::env::var("COLA_ARCHIVE_EXIT_POINT").is_ok_and(|value| value == format!("{point:?}")) {
        std::process::exit(86);
    }
    if FAULT.with(|fault| fault.get() == Some(point)) {
        FAULT.with(|fault| fault.set(None));
        Err(io::Error::other(format!("injected {point:?}")))
    } else {
        Ok(())
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::{
        app::*,
        domain::{NonEmptyText, UserMessage},
    };
    fn initial(store: &JsonlSessionStore) -> ArchiveChange {
        let change =
            ArchiveChange::initial(SessionArchive::new(SessionId::new("faults").unwrap())).unwrap();
        store.create(&change).unwrap();
        change
    }
    fn candidate(base: &SessionArchive) -> ArchiveChange {
        let mut next = base.clone();
        next.session_mut()
            .start_turn(UserMessage::new(NonEmptyText::new("intent").unwrap()))
            .unwrap();
        ArchiveChange::next(base, next).unwrap()
    }
    #[test]
    fn errors_before_publication_preserve_exact_base_and_allow_new_commit() {
        for point in [
            FaultPoint::BeforeTempfile,
            FaultPoint::BeforeWrite,
            FaultPoint::BeforeFileSync,
            FaultPoint::BeforeRename,
        ] {
            let root = tempfile::tempdir().unwrap();
            let store = JsonlSessionStore::new(root.path());
            let init = initial(&store);
            let mut tx = store.acquire(init.candidate().session().id()).unwrap();
            let change = candidate(tx.confirmed());
            FAULT.with(|fault| fault.set(Some(point)));
            assert!(
                matches!(tx.commit(&change), Err(CommitFault::NotPublished(_))),
                "{point:?}"
            );
            assert_eq!(
                fs::read(store.path(init.candidate().session().id())).unwrap(),
                init.encoded()
            );
            assert_eq!(tx.confirmed(), init.candidate());
            tx.commit(&change).unwrap();
            assert_eq!(tx.confirmed(), change.candidate());
        }
    }
    #[test]
    fn ambiguous_publication_blocks_commits_until_exact_candidate_is_synced() {
        for point in [FaultPoint::AfterRename, FaultPoint::DirectorySync] {
            let root = tempfile::tempdir().unwrap();
            let store = JsonlSessionStore::new(root.path());
            let init = initial(&store);
            let mut tx = store.acquire(init.candidate().session().id()).unwrap();
            let change = candidate(tx.confirmed());
            FAULT.with(|fault| fault.set(Some(point)));
            assert!(
                matches!(tx.commit(&change), Err(CommitFault::Uncertain(_))),
                "{point:?}"
            );
            assert_eq!(
                fs::read(store.path(init.candidate().session().id())).unwrap(),
                change.encoded()
            );
            assert_eq!(tx.confirmed(), init.candidate());
            assert!(matches!(tx.commit(&change), Err(CommitFault::Conflict)));
            // Seeing the same bytes cannot repair a failed synchronization barrier.
            FAULT.with(|fault| fault.set(Some(FaultPoint::DirectorySync)));
            assert!(matches!(
                tx.resolve(&change),
                Err(CommitFault::Uncertain(_))
            ));
            assert_eq!(tx.confirmed(), init.candidate());
            tx.resolve(&change).unwrap();
            assert_eq!(tx.confirmed(), change.candidate());
        }
    }
    #[test]
    fn rename_error_resolves_to_absent_candidate_without_overwriting_base() {
        let root = tempfile::tempdir().unwrap();
        let store = JsonlSessionStore::new(root.path());
        let init = initial(&store);
        let mut tx = store.acquire(init.candidate().session().id()).unwrap();
        let change = candidate(tx.confirmed());
        FAULT.with(|fault| fault.set(Some(FaultPoint::RenameError)));
        assert!(matches!(tx.commit(&change), Err(CommitFault::Uncertain(_))));
        assert!(matches!(
            tx.resolve(&change),
            Err(CommitFault::NotPublished(_))
        ));
        assert_eq!(tx.confirmed(), init.candidate());
        tx.commit(&change).unwrap();
    }
    #[test]
    fn fresh_acquire_requires_synchronization_of_predecessor_uncertainty() {
        let root = tempfile::tempdir().unwrap();
        let store = JsonlSessionStore::new(root.path());
        let init = initial(&store);
        let mut tx = store.acquire(init.candidate().session().id()).unwrap();
        let change = candidate(tx.confirmed());
        FAULT.with(|fault| fault.set(Some(FaultPoint::AfterRename)));
        assert!(matches!(tx.commit(&change), Err(CommitFault::Uncertain(_))));
        drop(tx);
        FAULT.with(|fault| fault.set(Some(FaultPoint::DirectorySync)));
        assert!(store.acquire(init.candidate().session().id()).is_err());
        assert_eq!(
            store
                .acquire(init.candidate().session().id())
                .unwrap()
                .confirmed(),
            change.candidate()
        );
    }
    #[test]
    fn uncertain_creation_requires_fresh_sync_and_cannot_replace_id() {
        let root = tempfile::tempdir().unwrap();
        let store = JsonlSessionStore::new(root.path());
        let change =
            ArchiveChange::initial(SessionArchive::new(SessionId::new("new").unwrap())).unwrap();
        FAULT.with(|fault| fault.set(Some(FaultPoint::AfterRename)));
        assert!(matches!(
            store.create(&change),
            Err(CommitFault::Uncertain(_))
        ));
        assert!(matches!(store.create(&change), Err(CommitFault::Conflict)));
        FAULT.with(|fault| fault.set(Some(FaultPoint::DirectorySync)));
        assert!(store.acquire(change.candidate().session().id()).is_err());
        assert_eq!(
            store
                .acquire(change.candidate().session().id())
                .unwrap()
                .confirmed(),
            change.candidate()
        );
    }
    #[test]
    fn transaction_child_process_exit_preserves_publication_boundary() {
        use std::process::{Command, Stdio};
        const ROOT: &str = "COLA_ARCHIVE_EXIT_ROOT";
        if let Ok(root) = std::env::var(ROOT) {
            let store = JsonlSessionStore::new(root);
            let id = SessionId::new("faults").unwrap();
            let mut tx = store.acquire(&id).unwrap();
            let change = candidate(tx.confirmed());
            tx.commit(&change).unwrap();
            panic!("child missed crash point");
        }
        for point in ["BeforeRename", "AfterRename"] {
            let root = tempfile::tempdir().unwrap();
            let store = JsonlSessionStore::new(root.path());
            let init = initial(&store);
            let status = Command::new(std::env::current_exe().unwrap())
                .args([
                    "transaction_child_process_exit_preserves_publication_boundary",
                    "--nocapture",
                ])
                .env(ROOT, root.path())
                .env("COLA_ARCHIVE_EXIT_POINT", point)
                .stdout(Stdio::null())
                .stderr(Stdio::null())
                .status()
                .unwrap();
            assert_eq!(status.code(), Some(86));
            let visible = store.inspect(init.candidate().session().id()).unwrap();
            assert_eq!(
                visible.revision().get(),
                if point == "BeforeRename" { 1 } else { 2 }
            );
            assert_eq!(visible.needs_recovery(), point == "AfterRename");
            assert_eq!(store.list().unwrap(), vec![visible.clone()]);
            assert_eq!(
                store
                    .acquire(init.candidate().session().id())
                    .unwrap()
                    .confirmed(),
                &visible
            );
        }
    }
}
