# Session persistence

The application defines persistence of complete session facts. The JSONL adapter
owns the file format and publication. The domain validates recovered state.

## Application contract

[`SessionStore`](../src/app/session_store.rs) is synchronous and object safe.

| Method | Contract |
| --- | --- |
| `list` | Returns sessions sorted by ID. A missing store is empty. Any corrupt committed session fails the listing. |
| `save` | Replaces all facts for the ID without changing the live session. Repeating an identical save is safe. |
| `load` | Returns a session or `NotFound`. Closes saved active work as `Interrupted(ProcessLost)` without writing back. |

Recovery retains dispatch intent, results, and effect resolutions. It never
resumes or re-executes a call. Save refuses unreadable representations and
corrupt or unsupported existing records. A save error after replacement can
leave new state visible and does not imply rollback.

`SessionStoreError::kind()` exposes `NotFound`, `InvalidData`,
`UnsupportedVersion`, or `Storage`. The source chain retains the underlying error.

## JSONL adapter

[`JsonlSessionStore`](../src/adapters/stores/jsonl.rs) uses `Session::snapshot()`
for export and `Session::restore()` for recovery. Domain types have no storage
or serialization dependencies. See [snapshot validation](domain.md#snapshots-and-recovery).

`new(root)` stores the path without filesystem access. Save creates missing
directories. Each session file is named by the lowercase SHA-256 digest of its
ID bytes followed by `.jsonl`. The file embeds the original ID, so IDs never
become path components.

Load checks the requested ID and digest filename. List checks each embedded ID
against its filename. Only 64 lowercase hexadecimal characters followed by
`.jsonl` identify a committed file. Temporary files and unrelated names are
ignored. A corrupt recognized file fails the listing. Listing is not a
transaction across concurrent directory changes.

### Version 1 records

The header declares the format, version, session ID, closed-turn count, and
presence of active work. Closed-turn records follow in order. An optional active
record appears last. Every record ends in LF, including the final record.

A session with one answered turn has this representation.

```jsonl
{"format":"cola.session","version":1,"session_id":"example","closed_turn_count":1,"active":false}
{"record":"closed","turn":{"id":1,"user":"Hello","steps":[{"assistant":{"kind":"answer","text":"Hello"},"executions":[]}],"outcome":{"kind":"completed"}}}
```

The private wire types in `jsonl.rs` define the fields. Records preserve messages,
call arguments, execution states, results, resolutions, and turn outcomes.
Active turns have a null outcome. Descriptions and evidence must be nonblank.

The decoder rejects unknown fields, blank lines, malformed JSON, missing final
LF, wrong record order or count, identity mismatches, and invalid domain state.
Declared counts detect truncation that removes complete lines.

Format and version are read before the strict version 1 header. Future versions
return `UnsupportedVersion` even when their other fields differ. Errors include
paths and record ordinals where known and retain typed sources. The
`serde_json` feature `float_roundtrip` preserves floating-point arguments.

### Publication and concurrency

Save validates existing data and decodes the newly encoded bytes before
publication. The second check rejects unreadable representations, including
snapshots beyond the decoder's recursion limit, while preserving previous bytes.

Publication follows this sequence.

1. Write a unique temporary file in the destination directory.
2. Sync the temporary file.
3. Replace the committed path.
4. Sync the directory on Unix.

Failures before replacement preserve the prior file. A directory-sync error can
return after new bytes become visible. Concurrent saves use separate temporary
files. For the same ID, the last rename wins without stale-write detection.
Reads observe complete committed files.

Unix temporary files start with mode `0600`, subject to the process umask.
Orphan temporary files are ignored and are not automatically removed. Tests use
real files but do not simulate power loss or verify network filesystem guarantees.

## Dispatch checkpoints

Persistence distinguishes a requested call from dispatch intent only when the
caller records `Session::start_tool` and saves before external dispatch.
If that save fails, the caller must stop dispatch. After execution, the caller
records and saves the result. Recovery preserves these facts without retrying.

`AgentService` includes review in dispatch. It saves `Started` before reviewer
I/O, then either invokes the tool or records a denial. A crash during review can
therefore recover as an unknown effect. See [service checkpoints](agent-service.md#persistence-checkpoints).
