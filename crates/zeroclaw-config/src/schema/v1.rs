//! V1 schema partial typed lens for V1 → V2 migration.
//!
//! Frozen after V2 shipped (PR #5517 / `4259f27cb`). Explicit fields only for
//! top-level keys that change between V1 and V2; everything else rides through
//! `passthrough`.
//!
//! V1 → V2 transformation inventory (ground truth: `git show 1ec9c14ca:crates/zeroclaw-config/src/schema.rs`):
//!
//! Twelve former top-level fields fold into the new V2 `[providers]` section:
//!
//! | V1 path | V2 destination |
//! |---|---|
//! | `api_key` | `providers.api_key` |
//! | `api_url` | `providers.api_url` |
//! | `api_path` | `providers.api_path` |
//! | `default_provider` (alias `model_provider`) | `providers.default_provider` |
//! | `default_model` (alias `model`) | `providers.default_model` |
//! | `model_providers` | `providers.models` |
//! | `default_temperature` | `providers.default_temperature` |
//! | `provider_timeout_secs` | `providers.provider_timeout_secs` |
//! | `provider_max_tokens` | `providers.provider_max_tokens` |
//! | `extra_headers` | `providers.extra_headers` |
//! | `model_routes` | `providers.model_routes` |
//! | `embedding_routes` | `providers.embedding_routes` |
//!
//! Plus rename: `channels_config` → `channels`. And: `schema_version = 2`
//! is set on output.
//!
//! V2 ProvidersConfig (`git show 68a875b5b:crates/zeroclaw-config/src/providers.rs`)
//! at the time of the V1→V2 cut had `{fallback, models, model_routes, embedding_routes}`.
//! V2 then accumulated additional fields on the top-level Config or providers
//! over its lifetime; we only handle the V1→V2 step here. Any fields that V2
//! *expected* but V1 didn't have either default at deserialize time or remain
//! unset (passthrough handles user-added unknowns).

use serde::{Deserialize, Serialize};
use std::collections::HashMap;

use crate::schema::v2::V2Config;

/// V1 partial typed lens. Anything not explicitly named flows through
/// `passthrough` unchanged.
#[derive(Debug, Default, Deserialize, Serialize)]
pub struct V1Config {
    // ── 12 fields folded into V2 [providers] ──────────────────────────
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub api_key: Option<toml::Value>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub api_url: Option<toml::Value>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub api_path: Option<toml::Value>,
    #[serde(
        default,
        skip_serializing_if = "Option::is_none",
        alias = "model_provider"
    )]
    pub default_provider: Option<toml::Value>,
    #[serde(default, skip_serializing_if = "Option::is_none", alias = "model")]
    pub default_model: Option<toml::Value>,
    #[serde(default, skip_serializing_if = "HashMap::is_empty")]
    pub model_providers: HashMap<String, toml::Value>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub default_temperature: Option<toml::Value>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub provider_timeout_secs: Option<toml::Value>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub provider_max_tokens: Option<toml::Value>,
    #[serde(default, skip_serializing_if = "HashMap::is_empty")]
    pub extra_headers: HashMap<String, toml::Value>,
    #[serde(default, skip_serializing_if = "Vec::is_empty")]
    pub model_routes: Vec<toml::Value>,
    #[serde(default, skip_serializing_if = "Vec::is_empty")]
    pub embedding_routes: Vec<toml::Value>,

    // ── renamed channels_config → channels ────────────────────────────
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub channels_config: Option<toml::Value>,

    /// Everything else passes through unchanged.
    #[serde(flatten)]
    pub passthrough: toml::Table,
}

