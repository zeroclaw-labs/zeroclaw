//! Canonical model inputs and explicit temporary CLI selections. Historical
//! configuration is normalized in migration before these consumers run.

use anyhow::{Context, Result, ensure};
use serde_json::json;
use zeroclaw_api::capability::Capability;

use super::{ModelDefinition, ModelRef, ProviderId, ProviderRegistry};
use crate::schema::Config;

impl Config {
    /// Rename only the complete configured model key.
    pub fn renamed_model_input(&self, current: &str, old: &str, new: &str) -> Option<String> {
        (current == old).then(|| new.to_owned())
    }

    /// Materialize an effort scope from exact model keys. The admitted
    /// registry is already canonical; no profile expansion happens here.
    pub fn restrict_chat_inputs(
        &mut self,
        allowed: &std::collections::HashSet<String>,
    ) -> anyhow::Result<()> {
        let registry = self.provider_registry()?;
        let allowed = |key: &ModelRef| allowed.contains(key.as_str());
        self.providers.models = Default::default();
        self.providers.connections = registry.connections;
        for (key, model) in registry.models {
            if model.capability() != Capability::Chat || allowed(&key) {
                self.providers.models.insert_canonical_model(
                    key,
                    serde_json::from_value(serde_json::to_value(model)?)?,
                );
            }
        }
        self.providers.fallback.retain(|source, targets| {
            targets.retain(|target| allowed(target));
            allowed(source) && !targets.is_empty()
        });
        self.model_routes
            .retain(|route| allowed(&route.model_ref.as_str().into()));
        Ok(())
    }

    /// Optional credential inherited by consumers that support an agent-key
    /// fallback. Explicit embedding/tool credentials retain their precedence.
    pub fn agent_model_api_key(&self, agent: &str) -> Option<String> {
        let reference = self.agent(agent)?.model_provider.as_str();
        let registry = self.provider_registry().ok()?;
        let selected = registry.resolve_chat(&reference.into()).ok()?;
        selected.selection.connection.api_key().map(str::to_owned)
    }

    /// Resolve Gemini OAuth client input at the config boundary. New auth
    /// profiles select an exact connection or model key. Historical input
    /// has already been normalized before configuration admission.
    pub fn gemini_oauth_client_credentials(
        &self,
        profile: &str,
    ) -> Result<Option<(String, String)>> {
        let registry = self.provider_registry()?;
        let connection =
            if let Some(connection) = registry.connections.get(&ProviderId::from(profile)) {
                connection
            } else if let Some(model) = registry.models.get(&ModelRef::from(profile)) {
                registry
                    .connections
                    .get(model.provider())
                    .context("OAuth model has no connection")?
            } else {
                return Ok(None);
            };
        let super::ProviderConnection::Gemini(connection) = connection else {
            anyhow::bail!("OAuth profile must select a Gemini connection");
        };
        Ok(match &connection.auth {
            Some(super::GeminiAuth::OAuth {
                client_id,
                client_secret,
            }) => Some((
                client_id.clone().unwrap_or_default(),
                client_secret.clone().unwrap_or_default(),
            )),
            _ => None,
        })
    }

    /// Normalize catalog CLI input; exact model keys always take precedence.
    pub fn chat_catalog_ref(&self, reference: &str) -> Result<ModelRef> {
        let reference = reference.trim();
        let registry = self.provider_registry()?;
        let exact = ModelRef::from(reference);
        if registry.models.contains_key(&exact) {
            registry.resolve_chat(&exact)?;
            return Ok(exact);
        }
        if !reference.contains(['.', ':']) {
            let default = ModelRef::from(format!("{reference}.default"));
            if registry.models.contains_key(&default) {
                registry.resolve_chat(&default)?;
                return Ok(default);
            }
        }
        self.chat_registry_for_input(None, reference)?;
        Ok(exact)
    }

