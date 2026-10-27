# Agent service

[AgentService](../src/app/agent_service.rs) runs a user message through the model,
tool review, execution, and persistence. Keep one service alive across chat
calls so each session retains its native model continuation.

## Create and use the service

Supply a model client, session store, assistant, reviewer, and executable tools.
`Assistant` and `ToolCallReviewer` build model input. `ModelClient` makes the
actual model requests. Both existing provider clients support the required
assistant and decision outputs.

```rust
use cola::app::{AgentService, AgentSettings};
use cola::domain::{NonEmptyText, UserMessage};
use std::num::NonZeroU32;

let mut agent = AgentService::new(
	model_client,
	session_store,
	assistant,
	reviewer,
	vec![(tool_definition, Box::new(tool))],
	AgentSettings {
		assistant_model_settings,
		reviewer_model_settings,
		max_iterations: NonZeroU32::new(8).unwrap(),
	},
)?;

let session = agent.new_session()?;
let user = UserMessage::new(NonEmptyText::new("Read the configuration")?);
let messages = agent.chat(session.id(), user)?;
let sessions = agent.list_session()?;
```

The example assumes the supplied collaborators and model settings are already
configured. The two settings can choose different models and efforts on the
same provider client. Tool names must be unique. The assistant can advertise a
subset of registered tools, with matching definitions. Use a tool adapter's
`definition()` for both its assistant catalog entry and service registration.

`new_session` saves an empty session and returns a clone. `chat` returns only
the new completed turn's messages, including its user message. `list_session`
returns sessions sorted by ID, with resident facts taking precedence over
stored copies. Returned session values cannot mutate the service.

## Handle each tool call separately

The service accepts the exact assistant response, then processes its requested
calls in declaration order. Each call gets its own review.

For example, a response requests a file read and a log deletion. If the read
passes review and the deletion is denied, the read executes. The deletion
receives a result stating that it was not executed, together with the refusal
reason. The model receives both results and continues within the same turn.

A negative `Decision` is a normal `ToolResult` containing
`ToolOutcome::Failure`. It does not end the turn or become `ModelFailure`.
If the reviewer gives no reason, the result says no reason was supplied.
An ordinary tool failure also returns to the model without cancelling siblings.

The reviewer receives the current session and exact call. Its context builder
must include relevant active-turn facts, such as earlier calls' results.
`closed_context_exchanges()` remains available during a pending batch.
See the [role contracts](domain.md#context-assembly-contracts).

## Save before external work

For this service, dispatch includes the review gate. `Started` records an
attempt to handle a protected call. It does not assert that the target tool ran.

The service saves the accepted assistant response, then follows this sequence
for each call.

1. Record `Started` and save before calling the reviewer.
2. On approval, execute the registered tool. On denial, skip it and create an
   explicit non-execution result.
3. Record and save that result before processing the next call.

A failed intent save prevents both review and tool execution. A crash during
review can recover as an unknown effect even if the target never ran. This is
the existing conservative recovery rule. Denial and target failure share the
same stored outcome type; the refusal text distinguishes them.

See [session persistence](session-store.md#dispatch-checkpoints) for publication
and recovery details.

## Stop and inspect errors

`AgentError::kind()` identifies the cause. `progress()` exposes the actual
in-memory session when available. Those facts may be newer than stored data.

| Situation | Behavior |
| --- | --- |
| Review denies a call | Save its refusal result and continue. This is not a service error. |
| Reviewer cannot produce a decision | Preserve the active call, block the resident session, and return the reviewer error. No target or later sibling runs. |
| A save fails | Preserve live facts, block the session, and return the checkpoint and store error. Publication may already have happened. |
| Assistant cannot complete | Return the typed cause and persist the appropriate failed-turn state. |
| Iteration budget is exhausted | Save `Interrupted(IterationLimit)` and return progress. |

A blocked session rejects later chat calls without further I/O. The service
does not retry tools, reload over known results, or repair blocked sessions.
Inspect progress before recreating the service after resolving the failure.

`max_iterations` counts assistant generations, excluding reviews and tool
calls. A requested batch finishes before the next budget check. If the final
generation requests a denied call, its reason is saved, but the budget may
prevent another same-turn model request from reading it.

## Lifetime and restart limits

Continuations live only in memory. Recreating the service loads session facts
through normal store recovery, which closes saved active work as `ProcessLost`.
It never resumes or repeats that work. With the existing provider adapters,
complete tool history without its continuation returns `MissingContinuation`.
See [native replay and recovery](model-clients.md#recovery-and-limits).

Use one service writer per stored session. The store has no stale-write
detection across service instances. Resident sessions and continuations remain
in memory for the service's lifetime. The loop is synchronous and provides no
streaming, cancellation, automatic retries, or history fitting.
