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
pub mod tool_execution;
pub mod tool_receipts;
pub(crate) mod turn;

pub(crate) fn is_runtime_approved_arg_tool(tool_name: &str) -> bool {
    matches!(
        tool_name,
        "shell" | "schedule" | "cron_add" | "cron_update" | "cron_run"
    )
}

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

pub(crate) fn set_fresh_confirmation_approved_arg(
    confirmation_requirement: zeroclaw_api::tool::ConfirmationRequirement,
    args: &mut serde_json::Value,
    approved: bool,
) {
    if confirmation_requirement == zeroclaw_api::tool::ConfirmationRequirement::Fresh
        && let Some(args) = args.as_object_mut()
    {
        args.insert("approved".to_string(), serde_json::Value::Bool(approved));
    }
}

pub(crate) fn set_computer_use_approval_context(
    tool_name: &str,
    args: &mut serde_json::Value,
    context: Option<zeroclaw_api::tool::ApprovalContext>,
) {
    if tool_name != "computer_use" {
        return;
    }
    let Some(args) = args.as_object_mut() else {
        return;
    };
    args.remove(zeroclaw_api::tool::APPROVAL_CONTEXT_ARG);
    if let Some(context) = context
        && let Ok(value) = serde_json::to_value(context)
    {
        args.insert(zeroclaw_api::tool::APPROVAL_CONTEXT_ARG.to_string(), value);
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
