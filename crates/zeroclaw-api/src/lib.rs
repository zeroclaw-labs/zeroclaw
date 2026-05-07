//! ZeroClaw API layer — trait definitions and shared types.
//!
//! This crate defines the fundamental abstractions that all ZeroClaw subsystems
//! depend on. No implementations, no heavy dependencies. Every other crate in
//! the workspace depends on this. The compiler enforces that no implementation
//! crate can import another without going through these interfaces.
//!
//! ## Traits
//! - [`model_provider::ModelProvider`] — LLM inference backends
//! - [`channel::Channel`] — messaging platform integrations
//! - [`tool::Tool`] — agent-callable capabilities
//! - [`memory_traits::Memory`] — conversation memory backends
//! - [`observability_traits::Observer`] — metrics and tracing
//! - [`runtime_traits::RuntimeAdapter`] — execution environment adapters
//! - [`peripherals_traits::Peripheral`] — hardware board integrations

pub mod agent;
pub mod channel;
pub mod media;
pub mod memory_traits;
pub mod model_provider;
pub mod observability_traits;
pub mod peripherals_traits;
pub mod runtime_traits;
pub mod schema;
pub mod tool;
pub mod vad;

tokio::task_local! {
    /// Current thread/sender ID for per-sender rate limiting.
    /// Set by the agent loop, read by SecurityPolicy.
    pub static TOOL_LOOP_THREAD_ID: Option<String>;

    /// Override for tool choice mode, set by the agent loop.
    /// Read by model_providers that support native tool calling.
    pub static TOOL_CHOICE_OVERRIDE: Option<String>;

    /// Session key for the currently active session.
    /// Set by the gateway WebSocket handler, read by SessionsCurrentTool.
    pub static TOOL_LOOP_SESSION_KEY: Option<String>;
}
