# Conversation contracts

Rust callers construct checked values, record conversation facts, recover
interrupted work, and build typed context with their own assistant and reviewer.

## Sub-features

- `conversation-values` validates identities, schemas, messages, and decisions.
- `conversation-turns` records batches and results, closes turns, and rejects invalid transitions atomically.
- `conversation-recover` preserves dispatch intent and resolution evidence across snapshot restoration.
- `conversation-context` projects valid exchanges and reports unresolved or inconsistent history.
- `conversation-extension` supports caller-defined roles and tools through public traits.

## How to get to it (user POV)

Import `cola::domain`. Construct `Session` and checked messages, then call
`start_turn`, `record_assistant`, `start_tool`, and `record_tool_result`.
Call `interrupt`, `resolve_effect`, `snapshot`, and `restore` for recovery.
Build `Context` or implement `Assistant`, `ToolCallReviewer`, and `Tool`.

## Driving it with the Rust test runner

Preconditions:

- Launch and Doctor pass. Set `COLA_VERIFY_FEATURE=conversation-contracts`.
- Tests construct every value in memory. No seed files or credentials are needed.

Run `"$COLA_VERIFY_DOMAIN" --nocapture --test-threads=1` in the capture block.
Cases live in `tests/domain/` and share public constructors in `tests/domain.rs`.

- Construct checked values, messages, schemas, tools, and decisions through `value::`, `message::`, `tool::`, and `roles::`. Assert accepted values and rejected inputs.
- Record answers and multiple tool batches through `conversation::`. Compare public messages, outcomes, IDs, and unchanged snapshots after rejected transitions.
- Interrupt partial dispatch and reconcile effects through the cancellation, restoration, and reconciliation cases. Assert retained started calls, provenance, evidence, and no fabricated results.
- Build complete and closed context through `context::`. Invalid exchange order and unresolved batches fail without losing facts.
- Implement caller-owned roles through `roles::external_roles_assemble_typed_context_through_trait_objects` and a tool through `tool::external_tool_executes_arguments_and_records_success_or_failure`. Trait-object outputs prove external implementation works.

## Gotchas

- Domain transitions are the library user's own operations. They perform no model, filesystem, or subprocess I/O.
- Recording `Started` proves intent. It does not prove that a tool executed.
- Recovery closes active work as `ProcessLost`. Resolution adds evidence without reopening the old turn.
- Passing domain tests does not prove adapter effects. Use the relevant adapter map for disk, process, or HTTP behavior.
