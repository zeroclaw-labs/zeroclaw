use serde::{Deserialize, Serialize};
use std::collections::HashMap;
use zeroclaw_macros::Configurable;

use super::schema::{EmbeddingRouteConfig, ModelProviderConfig, ModelRouteConfig};

/// Top-level `[providers]` section. Wraps model provider profiles, routing rules,
/// and an optional fallback reference.
#[derive(Debug, Clone, Serialize, Deserialize, Configurable, Default)]
#[cfg_attr(feature = "schema-export", derive(schemars::JsonSchema))]
#[prefix = "providers"]
pub struct ProvidersConfig {
    /// Ordered fallback chain of dotted provider aliases.
    /// First entry is primary; rest are tried in order on failure.
    #[serde(default, skip_serializing_if = "Vec::is_empty")]
    pub fallback: Vec<String>,

    /// Named model provider profiles: outer key = provider type, inner key = user alias.
    /// V3 shape: `[providers.models.<type>.<alias>]` e.g. `[providers.models.anthropic.default]`.
    #[serde(default, skip_serializing_if = "HashMap::is_empty")]
    pub models: HashMap<String, HashMap<String, ModelProviderConfig>>,

    /// Model routing rules — route `hint:<name>` to specific provider+model combos.
    #[serde(default, skip_serializing_if = "Vec::is_empty")]
    pub model_routes: Vec<ModelRouteConfig>,

    /// Embedding routing rules — route `hint:<name>` to specific provider+model combos.
    #[serde(default, skip_serializing_if = "Vec::is_empty")]
    pub embedding_routes: Vec<EmbeddingRouteConfig>,
}

impl ProvidersConfig {
    /// The provider type portion of the primary fallback — the part before the first `.`.
    /// `"anthropic.default"` → `"anthropic"`, `"anthropic"` → `"anthropic"`.
    pub fn fallback_type(&self) -> Option<&str> {
        let name = self.fallback.first()?;
        Some(name.split_once('.').map_or(name.as_str(), |(t, _)| t))
    }

    fn resolve_provider(&self, name: &str) -> Option<&ModelProviderConfig> {
        if let Some((type_key, alias_key)) = name.split_once('.') {
            self.models.get(type_key)?.get(alias_key)
        } else {
            self.models.get(name)?.get("default")
        }
    }

    fn resolve_provider_mut(&mut self, name: &str) -> Option<&mut ModelProviderConfig> {
        if let Some((type_key, alias_key)) = name.split_once('.') {
            let alias_owned = alias_key.to_string();
            self.models.get_mut(type_key)?.get_mut(&alias_owned)
        } else {
            self.models.get_mut(name)?.get_mut("default")
        }
    }

    pub fn fallback_provider(&self) -> Option<&ModelProviderConfig> {
        self.resolve_provider(self.fallback.first()?)
    }

    pub fn fallback_provider_mut(&mut self) -> Option<&mut ModelProviderConfig> {
        let name = self.fallback.first()?.clone();
        self.resolve_provider_mut(&name)
    }

    /// Return the first concrete `model` string available for use as a default.
    ///
    /// Resolution order:
    ///
    /// 1. The fallback provider's `model` field, if set.
    /// 2. The first entry in `models` (iteration order) that has `model` set.
    ///
    /// Returns `None` only when no provider entry has any model configured at all.
    pub fn resolve_default_model(&self) -> Option<String> {
        if let Some(model) = self
            .fallback_provider()
            .and_then(|e| e.model.as_deref())
            .map(str::trim)
            .filter(|m| !m.is_empty())
        {
            return Some(model.to_string());
        }

        self.models
            .values()
            .flat_map(|alias_map| alias_map.values())
            .filter_map(|entry| entry.model.as_deref().map(str::trim))
            .find(|m| !m.is_empty())
            .map(ToString::to_string)
    }
}
