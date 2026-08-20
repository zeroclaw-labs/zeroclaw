//! End-to-end: drive an in-tree tool component exactly as the daemon does,
//! against a throwaway config dir. The test seeds a disposable
//! `ZEROCLAW_CONFIG_DIR` with the README's install layout, loads the real
//! `Config`, discovers the plugin through the real `PluginHost`, keys the
//! plugin's own config section by the real instance key, materializes it
//! against the manifest's `config_schema`, and executes the component through
//! wasmtime.
//!
//! The component is the in-tree `tests/fixtures/tool-fixture` crate, built on
//! demand into a separate target directory so the nested Cargo invocation cannot
//! contend with the host test process's build lock. There is no skip path: a
//! fixture that cannot be built is a test failure.

#![cfg(feature = "plugins-wasm-cranelift")]

use std::fs;
use std::path::PathBuf;
use std::process::Command;
use std::sync::OnceLock;

use tokio::sync::Mutex;
use zeroclaw_config::schema::Config;
use zeroclaw_plugins::component::PluginLimits;
use zeroclaw_plugins::config::resolve_plugin_config;
use zeroclaw_plugins::host::PluginHost;
use zeroclaw_plugins::instance::PluginInstanceScope;
use zeroclaw_plugins::runtime;
use zeroclaw_plugins::{PluginCapability, PluginManifest, PluginPermission};

static ENV_LOCK: Mutex<()> = Mutex::const_new(());

/// The fixture package's manifest: the single source of truth for both the
/// seeded `manifest.toml` and the instance key its config entry is stored under.
const FIXTURE_MANIFEST: &str = r#"name = "tool-fixture"
version = "0.0.0"
wasm_path = "tool-fixture.wasm"
capabilities = ["tool"]
permissions = ["config_read"]

[config_schema]
"$schema" = "https://json-schema.org/draft/2020-12/schema"
type = "object"
additionalProperties = false

[config_schema.properties.label]
type = "string"

[config_schema.properties.max_len]
type = "integer"

[config_schema.properties.uppercase]
type = "boolean"
"#;

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

/// Lay out a disposable config dir the way `zeroclaw plugin install` does.
///
/// Three packages are seeded. Only the first is admissible; the other two exist
/// to prove the discovery gates reject them:
///
/// * `tool-fixture` — declares `config_read` and a matching closed schema.
/// * `broken-tool` — declares the `tool` capability with no `wasm_path`
///   (manifest-shape rejection).
/// * `schemaless-tool` — requests `config_read` with no `config_schema`
///   (config-contract rejection; the biconditional, through real discovery).
fn seed_config_dir(dir: &std::path::Path) {
    let plugins_root = dir.join("plugins");

    let plugin_dir = plugins_root.join("tool-fixture");
    fs::create_dir_all(&plugin_dir).unwrap();
    fs::copy(fixture(), plugin_dir.join("tool-fixture.wasm")).unwrap();
    fs::write(plugin_dir.join("manifest.toml"), FIXTURE_MANIFEST).unwrap();

    let broken_dir = plugins_root.join("broken-tool");
    fs::create_dir_all(&broken_dir).unwrap();
    fs::write(
        broken_dir.join("manifest.toml"),
        "name = \"broken-tool\"\n\
         version = \"0.0.0\"\n\
         capabilities = [\"tool\"]\n",
    )
    .unwrap();

    let schemaless_dir = plugins_root.join("schemaless-tool");
    fs::create_dir_all(&schemaless_dir).unwrap();
    fs::write(schemaless_dir.join("schemaless-tool.wasm"), b"").unwrap();
    fs::write(
        schemaless_dir.join("manifest.toml"),
        "name = \"schemaless-tool\"\n\
         version = \"0.0.0\"\n\
         wasm_path = \"schemaless-tool.wasm\"\n\
         capabilities = [\"tool\"]\n\
         permissions = [\"config_read\"]\n",
    )
    .unwrap();

    // The operator's canonical section is a string map keyed by the full
    // instance key, and its values are the JSON encodings the schema's types
    // demand: a bare string for `label`, `true` for a boolean, `5` for an
    // integer.
    let manifest: PluginManifest = toml::from_str(FIXTURE_MANIFEST).unwrap();
    let scope = PluginInstanceScope::for_package_binding(
        &manifest,
        PluginCapability::Tool,
        manifest.permissions.iter().copied(),
    )
    .unwrap();
    let instance_key = scope.id().config_entry_key().unwrap();

    fs::write(
        dir.join("config.toml"),
        format!(
            "schema_version = 3\n\n\
             [plugins]\n\
             enabled = true\n\
             auto_discover = true\n\
             plugins_dir = \"{}\"\n\n\
             [[plugins.entries]]\n\
             name = \"{}\"\n\n\
             [plugins.entries.config]\n\
             label = \"masked\"\n\
             uppercase = \"true\"\n\
             max_len = \"5\"\n",
            plugins_root.display(),
            instance_key
        ),
    )
    .unwrap();
}

