pub use zeroclaw_plugins::*;

pub mod archive;
pub mod host_functions;
pub mod loader;

use std::collections::{BTreeMap, HashMap};

/// Returns `true` if a manifest config declaration marks the key as sensitive.
pub fn is_sensitive_key(decl: &serde_json::Value) -> bool {
    decl.as_object()
        .and_then(|obj| obj.get("sensitive"))
        .and_then(|v| v.as_bool())
        .unwrap_or(false)
}

/// Resolve plugin configuration for Extism from ZeroClaw's `config.toml`.
#[allow(clippy::implicit_hasher)]
pub fn resolve_plugin_config(
    plugin_name: &str,
    manifest_config: &HashMap<String, serde_json::Value>,
    config_values: Option<&HashMap<String, String>>,
) -> Result<BTreeMap<String, String>, error::PluginError> {
    let empty = HashMap::new();
    let values = config_values.unwrap_or(&empty);

    let mut resolved = BTreeMap::new();
    let mut missing = Vec::new();

    for (key, decl) in manifest_config {
        let sensitive = is_sensitive_key(decl);

        if let Some(val) = values.get(key) {
            let display_val = if sensitive {
                crate::security::redact(val)
            } else {
                val.clone()
            };
            tracing::debug!(
                plugin = %plugin_name,
                key = %key,
                value = %display_val,
                sensitive,
                "resolved config key from operator config"
            );
            resolved.insert(key.clone(), val.clone());
            continue;
        }

        match decl {
            serde_json::Value::String(default) => {
                tracing::debug!(
                    plugin = %plugin_name,
                    key = %key,
                    "using bare-string default for config key"
                );
                resolved.insert(key.clone(), default.clone());
            }
            serde_json::Value::Object(obj) => {
                if obj
                    .get("required")
                    .and_then(|v| v.as_bool())
                    .unwrap_or(false)
                {
                    missing.push(key.clone());
                } else if let Some(default) = obj.get("default").and_then(|v| v.as_str()) {
                    let display_val = if sensitive {
                        crate::security::redact(default)
                    } else {
                        default.to_string()
                    };
                    tracing::debug!(
                        plugin = %plugin_name,
                        key = %key,
                        value = %display_val,
                        sensitive,
                        "using manifest default for config key"
                    );
                    resolved.insert(key.clone(), default.to_string());
                }
            }
            _ => {
                resolved.insert(key.clone(), decl.to_string());
            }
        }
    }

    if !missing.is_empty() {
        missing.sort();
        tracing::warn!(
            plugin = %plugin_name,
            missing_keys = %missing.join(", "),
            "plugin config resolution failed — required keys missing"
        );
        return Err(error::PluginError::MissingConfig {
            plugin: plugin_name.to_string(),
            keys: missing.join(", "),
        });
    }

    for (key, val) in values {
        if !resolved.contains_key(key) {
            tracing::debug!(
                plugin = %plugin_name,
                key = %key,
                "passing through undeclared config key from operator config"
            );
            resolved.insert(key.clone(), val.clone());
        }
    }

    tracing::info!(
        plugin = %plugin_name,
        keys = %resolved.keys().cloned().collect::<Vec<_>>().join(", "),
        "plugin config resolved successfully"
    );

    Ok(resolved)
}
