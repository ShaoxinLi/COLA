# Agent chats

Rust callers drive `AgentService` through committed session documents. Run the
entire app integration executable after Launch and Doctor:

```bash
"$COLA_VERIFY_APP" --nocapture --test-threads=1
```

Set `COLA_VERIFY_FEATURE=agent-chats`. Fixtures use real JSONL storage and both
HTTP adapters with local provider responses. They require writable temporary
directories and loopback TCP, but no provider credentials.

The current public journeys are in `tests/app/agent_service.rs`.

- `every_checkpoint_failure_stops_external_io_even_after_publication` injects failures at each revision before and after publication. Request counts and target counts prove ordering.
- `reviewer_failure_is_durable_and_acknowledgement_is_idempotent_without_tool_io` recreates the service and checks block persistence, identical retry, conflict, and unknown-effect preservation.
- `acknowledgement_preserves_original_finished_failure_when_turn_is_still_active` checks direct acknowledgement without rewriting the cause to process loss.
- `planned_only_recovery_is_not_sent_and_prepared_review_can_be_acknowledged_directly` distinguishes not sent from response unknown.
- `accepted_review_is_committed_before_target_and_real_denial_is_not_a_block` checks denied execution and the saved decision.
- `replacement_policy_cannot_unpin_unknown_effects` rejects a custom policy that omits required facts.
- `growing_native_chain_hits_exact_body_budget_without_truncating_results` preserves the real result and refuses the next request before HTTP.
- `same_turn_instruction_edits_fail_before_next_network_request` checks frozen-prefix compatibility.

The domain, adapter, and executable suites add scoped receipts, cold reload,
multiple native steps, CLI/Web acknowledgement, iteration limits, and fresh
process restart. Fixtures demonstrate local compatibility and ordering, not live
provider availability or hardware power-loss durability. Never replace fixture
roots with existing user sessions.
