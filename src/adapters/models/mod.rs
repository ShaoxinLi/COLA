//! Native blocking model adapters. Provider JSON and immutable replay records stay private.
mod anthropic;
mod client;
mod openai;

pub use anthropic::AnthropicClient;
pub use client::ClientConfig;
pub use openai::OpenAIClient;
