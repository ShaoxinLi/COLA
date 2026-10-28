# Conversation contracts

The domain records checked conversation and execution facts without external I/O.
`GenerationId` identifies an invocation; `CallRef` combines it with the original
opaque `ToolCallId`. Review, dispatch intent and actual returns are separate states.

## Public entry points

Construct `Session` and checked domain values. Use `start_turn`,
`record_assistant`, `review_tool`, `start_tool`, `record_tool_result`,
`interrupt`, `fail`, `resolve_effect`, `snapshot`, and `restore`.
Restoration faithfully preserves active work. It does not implicitly record
process loss. Explicit application recovery owns that operation.

## Drive

After Launch and Doctor, set `COLA_VERIFY_FEATURE=conversation-contracts` and run
`"$COLA_VERIFY_DOMAIN" --nocapture --test-threads=1` in the capture block.
Read `tests/domain/` and the constructors in `tests/domain.rs`.

`value::`, `message::`, `roles::`, and `tool::` cover checked identities, message
batches, schemas, decisions and externally implemented tools. `conversation::`
covers scoped IDs, approval/denial, ordered dispatch, actual outcomes, atomic
rejection, faithful restoration and resolution evidence. Invalid snapshots are
rejected without fabricating results.

`ContextPlan` is domain policy. `tests/domain/context.rs` covers whole-turn selection,
required history, metadata omission, active native evidence and exact rendered byte admission.
Archive snapshot and immutable extension cases also belong to the domain group.
Custom `ModelClient` and `SessionStore` implementations remain application-port tests.

## Limits

`Started` proves committed dispatch intent, not a successful effect. Unknown
started effects remain unknown until explicit evidence resolves them. Domain
checks alone do not prove HTTP, filesystem or subprocess behavior.
