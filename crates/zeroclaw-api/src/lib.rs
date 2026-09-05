//! ZeroClaw API layer — trait definitions and shared types.

pub mod agent;
pub mod attribution;
pub mod channel;
pub mod elicitation;
pub mod hook;
pub mod ingress;
pub mod jsonrpc;
pub mod media;
pub mod memory_traits;
pub mod model_provider;
pub mod observability_traits;
pub mod peripherals_traits;
pub mod plan;
pub mod platform;
pub mod plugin;
pub mod principal;
pub mod runtime_status;
pub mod runtime_traits;
pub mod schema;
pub mod session_keys;
pub mod tool;
pub mod vad;

/// Reserved names for tools that manage durable session-prompt attachments.
/// Every policy and presentation boundary must use this single vocabulary.
pub const SESSION_PROMPT_TOOL_NAMES: [&str; 3] = [
    "session_prompt_list",
    "session_prompt_set",
    "session_prompt_delete",
];

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

    /// Capability marker for primary durable chat turns. It is deliberately
    /// absent from ACP, one-shot, delegate, cron, and auxiliary executions so
    /// session prompt tools cannot create unsupported attachment records.
    pub static TOOL_LOOP_SESSION_PROMPTS_ALLOWED: bool;

    /// Native extended thinking parameters, set by the outer orchestration
    /// functions and read by `run_tool_call_loop` when building `ChatRequest`.
    pub static NATIVE_THINKING_OVERRIDE: Option<crate::model_provider::NativeThinkingParams>;
}