impl V1Config {
    /// Migrate V1 → V2.
    ///
    /// Compile-time guarantee: this function returns `V2Config`. The V3
    /// schema is structurally unable to leak into the V1→V2 step.
    pub fn migrate(self) -> V2Config {
        let V1Config {
            api_key,
            api_url,
            api_path,
            default_provider,
            default_model,
            model_providers,
            default_temperature,
            provider_timeout_secs,
            provider_max_tokens,
            extra_headers,
            model_routes,
            embedding_routes,
            channels_config,
            mut passthrough,
        } = self;

        // Build V2 [providers] from the 12 former top-level fields.
        let mut providers = toml::Table::new();
        if let Some(v) = api_key {
            providers.insert("api_key".to_string(), v);
        }
        if let Some(v) = api_url {
            providers.insert("api_url".to_string(), v);
        }
        if let Some(v) = api_path {
            providers.insert("api_path".to_string(), v);
        }
        if let Some(v) = default_provider {
            providers.insert("default_provider".to_string(), v);
        }
        if let Some(v) = default_model {
            providers.insert("default_model".to_string(), v);
        }
        if !model_providers.is_empty() {
            let table: toml::Table = model_providers.into_iter().collect();
            providers.insert("models".to_string(), toml::Value::Table(table));
        }
        if let Some(v) = default_temperature {
            providers.insert("default_temperature".to_string(), v);
        }
        if let Some(v) = provider_timeout_secs {
            providers.insert("provider_timeout_secs".to_string(), v);
        }
        if let Some(v) = provider_max_tokens {
            providers.insert("provider_max_tokens".to_string(), v);
        }
        if !extra_headers.is_empty() {
            let table: toml::Table = extra_headers.into_iter().collect();
            providers.insert("extra_headers".to_string(), toml::Value::Table(table));
        }
        if !model_routes.is_empty() {
            providers.insert("model_routes".to_string(), toml::Value::Array(model_routes));
        }
        if !embedding_routes.is_empty() {
            providers.insert(
                "embedding_routes".to_string(),
                toml::Value::Array(embedding_routes),
            );
        }

        let providers_value = if providers.is_empty() {
            None
        } else {
            tracing::info!(
                target: "migration",
                "[api_key, api_url, default_provider, default_model, model_providers, …] folded into [providers]"
            );
            Some(toml::Value::Table(providers))
        };

        // Rename channels_config → channels (passthrough into V2Config.passthrough,
        // since V2Config doesn't model channels explicitly until V2→V3 step).
        if let Some(channels_value) = channels_config {
            passthrough.insert("channels".to_string(), channels_value);
            tracing::info!(target: "migration", "channels_config → channels");
        }

        // Set V2 schema_version = 2.
        let mut v2 = V2Config {
            schema_version: 2,
            providers: providers_value,
            passthrough,
            ..V2Config::default()
        };

        // Pull V2-relevant top-level keys (autonomy, agent, swarms, cron, cost,
        // channels, agents) out of passthrough into their typed slots so that
        // V2Config::migrate sees them in the lens. V1 input never sets these
        // (they're V2-or-later additions or pass-through), but if a user did
        // include them inline we honor them.
        if let Some(v) = v2.passthrough.remove("autonomy") {
            v2.autonomy = Some(v);
        }
        if let Some(v) = v2.passthrough.remove("agent") {
            v2.agent = Some(v);
        }
        if let Some(toml::Value::Table(t)) = v2.passthrough.remove("swarms") {
            v2.swarms = t.into_iter().collect();
        }
        if let Some(v) = v2.passthrough.remove("cron") {
            v2.cron = Some(v);
        }
        if let Some(v) = v2.passthrough.remove("cost") {
            v2.cost = Some(v);
        }
        if let Some(v) = v2.passthrough.remove("channels") {
            v2.channels = Some(v);
        }
        if let Some(toml::Value::Table(t)) = v2.passthrough.remove("agents") {
            v2.agents = t.into_iter().collect();
        }
        // If V1 user happened to specify their own `providers` block (unlikely
        // — that section was V2-introduced), prefer the synthesized one we
        // built; merge any user-provided keys not already set.
        if let Some(toml::Value::Table(user_providers)) = v2.passthrough.remove("providers") {
            let synthesized = v2
                .providers
                .take()
                .and_then(|v| match v {
                    toml::Value::Table(t) => Some(t),
                    _ => None,
                })
                .unwrap_or_default();
            let mut merged = user_providers;
            for (k, v) in synthesized {
                merged.insert(k, v);
            }
            if !merged.is_empty() {
                v2.providers = Some(toml::Value::Table(merged));
            }
        }

        v2
    }
}
