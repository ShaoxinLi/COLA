# Model clients

## Usage

Call either adapter with a checked context and explicit settings. Both implement
`ModelClient<AssistantMessage>` and `ModelClient<Decision>`. Construction takes a
base URL, API key, timeout, and response byte cap. The clients read no credentials
from the environment.

```rust,no_run
use cola::{
	adapters::models::{AnthropicClient, ClientConfig, OpenAIClient},
	app::{Continuation, Effort, Generation, ModelClient, ModelError, ModelId, ModelSettings},
	domain::{AssistantMessage, Context, Decision, Session},
};
use std::{num::{NonZeroU32, NonZeroUsize}, time::Duration};

fn openai(key: &str) -> Result<OpenAIClient, ModelError> {
	OpenAIClient::new(ClientConfig::new(
		"https://api.openai.com/v1", key, Duration::from_secs(60),
		NonZeroUsize::new(4_000_000).unwrap(),
	)?)
}

fn respond(
	client: &dyn ModelClient<AssistantMessage>,
	context: &Context<AssistantMessage>,
	settings: &ModelSettings,
	session: &mut Session,
	previous: &mut Option<Continuation<AssistantMessage>>,
) -> Result<(), Box<dyn std::error::Error>> {
	match client.generate(context, settings, previous.as_ref())? {
		Generation::Complete { output, continuation } => {
			let turn = session.active_turn().ok_or("no active turn")?.id();
			session.record_assistant(turn, output)?;
			*previous = continuation;
		}
		Generation::Refused { .. } | Generation::Incomplete { .. } => {}
	}
	Ok(())
}

fn review(
	client: &AnthropicClient,
	context: &Context<Decision>,
) -> Result<Generation<Decision>, ModelError> {
	let settings = ModelSettings {
		model_id: ModelId::new("your-model-id")?,
		effort: Effort::Medium,
		max_output_tokens: NonZeroU32::new(4096).unwrap(),
	};
	let reviewer: &dyn ModelClient<Decision> = client;
	reviewer.generate(context, &settings, None)
}
```

The caller records actual tool returns before generating again. Keep the previous
token after any error, refusal, incomplete response, or rejected domain output.
Publish the returned token only after accepting the exact output into the session.
Review calls produce a checked decision without a continuation or callable tools.

## Problem

`Context<O>` already checks exchanges and associates input with its expected domain
output. Native reasoning, encrypted content, and thinking signatures cannot be
recovered from those domain messages. The model boundary must preserve native
facts without adding provider JSON to the domain or owning the conversation.

## Shape

`ModelClient<O>::generate` is synchronous and object safe. Its input is
`&Context<O>`, `&ModelSettings`, and `Option<&Continuation<O>>`. It returns
`Result<Generation<O>, ModelError>`. The output parameter prevents passing a review
context or token to an assistant client.

`ModelSettings` contains an arbitrary nonblank `ModelId`, an `Effort`, and a nonzero
output token limit. There is no model catalog or capability guess. The provider
checks model support. `ModelError` exposes a fixed kind and an optional numeric HTTP
status. Its formatting and source chain contain no remote message, endpoint,
credential, prompt, arguments, or transport error.

`Continuation<O>` privately owns immutable `Arc` state. Each complete assistant
response returns a cumulative replacement token, including after a final answer.
The adapter binds it to the provider, normalized endpoint, model ID, full previous
exchange history, and exact returned assistant output. Instructions, catalog,
effort, output limit, credentials, and transport limits can change. Existing
message and notice facts cannot change.

Replay admits the accepted output followed by its ordered actual tool returns, or
an accepted answer that completes the exchange. A completed tool batch can close
as interrupted or failed with its exact closure notice. Later exchanges may add
user input and output-free closed turns. Extra unreceipted assistant output,
rewritten history, omitted exchanges, and partial native batches fail before HTTP.

The interface hides provider rendering, native correlations, validation, and
bounded HTTP. The caller owns checked context construction, session acceptance,
tool execution, and token publication. Model the Domain shaped the immutable
receipt and cumulative records so those responsibilities have one owner each.

