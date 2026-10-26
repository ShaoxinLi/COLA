# Domain contracts

`cola::domain` defines checked values, conversation state, and context assembly
contracts. `Session` records facts and validates transitions. The caller invokes
models, executes tools, and saves sessions.

## Module map

All public types are re-exported from `cola::domain`.

| Source | Contracts and objects |
| --- | --- |
| [`value.rs`](../src/domain/value.rs) | Text, identities, tool names, and value errors. |
| [`message.rs`](../src/domain/message.rs) | User messages, assistant messages, and the message enum. |
| [`tool.rs`](../src/domain/tool.rs) | Tool execution, definitions, calls, batches, and results. |
| [`conversation.rs`](../src/domain/conversation.rs) | Sessions, turns, execution state, snapshots, and restoration. |
| [`context.rs`](../src/domain/context.rs) | Checked model input, exchange boundaries, and execution notices. |
| [`roles.rs`](../src/domain/roles.rs) | Assistant context assembly, review context assembly, and decisions. |

## Values and messages

Constructors preserve valid input rather than trim or normalize it, with one
exception for optional assistant preambles.

| Type | Meaning and validation |
| --- | --- |
| `NonEmptyText` | Text with at least one non-whitespace character. Preserves surrounding whitespace. |
| `SessionId`, `ToolCallId` | Opaque strings. Reject only blank values. |
| `TurnId` | Nonzero `u64`. `Session` allocates consecutive values starting at one. |
| `ToolName` | A name matching `[A-Za-z_][A-Za-z0-9_-]*`, without a length limit. |
| `UserMessage` | A checked `NonEmptyText` value. |
| `AssistantMessage::Answer` | A terminal answer containing `NonEmptyText`. |
| `AssistantMessage::ToolRequests` | A nonempty batch and an optional preamble. A whitespace-only preamble becomes `None`. |
| `Message` | `User(UserMessage)`, `Assistant(AssistantMessage)`, or `Tool(ToolResult)`. |

## Tool contracts

`Tool::execute(&self, arguments: &Map<String, Value>) -> ToolOutcome` is
synchronous and object safe. Implementations validate their own argument fields.
Execution can block the caller. Definitions describe tools without executing them.

| Type | Contents and guarantees |
| --- | --- |
| `InputSchema` | A JSON object whose `type` is exactly `"object"`. Other JSON Schema keywords and argument conformance are unchecked. |
| `ToolDefinition` | A `ToolName`, a nonblank description, and an `InputSchema`. Contains no executable implementation. |
| `ToolCall` | A `ToolCallId`, a `ToolName`, and JSON object arguments. Does not check the arguments against a schema. |
| `ToolCalls` | An ordered, nonempty batch with distinct call IDs. |
| `ToolError` | A nonblank failure description. |
| `ToolOutcome` | `Success(String)` or `Failure(ToolError)`. Empty success output is valid. |
| `ToolResult` | An outcome associated with its original call ID. |

The caller selects an implementation, executes the call, and records a
`ToolResult`. Neither `Session` nor `Context` resolves tools or checks requested
names against the catalog. Call IDs correlate facts but do not guarantee
exactly-once effects. The [tool adapters reference](tools.md) lists the concrete
host tools.

## Conversation objects

`Session` owns ordered closed turns and at most one active turn. Getters expose
immutable facts.

| Object | Owned data |
| --- | --- |
| `Session` | A `SessionId`, closed `Turn` values, and an optional `ActiveTurn`. |
| `Turn`, `ActiveTurn` | A `TurnId`, one `UserMessage`, and ordered `Step` values. Only `Turn` has a terminal `TurnOutcome`. |
| `Step` | One assistant message and one execution record per requested tool call. An answer has no execution records. |
| `ToolExecution` | A borrowed view of a call, its `ExecutionState`, and an optional `EffectResolution`. |

`TurnOutcome` distinguishes `Completed`, `Interrupted(StopReason)`, and
`Failed(TurnFailure)`. `StopReason` contains `UserCancelled`, `ProcessLost`,
`ContextCapacity`, and `IterationLimit`. `TurnFailure` contains `ModelFailure`
and `MalformedResponse`, each with nonblank text. Callers supply these labels.

### Session transitions

Every rejected transition leaves the complete session unchanged. Operations on
active work require the current `TurnId`. Call operations address the latest
assistant batch and follow its declaration order.

| Operation | Required state | Recorded change |
| --- | --- | --- |
| `start_turn(user)` | No active turn. | Allocates the next turn ID and stores the user message. |
| `record_assistant(id, message)` | `AskModel`. Call IDs must be unused throughout the session. | Adds a step. An answer closes the turn as `Completed`. |
| `start_tool(id, call_id)` | The next call is `NotStarted`. | Records `Started` and returns borrowed call data before external I/O. |
| `record_tool_result(id, result)` | The matching call is `Started`. | Records `Returned(outcome)`. Tool failures count as ordinary returns. |
| `interrupt(id, reason)` | An active turn. | Closes the turn as `Interrupted` without inventing results. |
| `fail(id, failure)` | An active turn. | Closes the turn as `Failed` without inventing results. |
| `resolve_effect(id, call_id, resolution)` | A closed call with state `Started`. | Adds evidence about its effect without changing execution state. |

`next_action()` derives the caller's next permitted action.

