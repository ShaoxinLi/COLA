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
execute Bash commands and file operations on the host. The agent execution
loop is not implemented. The application port `cola::app::ModelClient<O>` and
`cola::adapters::models::{OpenAIClient, AnthropicClient}` provide blocking model
generation with checked outputs and caller-owned native continuation tokens.

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

## Documentation

- [Model clients](docs/model-clients.md) describes explicit configuration,
  effort mapping, checked outputs, native replay, and recovery limits.

- [Basic tool adapters](docs/tools.md) describes arguments, host behavior,
  output limits, and blocking execution.
- [Session persistence](docs/session-store.md) describes atomic JSONL snapshots
  and recovery on a local filesystem.

- [Domain contracts](docs/domain.md) describes the domain types, transitions,
  context rules, and snapshot recovery.

Generate local API documentation with:

```sh
cargo doc --no-deps
```
