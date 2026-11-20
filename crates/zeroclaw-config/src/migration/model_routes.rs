//! Released V4 route-owned model settings become ordinary V5 model definitions.
//! Conversion belongs to V4 → V5 admission and file migration. Runtime routes
//! resolve exact canonical model references and have no legacy path.

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
    let fallbacks = provider_table
        .get("fallback")
        .and_then(toml::Value::as_table)
        .cloned()
        .unwrap_or_default();
    let mut route_fallbacks = Vec::new();
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
                let mut definition = legacy_model(
                    &raw,
                    selection.family,
                    selection.model_entry,
                    explicit_model,
                )?;
                // V4 strict_tool_parsing was preserved as a profile marker.
                // When route admission selects the profile's default/sole
                // entry, pass that marker to the materialized route model as
                // well; otherwise this path silently loses the primary
                // parser policy while ordinary model admission keeps it.
                // A named model entry owns its parser policy.  Only copy the
                // retired profile marker when the reference is exactly the
                // profile key (the default/sole entry case).  TOML keys are
                // opaque: aliases may contain dots, so a suffix check would
                // incorrectly apply the profile policy to a named sibling.
                if selection.model_entry.is_some()
                    && reference == format!("{}.{}", selection.family, selection.alias)
                    && let Some(policy) = raw
                        .get("provider_extra")
                        .and_then(|extra| extra.get("__zeroclaw_migration_extract_text_tool_calls"))
                {
                    definition.1["options"]["extract_text_tool_calls"] = policy.clone();
                }
                definition
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
            if let Some(key) = key.filter(|key| !key.trim().is_empty()) {
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
            if let Some(targets) = fallbacks.get(reference) {
                route_fallbacks.push((name.clone(), targets.clone()));
            }
            model_ref = name;
        }
        let mut row = toml::Table::new();
        row.insert("hint".into(), hint.into());
        row.insert("model_ref".into(), model_ref.into());
        updated.push(row.into());
    }
    provider_table.insert("models".into(), models.into());
    provider_table.insert("connections".into(), connections.into());
    if !route_fallbacks.is_empty() {
        let fallback = provider_table
            .entry("fallback")
            .or_insert_with(|| toml::Value::Table(toml::Table::new()))
            .as_table_mut()
            .context("providers.fallback must be a table")?;
        for (source, targets) in route_fallbacks {
            fallback.insert(source, targets);
        }
    }
    let document_table = document.as_table_mut().context("config must be a table")?;
    document_table.insert("providers".into(), providers);
    document_table.insert("model_routes".into(), updated.into());
    Ok(document)
}

