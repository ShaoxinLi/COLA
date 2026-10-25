# COLA

COLA (Coffee-Overclocked Little Agent) is an agent project built from scratch
with Rust.

## Current status

COLA is in early development. The domain layer is implemented as the
`cola::domain` library, with conversation state, messages, tool definitions,
context values, snapshot export, and validated snapshot restoration.

The agent execution loop, model integration, tool executors, and persistence
adapters are not implemented.

## Build and test

The repository pins Rust 1.98.1. Run the same checks as GitHub Actions from
the repository root.

```sh
./ci/check.sh
```

Run the domain example with manually supplied model and tool output.

```sh
cargo run --example read_file
```

The example records an in-memory exchange and performs no file operation.

## Documentation

- [Server checks](ci/server/README.md) describes the credential-free branch
  poller, installation, results, and retries.
- [Domain contracts](docs/domain.md) describes the domain types, transitions,
  context rules, and snapshot recovery.
- [Domain example](examples/read_file.rs) shows a complete conversation with a
  manually supplied tool result.

Generate local API documentation with:

```sh
cargo doc --no-deps
```
