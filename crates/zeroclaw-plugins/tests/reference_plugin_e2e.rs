//! End-to-end: drive the published reference plugin exactly as the daemon does,
//! against a throwaway config dir. The test seeds a disposable `ZEROCLAW_CONFIG_DIR`
//! with the README's install layout, loads the real `Config`, discovers the plugin
//! through the real `PluginHost`, resolves the plugin's own config section through

#![cfg(feature = "plugins-wasm-cranelift")]

#[path = "support/egress.rs"]
mod egress_support;
#[path = "support/state.rs"]
mod state_support;

use std::fs;
use std::path::PathBuf;

use tokio::sync::Mutex;
use zeroclaw_config::schema::Config;
use zeroclaw_plugins::component::PluginLimits;
use zeroclaw_plugins::config::{PluginConfigResolver, resolve_plugin_config};
use zeroclaw_plugins::host::PluginHost;
use zeroclaw_plugins::instance::PluginInstanceScope;
use zeroclaw_plugins::runtime;
use zeroclaw_plugins::services::PluginHostServices;
use zeroclaw_plugins::{PluginCapability, PluginManifest, PluginPermission};

use state_support::state_service;

static ENV_LOCK: Mutex<()> = Mutex::const_new(());

fn fixture() -> Option<PathBuf> {
    let path =
        PathBuf::from(env!("CARGO_MANIFEST_DIR")).join("tests/fixtures/reference-plugin.wasm");
    path.exists().then_some(path)
}

fn seed_config_dir(dir: &std::path::Path) -> bool {
    let Some(fixture) = fixture() else {
        return false;
    };
    let plugin_dir = dir.join("plugins").join("zeroclaw-reference-plugin");
    fs::create_dir_all(&plugin_dir).unwrap();
    fs::copy(&fixture, plugin_dir.join("reference-plugin.wasm")).unwrap();
    let manifest_toml = "name = \"zeroclaw-reference-plugin\"\n\
         version = \"0.1.0\"\n\
         wasm_path = \"reference-plugin.wasm\"\n\
         capabilities = [\"tool\"]\n\
         permissions = [\"config_read\"]\n\n\
         [config_schema]\n\
         \"$schema\" = \"https://json-schema.org/draft/2020-12/schema\"\n\
         type = \"object\"\n\
         additionalProperties = false\n\n\
         [config_schema.properties.replacement]\n\
         type = \"string\"\n\n\
         [config_schema.properties.redact_emails]\n\
         type = \"string\"\n\n\
         [config_schema.properties.patterns]\n\
         type = \"string\"\n";
    fs::write(plugin_dir.join("manifest.toml"), manifest_toml).unwrap();
    let manifest: PluginManifest = toml::from_str(manifest_toml).unwrap();
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
             replacement = \"<MASK>\"\n\
             redact_emails = \"true\"\n\
             patterns = \"project-zeus\"\n",
            dir.join("plugins").display(),
            instance_key
        ),
    )
    .unwrap();
    true
}

#[tokio::test]
async fn reference_plugin_end_to_end_from_throwaway_config() {
    let _guard = ENV_LOCK.lock().await;
    let tmp = tempfile::tempdir().unwrap();
    if !seed_config_dir(tmp.path()) {
        eprintln!("skipping: reference-plugin.wasm fixture not provisioned");
        return;
    }

    // SAFETY: serialized by ENV_LOCK; restored before the lock is released.
    let prev = std::env::var("ZEROCLAW_CONFIG_DIR").ok();
    unsafe { std::env::set_var("ZEROCLAW_CONFIG_DIR", tmp.path()) };

    let config = Config::load_or_init().await.expect("load throwaway config");

    assert!(config.plugins.enabled, "plugin system enabled from config");
    let plugins_dir = config.plugins.resolved_plugins_dir();
    assert_eq!(plugins_dir, tmp.path().join("plugins"));

    let host = PluginHost::from_plugins_dir(&plugins_dir).expect("scan throwaway plugins dir");
    let details = host.tool_plugin_details();
    assert_eq!(details.len(), 1, "exactly the reference tool discovered");
    let (manifest, component) = details[0];
    assert_eq!(manifest.name, "zeroclaw-reference-plugin");
    assert!(manifest.capabilities.contains(&PluginCapability::Tool));
    assert!(manifest.permissions.contains(&PluginPermission::ConfigRead));

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
    assert_eq!(
        section.get("replacement").map(String::as_str),
        Some("<MASK>")
    );
    let resolver_manifest = manifest.clone();
    let resolver_section = section.clone();
    let services = PluginHostServices::new(
        PluginConfigResolver::new(move |scope| {
            resolve_plugin_config(&resolver_manifest, scope, Some(&resolver_section))
        }),
        state_service(),
        egress_support::egress_service(),
    );
    let mut plugin = runtime::create_plugin(
        component,
        &scope,
        &services,
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
    assert_eq!(meta.name, "redact");

    let result = runtime::call_execute(
        &mut plugin,
        br#"{"text":"mail bob@corp.com about project-zeus, key sk-abcdef0123456789"}"#,
    )
    .await
    .expect("execute discovered tool");

    // SAFETY: serialized by ENV_LOCK.
    match prev {
        Some(v) => unsafe { std::env::set_var("ZEROCLAW_CONFIG_DIR", v) },
        None => unsafe { std::env::remove_var("ZEROCLAW_CONFIG_DIR") },
    }

    assert!(result.success);
    assert!(!result.output.contains("bob@corp.com"), "email masked");
    assert!(
        !result.output.contains("project-zeus"),
        "configured pattern masked"
    );
    assert!(
        !result.output.contains("sk-abcdef0123456789"),
        "token masked"
    );
    assert!(
        result.output.contains("<MASK>"),
        "config-driven replacement applied"
    );
}
