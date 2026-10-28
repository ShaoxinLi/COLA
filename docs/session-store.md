# Session persistence

A `SessionArchive` is the source of truth for committed conversation facts, model
invocations and native evidence. A new process can rebuild its next request from that
archive. There is no separate memory-only continuation cache.

## Application contract

`SessionStore` supports sorted listing, read-only inspection, creation and exclusive
acquisition. An acquired `SessionTransaction` exposes its confirmed archive, commits an
`ArchiveChange`, and resolves uncertain publication. Custom stores implement the same
contract without depending on provider wire types.

`SessionArchive::encode` and checked `decode` provide the canonical representation.
`ArchiveChange` exposes its candidate, encoded bytes and commit key so another store can
persist it. Its key combines the candidate's commit sequence and SHA-256 digest. Those
values identify a publication and detect stale writers; they do not identify a file format.

Inspection and listing preserve active turns. They do not infer process loss, mutate bytes,
close work or replace committed facts with a service's working copy. A missing store lists
as empty. Unknown or corrupt committed records fail rather than being silently skipped.

## Archive contents

The archive stores accepted domain facts and a separate ordered invocation ledger. Each
assistant and reviewer attempt has a generation ID, role, requested model/settings/target,
exact credential-free request, timestamp, selection manifest and budget measurement.
Observation adds returned metadata, a bounded raw response capture and its disposition.
Pending requests can later be recorded as abandoned by explicit recovery.

Accepted steps identify their source generation. The archive validates source roles,
turns, accepted observations, call/review relationships and execution ordering. It does
not turn rejected native replies into accepted steps. Actual returned outcomes, denials,
undispatched calls, unknown effects and reconciliation evidence remain distinct.

Each generation retains its own evidence once rather than another copy of a cumulative
continuation. The archive includes enough native data for the active tool loop, while
closed history is projected into selected factual groups for future turns.

## JSONL adapter

`JsonlSessionStore::new(root)` stores one snapshot per session. A session ID is an opaque
string; its filename is the lowercase SHA-256 hash of its UTF-8 bytes plus `.jsonl`.
Lock files use the same stem with `.lock`. IDs do not become filesystem paths.

A snapshot consists of a session header, ordered closed turns, an optional active turn,
and generation records. Every record is one JSON object followed by LF. There is one
current structural schema and no format-version field, migration dispatcher or legacy
compatibility writer.

Decode validates values, counts, ordering, references, legal transitions and terminal
state. Unknown fields and unreadable records are invalid. Legacy or corrupt snapshots
are preserved on rejection. The library does not delete old conversations automatically.
Use a fresh store root when adopting the changed archive contract.

## Ownership and publication

An operation acquires an exclusive OS lock on a stable sibling lock file and then loads
the latest committed archive. The lock spans the synchronous chat or recovery operation
and survives snapshot replacement. A competing cooperating writer gets a busy error.
Independent sessions do not share a lock.

Before commit, the transaction checks the destination against its confirmed base digest.
A stale or unexpected base is a conflict. Publication writes a same-directory temporary
file, synchronizes it, replaces the snapshot and synchronizes the directory.

| Result | Meaning |
|---|---|
| Successful commit | The candidate is the confirmed archive and further effects may proceed. |
| `NotPublished` | Publication did not replace the confirmed destination. |
| `Uncertain` | Replacement or synchronization may have changed what is visible. Further dispatch stops. |
| `Conflict` | Destination differs from the expected base or candidate. It is not overwritten. |

Resolution runs under ownership. If the exact candidate is present, it synchronizes the
file and directory before confirming it. If the old base remains, publication is reported
as absent. A different destination is a conflict. Reading matching bytes alone is not
proof of successful synchronization. Resolution never repeats a model request or tool.

These are cooperative local-filesystem guarantees. They are not distributed locking or
proof of arbitrary device behavior during power loss.

## Dispatch and recovery

Commit the exact model intent before HTTP and the observed reply with its accepted facts.
Commit review separately from target dispatch. Commit dispatch intent immediately before
target execution, and the actual return before any next model request.

A crash during review leaves a known-undispatched target. A crash after dispatch intent
but before result publication leaves an unknown effect, whether the target ran or not.
A response or result that was observed just before the process died may never reach disk.
The archive records incomplete knowledge rather than inventing the missing result.

Explicit `AgentService::recover` acquires ownership, abandons pending invocations and
closes active work as `ProcessLost`. It is idempotent and performs no model or tool I/O.
Unknown effects remain unknown, with any later reconciliation added as separate evidence.
See [agent operations](agent-service.md) and [domain execution facts](domain.md).
