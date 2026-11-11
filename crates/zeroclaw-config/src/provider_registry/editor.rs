//! UI editing boundary for the canonical registry. Exact map keys remain JSON
//! keys throughout; no flattened property path is used to address an entity.

use anyhow::{Context, Result, ensure};
use serde::{Deserialize, Serialize};
use serde_json::Value;

#[path = "editor_mutations.rs"]
mod mutations;
pub use mutations::{create_entry, delete_entry, rename_entry};

use crate::{schema::Config, traits::MASKED_SECRET};

#[derive(Clone, Serialize, Deserialize)]
#[cfg_attr(feature = "schema-export", derive(schemars::JsonSchema))]
#[serde(deny_unknown_fields)]
pub struct RegistryEdit {
    /// Masked document originally read by the editor. Compared under the
    /// existing config commit lock so a stale editor cannot overwrite changes.
    pub expected: Value,
    /// Complete canonical registry candidate.
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

/// Edit exact registry keys on a caller-owned candidate using the same
/// validation and credential preservation as the UI.
/// Persistence and live publication remain the caller's transaction.
pub fn update(
    config: &mut Config,
    edit: impl FnOnce(&mut super::ProviderRegistry) -> Result<()>,
) -> Result<()> {
    let expected = snapshot(config)?;
    let mut registry = config.provider_registry()?;
    edit(&mut registry)?;
    stage(
        config,
        RegistryEdit {
            expected,
            registry: serde_json::to_value(registry)?,
        },
    )
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
    for (id, connection) in &mut registry.connections {
        if let Some(current) = config.providers.connections.get(id) {
            connection.restore_secrets_from(current);
        }
        connection.validate_editor_secrets()?;
    }
    let mut candidate = config.clone();
    candidate.providers.connections = registry.connections;
    candidate.providers.models.canonical_models =
        serde_json::from_value(serde_json::to_value(registry.models)?)?;
    candidate.providers.model_presets = registry.model_presets;
    candidate.providers.fallback = registry.fallback;
    let previous = serde_json::to_value(&config.providers)?;
    let admitted = serde_json::to_value(&candidate.providers)?;
    // Keep external references only for credentials that were left untouched.
    // Removed/changed credentials must never be recreated during save.
    candidate.onepassword_reference_snapshots.retain(|path, _| {
        path.strip_prefix("/providers").is_none_or(|pointer| {
            previous.pointer(pointer).is_some()
                && previous.pointer(pointer) == admitted.pointer(pointer)
        })
    });
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
    *config = candidate;
    Ok(())
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
