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

/// Resolve the live provider's explicit `context_window`, or `None`.
/// Both RPC dispatch and the gateway WS path call this so wire
/// emissions always reflect the provider that served the turn.
pub fn resolve_live_model_context_window(
    config: &zeroclaw_config::schema::Config,
    live_provider_ref: &str,
) -> Option<u64> {
    config
        .model_provider_context_window_opt(live_provider_ref)
        .map(|v| v as u64)
}

#[cfg(test)]
mod tests {
    use super::*;
    use zeroclaw_config::schema::Config;

    #[test]
    fn resolve_live_model_context_window_returns_window_when_provider_set() {
        let mut cfg = Config::default();
        let provider = cfg
            .providers
            .models
            .ensure("openai", "provider-a")
            .expect("ensure provider A");
        provider.context_window = Some(128_000);

        let res = resolve_live_model_context_window(&cfg, "openai.provider-a");
        assert_eq!(res, Some(128_000));
    }

    #[test]
    fn resolve_live_model_context_window_returns_none_when_provider_unset() {
        let mut cfg = Config::default();
        cfg.providers
            .models
            .ensure("openai", "provider-b")
            .expect("ensure provider B");
        // No context_window set

        let res = resolve_live_model_context_window(&cfg, "openai.provider-b");
        assert_eq!(res, None);
    }

    #[test]
    fn resolve_live_model_context_window_returns_none_for_empty_ref() {
        let cfg = Config::default();
        let res = resolve_live_model_context_window(&cfg, "");
        assert_eq!(res, None);
    }
}
