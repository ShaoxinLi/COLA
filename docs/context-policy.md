# Context policy

[`context_policy.rs`](../src/app/context_policy.rs) owns the public
`ContextPolicy`, `RecentContextPolicy`, and `ContextSelection` contracts.
It selects closed-turn IDs and fits them to a request-byte budget. It does not
render provider messages, write sessions, or perform model I/O.

## Selection and required history

`ContextPolicy::select(&Session)` returns a `ContextSelection` with ordered
`closed_turns`, `mandatory_turns`, and `max_request_bytes`.
`RecentContextPolicy` defaults to eight optional closed turns and 64 KiB.
It includes every closed turn containing a started execution without an effect
resolution, then the most recent optional turns in chronological order.
Mandatory turns do not consume the optional quota. Active-turn facts are supplied
separately. Resolving an unknown effect removes its mandatory status; it may still
be selected as optional history.

`AgentService::with_context_policy` installs a replacement policy. The service
checks its selection against the committed domain session and caller-defined role
context. A policy cannot omit an unresolved-effect turn or select a current,
future, or unknown turn. A role can omit optional history but must preserve mandatory history. The
assistant context preserves the exact current exchange; reviewer subjects carry
mandatory active-turn facts separately. These checks belong to service
orchestration because they reconcile the policy and role contracts.

## Whole-turn fitting

`ContextSelection::fit` receives a callback that measures a candidate selection.
It rejects missing mandatory IDs and duplicate or unordered closed IDs with
`InvalidSelection`. It removes the oldest optional whole turn until the measured
size fits, preserving all mandatory turns. A request exactly at the cap fits.
The callback's errors propagate. If even the required content exceeds the cap,
fitting returns `ContextSelectionTooLarge`.

Model adapters measure the complete encoded request, including instructions,
tools, settings, current input, selected history, and native dependencies.
The first assistant request freezes the fitted view in the
[session record](session-record.md). Later requests preserve that prefix and
append accepted native responses and actual tool results. The same byte cap
applies as the chain grows; native content and tool results are not truncated.
Reviewer requests fit independently without pruning the assistant view.

The byte cap is not a model-specific token count. Callers choose a cap suitable
for their configured model. Oversized assistant requests fail before HTTP;
reviewer preparation failures retain the normal review block.
See [service limits](agent-service.md#context-policy-and-limits) and
[model clients](model-clients.md#native-history) for those workflow boundaries.

## Tests and ownership

[`tests/app/context_policy.rs`](../tests/app/context_policy.rs) drives the public
selection and fitting contracts: defaults, chronological selection, active-turn
exclusion, effect pinning and resolution, whole-turn dropping, invalid selections,
required-content overflow, and measurement errors. The original pure selection
case lived in `tests/app/agent_service.rs`.

Service tests retain policy/role reconciliation and failure-before-I/O checks.
`tests/adapters/models/client.rs` retains actual encoded-body budgets and growing
native-chain cases because those assertions depend on provider rendering.
