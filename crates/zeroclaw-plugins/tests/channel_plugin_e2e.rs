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

use std::collections::{HashMap, HashSet};
use std::path::{Path, PathBuf};
use std::process::Command;
use std::sync::atomic::{AtomicUsize, Ordering};
use std::sync::{Arc, OnceLock};
use std::time::Duration;

use serde_json::Value;
use zeroclaw_api::channel::{Channel, ChannelMessage, SendMessage};
use zeroclaw_api::webhook::{RawWebhook, WebhookCancellation, WebhookIdempotency, WebhookReject};
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

#[tokio::test]
async fn webhook_ingress_delivers_inbound() {
    let wasm = fixture();
    let digest = fixture_digest(&wasm);
    let resolver_calls = Arc::new(AtomicUsize::new(0));
    let resolver_counter = Arc::clone(&resolver_calls);
    let runtime = Arc::new(move || {
        let call = resolver_counter.fetch_add(1, Ordering::SeqCst);
        let secret = if call == 0 {
            "startup-secret"
        } else {
            "test-secret"
        };
        Ok((secret.to_string(), test_limits()))
    }) as zeroclaw_plugins::wasm_channel::ChannelRuntimeResolver;

    // The warm instance receives `startup-secret`; each disposable parser
    // resolves `test-secret`. A valid request therefore proves runtime config
    // is materialized per request rather than cached at channel startup.
    let channel = Arc::new(
        WasmChannel::from_wasm_mirror_with_runtime_resolver_and_digest(
            "fixture",
            "default",
            &wasm,
            Some(&digest),
            &[PluginPermission::ConfigRead],
            runtime,
            allow_all(),
        )
        .await
        .expect("fixture instantiates"),
    );
    assert_eq!(channel.webhook_path().await.as_deref(), Some("fixture"));

    // Register the sink drain end and run the owned listener while feeding raw
    // webhooks as the gateway would.
    let (sink_tx, sink_rx) = tokio::sync::mpsc::channel::<RawWebhook>(4);
    channel.set_webhook_rx(sink_rx);
    let (tx, mut rx) = tokio::sync::mpsc::channel(4);
    let listener_channel = Arc::clone(&channel);
    let listener = ::zeroclaw_spawn::spawn!(async move { listener_channel.listen(tx).await });

    // Valid signature → the body is decoded into an inbound message and the
    // reply resolves Ok.
    let (reply_tx, reply_rx) = tokio::sync::oneshot::channel();
    sink_tx
        .send(RawWebhook {
            headers: vec![("x-fixture-secret".to_string(), "test-secret".to_string())],
            body: b"hello from webhook".to_vec(),
            cancellation: WebhookCancellation::new(),
            idempotency: None,
            reply: reply_tx,
        })
        .await
        .expect("sink accepts");
    assert!(
        matches!(reply_rx.await, Ok(Ok(()))),
        "valid webhook → reply Ok"
    );
    assert!(
        resolver_calls.load(Ordering::SeqCst) >= 2,
        "webhook parser resolves config after warm startup"
    );

    // Both the fixture's one-shot config echo (poll) and the webhook message
    // arrive on `tx`; order is not guaranteed, so assert the webhook body is
    // among the delivered messages.
    let mut delivered = Vec::new();
    for _ in 0..2 {
        if let Ok(Some(m)) = tokio::time::timeout(Duration::from_secs(5), rx.recv()).await {
            delivered.push(m.content);
        }
    }
    assert!(
        delivered.iter().any(|c| c == "hello from webhook"),
        "webhook body delivered as inbound: {delivered:?}"
    );

    // Wrong signature → the plugin rejects it: reply Err(Unauthorized).
    let (reply_tx2, reply_rx2) = tokio::sync::oneshot::channel();
    sink_tx
        .send(RawWebhook {
            headers: vec![("x-fixture-secret".to_string(), "wrong".to_string())],
            body: b"nope".to_vec(),
            cancellation: WebhookCancellation::new(),
            idempotency: None,
            reply: reply_tx2,
        })
        .await
        .expect("sink accepts");
    assert!(
        matches!(reply_rx2.await, Ok(Err(WebhookReject::Unauthorized(_)))),
        "bad signature → Unauthorized"
    );

    // A valid signature with a malformed payload is a distinct 400-class
    // rejection in the canonical WIT contract.
    let (reply_tx3, reply_rx3) = tokio::sync::oneshot::channel();
    sink_tx
        .send(RawWebhook {
            headers: vec![("x-fixture-secret".to_string(), "test-secret".to_string())],
            body: vec![0xff],
            cancellation: WebhookCancellation::new(),
            idempotency: None,
            reply: reply_tx3,
        })
        .await
        .expect("sink accepts malformed payload");
    assert!(
        matches!(reply_rx3.await, Ok(Err(WebhookReject::BadRequest(_)))),
        "malformed payload → BadRequest"
    );

    listener.abort();
    assert!(
        listener
            .await
            .expect_err("listener cancellation returns a join error")
            .is_cancelled()
    );
}

