# COLA

COLA (Coffee-Overclocked Little Agent) is an agent project built from scratch
with Rust.

## Current status

COLA is in early development. The domain layer is implemented as the
`cola::domain` library, with conversation state, messages, tool definitions,
the synchronous `Tool`, `Assistant`, and `ToolCallReviewer` contracts, typed
context values, review decisions, snapshot export, and validated snapshot
restoration.

The application port `cola::app::SessionStore` and filesystem adapter
`cola::adapters::stores::JsonlSessionStore` persist complete session facts.
The adapters `cola::adapters::tools::{BashTool, ReadFileTool, WriteFileTool}`
execute Bash commands and file operations on the host. The blocking model adapters
`cola::adapters::models::{OpenAIClient, AnthropicClient, GoogleClient}` implement
`cola::app::ModelClient<O>` for assistant messages and review decisions, with
checked native replay. The agent execution loop is not implemented.

Implementations of `Tool` validate their argument fields and return
`ToolOutcome`. Execution can block the caller. The caller selects the tool,
associates its outcome with the original call ID, and records the result in
`Session`.

## Build and test

Use a Rust toolchain that supports the Rust 2024 edition. Run these commands
from the repository root.

```sh
cargo build
cargo test --all-targets
```

Run the domain example with manually supplied model and tool output.

```sh
cargo run --example read_file
```

The example records an in-memory exchange and performs no file operation.

Run the persistence example to save and reload a session on a local filesystem.

```sh
cargo run --example session_store -- /path/to/sessions
```

Run the real tool example to write and read a temporary file and execute Bash.
Bash must be available on PATH.

```sh
cargo run --example basic_tools
```

## Documentation

- [Model clients](docs/model-clients.md) describes native APIs, exact model
  profiles, typed output, replay binding, and transport limits.

- [Basic tool adapters](docs/tools.md) describes arguments, host behavior,
  output limits, and blocking execution.
- [Session persistence](docs/session-store.md) describes atomic JSONL snapshots
  and recovery on a local filesystem.

- [Domain contracts](docs/domain.md) describes the domain types, transitions,
  context rules, and snapshot recovery.
- [Domain example](examples/read_file.rs) shows a complete conversation with a
  manually supplied tool result.

Generate local API documentation with:

```sh
cargo doc --no-deps
```