The modules own the following knowledge.

| Module | Responsibility |
| --- | --- |
| `src/app/model_client.rs` | Port, settings, outcomes, typed opaque token, and safe errors. |
| `src/adapters/models/openai.rs` | `OpenAIClient`, Responses requests, and native decoding. |
| `src/adapters/models/anthropic.rs` | `AnthropicClient`, Messages requests, and native decoding. |
| `src/adapters/models/client.rs` | Shared invocation, offered-tool checks, and fresh domain call IDs. |
| `src/adapters/models/replay.rs` | Exact history binding and cumulative native records. |
| `src/adapters/models/context.rs` | Factual history, notices, and the decision schema. |
| `src/adapters/models/output.rs` | Private checked conversion to the two supported domain outputs. |
| `src/adapters/models/transport.rs` | Explicit configuration, native headers, deadlines, and byte caps. |
| `src/adapters/models/mod.rs` | Re-exports, shared decoded items, and ordered replay events. |

## Outcome and protocol contracts

Completion metadata takes precedence over content. Refusal produces `Refused`,
never a negative decision. Truncation or a stopped response produces `Incomplete`
without a domain output. Unsupported or malformed native data returns a safe
error. Blank answers, unoffered tools, invalid call names, non-object arguments,
duplicate native IDs, and invalid decisions never become executable output.
Domain call IDs are fresh UUID-based values independent of native IDs.

The decision object requires exactly `should_allow: boolean` and
`reason: string | null`. A non-null reason must be nonblank. Review history and the
catalog appear as descriptive facts. Empty review history gets a neutral user
prompt while retaining the review instructions and descriptive catalog.

OpenAI appends `/responses`, sends `store:false`, requests
`reasoning.encrypted_content`, and uses `text.format` for decisions. Callable
function schemas are preserved exactly with `strict:false`. Anthropic appends
`/messages`, sends `anthropic-version: 2026-10-21`, and uses
`output_config.format` for decisions. Neither adapter normalizes offered schemas
or rejects a handmade subset before sending them.

| Effort | OpenAI | Anthropic |
| --- | --- | --- |
| `Default` | Omit reasoning effort. | Omit effort and thinking. |
| `Off` | `reasoning.effort = "none"`. | `thinking.type = "disabled"`, omit effort. |
| `Low`, `Medium`, `High`, `Xhigh`, `Max` | Exact lowercase `reasoning.effort`. | Exact lowercase `output_config.effort`, omit thinking. |

A model can reject any requested effort. HTTP 400 returns `InvalidSettings` with
status 400, without retry, clamping, or fallback. Other non-success statuses retain
their number with a fixed category. Redirects, automatic retries, and environment
proxies are disabled. Successful responses are bounded before JSON decoding.

Supported native reasoning, encrypted content, thinking signatures, redacted
thinking, text, and tool blocks retain unknown fields. Each original group replays
exactly once at its recorded position. Execution notices preserve closure reasons,
unknown effects, confirmed results, undispatched calls, and resolution provenance.
Unknown effects do not acquire invented native tool returns.

## Synthesis decision

The selected architecture uses Astra's immutable typed token and full history
binding. Sol contributed arbitrary model IDs, exact effort forwarding, and errors
with fixed categories and numeric status. Independent Claude Opus 5.5 judged
Astra the better base despite scoring Sol higher overall, because mutable replay
could publish native state before domain acceptance. Its correction to allow
instructions, catalog, effort, and output limits to change is implemented.

The throwaway Rust prototype proved typed dynamic dispatch and immutable opaque
tokens. Wrong context and token types failed with E0308. The generic-method probe
failed with E0038. A two-method alternative passed three projection and replay
checks, but kept mutable active-turn state. These probes establish Rust API
properties, not provider compatibility. The judge reviewed candidates rather than
this production implementation.

## Tradeoffs accepted

- We accept cumulative in-memory native history in exchange for exact replay across
  answers and later turns. Tokens can grow with conversation length.
- We accept explicit token publication in exchange for preserving the old token
  when domain acceptance fails.
