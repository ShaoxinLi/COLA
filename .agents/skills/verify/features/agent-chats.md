# Agent chats

Rust callers create sessions and send user messages through `AgentService`.
The service reviews each proposed call, runs allowed tools, saves progress,
and returns the turn's messages.

## Sub-features

- `chat-review` preserves the proposed batch and handles each approval or denial separately.
- `chat-continue` retains native provider continuation across chats.
- `chat-limit` drains the accepted tool batch before an iteration interruption.
- `chat-block` retains live facts and blocks further I/O after save or reviewer failures.
- `chat-reject` refuses duplicate registration, mismatched tools, and reused call IDs.
- `chat-list` sorts stored sessions with resident facts taking precedence.
- `chat-port-errors` preserves checked model identifiers and the original storage error source.

## How to get to it (user POV)

Construct `cola::app::AgentService::new` with caller-supplied roles, clients,
storage, tool definitions, and settings. Call `new_session()`, `chat(id, user)`,
and `list_session()`. Keep the service alive between chats for continuation.

## Driving it with the Rust test runner

Preconditions:

- Launch and Doctor pass. Set `COLA_VERIFY_FEATURE=agent-chats`.
- Allow loopback listeners for the provider-backed cases. Fixtures supply all collaborators.

Run `"$COLA_VERIFY_APP" --nocapture --test-threads=1` in the capture block.
The cases live in `tests/app/agent_service.rs`, `tests/app/model_client.rs`,
and `tests/app/session_store.rs`.

- Create a session, chat, review all approval combinations, and chat again through `selective_review_preserves_exact_batch_and_continues_the_same_turn`. Returned messages, target invocation records, saved facts, and listing prove the result.
- Exercise tool failure and denial through `no_reason_denial_and_genuine_tool_failure_are_normal_results`. Both return ordinary tool results.
- Exhaust generations through `assistant_attempt_limit_drains_each_tool_and_saves_interruption`. The saved interruption follows every tool receipt.
- Inject checkpoint and reviewer failures through the group's failure cases. Assert blocked sessions make no subsequent I/O and keep progress accessible.
- Run the native continuation cases, including `real_providers_receive_correlated_denials_and_keep_native_continuation_across_chats`. Captured HTTP requests preserve native correlations and denied results.
- Run registration, catalog, ID reuse, missing-load, rejected-start, and listing cases. Rejections retain the existing session facts.
- Construct model IDs and storage errors through `model_client::model_ids_preserve_nonblank_values_and_redact_diagnostics` and `session_store::port_errors_preserve_the_original_source`. Checked values and source chains remain available without leaking model IDs in diagnostics.

## Gotchas

- Most service cases use in-memory store and tool doubles. The native provider cases use the real JSONL store and HTTP adapters with local provider fixtures. They reload saved sessions from disk, but their tool target is a double. Use the host tool map for real Bash and file-tool execution.
- `real_providers` means actual adapters against local HTTP fixtures. It does not mean a live provider call.
- Continuations live in memory. Rebuilding a service cannot recreate native continuation from JSONL history.
- Fixtures create sessions from scratch. Do not substitute an existing user session to satisfy a failed precondition.
