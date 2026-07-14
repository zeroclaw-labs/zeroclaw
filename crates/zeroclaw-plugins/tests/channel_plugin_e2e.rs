//! End-to-end: drive a real WASM channel plugin through the host's `WasmChannel`
//! exactly as the daemon does. Loads `channel-fixture.wasm` — a minimal echo
//! channel built for `wasm32-wasip2` (source in `tests/fixtures/channel-fixture/`)
//! — instantiates it with `WasmChannel::from_wasm`, and exercises the full path:
//! configure → get-channel-capabilities → name → self-handle → health-check →
//! send → inbound poll (via `listen`).
//!
//! The component is built from its checked-in source on demand:
//!
//! ```text
//! cd crates/zeroclaw-plugins/tests/fixtures/channel-fixture
//! cargo build --locked --target wasm32-wasip2
//! ```

#![cfg(feature = "plugins-wasm-cranelift")]

use std::collections::HashMap;
use std::path::PathBuf;
use std::process::Command;
use std::sync::OnceLock;
use std::time::Duration;

use zeroclaw_api::channel::{Channel, SendMessage};
use zeroclaw_plugins::PluginPermission;
use zeroclaw_plugins::component::PluginLimits;
use zeroclaw_plugins::error::PluginError;
use zeroclaw_plugins::wasm_channel::WasmChannel;

fn fixture() -> PathBuf {
    static FIXTURE: OnceLock<PathBuf> = OnceLock::new();
    FIXTURE
        .get_or_init(|| {
            let fixture_dir =
                PathBuf::from(env!("CARGO_MANIFEST_DIR")).join("tests/fixtures/channel-fixture");
            let target_dir = fixture_dir.join("target");
            let status = Command::new(env!("CARGO"))
                .current_dir(&fixture_dir)
                .args([
                    "build",
                    "--locked",
                    "--quiet",
                    "--target",
                    "wasm32-wasip2",
                    "--target-dir",
                ])
                .arg(&target_dir)
                .status()
                .expect("run cargo to build channel component fixture");
            assert!(
                status.success(),
                "channel component fixture must build; install the wasm32-wasip2 target"
            );
            let wasm = target_dir.join("wasm32-wasip2/debug/channel_fixture.wasm");
            assert!(wasm.is_file(), "channel component fixture was not produced");
            wasm
        })
        .clone()
}

fn test_limits() -> PluginLimits {
    PluginLimits {
        call_fuel: 1_000_000_000,
        max_memory_bytes: 64 * 1024 * 1024,
        max_table_elements: 100_000,
        max_instances: 64,
    }
}

fn outbound(content: &str) -> SendMessage {
    SendMessage {
        content: content.to_string(),
        recipient: "tester".to_string(),
        subject: None,
        thread_ts: None,
        cancellation_token: None,
        attachments: Vec::new(),
        in_reply_to: None,
        suppress_voice: false,
        force_voice: false,
    }
}

#[tokio::test]
async fn channel_plugin_runs_end_to_end() {
    let wasm = fixture();

    // Instantiate exactly as the runtime helper does: alias, permissions, config,
    // limits. Succeeding proves `configure` and `get-channel-capabilities` ran.
    let config: HashMap<String, String> = HashMap::new();
    let bytes = std::fs::read(&wasm).expect("read fixture for signed digest");
    let digest = zeroclaw_plugins::signature::sha256_hex(&bytes);
    let channel = WasmChannel::from_wasm_with_digest(
        "echo-channel",
        &wasm,
        Some(&digest),
        &[PluginPermission::ConfigRead],
        &config,
        test_limits(),
    )
    .await
    .expect("channel plugin instantiates (configure + get-channel-capabilities)");

    let tampered_dir = tempfile::tempdir().expect("temporary tampered fixture");
    let tampered_path = tampered_dir.path().join("channel-fixture.wasm");
    let mut tampered = bytes;
    tampered.push(0);
    std::fs::write(&tampered_path, tampered).expect("write tampered fixture");
    let error = WasmChannel::from_wasm_with_digest(
        "tampered-channel",
        &tampered_path,
        Some(&digest),
        &[PluginPermission::ConfigRead],
        &config,
        test_limits(),
    )
    .await
    .err()
    .expect("payload replacement must fail before component loading");
    assert!(
        matches!(
            error.downcast_ref::<PluginError>(),
            Some(PluginError::PayloadDigestMismatch { .. })
        ),
        "unexpected tamper error: {error:#}"
    );

    // Identity + capability-gated exports cached at load time.
    assert_eq!(channel.name(), zeroclaw_api::channel::PLUGIN_CHANNEL_TYPE);
    assert_eq!(
        channel.self_handle().as_deref(),
        Some("@echo"),
        "fixture advertises SELF_HANDLE and returns @echo"
    );
    assert!(channel.health_check().await, "fixture reports healthy");

    // Outbound send is accepted by the plugin.
    channel
        .send(&outbound("pong"))
        .await
        .expect("send succeeds");

    // Inbound: the host listen loop drains the plugin's `poll-message` and
    // forwards the one canned message the fixture delivers.
    let (tx, mut rx) = tokio::sync::mpsc::channel(4);
    let listener = tokio::spawn(async move { channel.listen(tx).await });
    let msg = tokio::time::timeout(Duration::from_secs(5), rx.recv())
        .await
        .expect("inbound message arrives within timeout")
        .expect("channel sender not dropped");
    assert_eq!(msg.content, "ping");
    assert_eq!(msg.channel, zeroclaw_api::channel::PLUGIN_CHANNEL_TYPE);
    assert_eq!(msg.channel_alias.as_deref(), Some("echo-channel"));

    assert!(
        !listener.is_finished(),
        "listen must own the polling loop until its caller cancels it"
    );
    listener.abort();
    let err = listener
        .await
        .expect_err("aborting the listener should cancel the polling loop");
    assert!(err.is_cancelled());
}
