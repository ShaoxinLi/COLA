# Domain contracts

`cola::domain` owns checked values, conversation transitions, and model input.
It has no dependency on the application or adapters and performs no external I/O.
All public types are re-exported from `cola::domain`.

## Module map

| Source | Responsibility |
| --- | --- |
| [`value.rs`](../src/domain/value.rs) | Text, identities, tool names, and value errors. |
| [`message.rs`](../src/domain/message.rs) | User, assistant, and tool messages. |
| [`tool.rs`](../src/domain/tool.rs) | Tool contracts, definitions, calls, batches, and results. |
| [`conversation.rs`](../src/domain/conversation.rs) | Session transitions, execution facts, snapshots, and recovery. |
| [`context.rs`](../src/domain/context.rs) | Checked exchanges and execution notices. |
| [`roles.rs`](../src/domain/roles.rs) | Assistant and reviewer context assembly, and decisions. |

## Values and messages

Constructors preserve valid input, including whitespace. `NonEmptyText`,
`SessionId`, and `ToolCallId` reject blank strings. `TurnId` is a nonzero `u64`.
`ToolName` matches `[A-Za-z_][A-Za-z0-9_-]*` without a length limit.

An assistant message contains either a nonblank terminal answer or a nonempty
batch of tool calls with distinct IDs. A batch can include a preamble.
Whitespace-only preambles become `None`.

## Tool contracts

`Tool::execute` synchronously accepts JSON object arguments and returns
`ToolOutcome`. Implementations validate their own fields and can block the caller.
Empty success output is valid. Tool failure is an ordinary recorded return.

`ToolDefinition` describes a tool without an executable implementation.
`InputSchema` checks only that the schema is an object with `"type": "object"`.
Neither it nor `ToolCall` validates argument conformance to that schema.

The caller selects an implementation and records a `ToolResult` with the original
call ID. `Session` and `Context` do not resolve tools or check requested names
against a catalog. Call IDs correlate facts but do not guarantee exactly-once
effects. See the [host tool adapters](tools.md).

## Session transitions

`Session` owns closed turns and at most one active turn. Each turn contains one
user message and ordered assistant steps. Each requested call has an execution
record. Getters expose immutable facts.

Rejected transitions leave the session unchanged. Active operations require the
current `TurnId`. `CallAddress { session, turn, step, native }` identifies a tool
execution. Its one-based step is validated at every operation boundary.
`call_address(native)` obtains an address for the current batch.

| Operation | Required state | Recorded change |
| --- | --- | --- |
| `start_turn(user)` | No active turn. | Allocates consecutive IDs starting at one and stores the user message. |
| `record_assistant(id, message)` | `AskModel`, with IDs distinct inside the new batch. | Adds a step. An answer closes the turn as `Completed`. |
| `start_tool(address)` | The next call is `NotStarted`. | Records `Started` and returns the call before external I/O. |
| `record_tool_result(address, outcome)` | The matching call is `Started`. | Records `Returned(outcome)`, including tool failures. |
| `interrupt(id, reason)` | An active turn. | Closes it as `Interrupted` without inventing results. |
| `fail(id, failure)` | An active turn. | Closes it as `Failed` without inventing results. |
| `resolve_effect(address, resolution)` | A closed call remains `Started`. | Adds effect evidence without changing execution state. |

`next_action()` returns `AwaitUser` when no turn is active, `AskModel` when no
call is pending, `Execute(call_id)` for the next unstarted call, or `AwaitTool`
for a started call without a return.

### Unknown effects and resolutions

`NotStarted` means undispatched under this protocol. `Started` records dispatch
intent. A crash before actual tool I/O can therefore leave an unknown effect.
Closing a turn neither cancels external work nor undoes effects.

`EffectResolution` records whether the effect completed or was not performed,
its source, and nonblank evidence. The caller verifies that evidence. Repeating
an identical resolution succeeds. A conflicting resolution fails. Resolution
preserves `Started`, creates no result, and never reopens a turn or schedules a retry.

## Model input objects

`Context<O>` associates input with expected output type `O`. It contains nonblank
instructions, ordered `ContextExchange` values, and a catalog with unique tool
names. Native call IDs can repeat across steps and turns. Turn IDs strictly
increase. Empty or entirely closed history is valid.

Each exchange starts with one user message and contains complete tool batches
with returns in declaration order. Orphan results and unresolved batches are rejected. A completed exchange ends in an answer.
An active, interrupted, or failed exchange has no answer. Only the last exchange
can be active, and it must await the model.

### History projections

| Projection | Contents and limits |
| --- | --- |
| `Session::messages()` | All recorded messages, including unresolved requests and partial results. |
| `Session::context_exchanges()` | All turns as checked exchanges. Rejects an active unresolved batch. |
| `Session::closed_context_exchanges()` | Closed turns only. Available while an active batch is pending. |
| `Context::messages()` | Flat messages, excluding notices. |
| `Context::execution_notices()` | Notices across all exchanges. |

Closed partial batches become `Undispatched`, `UnknownEffect`, and
`ConfirmedResult` notices instead of incomplete messages. Complete batches stay
as messages. `TurnClosed` preserves interruption or failure. Notices describe
facts and never fabricate tool returns. `Context` does not select or fit history.

## Context assembly contracts

`Assistant::context` builds `Context<AssistantMessage>` from immutable session
facts. `ToolCallReviewer::context` also receives the exact call and builds
`Context<Decision>`. Neither trait invokes a model or tool.

The reviewer selects history and policy instructions. The application adds the
exact candidate address and complete active-turn facts as a typed review subject.
The adapter renders it as user-role data separate from policy instructions.
`closed_context_exchanges` remains available during a pending batch.
`Decision` records allowance and an optional nonblank reason. It does not itself
authorize execution, establish freshness, or verify call membership.

## Snapshots and recovery

`Session::snapshot()` exports facts without persistence. Freely constructible
snapshot types mirror the session, turn, step, call, and execution hierarchy.
Closed turns require an outcome. The active turn requires none.

`Session::restore()` validates raw values through normal transitions. It checks
turn numbering, batch-local call uniqueness, execution alignment and order, outcomes, and
resolution eligibility. Failures return `RestoreError`.

Restore preserves active work exactly. The application commits explicit
`Interrupted(ProcessLost)` recovery for abandoned work under its write lock
without executing calls. A known completed reviewer failure instead remains
active until acknowledgement closes it with its original cause.
Closed turns retain partial batches and resolution evidence. Validation checks
structural consistency but does not authenticate recorded effects.

Invariant-bearing domain types have no deserialization implementations.
The [session persistence reference](session-store.md) describes the storage
contract and JSONL format. Rust declarations define fields and error variants.
