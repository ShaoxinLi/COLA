# Model clients

`OpenAIClient` implements the Responses protocol. `AnthropicClient` implements
the Messages protocol. Both implement the typed application port
`ModelClient<AssistantMessage>` and `ModelClient<Decision>`.

## Invocation contract

`ModelClient::route()` returns a nonsecret route identity. `invoke(input, recorder)`
hides request compilation, protocol checks, fitting, preparation persistence,
HTTP, response capture, and typed decoding. Supply clients to `AgentService` to
obtain the application-owned recorder and durable workflow.

`ModelInput<O>` carries checked semantic context, model settings, selection
policy, an optional frozen turn view, and accepted native steps read from the
[session record](session_record.md). Each native step includes its archived response and actual results.
Reviewer inputs carry a mandatory `ReviewSubject` with the full active turn,
candidate call, and exact address. They contain no assistant native steps.

Before network I/O, a client must call `BeforeModelSend::persist` with the exact
request bytes and, for the first assistant invocation, its new `TurnView`.
Only a successful commit returns `SendPermit`, which owns the committed request
artifact. Transport consumes the permit and sends its bytes. Persistence errors
escape the invocation immediately. They are not model failures and must not
trigger HTTP.

`Observation<O>` contains `Generation<O>` and independent `ResponseEvidence`.
Generation is complete, refused, incomplete, or failed. Preparation failures
have no response and are distinguished in the committed lifecycle. The evidence
retains response bytes, returned model identity when present, response ID,
status, usage, capture completeness, and timing. Missing actual model identity
stays unknown rather than copying the requested model.

The application accepts output and evidence in one record revision. A complete
response that violates the advertised catalog or domain transition becomes an
audited `OutputRejected` with a reason and no executable step.

## Connection and response boundaries

`ClientConfig::new` takes a base URL, credential, total timeout, and maximum
response bytes. Base URLs allow HTTP or HTTPS without userinfo, query, or
fragment. Clients append `responses` or `messages`. Authentication is sent only
in headers. Headers and credentials are never included in `ModelArtifact`.
Redirects, retries, and environment proxies are disabled.

Responses are captured up to the configured byte limit, including non-success
HTTP bodies. Truncated and interrupted reads are marked partial. A transport
failure before a response is marked not returned. Safe `ModelError` values expose
a kind and optional HTTP status without raw provider bodies, URLs, or credentials.

Requests preserve arbitrary nonblank model IDs and explicit effort/output limits.
Native tool schemas retain the caller's JSON Schema fields. Review requests use
a strict decision schema with `should_allow` and nullable nonblank `reason`.
They contain descriptive tool catalogs, not callable tools. Adapters render the
mandatory subject and optional history as labelled user-role data. Current user
text and tool results never enter system instructions.

Unknown native item types, malformed calls, invalid decisions, and incomplete
responses fail closed. Raw evidence remains in the archive. A refusal or partial
response never creates executable tool calls.

## Native history

Native tool IDs are preserved verbatim. Domain uniqueness applies only inside a
response batch. Both adapters conservatively reject repeated call IDs within the
current native request chain. OpenAI also checks output-item IDs separately.
This is an adapter compatibility policy, not a universal provider/domain rule.

Each completed turn becomes quoted semantic history for the next turn. Old
native reasoning and tool associations remain archived but are not replayed
across turn boundaries. Historical requested model names, usage, and timings are
audit only.

Within a turn, the first request is the frozen prefix. Every subsequent request
validates that semantic prefix, system, tools, route, model, and settings are
unchanged. It appends exact accepted native blocks and correlated real results.
Anthropic thinking signatures and OpenAI reasoning items remain intact.
Missing or incompatible native evidence fails before HTTP.

The complete request-byte cap also applies after native-chain growth. It does
not trim native dependencies or tool output. See the
[context policy limits](agent-service.md#context-policy-and-limits) for the
model-token-capacity boundary.

## Previous live validation

On 2026-10-29, a live run of the durable-session implementation used seven fresh
CLI/Web processes against DeepSeek and Alibaba Cloud Bailian Responses endpoints.
Its 15 calls completed with HTTP 200; four reviewed file reads retained their
native IDs, and subsequent turns recalled four changing file values across model
switches. This was a scoped compatibility check, not a reliability or quality
evaluation. Anthropic coverage used local fixtures. Current automated verification
requires no live credentials; this refactor makes no new live-provider claim.
