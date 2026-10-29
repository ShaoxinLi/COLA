# Session persistence

`SessionStore` persists complete application `SessionRecord` snapshots.
Documents contain the domain session, revision, per-invocation plans and audit,
frozen turn views, and review-failure acknowledgements.

## Store and writer contracts

| Method | Contract |
| --- | --- |
| `list()` | Returns sorted IDs without recovery or writes. |
| `inspect(id)` | Validates and returns exact stored facts, including any active turn. |
| `create()` | Publishes a new empty session and returns its exclusive writer. |
| `acquire(id)` | Acquires the stable OS lock, synchronizes file and directory, then reads the current document. |
| `SessionWriter::committed()` | Returns the last successfully committed working copy. |
| `SessionWriter::commit(next)` | Publishes a validated successor with the same identity and revision exactly one higher. |

Document fields and mutation methods are not externally writable. Application
transitions bind accepted assistant steps to invocation provenance, reviews to
complete call addresses, requests to frozen views, and acknowledgements to actual
review failures. Restore rejects missing or duplicate accepted-step provenance,
invalid ordinals, invalid lifecycle associations, and conflicting view metadata.
Domain snapshots are validated separately through normal domain transitions.

## JSONL version 2

`JsonlSessionStore::new(root)` selects a directory without opening it. Each session
uses a SHA-256-derived filename ending in `.jsonl`. A stable sibling `.lock` file
is held with the operating system's exclusive file lock for the whole command.
It is never deleted to release ownership. A competing process receives `Busy`.
The OS releases ownership when the writer drops or its process exits. Acquiring
a missing session creates no directory or lock file. An existing authoritative
JSONL archive can recreate its missing lock file.

The first JSON line contains `format: "cola.session"`, `version: 2`, session ID,
record counts, revision, invocation audit, views, and acknowledgements. Subsequent
lines hold ordered closed turns followed by an optional active turn. The file
ends with a newline. Header and turn-record schemas reject unknown fields;
nested application audit objects accept additional fields. Invalid counts,
invalid associations, and filename/identity mismatches are rejected. Version 1 is intentionally unsupported;
there is no compatibility conversion or automatic deletion of old archives.

Provider payloads are opaque versioned `ModelArtifact` byte envelopes. The JSONL
adapter owns format DTOs. Domain types do not implement storage deserialization
or contain provider wire artifacts. Existing generic tool JSON values remain
part of the domain API.

## Publication and failure semantics

A commit serializes and validates the full successor, writes a same-directory
temporary file, synchronizes that file, atomically replaces the session path,
then synchronizes the directory. Only complete snapshots become visible.

Any commit error invalidates that writer. A failure before replacement preserves
the prior file. A failure after replacement returns
`PublishedDurabilityUncertain`; the new file may already be visible. The command
must stop all external I/O and cannot treat the error as proof of rollback.
The next writer reacquires the lock, synchronizes and rereads the file, then
commits conservative recovery. There is no second authority or uncertainty file.

Read-only inspection does not declare a live writer lost. Recovery is an explicit
application transaction under the lock. It closes abandoned active work as
`ProcessLost`, classifies planned calls as not sent and prepared calls as response
unknown, and preserves dispatch/effect evidence. Completed reviewer failures
retain their active turn and known cause until acknowledgement. Recovery never
retries a tool or manufactures a tool result.

The local filesystem tests cover real process locks, stale revisions, corruption,
atomic checkpoint ordering, and injected failures on both sides of publication.
They do not claim network-filesystem locking or hardware power-loss guarantees.
