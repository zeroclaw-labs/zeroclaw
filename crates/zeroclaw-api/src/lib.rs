//! ZeroClaw API layer — trait definitions and shared types.

pub mod a2a_wire;
pub mod agent;
pub mod attribution;
pub mod channel;
pub mod conversation_binding;
pub mod elicitation;
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
pub mod turn_stop;
pub mod vad;
pub mod webhook;

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

    /// Selected native reasoning level for the current turn, scoped by its
    /// orchestration owner and copied into each provider `ChatRequest`.
    /// Independent of the legacy token-budget override above.
    pub static NATIVE_THINKING_LEVEL: Option<crate::model_provider::ThinkingLevel>;

    /// Native level for a single-call delegate's legacy `chat_with_system`
    /// request. Only that delegate call scopes it; OpenAI leaves read it there
    /// instead of the turn carrier so auxiliary one-shot calls stay isolated.
    pub static ONE_SHOT_NATIVE_THINKING_LEVEL: Option<crate::model_provider::ThinkingLevel>;
}

/// Read the native level bound to this turn, or `None` outside a turn scope.
pub fn current_native_thinking_level() -> Option<model_provider::ThinkingLevel> {
    NATIVE_THINKING_LEVEL
        .try_with(|level| *level)
        .ok()
        .flatten()
}

/// Read the native level bound to a single-call delegate request.
pub fn current_one_shot_native_thinking_level() -> Option<model_provider::ThinkingLevel> {
    ONE_SHOT_NATIVE_THINKING_LEVEL
        .try_with(|level| *level)
        .ok()
        .flatten()
}
