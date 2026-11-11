//! UI editing boundary for the canonical registry. Exact map keys remain JSON
//! keys throughout; no flattened property path is used to address an entity.

use anyhow::{Context, Result, ensure};
use serde::{Deserialize, Serialize};
use serde_json::Value;

use crate::{schema::Config, traits::MASKED_SECRET};

#[derive(Clone, Serialize, Deserialize)]
#[cfg_attr(feature = "schema-export", derive(schemars::JsonSchema))]
#[serde(deny_unknown_fields)]
pub struct RegistryEdit {
    /// Masked document originally read by the editor. Compared under the
    /// existing config commit lock so a stale editor cannot overwrite changes.
    pub expected: Value,
    /// Complete canonical registry candidate, including projected legacy entries.
    pub registry: Value,
}

/// A request-time, masked projection of the canonical source, never a cache.
pub fn snapshot(config: &Config) -> Result<Value> {
    let mut registry = config.provider_registry()?;
    for connection in registry.connections.values_mut() {
        connection.mask_secrets();
    }
    let mut document = serde_json::to_value(registry)?;
    for section in ["connections", "models", "model_presets", "fallback"] {
        document[section] = document
            .get(section)
            .cloned()
            .unwrap_or_else(|| serde_json::json!({}));
    }
    Ok(document)
}

/// Stage a complete, validated replacement on a caller-owned config candidate.
/// No disk or live config changes occur here. Unchanged masked secrets are
/// restored from the latest admitted config using the normal secret lifecycle.
pub fn stage(config: &mut Config, edit: RegistryEdit) -> Result<()> {
    ensure!(
        snapshot(config)? == edit.expected,
        "provider registry changed; reload before saving"
    );
    let mut registry: super::ProviderRegistry =
        serde_json::from_value(edit.registry).context("invalid provider registry")?;
    // `snapshot` is built from the projected registry, so legacy typed
    // providers appear as generated `legacy.*` connections even though they
    // are not stored in `providers.connections`.  Use that same projection to
    // restore masked credentials during an unchanged edit.
    let projected_current = config.provider_registry()?;
    for (id, connection) in &mut registry.connections {
        if let Some(current) = config
            .providers
            .connections
            .get(id)
            .or_else(|| projected_current.connections.get(id))
        {
            connection.restore_secrets_from(current);
        }
        connection.validate_editor_secrets()?;
    }
    let raw_models = serde_json::from_value(serde_json::to_value(&registry.models)?)?;
    let mut candidate = config.clone();
    // An editor save promotes the complete projection to canonical storage.
    // Keeping the legacy tables would create a second source for edited
    // connections and regenerate deleted models on the next read. Retain the
    // projected definitions themselves, including restored credentials.
    let mut agents_changed = false;
    for agent in candidate.agents.values_mut() {
        if let Some(reference) = projected_speech_reference(
            &projected_current,
            agent.tts_provider.as_str(),
            zeroclaw_api::capability::Capability::Tts,
            "tts",
        ) {
            agent.tts_provider = reference.into();
            agents_changed = true;
        }
        if let Some(reference) = projected_speech_reference(
            &projected_current,
            agent.transcription_provider.as_str(),
            zeroclaw_api::capability::Capability::Asr,
            "asr",
        ) {
            agent.transcription_provider = reference.into();
            agents_changed = true;
        }
    }
    candidate.providers.connections = registry.connections;
    candidate.providers.models = Default::default();
    candidate.providers.tts = Default::default();
    candidate.providers.transcription = Default::default();
    candidate.providers.models.canonical_models = raw_models;
    candidate.providers.model_presets = registry.model_presets;
    candidate.providers.fallback = registry.fallback;
    serde_json::to_value(&candidate.providers)?;
    candidate.validate()?;
    // Memory's canonical model reference is outside the provider graph. Do
    // not admit deletion or a capability change while that consumer still
    // points at the model: its live embedder must never retain an old view.
    if let Some(reference) = candidate
        .memory
        .embedding_model_ref
        .as_deref()
        .map(str::trim)
        .filter(|reference| !reference.is_empty())
    {
        candidate
            .provider_registry()?
            .resolve_embedding(&super::ModelRef::from(reference))
            .context("invalid memory.embedding_model_ref")?;
    }
    candidate.mark_dirty("providers");
    if agents_changed {
        candidate.mark_dirty("agents");
    }
    *config = candidate;
    Ok(())
}

fn projected_speech_reference(
    registry: &super::ProviderRegistry,
    reference: &str,
    capability: zeroclaw_api::capability::Capability,
    prefix: &str,
) -> Option<String> {
    if reference.is_empty()
        || registry
            .models
            .get(&super::ModelRef::from(reference))
            .is_some_and(|model| model.capability() == capability)
    {
        return None;
    }
    let projected = super::ModelRef::from(format!("{prefix}.{reference}"));
    registry
        .models
        .get(&projected)
        .is_some_and(|model| model.capability() == capability)
        .then(|| projected.to_string())
}

impl super::ProviderConnection {
    fn validate_editor_secrets(&mut self) -> Result<()> {
        self.with_secret_json_for_editor(|secret| {
            ensure!(
                secret != MASKED_SECRET && secret != "****",
                "masked secret has no existing credential to preserve"
            );
            Ok(())
        })
    }
}
