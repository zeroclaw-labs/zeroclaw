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
pub mod principal;
pub mod runtime_status;
pub mod runtime_traits;
pub mod schema;
pub mod session_keys;
pub mod tool;
pub mod vad;

/// Marks a process spawned by an agent's shell.
///
/// Set on every command a tool spawns on a model's behalf; read by the
/// `zeroclaw` CLI, which refuses to run when it sees it.
///
/// The CLI carries the *operator's* authority — it loads and rewrites
/// `config.toml` with no agent policy anywhere in the process. So an agent that
/// can execute it does not act with its own privileges, it acts with yours.
/// `balanced`, the default preset, allows any command name (`allowed_commands =
/// ["*"]`) and `zeroclaw` is classified low-risk, so nothing else in the policy
/// stops the call.
///
/// Set unconditionally. [`TOOL_LOOP_SESSION_KEY`] looks like the same signal but
/// is absent on unscoped turns, which would leave exactly the one-shot and
/// webhook paths unmarked.
///
/// This binds the *caller*, not the command name, so copying or renaming the
/// binary does not shake it off. An agent under a wildcard policy can still
/// clear the variable deliberately (`env -u`); that is a bar to clear and a
/// thing to alarm on, not a proof.
pub const AGENT_SHELL_ENV_VAR: &str = "ZEROCLAW_AGENT_SHELL";

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

    /// Native extended thinking parameters, set by the outer orchestration
    /// functions and read by `run_tool_call_loop` when building `ChatRequest`.
    pub static NATIVE_THINKING_OVERRIDE: Option<crate::model_provider::NativeThinkingParams>;
}
