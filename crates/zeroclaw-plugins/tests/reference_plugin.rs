//! End-to-end load of the canonical reference plugin component through the real
//! wasmtime host path. The fixture is built from the standalone reference-plugin
//! app against `wit/v0`; loading it here proves the host instantiates and calls a
//! real component, the config jail injects only the plugin's own section, and the

#![cfg(feature = "plugins-wasm-cranelift")]

use std::collections::HashMap;
use std::path::PathBuf;

use zeroclaw_plugins::component::PluginLimits;
use zeroclaw_plugins::instance::PluginInstanceScope;
use zeroclaw_plugins::runtime;
use zeroclaw_plugins::{PluginCapability, PluginManifest, PluginPermission};

fn test_limits() -> PluginLimits {
    PluginLimits {
        call_fuel: 1_000_000_000,
        max_memory_bytes: 256 * 1024 * 1024,
        max_table_elements: 100_000,
        max_instances: 64,
    }
}

fn scope(grants: impl IntoIterator<Item = PluginPermission>) -> PluginInstanceScope {
    let permissions: Vec<_> = grants.into_iter().collect();
    let manifest = PluginManifest {
        name: "zeroclaw-reference-plugin".to_string(),
        version: "0.1.0".to_string(),
        description: None,
        author: None,
        wasm_path: Some("reference-plugin.wasm".to_string()),
        capabilities: vec![PluginCapability::Tool],
        permissions: permissions.clone(),
        signature: None,
        publisher_key: None,
    };
    PluginInstanceScope::from_manifest(&manifest, PluginCapability::Tool, "main", permissions)
        .expect("reference manifest admits its requested grants")
}

fn fixture() -> Option<PathBuf> {
    let path =
        PathBuf::from(env!("CARGO_MANIFEST_DIR")).join("tests/fixtures/reference-plugin.wasm");
    path.exists().then_some(path)
}

#[tokio::test]
async fn reference_plugin_reports_metadata() {
    let Some(fixture) = fixture() else {
        eprintln!("skipping: reference-plugin.wasm fixture not provisioned");
        return;
    };
    let scope = scope([]);
    let mut plugin = runtime::create_plugin(&fixture, &scope, test_limits())
        .await
        .expect("instantiate reference plugin");
    let meta = runtime::call_tool_metadata(&mut plugin)
        .await
        .expect("read tool metadata");
    assert_eq!(meta.name, "redact");
    assert!(meta.parameters_schema["properties"]["text"].is_object());
}

#[tokio::test]
async fn reference_plugin_redacts_with_config() {
    let Some(fixture) = fixture() else {
        eprintln!("skipping: reference-plugin.wasm fixture not provisioned");
        return;
    };
    let scope = scope([PluginPermission::ConfigRead]);
    let mut plugin = runtime::create_plugin(&fixture, &scope, test_limits())
        .await
        .expect("instantiate reference plugin");
    let config = HashMap::from([
        ("replacement".to_string(), "<X>".to_string()),
        ("patterns".to_string(), "swordfish".to_string()),
    ]);
    let result = runtime::call_execute(
        &mut plugin,
        br#"{"text":"ping me at a@b.com, pass is swordfish"}"#,
        &config,
    )
    .await
    .expect("execute redact tool");
    assert!(result.success);
    assert!(!result.output.contains("a@b.com"));
    assert!(!result.output.contains("swordfish"));
    assert!(result.output.contains("<X>"));
}

#[tokio::test]
async fn reference_plugin_jails_config_without_grant() {
    let Some(fixture) = fixture() else {
        eprintln!("skipping: reference-plugin.wasm fixture not provisioned");
        return;
    };
    let scope = scope([]);
    let mut plugin = runtime::create_plugin(&fixture, &scope, test_limits())
        .await
        .expect("instantiate reference plugin");
    let config = HashMap::from([("patterns".to_string(), "swordfish".to_string())]);
    let result = runtime::call_execute(&mut plugin, br#"{"text":"pass is swordfish"}"#, &config)
        .await
        .expect("execute redact tool");
    assert!(result.success);
    assert!(result.output.contains("swordfish"));
}

#[tokio::test]
async fn reference_plugin_masks_token_by_default() {
    let Some(fixture) = fixture() else {
        eprintln!("skipping: reference-plugin.wasm fixture not provisioned");
        return;
    };
    let scope = scope([PluginPermission::ConfigRead]);
    let mut plugin = runtime::create_plugin(&fixture, &scope, test_limits())
        .await
        .expect("instantiate reference plugin");
    let result = runtime::call_execute(
        &mut plugin,
        br#"{"text":"token sk-abcdef0123456789"}"#,
        &HashMap::new(),
    )
    .await
    .expect("execute redact tool");
    assert!(result.success);
    assert!(!result.output.contains("sk-abcdef0123456789"));
}

#[tokio::test]
async fn reference_plugin_traps_when_fuel_exhausted() {
    let Some(fixture) = fixture() else {
        eprintln!("skipping: reference-plugin.wasm fixture not provisioned");
        return;
    };
    let starved = PluginLimits {
        call_fuel: 1,
        max_memory_bytes: 256 * 1024 * 1024,
        max_table_elements: 100_000,
        max_instances: 64,
    };
    let scope = scope([]);
    let mut plugin = runtime::create_plugin(&fixture, &scope, starved)
        .await
        .expect("instantiate reference plugin");
    let result = runtime::call_execute(&mut plugin, br#"{"text":"hello"}"#, &HashMap::new()).await;
    assert!(
        result.is_err(),
        "a 1-unit fuel budget must trap the call, got {result:?}"
    );
}

#[tokio::test]
async fn reference_plugin_traps_when_memory_capped() {
    let Some(fixture) = fixture() else {
        eprintln!("skipping: reference-plugin.wasm fixture not provisioned");
        return;
    };
    let capped = PluginLimits {
        call_fuel: 1_000_000_000,
        max_memory_bytes: 1,
        max_table_elements: 100_000,
        max_instances: 64,
    };
    let scope = scope([]);
    let outcome = runtime::create_plugin(&fixture, &scope, capped).await;
    assert!(
        outcome.is_err(),
        "a 1-byte memory cap must reject instantiation, got ok"
    );
}
