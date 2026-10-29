# Provider adapters

Run the provider group after Launch and Doctor:

```bash
"$COLA_VERIFY_ADAPTERS" models:: --nocapture --test-threads=1
```

Set `COLA_VERIFY_FEATURE=provider-adapters`. These tests drive `OpenAIClient` and
`AnthropicClient` through `AgentService`, with real HTTP capture and durable
recorders. They require loopback TCP and use dummy credentials.

- `native_chains_preserve_ids_and_raw_evidence_while_new_turns_quote_history` recreates the service and compares archived exact request bytes with bytes received over HTTP.
- `multiple_native_steps_append_exact_responses_and_correlated_results` checks all accepted native blocks through three tool steps.
- `repeated_ids_within_one_native_chain_are_adapter_rejections` checks conservative request-level uniqueness independently of domain scope.
- `every_effort_native_schema_and_auth_header_remain_provider_specific` checks headers, routes, settings, and unchanged tool schemas.
- `reviewer_schema_has_no_callable_tools_and_invalid_decisions_block` checks the strict decision schema and malformed decisions.
- `http_failures_are_audited_without_retry_and_do_not_leak_to_errors` checks status mapping, raw archive evidence, and safe errors.
- `deadline_bounded_capture_malformed_json_and_connection_errors_are_safe` checks response deadlines, partial capture, malformed JSON, and bounded chunked reads.
- `redirects_are_never_followed` checks that credentials cannot reach a redirect destination.
- `corrupt_invocation_associations_and_frozen_views_fail_read_validation` and `archive_rejects_a_successful_return_after_a_denied_review` keep raw archive mutation at the adapter boundary; public record lifecycle assertions live in `tests/app/session_record.rs`.
- The remaining decode and provider-specific cases check refusal, incomplete output, unknown native blocks, invalid calls, actual-model metadata, and usage capture.

These are local protocol fixtures. They do not prove current live schemas,
authentication, quotas, or token capacities. Timing assertions are deadline tests,
not performance benchmarks. Full-body byte limits apply before every request;
they do not infer an arbitrary model's token window.