| `NextAction` | Meaning |
| --- | --- |
| `AwaitUser` | No active turn exists. |
| `AskModel` | The active turn has no pending tool calls. |
| `Execute(call_id)` | The next call has not started. |
| `AwaitTool` | A started call has no recorded return. |

### Unknown effects and resolutions

`NotStarted` means undispatched under this protocol. `Started` records dispatch
intent, so a crash before actual tool I/O can still leave an unknown effect.
Closing a turn neither cancels external work nor undoes effects.

`EffectResolution` stores an `EffectDisposition`, a `ResolutionSource`, and
nonblank evidence. Dispositions are `Completed` and `NotPerformed`. Sources are
`Observation`, `UserAttestation`, and `ApplicationAttestation`.

The caller supplies and verifies the evidence. Repeating an identical resolution
succeeds. A conflicting resolution fails. Resolution preserves `Started`, creates
no tool result, and never reopens the turn or schedules a retry.

## Model input objects

`Context<O>` associates checked input with expected output type `O`. It contains
nonblank instructions, ordered `ContextExchange` values, and a tool catalog.
Catalog names and call IDs are unique. Exchange turn IDs strictly increase.
Only the last exchange can be active, and that exchange must await the model.
Empty history and entirely closed history are valid.

`ContextExchange` holds a turn ID, messages, an optional outcome, and execution
notices. Its constructor enforces the following message rules.

- Each exchange starts with exactly one user message.
- Tool batches have complete returns in declaration order.
- Orphan results, unresolved batches, and repeated call IDs are rejected.
- A completed exchange ends in an assistant answer.
- An active, interrupted, or failed exchange has no terminal answer.

### History projections

| Projection | Contents and limits |
| --- | --- |
| `Session::messages()` | All recorded messages, including unresolved tool requests and partial results. |
| `Session::context_exchanges()` | All turns as checked exchanges. Rejects an active unresolved batch. |
| `Session::closed_context_exchanges()` | Closed turns only. Remains available while an active batch is pending. |
| `Context::messages()` | Flat message projection. Notices remain separate. |
| `Context::execution_notices()` | Notices across all exchanges. |

For a closed partial batch, the context projection omits the batch's messages
and preserves its facts as notices. Complete batches remain messages.

| `ExecutionNotice` | Preserved fact |
| --- | --- |
| `TurnClosed` | Interruption or failure outcome and turn ID. |
| `Undispatched` | A call that never started. |
| `UnknownEffect` | A started call without a return, including any later resolution. |
| `ConfirmedResult` | An actual result from a closed partial batch. |

Notices are facts, not fabricated tool messages. `Context` performs no history
selection, token measurement, prompt rendering, or fitting.

## Context assembly contracts

Both role traits assemble input from immutable session facts. They do not invoke
models or tools.

| Contract | Method and output |
| --- | --- |
| `Assistant` | `context(&self, session: &Session) -> Result<Context<AssistantMessage>, ContextError>` |
| `ToolCallReviewer` | `context(&self, session: &Session, call: &ToolCall) -> Result<Context<Decision>, ContextError>` |
| `Decision` | A `should_allow` boolean and an optional nonblank reason. |

A reviewer chooses history and renders the target call and relevant active-turn
facts in its instructions. `closed_context_exchanges` remains available during
a pending batch. There is no built-in lossless projection of an active review.
A decision alone does not authorize execution, establish freshness, or verify
that the call belongs to the session.

## Snapshots and recovery

`Session::snapshot()` exports in-memory data without persistence. The snapshot
objects mirror the conversation hierarchy.

| Snapshot | Fields |
| --- | --- |
| `SessionSnapshot` | `session_id`, closed `turns`, and optional `active`. |
| `TurnSnapshot` | `id`, `user`, `steps`, and `outcome`. Closed entries require an outcome. Active entries require none. |
| `StepSnapshot` | `assistant` and `executions`. |
| `AssistantSnapshot` | `Answer(String)` or `ToolRequests` with optional text and raw calls. |
| `ToolCallSnapshot` | Raw `id`, `name`, and JSON `arguments`. |
| `ExecutionSnapshot` | `call_id`, typed `state`, and optional typed `resolution`. |

`Session::restore()` validates raw values through normal transitions. It checks
turn numbering, call uniqueness, execution alignment and order, outcomes, and
resolution eligibility. Failures return `RestoreError`.

Restore closes active work as `Interrupted(ProcessLost)` without executing calls.
Closed turns retain all facts, including partial batches and resolution evidence.

Invariant-bearing domain types do not implement deserialization. Restoration
checks structural consistency but does not authenticate recorded effects.
The [session persistence reference](session-store.md) describes the application
port and the private JSONL representation.

## Errors

| Error | Rejected input or operation |
| --- | --- |
| `ValueError` | Blank values, zero turn IDs, invalid tool names, empty batches or duplicate call IDs, non-object arguments, and invalid schema roots. |
| `SessionError` | Invalid turn selection, exhausted turn IDs, unavailable actions, unknown calls, wrong execution order, repeated starts or returns, duplicate IDs, and ineligible or conflicting resolutions. |
| `ContextError` | Missing user input, invalid message or turn order, unresolved batches, mismatched results, duplicate IDs or tool names, inconsistent outcomes, and a nonfinal active exchange. |
| `RestoreError` | `InvalidValue(ValueError)`, `InvalidTransition(SessionError)`, or `InvalidSnapshot` with a structural failure description. |
