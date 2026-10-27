# COLA

COLA (Coffee-Overclocked Little Agent) is an AI agent library written in Rust.
It defines conversation contracts, model and storage ports, concrete adapters,
and a synchronous agent loop in `AgentService`.

## Architecture

Dependencies point toward the domain. The application uses domain values and
traits. Adapters implement those contracts and own external I/O.

| Layer | Responsibility | Reference |
| --- | --- | --- |
| `src/domain` | Checked values, conversation transitions, tools, context, and recovery. | [Domain contracts](docs/domain.md) |
| `src/app` | Agent orchestration, model generation, and session persistence ports. | [Agent service](docs/agent-service.md), [model clients](docs/model-clients.md#application-contracts), [session persistence](docs/session-store.md#application-contract) |
| `src/adapters` | OpenAI and Anthropic clients, JSONL storage, Bash, and file tools. | [Model clients](docs/model-clients.md#adapter-implementations), [session persistence](docs/session-store.md#jsonl-adapter), [tool adapters](docs/tools.md) |

`Session` validates and records facts without external I/O. `Assistant` and
`ToolCallReviewer` assemble typed model input. `AgentService` coordinates model
requests, per-call review, tool execution, continuations, and saves. Callers
supply its clients, store, roles, settings, and tools.

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
`tests/adapters.rs`. Model tests use local HTTP fixtures, while storage and tool
tests use temporary files and real processes. They do not verify live providers.

Generate API reference documentation with `cargo doc --no-deps`.
