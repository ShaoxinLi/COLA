# Model clients

The application layer defines typed, synchronous model generation. The adapter
layer implements that contract for OpenAI Responses and Anthropic Messages.
The caller owns context construction, session changes, and tool execution.

## Application contracts

[`ModelClient<O>`](../src/app/model_client.rs) defines synchronous, object-safe
`generate(context, settings, continuation)`. Its output type `O` binds the context,
client, and continuation. Implementations can return complete output without a
continuation. Provider configuration belongs to the adapters.

### Settings and outcomes

| Type | Contract |
| --- | --- |
| `ModelId` | An arbitrary nonblank string. Preserves its contents and redacts `Debug` output. |
| `ModelSettings` | A `model_id`, an `Effort`, and nonzero `max_output_tokens`. |
| `Effort` | `Default`, `Off`, `Low`, `Medium`, `High`, `Xhigh`, or `Max`. |
| `Generation::Complete` | Checked output and an optional replacement continuation. |
| `Generation::Refused` | No output, with an optional nonblank reason. |
| `Generation::Incomplete` | No output, with `IncompleteReason::OutputLimit` or `Other`. |
| `Continuation<O>` | Opaque, immutable, cloneable adapter state associated with output type `O`. |
| `ModelError` | A fixed `ModelErrorKind` and optional numeric HTTP status. |

`ModelError` diagnostics exclude provider messages, URLs, credentials, request
bodies, and underlying error sources. `kind()` and `http_status()` expose the
safe classification.

| Boundary | `ModelErrorKind` values |
| --- | --- |
| Configuration and input | `InvalidConfig`, `InvalidSettings`, `UnsupportedInput`. |
| Continuation | `MissingContinuation`, `ContinuationMismatch`. |
| HTTP exchange | `Transport`, `Timeout`, `Http`, `Authentication`, `RateLimited`, `ProviderFailure`, `ResponseTooLarge`. |
| Output | `MalformedResponse`, `UnsupportedResponse`. |

### Continuation ownership

A continuation is native model history, not a session snapshot. The caller
publishes the replacement token only after accepting the exact returned output
into `Session`. Errors, refusals, incomplete responses, and rejected output leave
the previous token available for reuse.

The port defines no token serialization or reconstruction.

## Adapter implementations

`cola::adapters::models` exports `OpenAIClient`, `AnthropicClient`, and
`ClientConfig`. Both clients implement `ModelClient<AssistantMessage>` and
`ModelClient<Decision>`. Their implementation modules are private.

| Source | Responsibility |
| --- | --- |
| [`openai.rs`](../src/adapters/models/openai.rs) | `OpenAIClient`, Responses request rendering, and native response decoding. |
| [`anthropic.rs`](../src/adapters/models/anthropic.rs) | `AnthropicClient`, Messages request rendering, and native response decoding. |
| [`client.rs`](../src/adapters/models/client.rs) | `ClientConfig`, HTTP transport, shared invocation, checked output conversion, native call correlation, continuation checks, and factual history rendering. |

### Explicit configuration and transport

`ClientConfig::new(base_url, api_key, timeout, max_response_bytes)` validates
connection settings. Both client constructors accept a `ClientConfig`.
Credentials are explicit.

The base URL requires HTTP or HTTPS and a host. User information, query strings,
and fragments are rejected. The key must be nonblank and valid as an HTTP header.
The timeout must be positive and fit in `u64` milliseconds. The byte cap uses
`NonZeroUsize` and rejects `usize::MAX`.

Base paths are retained. Keys appear only in authentication headers, and clients
redact `Debug` output. Redirects, retries, and environment proxies are disabled.
The deadline covers the whole HTTP exchange. Body reads enforce the byte cap
before JSON decoding, including chunked responses.

Non-success HTTP responses retain their status and use these classifications.

