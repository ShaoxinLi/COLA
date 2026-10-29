# Agent chats

Rust callers drive `AgentService` through committed session records. Run the
entire app integration executable after Launch and Doctor:

```bash
"$COLA_VERIFY_APP" --nocapture --test-threads=1
```

Set `COLA_VERIFY_FEATURE=agent-chats`. Service and record journeys use real JSONL
storage and both HTTP adapters with local provider responses. Policy journeys
also exercise selection and fitting directly through their public contracts.
They require writable temporary directories and loopback TCP, but no credentials.

| Test owner | Public behavior |
| --- | --- |
| `tests/app/agent_service.rs` | Commit-before-I/O ordering, stopping after uncertain publication, denial, role/policy reconciliation, frozen instructions, growing native budgets, and effect-resolution context. |
| `tests/app/context_policy.rs` | Defaults, ordered closed history, unresolved-effect pinning and resolution, whole-turn fitting, invalid selection, required-content overflow, and renderer errors. |
| `tests/app/session_record.rs` | Durable review blocks, acknowledgement evidence/actor/conflicts, known failure preservation, planned/prepared recovery, exact invocation evidence, accepted-step provenance, frozen views, and reload consistency. |
| `tests/app/session_store.rs` | Storage-port error source preservation and the narrowly shared faulting store used for checkpoint/recovery journeys. |

The policy selection and record acknowledgement/recovery cases originally lived
in the service suite. Service orchestration retains the checks that relate a
policy or role to external I/O. Provider rendering, actual encoded-body fitting,
and archive corruption remain in `tests/adapters/models/client.rs`.
See the [policy guide](../../../../docs/context_policy.md) and
[record guide](../../../../docs/session_record.md) for module boundaries.

The domain, adapter, and executable suites add scoped receipts, cold reload,
multiple native steps, CLI/Web acknowledgement, iteration limits, and fresh
process restart. Fixtures demonstrate local compatibility and ordering, not live
provider availability or hardware power-loss durability. Never replace fixture
roots with existing user sessions.
