//! SLM local gatekeeper system for ZeroClaw.
//!
//! The gatekeeper runs a small language model (SLM) locally via Ollama to:
//! - Classify user intent and decide routing (local vs cloud)
//! - Handle simple tasks (greetings, heartbeat checks) without cloud calls
//! - Detect privacy-sensitive patterns before data leaves the device
//! - Queue tasks for cloud delegation when offline
//!
//! ## Design
//! - Uses Ollama REST API at `http://127.0.0.1:11434/v1`
//! - Default model: Qwen3 0.6B (Q4_K_M quantization, ~400MB)
//! - All routing decisions are structured JSON for deterministic parsing
//! - Offline-capable: queues cloud tasks locally when disconnected

pub mod router;
pub mod routing_policy;

#[allow(unused_imports)]
pub use router::GatekeeperRouter;
