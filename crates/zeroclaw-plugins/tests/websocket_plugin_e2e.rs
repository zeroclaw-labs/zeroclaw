//! End-to-end coverage for the Component Model WebSocket host resource.

#![cfg(feature = "plugins-wasm-cranelift")]

use std::collections::HashMap;
use std::path::PathBuf;
use std::process::Command;
use std::sync::OnceLock;
use std::time::Duration;

use futures_util::{SinkExt, StreamExt};
use tokio::net::TcpListener;
use tokio_tungstenite::accept_hdr_async;
use tokio_tungstenite::tungstenite::Message;
use tokio_tungstenite::tungstenite::handshake::server::{
    Callback, ErrorResponse, Request as ServerRequest, Response as ServerResponse,
};
use tokio_tungstenite::tungstenite::http::HeaderValue;
use zeroclaw_api::channel::Channel;
use zeroclaw_plugins::component::PluginLimits;
use zeroclaw_plugins::config::{PluginConfigResolver, resolve_plugin_config};
use zeroclaw_plugins::egress::{EgressHostService, EgressPolicy, EgressPolicyResolver};
use zeroclaw_plugins::endpoint::PluginChannelEndpoint;
use zeroclaw_plugins::instance::PluginInstanceScope;
use zeroclaw_plugins::services::PluginHostServices;
use zeroclaw_plugins::wasm_channel::WasmChannel;
use zeroclaw_plugins::{PluginCapability, PluginManifest, PluginPermission};

const ECHO_HOST: &str = "127.0.0.1";

struct EchoHandshake;

impl Callback for EchoHandshake {
    fn on_request(
        self,
        request: &ServerRequest,
        mut response: ServerResponse,
    ) -> Result<ServerResponse, ErrorResponse> {
        assert_eq!(request.uri().path(), "/echo");
        assert_eq!(request.headers()["x-fixture"], "channel-websocket");
        assert_eq!(request.headers()["sec-websocket-protocol"], "echo.v1");
        response.headers_mut().insert(
            "sec-websocket-protocol",
            HeaderValue::from_static("echo.v1"),
        );
        Ok(response)
    }
}

