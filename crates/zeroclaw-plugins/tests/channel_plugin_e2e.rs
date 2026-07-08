//! End-to-end coverage for novel and built-in-mirror WASM channel plugins.
//!
//! The checked-in fixture echoes the JSON received by `configure` as its first
//! inbound message. Tests build that real component on demand, bind execution
//! to its digest, and exercise the long-running host listener.
//!
//! ```text
//! cd crates/zeroclaw-plugins/tests/fixtures/channel-fixture
//! cargo build --locked --target wasm32-wasip2
//! ```

#![cfg(feature = "plugins-wasm-cranelift")]

use std::collections::HashMap;
use std::path::{Path, PathBuf};
use std::process::Command;
use std::sync::{Arc, OnceLock};
use std::time::Duration;

use serde_json::Value;
use zeroclaw_api::channel::{Channel, ChannelMessage, SendMessage};
use zeroclaw_plugins::PluginPermission;
use zeroclaw_plugins::component::PluginLimits;
use zeroclaw_plugins::error::PluginError;
use zeroclaw_plugins::wasm_channel::{SenderAuthorizer, WasmChannel};

fn allow_all() -> SenderAuthorizer {
    Arc::new(|_| true)
}

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

fn fixture_digest(wasm: &Path) -> String {
    zeroclaw_plugins::signature::sha256_hex(
        &std::fs::read(wasm).expect("read channel component fixture"),
    )
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

async fn first_inbound(channel: &Arc<WasmChannel>) -> ChannelMessage {
    let (tx, mut rx) = tokio::sync::mpsc::channel(4);
    let listener_channel = Arc::clone(channel);
    let listener = ::zeroclaw_spawn::spawn!(async move { listener_channel.listen(tx).await });
    let message = tokio::time::timeout(Duration::from_secs(5), rx.recv())
        .await
        .expect("inbound message arrives within timeout")
        .expect("channel sender not dropped");
    assert!(!listener.is_finished(), "listen remains long-running");
    listener.abort();
    assert!(
        listener
            .await
            .expect_err("listener cancellation returns a join error")
            .is_cancelled()
    );
    message
}

#[tokio::test]
async fn novel_channel_plugin_runs_end_to_end() {
    let wasm = fixture();
    let digest = fixture_digest(&wasm);
    let config = HashMap::from([("greeting".to_string(), "hi".to_string())]);
    let channel = Arc::new(
        WasmChannel::from_wasm_with_digest(
            "echo-channel",
            &wasm,
            Some(&digest),
            &[PluginPermission::ConfigRead],
            &config,
            test_limits(),
            allow_all(),
        )
        .await
        .expect("novel channel plugin instantiates"),
    );

    let tampered_dir = tempfile::tempdir().expect("temporary tampered fixture");
    let tampered_path = tampered_dir.path().join("channel-fixture.wasm");
    let mut tampered = std::fs::read(&wasm).expect("read fixture for tampering");
    tampered.push(0);
    std::fs::write(&tampered_path, tampered).expect("write tampered fixture");
    let error = WasmChannel::from_wasm_with_digest(
        "tampered-channel",
        &tampered_path,
        Some(&digest),
        &[PluginPermission::ConfigRead],
        &config,
        test_limits(),
        allow_all(),
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

    assert_eq!(channel.name(), zeroclaw_api::channel::PLUGIN_CHANNEL_TYPE);
    assert_eq!(channel.self_handle().as_deref(), Some("@echo"));
    assert!(channel.health_check().await);
    channel
        .send(&outbound("pong"))
        .await
        .expect("send succeeds");

    let message = first_inbound(&channel).await;
    assert_eq!(message.channel, zeroclaw_api::channel::PLUGIN_CHANNEL_TYPE);
    assert_eq!(message.channel_alias.as_deref(), Some("echo-channel"));
    let echoed: Value = serde_json::from_str(&message.content).expect("echoed config is JSON");
    assert_eq!(echoed.get("greeting").and_then(Value::as_str), Some("hi"));
}

#[tokio::test]
async fn mirror_channel_plugin_receives_plaintext_typed_config() {
    let wasm = fixture();
    let digest = fixture_digest(&wasm);
    let config_json =
        r#"{"bot_token":"secret-123","mention_only":true,"guild_ids":[1,2],"enabled":true}"#;
    let channel = Arc::new(
        WasmChannel::from_wasm_mirror_with_digest(
            "telegram",
            "main",
            &wasm,
            Some(&digest),
            &[PluginPermission::ConfigRead],
            config_json,
            test_limits(),
            allow_all(),
        )
        .await
        .expect("mirror channel plugin instantiates"),
    );

    assert_eq!(channel.name(), "telegram");
    let message = first_inbound(&channel).await;
    assert_eq!(message.channel, "telegram");
    assert_eq!(message.channel_alias.as_deref(), Some("main"));
    let echoed: Value = serde_json::from_str(&message.content).expect("echoed config is JSON");
    assert_eq!(
        echoed.get("bot_token").and_then(Value::as_str),
        Some("secret-123")
    );
    assert_eq!(
        echoed.get("mention_only").and_then(Value::as_bool),
        Some(true)
    );
    assert_eq!(echoed.get("guild_ids"), Some(&serde_json::json!([1, 2])));
}

#[tokio::test]
async fn mirror_without_config_read_is_withheld() {
    let wasm = fixture();
    let digest = fixture_digest(&wasm);
    let channel = Arc::new(
        WasmChannel::from_wasm_mirror_with_digest(
            "telegram",
            "main",
            &wasm,
            Some(&digest),
            &[],
            r#"{"bot_token":"secret-123","enabled":true}"#,
            test_limits(),
            allow_all(),
        )
        .await
        .expect("mirror instantiates with withheld config"),
    );

    let message = first_inbound(&channel).await;
    assert_eq!(message.content, "{}");
    assert_eq!(message.channel, "telegram");
    assert_eq!(message.channel_alias.as_deref(), Some("main"));
}

#[tokio::test]
async fn unauthorized_poll_sender_is_not_forwarded() {
    let wasm = fixture();
    let config: HashMap<String, String> = HashMap::new();
    let deny_fixture_sender: SenderAuthorizer = Arc::new(|sender| sender != "tester");
    let channel = WasmChannel::from_wasm(
        "echo-channel",
        &wasm,
        &[PluginPermission::ConfigRead],
        &config,
        test_limits(),
        deny_fixture_sender,
    )
    .await
    .expect("channel plugin instantiates");

    let (tx, mut rx) = tokio::sync::mpsc::channel(4);
    let listener = ::zeroclaw_spawn::spawn!(async move { channel.listen(tx).await });
    assert!(
        tokio::time::timeout(Duration::from_millis(500), rx.recv())
            .await
            .is_err(),
        "the fixture's denied sender must not reach the agent queue"
    );
    assert!(
        !listener.is_finished(),
        "dropping one denied message must not stop the channel listener"
    );

    listener.abort();
    let err = listener
        .await
        .expect_err("aborting the listener should cancel the polling loop");
    assert!(err.is_cancelled());
}
