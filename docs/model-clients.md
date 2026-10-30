# Model clients

The application defines synchronous model generation. Adapters implement it for
OpenAI Responses and Anthropic Messages. The caller owns context assembly,
session transitions, tool execution, and acceptance of continuations.

## Application contracts

[`ModelClient<O>`](../src/app/model_client.rs) defines object-safe
`generate(context, settings, continuation)`. The output type `O` associates the
context, client, and continuation. `ModelSettings` contains a nonblank `ModelId`,
an `Effort`, and nonzero `max_output_tokens`. Model IDs preserve their contents
and redact `Debug` output.

`Generation::Complete` returns checked output and an optional replacement
`Continuation<O>`. `Refused` carries an optional nonblank reason. `Incomplete`
carries `OutputLimit` or `Other`. Neither stopped outcome exposes partial output.

A continuation is opaque, immutable, cloneable native model history. The caller
accepts the exact returned output into `Session` before publishing the replacement token.
Errors, refusals, incomplete responses, and rejected output leave the previous
token available for reuse. The port defines no token serialization or recovery.

`ModelError::kind()` and `http_status()` expose a fixed error category and optional
numeric status. Diagnostics exclude provider messages, URLs, credentials,
request bodies, and underlying error sources.

## Adapter implementations

`cola::adapters::models` exports `OpenAIClient`, `AnthropicClient`, and
`ClientConfig`. Both clients support `AssistantMessage` and `Decision` outputs.
Provider request rendering and response decoding live in
[`openai.rs`](../src/adapters/models/openai.rs) and
[`anthropic.rs`](../src/adapters/models/anthropic.rs).
[`client.rs`](../src/adapters/models/client.rs) owns shared transport, checked
output conversion, native call correlation, replay, and history rendering.

### Configuration and transport

`ClientConfig::new(base_url, api_key, timeout, max_response_bytes)` validates
explicit connection settings. Both client constructors accept this configuration.

The base URL requires HTTP or HTTPS and a host. User information, queries, and
fragments are rejected. The key must be nonblank and valid as an HTTP header.
Timeouts must be positive and fit in `u64` milliseconds. The byte cap is a
`NonZeroUsize` other than `usize::MAX`.

Base paths are retained. Keys appear only in authentication headers. Client
`Debug` output is redacted. Redirects, retries, and environment proxies are
disabled. The deadline covers the whole HTTP exchange. Body reads enforce the
byte cap before JSON decoding, including chunked responses.

Non-success responses retain their HTTP status. Status `400` maps to
`InvalidSettings`, `401` and `403` to `Authentication`, `429` to `RateLimited`,
and `500` through `599` to `ProviderFailure`. Other failures map to `Http`.

### Provider requests

Requests preserve model IDs, efforts, output limits, and schemas without
capability inference or normalization. Provider rejection returns a typed error.

| Setting | OpenAI | Anthropic |
| --- | --- | --- |
| Route appended to the base path | `/responses` | `/messages` |
| Authentication | `Authorization: Bearer ...` | `x-api-key`, with `anthropic-version: 2026-10-21` |
| Output limit | `max_output_tokens` | `max_tokens` |
| Assistant tools | Function schemas with `strict: false` | Tools with `input_schema` |
| Decision format | Strict JSON schema in `text.format` | JSON schema in `output_config.format` |
| Native history | `store: false` and encrypted reasoning requested | Returned thinking, signatures, and redacted thinking retained |

| `Effort` | OpenAI | Anthropic |
| --- | --- | --- |
| `Default` | Omit reasoning effort. | Omit effort and thinking. |
| `Off` | Set `reasoning.effort` to `"none"`. | Set `thinking.type` to `"disabled"`. Omit effort. |
| `Low`, `Medium`, `High`, `Xhigh`, `Max` | Exact lowercase `reasoning.effort`. | Exact lowercase `output_config.effort`. Omit thinking. |

### Checked outputs

Assistant generation requires an active last exchange. Complete text becomes an
answer. Calls become a nonempty batch with offered names and JSON object
arguments. Native call IDs must be nonblank and unique across the continuation.
Domain call IDs are fresh UUID-based values independent of native IDs.

Decision requests contain descriptive history and a descriptive catalog without
callable tools. Empty history receives a neutral user prompt. Output contains
exactly `should_allow: boolean` and `reason: string | null`. String reasons must
be nonblank. Complete decisions return no continuation.

Completion metadata takes precedence over content. Refusals and incomplete
responses expose no partial executable output. Anthropic's
`model_context_window_exceeded` maps to `Incomplete(Other)`. Unknown native items
return `UnsupportedResponse`. Malformed supported items return `MalformedResponse`.

OpenAI reasoning and function calls allow omitted or null optional statuses.
Function-call item IDs can also be omitted or null. An explicit pending or
incomplete item status prevents complete output.

### Native replay

Every complete assistant response returns a cumulative replacement continuation,
including final answers. Native items retain their original fields and replay
once at their recorded positions.

The token binds the provider, normalized endpoint, model ID, prior exchanges,
messages, outcomes, notices, returned assistant message, and native call
correlations. Instructions, catalog, effort, token limit, credentials, and
transport limits can change. Existing history cannot change.

Replay accepts the exact assistant output followed by ordered actual tool
returns, or an accepted answer that completes its exchange. A complete batch
can close as interrupted or failed with its exact closure notice. Later exchanges
can add user input and closed turns without assistant output.

Unaccepted output, changed or omitted history, partial native batches, and
mismatched tokens fail before HTTP. Notices never become invented native returns.

### Recovery and limits

JSONL recovery does not restore native continuations. Retained tool-request
messages without a token return `MissingContinuation`, even for closed complete
batches. Closed partial batches projected entirely as notices can form a new
factual baseline. Historical plain answers can also form a baseline without
restoring native reasoning.

Tokens have no durable format or hidden cache and can grow with conversation
length. Clients provide no streaming, asynchronous calls, history fitting,
model discovery, or agent loop. `AgentService` supplies the
[application loop](agent-service.md).

Local HTTP tests cover requests, output validation, replay, error redaction,
deadlines, redirects, and byte caps. They do not establish live-provider
acceptance of a model or schema.
