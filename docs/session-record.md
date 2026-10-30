# Session record

[`SessionRecord`](../src/app/session_record.rs) is the application-owned aggregate
for a committed session. It contains the domain `Session`, a revision, invocation
audit, frozen turn views, and explicit review-failure acknowledgements.
Its fields are private; callers inspect them through read-only accessors.
Application transitions return successor records rather than editing committed
facts in place. `RecordError` reports invalid lifecycle state, invalid evidence,
conflicting acknowledgement, or the absence of a review failure.

## Responsibilities and authority

| Owner | Responsibility |
| --- | --- |
| Domain `Session` | Conversation transitions, tool execution facts, outcomes, and effect resolutions. |
| Application `SessionRecord` | Associations between domain facts, model invocations, evidence, views, and acknowledgements. |
| Application `AgentService` | Workflow sequencing, recovery commands, commit checkpoints, and model/tool I/O coordination. |
| `SessionStore` / `SessionWriter` | Reading records and publishing validated successors under exclusive ownership. |
| JSONL adapter | Version 2 storage DTOs, file locks, atomic replacement, and durability errors. |

The committed record is the source of facts for later commands. Model input is a
derived view, not a second authority or a resident continuation cache. Provider
payloads remain opaque versioned `ModelArtifact` envelopes in the application
record and do not enter domain types. A later process can inspect the same
record or begin a new turn with different provider settings.
See [persistence](session-store.md) for publication and format details.

## Invocation lifecycle and evidence

An invocation identifies its requested route/settings and either an assistant
turn/step or a reviewer call's complete session/turn/step/native-ID address.
Its lifecycle records these states:

1. `Planned` records intent before request preparation.
2. `Prepared` records exact nonsecret request bytes and selection metadata.
   The first assistant request also establishes the frozen `TurnView`.
3. `Finished` records response evidence and the typed outcome together with any
   accepted domain step. A preparation failure has no prepared request.
4. `Interrupted` records recovery of unfinished intent, retaining any request
   and distinguishing not sent from response unknown.

Response evidence includes capture completeness, raw bytes when available,
status, reported model identity, response ID, usage, and timing. Accepted output
requires complete response evidence. Rejected output is audited without creating
an executable step. Each accepted assistant step has one invocation provenance;
reviews refer to calls with recorded dispatch intent. Frozen views and request metadata must
agree. Restore validates these associations in addition to domain validation.
`validates_successor` checks identity, the next revision, and record validity;
the writer enforces it before publication.

## Recovery and acknowledgement

Inspection preserves active facts. The next write command commits recovery under
the session lock: planned intent becomes not sent, prepared intent becomes
response unknown, and abandoned active work closes as `ProcessLost`. Recovery
never resumes a turn, retries a tool, or invents a result. A completed reviewer
infrastructure failure retains its active turn and known cause until explicit
acknowledgement.

`review_block()` exposes the first unacknowledged blocking reviewer invocation.
An ordinary allow or deny decision does not block. Acknowledgement requires an
actual review failure and nonblank evidence. Repeating the same invocation,
evidence, and actor is idempotent; changing evidence or actor conflicts.
Acknowledgement preserves the original failure or existing process-loss outcome.
It does not resolve an unknown tool effect; that is a separate domain operation.
See [service recovery](agent-service.md#recovery-and-review-failures).

## Tests and ownership

[`tests/app/session_record.rs`](../tests/app/session_record.rs) reads records
through public service/store entry points. It covers durable reviewer blocks,
acknowledgement evidence and conflicts, original failure preservation,
planned/prepared recovery, accepted-step provenance, exact audit bytes, frozen
views, and reload consistency. The acknowledgement and reviewer recovery cases
originally lived in `tests/app/agent_service.rs`.

Service tests retain checkpoint ordering and the requirement to stop external I/O
on write failure. Their shared faulting store lives with the storage-port tests
in `tests/app/session_store.rs`. App tests explicitly reuse the existing HTTP
fixture in `tests/adapters/models/support.rs`.
Provider-wire and corrupt-archive cases remain in `tests/adapters/models/client.rs`;
that file already covered invocation associations and view validation. The
successful-return-after-denied-review corruption case moved there from service
tests so archive byte mutation has one adapter-level owner.