#[tokio::test]
async fn reference_plugin_end_to_end_from_throwaway_config() {
    let _guard = ENV_LOCK.lock().await;
    let tmp = tempfile::tempdir().unwrap();
    seed_config_dir(tmp.path());

    // SAFETY: serialized by ENV_LOCK; restored before the lock is released.
    let prev = std::env::var("ZEROCLAW_CONFIG_DIR").ok();
    unsafe { std::env::set_var("ZEROCLAW_CONFIG_DIR", tmp.path()) };

    let config = Config::load_or_init().await.expect("load throwaway config");

    assert!(config.plugins.enabled, "plugin system enabled from config");
    let plugins_dir = config.plugins.resolved_plugins_dir();
    assert_eq!(plugins_dir, tmp.path().join("plugins"));

    let host = PluginHost::from_plugins_dir(&plugins_dir).expect("scan throwaway plugins dir");
    let details = host.tool_plugin_details();
    assert_eq!(
        details.len(),
        1,
        "only the schema-bearing fixture is admitted: the wasm_path-less \
         neighbour fails manifest-shape validation and the config_read-without-\
         config_schema neighbour fails the config contract"
    );
    let (manifest, wasm_path) = details[0];
    assert_eq!(manifest.name, "tool-fixture");
    assert!(manifest.capabilities.contains(&PluginCapability::Tool));
    assert!(manifest.permissions.contains(&PluginPermission::ConfigRead));
    assert!(
        manifest.config_schema.is_some(),
        "the admitted manifest carries the schema that types its config"
    );

    let scope = PluginInstanceScope::for_package_binding(
        manifest,
        PluginCapability::Tool,
        manifest.permissions.iter().copied(),
    )
    .expect("discovered manifest admits its requested tool grants");
    let section = config
        .plugins
        .entry_config(&scope.id().config_entry_key().unwrap())
        .expect("plugin config entry must be unique")
        .expect("plugin config section resolved")
        .clone();
    // Canonical storage is still a string map; typing happens at resolution.
    assert_eq!(section.get("label").map(String::as_str), Some("masked"));
    assert_eq!(section.get("uppercase").map(String::as_str), Some("true"));
    assert_eq!(section.get("max_len").map(String::as_str), Some("5"));

    let mut plugin = runtime::create_plugin(
        wasm_path,
        &scope,
        PluginLimits {
            call_fuel: 1_000_000_000,
            max_memory_bytes: 256 * 1024 * 1024,
            max_table_elements: 100_000,
            max_instances: 64,
        },
    )
    .await
    .expect("instantiate discovered plugin");

    let meta = runtime::call_tool_metadata(&mut plugin)
        .await
        .expect("read metadata");

    let resolved = resolve_plugin_config(manifest, &scope, Some(&section))
        .expect("materialize typed plugin config");
    let result = runtime::call_execute(&mut plugin, br#"{"text":"hello world"}"#, &resolved).await;

    // SAFETY: serialized by ENV_LOCK.
    match prev {
        Some(v) => unsafe { std::env::set_var("ZEROCLAW_CONFIG_DIR", v) },
        None => unsafe { std::env::remove_var("ZEROCLAW_CONFIG_DIR") },
    }

    assert_eq!(meta.name, "config-echo");

    let result = result.expect("execute discovered tool");
    assert!(result.success);
    // Exact bytes, from a `config.toml` on disk all the way into a live guest:
    // every value reached the plugin as the type its manifest schema declares —
    // string echoed, boolean applied, integer used as a length — even though
    // the operator only ever wrote strings.
    assert_eq!(
        result.output.as_str(),
        "label=masked|uppercase=true|max_len=5|keys=3|text=HELLO"
    );
}
