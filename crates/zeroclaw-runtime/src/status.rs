//! Parts of the dashboard status overview that come from configuration
//! alone. The core's `status` overview and the in-process gateway's
//! `/api/status` both build their answer from these, so the two cannot
//! drift apart.

use std::collections::BTreeMap;

use zeroclaw_config::schema::Config;

/// The model fields of the status overview.
#[derive(Debug, Clone, PartialEq)]
pub struct ModelView {
    /// Dotted `<type>.<alias>` of the model provider, or `None` when none is
    /// configured.
    pub model_provider: Option<String>,
    /// The resolved model; empty when none is configured.
    pub model: String,
    pub temperature: Option<f64>,
    pub memory_backend: String,
}

/// The model fields resolved for the configured agent `alias`, or `None`
/// when no agent has that alias.
pub fn agent_model_view(config: &Config, alias: &str) -> Option<ModelView> {
    let agent = config.agent(alias)?;
    let model_provider = if agent.model_provider.is_empty() {
        None
    } else {
        Some(agent.model_provider.as_str().to_string())
    };
    let resolved = config.resolved_model_provider_for_agent(alias);
    let model = resolved
        .as_ref()
        .and_then(|(_, _, cfg)| cfg.model.clone())
        .unwrap_or_default();
    let temperature = resolved.as_ref().and_then(|(_, _, cfg)| cfg.temperature);
    let backend_kind = agent.memory.backend;
    let memory_backend = serde_json::to_value(backend_kind)
        .ok()
        .and_then(|v| v.as_str().map(String::from))
        .unwrap_or_else(|| format!("{backend_kind:?}").to_lowercase());
    Some(ModelView {
        model_provider,
        model,
        temperature,
        memory_backend,
    })
}

/// The install-wide model provider: the first configured entry, as
/// `<type>.<alias>`.
pub fn install_wide_model_provider(config: &Config) -> Option<String> {
    config
        .providers
        .models
        .iter_entries()
        .next()
        .map(|(ty, alias, _)| format!("{ty}.{alias}"))
}

/// The install-wide model and temperature: those of the first entry that
/// declares a model, which is the entry a daemon seeds its default provider
/// from. An empty model when no entry declares one.
pub fn install_wide_model(config: &Config) -> (String, Option<f64>) {
    let entry = config
        .providers
        .models
        .first_entry_with_model()
        .map(|(_, _, entry)| entry);
    let model = entry
        .and_then(|e| e.model.as_deref())
        .map(str::trim)
        .filter(|m| !m.is_empty())
        .map(ToString::to_string)
        .unwrap_or_default();
    (model, entry.and_then(|e| e.temperature))
}

/// Every configured channel instance, keyed `<type>.<alias>`.
pub fn channel_rows(config: &Config) -> BTreeMap<String, bool> {
    config
        .channels_by_alias()
        .into_iter()
        .map(|info| (format!("{}.{}", info.channel_type, info.alias), true))
        .collect()
}

/// The configured locale, or the one detected from the environment.
pub fn locale(config: &Config) -> String {
    config
        .locale
        .as_deref()
        .filter(|s| !s.is_empty())
        .map(String::from)
        .unwrap_or_else(crate::i18n::detect_locale)
}
