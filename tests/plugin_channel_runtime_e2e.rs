//! Cross-crate proof that configured channel plugins reach the real WASM
//! adapter with their exact host-owned logical alias.

#![cfg(feature = "plugins-wasm-cranelift")]

use std::collections::HashMap;
use std::path::PathBuf;
use std::process::Command;
use std::sync::{Arc, OnceLock};
use std::time::Duration;

use tempfile::TempDir;
use zeroclaw_api::channel::SendMessage;
use zeroclaw_api::webhook::{
    PluginWebhookRegistry, RawWebhook, WebhookCancellation, WebhookOutcome,
};
use zeroclaw_config::multi_agent::{PeerGroupConfig, PeerUsername};
use zeroclaw_config::providers::ChannelRef;
use zeroclaw_config::schema::{AliasedAgentConfig, Config, PluginChannelConfig, PluginEntryConfig};
use zeroclaw_plugins::host::PluginHost;
use zeroclaw_plugins::instance::PluginInstanceScope;
use zeroclaw_plugins::{PluginCapability, PluginManifest};

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

fn manifest() -> PluginManifest {
    toml::from_str(include_str!(
        "../crates/zeroclaw-plugins/tests/fixtures/channel-fixture/plugin-manifest.toml"
    ))
    .expect("parse canonical channel fixture manifest")
}

#[tokio::test]
async fn configured_channel_reaches_real_guest_and_shared_listener_contract() {
    let plugins = TempDir::new().expect("create plugin package root");
    let package = plugins.path().join("channel-fixture");
    std::fs::create_dir_all(&package).expect("create plugin package");
    std::fs::copy(fixture(), package.join("channel-fixture.wasm"))
        .expect("copy channel component fixture");
    std::fs::write(
        package.join("manifest.toml"),
        toml::to_string(&manifest()).expect("serialize fixture manifest"),
    )
    .expect("write fixture manifest");

    let host = PluginHost::from_plugins_dir(plugins.path()).expect("admit fixture package");
    let admitted_manifest = host
        .manifest("channel-fixture")
        .expect("fixture manifest is admitted");
    let scope = PluginInstanceScope::from_manifest(
        admitted_manifest,
        PluginCapability::Channel,
        "operations",
        admitted_manifest.permissions.iter().copied(),
    )
    .expect("admit configured logical channel");

    let mut config = Config::default();
    config.plugins.enabled = true;
    config.plugins.auto_discover = false;
    config.plugins.max_active_instances = 1;
    config.plugins.plugins_dir = plugins.path().display().to_string();
    config.channels.plugin.insert(
        "operations".to_string(),
        PluginChannelConfig {
            package: "channel-fixture".to_string(),
            enabled: true,
        },
    );
    config.agents.insert(
        "operator".to_string(),
        AliasedAgentConfig {
            channels: vec![ChannelRef::new("plugin.operations")],
            ..AliasedAgentConfig::default()
        },
    );
    config.peer_groups.insert(
        "plugin_operations".to_string(),
        PeerGroupConfig {
            channel: ChannelRef::new("plugin.operations"),
            external_peers: vec![PeerUsername::new("webhook")],
            ..PeerGroupConfig::default()
        },
    );
    config.plugins.entries.push(PluginEntryConfig {
        name: scope
            .id()
            .config_entry_key()
            .expect("derive canonical fixture config key"),
        config: HashMap::from([
            ("retry_count".to_string(), "5".to_string()),
            ("credential_epoch".to_string(), "v1".to_string()),
            ("api_token".to_string(), "token-operations".to_string()),
        ]),
        ..PluginEntryConfig::default()
    });

    let webhooks = PluginWebhookRegistry::new();
    let channels = zeroclaw_runtime::plugin_runtime::configured_plugin_channels(
        Arc::new(config),
        None,
        Some(&webhooks),
    )
    .await;
    assert_eq!(channels.len(), 1, "the configured fixture must construct");
    let channel = Arc::clone(&channels[0]);
    assert_eq!(channel.name(), "plugin");
    assert_eq!(channel.alias(), "operations");
    assert_eq!(channel.self_handle().as_deref(), Some("@fixture"));
    assert!(channel.health_check().await);
    channel
        .send(&SendMessage::new("v1:token-operations", "operations"))
        .await
        .expect("the real guest receives its scoped typed config and secret");

    let (tx, mut rx) = tokio::sync::mpsc::channel(1);
    let listener_channel = Arc::clone(&channel);
    let listener = zeroclaw_spawn::spawn!(async move { listener_channel.listen(tx).await });

    let sink = webhooks
        .get("fixture")
        .expect("runtime registers the guest-declared webhook path");
    let (reply, response) = tokio::sync::oneshot::channel();
    sink.send(RawWebhook {
        method: "POST".to_string(),
        query: String::new(),
        headers: vec![(
            "x-fixture-secret".to_string(),
            "token-operations".to_string(),
        )],
        body: b"runtime webhook".to_vec(),
        cancellation: WebhookCancellation::new(),
        idempotency: None,
        reply,
    })
    .await
    .expect("registered runtime sink accepts a webhook");
    assert!(matches!(response.await, Ok(Ok(WebhookOutcome::Ack))));
    let inbound = tokio::time::timeout(Duration::from_secs(2), rx.recv())
        .await
        .expect("runtime webhook reaches the shared listener")
        .expect("listener remains active");
    assert_eq!(inbound.content, "runtime webhook");
    assert_eq!(inbound.channel, "plugin");
    assert_eq!(inbound.channel_alias.as_deref(), Some("operations"));
    assert!(
        !listener.is_finished(),
        "the real plugin listener must retain its polling lifecycle"
    );
    drop(rx);
    tokio::time::timeout(Duration::from_secs(1), listener)
        .await
        .expect("listener exits after its receiver closes")
        .expect("listener task joins cleanly")
        .expect("listener returns successfully");
}
