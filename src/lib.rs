#![forbid(unsafe_code)]
//! Synchronous AI agent contracts, orchestration, and adapters.
//!
//! The domain validates conversation facts. The application coordinates model
//! requests, reviewed tool execution, and saves through caller-supplied adapters.
pub mod domain;

pub mod adapters;
pub mod app;
