//! V3 route-owned model/credential values become ordinary V4 model definitions.
//! Conversion belongs to config loading, including files already stamped V4
//! by releases with the old route shape. Runtime routes have no legacy path.

use anyhow::{Context, Result};
use serde_json::{Map, Value, json};

use crate::provider_registry::{ChatOptionsPatch, ModelRef, ProviderConnection};
use crate::schema::Config;

pub(super) fn needs_migration(document: &toml::Value) -> bool {
    document
        .get("model_routes")
        .and_then(toml::Value::as_array)
        .is_some_and(|routes| {
            routes
                .iter()
                .any(|route| route.get("model_provider").is_some())
        })
}

pub(super) fn migrate(mut document: toml::Value) -> Result<toml::Value> {
    if !needs_migration(&document) {
        return Ok(document);
    }
    let routes = document["model_routes"]
        .as_array()
        .context("model_routes must be an array")?;
    let mut input = document.clone();
    input
        .as_table_mut()
        .context("config must be a table")?
        .remove("model_routes");
    // Route-owned overrides need the historical profile before full admission
    // (for example, pricing must follow an overridden upstream model).
    let config = Config {
        providers: input
            .get("providers")
            .cloned()
            .map(toml::Value::try_into)
            .transpose()
            .context("read V3 route provider definitions")?
            .unwrap_or_default(),
        ..Config::default()
    };
    let mut providers = document
        .get("providers")
        .cloned()
        .unwrap_or(toml::Value::Table(toml::Table::new()));
    let provider_table = providers
        .as_table_mut()
        .context("providers must be a table")?;
    let mut models = provider_table
        .get("models")
        .and_then(toml::Value::as_table)
        .cloned()
        .unwrap_or_default();
    let mut connections = provider_table
        .get("connections")
        .and_then(toml::Value::as_table)
        .cloned()
        .unwrap_or_default();
    let mut updated = Vec::with_capacity(routes.len());
    for (index, route) in routes.iter().enumerate() {
        let Some(reference) = route.get("model_provider").and_then(toml::Value::as_str) else {
            updated.push(route.clone());
            continue;
        };
        let reference = reference.trim();
        let hint = route
            .get("hint")
            .and_then(toml::Value::as_str)
            .unwrap_or_default();
        let explicit_model = route
            .get("model")
            .and_then(toml::Value::as_str)
            .filter(|model| !model.trim().is_empty());
        let key = route.get("api_key").and_then(toml::Value::as_str);
        let canonical = config
            .providers
            .models
            .canonical_model(&ModelRef::from(reference));
        let mut model_ref = reference.to_owned();
        if canonical.is_none() || explicit_model.is_some() || key.is_some() {
            let (mut connection, mut model) = if let Some(model) = canonical {
                let connection = config
                    .providers
                    .connections
                    .get(model.provider())
                    .context("V3 route references a model with no connection")?;
                (
                    serde_json::to_value(connection)?,
                    serde_json::to_value(model)?,
                )
            } else if let Some(selection) = config.resolve_model_selection(reference) {
                let profiles = serde_json::to_value(&config.providers.models)?;
                let raw = profiles[selection.family][selection.alias].clone();
                legacy_model(
                    &raw,
                    selection.family,
                    selection.model_entry,
                    explicit_model,
                )?
            } else if !reference.is_empty() && explicit_model.is_some() {
                let family = reference
                    .split_once('.')
                    .map_or(reference, |(family, _)| family);
                legacy_model(&json!({}), family, None, explicit_model)?
            } else {
                // Keep an invalid/incomplete reference invalid for the normal
                // config validator; never silently substitute the primary.
                let mut row = toml::Table::new();
                row.insert("hint".into(), hint.into());
                row.insert("model_ref".into(), reference.into());
                updated.push(row.into());
                continue;
            };
            if let Some(model_id) = explicit_model {
                model["model"] = model_id.into();
            }
            if let Some(key) = key {
                connection["auth"] = json!({"mode": "api_key", "api_key": key});
            }
            let base = format!("route.{hint}");
            let mut name = base.clone();
            let mut suffix = 1;
            while models.contains_key(&name) || connections.contains_key(&name) {
                name = format!("{base}.{suffix}");
                suffix += 1;
            }
            model["provider"] = name.clone().into();
            // Decode the connection here to avoid producing a document which
            // only fails much later at first use. Values never enter errors.
            serde_json::from_value::<ProviderConnection>(connection.clone())
                .with_context(|| format!("cannot migrate model_routes[{index}] connection"))?;
            connections.insert(name.clone(), toml::Value::try_from(connection)?);
            models.insert(name.clone(), toml::Value::try_from(model)?);
            model_ref = name;
        }
        let mut row = toml::Table::new();
        row.insert("hint".into(), hint.into());
        row.insert("model_ref".into(), model_ref.into());
        updated.push(row.into());
    }
    provider_table.insert("models".into(), models.into());
    provider_table.insert("connections".into(), connections.into());
    let document_table = document.as_table_mut().context("config must be a table")?;
    document_table.insert("providers".into(), providers);
    document_table.insert("model_routes".into(), updated.into());
    Ok(document)
}