#[tokio::test]
async fn cancelled_webhook_parse_releases_warm_store_for_next_call() {
    let wasm = fixture();

    let channel = Arc::new(
        WasmChannel::from_wasm_mirror(
            "fixture",
            "default",
            &wasm,
            &[PluginPermission::ConfigRead],
            "test-secret",
            test_limits(),
            allow_all(),
        )
        .await
        .expect("fixture instantiates"),
    );
    let (sink_tx, sink_rx) = tokio::sync::mpsc::channel::<RawWebhook>(4);
    channel.set_webhook_rx(sink_rx);
    let (tx, mut rx) = tokio::sync::mpsc::channel(4);
    let listener_channel = Arc::clone(&channel);
    let listener = ::zeroclaw_spawn::spawn!(async move { listener_channel.listen(tx).await });

    let cancellation = WebhookCancellation::new();
    let cancel_from_deadline = cancellation.clone();
    let (stalled_reply_tx, stalled_reply_rx) = tokio::sync::oneshot::channel();
    sink_tx
        .send(RawWebhook {
            headers: vec![("x-fixture-secret".to_string(), "test-secret".to_string())],
            body: b"stall-parse".to_vec(),
            cancellation,
            idempotency: None,
            reply: stalled_reply_tx,
        })
        .await
        .expect("sink accepts stalled parse");
    zeroclaw_spawn::spawn!(async move {
        tokio::time::sleep(Duration::from_millis(100)).await;
        cancel_from_deadline.cancel();
    });
    let stalled = tokio::time::timeout(Duration::from_secs(3), stalled_reply_rx)
        .await
        .expect("cancelled parse resolves promptly")
        .expect("webhook worker keeps reply channel");
    assert!(matches!(stalled, Err(WebhookReject::Timeout)));

    tokio::time::timeout(
        Duration::from_secs(2),
        channel.send(&outbound("store-probe")),
    )
    .await
    .expect("store mutex is released after cancellation")
    .expect("same warm component remains callable after cancellation");

    let (recovery_reply_tx, recovery_reply_rx) = tokio::sync::oneshot::channel();
    sink_tx
        .send(RawWebhook {
            headers: vec![("x-fixture-secret".to_string(), "test-secret".to_string())],
            body: b"after-timeout".to_vec(),
            cancellation: WebhookCancellation::new(),
            idempotency: None,
            reply: recovery_reply_tx,
        })
        .await
        .expect("sink accepts recovery parse");
    assert!(
        matches!(
            tokio::time::timeout(Duration::from_secs(2), recovery_reply_rx).await,
            Ok(Ok(Ok(())))
        ),
        "later webhook succeeds on the same channel instance"
    );

    let mut delivered = Vec::new();
    for _ in 0..2 {
        if let Ok(Some(message)) = tokio::time::timeout(Duration::from_secs(2), rx.recv()).await {
            delivered.push(message.content);
        }
    }
    assert!(
        delivered.iter().any(|content| content == "after-timeout"),
        "recovered parse reaches normal inbound delivery: {delivered:?}"
    );

    tokio::time::timeout(Duration::from_secs(2), async {
        while !channel.health_check().await {
            tokio::time::sleep(Duration::from_millis(25)).await;
        }
    })
    .await
    .expect("poll health recovers after the cancelled parse");

    listener.abort();
    assert!(
        listener
            .await
            .expect_err("listener cancellation returns a join error")
            .is_cancelled()
    );
}

#[tokio::test]
async fn authenticated_webhook_message_ids_are_idempotent() {
    let wasm = fixture();
    let channel = Arc::new(
        WasmChannel::from_wasm_mirror(
            "fixture",
            "default",
            &wasm,
            &[PluginPermission::ConfigRead],
            "test-secret",
            test_limits(),
        )
        .await
        .expect("fixture instantiates"),
    );
    let (sink_tx, sink_rx) = tokio::sync::mpsc::channel::<RawWebhook>(4);
    channel.set_webhook_rx(sink_rx);
    let (tx, mut rx) = tokio::sync::mpsc::channel(4);
    let listener_channel = Arc::clone(&channel);
    let listener = ::zeroclaw_spawn::spawn!(async move { listener_channel.listen(tx).await });

    // Drain the fixture's one-shot configure echo before webhook assertions.
    tokio::time::timeout(Duration::from_secs(2), rx.recv())
        .await
        .expect("fixture emits configure echo")
        .expect("listener remains active");

    let keys = Arc::new(std::sync::Mutex::new(HashSet::<String>::new()));
    let reserve_keys = Arc::clone(&keys);
    let rollback_keys = Arc::clone(&keys);
    let idempotency = WebhookIdempotency::new(
        move |message_id| {
            reserve_keys
                .lock()
                .unwrap_or_else(std::sync::PoisonError::into_inner)
                .insert(message_id.to_string())
        },
        move |message_id| {
            rollback_keys
                .lock()
                .unwrap_or_else(std::sync::PoisonError::into_inner)
                .remove(message_id);
        },
    );

    for (body, expected_delivery) in [
        (b"first delivery".as_slice(), true),
        (b"provider retry".as_slice(), false),
    ] {
        let (reply_tx, reply_rx) = tokio::sync::oneshot::channel();
        sink_tx
            .send(RawWebhook {
                headers: vec![("x-fixture-secret".to_string(), "test-secret".to_string())],
                body: body.to_vec(),
                cancellation: WebhookCancellation::new(),
                idempotency: Some(idempotency.clone()),
                reply: reply_tx,
            })
            .await
            .expect("sink accepts authenticated webhook");
        assert!(
            matches!(reply_rx.await, Ok(Ok(()))),
            "valid delivery and an authenticated retry are both acknowledged"
        );
        if expected_delivery {
            let message = tokio::time::timeout(Duration::from_secs(2), rx.recv())
                .await
                .expect("first stable message ID is delivered")
                .expect("listener remains active");
            assert_eq!(message.content, "first delivery");
        } else {
            assert!(
                tokio::time::timeout(Duration::from_millis(200), rx.recv())
                    .await
                    .is_err(),
                "a retry with the same parsed stable ID is not delivered twice"
            );
        }
    }

    listener.abort();
    assert!(
        listener
            .await
            .expect_err("listener cancellation returns a join error")
            .is_cancelled()
    );
}