- We accept provider rejection of unsupported models, efforts, and schemas in
  exchange for preserving the caller's request without a stale capability catalog.
- We accept conservative cold-replay errors in exchange for never claiming that
  lost native reasoning was restored.

## Alternatives considered

| Alternative | Hidden complexity | Caller burden and reason rejected |
| --- | --- | --- |
| Mutable token or stateful conversation client | Native storage and correlations. | Session acceptance and token mutation can disagree, with no rollback. |
| Two-method client with active-turn replay | Concrete output conversion. | Separate methods expand for each output and discard native facts after answers. |
| Public generic provider codec | Arbitrary output decoding. | Callers inherit provider wire details and validation responsibilities. |
| Internal cache keyed by session or first call ID | Native lookup. | Hidden lifetime and key rules allow stale replay after edited history. |
| Reconstruct closed tool history without a token | Persistence convenience. | Cannot prove recovery of encrypted or signed native facts in this phase. |

## Implementation reconciliation

The implementation keeps the selected public API and places both concrete client
structs in their provider files. A private generic output implementation avoids a
public codec and the old boilerplate macro. The shared invocation module owns real
cross-provider validation and token publication rules. Native call IDs and the
assistant receipt are required private values after decision handling, so the
implementation has no third-provider optional-ID states.

The owner accepted rejection of native call ID reuse within a supplied
continuation because cumulative result correlation must stay unambiguous.
Independent requests can repeat native IDs and still receive fresh domain IDs.
Both cases have loopback assertions.

The owner corrected the initial outcome sketch to remove `Blocked`, a leftover
from the superseded three-provider design with no verified two-provider decoder.
The final outcomes are `Complete`, `Refused`, and `Incomplete`. The unused
`UnsupportedModel` and `UnsupportedEffort` error kinds were removed with the model
catalog. The owner also accepted the neutral empty-history review prompt as
native request rendering within the existing contract.

The judge suggested allowing cold closed tool history. The selected contract
explicitly deferred native reconstruction and retained `MissingContinuation` for
that history. The implementation follows the selected contract and documents the
limit rather than silently falling back.

## Limits and verification

Tokens have no serialization, raw wire accessor, hidden cache, session registry,
or previous-response storage. JSONL recovery cannot restore them. Tool history
without a token, including closed batches, fails with `MissingContinuation`.
Notices alone can form a factual baseline. A deliberately new baseline can include
plain historical answers, without a claim to restored native reasoning.

Assistant generation requires an active last exchange. Partial interrupted native
batches fail with `ContinuationMismatch` even when the domain can describe their
recovery notices. History fitting, durable native replay, streaming, async calls,
model discovery, agent loops, and application assembly remain outside this pair.

Bounded loopback servers with fake keys verify actual production HTTP, request
paths and headers, all efforts, schema preservation, decisions, exact cumulative
replay, recovery facts, safe errors, stops, timeouts, redirects, and byte caps.
They do not prove that a live provider accepts any model, schema, reasoning item,
or signature. No paid request or provider credential read was performed.

## Next implementation step

The standalone pair is implemented. Application assembly can consume this port
when its own scope defines session and tool execution ownership.

## Protocol sources

The protocol fields follow the official documentation read on 2026-10-26.

- [OpenAI reasoning](https://developers.openai.com/api/docs/guides/reasoning) describes
  reasoning effort and native reasoning continuation.
- [OpenAI function calling](https://developers.openai.com/api/docs/guides/function-calling)
  describes explicit non-strict tool schemas.
- [OpenAI structured outputs](https://developers.openai.com/api/docs/guides/structured-outputs)
  describes the decision response format.
- [Anthropic effort](https://platform.claude.com/docs/en/build-with-claude/effort) and
  [thinking](https://platform.claude.com/docs/en/build-with-claude/thinking) distinguish
  effort from thinking and describe signed content.
- [Anthropic structured outputs](https://platform.claude.com/docs/en/build-with-claude/structured-outputs)
  describes the decision schema field.
- [Anthropic versions](https://platform.claude.com/docs/en/api/versioning) describes
  the selected version header.
