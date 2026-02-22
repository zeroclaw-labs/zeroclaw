/// Render a minimal config.toml for a tenant container.
///
/// Generates [gateway] and [memory] sections always.
/// Omits [autonomy] so ZeroClaw uses safe defaults (supervised, workspace_only=true).
/// Optionally adds [proxy] and [agent] sections.
pub fn render_config_toml(
    _autonomy_level: &str,
    system_prompt: Option<&str>,
    _egress_proxy_url: &str,
) -> String {
    // Minimal config: required top-level fields + gateway override.
    // Gateway host/port/public-bind controlled via env vars (ZEROCLAW_GATEWAY_HOST etc).
    // All section configs (autonomy, memory, proxy, etc.) use ZeroClaw defaults.
    let mut config = String::from(
        r#"default_temperature = 0.7

[gateway]
trust_proxy = true
host = "0.0.0.0"
allow_public_bind = true
require_pairing = true
"#,
    );

    if let Some(prompt) = system_prompt {
        // Escape any TOML-special characters in the prompt string
        let escaped = prompt.replace('\\', "\\\\").replace('"', "\\\"");
        config.push_str(&format!(
            r#"
[agent]
system_prompt = "{escaped}"
"#,
            escaped = escaped,
        ));
    }

    config
}

/// Build environment variable list for a tenant container.
///
/// Returns: ZEROCLAW_API_KEY, PROVIDER, ZEROCLAW_MODEL,
/// ZEROCLAW_GATEWAY_PORT, ZEROCLAW_WORKSPACE, ZEROCLAW_ALLOW_PUBLIC_BIND, HOME
pub fn build_env_vars(
    api_key: &str,
    provider: &str,
    model: &str,
    port: u16,
) -> Vec<(String, String)> {
    vec![
        ("ZEROCLAW_API_KEY".to_string(), api_key.to_string()),
        ("PROVIDER".to_string(), provider.to_string()),
        ("ZEROCLAW_MODEL".to_string(), model.to_string()),
        ("ZEROCLAW_GATEWAY_PORT".to_string(), port.to_string()),
        (
            "ZEROCLAW_WORKSPACE".to_string(),
            "/zeroclaw-data/workspace".to_string(),
        ),
        ("ZEROCLAW_ALLOW_PUBLIC_BIND".to_string(), "true".to_string()),
        ("HOME".to_string(), "/zeroclaw-data".to_string()),
    ]
}

/// Render channel sections into ZeroClaw `[channels_config]` format.
///
/// Each entry in `channels` is (channel_type, config_value).
/// Renders as `[channels_config.<type>]` sub-sections.
/// Maps platform field names to ZeroClaw config field names where needed.
pub fn render_channel_config(channels: &[(String, serde_json::Value)]) -> String {
    if channels.is_empty() {
        return String::new();
    }

    let mut out = String::from("[channels_config]\ncli = false\n");

    for (channel_type, config) in channels {
        out.push_str(&format!("\n[channels_config.{}]\n", channel_type));
        if let Some(obj) = config.as_object() {
            for (k, v) in obj {
                let key = map_channel_field(channel_type, k);
                render_toml_value(&mut out, &key, v);
            }
        }
    }
    out
}

/// Map platform channel field names to ZeroClaw config field names.
fn map_channel_field(channel_type: &str, field: &str) -> String {
    match (channel_type, field) {
        // Telegram: chat_id → allowed_users (ZeroClaw uses allowed_users list)
        ("telegram", "chat_id") => "allowed_users".to_string(),
        _ => field.to_string(),
    }
}

