# Model clients

The application separates request preparation from model I/O. OpenAI Responses and
Anthropic Messages implement the same typed, object-safe application `ModelClient<O>` port.
Pure model settings, prepared requests, response evidence and safe failure values belong
to the domain; the application port owns invocation.

## Application contract

`prepare(plan, settings)` renders and fits a credential-free `PreparedRequest<O>` without
network access. `invoke(&prepared)` sends that exact request and returns `ObservedReply<O>`,
including response metadata, bounded raw evidence and a typed result. `AgentService`
commits the prepared intent before invoking it.

`Generation<O>` is complete output, refusal or incomplete generation. Rejected, malformed,
transport and HTTP failures remain typed errors. Partial executable output never escapes
a stopped or rejected response. Custom model clients can construct prepared and observed
values without a private continuation constructor.

`ModelSettings` preserves a nonblank model identifier, effort and nonzero output limit.
Model IDs remain redacted in diagnostics. There is no model-name capability catalog or
silent effort normalization.

The response receipt records status, returned model, response/request IDs, usage and finish
metadata when supplied. Missing values are not inferred. Capture distinguishes no response,
complete received bytes, interrupted reads and a bounded truncated prefix. Authentication
credentials are sent in headers rather than injected into the prepared body. Credential
echoes cannot enter accepted output. Complete responses containing decoded credential echoes
are masked, including echoes assembled across stopped text blocks. Copied metadata is
scrubbed too. Partial or malformed capture redactions preserve length and leave unrelated
bytes intact. This checks the configured credential, not arbitrary application secrets or
every possible reconstruction of incomplete protocol data.

## Durable context

`ContextPlan<AssistantMessage>` derives input from a committed `SessionArchive`.
`ContextPlan<Decision>` includes the exact candidate and active execution facts. Its
`required` turn selection is an explicit policy seam for retaining earlier constraints.

Historical closed turns become field-based factual groups. They retain original user and
assistant text, tool names/arguments, actual results, controller denials, closure and unknown
effect evidence. No model summarizes them. Historical model names, usage, timestamps and
request IDs remain audit data rather than prompt prose.

The active assistant turn uses the complete native bundle since its user input. Original
provider items, opaque reasoning/thinking strings, signatures and call/result ordering stay
intact. Results use the original call ID directly. Provider, endpoint, model and relevant
thinking settings must remain compatible within that native cycle. Closed portable history
allows model changes between turns.

## Selection and limits

`ContextPlan::fit` retains required groups and the newest optional whole-turn suffix that
fits. Selected groups stay chronological. The manifest records included, omitted and
required turn IDs, source revision and policy identity. One factual omission notice tells
the model when earlier turns are absent.

The actual rendered body is measured, including instructions, tool catalog and native
protocol data. Mandatory and required groups are never silently split or dropped. If they
cannot fit, preparation returns `ContextCapacity` before HTTP. Assistant and reviewer limits
are independent; reviewer input has no callable tools.

Request bytes are an exact measurement. Token use is a labeled UTF-8 byte estimate, not a
provider tokenizer. The configured input allowance reserves output space. Opaque reasoning
and vendor token accounting can still produce provider capacity rejection. Such failures
are recorded; clients do not retry by changing the request.

## HTTP adapters

`ClientConfig::new(base_url, api_key, timeout, max_response_bytes)` takes explicit credentials
and transport limits. Both clients append their native route to the configured base path.
The base requires HTTP or HTTPS, a host, and no user information, query or fragment.
Redirects, environment proxies and automatic retries are disabled. A deadline covers the
whole exchange, including body reads. The byte cap also applies to chunked responses.

| Setting | OpenAI | Anthropic |
|---|---|---|
| Route | `/responses` | `/messages` |
| Credentials | Bearer header | `x-api-key` header |
| Output limit | `max_output_tokens` | `max_tokens` |
| Assistant tools | Function schemas with `strict: false` | Tools with `input_schema` |
| Reviewer format | JSON schema in `text.format` | JSON schema in `output_config.format` |
| Native evidence | Original output items, including encrypted reasoning | Original content blocks, including signed/redacted thinking |

Default effort is omitted. Off sends OpenAI `none` or disables Anthropic thinking. Other
efforts preserve their requested lowercase spelling. Providers may reject unsupported
settings; that failure is recorded instead of silently choosing different settings.

Non-success responses retain numeric status. Authentication, rate limits, invalid settings,
provider failures, deadlines, malformed data and oversize responses have distinct internal
categories. Public error formatting excludes remote response bodies, credentials and URLs.
Raw evidence is available through the durable archive rather than public error messages.

## Verification

Local HTTP fixtures check actual rendering, decoding, statuses, limits and native fidelity.
They do not establish live provider acceptance. Use a separate private configuration
when checking a live provider, and keep credentials and run artifacts outside the repository.

`ResponseCapture::Interrupted` preserves the received prefix when a body read
fails. It differs from a configured size-limit truncation and from receiving no
response. Accepted output requires a complete captured response.
