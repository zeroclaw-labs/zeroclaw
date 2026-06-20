//! Round-trip: proves the SDK's `channel-plugin` guest bindings, plus the
//! `ChannelPlugin` trait's capability-gated default stubs, work end-to-end
//! through the real, unmodified host. `examples/channel-echo` implements
//! only the required methods and declares zero optional capabilities, so
//! the host never calls the four capability-gated functions
//! (self-handle/self-addressed-mention/drop-self-message/
//! multi-message-delay-ms) that are still `trappable` host-side pending
//! the channel sync-call fix — this test exercises only the required
//! `send`/`poll-message` path, which is unaffected by that open issue.

mod common;

use std::path::Path;
use std::time::Duration;

#[tokio::test]
async fn channel_echo_round_trips_through_plugin_host() {
    if !common::wasm32_wasip2_installed() {
        eprintln!("skipping: wasm32-wasip2 target not installed");
        return;
    }

    let example_dir = Path::new(env!("CARGO_MANIFEST_DIR")).join("examples/channel-echo");
    let wasm_path = common::build_example(&example_dir, "channel_echo");

    let workdir = tempfile::tempdir().expect("tempdir");
    let plugin_dir = workdir.path().join("plugins/echo");
    std::fs::create_dir_all(&plugin_dir).unwrap();
    std::fs::copy(&wasm_path, plugin_dir.join("echo.wasm")).unwrap();
    std::fs::write(
        plugin_dir.join("manifest.toml"),
        r#"
name = "echo"
version = "0.1.0"
description = "spike: channel-echo round trip"
wasm_path = "echo.wasm"
capabilities = ["channel"]
"#,
    )
    .unwrap();

    let host = zeroclaw_plugins::host::PluginHost::new(workdir.path()).expect("PluginHost::new");
    let channel = host
        .instantiate_channel_plugin("echo", None, None, Default::default())
        .await
        .expect("instantiate_channel_plugin");

    assert_eq!(zeroclaw_api::channel::Channel::name(&*channel), "echo");

    let send_message = zeroclaw_api::channel::SendMessage::new("hello from host", "someone");
    zeroclaw_api::channel::Channel::send(&*channel, &send_message)
        .await
        .expect("send");

    let (tx, mut rx) = tokio::sync::mpsc::channel(8);
    let listen_channel = channel.clone();
    zeroclaw_spawn::spawn!(async move {
        let _ = zeroclaw_api::channel::Channel::listen(&*listen_channel, tx).await;
    });

    let received = tokio::time::timeout(Duration::from_secs(5), rx.recv())
        .await
        .expect("timed out waiting for inbound message")
        .expect("channel closed without a message");

    assert_eq!(received.content, "hello from host");

    // channel-echo declares no optional capabilities, so this exercises
    // the host's documented fallback for an unset `health-check` flag:
    // the host composes the trait default (`true`) without calling into
    // the guest, mirroring the `reindex` assertion in spike_memory.rs.
    assert!(zeroclaw_api::channel::Channel::health_check(&*channel).await);
}
