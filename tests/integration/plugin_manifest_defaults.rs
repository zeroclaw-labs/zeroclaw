#![cfg(feature = "plugins-wasm")]

//! Integration test: non-sensitive manifest defaults are used as fallbacks.
//!
//! Acceptance criterion for US-ZCL-7:
//! "Non-sensitive defaults from manifest are used as fallbacks."
//!
//! Verifies that `resolve_plugin_config` falls back to manifest-declared defaults
//! across all supported declaration formats when the operator omits a key.

use std::collections::{BTreeMap, HashMap};

use zeroclaw::plugins::resolve_plugin_config;

/// Helper: build manifest config and resolve with the given operator values.
fn resolve(
    manifest_entries: Vec<(&str, serde_json::Value)>,
    operator_entries: Vec<(&str, &str)>,
) -> BTreeMap<String, String> {
    let manifest_config: HashMap<String, serde_json::Value> = manifest_entries
        .into_iter()
        .map(|(k, v)| (k.to_string(), v))
        .collect();

    let config_values: HashMap<String, String> = operator_entries
        .into_iter()
        .map(|(k, v)| (k.to_string(), v.to_string()))
        .collect();

    let cv = if config_values.is_empty() {
        None
    } else {
        Some(&config_values)
    };

    resolve_plugin_config("defaults-test", &manifest_config, cv)
        .expect("config resolution should succeed")
}

/// Bare string defaults (e.g. `model = "gpt-4"`) are used when operator omits the key.
#[test]
fn bare_string_default_used_as_fallback() {
    let resolved = resolve(
        vec![("model", serde_json::json!("gpt-4"))],
        vec![], // operator supplies nothing
    );

    assert_eq!(
        resolved.get("model").map(String::as_str),
        Some("gpt-4"),
        "bare string default should be used when operator omits the key"
    );
}

/// Object-style defaults (`{ default = "30" }`) are used when operator omits the key.
#[test]
fn object_default_field_used_as_fallback() {
    let resolved = resolve(
        vec![("timeout", serde_json::json!({"default": "30"}))],
        vec![],
    );

    assert_eq!(
        resolved.get("timeout").map(String::as_str),
        Some("30"),
        "object-style {{default: ...}} should be used when operator omits the key"
    );
}

/// Non-string manifest values (numbers, bools) are stringified and used as defaults.
#[test]
fn non_string_values_stringified_as_defaults() {
    let resolved = resolve(
        vec![
            ("retries", serde_json::json!(3)),
            ("verbose", serde_json::json!(true)),
        ],
        vec![],
    );

    assert_eq!(
        resolved.get("retries").map(String::as_str),
        Some("3"),
        "numeric default should be stringified"
    );
    assert_eq!(
        resolved.get("verbose").map(String::as_str),
        Some("true"),
        "boolean default should be stringified"
    );
}

/// When operator supplies a value, the manifest default is NOT used.
#[test]
fn operator_value_overrides_manifest_default() {
    let resolved = resolve(
        vec![("model", serde_json::json!("gpt-4"))],
        vec![("model", "claude-3")],
    );

    assert_eq!(
        resolved.get("model").map(String::as_str),
        Some("claude-3"),
        "operator value should override manifest default"
    );
}

/// An object declaration with neither `required` nor `default` omits the key entirely.
#[test]
fn declaration_without_default_or_required_omits_key() {
    let resolved = resolve(
        vec![(
            "optional_flag",
            serde_json::json!({"description": "some flag"}),
        )],
        vec![],
    );

    assert!(
        !resolved.contains_key("optional_flag"),
        "key with no default and no required flag should be omitted, got: {resolved:?}"
    );
}

/// Multiple defaults of different formats all resolve correctly together.
#[test]
fn mixed_default_formats_all_resolve() {
    let resolved = resolve(
        vec![
            ("api_key", serde_json::json!({"required": true})),
            ("model", serde_json::json!("gpt-4")),
            ("timeout", serde_json::json!({"default": "30"})),
            ("retries", serde_json::json!(5)),
            ("debug", serde_json::json!(false)),
        ],
        vec![("api_key", "sk-test")], // only required key supplied
    );

    assert_eq!(resolved.get("api_key").map(String::as_str), Some("sk-test"));
    assert_eq!(resolved.get("model").map(String::as_str), Some("gpt-4"));
    assert_eq!(resolved.get("timeout").map(String::as_str), Some("30"));
    assert_eq!(resolved.get("retries").map(String::as_str), Some("5"));
    assert_eq!(resolved.get("debug").map(String::as_str), Some("false"));
}

/// Defaults are used even when `config_values` is `None` (no `[plugins.<name>]` section at all).
#[test]
fn defaults_used_when_no_plugin_config_section() {
    let manifest_config: HashMap<String, serde_json::Value> = [
        ("model".to_string(), serde_json::json!("gpt-4")),
        ("timeout".to_string(), serde_json::json!({"default": "60"})),
    ]
    .into_iter()
    .collect();

    let resolved = resolve_plugin_config("no-section", &manifest_config, None)
        .expect("should succeed — no required keys");

    assert_eq!(resolved.get("model").map(String::as_str), Some("gpt-4"));
    assert_eq!(resolved.get("timeout").map(String::as_str), Some("60"));
}
