# Sessions

A `SessionArchive` is the source of truth for committed conversation facts, model
invocations and native evidence. A new process can rebuild its next request from that
archive. There is no separate memory-only continuation cache.

## Application contract

`SessionStore` supports sorted listing, read-only inspection, creation and exclusive
acquisition. An acquired `SessionTransaction` exposes its confirmed archive, commits an
`ArchiveChange`, and resolves uncertain publication. Custom stores implement the same
contract without depending on provider wire types.

`SessionArchive` owns conversation facts, invocation evidence and legal transitions in the
domain. Its public snapshot and checked restoration API are independent of a file format.
`ArchiveChange` captures a checked base and candidate. A custom store can persist these
facts using its own representation. The JSONL adapter alone owns encoded bytes and digests.

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

## Context and call identity

A tool occurrence uses `CallRef { generation, call_id }`. The ID is the original opaque
provider string. Generation 7 with `native-read` identifies one occurrence, and its provider
result also uses `native-read`. There is no random replacement or reverse mapping. A later
closed turn can reuse that string. Duplicate IDs in one active native bundle are rejected.

### Selected context

Closed turns become portable factual groups. A group includes the original user
request, assistant statements, tool names and arguments, actual results, denials,
unknown effects and closure reason. Model names, usage, audit timestamps and
native IDs are omitted from this historical prose.

For example, stored user text asking to read `config.txt`, tool arguments naming
that file, and an actual return containing `port=8080` become a fixed record of
those fields. A later question about the port receives that record. No model
extracts or invents the record, and the file need not still exist.

The active assistant turn keeps its entire compatible native response bundle
and ordered tool outcomes. Native reasoning items and signatures remain available
where the provider protocol needs them. The reviewer receives the exact proposed
call, current user request and execution facts, with no callable tools.

Preparation measures the actual serialized request, including instructions,
tools and protocol framing. It retains required turns and the newest contiguous
suffix of optional whole turns that fits. It preserves chronological order and
adds an omission notice. `chat_with_history` lets library callers require older
turns. It does not automatically discover lasting preferences or semantic
relevance. An oversized mandatory bundle or required turn fails before HTTP.

The executable reserves `max_output_tokens` from each role's configured context
allowance. Input tokens are estimated as UTF-8 bytes divided by three, rounded
up; request bytes are measured exactly. The estimate is recorded and makes no
claim to match every provider tokenizer. There are no hidden token-count requests
or automatic history-changing retries.

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

## Ownership by layer

The domain owns conversation and invocation facts, archive invariants and deterministic
context selection. It has no dependency on application ports or adapter wire formats.
The application owns the model and store ports, and sequences durable commits and effects.
Adapters own provider HTTP, credential handling, canonical JSONL and filesystem publication.

## Tradeoffs and verification

Complete snapshots rewrite growing archives while retaining atomic publication. An
append-only journal would need separate framing, reduction and torn-tail repair contracts.
Recency selection and a byte-based token estimate are explicit limits, not semantic search
or a guarantee about every provider tokenizer.

Public integration tests cover both provider protocols with local HTTP fixtures, context
rules, custom stores, transaction failures and explicit recovery. Executable tests exercise
cold CLI and Web continuation. Live-provider acceptance requires a separate configured run;
local fixtures alone do not establish it.
