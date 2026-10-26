# Model clients

The application layer defines typed, synchronous model generation. The adapter
layer implements that contract for OpenAI Responses and Anthropic Messages.
The caller owns context construction, session changes, and tool execution.

## Application contracts

[`src/app/model_client.rs`](../src/app/model_client.rs) defines the port and its
supporting types. It depends on domain values and has no provider configuration.

```rust
use cola::{
	app::{Continuation, Generation, ModelError, ModelSettings},
	domain::Context,
};

pub trait ModelClient<O> {
	fn generate(
		&self,
		context: &Context<O>,
		settings: &ModelSettings,
		continuation: Option<&Continuation<O>>,
	) -> Result<Generation<O>, ModelError>;
}
```

`ModelClient<O>` is object safe. The output parameter binds the context, client,
and continuation to the same output type. Fake implementations can return
complete output without a continuation.

### Settings and outcomes

The application types describe caller intent and checked results.

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

The error kinds identify the failed boundary.

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

The port does not define token serialization or reconstruction. The concrete
adapters below define their own continuation checks.

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

`ClientConfig::new(base_url, api_key, timeout, max_response_bytes)` validates and
stores connection settings. Each client constructor accepts a `ClientConfig`.
Clients do not read environment credentials.

The base URL requires HTTP or HTTPS and a host. User information, query strings,
and fragments are rejected. The key must be nonblank and valid as an HTTP header.
The timeout must be positive and fit in `u64` milliseconds. The byte cap uses
`NonZeroUsize` and rejects `usize::MAX`.

Base paths are retained. OpenAI appends `/responses`; Anthropic appends
`/messages`. Keys appear only in authentication headers. Both clients redact
`Debug` output. Redirects, automatic retries, and environment proxies are disabled.
The deadline covers the whole HTTP exchange, including body reads. Successful
bodies are capped before JSON decoding, including chunked responses.

Non-success HTTP responses retain their status and use these classifications.

| Status | Error kind |
| --- | --- |
| `400` | `InvalidSettings` |
| `401`, `403` | `Authentication` |
| `429` | `RateLimited` |
| `500` through `599` | `ProviderFailure` |
| Other non-success statuses | `Http` |

### Provider requests

Provider requests preserve the supplied model ID, effort, output limit, and tool
schemas. The adapters have no model catalog, capability inference, effort
fallback, or schema normalization.

| Setting | OpenAI | Anthropic |
| --- | --- | --- |
| Native route | Responses. | Messages. |
| Authentication | `Authorization: Bearer ...`. | `x-api-key`, with `anthropic-version: 2026-10-21`. |
| Output limit | `max_output_tokens`. | `max_tokens`. |
| Assistant tools | Function schemas with `strict: false`. | Tools with `input_schema`. |
| Decision format | `text.format` with a strict JSON schema. | `output_config.format` with a JSON schema. |
| Native history | `store: false` and `reasoning.encrypted_content` requested. | Thinking, signatures, and redacted thinking retained when returned. |

Effort has the following exact mapping.

| `Effort` | OpenAI | Anthropic |
| --- | --- | --- |
| `Default` | Omit reasoning effort. | Omit effort and thinking. |
| `Off` | `reasoning.effort = "none"`. | `thinking.type = "disabled"`. Omit effort. |
| `Low`, `Medium`, `High`, `Xhigh`, `Max` | Exact lowercase `reasoning.effort`. | Exact lowercase `output_config.effort`. Omit thinking. |

A provider can reject a model, effort, or schema. HTTP 400 returns
`InvalidSettings` without changing or retrying the request.

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

The token binds the following facts.

- The provider, normalized endpoint, and model ID.
- All previous exchange boundaries, messages, outcomes, and notices.
- The exact returned assistant message and native call correlations.

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

## Verification

Application-port tests live in `tests/app/model_client.rs`. Adapter tests live in
`tests/adapters/models`, with common cases in `client.rs` and provider cases in
`openai.rs` and `anthropic.rs`. Shared fixtures live in `tests/adapters/models/support.rs`.
Common model cases exercise both providers through local HTTP fixtures. They check request
bodies, headers, decoding, replay, safe errors, deadlines, redirects, and byte caps.
These tests do not establish live-provider acceptance of a model or schema.
