# Session persistence

## Problem

An application must save complete session facts before dispatching a tool.
Messages alone omit dispatch intent and effect reconciliation.
The domain already exports `SessionSnapshot` and validates recovery through
`Session::restore`. The application port uses that boundary.

## Usage

```rust,no_run
use cola::{
    adapters::stores::JsonlSessionStore,
    app::{SessionStore, SessionStoreError},
    domain::{Session, SessionId},
};

# fn main() -> Result<(), Box<dyn std::error::Error>> {
let store = JsonlSessionStore::new(".cola/sessions");
let session = Session::new(SessionId::new("project/会话")?);
store.save(&session)?;
let recovered = store.load(session.id())?;
let sessions = store.list()?;
# Ok(())
# }

fn checkpoint(store: &dyn SessionStore, session: &Session) -> Result<(), SessionStoreError> {
    store.save(session)
}
```

Record `Session::start_tool`, save successfully, then perform external tool I/O.
Record the returned result and save again. A failed save stops dispatch.
Loading an active session closes its active turn as `ProcessLost` and retains
`Started` executions. Loading neither changes the live session nor writes the
recovered closure back to disk.

## Shape

`src/app/session_store.rs` owns the synchronous, object-safe `SessionStore`
trait and storage-neutral errors. Its dependency points to the domain.
`src/adapters/stores` owns `JsonlSessionStore`, its private wire format, and
filesystem publication. The domain has no storage dependency.

The historical design sketch established these public signatures.

```rust,ignore
pub trait SessionStore {
    fn list(&self) -> Result<Vec<Session>, SessionStoreError>;
    fn save(&self, session: &Session) -> Result<(), SessionStoreError>;
    fn load(&self, id: &SessionId) -> Result<Session, SessionStoreError>;
}

pub enum SessionStoreErrorKind {
    NotFound,
    InvalidData,
    UnsupportedVersion,
    Storage,
}

pub struct JsonlSessionStore { root: PathBuf }
impl JsonlSessionStore {
    pub fn new(root: impl Into<PathBuf>) -> Self { /* stores the root only */ }
}
```

The data structure is the existing snapshot tree. Private serde wire structs
use `deny_unknown_fields` and tagged enums to represent its fields and variants.
Decoding reconstructs checked leaf values through
their constructors. `Session::restore` owns transition validation and recovery.
Per **Model the Domain**, persistence retains execution records and resolutions
instead of flattening them into messages. Per **Boundary Discipline**, wire
values stay inside the adapter.

Each session owns one file named by the lowercase SHA-256 digest of its ID bytes.
The file embeds the original ID. Load verifies the requested ID and its digest
filename. List verifies every embedded ID against its digest filename.
Valid IDs may contain slashes, NUL,
Unicode, and long text without becoming path components.

The first JSONL record declares the format, version, session ID, closed-turn
count, and presence of an active turn. It is followed by one tagged record per
closed turn and an optional active-turn record. Every record ends in LF.
The reader rejects missing or extra records, wrong order, blank lines, unknown
fields, unsupported versions, malformed JSON, and invalid domain state.
It reads the format and version before applying the strict v1 header schema.
Future versions return `UnsupportedVersion` even when their header fields differ.
The closed-record count must match the declared count. The active record must
exist exactly when the header flag is true, and it must occur last.
Declared counts detect truncation that removes entire valid lines.

Save writes a complete snapshot to a unique temporary file in the same
directory, syncs the file, replaces the committed path, and syncs the directory
on Unix. Unix directory-sync errors propagate, including unsupported filesystem
operations. Failures before replacement preserve the prior file.
A directory-sync failure after replacement can return an error with new bytes
already visible. Retrying the same save is safe.

Save first verifies that the emitted representation is readable by the same
decoder. A snapshot beyond serde_json's parsing recursion limit fails with
`InvalidData` before publication, preserving the previous committed state.

Per **Separate Before Serializing Shared State**, sessions have separate files
and saves have separate temporary files. Concurrent saves of one ID use the
last rename. They provide no stale-write detection. Reads observe complete
files. `list` sorts by `SessionId`; it is not a transaction across concurrent
directory changes. A corrupt recognized file fails the listing.

Missing roots yield an empty list and a `NotFound` load error. Saving creates
the root. Save refuses to replace corrupt files or unsupported versions.
Only filenames matching 64 lowercase hexadecimal characters plus `.jsonl` are
committed session files. Temporary files use a distinct `.tmp` naming pattern.
Temporary files and unrelated filenames are ignored.

## Synthesis decision

Four independent candidates explored per-session replacement, numeric filename
allocation, and shared append-only journals. The full-snapshot candidate supplies
the per-session replacement structure. The header/turn candidate supplies the
format. The journal candidate supplies fixed-length digest naming
and separates independent sessions. The full-snapshot candidates supply refusal
to overwrite unreadable data and a numeric-fidelity experiment.

