# Provider adapters

Rust callers use OpenAI or Anthropic clients to generate assistant output and
review decisions, retain continuation, and receive typed safe errors.

## Sub-features

- `provider-wire` sends native paths, headers, schemas, settings, and tool catalogs.
- `provider-output` decodes checked answers, tool calls, decisions, refusals, and incomplete responses.
- `provider-history` preserves correlations, notices, and continuation across requests.
- `provider-binding` rejects changed or incomplete history before HTTP.
- `provider-errors` bounds reads and deadlines, rejects redirects, and redacts diagnostics.

## How to get to it (user POV)

Construct `cola::adapters::models::ClientConfig`, then `OpenAIClient` or
`AnthropicClient`. Call `cola::app::ModelClient::generate` for either
`AssistantMessage` or `Decision`. Carry the returned continuation with its
accepted session history. Both clients can also be supplied to `AgentService`.

## Driving it with the Rust test runner

Preconditions:

- Launch and Doctor pass. Set `COLA_VERIFY_FEATURE=provider-adapters`.
- Loopback TCP bind is available. Fixtures supply dummy secrets and native payloads.

Run `"$COLA_VERIFY_ADAPTERS" models:: --nocapture --test-threads=1` in the capture block.
Cases live in `tests/adapters/models/client.rs`, `openai.rs`, and `anthropic.rs`.

- Generate requests through `client::paths_headers_native_schemas_effort_and_defaults` and `client::decision_uses_fixed_strict_native_schema_and_no_callable_tools`. Inspect captured HTTP requests and typed outputs.
- Decode native optional metadata, stops, refusals, malformed calls, and invalid decisions through the `openai::`, `anthropic::`, and `client::` cases. Partial executable output must never escape rejected responses.
- Continue generations and completed turns through `client::cumulative_native_replay_survives_four_generations_and_completed_turns` and the receipt-binding cases. Captured request history proves replay and correlation.
- Change history, endpoint, or provider through `client::model_endpoint_provider_history_and_receipt_binding_fail_without_network`. The fixture's captured request count proves rejection occurs before HTTP.
- Exercise statuses, redirects, timeouts, and bounded reads through the corresponding `client::` cases. Assert typed causes, no automatic retries, and redacted diagnostics.

## Gotchas

- This is local HTTP compatibility proof. Live authentication, current provider schemas, quotas, and model availability remain unverified.
- Adapter tests manipulate accepted history through public domain operations. They do not prove service review ordering. Use the agent chat map too.
- Continuations are bound to exact accepted history. A JSONL reload does not recreate one.
- Timing cases test limits, not performance. Report failures with the exact assertion before interpreting a slow host.
