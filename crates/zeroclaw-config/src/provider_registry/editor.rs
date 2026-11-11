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
    let mut candidate = config.clone();
    promote_embedding_bindings(&mut candidate, &projected_current, &mut registry)?;
    let raw_models = serde_json::from_value(serde_json::to_value(&registry.models)?)?;
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

/// Materialize legacy embedding consumers before retiring their profiles.
/// The generated model owns its upstream ID/dimensions. Native embedding
/// connections are reused; chat-only kinds with an explicit compatible endpoint
/// need a separate custom connection to retain their legacy embedding protocol.
fn promote_embedding_bindings(
    config: &mut Config,
    current: &super::ProviderRegistry,
    candidate: &mut super::ProviderRegistry,
) -> Result<()> {
    fn embedding_connection(
        candidate: &mut super::ProviderRegistry,
        provider: &super::ProviderId,
    ) -> Result<super::ProviderId> {
        use zeroclaw_api::capability::Capability;

        let connection = candidate
            .connections
            .get(provider)
            .context("cannot delete a connection used by legacy embeddings")?;
        if connection
            .implemented_capabilities()
            .contains(&Capability::Embedding)
        {
            ensure!(
                connection.supports(Capability::Embedding),
                "legacy embedding connection does not support embeddings"
            );
            return Ok(provider.clone());
        }
        // The legacy embedder used any explicit profile URI as an OpenAI
        // compatible embedding endpoint, independently of the chat driver.
        // Preserve that protocol without broadening the native driver's
        // capabilities. The edited connection is the sole migration input.
        let uri = connection
            .uri()
            .map(str::trim)
            .filter(|uri| !uri.is_empty())
            .context("legacy embeddings on a chat-only connection require an explicit URI")?;
        let promoted = super::ProviderConnection::Custom(super::CompatibleConnection {
            capabilities: [Capability::Embedding].into_iter().collect(),
            uri: Some(uri.to_owned()),
            auth: Some(super::ApiKeyAuth::ApiKey {
                api_key: connection.api_key().map(str::to_owned),
            }),
            extra_headers: connection.extra_headers().cloned().unwrap_or_default(),
            tls_ca_cert_path: connection.tls_ca_cert_path().map(str::to_owned),
            api: Default::default(),
        });
        let definition = serde_json::to_value(&promoted)?;
        let base = format!("embedding.{provider}");
        let mut id = super::ProviderId::from(base.as_str());
        let mut suffix = 2;
        while let Some(existing) = candidate.connections.get(&id) {
            if serde_json::to_value(existing)? == definition {
                return Ok(id);
            }
            id = format!("{base}.{suffix}").into();
            suffix += 1;
        }
        candidate.connections.insert(id.clone(), promoted);
        Ok(id)
    }

    fn promote(
        profiles: &crate::providers::ModelProviders,
        current: &super::ProviderRegistry,
        candidate: &mut super::ProviderRegistry,
        reference: &str,
        model: &str,
        dimensions: usize,
        suggested: &str,
    ) -> Result<Option<super::ModelRef>> {
        if !profiles
            .profile_ref(reference.trim())
            .is_some_and(|(family, alias)| profiles.find(family, alias).is_some())
        {
            return Ok(None);
        }
        let Some(source) = current.models.get(reference.trim()) else {
            return Ok(None);
        };
        if source.capability() != zeroclaw_api::capability::Capability::Chat {
            return Ok(None);
        }
        let provider = embedding_connection(candidate, source.provider())?;
        ensure!(
            !model.trim().is_empty() && dimensions > 0,
            "legacy embedding model and dimensions must be configured"
        );
        let mut name = super::ModelRef::from(suggested);
        let mut suffix = 2;
        while candidate.models.contains_key(&name) {
            name = format!("{suggested}.{suffix}").into();
            suffix += 1;
        }
        candidate.models.insert(
            name.clone(),
            super::ModelDefinition::Embedding(super::EmbeddingModelConfig {
                provider,
                model: model.trim().to_owned(),
                metadata: super::EmbeddingMetadata {
                    dimensions: Some(dimensions),
                },
            }),
        );
        Ok(Some(name))
    }

    if config
        .memory
        .embedding_model_ref
        .as_deref()
        .is_none_or(|r| r.trim().is_empty())
        && !config.memory.embedding_model.starts_with("hint:")
        && let Some(reference) = promote(
            &config.providers.models,
            current,
            candidate,
            &config.memory.embedding_provider,
            &config.memory.embedding_model,
            config.memory.embedding_dimensions,
            "embedding.memory",
        )?
    {
        config.memory.embedding_model_ref = Some(reference.to_string());
        config.mark_dirty("memory");
    }
    let mut routes_changed = false;
    for route in &mut config.embedding_routes {
        if let Some(reference) = promote(
            &config.providers.models,
            current,
            candidate,
            &route.model_provider,
            &route.model,
            route
                .dimensions
                .unwrap_or(config.memory.embedding_dimensions),
            &format!("embedding.route.{}", route.hint),
        )? {
            route.model_provider = reference.to_string();
            route.model.clear();
            route.dimensions = None;
            routes_changed = true;
        }
    }
    if routes_changed {
        config.mark_dirty("embedding_routes");
    }
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
