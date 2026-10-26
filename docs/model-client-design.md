# Model client design

The accepted design places `ModelClient<O>` in the application layer and native
OpenAI Responses, Anthropic Messages, and Google GenerateContent adapters in the
adapter layer. Domain values and JSONL session facts keep their existing roles.
The production contract is documented in [Model clients](model-clients.md).

## Accepted shape

`ModelClient<O>` is synchronous and object-safe. The two built-in outputs are
`AssistantMessage` and `Decision`. `ModelSettings` names the exact model, effort,
and nonzero output limit. `Effort::Default` omits the provider control, while
`Off` requests a capability that the initial profiles reject.

`Generation<O>` distinguishes complete, refused, blocked, and incomplete output.
Only `Complete` exposes checked domain output. Its optional typed continuation
allows downstream fakes to return output without creating native state.
`Decision` remains data and grants no tool execution authority.

`Continuation<O>` is immutable caller-owned state with private construction.
The caller accepts generated assistant output before publishing the returned
token. The token accumulates native records, including opaque reasoning and
signatures, across generations and completed turns. Request failures leave the
previous token reusable. No implicit cache or mutex owns session state.

Full checked context values bind replay. The binding includes the provider,
canonical endpoint, model and profile, output kind, settings, instructions,
ordered catalog, turn identities, outcomes, messages, notices, and resolutions.
Generated domain call IDs are independent of native IDs and Google part
positions. Exact output receipts prevent hidden or rewritten assistant history.

## Why this structure

The domain's `Context<O>` already associates input with its expected output.
A trait generic over `O` preserves that association through trait objects.
A generic method would prevent direct trait-object use. Public normalized request
and reply objects would expose a second model that callers and fakes must learn.

Domain messages omit provider reasoning and signatures. Rebuilding native tool
history from those messages would lose required data. Explicit continuation
keeps ownership visible and avoids instance-local caches or silent eviction.
Current JSONL snapshots cannot restore native state, so recovery reports missing
continuation rather than inventing a signed transcript.

A single effort enum represents caller intent without claiming equal provider
compute budgets. Exact private profiles reject unsupported settings before I/O.
The initial profiles are `gpt-6.1-sol`, `claude-opus-5-5`, and
`gemini-3.8-flash`.

## Protocol evidence

The protocol choices were checked against official documentation on 2026-10-26.
The sources establish documented behavior, not live service compatibility.

- [OpenAI Responses](https://developers.openai.com/api/reference/resources/responses/methods/create)
  and [reasoning](https://developers.openai.com/api/docs/guides/reasoning)
  establish the native endpoint, effort control, and opaque reasoning replay.
- [GPT-6.1 Sol](https://developers.openai.com/api/docs/models/gpt-6.1-sol)
  documents its exact effort levels.
- [Anthropic effort](https://platform.claude.com/docs/en/build-with-claude/effort)
  and [thinking](https://platform.claude.com/docs/en/build-with-claude/thinking)
  establish model-specific effort and signed thinking content.
- [Google GenerateContent](https://ai.google.dev/api/generate-content),
  [thinking](https://ai.google.dev/gemini-api/docs/generate-content/thinking),
  and [thought signatures](https://ai.google.dev/gemini-api/docs/generate-content/thought-signatures)
  establish the REST controls and preservation of signed parts.

## Implementation reconciliation

Production code now implements the accepted application port and all three
blocking native adapters. The opaque token uses `Arc<dyn Any + Send + Sync>`
with private construction and downcasting. Adapter-owned payloads compare full
cloned checked values and accumulate native output records.

Local TCP HTTP fixtures exercise the real reqwest 0.12.28 path, including exact
payloads, completion checks, structured Decisions, tool results, full binding,
four-generation replay across completed turns, and safe transport failures.
Compile-fail doctests enforce the output and continuation type boundaries.
The domain and JSONL formats are unchanged.

These bounded choices refine the accepted design. Assistant generation requires
an active final exchange so an output receipt has an unambiguous position.
Reviewer calls return no continuation and render closed tool history as
descriptive facts. A generated tool batch later reduced to interrupted notices
cannot prove acceptance of its exact assistant preamble and therefore fails
continuation validation. Existing notice-only history can be rendered without
fabricated native tool results.

Independent review found that complete tool batches closed after a stopped
generation must remain usable. Receipt validation now permits an exact complete
batch with its domain-generated interrupted or failed closure notice. It also
admits sequential user-only stopped turns before fresh input. Previously captured
history remains immutable, partial batches still fail, and new tokens bind newly
admitted closure facts. Local HTTP regressions cover this recovery across all
three adapters. Review also corrected Google's optional omitted arguments without
rewriting its signed native part, and classified failed OpenAI responses as
provider errors.

Model the Domain shaped the output variants and typed receipts. Boundary
Discipline keeps provider JSON, schema validation, completion checks, and safe
errors inside adapters. Test Behavior, Not Implementation shaped tests around
public calls and externally observed HTTP payloads.

## Remaining limits

Live provider acceptance has not been tested. The implementation supports only
the documented schema subset and native content kinds described in the reference.
Persistent continuation, streaming, asynchronous invocation, cancellation, and an
agent execution loop remain separate work.