| Status | Error kind |
| --- | --- |
| `400` | `InvalidSettings` |
| `401`, `403` | `Authentication` |
| `429` | `RateLimited` |
| `500` through `599` | `ProviderFailure` |
| Other non-success statuses | `Http` |

### Provider requests

Requests preserve model IDs, efforts, output limits, and schemas without
capability inference or normalization. Provider rejection returns a typed error.

| Setting | OpenAI | Anthropic |
| --- | --- | --- |
| Route appended to base path | `/responses` | `/messages` |
| Authentication | `Authorization: Bearer ...`. | `x-api-key`, with `anthropic-version: 2026-10-21`. |
| Output limit | `max_output_tokens`. | `max_tokens`. |
| Assistant tools | Function schemas with `strict: false`. | Tools with `input_schema`. |
| Decision format | `text.format` with a strict JSON schema. | `output_config.format` with a JSON schema. |
| Native history | `store: false` and `reasoning.encrypted_content` requested. | Thinking, signatures, and redacted thinking retained when returned. |

| `Effort` | OpenAI | Anthropic |
| --- | --- | --- |
| `Default` | Omit reasoning effort. | Omit effort and thinking. |
| `Off` | `reasoning.effort = "none"`. | `thinking.type = "disabled"`. Omit effort. |
| `Low`, `Medium`, `High`, `Xhigh`, `Max` | Exact lowercase `reasoning.effort`. | Exact lowercase `output_config.effort`. Omit thinking. |

### Checked outputs

Assistant generation requires an active last exchange. Completed text becomes
an answer; tool calls become a nonempty checked batch. Call names must be offered
in the current catalog. Arguments must be JSON objects. Native call IDs must be
nonblank and unique across the supplied continuation. Domain call IDs are fresh
UUID-based values independent of native IDs.

Decision requests contain descriptive history and a descriptive catalog, with no
callable tools. Empty review history receives a neutral user prompt. A decision
requires exactly `should_allow: boolean` and `reason: string | null`. A string
reason must be nonblank. Complete decisions return no continuation.

Completion metadata takes precedence over content. Refusals and incomplete
responses expose no partial executable output. Anthropic's
`model_context_window_exceeded` maps to `Incomplete(Other)`. Unknown native items
fail with `UnsupportedResponse`; malformed supported items fail with
`MalformedResponse`.

OpenAI reasoning and function-call items permit omitted or null optional status
fields. A function-call item ID may also be omitted or null. An explicit pending
or incomplete item status prevents complete output.

### Native replay

Every complete assistant response returns a cumulative replacement continuation,
including after a final answer. Supported native items retain their original
fields and replay once at their recorded positions.

The token binds the provider, normalized endpoint, model ID, and all prior
exchange boundaries, messages, outcomes, and notices. It also binds the returned
assistant message and native call correlations.

Instructions, the tool catalog, effort, token limit, credentials, and transport
limits can change. Existing history cannot change.

Replay accepts the exact assistant output followed by its ordered actual tool
returns, or an accepted answer that completes its exchange. A complete tool batch
can close as interrupted or failed with its exact closure notice. Later exchanges
can add user input and closed turns without assistant output.

Unaccepted assistant output, edited or omitted history, partial native batches,
and mismatched tokens fail before HTTP. Execution notices remain descriptive
facts and never become invented native tool returns.

### Recovery and limits

JSONL session recovery does not restore native continuations. Retained tool-request
messages without a token fail with `MissingContinuation`, including closed,
complete batches. Closed partial batches projected entirely as notices can form
a factual baseline. Historical plain answers can also form a new baseline, with
no claim that native reasoning was restored.

Tokens have no durable format or hidden cache and can grow with conversation
length. These adapters provide no streaming, asynchronous calls, history fitting,
model discovery, or agent loop.

## Test scope

Model tests use local HTTP fixtures for both providers. They verify requests,
checked output, replay, error redaction, deadlines, redirects, and byte caps.
They do not establish live-provider acceptance of a model or schema.
