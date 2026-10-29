use crate::{
    app::{
        Invocation, ReviewFailureAcknowledgement, SessionDocument, SessionStore, SessionStoreError,
        SessionStoreErrorKind, SessionWriter, TurnView,
    },
    domain::{
        AssistantSnapshot, EffectDisposition, EffectResolution, ExecutionSnapshot, ExecutionState,
        NonEmptyText, ResolutionSource, Session, SessionId, SessionSnapshot, StepSnapshot,
        StopReason, ToolCallSnapshot, ToolError, ToolOutcome, TurnFailure, TurnOutcome,
        TurnSnapshot,
    },
};
use serde::{Deserialize, Serialize};
use serde_json::Value;
use sha2::{Digest, Sha256};
use std::{
    error::Error,
    fmt, fs,
    io::{self, Write},
    path::{Path, PathBuf},
};

#[derive(Clone, Debug)]
pub struct JsonlSessionStore {
    root: PathBuf,
}
impl JsonlSessionStore {
    pub fn new(root: impl Into<PathBuf>) -> Self {
        Self { root: root.into() }
    }
    fn read(
        &self,
        path: &Path,
        requested: Option<&SessionId>,
    ) -> Result<SessionDocument, SessionStoreError> {
        let bytes = fs::read(path).map_err(read_error)?;
        let doc = decode(&bytes)?;
        if path.file_name().and_then(|s| s.to_str())
            != Some(filename(doc.session().id().as_str()).as_str())
            || requested.is_some_and(|id| id != doc.session().id())
        {
            return Err(invalid("session identity does not match file"));
        }
        Ok(doc)
    }
    fn lock(&self, id: &SessionId) -> Result<fs::File, SessionStoreError> {
        fs::create_dir_all(&self.root).map_err(storage)?;
        let lock = fs::OpenOptions::new()
            .read(true)
            .write(true)
            .create(true)
            .truncate(false)
            .open(self.root.join(format!("{}.lock", filename(id.as_str()))))
            .map_err(storage)?;
        lock.try_lock().map_err(|error| match error {
            fs::TryLockError::WouldBlock => SessionStoreError::new(
                SessionStoreErrorKind::Busy,
                io::Error::new(io::ErrorKind::WouldBlock, "session_busy"),
            ),
            fs::TryLockError::Error(e) => storage(e),
        })?;
        Ok(lock)
    }
}
impl SessionStore for JsonlSessionStore {
    fn list(&self) -> Result<Vec<SessionId>, SessionStoreError> {
        let entries = match fs::read_dir(&self.root) {
            Ok(e) => e,
            Err(e) if e.kind() == io::ErrorKind::NotFound => return Ok(vec![]),
            Err(e) => return Err(storage(e)),
        };
        let mut ids = vec![];
        for entry in entries {
            let entry = entry.map_err(storage)?;
            if entry.file_name().to_str().is_some_and(recognized) {
                ids.push(self.read(&entry.path(), None)?.session().id().clone());
            }
        }
        ids.sort();
        Ok(ids)
    }
    fn create(&self) -> Result<Box<dyn SessionWriter + '_>, SessionStoreError> {
        let id = SessionId::new(uuid::Uuid::new_v4().to_string()).unwrap();
        let lock = self.lock(&id)?;
        let doc = SessionDocument::new(id);
        let path = self.root.join(filename(doc.session().id().as_str()));
        publish(&self.root, &path, &doc)?;
        Ok(Box::new(Writer {
            root: self.root.clone(),
            path,
            _lock: lock,
            committed: doc,
            valid: true,
        }))
    }
    fn acquire(&self, id: &SessionId) -> Result<Box<dyn SessionWriter + '_>, SessionStoreError> {
        let lock = self.lock(id)?;
        let path = self.root.join(filename(id.as_str()));
        fs::File::open(&path)
            .map_err(read_error)?
            .sync_all()
            .map_err(storage)?;
        fs::File::open(&self.root)
            .and_then(|f| f.sync_all())
            .map_err(storage)?;
        let doc = self.read(&path, Some(id))?;
        Ok(Box::new(Writer {
            root: self.root.clone(),
            path,
            _lock: lock,
            committed: doc,
            valid: true,
        }))
    }
    fn inspect(&self, id: &SessionId) -> Result<SessionDocument, SessionStoreError> {
        self.read(&self.root.join(filename(id.as_str())), Some(id))
    }
}
struct Writer {
    root: PathBuf,
    path: PathBuf,
    _lock: fs::File,
    committed: SessionDocument,
    valid: bool,
}
impl SessionWriter for Writer {
    fn committed(&self) -> &SessionDocument {
        &self.committed
    }
    fn commit(&mut self, next: SessionDocument) -> Result<u64, SessionStoreError> {
        if !self.valid {
            return Err(SessionStoreError::new(
                SessionStoreErrorKind::WriterInvalidated,
                io::Error::other("writer invalidated"),
            ));
        }
        self.valid = false;
        if !self.committed.validates_successor(&next) {
            return Err(SessionStoreError::new(
                SessionStoreErrorKind::StaleRevision,
                io::Error::other("invalid document successor"),
            ));
        }
        publish(&self.root, &self.path, &next)?;
        self.committed = next;
        self.valid = true;
        Ok(self.committed.revision())
    }
}
fn publish(root: &Path, path: &Path, doc: &SessionDocument) -> Result<(), SessionStoreError> {
    let bytes = encode(doc)?;
    decode(&bytes)?;
    let mut staged = tempfile::Builder::new()
        .prefix(".cola-")
        .suffix(".tmp")
        .tempfile_in(root)
        .map_err(storage)?;
    staged.write_all(&bytes).map_err(storage)?;
    staged.as_file().sync_all().map_err(storage)?;
    staged.persist(path).map_err(|e| storage(e.error))?;
    fs::File::open(root)
        .and_then(|f| f.sync_all())
        .map_err(|e| {
            SessionStoreError::new(SessionStoreErrorKind::PublishedDurabilityUncertain, e)
        })?;
    Ok(())
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

#[derive(Deserialize)]
struct VersionProbe {
    format: String,
    version: u64,
}
#[derive(Serialize, Deserialize)]
#[serde(deny_unknown_fields)]
struct Header {
    format: String,
    version: u64,
    session_id: String,
    closed_turn_count: usize,
    active: bool,
    revision: u64,
    invocations: Vec<Invocation>,
    views: Vec<TurnView>,
    acknowledgements: Vec<ReviewFailureAcknowledgement>,
}
#[derive(Serialize, Deserialize)]
#[serde(tag = "record", rename_all = "snake_case", deny_unknown_fields)]
enum Record {
    Closed { turn: Turn },
    Active { turn: Turn },
}
#[derive(Serialize, Deserialize)]
#[serde(deny_unknown_fields)]
struct Turn {
    id: u64,
    user: String,
    steps: Vec<Step>,
    outcome: Option<Outcome>,
}
#[derive(Serialize, Deserialize)]
#[serde(deny_unknown_fields)]
struct Step {
    assistant: Assistant,
    executions: Vec<Execution>,
}
#[derive(Serialize, Deserialize)]
#[serde(tag = "kind", rename_all = "snake_case", deny_unknown_fields)]
enum Assistant {
    Answer {
        text: String,
    },
    ToolRequests {
        text: Option<String>,
        calls: Vec<Call>,
    },
}
#[derive(Serialize, Deserialize)]
#[serde(deny_unknown_fields)]
struct Call {
    id: String,
    name: String,
    arguments: Value,
}
#[derive(Serialize, Deserialize)]
#[serde(deny_unknown_fields)]
struct Execution {
    call_id: String,
    state: State,
    resolution: Option<Resolution>,
}
#[derive(Serialize, Deserialize)]
#[serde(tag = "kind", rename_all = "snake_case", deny_unknown_fields)]
enum State {
    NotStarted {},
    Started {},
    Returned { outcome: ToolReturn },
}
#[derive(Serialize, Deserialize)]
#[serde(tag = "kind", rename_all = "snake_case", deny_unknown_fields)]
enum ToolReturn {
    Success { output: String },
    Failure { description: String },
}
#[derive(Serialize, Deserialize)]
#[serde(deny_unknown_fields)]
struct Resolution {
    disposition: Disposition,
    source: Source,
    evidence: String,
}
#[derive(Serialize, Deserialize)]
#[serde(tag = "kind", rename_all = "snake_case", deny_unknown_fields)]
enum Disposition {
    Completed {},
    NotPerformed {},
}
#[derive(Serialize, Deserialize)]
#[serde(tag = "kind", rename_all = "snake_case", deny_unknown_fields)]
enum Source {
    Observation {},
    UserAttestation {},
    ApplicationAttestation {},
}
#[derive(Serialize, Deserialize)]
#[serde(tag = "kind", rename_all = "snake_case", deny_unknown_fields)]
enum Outcome {
    Completed {},
    Interrupted { reason: Reason },
    Failed { failure: Failure },
}
#[derive(Serialize, Deserialize)]
#[serde(tag = "kind", rename_all = "snake_case", deny_unknown_fields)]
enum Reason {
    UserCancelled {},
    ProcessLost {},
    ContextCapacity {},
    IterationLimit {},
}
#[derive(Serialize, Deserialize)]
#[serde(tag = "kind", rename_all = "snake_case", deny_unknown_fields)]
enum Failure {
    ModelFailure { description: String },
    MalformedResponse { description: String },
}

fn data_error(e: impl Error + Send + Sync + 'static) -> SessionStoreError {
    SessionStoreError::new(SessionStoreErrorKind::InvalidData, e)
}
fn encode(doc: &SessionDocument) -> Result<Vec<u8>, SessionStoreError> {
    let snapshot = doc.session().snapshot();
    let mut bytes = vec![];
    let header = Header {
        format: "cola.session".into(),
        version: 2,
        revision: doc.revision(),
        invocations: doc.invocations().to_vec(),
        views: doc.views().to_vec(),
        acknowledgements: doc.acknowledgements().to_vec(),
        session_id: snapshot.session_id,
        closed_turn_count: snapshot.turns.len(),
        active: snapshot.active.is_some(),
    };
    line(&mut bytes, &header)?;
    for turn in snapshot.turns {
        line(&mut bytes, &Record::Closed { turn: turn.into() })?;
    }
    if let Some(turn) = snapshot.active {
        line(&mut bytes, &Record::Active { turn: turn.into() })?;
    }
    Ok(bytes)
}
fn line(bytes: &mut Vec<u8>, value: &impl Serialize) -> Result<(), SessionStoreError> {
    serde_json::to_writer(&mut *bytes, value).map_err(data_error)?;
    bytes.push(b'\n');
    Ok(())
}
fn decode(bytes: &[u8]) -> Result<SessionDocument, SessionStoreError> {
    if !bytes.ends_with(b"\n") {
        return Err(invalid("missing final LF"));
    }
    let mut lines = bytes[..bytes.len() - 1].split(|b| *b == b'\n');
    let header_bytes = lines.next().ok_or_else(|| invalid("missing header"))?;
    let header = (|| {
        let probe: VersionProbe = serde_json::from_slice(header_bytes).map_err(data_error)?;
        if probe.format != "cola.session" {
            return Err(invalid("unknown session format"));
        }
        if probe.version != 2 {
            return Err(SessionStoreError::new(
                SessionStoreErrorKind::UnsupportedVersion,
                io::Error::new(
                    io::ErrorKind::InvalidData,
                    format!("unsupported session version {}", probe.version),
                ),
            ));
        }
        serde_json::from_slice::<Header>(header_bytes).map_err(data_error)
    })()
    .map_err(|e| context(e, "record 1".into()))?;
    let mut turns = vec![];
    let mut active = None;
    for (index, bytes) in lines.enumerate() {
        (|| {
            let record: Record = serde_json::from_slice(bytes).map_err(data_error)?;
            match record {
                Record::Closed { turn }
                    if active.is_none() && turns.len() < header.closed_turn_count =>
                {
                    turns.push(turn.try_into()?)
                }
                Record::Active { turn }
                    if active.is_none()
                        && header.active
                        && turns.len() == header.closed_turn_count =>
                {
                    active = Some(turn.try_into()?)
                }
                _ => return Err(invalid("unexpected session record order or count")),
            }
            Ok(())
        })()
        .map_err(|e| context(e, format!("record {}", index + 2)))?;
    }
    if turns.len() != header.closed_turn_count || active.is_some() != header.active {
        return Err(invalid("session record count mismatch"));
    }
    let session = Session::restore(SessionSnapshot {
        session_id: header.session_id,
        turns,
        active,
    })
    .map_err(data_error)?;
    SessionDocument::restore(
        header.revision,
        session,
        header.invocations,
        header.views,
        header.acknowledgements,
    )
    .map_err(data_error)
}
impl From<TurnSnapshot> for Turn {
    fn from(t: TurnSnapshot) -> Self {
        Self {
            id: t.id,
            user: t.user,
            steps: t.steps.into_iter().map(Into::into).collect(),
            outcome: t.outcome.map(Into::into),
        }
    }
}
impl TryFrom<Turn> for TurnSnapshot {
    type Error = SessionStoreError;
    fn try_from(t: Turn) -> Result<Self, Self::Error> {
        Ok(Self {
            id: t.id,
            user: t.user,
            steps: t
                .steps
                .into_iter()
                .map(TryInto::try_into)
                .collect::<Result<_, _>>()?,
            outcome: t.outcome.map(TryInto::try_into).transpose()?,
        })
    }
}
impl From<StepSnapshot> for Step {
    fn from(s: StepSnapshot) -> Self {
        Self {
            assistant: match s.assistant {
                AssistantSnapshot::Answer(text) => Assistant::Answer { text },
                AssistantSnapshot::ToolRequests { text, calls } => Assistant::ToolRequests {
                    text,
                    calls: calls
                        .into_iter()
                        .map(|c| Call {
                            id: c.id,
                            name: c.name,
                            arguments: c.arguments,
                        })
                        .collect(),
                },
            },
            executions: s.executions.into_iter().map(Into::into).collect(),
        }
    }
}
impl TryFrom<Step> for StepSnapshot {
    type Error = SessionStoreError;
    fn try_from(s: Step) -> Result<Self, Self::Error> {
        Ok(Self {
            assistant: match s.assistant {
                Assistant::Answer { text } => AssistantSnapshot::Answer(text),
                Assistant::ToolRequests { text, calls } => AssistantSnapshot::ToolRequests {
                    text,
                    calls: calls
                        .into_iter()
                        .map(|c| ToolCallSnapshot {
                            id: c.id,
                            name: c.name,
                            arguments: c.arguments,
                        })
                        .collect(),
                },
            },
            executions: s
                .executions
                .into_iter()
                .map(TryInto::try_into)
                .collect::<Result<_, _>>()?,
        })
    }
}
impl From<ExecutionSnapshot> for Execution {
    fn from(e: ExecutionSnapshot) -> Self {
        Self {
            call_id: e.call_id,
            state: match e.state {
                ExecutionState::NotStarted => State::NotStarted {},
                ExecutionState::Started => State::Started {},
                ExecutionState::Returned(outcome) => State::Returned {
                    outcome: match outcome {
                        ToolOutcome::Success(output) => ToolReturn::Success { output },
                        ToolOutcome::Failure(e) => ToolReturn::Failure {
                            description: e.description().as_str().into(),
                        },
                    },
                },
            },
            resolution: e.resolution.map(|r| Resolution {
                disposition: match r.disposition() {
                    EffectDisposition::Completed => Disposition::Completed {},
                    EffectDisposition::NotPerformed => Disposition::NotPerformed {},
                },
                source: match r.source() {
                    ResolutionSource::Observation => Source::Observation {},
                    ResolutionSource::UserAttestation => Source::UserAttestation {},
                    ResolutionSource::ApplicationAttestation => Source::ApplicationAttestation {},
                },
                evidence: r.evidence().as_str().into(),
            }),
        }
    }
}
impl TryFrom<Execution> for ExecutionSnapshot {
    type Error = SessionStoreError;
    fn try_from(e: Execution) -> Result<Self, Self::Error> {
        Ok(Self {
            call_id: e.call_id,
            state: match e.state {
                State::NotStarted {} => ExecutionState::NotStarted,
                State::Started {} => ExecutionState::Started,
                State::Returned { outcome } => ExecutionState::Returned(match outcome {
                    ToolReturn::Success { output } => ToolOutcome::Success(output),
                    ToolReturn::Failure { description } => ToolOutcome::Failure(ToolError::new(
                        NonEmptyText::new(description).map_err(data_error)?,
                    )),
                }),
            },
            resolution: e
                .resolution
                .map(|r| {
                    Ok(EffectResolution::new(
                        match r.disposition {
                            Disposition::Completed {} => EffectDisposition::Completed,
                            Disposition::NotPerformed {} => EffectDisposition::NotPerformed,
                        },
                        match r.source {
                            Source::Observation {} => ResolutionSource::Observation,
                            Source::UserAttestation {} => ResolutionSource::UserAttestation,
                            Source::ApplicationAttestation {} => {
                                ResolutionSource::ApplicationAttestation
                            }
                        },
                        NonEmptyText::new(r.evidence).map_err(data_error)?,
                    ))
                })
                .transpose()?,
        })
    }
}
impl From<TurnOutcome> for Outcome {
    fn from(o: TurnOutcome) -> Self {
        match o {
            TurnOutcome::Completed => Self::Completed {},
            TurnOutcome::Interrupted(r) => Self::Interrupted {
                reason: match r {
                    StopReason::UserCancelled => Reason::UserCancelled {},
                    StopReason::ProcessLost => Reason::ProcessLost {},
                    StopReason::ContextCapacity => Reason::ContextCapacity {},
                    StopReason::IterationLimit => Reason::IterationLimit {},
                },
            },
            TurnOutcome::Failed(f) => Self::Failed {
                failure: match f {
                    TurnFailure::ModelFailure(t) => Failure::ModelFailure {
                        description: t.as_str().into(),
                    },
                    TurnFailure::MalformedResponse(t) => Failure::MalformedResponse {
                        description: t.as_str().into(),
                    },
                },
            },
        }
    }
}
impl TryFrom<Outcome> for TurnOutcome {
    type Error = SessionStoreError;
    fn try_from(o: Outcome) -> Result<Self, Self::Error> {
        Ok(match o {
            Outcome::Completed {} => Self::Completed,
            Outcome::Interrupted { reason } => Self::Interrupted(match reason {
                Reason::UserCancelled {} => StopReason::UserCancelled,
                Reason::ProcessLost {} => StopReason::ProcessLost,
                Reason::ContextCapacity {} => StopReason::ContextCapacity,
                Reason::IterationLimit {} => StopReason::IterationLimit,
            }),
            Outcome::Failed { failure } => Self::Failed(match failure {
                Failure::ModelFailure { description } => {
                    TurnFailure::ModelFailure(NonEmptyText::new(description).map_err(data_error)?)
                }
                Failure::MalformedResponse { description } => TurnFailure::MalformedResponse(
                    NonEmptyText::new(description).map_err(data_error)?,
                ),
            }),
        })
    }
}
