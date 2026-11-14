//! ZeroClaw API layer — trait definitions and shared types.

pub mod a2a_wire;
pub mod agent;
pub mod asr;
pub mod attribution;
pub mod capability;
pub mod channel;
pub mod conversation_binding;
pub mod decision;
pub mod elicitation;
pub mod embedding;
pub mod grants;
pub mod hook;
pub mod ingress;
pub mod jsonrpc;
pub mod lifecycle;
pub mod media;
pub mod memory_traits;
pub mod model_provider;
pub mod observability_traits;
pub mod peripherals_traits;
pub mod plan;
pub mod platform;
pub mod plugin;
pub mod plugin_egress;
pub mod plugin_key;
pub mod principal;
pub mod runtime_status;
pub mod runtime_traits;
pub mod schema;
pub mod session_keys;
pub mod tool;
pub mod tool_carrier;
pub mod tts;
pub mod turn_stop;
pub mod vad;
pub mod webhook;

use std::sync::Arc;

/// Admitted model policy for extracting protocol data embedded in text.
#[derive(Debug, Clone, Copy, Default, PartialEq, Eq)]
pub struct TextExtraction {
    pub reasoning: bool,
    pub tool_calls: bool,
}

/// Resolver over the admitted configuration generation. No provider keeps a
/// second copy of model parsing policy.
pub type TextExtractionResolver = Arc<dyn Fn(&str) -> TextExtraction + Send + Sync>;

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

    /// The conversation the current turn is serving, when its surface
    /// supports conversation binding. Scoped beside `TOOL_LOOP_SESSION_KEY`
    /// by that surface; read by tools that record a binding.
    pub static TOOL_LOOP_ACTIVE_CONVERSATION:
        Option<crate::conversation_binding::ActiveConversation>;

    /// Native extended thinking parameters, set by the outer orchestration
    /// functions and read by `run_tool_call_loop` when building `ChatRequest`.
    pub static NATIVE_THINKING_OVERRIDE: Option<crate::model_provider::NativeThinkingParams>;

    /// Per-call response parsing policy. The runtime admits this from the
    /// selected Config generation; provider/reliability code reads it while
    /// classifying a response and never stores a config snapshot.
    pub static TEXT_EXTRACTION_RESOLVER:
        Option<TextExtractionResolver>;
}

pub async fn scope_text_extraction<F>(
    resolver: Option<TextExtractionResolver>,
    future: F,
) -> F::Output
where
    F: std::future::Future,
{
    TEXT_EXTRACTION_RESOLVER.scope(resolver, future).await
}

pub fn text_extraction_for(model: &str) -> TextExtraction {
    TEXT_EXTRACTION_RESOLVER
        .try_with(|resolver| {
            resolver
                .as_ref()
                .map(|resolve| resolve(model))
                .unwrap_or_default()
        })
        .unwrap_or_default()
}
