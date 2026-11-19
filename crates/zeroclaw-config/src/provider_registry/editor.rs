//! UI editing boundary for the canonical registry. Exact map keys remain JSON
//! keys throughout; no flattened property path is used to address an entity.

use anyhow::{Context, Result, ensure};
use hmac::{Hmac, Mac};
use serde::{Deserialize, Serialize};
use serde_json::Value;
use sha2::Sha256;
use std::sync::LazyLock;

#[path = "editor_mutations.rs"]
mod mutations;
pub use mutations::{create_entry, delete_entry, rename_entry};

use crate::{schema::Config, traits::MASKED_SECRET};

// This process-local signing key creates an opaque revision from canonical
// state, including credentials. No config revision or registry copy is stored.
// Restarting the process deliberately requires editors to reload their baseline.
static REVISION_KEY: LazyLock<[u8; 32]> = LazyLock::new(rand::random);

#[derive(Serialize)]
#[cfg_attr(feature = "schema-export", derive(schemars::JsonSchema))]
pub struct RegistrySnapshot {
    #[serde(flatten)]
    pub registry: super::ProviderRegistry,
    /// Opaque concurrency token. Changes to masked credentials change it too.
    pub revision: String,
}

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
    let mut mac = Hmac::<Sha256>::new_from_slice(REVISION_KEY.as_slice())?;
    mac.update(&serde_json::to_vec(&registry)?);
    let revision = hex::encode(mac.finalize().into_bytes());
    for connection in registry.connections.values_mut() {
        connection.mask_secrets()?;
    }
    let mut document = serde_json::to_value(RegistrySnapshot { registry, revision })?;
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

/// Stage an external editor's replacement only while its persisted baseline
/// still matches the daemon. The caller holds the normal config commit lock.
/// Restore environment and external-secret references for this comparison;
/// resolving an `op://` reference again would compare the secret's live value
/// rather than the operator's saved configuration.
pub async fn stage_persisted(config: &mut Config, edit: RegistryEdit) -> Result<()> {
    let raw = match tokio::fs::read_to_string(&config.config_path).await {
        Ok(raw) => Some(raw),
        Err(error) if error.kind() == std::io::ErrorKind::NotFound => None,
        Err(error) => return Err(error).context("cannot inspect persisted provider registry"),
    };
    if let Some(raw) = raw {
        // The daemon compares the admitted, current-schema view.  Re-run the
        // same versioned migration chain for the disk copy so a V3/V4 file is
        // compared after its legacy provider/profile fields have been
        // projected into the canonical registry.
        let disk = crate::migration::migrate_to_current(&raw)
            .context("config changed on disk; reload before saving the provider registry")?;
        let mut saved = config.clone();
        crate::env_overrides::mask_env_overrides_for_save(
            &mut saved,
            &config.pre_override_snapshots,
        )?;
        crate::schema::restore_onepassword_references_for_save(
            &mut saved,
            &config.onepassword_reference_snapshots,
            &Default::default(),
        )?;
        let mut disk_registry = disk.provider_registry()?;
        let root = config
            .config_path
            .parent()
            .context("config path has no parent")?;
        let store = crate::secrets::SecretStore::new(root, disk.secrets.encrypt);
        for connection in disk_registry.connections.values_mut() {
            connection.with_secret_json_for_editor(|secret| {
                if !crate::secrets::SecretStore::is_onepassword_ref(secret) {
                    *secret = store.decrypt(secret)?;
                }
                Ok(())
            })?;
        }
        ensure!(
            serde_json::to_value(disk_registry)?
                == serde_json::to_value(saved.provider_registry()?)?,
            "provider registry changed on disk; reload before saving"
        );
    }
    // Stage owns external secret resolution, which may invoke the 1Password
    // CLI. Keep subprocess waits off the async request worker and publish
    // the caller's candidate only after the complete stage succeeds.
    let mut candidate = config.clone();
    let candidate = tokio::task::spawn_blocking(move || {
        stage(&mut candidate, edit)?;
        Ok::<_, anyhow::Error>(candidate)
    })
    .await
    .context("provider registry staging worker failed")??;
    *config = candidate;
    Ok(())
}

