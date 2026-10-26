# Model clients

`cola::app::ModelClient<O>` synchronously generates checked domain output from
`Context<O>`. `OpenAIClient`, `AnthropicClient`, and `GoogleClient` implement the
port for `AssistantMessage` and `Decision`. Callers can use trait objects and
implement their own fakes without handling provider JSON.

## Application contract

`ModelSettings` contains a nonblank `ModelId`, an `Effort`, and a nonzero
`max_output_tokens`. Model identifiers stay out of error diagnostics.

`Generation` distinguishes these results:

- `Complete` contains the checked output and an optional typed `Continuation`.
- `Refused` and `Blocked` contain no executable output. Built-in adapters omit
  provider reason text to avoid copying sensitive response text into diagnostics.
- `Incomplete` contains `OutputLimit` or `Other`, without partial output.

`ModelError::kind()` identifies invalid configuration or settings, unsupported
models, efforts, inputs, responses, missing or mismatched continuation,
transport failures, timeouts, HTTP failures, and oversized or malformed responses.
Authentication failures, rate limits, and server failures have separate kinds.
`ModelError` has no raw error source. Its `Debug` and `Display` contain only a
safe kind. Configurations, clients, model identifiers, and continuation tokens
also redact their contents in `Debug`.

The caller accepts `AssistantMessage` through `Session::record_assistant` before
publishing the returned continuation. Tool execution remains the caller's job.
A `Decision` is review data and grants no execution authority.

## Connection configuration

`ClientConfig::new(base_url, api_key, timeout, max_response_bytes)` owns connection
settings. It rejects non-HTTP URLs, URL credentials, queries, fragments, empty or
whitespace-only keys, invalid header values, zero timeouts, excessively large
timeouts, and a response limit of `usize::MAX`. The limit uses `NonZeroUsize`.
A trailing slash does not change the appended route. Custom base paths remain.

The adapters append these routes to the supplied base path:

| Adapter | Typical base URL | Appended route | Authentication |
| --- | --- | --- | --- |
| `OpenAIClient` | `https://api.openai.com/v1` | `/responses` | Bearer header |
| `AnthropicClient` | `https://api.anthropic.com/v1` | `/messages` | `x-api-key` header |
| `GoogleClient` | `https://generativelanguage.googleapis.com/v1beta` | `/models/{encoded model}:generateContent` | `x-goog-api-key` header |

Anthropic requests include `anthropic-version: 2023-06-01`. The adapters use
blocking reqwest 0.12.28 with JSON and rustls. Redirects, automatic retries, and
environment proxies are explicitly disabled. A timeout covers the request and
response body. Both declared and streamed body sizes are bounded before JSON
decoding. Non-success response bodies never enter application errors.

## Exact model profiles

`Default` omits the effort field. It differs from `Off`, which none of the initial
profiles supports. Unknown identifiers and unsupported efforts fail before I/O.
There are no prefix matches or silent effort reductions.

| Model ID | Supported explicit efforts | Native control |
| --- | --- | --- |
| `gpt-6.1-sol` | Low, Medium, High, Xhigh, Max | `reasoning.effort` |
| `claude-opus-5-5` | Low, Medium, High, Xhigh, Max | `output_config.effort` |
| `gemini-3.8-flash` | Low, Medium, High | `generationConfig.thinkingConfig.thinkingLevel` |

Anthropic requests enable adaptive thinking. OpenAI requests disable server
storage and request encrypted reasoning content for stateless replay.

## Assistant input and native replay

Built-in assistant adapters require the last exchange to be active and awaiting
the model. They preserve turn identities, outcomes, ordered messages, execution
notices, and effect resolutions. Closed turns appear before newly admitted input.
Notices remain descriptive facts at their exchange position. They never become
fabricated results for undispatched calls or unknown effects.

`Continuation<AssistantMessage>` is opaque, immutable, and cloneable. Its private
adapter payload owns the full checked input snapshot, the exact generated output
receipt, native output records, and the mapping between fresh domain call IDs and
native IDs or part positions. New domain IDs contain a random UUID and batch
index. Provider IDs never become session IDs, and Google parts never receive
invented IDs. Google calls that omit optional `args` produce an empty domain
argument object. Their preserved native part still omits `args`.

