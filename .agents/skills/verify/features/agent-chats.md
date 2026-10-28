# Agent chats

Rust callers create sessions and send messages through `AgentService`. Each
operation acquires and reloads its archive. The service commits invocation intent,
observations, review and tool effects at separate durability gates.

## Public entry points

Construct `AgentService::new(model, store, tools, settings)`. Call `new_session`,
`chat`, `chat_with_history`, `inspect`, `list_sessions`, or `recover`.
`RoleSettings` supplies instructions, model settings and context limits.
The model port has pure `prepare` and effectful `invoke` methods. Rebuilding the
service preserves continuation from disk; active abandoned work needs recovery.

## Drive

After Launch and Doctor, set `COLA_VERIFY_FEATURE=agent-chats` and run
`"$COLA_VERIFY_APP" --nocapture --test-threads=1` in the driver's capture block.
Read `tests/app/agent_service.rs`, `context.rs`, `model_client.rs`, and
`session_store.rs` before describing their assertions.

The full group covers selective approval and denial, per-call facts, iteration
limits, all publication gates, owned uncertainty repair, separate unsaved
observations, stopped reviewers, original ID scoping, invocation provenance,
per-role context capacity, ownership exclusion and close-only idempotent recovery.
`real_local_providers_correlate_denials_and_fresh_service_uses_portable_disk_history`
combines real JSONL and both HTTP adapters with local fixtures and a tool double.
`context::` covers whole-turn selection, required history, metadata omission,
unknown effects and exact rendered byte admission.

## Inbound consumers

After the same Doctor, set `COLA_VERIFY_FEATURE=inbound-apis` and run
`"$COLA_VERIFY_ADAPTERS" api:: --nocapture --test-threads=1` in a separate capture
block. `tests/adapters/api/{cli,web}.rs` drive command streams and loopback HTTP,
strict boundaries, durable error views, recovery and real disk readback.

`cargo test --locked --test main` separately drives the built executable with
both protocol fixtures, startup configuration and cold CLI/Web continuation.
For live DeepSeek and actual host-file reads, use the opt-in command documented
in the repository README. It requires a private local configuration and creates
its own disposable sessions. Never use a user's existing archive as a fixture.

## Limits

Fault injection proves controller ordering and outcomes at the public ports.
It does not prove hardware power-loss behavior. Provider fixtures use dummy
credentials; they do not establish live authentication, quotas or availability.
