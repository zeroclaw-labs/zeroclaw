use serde::{Deserialize, Serialize};
use std::collections::HashMap;
use zeroclaw_macros::Configurable;

use super::schema::{
    EmbeddingRouteConfig, ModelProviderConfig, ModelRouteConfig, TtsProviderConfig,
};

/// Top-level `[providers]` section. Wraps model provider profiles and routing rules.
#[derive(Debug, Clone, Serialize, Deserialize, Configurable, Default)]
#[cfg_attr(feature = "schema-export", derive(schemars::JsonSchema))]
#[prefix = "providers"]
pub struct ProvidersConfig {
    /// Named model provider profiles: outer key = provider type, inner key = user alias.
    /// V3 shape: `[providers.models.<type>.<alias>]` e.g. `[providers.models.anthropic.default]`.
    #[serde(default, skip_serializing_if = "HashMap::is_empty")]
    #[nested]
    pub models: HashMap<String, HashMap<String, ModelProviderConfig>>,

    /// Named TTS provider profiles: outer key = provider type, inner key = user alias.
    /// V3 shape: `[providers.tts.<type>.<alias>]` e.g. `[providers.tts.openai.default]`.
    /// Mirrors `models` for parallel runtime dispatch through agent `tts_provider`
    /// alias references.
    #[serde(default, skip_serializing_if = "HashMap::is_empty")]
    #[nested]
    pub tts: HashMap<String, HashMap<String, TtsProviderConfig>>,

    /// Model routing rules — route `hint:<name>` to specific provider+model combos.
    #[serde(default, skip_serializing_if = "Vec::is_empty")]
    pub model_routes: Vec<ModelRouteConfig>,

    /// Embedding routing rules — route `hint:<name>` to specific provider+model combos.
    #[serde(default, skip_serializing_if = "Vec::is_empty")]
    pub embedding_routes: Vec<EmbeddingRouteConfig>,
}

impl ProvidersConfig {
    /// Return the first concrete `model` string available for use as a default.
    ///
    /// Scans all entries in `models` (iteration order) for one that has `model` set.
    ///
    /// Returns `None` only when no provider entry has any model configured at all.
    pub fn resolve_default_model(&self) -> Option<String> {
        self.models
            .values()
            .flat_map(|alias_map| alias_map.values())
            .filter_map(|entry| entry.model.as_deref().map(str::trim))
            .find(|m| !m.is_empty())
            .map(ToString::to_string)
    }

    /// Return the first `ModelProviderConfig` from `models`, if any exists.
    pub fn first_provider(&self) -> Option<&ModelProviderConfig> {
        self.models
            .values()
            .flat_map(|alias_map| alias_map.values())
            .next()
    }

    /// Return a mutable reference to the first `ModelProviderConfig` from `models`, if any exists.
    pub fn first_provider_mut(&mut self) -> Option<&mut ModelProviderConfig> {
        self.models
            .values_mut()
            .flat_map(|alias_map| alias_map.values_mut())
            .next()
    }

    /// Return the provider type key of the first entry in `models`, if any.
    /// Use this when callers need the bare type name (e.g. provider
    /// routing factories that take `"openrouter"` not `"openrouter.default"`).
    pub fn first_provider_type(&self) -> Option<&str> {
        self.models.keys().next().map(String::as_str)
    }

    /// Return the V3 dotted `<type>.<alias>` identifier of the first
    /// configured model provider entry, if any. Use this when callers
    /// need the V3 alias reference (matches `agents.<x>.model_provider`
    /// values).
    pub fn first_provider_alias(&self) -> Option<String> {
        self.models
            .iter()
            .find_map(|(ty, alias_map)| alias_map.keys().next().map(|a| format!("{ty}.{a}")))
    }
}
