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
    /// Key of the provider entry to use when no route matches.
    /// Optional — if unset, requests without a matching route fail at runtime.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub fallback: Option<String>,

    /// Named model provider profiles keyed by id.
    #[serde(default, skip_serializing_if = "HashMap::is_empty")]
    #[nested]
    pub models: HashMap<String, ModelProviderConfig>,

    /// Model routing rules — route `hint:<name>` to specific provider+model combos.
    #[serde(default, skip_serializing_if = "Vec::is_empty")]
    pub model_routes: Vec<ModelRouteConfig>,

    /// Embedding routing rules — route `hint:<name>` to specific provider+model combos.
    #[serde(default, skip_serializing_if = "Vec::is_empty")]
    pub embedding_routes: Vec<EmbeddingRouteConfig>,
}

impl ProvidersConfig {
    pub fn fallback_provider(&self) -> Option<&ModelProviderConfig> {
        self.fallback
            .as_deref()
            .and_then(|name| self.models.get(name))
    }
    pub fn fallback_provider_mut(&mut self) -> Option<&mut ModelProviderConfig> {
        let name = self.fallback.clone()?;
        self.models.get_mut(&name)
    }

    /// Return the first concrete `model` string available for use as a default.
    ///
    /// Resolution order:
    ///
    /// 1. The fallback provider's `model` field, if set.
    /// 2. The first entry in `models` (iteration order) that has `model` set.
    ///
    /// Returns `None` only when no provider entry has any model configured at all.
    /// Callers should treat `None` as a configuration error and surface it rather
    /// than silently substituting a hardcoded model identifier.
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
            .filter_map(|entry| entry.model.as_deref().map(str::trim))
            .find(|m| !m.is_empty())
            .map(ToString::to_string)
    }
}