    /// Resolve a model or adapt a bare/URL CLI input for this construction only.
    /// The returned registry is an ephemeral view, never a second live source.
    pub fn chat_registry_for_input(
        &self,
        agent: Option<&str>,
        reference: &str,
    ) -> Result<ProviderRegistry> {
        let reference = reference.trim();
        let mut registry = self.provider_registry()?;
        let key = ModelRef::from(reference);
        if registry.models.contains_key(&key) {
            registry.resolve_chat(&key)?;
            return Ok(registry);
        }
        let (family, uri) = reference
            .split_once(':')
            .map_or((reference, None), |(family, uri)| (family, Some(uri)));
        let family = match family {
            "anthropic-custom" => "anthropic",
            "openai-compatible" | "openai_compatible" => "custom",
            other => other,
        };
        // Only the config migration recognizes historical vendor spellings.
        // Every other dot belongs to a registered key, never profile ancestry.
        ensure!(
            !family.contains('.'),
            "unknown model reference `{reference}`"
        );
        let selected_ref = agent
            .and_then(|agent| self.agent(agent))
            .map(|agent| ModelRef::from(agent.model_provider.as_str()));
        let selected = selected_ref
            .as_ref()
            .and_then(|key| registry.models.get(key))
            .cloned();
        let selected_options = selected_ref
            .as_ref()
            .and_then(|key| registry.resolve_chat(key).ok())
            .map(|selected| selected.options);
        if let Some(model) = selected.as_ref()
            && registry
                .connections
                .get(model.provider())
                .is_some_and(|connection| connection.kind() == family)
        {
            let mut model = model.clone();
            if let Some(uri) = uri {
                ensure!(
                    uri.starts_with("http://") || uri.starts_with("https://"),
                    "invalid provider URL"
                );
                let mut connection = serde_json::to_value(&registry.connections[model.provider()])?;
                connection["uri"] = uri.into();
                let mut provider = ProviderId::from(format!("temporary.{reference}"));
                while registry.connections.contains_key(&provider) {
                    provider = ProviderId::from(format!("temporary.{}", provider.as_str()));
                }
                let mut raw_model = serde_json::to_value(&model)?;
                raw_model["provider"] = provider.as_str().into();
                registry
                    .connections
                    .insert(provider, serde_json::from_value(connection)?);
                model = serde_json::from_value(raw_model)?;
            }
            if let Some(targets) = selected_ref
                .as_ref()
                .and_then(|reference| registry.fallback.get(reference))
                .cloned()
            {
                registry.fallback.insert(key.clone(), targets);
            }
            registry.models.insert(key, model);
            registry.validate()?;
            return Ok(registry);
        }
        let mut connection = json!({"kind":family});
        if let Some(uri) = uri {
            ensure!(
                uri.starts_with("http://") || uri.starts_with("https://"),
                "invalid provider URL"
            );
            connection["uri"] = uri.into();
        }
        let mut model = json!({"capability":"chat", "provider":""});
        // Preserve model policy when a CLI transport override inherits its model
        // from an agent, while constructing the requested connection afresh.
        if let Some(ModelDefinition::Chat(selected)) = selected {
            let selected = serde_json::to_value(selected)?;
            for field in ["model", "metadata"] {
                if let Some(value) = selected.get(field) {
                    model[field] = value.clone();
                }
            }
        }
        if let Some(options) = selected_options {
            model["options"] = serde_json::to_value(options)?;
        }
        let mut provider = ProviderId::from(format!("temporary.{reference}"));
        while registry.connections.contains_key(&provider) {
            provider = ProviderId::from(format!("temporary.{}", provider.as_str()));
        }
        model["provider"] = provider.as_str().into();
        registry
            .connections
            .insert(provider, serde_json::from_value(connection)?);
        if let Some(targets) = selected_ref
            .as_ref()
            .and_then(|reference| registry.fallback.get(reference))
            .cloned()
        {
            registry.fallback.insert(key.clone(), targets);
        }
        registry.models.insert(key, serde_json::from_value(model)?);
        registry.validate()?;
        Ok(registry)
    }