pub(super) fn legacy_chat_api_fields(kind: &str) -> &'static [(&'static str, &'static str)] {
    match kind {
        "gemini_cli" | "kilocli" => &[("binary_path", "binary_path")],
        "grok_cli" => &[
            ("binary_path", "binary_path"),
            ("working_directory", "working_directory"),
            ("env_passthrough", "env_passthrough"),
            ("extra_args", "extra_args"),
            ("max_acp_stdout_bytes", "max_acp_stdout_bytes"),
        ],
        "hailo_ollama" => &[("queue_timeout_secs", "queue_timeout_secs")],
        _ => &[
            ("wire_api", "wire_api"),
            ("timeout_secs", "request_timeout_secs"),
            ("live_pricing", "live_pricing"),
        ],
    }
}

pub(super) const LEGACY_CONNECTION_EXTRA_FIELDS: &[(&str, &str)] = &[
    ("resource", "resource"),
    ("api_version", "api_version"),
    ("region", "region"),
    ("oauth_project", "project"),
];

pub(super) const LEGACY_OAUTH_FIELDS: &[(&str, &str)] = &[
    ("oauth_refresh_token", "refresh_token"),
    ("oauth_client_id", "client_id"),
    ("oauth_client_secret", "client_secret"),
];

pub(super) fn legacy_model(
    raw: &Value,
    family: &str,
    entry: Option<&crate::schema::ModelEntryConfig>,
    model_override: Option<&str>,
) -> Result<(Value, Value)> {
    let kind = raw.get("kind").and_then(Value::as_str).unwrap_or(family);
    let kind = if kind == "openai-compatible" {
        "custom"
    } else {
        kind
    };
    let template: ProviderConnection = serde_json::from_value(json!({"kind": kind}))?;
    let mut connection = json!({"kind": kind});
    for field in ["uri", "extra_headers", "tls_ca_cert_path"] {
        if template.accepts_connection_field(field)
            && let Some(value) = raw.get(field)
        {
            connection[field] = value.clone();
        }
    }
    if kind == "qwen"
        && connection.get("uri").is_none_or(Value::is_null)
        && let Some(resource) = raw
            .get("oauth_resource_url")
            .filter(|value| !value.is_null())
    {
        connection["uri"] = resource.clone();
    }
    let mut extra = Map::new();
    for &(old, new) in LEGACY_CONNECTION_EXTRA_FIELDS {
        if let Some(value) = raw.get(old).filter(|value| !value.is_null()) {
            extra.insert(new.into(), value.clone());
        }
    }
    if matches!(
        kind,
        "qwen" | "moonshot" | "stepfun" | "glm" | "minimax" | "zai" | "kilo"
    ) && let Some(endpoint) = raw.get("endpoint")
    {
        extra.insert("endpoint".into(), endpoint.clone());
    }
    if !extra.is_empty() {
        connection["extra"] = extra.into();
    }
    if raw.get("requires_openai_auth").and_then(Value::as_bool) == Some(true) {
        connection["auth"] = json!({"mode":"codex"});
    } else if (kind == "qwen"
        && raw
            .get("oauth_refresh_token")
            .and_then(Value::as_str)
            .is_some_and(|token| !token.trim().is_empty()))
        // Legacy Gemini selected managed/CLI OAuth when no explicit API key
        // was configured, even without auth_mode. Preserve its client
        // credentials when projecting or saving the canonical connection.
        || (kind == "gemini"
            && raw.get("auth_mode").is_none_or(Value::is_null)
            && raw
                .get("api_key")
                .and_then(Value::as_str)
                .is_none_or(|key| key.trim().is_empty())
            && ["oauth_client_id", "oauth_client_secret"].iter().any(|field| {
                raw.get(*field)
                    .and_then(Value::as_str)
                    .is_some_and(|value| !value.trim().is_empty())
            }))
        || raw
            .get("auth_mode")
            .and_then(Value::as_str)
            .is_some_and(|mode| matches!(mode, "o_auth" | "oauth"))
    {
        let mut auth = json!({"mode":"oauth"});
        for &(old, new) in LEGACY_OAUTH_FIELDS {
            if let Some(value) = raw.get(old).filter(|value| !value.is_null()) {
                auth[new] = value.clone();
            }
        }
        connection["auth"] = auth;
    } else if template.accepts_connection_field("auth") {
        connection["auth"] = json!({"mode":"api_key"});
        if let Some(key) = raw.get("api_key").filter(|value| !value.is_null()) {
            connection["auth"]["api_key"] = key.clone();
        }
    }
    let mut api = Map::new();
    let fields = legacy_chat_api_fields(kind);
    for &(old, new) in fields {
        if let Some(value) = raw.get(old).filter(|value| !value.is_null()) {
            api.insert(new.into(), value.clone());
        }
    }
    // V3 omitted wire_api means chat completions, including old OpenAI rows.
    if !matches!(kind, "gemini_cli" | "kilocli" | "grok_cli" | "hailo_ollama") {
        api.entry("wire_api").or_insert(json!("chat_completions"));
    }
    connection["api"] = json!({"chat":api});
    let entry = entry
        .map(serde_json::to_value)
        .transpose()?
        .unwrap_or(json!({}));
    let mut options = Map::new();
    for &field in ChatOptionsPatch::FIELDS {
        if let Some(value) = entry
            .get(field)
            .filter(|value| !value.is_null())
            .or_else(|| raw.get(field).filter(|value| !value.is_null()))
        {
            options.insert(field.into(), value.clone());
        }
    }
    let mut metadata = Map::new();
    for field in ["context_window", "vision"] {
        if let Some(value) = entry
            .get(field)
            .filter(|value| !value.is_null())
            .or_else(|| raw.get(field).filter(|value| !value.is_null()))
        {
            metadata.insert(field.into(), value.clone());
        }
    }
    let model_id = model_override.map(Value::from).or_else(|| {
        entry
            .get("id")
            .filter(|value| !value.is_null())
            .cloned()
            .or_else(|| raw.get("model").filter(|value| !value.is_null()).cloned())
    });
    if let Some(id) = model_id.as_ref().and_then(Value::as_str)
        && let Some(prices) = raw.get("pricing").and_then(Value::as_object)
    {
        let mut rates = Map::new();
        for (suffix, field) in [
            ("input", "input_per_mtok"),
            ("output", "output_per_mtok"),
            ("cached_input", "cached_input_per_mtok"),
            ("cache_write", "cache_write_per_mtok"),
        ] {
            let rate = prices.get(&format!("{id}.{suffix}")).or_else(|| {
                matches!(suffix, "input" | "output")
                    .then(|| prices.get(id))
                    .flatten()
            });
            if let Some(rate) = rate {
                rates.insert(field.into(), rate.clone());
            }
        }
        if !rates.is_empty() {
            metadata.insert("pricing".into(), rates.into());
        }
    }
    let mut model =
        json!({"capability":"chat", "provider":"", "options":options, "metadata":metadata});
    if let Some(id) = model_id {
        model["model"] = id;
    }
    if kind == "azure"
        && let Some(deployment) = raw.get("deployment")
    {
        model["extra"] = json!({"deployment":deployment});
    }
    if kind == "anthropic"
        && let Some(fallbacks) = raw
            .get("server_fallback_models")
            .filter(|value| value.as_array().is_some_and(|items| !items.is_empty()))
    {
        model["extra"] = json!({"server_fallback_models":fallbacks});
    }
    Ok((connection, model))
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn cli_routes_preserve_driver_settings_without_http_fields() {
        for kind in ["gemini_cli", "kilocli", "grok_cli", "hailo_ollama"] {
            let raw = json!({
                "model":"selected", "timeout_secs":30, "binary_path":"/usr/bin/driver",
                "working_directory":"/workspace", "env_passthrough":["XAI_API_KEY"],
                "extra_args":["--sandbox=strict"], "max_acp_stdout_bytes":1048576,
                "queue_timeout_secs":12
            });
            let (connection, _) = legacy_model(&raw, kind, None, None).unwrap();
            let decoded: ProviderConnection = serde_json::from_value(connection.clone()).unwrap();
            assert_eq!(decoded.kind(), kind);
            assert!(
                connection["api"]["chat"]
                    .get("request_timeout_secs")
                    .is_none()
            );
            if kind == "grok_cli" {
                for field in [
                    "binary_path",
                    "working_directory",
                    "env_passthrough",
                    "extra_args",
                    "max_acp_stdout_bytes",
                ] {
                    assert_eq!(connection["api"]["chat"][field], raw[field]);
                }
            }
        }
    }

    #[test]
    fn route_prices_follow_the_selected_model_including_explicit_override() {
        let raw = json!({"model":"primary", "pricing":{
            "primary":1.0, "target":2.0, "target.output":5.0,
            "target.cached_input":0.5, "target.cache_write":3.0
        }});
        let (_, model) = legacy_model(&raw, "openai", None, Some("target")).unwrap();
        assert_eq!(model["model"], "target");
        assert_eq!(
            model["metadata"]["pricing"],
            json!({
                "input_per_mtok":2.0, "output_per_mtok":5.0,
                "cached_input_per_mtok":0.5, "cache_write_per_mtok":3.0
            })
        );
        let (_, model) = legacy_model(&raw, "openai", None, None).unwrap();
        assert_eq!(model["metadata"]["pricing"]["input_per_mtok"], 1.0);
        assert_eq!(model["metadata"]["pricing"]["output_per_mtok"], 1.0);
    }
}
