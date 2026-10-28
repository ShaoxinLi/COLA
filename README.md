# COLA

COLA (Coffee-Overclocked Little Agent) is an AI agent library written in Rust.
It defines conversation contracts, model and storage ports, concrete adapters,
and a synchronous agent loop in `AgentService`. The `cola` executable assembles
these components from a JSON configuration file.

## Run COLA

Copy the configuration template from the repository root:

```sh
cp config.example.json config.json
```

Fill in `api_key` in `config.json`. The template selects `deepseek-flash` for
both the assistant and tool reviewer through DeepSeek's Responses endpoint.
`config.json` is ignored by Git. Keep the example's key empty.

Start the CLI:

```sh
cargo run -- cli --config config.json
```

Enter `/new`, send a message, then enter `/exit`. Replies are JSON lines.

To serve the same agent loop through HTTP instead:

```sh
cargo run -- web --config config.json --bind 127.0.0.1:8080
```

The executable registers Bash, file-read, and file-write tools using `tool_cwd`.
Every proposed call is reviewed by the configured reviewer model before local
execution. These tools use the host user's privileges; the working directory
does not confine their access. The process keeps one service alive for its
selected API mode. Operations use per-session OS locks. Simultaneous writers to the same session receive a busy error.

See [startup configuration](docs/api.md#executable-configuration) for fields,
defaults, exits, and restart limits.

## Architecture

Dependencies point toward the domain. The application uses domain values and
traits. Adapters implement those contracts and own external I/O.

| Layer | Responsibility | Reference |
| --- | --- | --- |
| `src/domain` | Checked values, conversation and invocation facts, context rules, tools and recovery. | [Domain contracts](docs/domain.md) |
| `src/app` | Agent orchestration, model generation, and session persistence ports. | [Agent service](docs/agent-service.md), [model clients](docs/model-clients.md), [session persistence](docs/sessions.md) |
| `src/adapters` | Web and CLI APIs, OpenAI and Anthropic clients, JSONL storage, Bash, and file tools. | [Inbound APIs](#inbound-apis), [model clients](docs/model-clients.md), [session persistence](docs/sessions.md), [tool adapters](docs/tools.md) |
| `src/main.rs` | JSON configuration, context policies, component assembly, and process startup. | [Run COLA](#run-cola), [startup configuration](docs/api.md#executable-configuration) |

`Session` validates and records facts without external I/O. `SessionArchive` adds invocation provenance and native evidence. Context plans select factual history separately from the complete archive. `AgentService` coordinates model requests, review, execution and commits. Callers supply its client, store, role settings and tools.

## Inbound APIs

The library exposes `cola::adapters::api::web::WebApi` and
`cola::adapters::api::cli::{CliApp, CliExit}`. Construct an `AgentService` with
your chosen collaborators, then pass it to one entry point. The embedding
program supplies configuration and chooses its process exit code.

To serve HTTP with an already-constructed `service`:

```rust
use cola::adapters::api::web::WebApi;
use std::net::TcpListener;

let listener = TcpListener::bind("127.0.0.1:8080")?;
WebApi::new(service).run(listener)?;
```

| Request | Result |
| --- | --- |
| `POST /sessions` with an empty body or `{}` | Creates a session and returns `{"session": ...}` with status 201. |
| `GET /sessions` | Returns `{"sessions": [...]}` sorted by ID. |
| `POST /sessions/{session_id}/messages` with `{"text":"Hello"}` | Runs one turn and returns `{"session_id":"...","messages":[...]}`. |
| `POST /sessions/{session_id}/recover` with an empty body or `{}` | Closes interrupted work without replaying model or tool effects. |

Message requests use `Content-Type: application/json`. Request bodies are
limited to 64 KiB. Session responses retain turn outcomes, active work, tool
execution states, and effect resolutions. Chat responses contain only the new
turn, including its user message.

To read CLI commands from standard input with an already-constructed `service`:

```rust
use cola::adapters::api::cli::CliApp;
use std::io;

let exit = CliApp::new(service).run(io::stdin().lock(), io::stdout().lock())?;
let code = exit.code();
```

The CLI keeps one selected session for the lifetime of `run`. `/new` creates
and selects a session, `/sessions` lists sessions, and `/use ID` selects an
opaque session ID without loading it. An ordinary nonblank line sends a
message to the selected session. `/recover` closes interrupted active work in that session without executing its tools. Prefix a literal slash with another slash,
such as `//new`. `/exit` or EOF ends the session.

Each non-exit command writes and flushes one JSON line. Replies use `kind`
values `created`, `listed`, `chatted`, `selected`, `recovered`, or `error`. Blank lines emit
nothing. Input lines are limited to 64 KiB. Command failures preserve the
running service. `CliExit::code()` returns 0 on success, 1 after a service
failure, or 2 after invalid input. Stream failures return `io::Error`.

Both entry points retain one service on the calling thread. The Web adapter
handles network I/O on a private thread and sends checked commands to that
service. Service operations run one at a time, so a long chat delays later
operations. Call `WebApi::run` from a synchronous thread outside a Tokio runtime.
The server has no authentication, cancellation, or bounded
shutdown. Choose a local bind address for local use. A disconnected client does
not cancel an admitted turn, and the adapter does not retry it.

Service failures return fixed diagnostic codes and safe messages. Durable progress is separate from
uncommitted observations or uncertain publication. Inspection reads disk facts rather than a resident
overlay. A fresh service can continue completed tool history from the archive. Interrupted active work
requires explicit recovery; dispatched calls without a committed return remain unknown and are never
automatically repeated. See [session operations](docs/agent-service.md).

## Verify changes

Use a Rust toolchain that supports the Rust 2024 edition. Run these commands
from the repository root.

```sh
cargo test --all-targets
cargo test --doc
cargo fmt --all -- --check
cargo clippy --all-targets -- -D warnings
```

Tests mirror the source layers in `tests/domain.rs`, `tests/app.rs`, and
`tests/adapters.rs`. `tests/main.rs` launches the executable through both APIs.
Model tests use local HTTP fixtures, while storage and tool tests use temporary
files and real processes. Automated tests do not require live provider keys.

Generate API reference documentation with `cargo doc --no-deps`.