A continuation binds the provider, canonical endpoint, exact model, output kind,
all generation settings, instructions, ordered catalog, and every field of the
context snapshot. Validation compares cloned checked values, without debug
strings or flattened message fingerprints.

For the next request, the context must contain the exact generated assistant
output. A tool batch must have exactly its ordered results. Results preserve
empty successes and ordinary failures. A complete tool batch may remain active
or close as `Interrupted` or `Failed` with exactly its matching `TurnClosed`
notice. An answer must close its turn as `Completed`. Sequential user-only turns
that stopped without model output may follow, each with its exact closure
notice, before the final active user-only turn. New assistant output, rewritten
or truncated recorded history, changed prior notices or resolutions, and
different bindings fail before network I/O. A successful generation's next token
also binds these newly admitted closed turns.

Native records accumulate across generations and completed turns. OpenAI
reasoning and output items, Anthropic thinking and redacted-thinking blocks, and
Google signed parts retain their original order and JSON values. Google same-name
parallel calls correlate through their original positions and optional native
IDs. Duplicate or empty native IDs are rejected, including reused call IDs in
preserved history. The first Google function-call part requires a nonempty
`thoughtSignature` for the supported profile.

A request failure never modifies its input token. A different client instance
with the same canonical endpoint can reuse it. Credential rotation does not
change the endpoint binding.

A token cannot prove acceptance when a generated batch is later projected only
as interrupted execution notices. That transition returns
`ContinuationMismatch`. Existing interrupted history supplied as descriptive
notices is accepted without a token. Actual historical tool-call messages require
native replay state, or the adapter returns `MissingContinuation`.

Continuations exist only in memory. JSONL sessions do not store them. A recovered
session that still contains native tool history cannot fabricate continuation.
Context selection or starting a separate conversation is an application policy.

## Decisions

Reviewer calls are one-shot and return `continuation: None`. Their catalog and
history, including prior calls, results, and notices, are descriptive text.
Reviewers expose no callable tools and do not require an assistant continuation.

The fixed schema requires `should_allow` as a boolean and `reason` as a string or
null. Both keys are required, and extra keys are forbidden. A non-null reason
must satisfy `NonEmptyText`. Tool calls, string booleans, missing keys, and extra
keys fail decoding. Provider refusals remain stopped outcomes.

The native structured-output controls are:

- OpenAI `text.format` with `type: json_schema`, `name: decision`, `strict: true`,
  and the schema.
- Anthropic `output_config.format` with `type: json_schema` and the schema.
- Google `generationConfig.responseMimeType: application/json` and
  `responseJsonSchema`.

## Tool schemas and response checks

Tool schemas use a documented portable subset. Supported keywords are `type`,
`title`, `description`, `properties`, `required`, boolean `additionalProperties`,
`items`, scalar `enum`, `minLength`, `maxLength`, `minItems`, `maxItems`, `minimum`,
and `maximum`. Types are object, array, string, number, integer, boolean, and null.
Each schema has a single string type. Nested schemas use the same subset.
Unsupported keywords or shapes return `UnsupportedInput` before I/O. The adapter
preserves accepted schemas verbatim, including constraints such as `minLength`.
Google uses `parametersJsonSchema`. OpenAI tool schemas use `strict: false`.

These checks cover representability, not argument conformance. Returned arguments
must have an object root and name an offered tool. Executors validate their own
argument fields. Completed tool batches use checked domain constructors.

Completion metadata is inspected before exposing output. An OpenAI response with
`status: failed` returns `ProviderFailure`. Truncation and blocked
or refused output never produce partial executable calls. Unknown content kinds,
missing completion metadata, missing or multiple Google candidates, malformed
calls, and blank answers fail closed. Supported text and tool combinations retain
both visible text and the native replay content.

## Verification and limits

`cargo test --test model_clients` drives each production transport through local
TCP HTTP fixtures. It checks native schemas and headers, exact effort fields,
cumulative signed replay, ordered parallel calls, factual notices, full binding
failures with zero sends, recovery failures, immutable retries, safe diagnostics,
redirects, deadlines, size bounds, and malformed or stopped responses. API
doctests check typed contexts, typed continuation, and downstream fake usage.

No paid API calls validate these fixtures. Provider acceptance, live credentials,
TLS service behavior, and undocumented response extensions remain unverified.
Streaming, async invocation, cancellation, retry orchestration, usage accounting,
context fitting, and persistent continuation are outside this implementation.
