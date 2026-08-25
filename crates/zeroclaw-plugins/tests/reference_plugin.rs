//! End-to-end load of a tool component through the real wasmtime host path.
//!
//! The component is the in-tree `tests/fixtures/tool-fixture` crate, a workspace
//! member built on demand into a separate target directory so the nested Cargo
//! invocation cannot contend with the host test process's build lock. Loading it
//! here proves the host instantiates and calls a real component, that the config
//! jail injects only the plugin's own section, and — the part no unit test can
//! reach — that the operator's canonical *string* values arrive inside a live
//! guest as the *typed* JSON its `config_schema` declares.
//!
//! There is no skip path: a fixture that cannot be built is a test failure, so
//! "the tool-plugin path works" is decided by CI rather than by whether a human
//! provisioned an artifact.

#![cfg(feature = "plugins-wasm-cranelift")]

use std::collections::HashMap;
use std::path::PathBuf;
use std::process::Command;
use std::sync::OnceLock;

use zeroclaw_plugins::component::PluginLimits;
use zeroclaw_plugins::config::{PluginConfigResolver, resolve_plugin_config};
use zeroclaw_plugins::error::PluginError;
use zeroclaw_plugins::instance::PluginInstanceScope;
use zeroclaw_plugins::runtime;
use zeroclaw_plugins::services::PluginHostServices;
use zeroclaw_plugins::{PluginCapability, PluginManifest, PluginPermission};

/// Build the in-tree tool fixture once per test binary and return its component.
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

fn test_limits() -> PluginLimits {
    PluginLimits {
        call_fuel: 1_000_000_000,
        max_memory_bytes: 256 * 1024 * 1024,
        max_table_elements: 100_000,
        max_instances: 64,
        call_timeout: std::time::Duration::from_secs(30),
    }
}

/// The fixture's manifest and one admitted scope over it.
///
/// `permissions` always requests `config_read` because the manifest declares a
/// `config_schema`, and the two are a strict biconditional at admission. What
/// varies per test is `grants` — the host's effective decision — which is the
/// axis the config jail actually keys on.
///
/// The schema declares no `required` keys: a withheld grant resolves to `{}`,
/// and `{}` must still satisfy the schema or resolution fails closed.
fn context(
    grants: impl IntoIterator<Item = PluginPermission>,
) -> (PluginManifest, PluginInstanceScope) {
    let manifest = PluginManifest {
        name: "tool-fixture".to_string(),
        version: "0.0.0".to_string(),
        description: None,
        author: None,
        wasm_path: Some("tool-fixture.wasm".to_string()),
        capabilities: vec![PluginCapability::Tool],
        permissions: vec![PluginPermission::ConfigRead],
        config_schema: Some(serde_json::json!({
            "$schema": "https://json-schema.org/draft/2020-12/schema",
            "type": "object",
            "additionalProperties": false,
            "properties": {
                "label": {"type": "string"},
                "max_len": {"type": "integer"},
                "uppercase": {"type": "boolean"}
            }
        })),
        signature: None,
        publisher_key: None,
    };
    let scope =
        PluginInstanceScope::from_manifest(&manifest, PluginCapability::Tool, "main", grants)
            .expect("fixture manifest admits its effective grants");
    (manifest, scope)
}

/// The operator's canonical section: a string map, exactly as
/// `[plugins.entries.config]` stores it. `uppercase` and `max_len` are the
/// JSON encodings of a boolean and an integer, not Rust-flavoured text.
fn operator_section() -> HashMap<String, String> {
    HashMap::from([
        ("label".to_string(), "masked".to_string()),
        ("uppercase".to_string(), "true".to_string()),
        ("max_len".to_string(), "5".to_string()),
    ])
}

fn host_services(
    manifest: PluginManifest,
    configured: Option<HashMap<String, String>>,
) -> PluginHostServices {
    PluginHostServices::new(PluginConfigResolver::new(move |scope| {
        resolve_plugin_config(&manifest, scope, configured.as_ref())
    }))
}

#[tokio::test]
async fn reference_plugin_reports_metadata() {
    let (manifest, scope) = context([]);
    let services = host_services(manifest, None);
    let mut plugin = runtime::create_plugin(&fixture(), &scope, &services, test_limits())
        .await
        .expect("instantiate tool fixture");
    let meta = runtime::call_tool_metadata(&mut plugin)
        .await
        .expect("read tool metadata");

    assert_eq!(meta.name, "config-echo");
    assert_eq!(
        meta.description,
        "Echo the caller's text after applying the plugin's typed config."
    );
    assert_eq!(
        meta.parameters_schema["$schema"],
        "https://json-schema.org/draft/2020-12/schema"
    );
    assert!(meta.parameters_schema["properties"]["text"].is_object());
    assert_eq!(
        meta.parameters_schema["properties"]["text"]["type"],
        "string"
    );
}

