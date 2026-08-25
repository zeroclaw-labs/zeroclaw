//! Cross-crate proof that a configured channel plugin reaches a real WASM
//! component with its exact host-owned logical alias.
//!
//! The per-crate tests cover admission planning and the channel adapter
//! separately. This one runs the whole activation path an operator actually
//! exercises: a `[channels.plugin.<alias>]` declaration plus an installed
//! package, in, and a live `Channel` backed by a compiled component, out.

#![cfg(feature = "plugins-wasm-cranelift")]

use std::collections::HashMap;
use std::path::PathBuf;
use std::process::Command;
use std::sync::{Arc, OnceLock};
use std::time::Duration;

use tempfile::TempDir;
use zeroclaw_api::channel::SendMessage;
use zeroclaw_config::providers::{ChannelRef, ModelProviderRef};
use zeroclaw_config::schema::{
    AliasedAgentConfig, AnthropicModelProviderConfig, Config, PluginChannelConfig,
    PluginEntryConfig, RiskProfileConfig,
};
use zeroclaw_plugins::PluginCapability;
use zeroclaw_plugins::host::PluginHost;
use zeroclaw_plugins::instance::PluginInstanceScope;

const MANIFEST: &str =
    "crates/zeroclaw-plugins/tests/fixtures/channel-fixture/plugin-manifest.toml";

/// Build the channel component once per test binary.
///
/// The fixture is a workspace member built into its own target directory so the
/// nested Cargo invocation cannot contend with this test process's build lock.
fn fixture() -> PathBuf {
    static FIXTURE: OnceLock<PathBuf> = OnceLock::new();
    FIXTURE
        .get_or_init(|| {
            let fixture_dir = PathBuf::from(env!("CARGO_MANIFEST_DIR"))
                .join("crates/zeroclaw-plugins/tests/fixtures/channel-fixture");
            let target_dir =
                PathBuf::from(env!("CARGO_TARGET_TMPDIR")).join("plugin-channel-runtime-fixture");
            let status = Command::new(env!("CARGO"))
                .current_dir(&fixture_dir)
                .args([
                    "build",
                    "--locked",
                    "--quiet",
                    "--package",
                    "zeroclaw-channel-plugin-fixture",
                    "--target",
                    "wasm32-wasip2",
                    "--target-dir",
                ])
                .arg(&target_dir)
                .status()
                .expect("run Cargo for the channel component fixture");
            assert!(
                status.success(),
                "channel fixture must build; install the wasm32-wasip2 target"
            );

            let wasm = target_dir.join("wasm32-wasip2/debug/zeroclaw_channel_plugin_fixture.wasm");
            assert!(wasm.is_file(), "channel fixture WASM was not produced");
            wasm
        })
        .clone()
}

/// Install the fixture as a real plugin package: the canonical manifest copied
/// verbatim, next to the component it names.
fn install_fixture_package() -> TempDir {
    let plugins = TempDir::new().expect("create plugin package root");
    let package = plugins.path().join("channel-fixture");
    std::fs::create_dir_all(&package).expect("create plugin package");
    std::fs::copy(fixture(), package.join("channel-fixture.wasm"))
        .expect("copy channel component fixture");
    std::fs::copy(
        PathBuf::from(env!("CARGO_MANIFEST_DIR")).join(MANIFEST),
        package.join("manifest.toml"),
    )
    .expect("install the canonical fixture manifest");
    plugins
}

