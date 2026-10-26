# COLA

COLA (Coffee-Overclocked Little Agent) is an AI agent project built from scratch
with Rust. It currently defines checked conversation state, application ports,
and concrete adapters. The agent execution loop is not implemented.

## Project structure

The library has three layers.

| Layer | Responsibility | Reference |
| --- | --- | --- |
| `src/domain` | Values, messages, conversation transitions, tools, context assembly, and snapshot recovery. | [Domain contracts](docs/domain.md) |
| `src/app` | Typed model generation and complete-session persistence contracts. | [Model clients](docs/model-clients.md#application-contracts), [session persistence](docs/session-store.md#application-contract) |
| `src/adapters` | OpenAI and Anthropic clients, JSONL storage, Bash, and file tools. | [Model clients](docs/model-clients.md#adapter-implementations), [session persistence](docs/session-store.md#jsonl-adapter), [tool adapters](docs/tools.md) |

The domain has no application or adapter dependency. Application contracts use
domain values. Adapters implement the contracts. Callers own model invocation,
tool selection, execution, continuation acceptance, and persistence ordering.

Integration tests have separate entry points in `tests/domain.rs`, `tests/app.rs`,
and `tests/adapters.rs`. Test files follow production ownership. Shared fixtures
live beside the tests that use them.

## Build and verify

Use a Rust toolchain that supports the Rust 2024 edition. Run these commands from
the repository root.

```sh
cargo build
cargo test --all-targets
cargo test --doc
cargo fmt --all -- --check
cargo clippy --all-targets -- -D warnings
```

Generate local API documentation with `cargo doc --no-deps`.