fn fixture() -> PathBuf {
    static FIXTURE: OnceLock<PathBuf> = OnceLock::new();
    FIXTURE
        .get_or_init(|| {
            let fixture_dir =
                PathBuf::from(env!("CARGO_MANIFEST_DIR")).join("tests/fixtures/channel-fixture");
            let target_dir =
                PathBuf::from(env!("CARGO_TARGET_TMPDIR")).join("channel-websocket-fixture");
            let status = Command::new(env!("CARGO"))
                .current_dir(&fixture_dir)
                .args([
                    "build",
                    "--locked",
                    "--quiet",
                    "--package",
                    "zeroclaw-channel-plugin-fixture",
                    "--example",
                    "zeroclaw-channel-websocket-fixture",
                    "--features",
                    "websocket",
                    "--target",
                    "wasm32-wasip2",
                    "--target-dir",
                ])
                .arg(&target_dir)
                .status()
                .expect("run Cargo for the channel WebSocket fixture");
            assert!(
                status.success(),
                "WebSocket fixture must build; install the wasm32-wasip2 target"
            );

            let wasm = target_dir
                .join("wasm32-wasip2/debug/examples/zeroclaw_channel_websocket_fixture.wasm");
            assert!(wasm.is_file(), "WebSocket fixture WASM was not produced");
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

fn manifest() -> PluginManifest {
    PluginManifest {
        name: "channel-websocket-fixture".to_string(),
        version: "0.0.0".to_string(),
        description: None,
        author: None,
        wasm_path: Some("channel-websocket-fixture.wasm".to_string()),
        capabilities: vec![PluginCapability::Channel],
        permissions: vec![
            PluginPermission::ConfigRead,
            PluginPermission::WebSocketClient,
        ],
        config_schema: Some(serde_json::json!({
            "$schema": "https://json-schema.org/draft/2020-12/schema",
            "type": "object",
            "required": ["url"],
            "additionalProperties": false,
            "properties": {
                "url": {"type": "string", "minLength": 1}
            }
        })),
        signature: None,
        publisher_key: None,
    }
}

fn host_services(url: String) -> PluginHostServices {
    let manifest = manifest();
    let config = PluginConfigResolver::new(move |scope| {
        let values = HashMap::from([("url".to_string(), url.clone())]);
        resolve_plugin_config(&manifest, scope, Some(&values))
    });
    let egress = EgressHostService::new(EgressPolicyResolver::new(|_| {
        EgressPolicy::new([ECHO_HOST.to_string()], [ECHO_HOST.to_string()], [], 4)
    }));
    PluginHostServices::new(config, egress)
}

async fn build_channel(
    url: String,
    grants: impl IntoIterator<Item = PluginPermission>,
) -> anyhow::Result<WasmChannel> {
    let manifest = manifest();
    let scope =
        PluginInstanceScope::from_manifest(&manifest, PluginCapability::Channel, "main", grants)?;
    let endpoint = PluginChannelEndpoint::new(scope, "websocket_fixture")?;
    WasmChannel::from_wasm(endpoint, &fixture(), &host_services(url), limits()).await
}

async fn run_echo_server(listener: TcpListener) {
    let (stream, _) = listener.accept().await.expect("accept WebSocket fixture");
    let mut socket = accept_hdr_async(stream, EchoHandshake)
        .await
        .expect("complete WebSocket fixture handshake");

    for _ in 0..2 {
        let message = socket
            .next()
            .await
            .expect("fixture sends an application message")
            .expect("read fixture application message");
        assert!(matches!(message, Message::Text(_) | Message::Binary(_)));
        socket.send(message).await.expect("echo fixture message");
    }
    socket.close(None).await.expect("close fixture socket");
}

#[tokio::test]
async fn channel_component_exchanges_typed_websocket_messages() {
    let listener = TcpListener::bind((ECHO_HOST, 0))
        .await
        .expect("bind WebSocket echo server");
    let port = listener.local_addr().expect("echo listener address").port();
    let server = zeroclaw_spawn::spawn!(run_echo_server(listener));
    let url = format!("ws://{ECHO_HOST}:{port}/echo");
    let channel = build_channel(
        url,
        [
            PluginPermission::ConfigRead,
            PluginPermission::WebSocketClient,
        ],
    )
    .await
    .expect("instantiate WebSocket channel fixture");

    let (tx, mut rx) = tokio::sync::mpsc::channel(2);
    let channel_listener = zeroclaw_spawn::spawn!(async move { channel.listen(tx).await });
    let first = tokio::time::timeout(Duration::from_secs(5), rx.recv())
        .await
        .expect("text echo arrives before timeout")
        .expect("channel listener remains connected");
    let second = tokio::time::timeout(Duration::from_secs(5), rx.recv())
        .await
        .expect("binary echo arrives before timeout")
        .expect("channel listener remains connected");

    assert_eq!(first.content, "text:component-text:echo.v1");
    assert_eq!(second.content, "binary:000102ff");
    for message in [&first, &second] {
        assert_eq!(message.channel, "websocket_fixture");
        assert_eq!(message.channel_alias.as_deref(), Some("main"));
    }

    channel_listener.abort();
    let error = channel_listener
        .await
        .expect_err("aborting listen cancels its polling loop");
    assert!(error.is_cancelled());
    tokio::time::timeout(Duration::from_secs(5), server)
        .await
        .expect("echo server exits before timeout")
        .expect("echo server task joins");
}

#[tokio::test]
async fn linker_omits_websocket_import_without_effective_permission() {
    let error = match build_channel(
        format!("ws://{ECHO_HOST}:1/echo"),
        [PluginPermission::ConfigRead],
    )
    .await
    {
        Ok(_) => panic!("WebSocket component must not instantiate without its effective grant"),
        Err(error) => error,
    };
    let message = format!("{error:#}");
    assert!(
        message.contains("zeroclaw:plugin/websocket@0.1.0")
            && message.contains("not found in the linker"),
        "linker denial should identify the missing WebSocket import: {message}"
    );
}
