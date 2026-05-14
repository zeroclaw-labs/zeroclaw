//! Per-slot agent config overrides.
//!
//! M2 (pragmatic slice) stops short of the warm `SlotRegistry` + shared
//! `Arc<McpRegistry>` refactor specified in `multi-session-dashboard.md
//! §4.5`. What lives here is the bounded piece needed for the messaging
//! handler: a pure function that clones the gateway `Config` and stamps
//! a `SlotAgentConfig`'s provider/model overrides onto it.
//!
//! The personality override is NOT a Config mutation — personalities are
//! workspace files the agent loads per-turn (see
//! `api_personality::personality_path`). M4a wires the per-slot
//! personality into the agent spawn path directly via the `_agent`
//! parameter reserved by upstream #5890.
//!
//! The helper is synchronous and pure so it can be unit-tested without
//! touching the provider ecosystem.

use zeroclaw_config::schema::Config;

use crate::slot::SlotAgentConfig;

/// Apply a slot's per-turn overrides to a base config, returning a new
/// `Config` owned by the caller.
///
/// Fields that are `None` on the override leave the base config untouched.
/// Overrides applied:
/// * `provider` — sets `config.providers.fallback` so
///   `ProvidersConfig::fallback_provider()` returns the requested provider
///   entry on subsequent reads.
/// * `model` — mutates the resolved fallback provider entry's `model`
///   field. Requires an existing entry in `providers.models` matching the
///   (possibly freshly overridden) fallback id; otherwise this is a
///   no-op and the caller falls back to the base config's model.
///
/// `mode`, `personality`, and `persona_preset` are UI/runtime concepts
/// that do not mutate the `Config` struct — callers consult the slot
/// directly for those values.
pub fn apply_slot_overrides(mut base: Config, overrides: &SlotAgentConfig) -> Config {
    if let Some(provider_id) = &overrides.provider {
        base.providers.fallback = Some(provider_id.clone());
    }

    if let Some(model_id) = &overrides.model
        && let Some(entry) = base.providers.fallback_provider_mut()
    {
        entry.model = Some(model_id.clone());
    }

    base
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::slot::{SlotAgentConfig, SlotMode};
    use zeroclaw_config::schema::{Config, ModelProviderConfig};

    fn base_with_anthropic_fallback() -> Config {
        let mut cfg = Config::default();
        cfg.providers.models.insert(
            "anthropic".to_string(),
            ModelProviderConfig {
                model: Some("default-model".to_string()),
                ..Default::default()
            },
        );
        cfg.providers.fallback = Some("anthropic".to_string());
        cfg
    }

    #[test]
    fn apply_overrides_sets_fallback_provider() {
        let mut base = base_with_anthropic_fallback();
        base.providers.models.insert(
            "openai".to_string(),
            ModelProviderConfig {
                model: Some("gpt-4".to_string()),
                ..Default::default()
            },
        );

        let overrides = SlotAgentConfig {
            provider: Some("openai".to_string()),
            model: None,
            mode: SlotMode::Normal,
            personality: None,
            persona_preset: None,
        };

        let effective = apply_slot_overrides(base, &overrides);
        assert_eq!(effective.providers.fallback.as_deref(), Some("openai"));
        // The fallback resolution now picks the openai entry.
        assert_eq!(
            effective
                .providers
                .fallback_provider()
                .and_then(|e| e.model.as_deref()),
            Some("gpt-4"),
        );
    }

    #[test]
    fn apply_overrides_sets_model_on_fallback_provider() {
        let base = base_with_anthropic_fallback();
        let overrides = SlotAgentConfig {
            provider: None,
            model: Some("claude-3-5-sonnet".to_string()),
            mode: SlotMode::Normal,
            personality: None,
            persona_preset: None,
        };
        let effective = apply_slot_overrides(base, &overrides);
        assert_eq!(
            effective
                .providers
                .fallback_provider()
                .and_then(|e| e.model.as_deref()),
            Some("claude-3-5-sonnet"),
        );
    }

    #[test]
    fn apply_overrides_combined_provider_and_model() {
        let mut base = base_with_anthropic_fallback();
        base.providers.models.insert(
            "openai".to_string(),
            ModelProviderConfig {
                model: Some("gpt-4".to_string()),
                ..Default::default()
            },
        );
        let overrides = SlotAgentConfig {
            provider: Some("openai".to_string()),
            model: Some("gpt-5".to_string()),
            mode: SlotMode::Normal,
            personality: None,
            persona_preset: None,
        };
        let effective = apply_slot_overrides(base, &overrides);
        assert_eq!(effective.providers.fallback.as_deref(), Some("openai"));
        assert_eq!(
            effective
                .providers
                .fallback_provider()
                .and_then(|e| e.model.as_deref()),
            Some("gpt-5"),
            "model override lands on the newly selected fallback provider",
        );
    }

    #[test]
    fn apply_overrides_model_no_op_when_fallback_missing() {
        let mut base = Config::default();
        base.providers.fallback = None;
        // No matching entry under fallback → model override is a no-op.
        let overrides = SlotAgentConfig {
            provider: None,
            model: Some("ignored".to_string()),
            mode: SlotMode::Normal,
            personality: None,
            persona_preset: None,
        };
        let effective = apply_slot_overrides(base, &overrides);
        assert!(effective.providers.fallback.is_none());
    }

    #[test]
    fn apply_overrides_leaves_base_when_all_none() {
        let base = base_with_anthropic_fallback();
        let before_fallback = base.providers.fallback.clone();
        let before_model = base
            .providers
            .models
            .get("anthropic")
            .and_then(|e| e.model.clone());

        let overrides = SlotAgentConfig::default();
        let after = apply_slot_overrides(base, &overrides);

        assert_eq!(after.providers.fallback, before_fallback);
        assert_eq!(
            after
                .providers
                .models
                .get("anthropic")
                .and_then(|e| e.model.clone()),
            before_model,
        );
    }
}
