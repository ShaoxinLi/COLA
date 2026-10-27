# Agent service

[`AgentService`](../src/app/agent_service.rs) runs a user message through model
requests, per-call review, tool execution, and saves. Keep the service alive
across chat calls to retain each session's native model continuation.

## Collaborators and operations

The service receives a model client, session store, assistant, reviewer,
registered tools, and `AgentSettings`. `Assistant` and `ToolCallReviewer` assemble
input. The client implements both `ModelClient<AssistantMessage>` and
`ModelClient<Decision>`, as both provider adapters do.

`AgentSettings` supplies separate assistant and reviewer model settings and a
nonzero `max_iterations`. The two settings can select different models and
efforts on the same provider client.

Tool names must be unique. The assistant can advertise a subset of registered
tools with matching definitions. A tool adapter's `definition()` supplies both
the catalog entry and the registration.

| Method | Result |
| --- | --- |
| `new_session()` | Saves an empty session and returns a clone. |
| `chat(id, user)` | Runs one turn and returns that turn's messages, including its user message. |
| `list_session()` | Returns sessions sorted by ID. Resident facts override stored copies. |

Returned values cannot mutate the service.

## Per-call review

The service accepts the exact assistant response and handles calls in declaration
order. Each call gets a separate review. If a file read is approved and a log
deletion is denied, the read executes and the deletion receives a failure result
that states it was not executed and gives the denial reason.

A denial is an ordinary `ToolOutcome::Failure`. It does not fail the turn or
cancel sibling calls. If no reason is supplied, the result says so. Ordinary
tool failures also return to the model.

The reviewer receives the current session and exact call. Its context builder
must include relevant active-turn facts, such as earlier results. See the
[context assembly contracts](domain.md#context-assembly-contracts).

## Persistence checkpoints

The service saves the accepted assistant response, then handles each call as
follows.

1. Record `Started` and save before invoking the reviewer.
2. On approval, execute the tool. On denial, create a non-execution result.
3. Record and save the result before handling the next call.

Dispatch includes review. `Started` records an attempt and does not prove that
the target tool ran. A failed intent save prevents review and execution. A crash
during review can recover as an unknown effect. Denials and tool failures share
the same outcome type and differ in their descriptions.

The [session persistence reference](session-store.md#dispatch-checkpoints)
describes the caller's checkpoint obligations.

## Failures and iteration limits

`AgentError::kind()` identifies the cause. `progress()` exposes resident session
facts when available. Those facts may be newer than stored data.

| Failure | Result |
| --- | --- |
| Reviewer cannot produce a decision | Return the error, preserve the started call, and block the session. No target or later sibling runs. |
| Save fails | Return the store error and checkpoint, preserve live facts, and block the session. Publication may already have happened. |
| Assistant cannot complete | Return the typed cause and save the failed turn. |
| Iteration limit is reached | Save `Interrupted(IterationLimit)` and return progress. |

A blocked session rejects later chats without further I/O. The service does not
retry tools, reload over known results, or repair blocked sessions. Inspect
progress before recreating the service after resolving the failure.

`max_iterations` counts assistant generations, excluding reviews and tool calls.
A requested batch finishes before the next budget check. Its saved results may
therefore receive no further model request in that turn.

## Lifetime and restart limits

Continuations live in memory. Recreating the service loads saved facts through
normal recovery, which closes active work as `ProcessLost` without repeating it.
Provider adapters reject retained complete tool history without its continuation
with `MissingContinuation`. See [recovery limits](model-clients.md#recovery-and-limits).

Use one service writer per stored session. Storage has no stale-write detection
across instances. Resident sessions and continuations remain in memory for the
service's lifetime. The loop is synchronous, with no streaming, cancellation,
automatic retries, or history fitting.