The independent Sonnet 5.5 cross-judge selected the same replacement structure
and accepted header/turn framing as an inspection tradeoff. The parent accepted
its requirements for explicit count checks, strict private DTOs, and filename
verification on both read paths. Standard dependencies replace handwritten
hashing, temporary-file management, and JSON field parsing.
Per **Exhaust the Design Space**, alternatives were compared before implementation.

## Tradeoffs accepted

- Whole snapshots are rewritten to keep recovery and publication simple.
- Full sessions are restored during listing to match the requested API.
- Last-rename-wins saves keep the three-method contract small.
- Unix temporary files start with mode `0600`, subject to the process umask.
- Orphan temporary files are ignored. The adapter does not clean them up automatically.
- Private serialization, SHA-256, and temporary-file dependencies replace
  handwritten parsing, hashing, and temporary-name machinery.

## Alternatives considered

| Alternative | Reason rejected |
| --- | --- |
| Shared append-only snapshot journal | Adds shared writes, retained history, tail recovery, and compaction. |
| Numeric filenames | Supports opaque IDs but adds allocation locking and whole-store lookup scans. |
| Hex-encoded IDs | Exceeds filesystem component limits for valid long IDs. |
| One snapshot per file line | Simpler framing, but offers less useful JSONL inspection of turns. |
| Message-only storage | Loses tool execution and effect reconciliation facts. |

## Open questions answered by prototypes

| Question | Observation and decision |
| --- | --- |
| Can a damaged journal tail be ignored? | The file prototype returned an older `not_started` value. Reject damaged committed data. |
| Does same-directory replacement publish complete bytes? | The prototype observed old bytes before rename and new bytes afterward. Stage complete files before replacement. |
| Can a complete line disappear undetected? | Remaining lines still parsed as JSON. Check declared record counts. |
| Can opaque IDs become filenames through hex encoding? | A 200-byte ID produced an overlong filename and the filesystem rejected it. Use fixed-length digests. |
| Does recovery resume active work? | The Rust prototype returned `Interrupted(ProcessLost)` and retained `Started`. Reuse domain recovery. |
| Are JSON floating-point arguments preserved? | Default parsing changed `5.757930830206806e-61` to an adjacent value. `float_roundtrip` produced no mismatches in the same deterministic probe. Enable it and keep a regression fixture. |

The throwaway probes live in the local `.scratch/session-store-prototype`
directory. They establish behavior on the local development filesystem.
They do not simulate power loss or establish network-filesystem guarantees.
Per **Prove It Works**, public store integration tests must also verify the
implemented artifact.

## Implementation reconciliation

The implementation follows the accepted port, ownership, and publication contract.
The codec lives in private `src/adapters/stores/jsonl/wire.rs`. Domain types have
no serialization derives or storage dependencies. `serde_json/float_roundtrip`
preserves the numeric regression fixture. Empty tagged variants use empty struct
variants because serde unit variants accepted unknown fields in a negative test.
Their serialized JSON remains unchanged.

The parent's post-implementation deep-argument prototype found that encoding
could succeed beyond the decoder's recursion limit. The parent accepted a
publication preflight through the existing decoder. This keeps a successful
save readable without weakening the parser's resource limit or changing the
public signatures. `JsonlSessionStore::save` decodes the encoded bytes before
creating a temporary file. The public integration regression proves refusal for
a new record and preserves the previous bytes and loaded session on replacement.

A typed format/version probe now precedes strict v1 header decoding. Future
versions return `UnsupportedVersion` without requiring v1 fields. Version 1
still rejects unknown fields and duplicate known keys. Read errors include the
file path and record ordinal where known, retaining their kind and typed source
chain.

The v1 format tag is `cola.session`. A complete one-turn file looks like this.
Each displayed record ends in LF, including the last record.

```jsonl
{"format":"cola.session","version":1,"session_id":"example","closed_turn_count":1,"active":false}
{"record":"closed","turn":{"id":1,"user":"Hello","steps":[{"assistant":{"kind":"answer","text":"Hello"},"executions":[]}],"outcome":{"kind":"completed"}}}
```

An active record uses `"record":"active"` and `"outcome":null`. Nested enum
variants use a `kind` field. Tool requests contain `text` and `calls`, each call
contains `id`, `name`, and `arguments`. Executions contain `call_id`, `state`, and
`resolution`. A returned state contains `outcome`, whose success stores `output`
and whose failure stores `description`. Resolutions store `disposition`, `source`,
and `evidence`. Interrupted outcomes contain `reason`; failed outcomes contain
`failure` with its nonblank `description`. The adapter owns this schema.

Public-port integration tests exercise real temporary files, complete facts,
recovery, opaque IDs, strict corruption rejection, overwrite refusal, and
concurrent publication. They establish local-filesystem behavior and do not
simulate power loss.
