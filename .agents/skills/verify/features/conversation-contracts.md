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

Context construction is an application concern. The old domain `Context`,
`Assistant` and `ToolCallReviewer` traits have been removed. Run the full
application group from the agent-chat map for `ContextPlan` and custom model ports.

## Limits

`Started` proves committed dispatch intent, not a successful effect. Unknown
started effects remain unknown until explicit evidence resolves them. Domain
checks alone do not prove HTTP, filesystem or subprocess behavior.
