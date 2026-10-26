# Domain contracts

This document describes the implemented `cola::domain` library. It records
conversation facts and validates their order. Model clients, tool executors,
persistence adapters, and the application loop are outside this library.

The [domain example](../examples/read_file.rs) supplies model and tool output
manually. It performs no file operation.

## Tool execution contracts

`Tool` defines executable behavior. Implementations remain outside the domain
library. Its single method borrows the JSON object stored by `ToolCall`.

```rust
fn execute(&self, arguments: &serde_json::Map<String, serde_json::Value>) -> ToolOutcome;
```

Execution is synchronous and can block the caller. Each implementation validates
its tool-specific argument fields. `ToolCall` guarantees only an object root,
not conformance to `InputSchema`. Ordinary failures return `ToolOutcome::Failure`.
`ToolOutcome::Success` accepts an empty string.

The caller selects the implementation, passes `call.arguments()` to `execute`,
and wraps the outcome in `ToolResult` with the original `call.id()`.
The caller records that result through `Session::record_tool_result`.
`Session` records execution state without invoking `Tool`.

## Conversation contracts

`Session` owns closed `Turn` values and at most one `ActiveTurn`. Both use the
same private turn record. A step stores its assistant message once and has one
execution slot per requested call. Public getters expose immutable facts.

`start_turn` allocates consecutive `TurnId` values starting at one. It rejects
a new input while a turn is active. Each turn starts with one user message and
can contain several assistant tool-request batches before its final answer.
`record_assistant` accepts output only when `next_action` is `AskModel`. An answer
closes the turn as `Completed`. A tool request batch must be nonempty, with
distinct call IDs that are unused throughout the session. The entire batch is
checked before mutation.

`record_assistant`, `start_tool`, `record_tool_result`, `interrupt`, and `fail`
require the supplied `TurnId` to identify the current active turn. Call operations
target the latest assistant tool-request batch.

`next_action` reports `AwaitUser` when no turn is active, `AskModel` when the
active turn is ready for an assistant message, `Execute(call_id)` for the next
unstarted call, or `AwaitTool` while a started call has no result.

`Session` accepts call starts and results sequentially in declaration order.
`start_tool` records `Started` before returning the call data. It does not execute
the tool. `next_action` then reports `AwaitTool`, so the same call cannot be
dispatched again through this protocol. `record_tool_result`
accepts one return for the matching started call. A tool failure is a normal
return, and an empty success output is valid. After the last return, the next
action is `AskModel`.

`interrupt` and `fail` close the turn without inventing results. `NotStarted`
means undispatched under this protocol. A `Started` call without a return has an
unknown effect after closure. A crash between `start_tool` and actual I/O can
therefore conservatively leave an unknown effect. These methods change domain
state only. They do not cancel external work or undo tool effects. Closed turns
reject further assistant messages, tool starts, and tool results.
Reasons such as `ContextCapacity` and `IterationLimit` are caller-supplied labels.
The library does not detect those limits.

`resolve_effect` adds a disposition, a source, and nonblank evidence to a closed
unknown call. Sources distinguish observation, user attestation, and application
attestation. The caller supplies these facts. The library does not verify the
evidence. The original execution stays `Started`, and no tool result is created.
Repeating an identical resolution succeeds without mutation. A different
resolution is rejected. Resolution never reopens a turn or schedules a retry.
Correlation IDs do not guarantee exactly-once effects.

Every rejected Session transition leaves the complete session unchanged.

`Session::messages` exposes the recorded history, which can contain unresolved
tool requests. `Session::context_exchanges` provides a separate projection that
omits incomplete tool-message sequences and preserves their facts as notices.

## Model input contracts

`Context` contains caller-supplied nonblank instructions, checked `ContextExchange`
values, factual `ExecutionNotice` values, and a caller-supplied tool catalog. Catalog
names and call IDs must be unique. Exchanges have strictly increasing turn IDs.
Only the last exchange can be active, and it must await the model.
An empty history or a history containing only closed exchanges is also valid.

Each exchange keeps its turn boundary and outcome. This allows interrupted
history to end after a complete tool batch without requiring a fabricated
assistant answer. `ContextExchange::new` rejects orphan returns, incorrect call
order, unresolved batches, repeated call IDs, and inconsistent outcomes.
Each exchange starts with one user message. Completed exchanges end in an
assistant answer; active, interrupted, and failed exchanges cannot contain a
terminal answer.

`Session::context_exchanges` projects every turn. Complete batches remain
messages. For a closed partial batch, confirmed results, undispatched calls, and
unknown effects become factual notices. An `UnknownEffect` notice carries any
reconciliation in its `resolution` field. Closure notices carry interruption or
failure reasons. These notices are domain-derived status and are not tool
messages. The original assistant message and partial results remain available
through the Session history getters.

An active unresolved batch cannot become Context. The caller must finish or
close it first. Context performs no history selection, token measurement, prompt
rendering, or fitting. `Context::messages` exposes only the flat message
projection. Read `execution_notices` or each exchange's `notices` as well to retain
closure reasons, unknown effects, and reconciliations.
Neither Session nor Context checks whether requested tool names appear in the
catalog. Tool definitions do not bind or resolve executable implementations.

## Value and recovery contracts

`NonEmptyText` rejects whitespace-only text and otherwise preserves the original
text, including leading and trailing whitespace. Session and call identities
reject only blank values. `ToolName` uses
`[A-Za-z_][A-Za-z0-9_-]*` with no length limit. Whitespace-only optional assistant
preambles normalize to `None`. Tool arguments must be JSON objects.

`InputSchema` checks only that its descriptor is a JSON object whose `type` is
exactly `"object"`. It does not compile JSON Schema, validate other keywords,
resolve references, or validate tool arguments. `ToolDefinition` contains only
model-visible name, description, and schema data.

`Session::snapshot` exports explicit DTO values without persisting anything.
Snapshot identities, user and assistant text, call names, and argument roots are
raw boundary input. `Session::restore` checks them and replays the normal
transitions. It also checks consecutive turn IDs starting at one, call uniqueness,
slot alignment, execution order, outcomes, and resolution eligibility. Invalid
snapshots return `RestoreError`.
Invariant-bearing domain types do not implement deserialization.
Snapshot export and restoration operate on in-memory values. They provide no
serialization format, storage, or crash-safe write protocol.
Restore checks structural consistency. It does not authenticate a snapshot's
source or verify whether its recorded tool outcomes actually occurred.

Restore always closes formerly active work as `Interrupted(ProcessLost)`. It
preserves partial facts and never resumes or re-executes a call. A closed snapshot
roundtrip preserves all facts, including reconciliation evidence.
