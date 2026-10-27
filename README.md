# COLA

COLA (Coffee-Overclocked Little Agent) is an AI agent project built from scratch
with Rust. It currently defines checked conversation state, application ports,
concrete adapters, and a synchronous agent loop.

## Project structure

The library has three layers.

| Layer | Responsibility | Reference |
| --- | --- | --- |
| `src/domain` | Values, messages, conversation transitions, tools, context assembly, and snapshot recovery. | [Domain contracts](docs/domain.md) |
| `src/app` | Agent orchestration, typed model generation, and complete-session persistence contracts. | [Agent service](docs/agent-service.md), [model clients](docs/model-clients.md#application-contracts), [session persistence](docs/session-store.md#application-contract) |
| `src/adapters` | OpenAI and Anthropic clients, JSONL storage, Bash, and file tools. | [Model clients](docs/model-clients.md#adapter-implementations), [session persistence](docs/session-store.md#jsonl-adapter), [tool adapters](docs/tools.md) |

The domain has no application or adapter dependency. Application contracts use
domain values. Adapters implement the contracts. `AgentService` coordinates
model invocation, per-call review, tool execution, continuation acceptance,
and persistence ordering. Callers supply its configured clients, roles, and tools.

## Test structure

Tests follow the same layer boundaries.

| Entry point | Coverage and shared setup |
| --- | --- |
| [`tests/domain.rs`](tests/domain.rs) | Shared domain fixtures and per-module tests. Snapshot recovery stays with conversation tests. |
| [`tests/app.rs`](tests/app.rs) | Agent service behavior, model contracts, and persistence contracts. |
| [`tests/adapters.rs`](tests/adapters.rs) | Model, store, and tool tests. The `tools` module owns shared tool contracts and helpers. |

Model tests share a local HTTP server and provider fixtures in
[`tests/adapters/models/support.rs`](tests/adapters/models/support.rs).

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
