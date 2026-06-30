//! End-to-end load of the canonical reference plugin component through the real
//! wasmtime host path. The fixture is built from the standalone reference-plugin
//! app against `wit/v0`; loading it here proves the host instantiates and calls a
//! real component, the config jail injects only the plugin's own section, and the
//! redaction policy is driven by that section.

#![cfg(feature = "plugins-wasm-cranelift")]

use std::collections::HashMap;
use std::path::PathBuf;

use zeroclaw_plugins::PluginPermission;
use zeroclaw_plugins::runtime;

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
    let mut plugin = runtime::create_plugin(&fixture, &[])
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
    let mut plugin = runtime::create_plugin(&fixture, &[PluginPermission::ConfigRead])
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
        &[PluginPermission::ConfigRead],
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
    let mut plugin = runtime::create_plugin(&fixture, &[])
        .await
        .expect("instantiate reference plugin");
    let config = HashMap::from([("patterns".to_string(), "swordfish".to_string())]);
    let result = runtime::call_execute(
        &mut plugin,
        br#"{"text":"pass is swordfish"}"#,
        &config,
        &[],
    )
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
    let mut plugin = runtime::create_plugin(&fixture, &[PluginPermission::ConfigRead])
        .await
        .expect("instantiate reference plugin");
    let result = runtime::call_execute(
        &mut plugin,
        br#"{"text":"token sk-abcdef0123456789"}"#,
        &HashMap::new(),
        &[PluginPermission::ConfigRead],
    )
    .await
    .expect("execute redact tool");
    assert!(result.success);
    assert!(!result.output.contains("sk-abcdef0123456789"));
}