pub(super) fn legacy_chat_api_fields(kind: &str) -> &'static [(&'static str, &'static str)] {
    match kind {
        "gemini_cli" | "kilocli" => &[("binary_path", "binary_path")],
        "grok_cli" => &[
            ("timeout_secs", "request_timeout_secs"),
            ("binary_path", "binary_path"),
            ("working_directory", "working_directory"),
            ("env_passthrough", "env_passthrough"),
            ("extra_args", "extra_args"),
            ("max_acp_stdout_bytes", "max_acp_stdout_bytes"),
        ],
        "hailo_ollama" => &[
            ("timeout_secs", "request_timeout_secs"),
            ("queue_timeout_secs", "queue_timeout_secs"),
        ],
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
    // V4 runtime-profile policy is carried through the typed V4 decoder in a
    // reserved provider_extra key. Consume it here before projecting ordinary
    // provider_extra values into the canonical model options.
    let mut raw = raw.clone();
    let migrated_extract_text_tool_calls = raw
        .get_mut("provider_extra")
        .and_then(Value::as_object_mut)
        .and_then(|extra| extra.remove("__zeroclaw_migration_extract_text_tool_calls"));
    // A profile-level marker belongs only to its primary selection. Named
    // models carry their own marker and must not inherit a sibling's policy.
    let migrated_extract_text_tool_calls =
        migrated_extract_text_tool_calls.filter(|_| entry.is_none());
    if raw
        .get("provider_extra")
        .and_then(Value::as_object)
        .is_some_and(Map::is_empty)
    {
        raw.as_object_mut()
            .and_then(|object| object.remove("provider_extra"));
    }
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
    if kind == "openai"
        && raw.get("requires_openai_auth").and_then(Value::as_bool) == Some(true)
        && raw
            .get("api_key")
            .and_then(Value::as_str)
            .is_none_or(|key| key.trim().is_empty())
    {
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
        || (matches!(kind, "qwen" | "minimax" | "gemini")
            && raw
                .get("auth_mode")
                .and_then(Value::as_str)
                .is_some_and(|mode| matches!(mode, "o_auth" | "oauth"))
            && (kind != "qwen"
                || raw
                    .get("api_key")
                    .and_then(Value::as_str)
                    .is_none_or(|key| key.trim().is_empty())))
    {
        let mut auth = json!({"mode":"oauth"});
        for &(old, new) in LEGACY_OAUTH_FIELDS {
            if (kind == "gemini" && new == "refresh_token")
                || (kind != "gemini" && new == "client_secret")
            {
                continue;
            }
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
    if kind == "grok_cli" && api.get("request_timeout_secs").and_then(Value::as_u64) == Some(0) {
        // The historical process builder treated zero as the bounded default.
        api.remove("request_timeout_secs");
    }
    if kind != "grok_cli"
        && let Some(timeout) = api.get("request_timeout_secs").and_then(Value::as_u64)
    {
        if timeout == 0 {
            // V4 ignored zero rather than disabling transport bounds.
            api.remove("request_timeout_secs");
        } else if kind != "hailo_ollama" {
            // V4's finite timeout applied only to non-streaming requests.
            // Express that policy with ordinary V5 fields instead of a
            // legacy-only runtime branch or a new total streaming deadline.
            api.insert("stream_request_timeout_secs".into(), json!("null"));
            if !matches!(kind, "anthropic" | "openrouter") {
                api.insert("read_timeout_secs".into(), json!(timeout.max(300)));
            }
        }
    }
    // V3 omitted wire_api means chat completions, including old OpenAI rows.
    if !matches!(kind, "gemini_cli" | "kilocli" | "grok_cli" | "hailo_ollama") {
        api.entry("wire_api").or_insert(json!("chat_completions"));
    }
    // Hailo's queue deadline is connection policy; the inference request
    // deadline belongs to the selected model's API override.
    let hailo_request_timeout = (kind == "hailo_ollama")
        .then(|| api.remove("request_timeout_secs"))
        .flatten();
    connection["api"] = json!({"chat":api});
    let mut entry = entry
        .map(serde_json::to_value)
        .transpose()?
        .unwrap_or(json!({}));
    let migrated_extract_text_tool_calls = entry
        .get_mut("provider_extra")
        .and_then(Value::as_object_mut)
        .and_then(|extra| extra.remove("__zeroclaw_migration_extract_text_tool_calls"))
        .or(migrated_extract_text_tool_calls);
    if entry
        .get("provider_extra")
        .and_then(Value::as_object)
        .is_some_and(Map::is_empty)
    {
        entry
            .as_object_mut()
            .and_then(|object| object.remove("provider_extra"));
    }
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
    if let Some(value) = migrated_extract_text_tool_calls {
        let value = value
            .as_bool()
            .context("migrated extract_text_tool_calls must be a boolean")?;
        options.insert("extract_text_tool_calls".into(), value.into());
    } else if raw.get("native_tools").and_then(Value::as_bool) == Some(false) {
        // V4's prompt-guided tool mode parsed tool calls from content even on
        // custom gateways. V5 defaults remain off for new custom models.
        options.insert("extract_text_tool_calls".into(), true.into());
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
    if let Some(timeout) = hailo_request_timeout {
        model["api"] = json!({"request_timeout_secs":timeout});
    }
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
            let (connection, model) = legacy_model(&raw, kind, None, None).unwrap();
            let decoded: ProviderConnection = serde_json::from_value(connection.clone()).unwrap();
            assert_eq!(decoded.kind(), kind);
            if kind == "grok_cli" {
                assert_eq!(connection["api"]["chat"]["request_timeout_secs"], 30);
                assert!(
                    connection["api"]["chat"]
                        .get("stream_request_timeout_secs")
                        .is_none()
                );
                assert!(connection["api"]["chat"].get("read_timeout_secs").is_none());
            } else {
                assert!(
                    connection["api"]["chat"]
                        .get("request_timeout_secs")
                        .is_none()
                );
            }
            if kind == "hailo_ollama" {
                assert_eq!(model["api"]["request_timeout_secs"], 30);
                assert!(model["api"].get("stream_request_timeout_secs").is_none());
                assert!(model["api"].get("read_timeout_secs").is_none());
                assert_eq!(connection["api"]["chat"]["queue_timeout_secs"], 12);
            }
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
    fn grok_zero_timeout_keeps_the_bounded_process_default() {
        let (connection, _) = legacy_model(
            &json!({"model":"chat", "working_directory":"/workspace", "timeout_secs":0}),
            "grok_cli",
            None,
            None,
        )
        .unwrap();
        assert!(
            connection["api"]["chat"]
                .get("request_timeout_secs")
                .is_none()
        );
        serde_json::from_value::<ProviderConnection>(connection).unwrap();
    }

    #[test]
    fn qwen_explicit_api_key_wins_over_oauth_mode_without_refresh_token() {
        let (connection, _) = legacy_model(
            &json!({
                "model": "qwen-plus",
                "auth_mode": "o_auth",
                "api_key": "explicit-key"
            }),
            "qwen",
            None,
            None,
        )
        .unwrap();
        assert_eq!(connection["auth"]["mode"], "api_key");
        assert_eq!(connection["auth"]["api_key"], "explicit-key");
    }

    #[test]
    fn finite_v4_http_timeout_does_not_create_a_stream_total_deadline() {
        for timeout in [30, 600] {
            let (connection, _) = legacy_model(
                &json!({"model":"chat", "uri":"https://example.test/v1", "timeout_secs":timeout}),
                "custom",
                None,
                None,
            )
            .unwrap();
            assert_eq!(connection["api"]["chat"]["request_timeout_secs"], timeout);
            assert_eq!(
                connection["api"]["chat"]["stream_request_timeout_secs"],
                "null"
            );
            assert_eq!(
                connection["api"]["chat"]["read_timeout_secs"],
                timeout.max(300)
            );
            serde_json::from_value::<ProviderConnection>(connection).unwrap();
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

    #[test]
    fn blank_route_api_key_keeps_the_inherited_connection_key() {
        let document: toml::Value = toml::from_str(
            r#"
schema_version = 4

[providers.models.openai.main]
model = "gpt-4o"
api_key = "profile-key"

[[model_routes]]
hint = "blank-key"
model_provider = "openai.main"
api_key = "   "
"#,
        )
        .unwrap();
        let migrated = migrate(document).unwrap();
        assert_eq!(
            migrated["providers"]["connections"]["route.blank-key"]["auth"]["api_key"],
            "profile-key".into()
        );
    }

    #[test]
    fn generated_route_models_keep_the_source_fallback_chain() {
        let document: toml::Value = toml::from_str(
            r#"
schema_version = 4

[providers.models.openai.main]
model = "gpt-4o"
fallback = ["openai.backup"]

[providers.models.openai.backup]
model = "gpt-4o-backup"

[providers.fallback]
"openai.main" = ["openai.backup"]

[[model_routes]]
hint = "effort"
model_provider = "openai.main"
model = "gpt-4o-route"
"#,
        )
        .unwrap();
        let migrated = migrate(document).unwrap();
        assert_eq!(
            migrated["providers"]["fallback"]["route.effort"],
            toml::Value::Array(vec![toml::Value::String("openai.backup".into())])
        );
    }

    #[test]
    fn named_route_does_not_inherit_profile_parser_marker() {
        let document: toml::Value = toml::from_str(
            r#"
schema_version = 4

[providers.models.ollama.main]
model = "primary"
provider_extra = { __zeroclaw_migration_extract_text_tool_calls = true }
[providers.models.ollama.main.models.fast]
id = "fast"

[[model_routes]]
hint = "fast"
model_provider = "ollama.main.fast"
"#,
        )
        .unwrap();
        let migrated = migrate(document).unwrap();
        let named = &migrated["providers"]["models"]["route.fast"]["options"];
        assert!(named.get("extract_text_tool_calls").is_none());
    }
}
