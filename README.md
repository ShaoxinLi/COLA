# COLA

COLA (Coffee-Overclocked Little Agent) is an agent project built from scratch
with Rust.

## Current status

COLA is in early development. The domain layer is implemented as the
`cola::domain` library, with conversation state, messages, tool definitions,
the synchronous `Tool`, `Assistant`, and `ToolCallReviewer` contracts, typed
context values, review decisions, snapshot export, and validated snapshot
restoration.

The agent execution loop, model integration, tool executors, and persistence
adapters are not implemented.

External implementations of `Tool` validate their argument fields and return
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

## Documentation

- [Domain contracts](docs/domain.md) describes the domain types, transitions,
  context rules, and snapshot recovery.
- [Domain example](examples/read_file.rs) shows a complete conversation with a
  manually supplied tool result.

Generate local API documentation with:

```sh
cargo doc --no-deps
```