/// Config declaring one logical channel instance owned by an enabled agent.
///
/// The agent carries a model provider and risk profile so the whole thing
/// passes `Config::validate`, making this a realistic operator config rather
/// than a fixture shaped only to satisfy the loader.
fn activation_config(plugins: &TempDir, alias: &str, retry_count: &str) -> Config {
    let mut config = Config::default();
    config.plugins.enabled = true;
    config.plugins.auto_discover = false;
    config.plugins.max_active_instances = 1;
    config.plugins.plugins_dir = plugins.path().display().to_string();
    config
        .risk_profiles
        .insert("default".to_string(), RiskProfileConfig::default());
    config.providers.models.anthropic.insert(
        "default".to_string(),
        AnthropicModelProviderConfig::default(),
    );
    config.channels.plugin.insert(
        alias.to_string(),
        PluginChannelConfig {
            package: "channel-fixture".to_string(),
            enabled: true,
        },
    );
    config.agents.insert(
        "operator".to_string(),
        AliasedAgentConfig {
            channels: vec![ChannelRef::new(format!("plugin.{alias}"))],
            model_provider: ModelProviderRef::new("anthropic.default"),
            risk_profile: "default".into(),
            ..AliasedAgentConfig::default()
        },
    );

    // Operator values live under the instance-owned config key, exactly as the
    // activation loader will resolve them.
    let host = PluginHost::from_plugins_dir(plugins.path()).expect("admit fixture package");
    let manifest = host
        .manifest("channel-fixture")
        .expect("fixture manifest is admitted");
    let scope = PluginInstanceScope::from_manifest(
        manifest,
        PluginCapability::Channel,
        alias,
        manifest.permissions.iter().copied(),
    )
    .expect("admit configured logical channel");
    // The strict fixture requires a typed retry_count, a non-empty
    // credential_epoch, and a scoped api_token secret, all resolved from this
    // instance-owned entry. The send below must present the current
    // `{credential_epoch}:{api_token}` revision.
    config.plugins.entries.push(PluginEntryConfig {
        name: scope
            .id()
            .config_entry_key()
            .expect("derive canonical fixture config key"),
        config: HashMap::from([
            ("retry_count".to_string(), retry_count.to_string()),
            ("credential_epoch".to_string(), "v1".to_string()),
            ("api_token".to_string(), "channel-secret".to_string()),
        ]),
        ..PluginEntryConfig::default()
    });

    config
}

#[tokio::test]
async fn configured_channel_reaches_real_guest_and_shared_listener_contract() {
    let plugins = install_fixture_package();
    let config = activation_config(&plugins, "operations", "5");
    config
        .validate()
        .expect("the activation declaration is valid operator config");

    let channels =
        zeroclaw_runtime::plugin_runtime::configured_plugin_channels(Arc::new(config), None).await;

    assert_eq!(channels.len(), 1, "the configured fixture must construct");
    let channel = Arc::clone(&channels[0]);
    assert_eq!(channel.name(), "plugin");
    assert_eq!(
        channel.alias(),
        "operations",
        "the channel must carry the operator's alias, not the package name"
    );
    assert_eq!(channel.self_handle().as_deref(), Some("@fixture"));
    assert!(channel.health_check().await);
    // The strict guest accepts a send only when its content is the current
    // `{credential_epoch}:{api_token}` revision resolved at point of use.
    channel
        .send(&SendMessage::new("v1:channel-secret", "room"))
        .await
        .expect("the real guest accepts an outbound message");

    // The adapter owns its poll loop and must keep running until its receiver
    // goes away, which is the contract the shared supervisor relies on.
    let (tx, rx) = tokio::sync::mpsc::channel(1);
    let listener_channel = Arc::clone(&channel);
    let listener = zeroclaw_spawn::spawn!(async move { listener_channel.listen(tx).await });
    tokio::time::sleep(Duration::from_millis(75)).await;
    assert!(
        !listener.is_finished(),
        "the real plugin listener must retain its polling lifecycle"
    );
    drop(rx);
    tokio::time::timeout(Duration::from_secs(5), listener)
        .await
        .expect("listener exits after its receiver closes")
        .expect("listener task joins cleanly")
        .expect("listener returns successfully");
}

/// The guest refuses any config other than `{"retry_count":5}`, so a wrong
/// operator value must surface as a construction failure that the loader
/// reports and skips — not as a half-configured live channel.
#[tokio::test]
async fn a_channel_whose_guest_rejects_its_config_is_not_activated() {
    let plugins = install_fixture_package();
    let config = activation_config(&plugins, "operations", "9");

    let channels =
        zeroclaw_runtime::plugin_runtime::configured_plugin_channels(Arc::new(config), None).await;

    assert!(
        channels.is_empty(),
        "a guest that refuses its configuration must not be registered"
    );
}

/// An installed, enabled, correctly configured package still must not activate
/// when no enabled agent routes to its alias.
#[tokio::test]
async fn a_channel_without_an_enabled_owner_is_not_activated() {
    let plugins = install_fixture_package();
    let mut config = activation_config(&plugins, "operations", "5");
    config.agents.get_mut("operator").unwrap().enabled = false;

    let channels =
        zeroclaw_runtime::plugin_runtime::configured_plugin_channels(Arc::new(config), None).await;

    assert!(
        channels.is_empty(),
        "an orphaned declaration must stay inert"
    );
}
