#![cfg(feature = "plugins-wasm")]
//! Integration test: Config changes persist to config.toml via API.
//!
//! Verifies the acceptance criterion for US-ZCL-21:
//! > Config changes persist to config.toml via API
//!
//! Simulates the PATCH handler's config mutation flow: updates per-plugin config
//! values in memory, saves to disk via `Config::save()`, then reloads and verifies
//! the values survived the round-trip through TOML serialization.

use std::collections::HashMap;

use zeroclaw::config::Config;

/// Creates a minimal Config pointed at a temp directory so `save()` writes there.
fn config_in_temp_dir(dir: &std::path::Path) -> Config {
    let config_path = dir.join("config.toml");
    let mut config = Config {
        config_path,
        ..Default::default()
    };
    config.plugins.enabled = true;
    // Disable secret encryption so we don't need a real key store for this test.
    config.secrets.encrypt = false;
    config
}

/// Simulates the PATCH handler's per-plugin config update (api_plugins.rs:296-306).
fn apply_patch(config: &mut Config, plugin_name: &str, updates: &HashMap<String, String>) {
    let plugin_cfg = config
        .plugins
        .per_plugin
        .entry(plugin_name.to_string())
        .or_default();
    for (k, v) in updates {
        plugin_cfg.insert(k.clone(), v.clone());
    }
}

// ---- Persistence tests ----

#[tokio::test]
async fn config_patch_persists_single_key_to_toml() {
    let tmp = tempfile::tempdir().expect("tempdir");
    let mut config = config_in_temp_dir(tmp.path());

    let mut updates = HashMap::new();
    updates.insert("api_key".to_string(), "sk-persist-test".to_string());
    apply_patch(&mut config, "multi-tool", &updates);

    config.save().await.expect("save should succeed");

    // Reload from disk
    let raw =
        std::fs::read_to_string(tmp.path().join("config.toml")).expect("config.toml should exist");
    let reloaded: Config = toml::from_str(&raw).expect("config.toml should parse");

    let plugin_cfg = reloaded
        .plugins
        .per_plugin
        .get("multi-tool")
        .expect("[plugins.multi-tool] section should exist");
    assert_eq!(
        plugin_cfg.get("api_key").map(String::as_str),
        Some("sk-persist-test"),
        "api_key should survive save/reload round-trip"
    );
}

#[tokio::test]
async fn config_patch_persists_multiple_keys_to_toml() {
    let tmp = tempfile::tempdir().expect("tempdir");
    let mut config = config_in_temp_dir(tmp.path());

    let mut updates = HashMap::new();
    updates.insert("api_key".to_string(), "sk-multi".to_string());
    updates.insert("model".to_string(), "gpt-4o".to_string());
    apply_patch(&mut config, "multi-tool", &updates);

    config.save().await.expect("save should succeed");

    let raw =
        std::fs::read_to_string(tmp.path().join("config.toml")).expect("config.toml should exist");
    let reloaded: Config = toml::from_str(&raw).expect("config.toml should parse");

    let plugin_cfg = reloaded
        .plugins
        .per_plugin
        .get("multi-tool")
        .expect("[plugins.multi-tool] section should exist");
    assert_eq!(
        plugin_cfg.get("api_key").map(String::as_str),
        Some("sk-multi")
    );
    assert_eq!(plugin_cfg.get("model").map(String::as_str), Some("gpt-4o"));
}

#[tokio::test]
async fn config_patch_persists_multiple_plugins_to_toml() {
    let tmp = tempfile::tempdir().expect("tempdir");
    let mut config = config_in_temp_dir(tmp.path());

    let mut updates_a = HashMap::new();
    updates_a.insert("api_key".to_string(), "key-a".to_string());
    apply_patch(&mut config, "plugin-alpha", &updates_a);

    let mut updates_b = HashMap::new();
    updates_b.insert("endpoint".to_string(), "https://example.com".to_string());
    apply_patch(&mut config, "plugin-beta", &updates_b);

    config.save().await.expect("save should succeed");

    let raw =
        std::fs::read_to_string(tmp.path().join("config.toml")).expect("config.toml should exist");
    let reloaded: Config = toml::from_str(&raw).expect("config.toml should parse");

    let alpha_cfg = reloaded
        .plugins
        .per_plugin
        .get("plugin-alpha")
        .expect("[plugins.plugin-alpha] should exist");
    assert_eq!(alpha_cfg.get("api_key").map(String::as_str), Some("key-a"));

    let beta_cfg = reloaded
        .plugins
        .per_plugin
        .get("plugin-beta")
        .expect("[plugins.plugin-beta] should exist");
    assert_eq!(
        beta_cfg.get("endpoint").map(String::as_str),
        Some("https://example.com")
    );
}

#[tokio::test]
async fn config_patch_overwrites_existing_value_and_persists() {
    let tmp = tempfile::tempdir().expect("tempdir");
    let mut config = config_in_temp_dir(tmp.path());

    // First patch
    let mut updates = HashMap::new();
    updates.insert("model".to_string(), "gpt-3.5-turbo".to_string());
    apply_patch(&mut config, "multi-tool", &updates);
    config.save().await.expect("first save");

    // Second patch overwrites the value
    let mut updates2 = HashMap::new();
    updates2.insert("model".to_string(), "gpt-4o".to_string());
    apply_patch(&mut config, "multi-tool", &updates2);
    config.save().await.expect("second save");

    let raw =
        std::fs::read_to_string(tmp.path().join("config.toml")).expect("config.toml should exist");
    let reloaded: Config = toml::from_str(&raw).expect("config.toml should parse");

    let plugin_cfg = reloaded
        .plugins
        .per_plugin
        .get("multi-tool")
        .expect("[plugins.multi-tool] should exist");
    assert_eq!(
        plugin_cfg.get("model").map(String::as_str),
        Some("gpt-4o"),
        "overwritten value should persist"
    );
}

#[tokio::test]
async fn config_patch_toml_contains_plugin_section() {
    let tmp = tempfile::tempdir().expect("tempdir");
    let mut config = config_in_temp_dir(tmp.path());

    let mut updates = HashMap::new();
    updates.insert("api_key".to_string(), "sk-section-test".to_string());
    apply_patch(&mut config, "my-plugin", &updates);

    config.save().await.expect("save should succeed");

    // Verify the raw TOML contains the expected section header
    let raw =
        std::fs::read_to_string(tmp.path().join("config.toml")).expect("config.toml should exist");
    assert!(
        raw.contains("[plugins.my-plugin]"),
        "TOML should contain [plugins.my-plugin] section, got:\n{raw}"
    );
    assert!(
        raw.contains("sk-section-test"),
        "TOML should contain the config value"
    );
}

#[tokio::test]
async fn config_patch_preserves_other_config_fields() {
    let tmp = tempfile::tempdir().expect("tempdir");
    let mut config = config_in_temp_dir(tmp.path());
    config.plugins.enabled = true;
    config.plugins.auto_discover = true;

    let mut updates = HashMap::new();
    updates.insert("key".to_string(), "value".to_string());
    apply_patch(&mut config, "test-plugin", &updates);

    config.save().await.expect("save should succeed");

    let raw =
        std::fs::read_to_string(tmp.path().join("config.toml")).expect("config.toml should exist");
    let reloaded: Config = toml::from_str(&raw).expect("config.toml should parse");

    assert!(
        reloaded.plugins.enabled,
        "plugins.enabled should be preserved"
    );
    assert!(
        reloaded.plugins.auto_discover,
        "plugins.auto_discover should be preserved"
    );
}
