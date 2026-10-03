//! The channel plugin fixture shared by the root plugin e2e targets: the
//! built component, its installed package, and a config that activates one
//! logical channel instance of it.
//!
//! Included with `#[path]` by each target that needs it, not declared in
//! `tests/support/mod.rs`, so no other test binary compiles it.

use std::collections::HashMap;
use std::path::PathBuf;
use std::process::Command;
use std::sync::OnceLock;

use tempfile::TempDir;
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

/// The fixture's package name, as its manifest declares it.
pub(crate) const PACKAGE: &str = "channel-fixture";
/// The logical channel alias the root e2e targets configure.
pub(crate) const ALIAS: &str = "operations";
/// The webhook path the fixture guest claims, whatever its alias.
pub(crate) const ROUTE: &str = "fixture";
/// The scoped `api_token` secret [`activation_config`] gives the instance.
/// The guest's webhook export also requires it in [`SECRET_HEADER`].
pub(crate) const SECRET: &str = "channel-secret";
/// The webhook header the guest checks against [`SECRET`].
pub(crate) const SECRET_HEADER: &str = "x-fixture-secret";
/// The `credential_epoch` [`activation_config`] gives the instance.
const CREDENTIAL_EPOCH: &str = "v1";
/// The only content the guest's `send` accepts: the current
/// `{CREDENTIAL_EPOCH}:{SECRET}` revision.
pub(crate) const REPLY: &str = "v1:channel-secret";

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
pub(crate) fn install_fixture_package() -> TempDir {
    let plugins = TempDir::new().expect("create plugin package root");
    let package = plugins.path().join(PACKAGE);
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
pub(crate) fn activation_config(plugins: &TempDir, alias: &str, retry_count: &str) -> Config {
    // Isolate the durable plugin state and encryption key for parallel fixtures.
    let mut config = Config {
        data_dir: plugins.path().join("data"),
        config_path: plugins.path().join("config.toml"),
        ..Config::default()
    };
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
            package: PACKAGE.to_string(),
            enabled: true,
            ..PluginChannelConfig::default()
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
        .manifest(PACKAGE)
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
            ("credential_epoch".to_string(), CREDENTIAL_EPOCH.to_string()),
            ("api_token".to_string(), SECRET.to_string()),
        ]),
        ..PluginEntryConfig::default()
    });

    config
}
