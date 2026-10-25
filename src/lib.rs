#![forbid(unsafe_code)]
//! COLA's synchronous, in-memory conversation domain.
//!
//! The library records facts and validates transitions. Callers supply model
//! messages and tool results. See `examples/read_file.rs` for a full exchange.
pub mod domain;
