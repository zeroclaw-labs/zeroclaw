#[allow(clippy::module_inception)]
pub mod agent;
pub(crate) mod approval_bridge;
pub mod classifier;
pub mod context_analyzer;
pub mod cost;
pub mod dispatcher;
pub mod eval;
pub mod execution_tree_budget;
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
    ContextWindowExceeded, append_safeguard_fallback_notice, context_window_exceeded_from_error,
    is_semantic_empty_terminal_completion,
    media_degrade::{
        degrade_media_in_message, degrade_media_in_messages, is_turn_opening_user_message,
    },
    redact::{is_credential_key, scrub_credentials_value},
    semantic_empty_terminal_completion_message, terminal_completion_error_message,
};
pub use turn::{SteeringAdmission, SteeringAdmit, SteeringInput, SteeringPosture};

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
pub use loop_::{process_message, process_message_with_live_config, run};

/// Resolve the context windows the shared model catalog knows for this config's
/// routes, and publish them for the process.
///
/// The window is model metadata, so an operator never has to supply it. This
/// runs once, at startup, from the composition root: the turn path only reads
/// the published snapshot.
///
/// Best effort by design. A model the catalog does not know (or a failed fetch)
/// leaves the route unknown, and unknown means "no local cap", so the cost of a
/// miss is lost proactive trimming, never a wrong refusal.
pub async fn publish_catalog_context_windows(config: &zeroclaw_config::schema::Config) {
    let mut entries = Vec::new();
    for (alias, agent) in &config.agents {
        // An explicit window is an override; leave it alone.
        if config.configured_model_context_window(alias).is_some() {
            continue;
        }
        let provider_ref = agent.model_provider.as_str();
        let Some((provider_type, _)) = provider_ref.split_once('.') else {
            continue;
        };
        let Some(catalog_key) = zeroclaw_providers::catalog::catalog_source_for(provider_type)
            .and_then(|(models_dev, _)| models_dev)
        else {
            continue;
        };
        let Some(model) = config
            .model_provider_for_agent(alias)
            .and_then(|provider| provider.model.clone())
        else {
            continue;
        };
        let Some(tokens) =
            zeroclaw_providers::models_dev::context_window_for(catalog_key, &model).await
        else {
            continue;
        };
        ::zeroclaw_log::record!(
            INFO,
            ::zeroclaw_log::Event::new(module_path!(), ::zeroclaw_log::Action::Note)
                .with_category(::zeroclaw_log::EventCategory::System)
                .with_outcome(::zeroclaw_log::EventOutcome::Success)
                .with_attrs(::serde_json::json!({
                    "agent": alias,
                    "model_provider": provider_ref,
                    "model": model,
                    "context_window": tokens,
                    "context_window_source": "discovered",
                })),
            "context window discovered from the model catalog"
        );
        entries.push((provider_ref.to_string(), model, tokens));
    }
    zeroclaw_config::schema::publish_discovered_context_windows(entries);
}

#[cfg(test)]
mod tests;