/// Render a single TOML key-value pair.
fn render_toml_value(out: &mut String, key: &str, value: &serde_json::Value) {
    match value {
        serde_json::Value::String(s) => {
            let escaped = s.replace('\\', "\\\\").replace('"', "\\\"");
            // Fields that ZeroClaw expects as arrays of strings
            if key == "allowed_users" {
                out.push_str(&format!("{} = [\"{}\"]\n", key, escaped));
            } else {
                out.push_str(&format!("{} = \"{}\"\n", key, escaped));
            }
        }
        serde_json::Value::Number(n) => {
            out.push_str(&format!("{} = {}\n", key, n));
        }
        serde_json::Value::Bool(b) => {
            out.push_str(&format!("{} = {}\n", key, b));
        }
        serde_json::Value::Array(arr) => {
            let items: Vec<String> = arr
                .iter()
                .filter_map(|v| v.as_str().map(|s| format!("\"{}\"", s)))
                .collect();
            out.push_str(&format!("{} = [{}]\n", key, items.join(", ")));
        }
        _ => {}
    }
}

/// Render tool settings JSON into TOML sections for config.toml.
///
/// Each top-level key in tool_settings becomes a `[key]` TOML section.
/// Fields ending in `_enc` are vault-decrypted and emitted under the original field name.
/// Unknown tool sections are silently skipped.
pub fn render_tool_settings(
    tool_settings: &serde_json::Value,
    vault: &crate::vault::VaultService,
) -> String {
    let obj = match tool_settings.as_object() {
        Some(o) => o,
        None => return String::new(),
    };

    // Only render known tool sections
    const KNOWN_TOOLS: &[&str] = &[
        "browser",
        "http_request",
        "web_search",
        "cron",
        "scheduler",
        "composio",
        "pushover",
        "autonomy",
    ];

    let mut out = String::new();

    for tool_name in KNOWN_TOOLS {
        if let Some(section) = obj.get(*tool_name) {
            if let Some(section_obj) = section.as_object() {
                out.push_str(&format!("\n[{}]\n", tool_name));
                for (key, value) in section_obj {
                    // Handle encrypted fields: strip _enc suffix and decrypt
                    if key.ends_with("_enc") {
                        let real_key = &key[..key.len() - 4];
                        if let Some(encrypted) = value.as_str() {
                            match vault.decrypt(encrypted) {
                                Ok(plaintext) => {
                                    let escaped =
                                        plaintext.replace('\\', "\\\\").replace('"', "\\\"");
                                    out.push_str(&format!("{} = \"{}\"\n", real_key, escaped));
                                }
                                Err(e) => {
                                    tracing::warn!(
                                        "tool_settings: decrypt failed for {}.{}: {}",
                                        tool_name,
                                        key,
                                        e
                                    );
                                }
                            }
                        }
                    } else {
                        render_toml_value(&mut out, key, value);
                    }
                }
            }
        }
    }

    out
}

/// Encrypt sensitive fields in tool_settings before storing in DB.
/// Returns a new Value with sensitive fields encrypted (suffixed with _enc).
pub fn encrypt_tool_secrets(
    tool_settings: &serde_json::Value,
    vault: &crate::vault::VaultService,
) -> anyhow::Result<serde_json::Value> {
    const TOOL_SECRETS: &[(&str, &[&str])] = &[
        ("web_search", &["api_key"]),
        ("composio", &["api_key"]),
        ("pushover", &["user_key", "app_token"]),
    ];

    let mut result = tool_settings.clone();

    if let Some(obj) = result.as_object_mut() {
        for &(tool_name, secret_fields) in TOOL_SECRETS {
            if let Some(section) = obj.get_mut(tool_name) {
                if let Some(section_obj) = section.as_object_mut() {
                    for &field in secret_fields {
                        if let Some(value) = section_obj.remove(field) {
                            if let Some(plaintext) = value.as_str() {
                                if !plaintext.is_empty() {
                                    let encrypted = vault.encrypt(plaintext)?;
                                    section_obj.insert(
                                        format!("{}_enc", field),
                                        serde_json::Value::String(encrypted),
                                    );
                                }
                            }
                        }
                    }
                }
            }
        }
    }

    Ok(result)
}

