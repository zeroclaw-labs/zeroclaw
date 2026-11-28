//! Post-turn memory consolidation for chat sessions.

use zeroclaw_api::model_provider::ModelProvider;
use zeroclaw_config::schema::Config;
use zeroclaw_runtime::agent::agent::build_session_model_provider;

/// Resolve provider, model, and temperature for post-turn memory consolidation
/// from the live provider reference the agent reports after the turn.
///
/// A mid-session model switch replaces the agent's active provider and model
/// together without touching the agent's configured default, so consolidation
/// must be built from that live pair. Returns `None` when the reference no
/// longer resolves (e.g. removed by config reload or not a dotted
/// `<family>.<alias>`), in which case consolidation is skipped rather than
/// silently rerouted to an unrelated entry.
pub fn build_consolidation_model(
    config: &Config,
    provider_ref: &str,
    model: &str,
) -> Option<(Box<dyn ModelProvider>, String, Option<f64>)> {
    let (provider_type, provider_alias) = provider_ref.split_once('.')?;
    let entry = config
        .providers
        .models
        .find(provider_type, provider_alias)?;
    let (provider, _, resolved_model, _) =
        build_session_model_provider(config, provider_ref, Some(model)).ok()?;
    Some((provider, resolved_model, entry.temperature))
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn build_consolidation_model_skips_missing_or_invalid_provider_refs() {
        let config = Config::default();
        assert!(build_consolidation_model(&config, "not-dotted", "m").is_none());
        assert!(build_consolidation_model(&config, "ollama.missing", "m").is_none());
    }
}
