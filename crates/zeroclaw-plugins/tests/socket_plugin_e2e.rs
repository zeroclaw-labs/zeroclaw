//! Real Component Model coverage for the typed plugin socket resource.

#![cfg(feature = "plugins-wasm-cranelift")]

use std::collections::HashMap;
use std::path::PathBuf;
use std::process::Command;
use std::sync::OnceLock;
use std::time::Duration;

use tokio::io::{AsyncReadExt, AsyncWriteExt};
use tokio::net::TcpListener;
use zeroclaw_api::channel::Channel;
use zeroclaw_api::tool::Tool;
use zeroclaw_plugins::component::PluginLimits;
use zeroclaw_plugins::config::{PluginConfigResolver, resolve_plugin_config};
use zeroclaw_plugins::egress::{EgressHostService, EgressPolicy, EgressPolicyResolver};
use zeroclaw_plugins::endpoint::PluginChannelEndpoint;
use zeroclaw_plugins::instance::PluginInstanceScope;
use zeroclaw_plugins::services::PluginHostServices;
use zeroclaw_plugins::wasm_channel::WasmChannel;
use zeroclaw_plugins::wasm_tool::WasmTool;
use zeroclaw_plugins::{PluginCapability, PluginManifest, PluginPermission};

fn build_fixture(package: &str, feature: &str, artifact: &str) -> PathBuf {
    let fixture_dir = PathBuf::from(env!("CARGO_MANIFEST_DIR"))
        .join("tests/fixtures")
        .join(if package.contains("channel") {
            "channel-fixture"
        } else {
            "tool-fixture"
        });
    let target_dir = PathBuf::from(env!("CARGO_TARGET_TMPDIR")).join(format!("{package}-socket"));
    let status = Command::new(env!("CARGO"))
        .current_dir(&fixture_dir)
        .args([
            "build",
            "--locked",
            "--quiet",
            "--package",
            package,
            "--features",
            feature,
            "--target",
            "wasm32-wasip2",
            "--target-dir",
        ])
        .arg(&target_dir)
        .status()
        .expect("run Cargo for socket component fixture");
    assert!(
        status.success(),
        "socket fixture must build; install the wasm32-wasip2 target"
    );
    let wasm = target_dir.join("wasm32-wasip2/debug").join(artifact);
    assert!(wasm.is_file(), "socket fixture WASM was not produced");
    wasm
}

fn channel_fixture() -> PathBuf {
    static FIXTURE: OnceLock<PathBuf> = OnceLock::new();
    FIXTURE
        .get_or_init(|| {
            build_fixture(
                "zeroclaw-channel-plugin-fixture",
                "socket-e2e",
                "zeroclaw_channel_plugin_fixture.wasm",
            )
        })
        .clone()
}

