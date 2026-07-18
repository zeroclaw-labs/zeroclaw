//! Minimal on-edge ZeroClaw agent.
//!
//! The smallest real wiring that constructs an `Agent` from the runtime builder
//! and runs a single turn against an **on-device** model server. Intended as a
//! starting point for an edge build (e.g. a flagship Android phone running a
//! local Ollama / llama.cpp / MLC server on localhost).
//!
//! What it deliberately does NOT use: config files, persistent memory, channels,
//! cron, SOP, skills, MCP, or any cloud provider. Layer those in later.
//!
//! The five things `AgentBuilder::build()` requires (see
//! `crates/zeroclaw-runtime/src/agent/agent.rs:577`):
//!   1. tools          — may be empty
//!   2. memory          — skipped here via `.exclude_memory(true)` (-> NoneMemory)
//!   3. model_provider  — the on-device LLM backend
//!   4. observer        — NoopObserver (zero overhead)
//!   5. tool_dispatcher — Native (model emits native tool-calls) or Xml (fallback)
//!
//! Plus one non-obvious requirement: `.model_name(...)` must be set, or dispatch
//! fails 4xx against the `"<unconfigured>"` sentinel (agent.rs:679).
//!
//! Run against a local Ollama server:
//!   ollama serve &                       # on the device
//!   ollama pull qwen2.5:3b               # any small on-device model
//!   ZC_MODEL=qwen2.5:3b cargo run -p zeroclaw-runtime --example minimal_agent -- "hello"
//!
//! Env knobs:
//!   ZC_BASE_URL  base URL of the local model server (default http://localhost:11434)
//!   ZC_MODEL     model id the server exposes        (default qwen2.5:3b)

use std::sync::Arc;

use zeroclaw_runtime::agent::Agent;
use zeroclaw_runtime::agent::dispatcher::NativeToolDispatcher;
use zeroclaw_runtime::observability::NoopObserver;

use zeroclaw_providers::ollama::OllamaModelProvider;

#[tokio::main]
async fn main() -> anyhow::Result<()> {
    // First CLI arg is the prompt; fall back to a canned message.
    let prompt = std::env::args()
        .nth(1)
        .unwrap_or_else(|| "Say hello in one short sentence.".to_string());

    let base_url =
        std::env::var("ZC_BASE_URL").unwrap_or_else(|_| "http://localhost:11434".to_string());
    let model = std::env::var("ZC_MODEL").unwrap_or_else(|_| "qwen2.5:3b".to_string());

    // --- Mandatory collaborator #3: an on-device ModelProvider ---------------
    // OllamaModelProvider::new(alias, base_url, api_key) — point base_url at the
    // local server. No API key for a local Ollama instance.
    let provider = OllamaModelProvider::new("edge", Some(&base_url), None);

    // --- Build the agent (the 5 required pieces + model_name) ----------------
    let mut agent = Agent::builder()
        .model_provider(Box::new(provider)) // #3 ModelProvider
        .model_name(model.clone()) // avoid the "<unconfigured>" sentinel
        .model_provider_name("ollama".to_string()) // optional, cosmetic
        .observer(Arc::new(NoopObserver)) // #4 Observer
        .tool_dispatcher(Box::new(NativeToolDispatcher)) // #5 ToolDispatcher
        .tools(vec![]) // #1 tools — empty for v0
        .exclude_memory(true) // #2 memory — skip; build() substitutes NoneMemory
        .build()?;

    eprintln!("[minimal_agent] model={model} base_url={base_url}");
    eprintln!("[minimal_agent] prompt: {prompt}");

    // --- Run a single turn ---------------------------------------------------
    // Agent::turn(&mut self, &str) -> Result<String> is self-contained: it does
    // NOT require channel/cron/MCP registration.
    let reply = agent.turn(&prompt).await?;

    println!("{reply}");
    Ok(())
}
