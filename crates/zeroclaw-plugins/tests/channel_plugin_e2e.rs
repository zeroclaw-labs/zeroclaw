//! End-to-end: drive a real WASM channel plugin through the host's `WasmChannel`
//! exactly as the daemon does. Loads `channel-fixture.wasm` — a minimal echo
//! channel built for `wasm32-wasip2` (source in `tests/fixtures/channel-fixture/`)
//! — instantiates it with `WasmChannel::from_wasm`, and exercises the full path:
//! configure → get-channel-capabilities → name → self-handle → health-check →
//! send → inbound poll (via `listen`).
//!
//! The component is provisioned out of band as a build artifact (never committed,
//! same as `reference_plugin_e2e`):
//!
//! ```text
//! cd crates/zeroclaw-plugins/tests/fixtures/channel-fixture
//! cargo build --target wasm32-wasip2 --release
//! cp target/wasm32-wasip2/release/channel_fixture.wasm ../channel-fixture.wasm
//! ```
//!
//! When the fixture is absent this test skips, so it never fails a checkout that
//! did not build it.

#![cfg(feature = "plugins-wasm-cranelift")]

use std::collections::HashMap;
use std::path::PathBuf;
use std::time::Duration;

use zeroclaw_api::channel::{Channel, SendMessage};
use zeroclaw_plugins::PluginPermission;
use zeroclaw_plugins::component::PluginLimits;
use zeroclaw_plugins::wasm_channel::WasmChannel;

fn fixture() -> Option<PathBuf> {
    let path =
        PathBuf::from(env!("CARGO_MANIFEST_DIR")).join("tests/fixtures/channel-fixture.wasm");
    path.exists().then_some(path)
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
    let Some(wasm) = fixture() else {
        eprintln!(
            "channel-fixture.wasm absent; skipping. Build it per the module docs \
             (cargo build --target wasm32-wasip2 --release in tests/fixtures/channel-fixture)."
        );
        return;
    };

    // Instantiate exactly as the runtime helper does: alias, permissions, config,
    // limits. Succeeding proves `configure` and `get-channel-capabilities` ran.
    let config: HashMap<String, String> = HashMap::new();
    let channel = WasmChannel::from_wasm(
        "echo-channel",
        &wasm,
        &[PluginPermission::ConfigRead],
        &config,
        test_limits(),
    )
    .await
    .expect("channel plugin instantiates (configure + get-channel-capabilities)");

    // Identity + capability-gated exports cached at load time.
    assert_eq!(channel.name(), "echo-channel");
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
    channel.listen(tx).await.expect("listen starts");
    let msg = tokio::time::timeout(Duration::from_secs(5), rx.recv())
        .await
        .expect("inbound message arrives within timeout")
        .expect("channel sender not dropped");
    assert_eq!(msg.content, "ping");
    assert_eq!(msg.channel, "echo-channel");
}