fn tool_fixture() -> PathBuf {
    static FIXTURE: OnceLock<PathBuf> = OnceLock::new();
    FIXTURE
        .get_or_init(|| {
            build_fixture(
                "zeroclaw-tool-plugin-fixture",
                "socket-e2e",
                "zeroclaw_tool_plugin_fixture.wasm",
            )
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

fn manifest(name: &str, capability: PluginCapability) -> PluginManifest {
    PluginManifest {
        name: name.to_string(),
        version: "0.0.0".to_string(),
        description: None,
        author: None,
        wasm_path: Some("fixture.wasm".to_string()),
        capabilities: vec![capability],
        permissions: vec![PluginPermission::ConfigRead, PluginPermission::SocketClient],
        config_schema: Some(serde_json::json!({
            "$schema": "https://json-schema.org/draft/2020-12/schema",
            "type": "object",
            "required": ["host", "port"],
            "additionalProperties": false,
            "properties": {
                "host": {"type": "string", "minLength": 1},
                "port": {"type": "integer", "minimum": 1, "maximum": 65535}
            }
        })),
        signature: None,
        publisher_key: None,
    }
}

fn services(
    manifest: PluginManifest,
    address: std::net::SocketAddr,
    allow_plaintext: bool,
) -> PluginHostServices {
    let configured = HashMap::from([
        ("host".to_string(), address.ip().to_string()),
        ("port".to_string(), address.port().to_string()),
    ]);
    let config_manifest = manifest.clone();
    let config = PluginConfigResolver::new(move |scope| {
        resolve_plugin_config(&config_manifest, scope, Some(&configured))
    });
    let egress = EgressHostService::new(EgressPolicyResolver::new(move |_| {
        EgressPolicy::new(
            ["127.0.0.1".to_string()],
            allow_plaintext.then(|| "127.0.0.1".to_string()),
            [],
            4,
        )
    }));
    PluginHostServices::new(config, egress)
}

async fn echo_server() -> std::net::SocketAddr {
    let listener = TcpListener::bind("127.0.0.1:0")
        .await
        .expect("bind component echo server");
    let address = listener.local_addr().expect("component echo address");
    zeroclaw_spawn::spawn!(async move {
        while let Ok((mut stream, _)) = listener.accept().await {
            zeroclaw_spawn::spawn!(async move {
                let mut bytes = [0_u8; 16 * 1024];
                loop {
                    match stream.read(&mut bytes).await {
                        Ok(0) | Err(_) => break,
                        Ok(count) => {
                            if stream.write_all(&bytes[..count]).await.is_err() {
                                break;
                            }
                        }
                    }
                }
            });
        }
    });
    address
}

fn scope(
    manifest: &PluginManifest,
    capability: PluginCapability,
    binding: &str,
    socket_grant: bool,
) -> PluginInstanceScope {
    PluginInstanceScope::from_manifest(
        manifest,
        capability,
        binding,
        [PluginPermission::ConfigRead]
            .into_iter()
            .chain(socket_grant.then_some(PluginPermission::SocketClient)),
    )
    .expect("admit socket fixture scope")
}

#[tokio::test]
async fn channel_component_round_trips_through_the_socket_resource() {
    let address = echo_server().await;
    let manifest = manifest("socket-fixture", PluginCapability::Channel);
    let services = services(manifest.clone(), address, true);
    let endpoint = PluginChannelEndpoint::new(
        scope(&manifest, PluginCapability::Channel, "mail.primary", true),
        "plugin",
    )
    .expect("socket channel endpoint");
    let channel = WasmChannel::from_wasm(endpoint, &channel_fixture(), &services, limits())
        .await
        .expect("instantiate socket channel component");
    let (tx, mut rx) = tokio::sync::mpsc::channel(1);
    let listener = zeroclaw_spawn::spawn!(async move { channel.listen(tx).await });
    let message = tokio::time::timeout(Duration::from_secs(5), rx.recv())
        .await
        .expect("component echo before timeout")
        .expect("channel listener stays connected");
    assert_eq!(message.content, "component-ping");
    assert_eq!(message.channel, "plugin");
    assert_eq!(message.channel_alias.as_deref(), Some("mail.primary"));
    listener.abort();
}

#[tokio::test]
async fn tool_component_round_trips_through_the_socket_resource() {
    let address = echo_server().await;
    let manifest = manifest("socket-tool-fixture", PluginCapability::Tool);
    let services = services(manifest.clone(), address, true);
    let tool = WasmTool::from_wasm(
        tool_fixture(),
        scope(&manifest, PluginCapability::Tool, "mail-tool", true),
        services,
        limits(),
    )
    .expect("instantiate socket tool component");
    let result = tool
        .execute(serde_json::json!({}))
        .await
        .expect("execute socket tool component");
    assert!(result.success);
    assert_eq!(result.output.to_string(), "tool-component-ping");
}

#[tokio::test]
async fn socket_import_is_unlinked_without_the_effective_grant() {
    let address = echo_server().await;
    let manifest = manifest("socket-fixture", PluginCapability::Channel);
    let services = services(manifest.clone(), address, true);
    let endpoint = PluginChannelEndpoint::new(
        scope(&manifest, PluginCapability::Channel, "denied", false),
        "plugin",
    )
    .expect("denied endpoint");
    let result = WasmChannel::from_wasm(endpoint, &channel_fixture(), &services, limits()).await;
    assert!(
        result.is_err(),
        "a component importing sockets must not instantiate without SocketClient"
    );
}

#[tokio::test]
async fn component_plaintext_fails_closed_without_shared_policy() {
    let address = echo_server().await;
    let manifest = manifest("socket-fixture", PluginCapability::Channel);
    let services = services(manifest.clone(), address, false);
    let endpoint = PluginChannelEndpoint::new(
        scope(&manifest, PluginCapability::Channel, "denied", true),
        "plugin",
    )
    .expect("policy-denied endpoint");
    let error = WasmChannel::from_wasm(endpoint, &channel_fixture(), &services, limits())
        .await
        .err()
        .expect("plaintext policy denies configure");
    let error = format!("{error:#}");
    assert!(error.contains("access-denied"), "{error}");
}
