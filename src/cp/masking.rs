use serde_json::Value;

const MASKED: &str = "***MASKED***";

/// Replace known secret fields in a serialized config JSON with `"***MASKED***"`.
///
/// Walks enumerated paths corresponding to every secret field in `Config` and
/// its nested channel/tunnel/composio/model_routes structs. Null or missing
/// values are left as-is (they are not a leak).
pub fn mask_config_secrets(value: &mut Value) {
    // Top-level scalar secrets
    mask_path(value, &["api_key"]);

    // Channels
    mask_path(value, &["channels_config", "telegram", "bot_token"]);
    mask_path(value, &["channels_config", "discord", "bot_token"]);
    mask_path(value, &["channels_config", "slack", "bot_token"]);
    mask_path(value, &["channels_config", "slack", "app_token"]);
    mask_path(value, &["channels_config", "webhook", "secret"]);
    mask_path(value, &["channels_config", "matrix", "access_token"]);
    mask_path(value, &["channels_config", "whatsapp", "access_token"]);
    mask_path(value, &["channels_config", "whatsapp", "verify_token"]);
    mask_path(value, &["channels_config", "whatsapp", "app_secret"]);
    mask_path(value, &["channels_config", "irc", "server_password"]);
    mask_path(value, &["channels_config", "irc", "nickserv_password"]);
    mask_path(value, &["channels_config", "irc", "sasl_password"]);
    mask_path(value, &["channels_config", "email", "password"]);

    // Gateway paired tokens (array of strings)
    mask_array_elements(value, &["gateway", "paired_tokens"]);

    // Composio
    mask_path(value, &["composio", "api_key"]);

    // Tunnel
    mask_path(value, &["tunnel", "ngrok", "auth_token"]);
    mask_path(value, &["tunnel", "cloudflare", "token"]);

    // Model routes (array of objects, each may have api_key)
    if let Some(routes) = value.pointer_mut("/model_routes") {
        if let Some(arr) = routes.as_array_mut() {
            for route in arr.iter_mut() {
                mask_path(route, &["api_key"]);
            }
        }
    }
}

/// Walk a dotted path into a JSON value and replace the leaf with MASKED,
/// but only if the leaf is a non-null string.
fn mask_path(value: &mut Value, segments: &[&str]) {
    if segments.is_empty() {
        return;
    }

    let mut current = value;
    // Navigate to the parent of the final segment
    for &seg in &segments[..segments.len() - 1] {
        match current.get_mut(seg) {
            Some(child) if child.is_object() => current = child,
            _ => return, // Path doesn't exist or isn't an object -- nothing to mask
        }
    }

    let leaf_key = segments[segments.len() - 1];
    if let Some(leaf) = current.get(leaf_key) {
        if leaf.is_string() {
            current[leaf_key] = Value::String(MASKED.to_string());
        }
        // null / missing: leave as-is
    }
}

/// Mask every string element in an array at the given path.
fn mask_array_elements(value: &mut Value, segments: &[&str]) {
    if segments.is_empty() {
        return;
    }

    let mut current = value as &mut Value;
    for &seg in &segments[..segments.len() - 1] {
        match current.get_mut(seg) {
            Some(child) if child.is_object() => current = child,
            _ => return,
        }
    }

    let leaf_key = segments[segments.len() - 1];
    if let Some(arr_val) = current.get_mut(leaf_key) {
        if let Some(arr) = arr_val.as_array_mut() {
            for elem in arr.iter_mut() {
                if elem.is_string() {
                    *elem = Value::String(MASKED.to_string());
                }
            }
        }
    }
}

/// Test helper: recursively check whether any string value in `json` exactly
/// matches one of the provided `secrets`. Returns true if a leak is found.
#[cfg(test)]
pub fn contains_raw_secrets(json: &Value, secrets: &[&str]) -> bool {
    match json {
        Value::String(s) => secrets.contains(&s.as_str()),
        Value::Array(arr) => arr.iter().any(|v| contains_raw_secrets(v, secrets)),
        Value::Object(map) => map.values().any(|v| contains_raw_secrets(v, secrets)),
        _ => false,
    }
}

/// Collect all dotted key paths to leaf nodes in a JSON value.
/// Used to diff raw TOML keys against typed Config keys.
pub fn collect_key_paths(value: &Value, prefix: &str) -> std::collections::HashSet<String> {
    let mut paths = std::collections::HashSet::new();
    match value {
        Value::Object(map) => {
            for (k, v) in map {
                let path = if prefix.is_empty() {
                    k.clone()
                } else {
                    format!("{prefix}.{k}")
                };
                match v {
                    Value::Object(_) => {
                        paths.extend(collect_key_paths(v, &path));
                    }
                    Value::Array(arr) => {
                        // For arrays of objects, recurse into each element
                        let mut has_object = false;
                        for (i, elem) in arr.iter().enumerate() {
                            if elem.is_object() {
                                has_object = true;
                                let elem_prefix = format!("{path}[{i}]");
                                paths.extend(collect_key_paths(elem, &elem_prefix));
                            }
                        }
                        if !has_object {
                            // Array of scalars -- the path itself is a leaf
                            paths.insert(path);
                        }
                    }
                    _ => {
                        paths.insert(path);
                    }
                }
            }
        }
        _ => {
            if !prefix.is_empty() {
                paths.insert(prefix.to_string());
            }
        }
    }
    paths
}

#[cfg(test)]
mod tests {
    use super::*;
    use serde_json::json;