    /// Validate an exact capability-model key; no legacy name rewriting.
    pub fn capability_model_ref(
        &self,
        reference: &str,
        capability: Capability,
    ) -> Result<ModelRef> {
        let registry = self.provider_registry()?;
        let key = ModelRef::from(reference.trim());
        registry.selection(&key, capability)?;
        Ok(key)
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use serde_json::json;

    fn fixture() -> Config {
        let mut config = Config {
            providers: serde_json::from_value(json!({
                "connections": {
                    "gateway.Prod": {"kind":"custom", "uri":"https://original.invalid/v1", "auth":{"mode":"api_key", "api_key":"fixture-key"}, "extra_headers":{"X-Route":"fixture"}},
                    "speech.local": {"kind":"custom", "uri":"https://speech.invalid/v1"}
                },
                "models": {
                    "chat.Fast.v2": {"capability":"chat", "provider":"gateway.Prod", "model":"wire-fast", "options":{"temperature":0.3}, "metadata":{"context_window":8192}},
                    "chat.Backup": {"capability":"chat", "provider":"gateway.Prod", "model":"wire-backup", "options":{"temperature":0.8}},
                    "audio.ref": {"capability":"asr", "provider":"speech.local", "model":"whisper-1"}
                },
                "fallback":{"chat.Fast.v2":["chat.Backup"]}
            })).unwrap(),
            ..Config::default()
        };
        config.agents.insert(
            "agent".into(),
            crate::schema::AliasedAgentConfig {
                model_provider: "chat.Fast.v2".into(),
                ..Default::default()
            },
        );
        config
    }

    #[test]
    fn historical_vendor_spellings_use_the_config_migration() {
        let config = Config::default();
        for (input, kind) in [
            ("google", "gemini"),
            ("aws-bedrock", "bedrock"),
            ("llama.cpp", "llamacpp"),
        ] {
            let registry = config.chat_registry_for_input(None, input).unwrap();
            assert_eq!(
                registry
                    .resolve_chat(&input.into())
                    .unwrap()
                    .selection
                    .connection
                    .kind(),
                kind
            );
        }
    }

    #[test]
    fn exact_model_names_never_expand_or_rename_by_prefix() {
        let config = fixture();
        assert!(config.chat_registry_for_input(None, "chat.Fast.v2").is_ok());
        assert!(config.chat_registry_for_input(None, "chat.Fast").is_err());
        assert_eq!(
            config.renamed_model_input("chat.Fast.v2", "chat.Fast", "other"),
            None
        );
        assert!(
            config
                .capability_model_ref("chat.Fast.v2", Capability::Asr)
                .is_err()
        );
        assert!(config.chat_registry_for_input(None, "audio.ref").is_err());
    }

    #[test]
    fn agent_key_inheritance_uses_the_exact_selected_connection() {
        let config = fixture();
        assert_eq!(
            config.agent_model_api_key("agent").as_deref(),
            Some("fixture-key")
        );
        assert_eq!(config.agent_model_api_key("absent"), None);
    }

    #[test]
    fn legacy_only_models_are_available_to_canonical_pickers() {
        let mut config = Config {
            providers: serde_json::from_value(json!({
                "models": {"openai": {"primary.prod": {"model": "legacy-chat"}}}
            }))
            .unwrap(),
            ..Config::default()
        };
        crate::migration::normalize_test_input(&mut config);
        let source = crate::traits::AliasSource::ModelProviders;
        assert_eq!(
            config.resolve_alias_source(source),
            vec!["openai.primary.prod"]
        );
        config
            .providers
            .connections
            .extend(fixture().providers.connections);
        config.providers.models.insert_canonical_model(
            "chat.independent".into(),
            serde_json::from_value(json!({
                "capability":"chat", "provider":"gateway.Prod", "model":"other"
            }))
            .unwrap(),
        );
        assert_eq!(
            config.resolve_alias_source(source),
            vec!["chat.independent", "openai.primary.prod"]
        );
    }

    #[test]
    fn explicit_canonical_capability_mismatch_never_uses_a_prefixed_alternative() {
        let mut config = fixture();
        config.providers.models.insert_canonical_model(
            "asr.chat.Fast.v2".into(),
            serde_json::from_value(json!({
                "capability":"asr", "provider":"speech.local", "model":"whisper-1"
            }))
            .unwrap(),
        );
        let error = config
            .capability_model_ref("chat.Fast.v2", Capability::Asr)
            .unwrap_err();
        assert!(error.to_string().contains("does not support"));
    }

    #[test]
    fn explicit_url_rebuilds_connection_and_preserves_complete_selected_policy() {
        let config = fixture();
        let reference = "custom:https://override.invalid/v1";
        let registry = config
            .chat_registry_for_input(Some("agent"), reference)
            .unwrap();
        let model = registry.resolve_chat(&reference.into()).unwrap();
        assert_eq!(
            model.selection.connection.uri(),
            Some("https://override.invalid/v1")
        );
        assert_eq!(model.selection.connection.api_key(), Some("fixture-key"));
        assert_eq!(
            model.selection.connection.extra_headers().unwrap()["X-Route"],
            "fixture"
        );
        assert_eq!(model.upstream_model(), Some("wire-fast"));
        assert!(
            matches!(model.options.temperature, Some(registry::registry::Override::Set(value)) if value == 0.3)
        );
        assert_eq!(
            registry.fallback_chain(&reference.into()).unwrap(),
            vec!["chat.Backup".into()]
        );
        assert_eq!(
            registry
                .resolve_chat(&"chat.Backup".into())
                .unwrap()
                .upstream_model(),
            Some("wire-backup")
        );
        assert_eq!(
            config.provider_registry().unwrap().connections["gateway.Prod"].uri(),
            Some("https://original.invalid/v1")
        );
    }

    #[test]
    fn effort_scope_retains_only_explicit_chat_keys_and_their_admitted_edges() {
        let mut config = fixture();
        config
            .restrict_chat_inputs(&["chat.Fast.v2".to_owned()].into_iter().collect())
            .unwrap();
        let registry = config.provider_registry().unwrap();
        assert!(registry.models.contains_key("chat.Fast.v2"));
        assert!(!registry.models.contains_key("chat.Backup"));
        assert!(registry.models.contains_key("audio.ref"));
        assert!(registry.fallback.is_empty());
    }
}
