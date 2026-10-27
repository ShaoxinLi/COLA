#![forbid(unsafe_code)]
//! COLA's synchronous conversation domain, host tools, and session persistence.
//!
//! The library records facts and validates transitions. Callers supply model
//! messages and tool results.
pub mod domain;

pub mod adapters;
pub mod app;

pub fn __ci_type_probe() -> u8 {
    "broken"
}
