#[allow(clippy::module_inception)]
pub mod agent;
pub(crate) mod approval_bridge;
pub mod classifier;
pub mod context_analyzer;
pub mod cost;
pub mod dispatcher;
pub mod eval;
pub mod history;
pub mod history_pruner;
pub mod history_trim;
pub mod loop_;
pub mod loop_detector;
pub mod memory_inject;
pub mod memory_strategy;
pub mod personality;
pub mod personality_templates;
pub mod pricing_catalog;
pub mod prompt;
pub mod system_prompt;
pub mod thinking;
pub(crate) mod tool_call_format;
pub mod tool_execution;
pub mod tool_receipts;
pub(crate) mod turn;

pub use turn::context::TurnMeta;
pub use turn::{
    is_semantic_empty_terminal_completion,
    redact::{is_credential_key, scrub_credentials_value},
    semantic_empty_terminal_completion_message, terminal_completion_error_message,
};

/// Tools whose execution policy consumes a runtime-owned `approved` bit.
///
/// The `approved` arg is runtime plumbing, NOT a model-facing parameter: no
/// tool schema advertises it (RFC 7155 — a model must never be told it can
/// self-approve). [`set_runtime_approved_arg`] is the only writer on the tool
/// loop path: `call_prep` overwrites the key unconditionally before the
/// approval gate (stripping any model-supplied value) and rewrites it with the
/// gate's decision after, so a model-supplied `approved` can never survive
/// into tool execution.
pub(crate) fn is_runtime_approved_arg_tool(tool_name: &str) -> bool {
    matches!(
        tool_name,
        "shell" | "schedule" | "cron_add" | "cron_update" | "cron_run"
    )
}

/// Overwrite the runtime-owned `approved` arg for an approval-gated tool.
///
/// Callers must treat this as the sole authority for the bit: the tool loop
/// always calls it (first with `false` to strip model input, then with the
/// approval gate's decision) before dispatching the tool.
pub(crate) fn set_runtime_approved_arg(
    tool_name: &str,
    args: &mut serde_json::Value,
    approved: bool,
) {
    if is_runtime_approved_arg_tool(tool_name)
        && let Some(args) = args.as_object_mut()
    {
        args.insert("approved".to_string(), serde_json::Value::Bool(approved));
    }
}

/// Borrow-only Attributable holding an agent alias.
/// Used by entry points (loop_::run, process_message, cron dispatch)
/// that don't construct a full `Agent` but still need to open an
/// `attribution_span!` carrying the agent's role + alias.
pub struct AgentAttribution<'a>(pub &'a str);

impl ::zeroclaw_api::attribution::Attributable for AgentAttribution<'_> {
    fn role(&self) -> ::zeroclaw_api::attribution::Role {
        ::zeroclaw_api::attribution::Role::Agent
    }
    fn alias(&self) -> &str {
        self.0
    }
}

#[cfg(test)]
mod tests;

#[allow(unused_imports)]
pub use agent::{Agent, AgentBuilder, TurnEvent};
#[allow(unused_imports)]
pub use loop_::{process_message, run};
