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
pub mod attribution;
pub mod channel;
pub mod jsonrpc;
pub mod media;
pub mod memory_traits;
pub mod model_provider;
pub mod observability_traits;
pub mod peripherals_traits;
pub mod runtime_traits;
pub mod schema;
pub mod session_keys;
pub mod tool;
pub mod vad;

/// Private re-export root for macros defined in this crate. External
/// crates must not reach for `tracing` through here — it exists solely
/// so `spawn!` can expand without callers needing `tracing` as a
/// direct dependency.
#[doc(hidden)]
pub mod __private {
    pub use ::tracing;
}

/// `tokio::spawn` that propagates the caller's current tracing span
/// into the spawned task. Use this anywhere a per-request / per-turn
/// child task needs to inherit the parent's attribution context
/// (session key, channel, agent, etc.) so log records emitted from
/// the task land attributed instead of orphaning.
///
/// Layering note: this lives in `zeroclaw-api` (the lowest crate in
/// the workspace) so every implementation crate can reach it without
/// inverting the dep graph. The macro itself only depends on `tokio`
/// and `tracing`, both of which `zeroclaw-api` already pulls in.
#[macro_export]
macro_rules! spawn {
    ($body:expr) => {{
        #[allow(unused_imports)]
        use $crate::__private::tracing::Instrument as _;
        #[allow(clippy::disallowed_methods)]
        let __zc_spawn_handle = ::tokio::spawn(($body).in_current_span());
        __zc_spawn_handle
    }};
}

tokio::task_local! {
    /// Current thread/sender ID for per-sender rate limiting.
    /// Set by the agent loop, read by SecurityPolicy.
    pub static TOOL_LOOP_THREAD_ID: Option<String>;

    /// Override for tool choice mode, set by the agent loop.
    /// Read by model_providers that support native tool calling.
    pub static TOOL_CHOICE_OVERRIDE: Option<String>;

    /// Session key for the currently active session.
    /// Scoped by gateway and channel turns, read by SessionsCurrentTool.
    pub static TOOL_LOOP_SESSION_KEY: Option<String>;
}
