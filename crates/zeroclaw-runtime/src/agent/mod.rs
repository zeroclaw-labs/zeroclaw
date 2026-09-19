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
    append_safeguard_fallback_notice, is_semantic_empty_terminal_completion,
    media_degrade::{
        degrade_media_in_message, degrade_media_in_messages, is_turn_opening_user_message,
    },
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

/// Runtime-only confirmation handle. This key is never advertised in a tool
/// schema and is stripped/replaced by the tool loop before dispatch.
pub(crate) const RUNTIME_CONFIRMATION_ID_ARG: &str = "__zeroclaw_confirmation_id";
pub(crate) const RUNTIME_CONFIRMATION_FINGERPRINT_ARG: &str = "__zeroclaw_confirmation_fingerprint";
pub(crate) const RUNTIME_CONFIRMATION_EXPIRES_AT_ARG: &str = "__zeroclaw_confirmation_expires_at";
pub(crate) const RUNTIME_POLICY_ALLOW_ARG: &str = "__zeroclaw_policy_allowed";
pub(crate) const RUNTIME_AUTHORIZATION_REJECTED_ARG: &str = "__zeroclaw_authorization_rejected";

/// Remove shell authorization plumbing before arguments cross any
/// presentation, logging, hook, receipt, or persistence boundary. Execution
/// keeps a separate copy and reattaches trusted values immediately before the
/// tool wrapper stack.
pub(crate) fn strip_runtime_authorization_args(tool_name: &str, args: &mut serde_json::Value) {
    if tool_name != "shell" {
        return;
    }
    if let Some(args) = args.as_object_mut() {
        args.remove("approved");
        args.remove(RUNTIME_CONFIRMATION_ID_ARG);
        args.remove(RUNTIME_CONFIRMATION_FINGERPRINT_ARG);
        args.remove(RUNTIME_CONFIRMATION_EXPIRES_AT_ARG);
        args.remove(RUNTIME_POLICY_ALLOW_ARG);
        args.remove(RUNTIME_AUTHORIZATION_REJECTED_ARG);
        args.remove("__zeroclaw_confirmation_consumed");
    }
}

pub(crate) fn visible_tool_arguments(
    tool_name: &str,
    args: &serde_json::Value,
) -> serde_json::Value {
    let mut visible = args.clone();
    strip_runtime_authorization_args(tool_name, &mut visible);
    visible
}

pub(crate) fn set_runtime_confirmation_id(
    tool_name: &str,
    args: &mut serde_json::Value,
    confirmation_id: Option<uuid::Uuid>,
) {
    if is_runtime_approved_arg_tool(tool_name)
        && let Some(args) = args.as_object_mut()
    {
        args.remove(RUNTIME_CONFIRMATION_ID_ARG);
        args.remove(RUNTIME_CONFIRMATION_FINGERPRINT_ARG);
        args.remove(RUNTIME_CONFIRMATION_EXPIRES_AT_ARG);
        args.remove(RUNTIME_POLICY_ALLOW_ARG);
        args.remove(RUNTIME_AUTHORIZATION_REJECTED_ARG);
        if let Some(id) = confirmation_id {
            args.insert(
                RUNTIME_CONFIRMATION_ID_ARG.to_string(),
                serde_json::Value::String(id.to_string()),
            );
        }
    }
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

#[allow(unused_imports)]
pub use agent::{Agent, AgentBuilder, TurnEvent};
#[allow(unused_imports)]
pub use loop_::{process_message, run};

#[cfg(test)]
mod tests;
