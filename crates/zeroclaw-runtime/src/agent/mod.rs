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

pub use turn::context::TurnMeta;

/// The built-in tools whose `approved` argument the runtime owns.
///
/// Only a fallback for when a call cannot be resolved to a tool in this turn's
/// registry. [`schema_declares_approved_arg`] is the real answer, and covers
/// tools whose names are not known at compile time.
pub(crate) fn is_runtime_approved_arg_tool(tool_name: &str) -> bool {
    matches!(
        tool_name,
        "shell" | "schedule" | "cron_add" | "cron_update" | "cron_run"
    )
}

/// Whether a tool's parameter schema declares an `approved` boolean.
///
/// Declaring one is a tool saying its approval decision comes from the host.
/// Deriving it from the schema rather than a name list is what lets skill shell
/// tools — named `skill__tool`, so unknowable at compile time — take part: they
/// would otherwise have to self-approve, which is how a `locked_down` agent
/// could run a medium-risk command through a skill that the shell tool refuses.
pub(crate) fn schema_declares_approved_arg(schema: &serde_json::Value) -> bool {
    schema
        .get("properties")
        .and_then(|properties| properties.get("approved"))
        .and_then(|approved| approved.get("type"))
        .is_some_and(|kind| kind == "boolean")
}

/// Overwrite the model's `approved` argument with the host's decision.
///
/// `host_owned` comes from [`crate::agent::tool_execution::host_owns_approved_arg`].
/// Unconditionally inserting would put an `approved` key into tools that never
/// declared one, which strict-schema surfaces (MCP) reject.
pub(crate) fn set_approved_arg(host_owned: bool, args: &mut serde_json::Value, approved: bool) {
    if host_owned && let Some(args) = args.as_object_mut() {
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
