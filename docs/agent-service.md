# Agent service

[`AgentService`](../src/app/agent_service.rs) runs model requests, independent tool review,
execution and durable commits. Each operation reloads its session from the store. There is
no resident session map or continuation token to retain between calls.

## Construction and operations

Supply a model client, a `SessionStore`, registered tools and `AgentSettings`. The client
implements `ModelClient<AssistantMessage>` and `ModelClient<Decision>`. Each role has model
settings, instructions and context limits. Tool names must be unique.

| Operation | Result |
|---|---|
| `new_session()` | Commits an empty archive. |
| `inspect(id)` | Reads the committed archive without changing active work. |
| `list_sessions()` | Reads committed archives sorted by ID, without a memory overlay. |
| `chat(id, user)` | Holds session ownership, runs one turn and returns its committed messages. |
| `recover(id)` | Closes interrupted active work and records abandonment without running models or tools. |

A completed session can be reopened by a new service or process. An interrupted active
session requires recovery before a new user turn. Recovering an idle session is a no-op.

## Invocation records

Every assistant and reviewer request has a session-local `GenerationId`. Before model I/O,
the service commits the exact prepared request, requested model and settings, target,
timestamp, context selection and budget measurement. An observed response is committed
with its metadata, bounded native capture and accepted, refused, incomplete, rejected or
failed outcome. Accepted assistant steps identify their source generation.

A single turn can contain several assistant generations and several reviewer generations.
Metadata belongs to those invocations rather than one model field on a whole turn. The
archive preserves rejected and unsuccessful attempts as evidence without turning them into
accepted messages. Missing served-model or usage fields remain unknown.

## Review and dispatch

Calls are handled in declaration order. A reviewer receives the current user request, the
exact candidate, prior sibling outcomes and selected historical facts. It has no callable
tools. A denial records that the target was not executed. It remains distinct from an
actual tool failure, even when the protocol conveys both through a tool-result channel.

The commit sequence is:

1. Commit the user turn and the assistant invocation intent.
2. Invoke the model and commit its observation together with any accepted output.
3. Commit each reviewer invocation intent and observed decision.
4. For an approved call, commit dispatch intent immediately before executing the target.
5. Commit the actual return before continuing to the next call or model request.

Reviewer failure leaves the target undispatched and closes the turn as failed. Approval is
not dispatch. A committed `Started` without a committed return means the effect is unknown,
including a crash just before execution. Recovery never repeats that effect automatically.

## Context selection

Closed turns become deterministic factual groups. The active assistant turn retains the
complete native bundle required for its current tool loop. No model is used to summarize
history. The planner selects whole groups and the adapter fits the serialized request.

Historical model names and usage stay in the archive rather than prompt prose. Byte limits
are exact; token figures are labeled estimates. A mandatory bundle that does not fit fails
before HTTP rather than losing required protocol items. See [model clients](model-clients.md).

## Commit failures

`AgentError::durable()` reports the last confirmed archive. `uncommitted()` is separate
working progress whose publication failed or remains uncertain. Read-only operations never
overlay that working copy onto disk facts.

A transaction holds an exclusive per-session lock. A conflicting writer receives a busy
error. Expected commit identity prevents stale snapshots from overwriting newer data.
After uncertain publication, the service stops external dispatch. Resolution compares the
exact proposed bytes and synchronizes them under ownership; merely seeing bytes on a read
is not proof of durability. See [session persistence](session-store.md).

`max_iterations` counts assistant generations, not reviews. Every call in an accepted batch
is handled before checking the next assistant-generation budget. Exhaustion commits an
`IterationLimit` interruption. There is no automatic model or tool retry.

## Runtime limits

The loop is synchronous. There is no streaming, cancellation, distributed writer lease,
automatic interrupted-turn resumption or model-generated summarization. Same-session
writers cooperate through local OS locks; unrelated sessions can proceed independently.
