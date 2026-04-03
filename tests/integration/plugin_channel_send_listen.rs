#![cfg(feature = "plugins-wasm")]

//! Integration test: WasmChannel bridge send/listen round-trip through Extism.
//!
//! Loads `channel_plugin.wasm`, wires it through `WasmChannel`, and validates
//! that `send()` + `listen()` produce the expected `ChannelMessage` events.

use std::path::Path;
use std::sync::{Arc, Mutex};
use zeroclaw::channels::traits::{Channel, SendMessage};
use zeroclaw::plugins::wasm_channel::WasmChannel;

const CHANNEL_WASM: &str = "tests/plugins/artifacts/channel_plugin.wasm";

fn channel_wasm_path() -> std::path::PathBuf {
    Path::new(env!("CARGO_MANIFEST_DIR")).join(CHANNEL_WASM)
}

fn make_channel_plugin() -> Arc<Mutex<extism::Plugin>> {
    let wasm_path = channel_wasm_path();
    assert!(
        wasm_path.is_file(),
        "channel_plugin.wasm not found at {}",
        wasm_path.display()
    );
    let manifest = extism::Manifest::new([extism::Wasm::file(&wasm_path)])
        .with_timeout(std::time::Duration::from_secs(5));
    let plugin =
        extism::Plugin::new(&manifest, [], true).expect("failed to instantiate channel plugin");
    Arc::new(Mutex::new(plugin))
}

#[tokio::test]
async fn channel_send_returns_ok() {
    let plugin = make_channel_plugin();
    let ch = WasmChannel::new("test-wasm".into(), "channel-plugin".into(), plugin);

    let msg = SendMessage::new("hello from test", "alice");
    let result = ch.send(&msg).await;
    assert!(result.is_ok(), "channel_send should succeed: {:?}", result);
}

#[tokio::test]
async fn channel_listen_returns_synthetic_messages() {
    let plugin = make_channel_plugin();
    let ch = WasmChannel::new("test-wasm".into(), "channel-plugin".into(), plugin);

    let (tx, mut rx) = tokio::sync::mpsc::channel(10);
    ch.listen(tx).await.expect("channel_listen should succeed");

    let msg = rx
        .recv()
        .await
        .expect("should receive at least one message");
    assert_eq!(msg.id, "synthetic-1");
    assert_eq!(msg.sender, "test-bot");
    assert_eq!(msg.content, "hello from wasm channel plugin");
    assert_eq!(msg.channel, "wasm-test");
}

#[tokio::test]
async fn channel_send_then_listen_echoes_back() {
    let plugin = make_channel_plugin();
    let ch = WasmChannel::new("test-wasm".into(), "channel-plugin".into(), plugin);

    // Send a message first
    let msg = SendMessage::new("round-trip test", "bob");
    ch.send(&msg).await.expect("send should succeed");

    // Listen should return the echoed message plus the synthetic one
    let (tx, mut rx) = tokio::sync::mpsc::channel(10);
    ch.listen(tx).await.expect("listen should succeed");

    let echo_msg = rx.recv().await.expect("should receive echo message");
    assert_eq!(echo_msg.id, "echo-1");
    assert_eq!(echo_msg.content, "round-trip test");
    assert_eq!(echo_msg.sender, "bob");

    let synthetic_msg = rx.recv().await.expect("should receive synthetic message");
    assert_eq!(synthetic_msg.id, "synthetic-1");
    assert_eq!(synthetic_msg.content, "hello from wasm channel plugin");
}

#[tokio::test]
async fn channel_listen_messages_have_correct_fields() {
    let plugin = make_channel_plugin();
    let ch = WasmChannel::new("test-wasm".into(), "channel-plugin".into(), plugin);

    let (tx, mut rx) = tokio::sync::mpsc::channel(10);
    ch.listen(tx).await.expect("listen should succeed");

    let msg = rx.recv().await.expect("should receive message");
    assert_eq!(msg.reply_target, "wasm-channel");
    assert_eq!(msg.timestamp, 2000);
    assert!(msg.thread_ts.is_none());
    assert!(msg.interruption_scope_id.is_none());
    assert!(msg.attachments.is_empty());
}
