# Provider adapters

OpenAI and Anthropic adapters prepare bounded protocol requests and invoke them
through HTTP. Active native bundles come from accepted archive observations;
closed turns use selected portable facts.

## Public entry points

Construct `ClientConfig`, then `OpenAIClient` or `AnthropicClient`. Use
`ModelClient::prepare` and `invoke` for `AssistantMessage` or `Decision`.
The service persists intent between these calls. Standalone callers own that
ordering themselves. There is no caller-maintained continuation token.

## Drive

After Launch and Doctor, set `COLA_VERIFY_FEATURE=provider-adapters` and run
`"$COLA_VERIFY_ADAPTERS" models:: --nocapture --test-threads=1` in the capture
block. Read `tests/adapters/models/{client,openai,anthropic}.rs` and `support.rs`.

The group captures native paths, headers, strict output schemas, arbitrary model
settings and tools. It rejects malformed calls, refusals, unsupported output,
duplicate active IDs and incompatible active targets. Native bundle roundtrips
preserve opaque items; fresh closed history omits audit metadata. Required-turn
selection and full protocol framing are measured before HTTP.

Receipt cases retain success and failure bytes and only supplied metadata.
Status, redirect, deadline, chunked-size and partial-body tests assert safe typed
errors and no automatic retries. Credential cases cover literal, escaped and
nested JSON echoes, including truncated captures that remain committable.

## Limits

Fixtures bind loopback with dummy credentials. They do not establish live auth,
quotas, availability or current vendor schemas. Live model acceptance requires a separate configured journey.
A DeepSeek run does not establish live Anthropic service compatibility.
