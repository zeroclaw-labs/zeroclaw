#[allow(clippy::module_inception)]
pub mod agent;
pub mod classifier;
pub mod context_analyzer;
pub mod context_compressor;
pub mod cost;
pub mod dispatcher;
pub mod eval;
pub mod history;
pub mod history_pruner;
pub mod loop_;
pub mod loop_detector;
pub mod memory_loader;
pub mod personality;
pub mod personality_templates;
pub mod prompt;
pub mod system_prompt;
pub mod thinking;
pub mod tool_execution;
pub mod tool_receipts;

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

#[cfg(test)]
mod tests;

#[allow(unused_imports)]
pub use agent::{Agent, AgentBuilder, TurnEvent};
#[allow(unused_imports)]
pub use loop_::{process_message, run};
