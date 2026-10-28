# Domain contracts

`cola::domain` owns checked values, accepted conversation facts and execution transitions.
It performs no external I/O and has no dependency on the application or adapters. All
public domain types are re-exported from `cola::domain`.

| Source | Responsibility |
|---|---|
| [`value.rs`](../src/domain/value.rs) | Text, turn/generation identities, call references and timestamps. |
| [`message.rs`](../src/domain/message.rs) | User, assistant and tool messages. |
| [`tool.rs`](../src/domain/tool.rs) | Tool definitions, calls, batches, results and execution port. |
| [`conversation.rs`](../src/domain/conversation.rs) | Session transitions, execution facts and faithful snapshots. |
| [`roles.rs`](../src/domain/roles.rs) | The review decision value. |

Context planning belongs to [the application](model-clients.md), separate from these facts.

## Values and identity

Constructors preserve valid contents, including whitespace. `NonEmptyText`, `SessionId`
and `ToolCallId` reject blank strings. Turn and generation IDs are nonzero. Tool names
match `[A-Za-z_][A-Za-z0-9_-]*`.

`CallRef` pairs a source generation with the provider's unchanged call string. No separate
UUID call identifier or reverse native-ID map is needed. A containing archive supplies the
session scope. A closed turn may reuse a string from an earlier turn. A native active turn
cannot contain ambiguous repeated call strings, and a batch cannot contain duplicate IDs.

Each accepted step has one authoritative `source_generation` link. Invocation metadata and
original native observations belong to the application archive, not the domain message.

## Messages and tools

An assistant message is either a nonblank terminal answer or a nonempty tool batch. A batch
can contain a preamble; a whitespace-only preamble becomes absent.

`Tool::execute` accepts JSON object arguments synchronously and returns `ToolOutcome`.
Empty success output is valid. An actual tool failure is an ordinary recorded return.
`ToolDefinition` describes a tool without executing it. `InputSchema` requires an object
schema but does not validate each argument against that schema. The application resolves
advertised names against registered tools.

## Execution state

| State | Fact |
|---|---|
| `Requested` | Review has not authorized target dispatch. |
| `Denied` | Review denied the call; the target was not executed. |
| `Approved` | A reviewer approved it, but dispatch has not started. |
| `Started` | Dispatch intent is recorded; execution or its result may be unknown. |
| `Returned` | An actual target outcome is recorded. |

Review records identify their reviewer generation and optional reason. `NextAction`
distinguishes review, execution, waiting for an outstanding target, asking the model and
waiting for the user. Denial is not an actual tool return. Its protocol projection explicitly
states controller non-execution.

Rejected transitions leave facts unchanged. Operations require the active turn and exact
call occurrence. Calls are reviewed and dispatched in declaration order. No result can
precede dispatch, no dispatched occurrence can be dispatched again, and no assistant output
can bypass an unresolved active batch. An accepted answer closes its turn as completed.

## Interruption and effect evidence

Closing a turn does not cancel external work or undo its effects. A dispatched call without
a return remains unknown. Undispatched or denied calls remain known not executed.

`EffectResolution` records a completed or not-performed disposition, its provenance and
nonblank evidence. Repeating identical evidence is idempotent. Conflicting evidence is
rejected. Reconciliation preserves the original observation, creates no synthetic actual
result, does not reopen a turn and schedules no retry.

## Snapshots and recovery

`Session::snapshot()` exports facts without performing persistence. `Session::restore()`
validates values, ordering, call/execution alignment, source identities and legal outcomes.
It preserves active work faithfully. Reading a snapshot does not declare a process lost.

Explicit application recovery acquires ownership and commits interruption and abandoned
invocation facts. The [session archive](sessions.md) validates the relationship between
domain steps, reviewer decisions and invocation evidence. Structural validation does not
authenticate an external effect.
