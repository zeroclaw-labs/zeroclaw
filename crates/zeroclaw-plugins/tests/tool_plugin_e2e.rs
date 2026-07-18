//! End-to-end fixture for the host's tool component and secret service.

#![cfg(feature = "plugins-wasm-cranelift")]

mod support;

use std::collections::HashMap;
use std::path::PathBuf;
use std::process::Command;
use std::sync::OnceLock;

use zeroclaw_plugins::component::PluginLimits;
use zeroclaw_plugins::config::{PluginConfigResolver, resolve_plugin_config};
use zeroclaw_plugins::instance::PluginInstanceScope;
use zeroclaw_plugins::runtime;
use zeroclaw_plugins::services::PluginHostServices;
use zeroclaw_plugins::{PluginCapability, PluginManifest, PluginPermission};

use support::{admit_fixture, state_service};

fn fixture() -> PathBuf {
    static FIXTURE: OnceLock<PathBuf> = OnceLock::new();
    FIXTURE
        .get_or_init(|| {
            let fixture_dir =
                PathBuf::from(env!("CARGO_MANIFEST_DIR")).join("tests/fixtures/tool-fixture");
            let target_dir = PathBuf::from(env!("CARGO_TARGET_TMPDIR")).join("tool-plugin-fixture");
            let status = Command::new(env!("CARGO"))
                .current_dir(&fixture_dir)
                .args([
                    "build",
                    "--locked",
                    "--quiet",
                    "--package",
                    "zeroclaw-tool-plugin-fixture",
                    "--target",
                    "wasm32-wasip2",
                    "--target-dir",
                ])
                .arg(&target_dir)
                .status()
                .expect("run Cargo for the tool component fixture");
            assert!(
                status.success(),
                "tool fixture must build; install the wasm32-wasip2 target"
            );

            let wasm = target_dir.join("wasm32-wasip2/debug/zeroclaw_tool_plugin_fixture.wasm");
            assert!(wasm.is_file(), "tool fixture WASM was not produced");
            wasm
        })
        .clone()
}

fn limits() -> PluginLimits {
    PluginLimits {
        call_fuel: 1_000_000_000,
        max_memory_bytes: 64 * 1024 * 1024,
        max_table_elements: 10_000,
        max_instances: 32,
    }
}

async fn execute(binding: &str, grant_state: bool) -> String {
    let manifest = PluginManifest {
        name: "tool-fixture".to_string(),
        version: "0.0.0".to_string(),
        description: None,
        author: None,
        wasm_path: Some("tool-fixture.wasm".to_string()),
        wasm_sha256: None,
        capabilities: vec![PluginCapability::Tool],
        permissions: vec![
            PluginPermission::ConfigRead,
            PluginPermission::StateRead,
            PluginPermission::StateWrite,
        ],
        config_schema: Some(serde_json::json!({
            "$schema": "https://json-schema.org/draft/2020-12/schema",
            "type": "object",
            "required": ["binding_label", "api_token"],
            "additionalProperties": false,
            "properties": {
                "binding_label": {"type": "string"},
                "api_token": {"type": "string", "x-secret": true}
            }
        })),
        signature: None,
        publisher_key: None,
    };
    let mut grants = vec![PluginPermission::ConfigRead];
    if grant_state {
        grants.extend([PluginPermission::StateRead, PluginPermission::StateWrite]);
    }
    let scope =
        PluginInstanceScope::from_manifest(&manifest, PluginCapability::Tool, binding, grants)
            .expect("admit fixture scope");
    let component = admit_fixture(&fixture(), &manifest);
    let configured = HashMap::from([
        ("binding_label".to_string(), binding.to_string()),
        ("api_token".to_string(), format!("token-{binding}")),
    ]);
    let resolver = PluginConfigResolver::new(move |scope| {
        resolve_plugin_config(&manifest, scope, Some(&configured))
    });
    let services = PluginHostServices::new(resolver, state_service(), support::egress_service());
    let mut plugin = runtime::create_plugin(&component, &scope, &services, limits())
        .await
        .expect("instantiate fixture tool");

    let metadata = runtime::call_tool_metadata(&mut plugin)
        .await
        .expect("read fixture metadata");
    assert_eq!(metadata.name, "scoped-secret-check");
    let result = runtime::call_execute(
        &mut plugin,
        br#"{"__config":{"binding_label":"forged","api_token":"forged"}}"#,
    )
    .await
    .expect("execute fixture tool");
    assert!(result.success);
    if grant_state {
        runtime::call_execute(&mut plugin, br#"{}"#)
            .await
            .expect("second execution reuses durable state with CAS");
    }
    result.output.to_string()
}

#[tokio::test]
async fn tool_world_reads_only_schema_designated_secrets() {
    let (main, backup) = tokio::join!(execute("main", true), execute("backup", true));

    assert_eq!(main, "main");
    assert_eq!(backup, "backup");
}

#[tokio::test]
async fn tool_world_denies_state_without_effective_grants() {
    assert_eq!(execute("state-denied", false).await, "state-denied");
}