/// Stage a complete, validated replacement on a caller-owned config candidate.
/// No disk or live config changes occur here. Unchanged masked secrets are
/// restored from the latest admitted config using the normal secret lifecycle.
pub fn stage(config: &mut Config, edit: RegistryEdit) -> Result<()> {
    ensure!(
        snapshot(config)? == edit.expected,
        "provider registry changed; reload before saving"
    );
    let mut document = edit.registry;
    // A caller may edit the snapshot directly. Its token belongs to the CAS
    // envelope, never to persisted provider configuration.
    if let Some(document) = document.as_object_mut() {
        document.remove("revision");
    }
    let mut registry: super::ProviderRegistry =
        serde_json::from_value(document).context("invalid provider registry")?;
    for (id, connection) in &mut registry.connections {
        if let Some(current) = config.providers.connections.get(id) {
            connection.restore_secrets_from(current);
        }
        connection.validate_editor_secrets()?;
    }
    // Environment overrides are admission-time ownership. A registry editor
    // must not replace any environment-owned registry field: the later save
    // mask would silently discard the edit. Compare the complete canonical
    // registry so connections, models, presets and fallback entries follow
    // the same rule.
    let current = serde_json::to_value(config.provider_registry()?)?;
    let incoming = serde_json::to_value(&registry)?;
    for path in config.pre_override_snapshots.keys() {
        let Some(field_path) = path.strip_prefix("/providers") else {
            continue;
        };
        ensure!(
            current.pointer(field_path) == incoming.pointer(field_path),
            "provider registry field `{path}` is environment-owned; update the environment before editing it"
        );
    }
    let mut candidate = config.clone();
    candidate.providers.connections = registry.connections;
    candidate.providers.models.canonical_models =
        serde_json::from_value(serde_json::to_value(registry.models)?)?;
    candidate.providers.model_presets = registry.model_presets;
    candidate.providers.fallback = registry.fallback;
    // Resolve newly entered external references for the live provider graph,
    // while retaining the opaque `op://` spelling for the next save.  The
    // normal config loader already follows this lifecycle; registry edits
    // must use the same source of truth instead of publishing a literal
    // reference as a bearer token.
    let new_references = crate::schema::collect_onepassword_reference_snapshots(&candidate)?;
    let secret_store = candidate
        .config_path
        .parent()
        .map(|root| crate::secrets::SecretStore::new(root, candidate.secrets.encrypt));
    for connection in candidate.providers.connections.values_mut() {
        connection.with_secret_json_for_editor(|secret| {
            if crate::secrets::SecretStore::is_onepassword_ref(secret) {
                *secret = secret_store
                    .as_ref()
                    .context("config path has no parent")?
                    .decrypt(secret)?;
            }
            Ok(())
        })?;
    }
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
    candidate
        .onepassword_reference_snapshots
        .extend(new_references);
    candidate.validate()?;
    let registry = candidate.provider_registry()?;
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
        registry
            .resolve_embedding(&super::ModelRef::from(reference))
            .context("invalid memory.embedding_model_ref")?;
    }
    if let Some(reference) = candidate
        .multimodal
        .vision_model_provider
        .as_deref()
        .map(str::trim)
        .filter(|reference| !reference.is_empty())
    {
        registry
            .resolve_chat(&super::ModelRef::from(reference))
            .context("invalid multimodal.vision_model_provider")?;
    }
    candidate.mark_dirty("providers");
    *config = candidate;
    Ok(())
}

impl super::ProviderConnection {
    fn validate_editor_secrets(&mut self) -> Result<()> {
        self.with_secret_json_for_editor(|secret| {
            ensure!(
                secret.as_str() != MASKED_SECRET && secret.as_str() != "****",
                "masked secret has no existing credential to preserve"
            );
            Ok(())
        })
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn unknown_option_diagnostics_keep_quoted_keys_and_loading_is_permissive() {
        let input = r#"
schema_version = 5
[providers.connections."gateway.prod"]
kind = "openai"
[providers.connections."gateway.prod".api.chat.options]
extract_text_tool_call = true
[providers.models."chat.fast"]
capability = "chat"
provider = "gateway.prod"
model = "test"
[providers.models."chat.fast".options]
extract_text_tool_call = true
"#;
        let _: Config = toml::from_str(input).unwrap();
        assert_eq!(
            Config::unknown_canonical_option_fields(input),
            vec![
                "providers.connections.\"gateway.prod\".api.chat.options.extract_text_tool_call",
                "providers.models.\"chat.fast\".options.extract_text_tool_call",
            ]
        );
    }

    #[tokio::test]
    async fn persisted_edit_checks_unmasked_disk_registry_without_resolving_external_secrets() {
        let tmp = tempfile::tempdir().unwrap();
        let mut config = Config {
            config_path: tmp.path().join("config.toml"),
            ..Config::default()
        };
        config.providers = serde_json::from_value(serde_json::json!({
            "connections": {"gateway": {"kind":"openai", "auth":{"mode":"api_key", "api_key":"op://test/item/key"}}},
            "models": {"chat": {"capability":"chat", "provider":"gateway", "model":"test"}}
        })).unwrap();
        tokio::fs::write(&config.config_path, toml::to_string(&config).unwrap())
            .await
            .unwrap();
        // Disk retains the reference; the daemon has already resolved it.
        config.onepassword_reference_snapshots =
            crate::schema::collect_onepassword_reference_snapshots(&config).unwrap();
        config
            .providers
            .connections
            .get_mut("gateway")
            .unwrap()
            .with_secret_json_for_editor(|secret| {
                *secret = "synthetic-resolved-key".into();
                Ok(())
            })
            .unwrap();
        let expected = snapshot(&config).unwrap();
        let edit = RegistryEdit {
            expected: expected.clone(),
            registry: expected,
        };
        stage_persisted(&mut config, edit.clone()).await.unwrap();

        let mut disk = config.clone();
        update(&mut disk, |registry| {
            let super::super::ModelDefinition::Chat(model) =
                registry.models.get_mut("chat").unwrap()
            else {
                unreachable!()
            };
            model.model = Some("edited-on-disk".into());
            Ok(())
        })
        .unwrap();
        tokio::fs::write(&config.config_path, toml::to_string(&disk).unwrap())
            .await
            .unwrap();
        let before = snapshot(&config).unwrap();
        let error = stage_persisted(&mut config, edit).await.unwrap_err();
        assert!(error.to_string().contains("changed on disk"), "{error:#}");
        assert_eq!(snapshot(&config).unwrap(), before);
    }
}
