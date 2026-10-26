# Session persistence

The application layer defines persistence of complete session facts. The JSONL
adapter stores those facts in per-session files and delegates recovery validation
to the domain.

## Application contract

[`SessionStore`](../src/app/session_store.rs) is a synchronous, object-safe port.
Each method operates on complete session state.

| Method | Contract |
| --- | --- |
| `list` | Returns sessions sorted by ID. A missing store is empty. Any corrupt committed session fails the listing. |
| `save` | Replaces all saved facts for the ID. Preserves active state without changing the live session. Repeating an identical save is safe. |
| `load` | Returns the requested session or `NotFound`. Closes formerly active work as `Interrupted(ProcessLost)` without writing back. |

Recovery retains dispatch intent, results, and effect resolutions. It never
resumes or re-executes a call. Save refuses unreadable representations and corrupt
or unsupported existing records. A save error after replacement can leave the
new state visible, so an error does not imply rollback.

`SessionStoreError::kind()` exposes a storage-neutral category. Its source chain
retains the underlying error.

| `SessionStoreErrorKind` | Meaning |
| --- | --- |
| `NotFound` | The requested record is absent. |
| `InvalidData` | Serialization, record structure, identity, or domain validation failed. |
| `UnsupportedVersion` | A recognized format declares an unsupported version. |
| `Storage` | Filesystem access or publication failed. |

## JSONL adapter

[`JsonlSessionStore`](../src/adapters/stores/jsonl.rs) owns the wire types and
filesystem publication. `new(root)` stores the path without filesystem access.
Save creates missing directories.

The adapter uses `Session::snapshot()` for export and `Session::restore()` for
validated recovery. Domain types have no serialization derives or storage
dependencies. The [domain reference](domain.md#snapshots-and-recovery) describes
snapshot validation.

### File identity and discovery

Each session has one file named by the lowercase SHA-256 digest of its ID bytes,
followed by `.jsonl`. The file embeds the original ID. Opaque IDs can contain
slashes, NUL, Unicode, and long text without becoming path components.

Load verifies the requested ID and digest filename. List verifies each embedded
ID against its filename. Only names matching 64 lowercase hexadecimal characters
plus `.jsonl` are recognized. Temporary files and unrelated names are ignored.
A corrupt recognized file fails the listing.

Listing is not a transaction across concurrent directory changes.

### Version 1 records

The first record declares the format, version, session ID, closed-turn count, and
presence of active work. Closed-turn records follow in order. An optional active
record appears last. Every record ends in LF, including the final record.

A complete session with one answered turn has this representation.

```jsonl
{"format":"cola.session","version":1,"session_id":"example","closed_turn_count":1,"active":false}
{"record":"closed","turn":{"id":1,"user":"Hello","steps":[{"assistant":{"kind":"answer","text":"Hello"},"executions":[]}],"outcome":{"kind":"completed"}}}
```

| Object | Fields |
| --- | --- |
| Header | `format`, `version`, `session_id`, `closed_turn_count`, `active`. |
| Turn record | `record` with value `closed` or `active`, and `turn`. |
| Turn | `id`, `user`, `steps`, `outcome`. An active turn has `outcome: null`. |
| Step | `assistant`, `executions`. |
| Assistant | `kind: answer` with `text`, or `kind: tool_requests` with `text` and `calls`. |
| Call | `id`, `name`, `arguments`. |
| Execution | `call_id`, `state`, `resolution`. |
| State | `kind: not_started`, `kind: started`, or `kind: returned` with `outcome`. |
| Tool outcome | `kind: success` with `output`, or `kind: failure` with `description`. |
| Resolution | `disposition`, `source`, `evidence`. |
| Turn outcome | `kind: completed`, `kind: interrupted` with `reason`, or `kind: failed` with `failure`. |

Descriptions and evidence must be nonblank. The decoder rejects unknown fields,
blank lines, malformed JSON, missing final LF, wrong record order or count,
identity mismatches, and invalid domain state.
Declared counts detect truncation that removes complete lines.

Format and version are read before the strict version 1 header. Future versions
return `UnsupportedVersion` even when their other header fields differ. Errors
include file paths and record ordinals where known while retaining typed sources.
`serde_json` uses `float_roundtrip` to preserve floating-point argument values.

### Publication and concurrency

Save checks existing committed data before replacement. It also decodes the new
encoded bytes before publication. This catches snapshots beyond the decoder's
recursion limit and preserves previous bytes on refusal.

Publication has the following order.

1. Write a unique temporary file in the destination directory.
2. Sync the temporary file.
3. Replace the committed path.
4. Sync the directory on Unix.

Failures before replacement preserve the prior file. A Unix directory-sync error
can return after new bytes become visible.

Concurrent saves use separate temporary files. Saves of the same ID use the last
rename and have no stale-write detection. Reads observe complete committed files.
Unix temporary files start with mode `0600`, subject to the process umask.
Orphan temporary files are ignored and are not automatically removed.

## Dispatch checkpoints

Persistence distinguishes a requested call from dispatch intent only when the
caller records `Session::start_tool` and saves before invoking the tool. A failed
save stops dispatch. After execution, the caller records the result and saves again.

## Test scope

Storage tests use real temporary files to check recovery, corruption rejection,
overwrite refusal, numeric fidelity, and concurrent publication. They do not
simulate power loss or establish guarantees for network filesystems.
