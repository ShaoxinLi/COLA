#![forbid(unsafe_code)]
//! COLA's synchronous conversation domain, host tools, and session persistence.
//!
//! The library records facts and validates transitions. Callers supply model
//! messages and tool results. See `examples/read_file.rs` for a full exchange
//! and `examples/session_store.rs` for saving and recovering sessions.
//! `examples/basic_tools.rs` runs the host Bash and file adapters.
pub mod domain;

pub mod adapters;
pub mod app;