    #[test]
    fn mask_top_level_api_key() {
        let mut v = json!({ "api_key": "sk-secret-123", "default_temperature": 0.7 });
        mask_config_secrets(&mut v);
        assert_eq!(v["api_key"], MASKED);
        assert_eq!(v["default_temperature"], 0.7);
    }

    #[test]
    fn mask_nested_channel_tokens() {
        let mut v = json!({
            "channels_config": {
                "telegram": { "bot_token": "SECRET_TG" },
                "discord": { "bot_token": "SECRET_DC" },
                "slack": { "bot_token": "SECRET_SL", "app_token": "SECRET_SL_APP" },
            }
        });
        mask_config_secrets(&mut v);
        assert_eq!(v["channels_config"]["telegram"]["bot_token"], MASKED);
        assert_eq!(v["channels_config"]["discord"]["bot_token"], MASKED);
        assert_eq!(v["channels_config"]["slack"]["bot_token"], MASKED);
        assert_eq!(v["channels_config"]["slack"]["app_token"], MASKED);
    }

    #[test]
    fn mask_null_stays_null() {
        let mut v = json!({ "api_key": null, "composio": { "api_key": null } });
        mask_config_secrets(&mut v);
        assert!(v["api_key"].is_null());
        assert!(v["composio"]["api_key"].is_null());
    }

    #[test]
    fn mask_missing_paths_no_panic() {
        let mut v = json!({ "default_temperature": 0.7 });
        mask_config_secrets(&mut v); // Should not panic
        assert_eq!(v["default_temperature"], 0.7);
    }

    #[test]
    fn mask_gateway_paired_tokens_array() {
        let mut v = json!({
            "gateway": { "paired_tokens": ["tok-a", "tok-b", "tok-c"] }
        });
        mask_config_secrets(&mut v);
        let tokens = v["gateway"]["paired_tokens"].as_array().unwrap();
        for t in tokens {
            assert_eq!(t, MASKED);
        }
    }

    #[test]
    fn mask_model_routes_api_keys() {
        let mut v = json!({
            "model_routes": [
                { "hint": "fast", "provider": "groq", "model": "llama", "api_key": "SECRET_ROUTE" },
                { "hint": "reason", "provider": "openai", "model": "gpt4" }
            ]
        });
        mask_config_secrets(&mut v);
        assert_eq!(v["model_routes"][0]["api_key"], MASKED);
        assert_eq!(v["model_routes"][0]["hint"], "fast"); // non-secret preserved
        // Second route has no api_key -- should not be added
        assert!(v["model_routes"][1].get("api_key").is_none());
    }

    #[test]
    fn mask_tunnel_secrets() {
        let mut v = json!({
            "tunnel": {
                "ngrok": { "auth_token": "SECRET_NGROK" },
                "cloudflare": { "token": "SECRET_CF" }
            }
        });
        mask_config_secrets(&mut v);
        assert_eq!(v["tunnel"]["ngrok"]["auth_token"], MASKED);
        assert_eq!(v["tunnel"]["cloudflare"]["token"], MASKED);
    }

    #[test]
    fn mask_irc_passwords() {
        let mut v = json!({
            "channels_config": {
                "irc": {
                    "server_password": "SECRET_IRC_SRV",
                    "nickserv_password": "SECRET_IRC_NS",
                    "sasl_password": "SECRET_IRC_SASL",
                    "server": "irc.example.com"
                }
            }
        });
        mask_config_secrets(&mut v);
        assert_eq!(v["channels_config"]["irc"]["server_password"], MASKED);
        assert_eq!(v["channels_config"]["irc"]["nickserv_password"], MASKED);
        assert_eq!(v["channels_config"]["irc"]["sasl_password"], MASKED);
        assert_eq!(v["channels_config"]["irc"]["server"], "irc.example.com");
    }

    #[test]
    fn mask_whatsapp_secrets() {
        let mut v = json!({
            "channels_config": {
                "whatsapp": {
                    "access_token": "SECRET_WA_AT",
                    "verify_token": "SECRET_WA_VT",
                    "app_secret": "SECRET_WA_AS",
                    "phone_number_id": "12345"
                }
            }
        });
        mask_config_secrets(&mut v);
        assert_eq!(v["channels_config"]["whatsapp"]["access_token"], MASKED);
        assert_eq!(v["channels_config"]["whatsapp"]["verify_token"], MASKED);
        assert_eq!(v["channels_config"]["whatsapp"]["app_secret"], MASKED);
        assert_eq!(v["channels_config"]["whatsapp"]["phone_number_id"], "12345");
    }

    #[test]
    fn mask_email_password() {
        let mut v = json!({
            "channels_config": {
                "email": { "password": "SECRET_EMAIL", "username": "user@test.com" }
            }
        });
        mask_config_secrets(&mut v);
        assert_eq!(v["channels_config"]["email"]["password"], MASKED);
        assert_eq!(v["channels_config"]["email"]["username"], "user@test.com");
    }

    #[test]
    fn contains_raw_secrets_detects_leak() {
        let v = json!({ "config": { "key": "my-secret" } });
        assert!(contains_raw_secrets(&v, &["my-secret"]));
    }

    #[test]
    fn contains_raw_secrets_no_false_positive() {
        let v = json!({ "config": { "key": "***MASKED***" } });
        assert!(!contains_raw_secrets(&v, &["my-secret"]));
    }

    #[test]
    fn collect_key_paths_simple() {
        let v = json!({ "a": 1, "b": { "c": 2, "d": 3 } });
        let paths = collect_key_paths(&v, "");
        assert!(paths.contains("a"));
        assert!(paths.contains("b.c"));
        assert!(paths.contains("b.d"));
        assert!(!paths.contains("b")); // not a leaf
    }
}
