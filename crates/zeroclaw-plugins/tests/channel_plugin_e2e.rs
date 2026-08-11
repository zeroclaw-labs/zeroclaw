//! End-to-end fixture for the host's channel-component adapter and scoped secrets.
//!
//! The source fixture is a workspace member and is built on demand into a
//! separate target directory so the nested Cargo invocation cannot contend
//! with the host test process's build lock.

#![cfg(feature = "plugins-wasm-cranelift")]

mod support;

use std::collections::{HashMap, HashSet};
use std::path::PathBuf;
use std::process::Command;
use std::sync::atomic::{AtomicUsize, Ordering};
use std::sync::{Arc, OnceLock, RwLock};
use std::time::Duration;

use zeroclaw_api::attribution::Attributable;
use zeroclaw_api::channel::{Channel, SendMessage};
use zeroclaw_api::webhook::{
    MAX_WEBHOOK_RESPONSE_BODY_BYTES, RawWebhook, WebhookCancellation, WebhookIdempotency,
    WebhookOutcome, WebhookReject,
};
use zeroclaw_plugins::component::{HostInboundMessage, PluginLimits};
use zeroclaw_plugins::config::{PluginConfigResolver, resolve_plugin_config};
use zeroclaw_plugins::endpoint::PluginChannelEndpoint;
use zeroclaw_plugins::instance::PluginInstanceScope;
use zeroclaw_plugins::services::PluginHostServices;
use zeroclaw_plugins::wasm_channel::{SenderAuthorizer, WasmChannel};
use zeroclaw_plugins::{PluginCapability, PluginManifest, PluginPermission};

use support::{admit_fixture, state_service};