/// Mask sensitive encrypted fields for API response.
/// Replaces `_enc` fields with masked `****` values under the original key name.
pub fn mask_tool_secrets(tool_settings: &serde_json::Value) -> serde_json::Value {
    let mut result = tool_settings.clone();

    if let Some(obj) = result.as_object_mut() {
        for (_tool_name, section) in obj.iter_mut() {
            if let Some(section_obj) = section.as_object_mut() {
                let enc_keys: Vec<String> = section_obj
                    .keys()
                    .filter(|k| k.ends_with("_enc"))
                    .cloned()
                    .collect();
                for enc_key in enc_keys {
                    section_obj.remove(&enc_key);
                    let real_key = &enc_key[..enc_key.len() - 4];
                    section_obj.insert(
                        real_key.to_string(),
                        serde_json::Value::String("****".to_string()),
                    );
                }
            }
        }
    }

    result
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn test_render_config_includes_gateway() {
        let cfg = render_config_toml("supervised", None, "");
        assert!(cfg.contains("[gateway]"));
        assert!(cfg.contains("trust_proxy = true"));
        assert!(cfg.contains("host = \"0.0.0.0\""));
        assert!(cfg.contains("allow_public_bind = true"));
        assert!(cfg.contains("default_temperature = 0.7"));
    }

    #[test]
    fn test_render_config_minimal_no_extra_sections() {
        let cfg = render_config_toml("supervised", None, "");
        assert!(!cfg.contains("[memory]"));
        assert!(!cfg.contains("[autonomy]"));
        assert!(!cfg.contains("[agent]"));
    }

    #[test]
    fn test_render_config_with_system_prompt() {
        let cfg = render_config_toml("autonomous", Some("You are a helpful assistant."), "");
        assert!(cfg.contains("[agent]"));
        assert!(cfg.contains("system_prompt = \"You are a helpful assistant.\""));
    }

    #[test]
    fn test_build_env_vars_keys() {
        let vars = build_env_vars("sk-test-key", "openai", "gpt-4o", 9000);
        let keys: Vec<&str> = vars.iter().map(|(k, _)| k.as_str()).collect();
        assert!(keys.contains(&"ZEROCLAW_API_KEY"));
        assert!(keys.contains(&"PROVIDER"));
        assert!(keys.contains(&"ZEROCLAW_MODEL"));
        assert!(keys.contains(&"ZEROCLAW_GATEWAY_PORT"));
        assert!(keys.contains(&"ZEROCLAW_WORKSPACE"));
        assert!(keys.contains(&"ZEROCLAW_ALLOW_PUBLIC_BIND"));
        assert!(keys.contains(&"HOME"));

        // Verify values
        let map: std::collections::HashMap<&str, &str> =
            vars.iter().map(|(k, v)| (k.as_str(), v.as_str())).collect();
        assert_eq!(map["ZEROCLAW_API_KEY"], "sk-test-key");
        assert_eq!(map["PROVIDER"], "openai");
        assert_eq!(map["ZEROCLAW_MODEL"], "gpt-4o");
        assert_eq!(map["ZEROCLAW_GATEWAY_PORT"], "9000");
        assert_eq!(map["ZEROCLAW_ALLOW_PUBLIC_BIND"], "true");
    }

    #[test]
    fn test_render_channel_config() {
        let channels = vec![
            (
                "telegram".to_string(),
                serde_json::json!({
                    "bot_token": "bot-token-123",
                    "chat_id": "12345"
                }),
            ),
            (
                "discord".to_string(),
                serde_json::json!({
                    "bot_token": "discord-token",
                    "guild_id": "guild-123"
                }),
            ),
        ];
        let rendered = render_channel_config(&channels);
        assert!(rendered.contains("[channels_config]"));
        assert!(rendered.contains("[channels_config.telegram]"));
        assert!(rendered.contains("bot_token = \"bot-token-123\""));
        // chat_id mapped to allowed_users array
        assert!(rendered.contains("allowed_users = [\"12345\"]"));
        assert!(rendered.contains("[channels_config.discord]"));
        assert!(rendered.contains("guild_id = \"guild-123\""));
    }

    #[test]
    fn test_render_channel_config_empty() {
        let rendered = render_channel_config(&[]);
        assert!(rendered.is_empty());
    }

    #[test]
    fn test_render_tool_settings_basic() {
        let vault = crate::vault::VaultService::new_for_test();
        let settings = serde_json::json!({
            "browser": { "enabled": true },
            "http_request": { "enabled": true, "allowed_domains": ["api.example.com"] },
            "cron": { "enabled": false }
        });
        let rendered = render_tool_settings(&settings, &vault);
        assert!(rendered.contains("[browser]"));
        assert!(rendered.contains("enabled = true"));
        assert!(rendered.contains("[http_request]"));
        assert!(rendered.contains("allowed_domains = [\"api.example.com\"]"));
        assert!(rendered.contains("[cron]"));
        assert!(rendered.contains("enabled = false"));
    }

    #[test]
    fn test_render_tool_settings_empty() {
        let vault = crate::vault::VaultService::new_for_test();
        let settings = serde_json::json!({});
        let rendered = render_tool_settings(&settings, &vault);
        assert!(rendered.is_empty());
    }

    #[test]
    fn test_render_tool_settings_unknown_ignored() {
        let vault = crate::vault::VaultService::new_for_test();
        let settings = serde_json::json!({
            "unknown_tool": { "enabled": true },
            "browser": { "enabled": true }
        });
        let rendered = render_tool_settings(&settings, &vault);
        assert!(rendered.contains("[browser]"));
        assert!(!rendered.contains("[unknown_tool]"));
    }

    #[test]
    fn test_render_tool_settings_autonomy() {
        let vault = crate::vault::VaultService::new_for_test();
        let settings = serde_json::json!({
            "autonomy": {
                "level": "supervised",
                "workspace_only": true,
                "allowed_commands": ["ls", "cat", "grep"]
            }
        });
        let rendered = render_tool_settings(&settings, &vault);
        assert!(rendered.contains("[autonomy]"));
        assert!(rendered.contains("level = \"supervised\""));
        assert!(rendered.contains("workspace_only = true"));
        assert!(rendered.contains("allowed_commands = [\"ls\", \"cat\", \"grep\"]"));
    }

    #[test]
    fn test_encrypt_tool_secrets() {
        let vault = crate::vault::VaultService::new_for_test();
        let settings = serde_json::json!({
            "web_search": { "enabled": true, "provider": "google", "api_key": "test-key-123" },
            "browser": { "enabled": true }
        });
        let encrypted = encrypt_tool_secrets(&settings, &vault).unwrap();
        let ws = encrypted.get("web_search").unwrap().as_object().unwrap();
        // api_key should be removed and api_key_enc added
        assert!(!ws.contains_key("api_key"));
        assert!(ws.contains_key("api_key_enc"));
        assert_eq!(ws["enabled"], true);
        assert_eq!(ws["provider"], "google");
        // browser should be untouched
        assert_eq!(encrypted["browser"]["enabled"], true);
    }

    #[test]
    fn test_mask_tool_secrets() {
        let settings = serde_json::json!({
            "web_search": { "enabled": true, "api_key_enc": "encrypted-value" },
            "browser": { "enabled": true }
        });
        let masked = mask_tool_secrets(&settings);
        let ws = masked.get("web_search").unwrap().as_object().unwrap();
        assert!(!ws.contains_key("api_key_enc"));
        assert_eq!(ws["api_key"], "****");
        assert_eq!(masked["browser"]["enabled"], true);
    }

    #[test]
    fn test_encrypt_then_render_roundtrip() {
        let vault = crate::vault::VaultService::new_for_test();
        let settings = serde_json::json!({
            "web_search": { "enabled": true, "provider": "google", "api_key": "my-secret-key" }
        });
        let encrypted = encrypt_tool_secrets(&settings, &vault).unwrap();
        let rendered = render_tool_settings(&encrypted, &vault);
        assert!(rendered.contains("[web_search]"));
        assert!(rendered.contains("enabled = true"));
        assert!(rendered.contains("provider = \"google\""));
        assert!(rendered.contains("api_key = \"my-secret-key\""));
    }
}
