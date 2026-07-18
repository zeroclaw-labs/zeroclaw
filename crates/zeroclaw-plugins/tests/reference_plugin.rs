//! End-to-end load of the canonical reference plugin component through the real
//! wasmtime host path. The fixture is built from the standalone reference-plugin
//! app against `wit/v0`; loading it here proves the host instantiates and calls a
//! real component, the config jail injects only the plugin's own section, and the

#![cfg(feature = "plugins-wasm-cranelift")]

mod support;

use std::collections::HashMap;
use std::path::PathBuf;

use zeroclaw_plugins::component::PluginLimits;
use zeroclaw_plugins::config::{PluginConfigResolver, resolve_plugin_config};
use zeroclaw_plugins::instance::PluginInstanceScope;
use zeroclaw_plugins::runtime;
use zeroclaw_plugins::services::PluginHostServices;
use zeroclaw_plugins::{PluginCapability, PluginManifest, PluginPermission};

use support::{admit_fixture, state_service};

fn test_limits() -> PluginLimits {
    PluginLimits {
        call_fuel: 1_000_000_000,
        max_memory_bytes: 256 * 1024 * 1024,
        max_table_elements: 100_000,
        max_instances: 64,
    }
}

fn context(
    grants: impl IntoIterator<Item = PluginPermission>,
) -> (PluginManifest, PluginInstanceScope) {
    let manifest = PluginManifest {
        name: "zeroclaw-reference-plugin".to_string(),
        version: "0.1.0".to_string(),
        description: None,
        author: None,
        wasm_path: Some("reference-plugin.wasm".to_string()),
        wasm_sha256: None,
        capabilities: vec![PluginCapability::Tool],
        permissions: vec![PluginPermission::ConfigRead],
        config_schema: Some(serde_json::json!({
            "$schema": "https://json-schema.org/draft/2020-12/schema",
            "type": "object",
            "additionalProperties": false,
            "properties": {
                "replacement": {"type": "string"},
                "patterns": {"type": "string"},
                "redact_emails": {"type": "string"}
            }
        })),
        signature: None,
        publisher_key: None,
    };
    let scope =
        PluginInstanceScope::from_manifest(&manifest, PluginCapability::Tool, "main", grants)
            .expect("reference manifest admits its effective grants");
    (manifest, scope)
}

fn fixture() -> Option<PathBuf> {
    let path =
        PathBuf::from(env!("CARGO_MANIFEST_DIR")).join("tests/fixtures/reference-plugin.wasm");
    path.exists().then_some(path)
}

fn host_services(
    manifest: PluginManifest,
    configured: Option<HashMap<String, String>>,
) -> PluginHostServices {
    PluginHostServices::new(
        PluginConfigResolver::new(move |scope| {
            resolve_plugin_config(&manifest, scope, configured.as_ref())
        }),
        state_service(),
        support::egress_service(),
    )
}

#[tokio::test]
async fn reference_plugin_reports_metadata() {
    let Some(fixture_path) = fixture() else {
        eprintln!("skipping: reference-plugin.wasm fixture not provisioned");
        return;
    };
    let (manifest, scope) = context([]);
    let fixture = admit_fixture(&fixture_path, &manifest);
    let services = host_services(manifest, None);
    let mut plugin = runtime::create_plugin(&fixture, &scope, &services, test_limits())
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
    let Some(fixture_path) = fixture() else {
        eprintln!("skipping: reference-plugin.wasm fixture not provisioned");
        return;
    };
    let (manifest, scope) = context([PluginPermission::ConfigRead]);
    let fixture = admit_fixture(&fixture_path, &manifest);
    let config = HashMap::from([
        ("replacement".to_string(), "<X>".to_string()),
        ("patterns".to_string(), "swordfish".to_string()),
    ]);
    let services = host_services(manifest, Some(config));
    let mut plugin = runtime::create_plugin(&fixture, &scope, &services, test_limits())
        .await
        .expect("instantiate reference plugin");
    let result = runtime::call_execute(
        &mut plugin,
        br#"{"text":"ping me at a@b.com, pass is swordfish"}"#,
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
    let Some(fixture_path) = fixture() else {
        eprintln!("skipping: reference-plugin.wasm fixture not provisioned");
        return;
    };
    let (manifest, scope) = context([]);
    let fixture = admit_fixture(&fixture_path, &manifest);
    let config = HashMap::from([("patterns".to_string(), "swordfish".to_string())]);
    let services = host_services(manifest, Some(config));
    let mut plugin = runtime::create_plugin(&fixture, &scope, &services, test_limits())
        .await
        .expect("instantiate reference plugin");
    let result = runtime::call_execute(&mut plugin, br#"{"text":"pass is swordfish"}"#)
        .await
        .expect("execute redact tool");
    assert!(result.success);
    assert!(result.output.contains("swordfish"));
}

#[tokio::test]
async fn reference_plugin_masks_token_by_default() {
    let Some(fixture_path) = fixture() else {
        eprintln!("skipping: reference-plugin.wasm fixture not provisioned");
        return;
    };
    let (manifest, scope) = context([PluginPermission::ConfigRead]);
    let fixture = admit_fixture(&fixture_path, &manifest);
    let services = host_services(manifest, None);
    let mut plugin = runtime::create_plugin(&fixture, &scope, &services, test_limits())
        .await
        .expect("instantiate reference plugin");
    let result = runtime::call_execute(&mut plugin, br#"{"text":"token sk-abcdef0123456789"}"#)
        .await
        .expect("execute redact tool");
    assert!(result.success);
    assert!(!result.output.contains("sk-abcdef0123456789"));
}

#[tokio::test]
async fn reference_plugin_traps_when_fuel_exhausted() {
    let Some(fixture_path) = fixture() else {
        eprintln!("skipping: reference-plugin.wasm fixture not provisioned");
        return;
    };
    let starved = PluginLimits {
        call_fuel: 1,
        max_memory_bytes: 256 * 1024 * 1024,
        max_table_elements: 100_000,
        max_instances: 64,
    };
    let (manifest, scope) = context([]);
    let fixture = admit_fixture(&fixture_path, &manifest);
    let services = host_services(manifest, None);
    let mut plugin = runtime::create_plugin(&fixture, &scope, &services, starved)
        .await
        .expect("instantiate reference plugin");
    let result = runtime::call_execute(&mut plugin, br#"{"text":"hello"}"#).await;
    assert!(
        result.is_err(),
        "a 1-unit fuel budget must trap the call, got {result:?}"
    );
}

#[tokio::test]
async fn reference_plugin_traps_when_memory_capped() {
    let Some(fixture_path) = fixture() else {
        eprintln!("skipping: reference-plugin.wasm fixture not provisioned");
        return;
    };
    let capped = PluginLimits {
        call_fuel: 1_000_000_000,
        max_memory_bytes: 1,
        max_table_elements: 100_000,
        max_instances: 64,
    };
    let (manifest, scope) = context([]);
    let fixture = admit_fixture(&fixture_path, &manifest);
    let services = host_services(manifest, None);
    let outcome = runtime::create_plugin(&fixture, &scope, &services, capped).await;
    assert!(
        outcome.is_err(),
        "a 1-byte memory cap must reject instantiation, got ok"
    );
}