fn fixture() -> PathBuf {
    static FIXTURE: OnceLock<PathBuf> = OnceLock::new();
    FIXTURE
        .get_or_init(|| {
            let fixture_dir =
                PathBuf::from(env!("CARGO_MANIFEST_DIR")).join("tests/fixtures/channel-fixture");
            let target_dir =
                PathBuf::from(env!("CARGO_TARGET_TMPDIR")).join("channel-plugin-fixture");
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

fn limits() -> PluginLimits {
    PluginLimits {
        call_fuel: 1_000_000_000,
        max_memory_bytes: 64 * 1024 * 1024,
        max_table_elements: 10_000,
        max_instances: 32,
    }
}

fn manifest() -> PluginManifest {
    toml::from_str(include_str!(
        "fixtures/channel-fixture/plugin-manifest.toml"
    ))
    .expect("parse canonical channel fixture manifest")
}

type InstanceConfig = HashMap<String, String>;
type CanonicalConfig = Arc<RwLock<HashMap<String, InstanceConfig>>>;

fn instance_config(epoch: &str, token: &str) -> InstanceConfig {
    HashMap::from([
        ("retry_count".to_string(), "5".to_string()),
        ("credential_epoch".to_string(), epoch.to_string()),
        ("api_token".to_string(), token.to_string()),
    ])
}

fn canonical_config(binding: &str, epoch: &str, token: &str) -> CanonicalConfig {
    Arc::new(RwLock::new(HashMap::from([(
        binding.to_string(),
        instance_config(epoch, token),
    )])))
}

fn host_services(config: CanonicalConfig) -> PluginHostServices {
    let manifest = manifest();
    let resolver = PluginConfigResolver::new(move |scope| {
        let configured = config.read().expect("lock canonical fixture config");
        let values = configured.get(scope.id().binding()).ok_or_else(|| {
            zeroclaw_plugins::error::PluginError::InvalidConfig(
                "missing canonical fixture binding".to_string(),
            )
        })?;
        resolve_plugin_config(&manifest, scope, Some(values))
    });
    PluginHostServices::new(resolver, state_service(), support::egress_service())
}

async fn build_channel(binding: &str, services: &PluginHostServices) -> WasmChannel {
    build_channel_with_authorizer(binding, services, Arc::new(|_| true)).await
}

async fn build_channel_with_authorizer(
    binding: &str,
    services: &PluginHostServices,
    authorizer: SenderAuthorizer,
) -> WasmChannel {
    let manifest = manifest();
    let scope = PluginInstanceScope::from_manifest(
        &manifest,
        PluginCapability::Channel,
        binding,
        [
            PluginPermission::ConfigRead,
            PluginPermission::StateRead,
            PluginPermission::StateWrite,
        ],
    )
    .expect("admit fixture scope");
    let endpoint = PluginChannelEndpoint::new(scope, "plugin").expect("bind fixture endpoint");
    let component = admit_fixture(&fixture(), &manifest);

    WasmChannel::from_wasm_with_authorizer(endpoint, &component, services, limits(), authorizer)
        .await
        .expect("instantiate fixture channel")
}

async fn channel(binding: &str) -> WasmChannel {
    let config = canonical_config(binding, "v1", &format!("token-{binding}"));
    let services = host_services(config);
    build_channel(binding, &services).await
}

fn outbound(content: &str, recipient: &str) -> SendMessage {
    SendMessage {
        content: content.to_string(),
        recipient: recipient.to_string(),
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
async fn channel_component_runs_through_host_ingress() {
    let channel = channel("main").await;

    assert_eq!(channel.name(), "plugin");
    assert_eq!(channel.alias(), "main");
    assert_eq!(channel.self_handle().as_deref(), Some("@fixture"));
    assert!(channel.health_check().await);
    channel
        .send(&outbound("v1:token-main", "main"))
        .await
        .expect("fixture accepts send");

    let inbound = channel.inbound();
    inbound.enqueue(HostInboundMessage {
        id: "host-1".to_string(),
        sender: "tester".to_string(),
        reply_target: "room".to_string(),
        content: "ping".to_string(),
        channel: "host-channel".to_string(),
        channel_alias: Some("host-alias".to_string()),
        timestamp: 7,
        ..Default::default()
    });

    let (tx, mut rx) = tokio::sync::mpsc::channel(1);
    let listener = zeroclaw_spawn::spawn!(async move { channel.listen(tx).await });
    let message = tokio::time::timeout(Duration::from_secs(5), rx.recv())
        .await
        .expect("fixture message arrives before timeout")
        .expect("listener remains connected");

    assert_eq!(message.id, "host-1");
    assert_eq!(message.content, "ping");
    assert_eq!(message.timestamp, 7);
    assert_eq!(message.channel, "plugin");
    assert_eq!(message.channel_alias.as_deref(), Some("main"));

    assert!(
        !listener.is_finished(),
        "listen must retain ownership of its polling loop"
    );
    listener.abort();
    let error = listener
        .await
        .expect_err("aborting listen must cancel its polling loop");
    assert!(error.is_cancelled());
}

#[tokio::test]
async fn channel_secrets_are_scoped_per_alias_at_point_of_use() {
    let config = Arc::new(RwLock::new(HashMap::from([
        ("main".to_string(), instance_config("v1", "token-main")),
        ("backup".to_string(), instance_config("v1", "token-backup")),
    ])));
    let services = host_services(config);
    let (main, backup) = tokio::join!(
        build_channel("main", &services),
        build_channel("backup", &services)
    );

    main.send(&outbound("v1:token-main", "main"))
        .await
        .expect("main alias reads its own secret");
    backup
        .send(&outbound("v1:token-backup", "backup"))
        .await
        .expect("backup alias reads its own secret");
    assert!(
        main.send(&outbound("v1:token-backup", "main"))
            .await
            .is_err(),
        "main alias must reject the backup secret"
    );
    assert!(
        backup
            .send(&outbound("v1:token-main", "backup"))
            .await
            .is_err(),
        "backup alias must reject the main secret"
    );
}

#[tokio::test]
async fn warm_channel_resolves_one_rotated_config_revision_at_point_of_use() {
    let config = canonical_config("main", "v1", "token-main");
    let services = host_services(Arc::clone(&config));
    let channel = build_channel("main", &services).await;

    channel
        .send(&outbound("v1:token-main", "main"))
        .await
        .expect("channel reads the initial canonical config revision");

    {
        let mut config = config.write().expect("lock canonical fixture config");
        let main = config
            .get_mut("main")
            .expect("main canonical fixture binding");
        main.insert("credential_epoch".to_string(), "v2".to_string());
        main.insert("api_token".to_string(), "rotated-main".to_string());
    }

    assert!(
        channel
            .send(&outbound("v1:token-main", "main"))
            .await
            .is_err(),
        "warm channel must not retain the previous config revision"
    );
    assert!(
        channel
            .send(&outbound("v1:rotated-main", "main"))
            .await
            .is_err(),
        "new secret must not pair with stale public config"
    );
    assert!(
        channel
            .send(&outbound("v2:token-main", "main"))
            .await
            .is_err(),
        "new public config must not pair with the stale secret"
    );
    channel
        .send(&outbound("v2:rotated-main", "main"))
        .await
        .expect("warm channel reads one rotated canonical config revision");
}

#[tokio::test]
async fn channel_listener_stops_when_receiver_closes() {
    let channel = channel("closed").await;
    let (tx, rx) = tokio::sync::mpsc::channel(1);
    let listener = zeroclaw_spawn::spawn!(async move { channel.listen(tx).await });

    drop(rx);
    tokio::time::timeout(Duration::from_secs(1), listener)
        .await
        .expect("listener observes receiver closure")
        .expect("listener task joins")
        .expect("listener exits cleanly");
}

#[tokio::test]
async fn webhook_ingress_authenticates_decodes_and_deduplicates() {
    let config = canonical_config("main", "v1", "token-main");
    let services = host_services(config);
    let channel = Arc::new(build_channel("main", &services).await);
    assert!(channel.has_webhook_ingress());
    assert_eq!(channel.webhook_path().await.as_deref(), Some("fixture"));

    let (sink_tx, sink_rx) = tokio::sync::mpsc::channel::<RawWebhook>(4);
    channel.set_webhook_rx(sink_rx);
    let (tx, mut rx) = tokio::sync::mpsc::channel(4);
    let listener_channel = Arc::clone(&channel);
    let listener = zeroclaw_spawn::spawn!(async move { listener_channel.listen(tx).await });

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
        let (reply, response) = tokio::sync::oneshot::channel();
        sink_tx
            .send(RawWebhook {
                method: "POST".to_string(),
                query: String::new(),
                headers: vec![("x-fixture-secret".to_string(), "token-main".to_string())],
                body: body.to_vec(),
                cancellation: WebhookCancellation::new(),
                idempotency: Some(idempotency.clone()),
                reply,
            })
            .await
            .expect("webhook sink accepts request");
        assert!(matches!(response.await, Ok(Ok(WebhookOutcome::Ack))));
        if expected_delivery {
            let message = tokio::time::timeout(Duration::from_secs(2), rx.recv())
                .await
                .expect("first authenticated message arrives")
                .expect("listener remains active");
            assert_eq!(message.content, "first delivery");
            assert_eq!(message.channel, "plugin");
            assert_eq!(message.channel_alias.as_deref(), Some("main"));
        } else {
            assert!(
                tokio::time::timeout(Duration::from_millis(200), rx.recv())
                    .await
                    .is_err(),
                "stable message ID is delivered only once"
            );
        }
    }

    let (reply, response) = tokio::sync::oneshot::channel();
    sink_tx
        .send(RawWebhook {
            method: "POST".to_string(),
            query: String::new(),
            headers: vec![("x-fixture-secret".to_string(), "wrong".to_string())],
            body: b"nope".to_vec(),
            cancellation: WebhookCancellation::new(),
            idempotency: None,
            reply,
        })
        .await
        .expect("webhook sink accepts invalid signature");
    assert!(matches!(
        response.await,
        Ok(Err(WebhookReject::Unauthorized(_)))
    ));

    let (reply, response) = tokio::sync::oneshot::channel();
    sink_tx
        .send(RawWebhook {
            method: "POST".to_string(),
            query: String::new(),
            headers: vec![("x-fixture-secret".to_string(), "token-main".to_string())],
            body: vec![0xff],
            cancellation: WebhookCancellation::new(),
            idempotency: None,
            reply,
        })
        .await
        .expect("webhook sink accepts malformed body");
    assert!(matches!(
        response.await,
        Ok(Err(WebhookReject::BadRequest(_)))
    ));

    listener.abort();
    assert!(
        listener
            .await
            .expect_err("listener is cancelled")
            .is_cancelled()
    );
}

#[tokio::test]
async fn webhook_challenge_returns_body_without_delivery_or_reservation() {
    let config = canonical_config("main", "v1", "token-main");
    let services = host_services(config);
    let channel = Arc::new(build_channel("main", &services).await);
    let (sink_tx, sink_rx) = tokio::sync::mpsc::channel::<RawWebhook>(2);
    channel.set_webhook_rx(sink_rx);
    let (tx, mut rx) = tokio::sync::mpsc::channel(2);
    let listener_channel = Arc::clone(&channel);
    let listener = zeroclaw_spawn::spawn!(async move { listener_channel.listen(tx).await });

    let reservations = Arc::new(AtomicUsize::new(0));
    let reserve_count = Arc::clone(&reservations);
    let idempotency = WebhookIdempotency::new(
        move |_| {
            reserve_count.fetch_add(1, Ordering::SeqCst);
            true
        },
        |_| {},
    );
    let (reply, response) = tokio::sync::oneshot::channel();
    sink_tx
        .send(RawWebhook {
            method: "GET".to_string(),
            query: "hub.mode=subscribe&challenge=echo-me-42".to_string(),
            headers: Vec::new(),
            body: Vec::new(),
            cancellation: WebhookCancellation::new(),
            idempotency: Some(idempotency),
            reply,
        })
        .await
        .expect("webhook sink accepts verification request");

    assert!(matches!(response.await, Ok(Ok(WebhookOutcome::Body(body))) if body == "echo-me-42"));
    assert_eq!(reservations.load(Ordering::SeqCst), 0);
    assert!(
        tokio::time::timeout(Duration::from_millis(200), rx.recv())
            .await
            .is_err(),
        "a response-only challenge must not reach the agent queue"
    );

    listener.abort();
    assert!(
        listener
            .await
            .expect_err("listener is cancelled")
            .is_cancelled()
    );
}

#[tokio::test]
async fn oversized_webhook_challenge_is_rejected_before_host_clone() {
    let config = canonical_config("main", "v1", "token-main");
    let services = host_services(config);
    let channel = Arc::new(build_channel("main", &services).await);
    let (sink_tx, sink_rx) = tokio::sync::mpsc::channel::<RawWebhook>(2);
    channel.set_webhook_rx(sink_rx);
    let (tx, mut rx) = tokio::sync::mpsc::channel(2);
    let listener_channel = Arc::clone(&channel);
    let listener = zeroclaw_spawn::spawn!(async move { listener_channel.listen(tx).await });

    let oversized = "x".repeat(MAX_WEBHOOK_RESPONSE_BODY_BYTES + 1);
    let (reply, response) = tokio::sync::oneshot::channel();
    sink_tx
        .send(RawWebhook {
            method: "GET".to_string(),
            query: format!("challenge={oversized}"),
            headers: Vec::new(),
            body: Vec::new(),
            cancellation: WebhookCancellation::new(),
            idempotency: None,
            reply,
        })
        .await
        .expect("webhook sink accepts oversized verification response");

    assert!(matches!(
        response.await,
        Ok(Err(WebhookReject::InvalidResponse))
    ));
    assert!(
        tokio::time::timeout(Duration::from_millis(200), rx.recv())
            .await
            .is_err(),
        "an oversized response-only challenge must not reach the agent queue"
    );

    listener.abort();
    assert!(
        listener
            .await
            .expect_err("listener is cancelled")
            .is_cancelled()
    );
}

#[tokio::test]
async fn webhook_sender_policy_precedes_idempotency_reservation() {
    let config = canonical_config("main", "v1", "token-main");
    let services = host_services(config);
    let channel = Arc::new(
        build_channel_with_authorizer("main", &services, Arc::new(|sender| sender != "webhook"))
            .await,
    );
    let (sink_tx, sink_rx) = tokio::sync::mpsc::channel::<RawWebhook>(2);
    channel.set_webhook_rx(sink_rx);
    let (tx, mut rx) = tokio::sync::mpsc::channel(2);
    let listener_channel = Arc::clone(&channel);
    let listener = zeroclaw_spawn::spawn!(async move { listener_channel.listen(tx).await });
    let reservations = Arc::new(AtomicUsize::new(0));
    let reserve_count = Arc::clone(&reservations);
    let idempotency = WebhookIdempotency::new(
        move |_| {
            reserve_count.fetch_add(1, Ordering::SeqCst);
            true
        },
        |_| {},
    );

    let (reply, response) = tokio::sync::oneshot::channel();
    sink_tx
        .send(RawWebhook {
            method: "POST".to_string(),
            query: String::new(),
            headers: vec![("x-fixture-secret".to_string(), "token-main".to_string())],
            body: b"denied".to_vec(),
            cancellation: WebhookCancellation::new(),
            idempotency: Some(idempotency),
            reply,
        })
        .await
        .expect("webhook sink accepts request");
    assert!(matches!(response.await, Ok(Ok(WebhookOutcome::Ack))));
    assert_eq!(reservations.load(Ordering::SeqCst), 0);
    assert!(
        tokio::time::timeout(Duration::from_millis(200), rx.recv())
            .await
            .is_err()
    );

    listener.abort();
    assert!(
        listener
            .await
            .expect_err("listener is cancelled")
            .is_cancelled()
    );
}

#[tokio::test]
async fn cancelled_webhook_parse_leaves_warm_channel_usable() {
    let config = canonical_config("main", "v1", "token-main");
    let services = host_services(config);
    let channel = Arc::new(build_channel("main", &services).await);
    let (sink_tx, sink_rx) = tokio::sync::mpsc::channel::<RawWebhook>(2);
    channel.set_webhook_rx(sink_rx);
    let (tx, _rx) = tokio::sync::mpsc::channel(2);
    let listener_channel = Arc::clone(&channel);
    let listener = zeroclaw_spawn::spawn!(async move { listener_channel.listen(tx).await });

    let cancellation = WebhookCancellation::new();
    let cancel = cancellation.clone();
    let (reply, response) = tokio::sync::oneshot::channel();
    sink_tx
        .send(RawWebhook {
            method: "POST".to_string(),
            query: String::new(),
            headers: vec![("x-fixture-secret".to_string(), "token-main".to_string())],
            body: b"stall-parse".to_vec(),
            cancellation,
            idempotency: None,
            reply,
        })
        .await
        .expect("webhook sink accepts stalled parse");
    zeroclaw_spawn::spawn!(async move {
        tokio::time::sleep(Duration::from_millis(100)).await;
        cancel.cancel();
    });
    assert!(matches!(
        tokio::time::timeout(Duration::from_secs(3), response).await,
        Ok(Ok(Err(WebhookReject::Timeout)))
    ));

    tokio::time::timeout(
        Duration::from_secs(2),
        channel.send(&outbound("v1:token-main", "main")),
    )
    .await
    .expect("warm store is not held by cancelled parser")
    .expect("warm channel remains usable");

    listener.abort();
    assert!(
        listener
            .await
            .expect_err("listener is cancelled")
            .is_cancelled()
    );
}
