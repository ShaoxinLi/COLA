#[path = "adapters/stores/jsonl.rs"]
mod jsonl;
#[path = "adapters/models"]
mod models {
    mod anthropic;
    mod client;
    mod openai;
    mod support;
}
#[path = "adapters/tools"]
mod tools {
    mod bash;
    mod contracts;
    mod files;
}