#[tokio::test]
async fn reference_plugin_materializes_typed_config_with_grant() {
    let (manifest, scope) = context([PluginPermission::ConfigRead]);
    let services = host_services(manifest, Some(operator_section()));
    let mut plugin = runtime::create_plugin(&fixture(), &scope, &services, test_limits())
        .await
        .expect("instantiate tool fixture");

    let result = runtime::call_execute(&mut plugin, br#"{"text":"hello world"}"#)
        .await
        .expect("execute config-echo tool");

    assert!(result.success);
    assert_eq!(result.error, None);
    // Exact bytes. The guest deserializes `uppercase` into a `bool` and
    // `max_len` into a `u32` with no string parsing, so this line can only be
    // produced if the host materialized `"true"` and `"5"` into typed JSON
    // before injection: the boolean drove the casing and the integer drove a
    // 5-character truncation.
    assert_eq!(
        result.output.as_str(),
        "label=masked|uppercase=true|max_len=5|keys=3|text=HELLO"
    );
}

#[tokio::test]
async fn reference_plugin_jails_config_without_grant() {
    let (manifest, scope) = context([]);
    let services = host_services(manifest, Some(operator_section()));
    let mut plugin = runtime::create_plugin(&fixture(), &scope, &services, test_limits())
        .await
        .expect("instantiate tool fixture");

    let result = runtime::call_execute(&mut plugin, br#"{"text":"hello world"}"#)
        .await
        .expect("execute config-echo tool");

    assert!(result.success);
    assert_eq!(
        result.output.as_str(),
        "label=unset|uppercase=false|max_len=0|keys=0|text=hello world",
        "a plugin without an effective config_read grant must observe no keys at all"
    );
}

#[tokio::test]
async fn reference_plugin_strips_caller_forged_config_section() {
    let (manifest, scope) = context([]);
    let services = host_services(manifest, None);
    let mut plugin = runtime::create_plugin(&fixture(), &scope, &services, test_limits())
        .await
        .expect("instantiate tool fixture");

    let result = runtime::call_execute(
        &mut plugin,
        br#"{"text":"hi","__config":{"label":"forged","max_len":99}}"#,
    )
    .await
    .expect("execute config-echo tool");

    assert!(result.success);
    assert_eq!(
        result.output.as_str(),
        "label=unset|uppercase=false|max_len=0|keys=0|text=hi",
        "caller-supplied __config must never reach the guest"
    );
}

#[tokio::test]
async fn reference_plugin_applies_defaults_without_config() {
    let (manifest, scope) = context([PluginPermission::ConfigRead]);
    let services = host_services(manifest, None);
    let mut plugin = runtime::create_plugin(&fixture(), &scope, &services, test_limits())
        .await
        .expect("instantiate tool fixture");

    let result = runtime::call_execute(&mut plugin, br#"{"text":"hello world"}"#)
        .await
        .expect("execute config-echo tool");

    assert!(result.success);
    assert_eq!(
        result.output.as_str(),
        "label=unset|uppercase=false|max_len=0|keys=0|text=hello world"
    );
}

#[tokio::test]
async fn reference_plugin_host_rejects_ill_typed_operator_value() {
    let (manifest, scope) = context([PluginPermission::ConfigRead]);
    // Instantiating first proves the rejection is not an artifact of a plugin
    // that never loaded: the component is live and simply never gets called.
    let services = host_services(manifest.clone(), Some(operator_section()));
    let mut plugin = runtime::create_plugin(&fixture(), &scope, &services, test_limits())
        .await
        .expect("instantiate tool fixture");

    for (section, expected) in [
        (
            HashMap::from([("max_len".to_string(), "not-a-number".to_string())]),
            "config property 'max_len' must contain valid JSON",
        ),
        (
            HashMap::from([("max_len".to_string(), "5.5".to_string())]),
            "config property 'max_len' must be a JSON integer",
        ),
        (
            HashMap::from([("uppercase".to_string(), "yes".to_string())]),
            "config property 'uppercase' must contain valid JSON",
        ),
        (
            HashMap::from([("nope".to_string(), "anything".to_string())]),
            "config contains a property absent from config_schema",
        ),
    ] {
        let error = resolve_plugin_config(&manifest, &scope, Some(&section))
            .err()
            .expect("an operator value that does not match its declared type must fail closed");
        assert!(
            matches!(error, PluginError::InvalidConfig(_)),
            "expected InvalidConfig, got {error:?}"
        );
        assert!(
            error.to_string().contains(expected),
            "expected {expected:?} in {error}"
        );
    }

    // The guest is still callable with a valid section, so the rejections above
    // were the config path failing, not the component. The store's own service
    // resolves that well-typed section lazily on this call.
    let result = runtime::call_execute(&mut plugin, br#"{"text":"hello world"}"#)
        .await
        .expect("execute config-echo tool");
    assert_eq!(
        result.output.as_str(),
        "label=masked|uppercase=true|max_len=5|keys=3|text=HELLO"
    );
}

#[tokio::test]
async fn reference_plugin_rejects_work_past_fuel_budget() {
    let starved = PluginLimits {
        call_fuel: 1,
        max_memory_bytes: 256 * 1024 * 1024,
        max_table_elements: 100_000,
        max_instances: 64,
        call_timeout: std::time::Duration::from_secs(30),
    };
    let (manifest, scope) = context([]);
    let services = host_services(manifest, None);
    match runtime::create_plugin(&fixture(), &scope, &services, starved).await {
        Ok(mut plugin) => {
            let result = runtime::call_execute(&mut plugin, br#"{"text":"hello"}"#).await;
            assert!(
                result.is_err(),
                "a 1-unit fuel budget must trap execution, got {result:?}"
            );
        }
        Err(error) => assert!(
            error.to_string().contains("fuel"),
            "an early failure must be fuel exhaustion, got {error:#}"
        ),
    }
}

#[tokio::test]
async fn reference_plugin_traps_when_memory_capped() {
    let capped = PluginLimits {
        call_fuel: 1_000_000_000,
        max_memory_bytes: 1,
        max_table_elements: 100_000,
        max_instances: 64,
        call_timeout: std::time::Duration::from_secs(30),
    };
    let (manifest, scope) = context([]);
    let services = host_services(manifest, None);
    let outcome = runtime::create_plugin(&fixture(), &scope, &services, capped).await;
    assert!(
        outcome.is_err(),
        "a 1-byte memory cap must reject instantiation, got ok"
    );
}
