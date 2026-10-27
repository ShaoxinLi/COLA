# Web and CLI APIs

Both adapters call one existing `AgentService`. They expose session creation,
session listing, and chat turns through HTTP or a persistent command session.
The embedding program supplies model configuration, storage, context assembly,
tools, and process startup. COLA provides library entry points, not a standalone
executable.

## Library entry points

| Type | Entry point | Result |
| --- | --- | --- |
| `cola::adapters::api::web::WebApi` | `WebApi::new(service).run(listener)` | `io::Result<()>` |
| `cola::adapters::api::cli::CliApp` | `CliApp::new(service).run(input, output)` | `io::Result<CliExit>` |

Both constructors take ownership of the service. The web listener is an
already-bound `std::net::TcpListener`. CLI input implements `BufRead`, and output
implements `Write`. The adapters require no `Send` or `Sync` bound on the service.

The [README examples](../README.md#inbound-apis) show both entry points.
[Agent service](agent-service.md) describes the collaborators and agent loop.

## HTTP

Application responses use `Content-Type: application/json`.

| Method and path | Request body | Success response |
| --- | --- | --- |
| `POST /sessions` | Empty body or `{}` | `201`, `{"session": Session}` |
| `GET /sessions` | Unused | `200`, `{"sessions": [Session, ...]}` |
| `POST /sessions/{session_id}/messages` | `{"text":"Hello"}` | `200`, `{"session_id":"...","messages":[Message, ...]}` |

`Session` and `Message` are the JSON shapes below. Session listings
are sorted by ID. Chat responses contain only the new turn, including its user
message and any tool requests and results.

Nonempty POST bodies require `Content-Type: application/json`. Media type
matching ignores case and accepts parameters such as `charset=utf-8`.
POST bodies are limited to 65,536 bytes, including chunked bodies.
JSON must be an object with exactly the expected fields. Unknown fields,
duplicate fields, and incorrect types are invalid. Message text must contain a
non-whitespace character. Valid text keeps its original whitespace.

Session IDs are opaque, case-sensitive strings. The path accepts a percent-encoded
UTF-8 ID but rejects decoded `/` and `\` characters. Query strings do not affect
route matching. Paths are exact, so `/sessions/` is not `/sessions`.

For a running server, a chat request has this form:

```sh
curl http://127.0.0.1:8080/sessions/SESSION_ID/messages \
	-H 'Content-Type: application/json' \
	-d '{"text":"Hello"}'
```

An example response without tool calls is:

```json
{
	"session_id": "SESSION_ID",
	"messages": [
		{"kind": "user", "text": "Hello"},
		{"kind": "assistant_answer", "text": "Hi"}
	]
}
```

## CLI

Each input line is a command or message. The CLI starts with no selected session.

| Input | Behavior | Reply `kind` |
| --- | --- | --- |
| `/new` | Creates and selects a session. | `created` |
| `/sessions` | Lists sessions sorted by ID. | `listed` |
| `/use ID` | Selects the ID without loading or checking that the session exists. | `selected` |
| Ordinary nonblank text | Runs a chat turn in the selected session. | `chatted` |
| Text starting with `//` | Sends the text with one leading slash removed. | `chatted` |
| `/exit` or EOF | Ends `run`. | No reply |
| Blank or whitespace-only line | Does nothing. | No reply |

Commands match exactly. The CLI removes LF or CRLF line endings and preserves
other whitespace. Unknown slash commands, invalid `/use` IDs, invalid UTF-8,
oversized lines, and messages without a selected session produce `invalid_input`
errors.
The limit is 65,536 bytes per line, excluding the line ending. An oversized line
is discarded through its end before the next command is read.

Each reply is one flushed JSON line. Successful replies use these fields:

| `kind` | Other fields |
| --- | --- |
| `created` | `session` |
| `listed` | `sessions` |
| `selected` | `session_id` |
| `chatted` | `session_id`, `messages` |

Errors use `{"kind":"error","error": ...}` with the shared error shape below.
Command errors leave the CLI running. Stream failures return `io::Error`
immediately.

`CliExit::code()` returns the accumulated command outcome without exiting the
process. Invalid input takes precedence over service failures.

| Code | Meaning |
| --- | --- |
| `0` | All commands succeeded. |
| `1` | At least one service command failed. |
| `2` | At least one input line or command was invalid. |

## Shared response shapes

A newly created session is `{"id":"...","turns":[],"active":null}`.
`turns` contains closed turns. `active` is the current turn or `null`.

| Object | Fields |
| --- | --- |
| Session | `id`, `turns`, `active` |
| Turn | Numeric `id`, `user` text, `steps`, `outcome` |
| Step | `assistant`, `executions` |
| Tool call | `id`, `name`, `arguments` object |
| Execution | `call_id`, `state`, `resolution` |

A step's `assistant.kind` is `answer` with `text`, or `tool_requests` with
nullable `text` and a `calls` array. Execution state uses `kind` values
`not_started`, `started`, or `returned`. A returned state also contains an
`outcome` with `kind` equal to `success` or `failure`, plus `text`.

`resolution` is `null` or contains `disposition`, `source`, and `evidence`.
Dispositions are `completed` and `not_performed`. Sources are `observation`,
`user_attestation`, and `application_attestation`.

A closed turn's `outcome.kind` is `completed`, `interrupted`, or `failed`.
Interrupted outcomes contain a `reason` of `user_cancelled`, `process_lost`,
`context_capacity`, or `iteration_limit`. Failed outcomes contain a `failure`
with `text` and a `kind` of `model_failure` or `malformed_response`.
Active turns have `outcome: null`.
The [conversation contracts](domain.md) define the underlying states.

Chat messages use the following tagged shapes:

| `kind` | Other fields |
| --- | --- |
| `user` | `text` |
| `assistant_answer` | `text` |
| `assistant_tool_requests` | Nullable `text`, `calls` |
| `tool_result` | `call_id`, `outcome` |

A denied tool call is a normal tool result with a failure outcome.

## Errors

HTTP errors use `{"error": ...}`. CLI errors add `"kind":"error"`.
Both contain the same error object:

```json
{
	"error": {
		"code": "session_not_found",
		"message": "Session not found",
		"role": null,
		"progress": null,
		"durability": "unconfirmed"
	}
}
```

`code` and `message` are fixed diagnostics. They exclude underlying provider and
storage error text. `role` is `assistant`, `reviewer`, or `null`.
`progress` is a session snapshot or `null`. The snapshot can contain changes
that failed to save. `durability` is always `"unconfirmed"` and does not prove
that the last save succeeded. An error does not imply that a tool had no effect.

| HTTP status | Error code |
| --- | --- |
| `400` | `invalid_input` |
| `404` | `route_not_found`, `session_not_found` |
| `405` | `method_not_allowed`, with an `Allow` header |
| `409` | `session_blocked` |
| `413` | `body_too_large` |
| `415` | `unsupported_media_type` |
| `422` | `generation_refused`, `iteration_limit` |
| `500` | `store_failure`, `context_failure`, `catalog_mismatch`, `unknown_tool`, `rejected_output`, `unexpected_state`, `duplicate_tool`, `internal_failure` |
| `502` | `generation_incomplete`, `model_failure` |
| `504` | `model_timeout` |

CLI input failures use `invalid_input`. Service failures use the corresponding
shared codes without HTTP status values. Malformed HTTP protocol input can
receive a server-generated response or a closed connection instead of JSON.

## Runtime limits

Both adapters keep one service on the calling thread and execute service
operations serially. A long turn delays later operations. `WebApi::run` requires
a synchronous caller outside a Tokio runtime and runs until a fatal networking
error. Disconnecting a client does not cancel an admitted turn. The adapter does
not retry that turn.

The web server supports HTTP/1.1 over plain TCP. It has no TLS, authentication,
connection cap, request deadline, or bounded shutdown.
Native model continuations remain in memory. Stored session facts do
not restore every continuation after restart. See
[service lifetime and restart limits](agent-service.md#lifetime-and-restart-limits).
