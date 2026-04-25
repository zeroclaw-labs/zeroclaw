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
        self.fallback.as_deref().and_then(|name| {
            // First try exact key match
            if let Some(entry) = self.models.get(name) {
                return Some(entry);
            }
            // For custom: URLs, search by base_url
            if let Some(url) = name.strip_prefix("custom:") {
                let normalized_url = url.trim_end_matches('/');
                return self.models.values().find(|entry| {
                    let entry_url = entry.base_url.as_deref().map(|u| u.trim_end_matches('/'));
                    entry_url == Some(normalized_url)
                });
            }
            None
        })
    }
    pub fn fallback_provider_mut(&mut self) -> Option<&mut ModelProviderConfig> {
        let name = self.fallback.clone()?;
        // For custom: URLs, find the matching key first
        if let Some(url) = name.strip_prefix("custom:") {
            let normalized_url = url.trim_end_matches('/');
            let matching_key = self
                .models
                .iter()
                .find(|(_, entry)| {
                    entry.base_url.as_deref().map(|u| u.trim_end_matches('/'))
                        == Some(normalized_url)
                })
                .map(|(k, _)| k.clone());
            if let Some(key) = matching_key {
                return self.models.get_mut(&key);
            }
        }
        // Try exact key match
        self.models.get_mut(&name)
    }
}
