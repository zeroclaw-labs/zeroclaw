use serde::{Deserialize, Serialize};
use std::collections::HashMap;

use crate::migration::fold_string_into_array;
use crate::schema::v2::V2Config;

/// V1 partial typed lens. Names only fields that change in the V1→V2
/// step; everything else rides through `passthrough`.
#[derive(Debug, Default, Deserialize, Serialize)]
pub struct V1Config {
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

    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub channels_config: Option<toml::Value>,

    #[serde(flatten)]
    pub passthrough: toml::Table,
}

impl V1Config {
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

        // V1 had provider knobs at the top level; V2 moved them per-provider.
        // Fold each into the ModelProviderConfig entry identified by V1's
        // default_provider key with field renames as below.
        let has_v1_providers_data = default_provider.is_some()
            || default_model.is_some()
            || api_key.is_some()
            || api_url.is_some()
            || api_path.is_some()
            || default_temperature.is_some()
            || provider_timeout_secs.is_some()
            || provider_max_tokens.is_some()
            || !extra_headers.is_empty()
            || !model_providers.is_empty()
            || !model_routes.is_empty()
            || !embedding_routes.is_empty();

        let providers_value = if !has_v1_providers_data {
            None
        } else {
            // V1 runtime hardcoded "openrouter" as the fallback when
            // default_provider was unset; preserve that so a stock V1 install
            // round-trips.
            let default_provider_key: String = default_provider
                .as_ref()
                .and_then(|v| v.as_str())
                .map(str::to_string)
                .unwrap_or_else(|| "openrouter".to_string());

            let mut models_table: toml::Table = model_providers.into_iter().collect();

            let needs_fold = api_key.is_some()
                || api_url.is_some()
                || api_path.is_some()
                || default_model.is_some()
                || default_temperature.is_some()
                || provider_timeout_secs.is_some()
                || provider_max_tokens.is_some()
                || !extra_headers.is_empty();

            if needs_fold {
                let entry_value = models_table
                    .remove(&default_provider_key)
                    .unwrap_or_else(|| toml::Value::Table(toml::Table::new()));
                let mut entry_table = match entry_value {
                    toml::Value::Table(t) => t,
                    other => {
                        // Preserve verbatim; nothing to fold into a non-table.
                        models_table.insert(default_provider_key.clone(), other);
                        toml::Table::new()
                    }
                };

                // or_insert so any value the user already set on the
                // per-provider entry wins over the V1 top-level global —
                // matches V1 runtime preference (per-provider > global).
                if let Some(v) = api_key {
                    entry_table.entry("api_key".to_string()).or_insert(v);
                }
                if let Some(v) = api_url {
                    entry_table.entry("base_url".to_string()).or_insert(v);
                }
                if let Some(v) = api_path {
                    entry_table.entry("api_path".to_string()).or_insert(v);
                }
                if let Some(v) = default_model {
                    entry_table.entry("model".to_string()).or_insert(v);
                }
                if let Some(v) = default_temperature {
                    entry_table.entry("temperature".to_string()).or_insert(v);
                }
                if let Some(v) = provider_timeout_secs {
                    entry_table.entry("timeout_secs".to_string()).or_insert(v);
                }
                if let Some(v) = provider_max_tokens {
                    entry_table.entry("max_tokens".to_string()).or_insert(v);
                }
                if !extra_headers.is_empty() {
                    let headers_table: toml::Table = extra_headers.into_iter().collect();
                    entry_table
                        .entry("extra_headers".to_string())
                        .or_insert_with(|| toml::Value::Table(headers_table));
                }

                models_table.insert(
                    default_provider_key.clone(),
                    toml::Value::Table(entry_table),
                );

                ::zeroclaw_log::record!(INFO, ::zeroclaw_log::Event::new(module_path!(), ::zeroclaw_log::Action::Note).with_attrs(::serde_json::json!({"default_provider_key": default_provider_key})), "V1 top-level provider globals folded into [providers.models.]");
            }

            let mut providers = toml::Table::new();
            providers.insert(
                "fallback".to_string(),
                toml::Value::String(default_provider_key),
            );
            if !models_table.is_empty() {
                providers.insert("models".to_string(), toml::Value::Table(models_table));
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
            Some(toml::Value::Table(providers))
        };

        // Rename channels_config → channels and apply the singular→plural
        // folds V2 needs (matrix.room_id, slack.channel_id).
        if let Some(mut channels_value) = channels_config {
            if let Some(channels_table) = channels_value.as_table_mut() {
                apply_v1_to_v2_channel_folds(channels_table);
            }
            passthrough.insert("channels".to_string(), channels_value);
            ::zeroclaw_log::record!(INFO, ::zeroclaw_log::Event::new(module_path!(), ::zeroclaw_log::Action::Note), "channels_config → channels");
        }

        let mut v2 = V2Config {
            schema_version: 2,
            providers: providers_value,
            passthrough,
            ..V2Config::default()
        };

        // Hoist keys that `V2Config::migrate` (V2→V3) operates on out of
        // passthrough into the typed slots so the V2 lens sees them.
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
        // Edge case: V1 user wrote a [providers] block themselves (V2
        // section name). Merge their keys with the synthesized ones,
        // letting the synthesized values win.
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

/// V2 dropped the singular `matrix.room_id` and `slack.channel_id`
/// fields in favor of the plural `allowed_rooms[]` / `channel_ids[]`.
/// Move the V1 singular values into the plural slots so they survive.
fn apply_v1_to_v2_channel_folds(channels: &mut toml::Table) {
    if let Some(toml::Value::Table(matrix)) = channels.get_mut("matrix")
        && fold_string_into_array(matrix, "room_id", "allowed_rooms")
    {
        ::zeroclaw_log::record!(INFO, ::zeroclaw_log::Event::new(module_path!(), ::zeroclaw_log::Action::Note), "channels.matrix.room_id folded into channels.matrix.allowed_rooms[]");
    }
    if let Some(toml::Value::Table(slack)) = channels.get_mut("slack")
        && fold_string_into_array(slack, "channel_id", "channel_ids")
    {
        ::zeroclaw_log::record!(INFO, ::zeroclaw_log::Event::new(module_path!(), ::zeroclaw_log::Action::Note), "channels.slack.channel_id folded into channels.slack.channel_ids[]");
    }
}
