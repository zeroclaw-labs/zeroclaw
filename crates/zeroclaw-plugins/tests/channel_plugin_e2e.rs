//! End-to-end fixture for the host's channel-component adapter and scoped secrets.
//!
//! The source fixture is a workspace member and is built on demand into a
//! separate target directory so the nested Cargo invocation cannot contend
//! with the host test process's build lock.

#![cfg(feature = "plugins-wasm-cranelift")]

mod support;

use std::collections::HashMap;
use std::path::PathBuf;
use std::process::Command;
use std::sync::atomic::{AtomicUsize, Ordering};
use std::sync::{Arc, OnceLock, RwLock};
use std::time::Duration;

use zeroclaw_api::attribution::Attributable;
use zeroclaw_api::channel::{Channel, ListenerHealth, SendMessage};
use zeroclaw_api::webhook::{
    MAX_WEBHOOK_RESPONSE_BODY_BYTES, RawWebhook, WebhookIdempotency, WebhookOutcome, WebhookReject,
};
use zeroclaw_plugins::component::{HostInboundMessage, PluginLimits};
use zeroclaw_plugins::config::{PluginConfigResolver, resolve_plugin_config};
use zeroclaw_plugins::endpoint::PluginChannelEndpoint;
use zeroclaw_plugins::instance::PluginInstanceScope;
use zeroclaw_plugins::services::PluginHostServices;
use zeroclaw_plugins::wasm_channel::WasmChannel;
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
    limits_with_timeout(Duration::from_secs(30))
}

fn limits_with_timeout(call_timeout: Duration) -> PluginLimits {
    limits_with(1_000_000_000, call_timeout)
}

fn limits_with(call_fuel: u64, call_timeout: Duration) -> PluginLimits {
    PluginLimits {
        call_fuel,
        max_memory_bytes: 64 * 1024 * 1024,
        max_table_elements: 10_000,
        max_instances: 32,
        call_timeout,
    }
}

fn manifest() -> PluginManifest {
    PluginManifest {
        name: "channel-fixture".to_string(),
        version: "0.0.0".to_string(),
        description: None,
        author: None,
        wasm_path: Some("channel-fixture.wasm".to_string()),
        wasm_sha256: None,
        capabilities: vec![PluginCapability::Channel],
        provides: None,
        // Every fixture channel is ConfigRead-granted so the typed-config and
        // scoped-secret contract is exercised on every instantiation, and
        // State-granted so the durable-state contract is too. The HttpClient
        // grant attaches the governed `wasi:http` surface (see
        // `new_channel_store`), but these channels are constructed with no egress
        // policy (`from_wasm(.., None)`), so reach is deny-all — no destination
        // is reachable. The deadline tests drive guest compute (a `spin`
        // message), not network, so a linked-but-ungoverned surface does not
        // change what they measure.
        permissions: vec![
            PluginPermission::ConfigRead,
            PluginPermission::HttpClient,
            PluginPermission::StateRead,
            PluginPermission::StateWrite,
        ],
        config_schema: Some(serde_json::json!({
            "$schema": "https://json-schema.org/draft/2020-12/schema",
            "type": "object",
            "required": ["retry_count", "credential_epoch", "api_token"],
            "additionalProperties": false,
            "properties": {
                "retry_count": {"type": "integer", "minimum": 1},
                "credential_epoch": {"type": "string", "minLength": 1},
                "api_token": {"type": "string", "minLength": 1, "x-secret": true},
                "handle": {"type": "string"}
            }
        })),
        signature: None,
        publisher_key: None,
        egress: Default::default(),
    }
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
    PluginHostServices::new(resolver, state_service())
}

async fn build_channel(binding: &str, services: &PluginHostServices) -> WasmChannel {
    let manifest = manifest();
    let scope = PluginInstanceScope::from_manifest(
        &manifest,
        PluginCapability::Channel,
        binding,
        manifest.permissions.iter().copied(),
    )
    .expect("admit fixture scope");
    let endpoint = PluginChannelEndpoint::new(scope, "plugin").expect("bind fixture endpoint");

    let component = admit_fixture(&fixture(), &manifest);
    WasmChannel::from_wasm(endpoint, &component, services, limits(), None)
        .await
        .expect("instantiate fixture channel")
}

async fn channel(binding: &str) -> WasmChannel {
    let config = canonical_config(binding, "v1", &format!("token-{binding}"));
    let services = host_services(config);
    build_channel(binding, &services).await
}

/// Build a channel whose canonical config additionally carries the caller's
/// `extra` non-secret entries (e.g. a `handle`), under a custom limits budget.
/// Every fixture instance is ConfigRead- and HttpClient-granted through the
/// shared manifest, so `permissions` only documents the case's intent.
async fn channel_with(
    binding: &str,
    _permissions: Vec<PluginPermission>,
    extra: &HashMap<String, String>,
    limits: PluginLimits,
) -> WasmChannel {
    let mut values = instance_config("v1", &format!("token-{binding}"));
    values.extend(extra.iter().map(|(k, v)| (k.clone(), v.clone())));
    let config: CanonicalConfig =
        Arc::new(RwLock::new(HashMap::from([(binding.to_string(), values)])));
    let services = host_services(config);
    let manifest = manifest();
    let scope = PluginInstanceScope::from_manifest(
        &manifest,
        PluginCapability::Channel,
        binding,
        manifest.permissions.iter().copied(),
    )
    .expect("admit fixture scope");
    let endpoint = PluginChannelEndpoint::new(scope, "plugin").expect("bind fixture endpoint");
    let component = admit_fixture(&fixture(), &manifest);
    WasmChannel::from_wasm(endpoint, &component, &services, limits, None)
        .await
        .expect("instantiate fixture channel")
}

async fn channel_with_timeout(binding: &str, timeout: Duration) -> WasmChannel {
    // u64::MAX fuel guarantees the wall-clock deadline, not fuel exhaustion,
    // interrupts a spinning guest send — the store-discard path under test.
    channel_with(
        binding,
        vec![PluginPermission::HttpClient],
        &HashMap::new(),
        limits_with(u64::MAX, timeout),
    )
    .await
}

/// A host-enqueued inbound message whose content the fixture interprets.
fn queued(id: &str, content: &str) -> HostInboundMessage {
    HostInboundMessage {
        id: id.to_string(),
        sender: "tester".to_string(),
        reply_target: "room".to_string(),
        content: content.to_string(),
        channel: "host-channel".to_string(),
        timestamp: 1,
        ..Default::default()
    }
}

/// Read `listener_health` the way the channel supervisor does, as time passes,
/// until it reports `expected` or `within` runs out.
async fn wait_for_listener_health(
    channel: &WasmChannel,
    expected: ListenerHealth,
    within: Duration,
) {
    let deadline = tokio::time::Instant::now() + within;
    while channel.listener_health() != Some(expected) {
        assert!(
            tokio::time::Instant::now() < deadline,
            "listener health stayed {:?} instead of {expected:?}",
            channel.listener_health()
        );
        tokio::time::sleep(Duration::from_millis(50)).await;
    }
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
        references: Vec::new(),
        suppress_voice: false,
        force_voice: false,
    }
}

fn fixture_webhook(
    body: impl Into<Vec<u8>>,
    secret: &str,
    cancellation: zeroclaw_api::webhook::WebhookCancellation,
    idempotency: Option<WebhookIdempotency>,
) -> (
    RawWebhook,
    tokio::sync::oneshot::Receiver<Result<WebhookOutcome, WebhookReject>>,
) {
    let (reply, outcome) = tokio::sync::oneshot::channel();
    (
        RawWebhook {
            method: "POST".to_string(),
            query: String::new(),
            headers: vec![("x-fixture-secret".to_string(), secret.to_string())],
            body: body.into(),
            cancellation,
            idempotency,
            reply,
        },
        outcome,
    )
}

#[tokio::test]
async fn channel_component_runs_through_host_ingress() {
    let channel = channel("main")
        .await
        .with_sender_authorizer(Arc::new(|_| true));

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
async fn poll_ingress_applies_the_host_sender_policy_before_delivery() {
    let channel = channel("main")
        .await
        .with_sender_authorizer(Arc::new(|sender| sender == "allowed"));
    let inbound = channel.inbound();
    let message = |id: &str, sender: &str| HostInboundMessage {
        id: id.to_string(),
        sender: sender.to_string(),
        reply_target: "room".to_string(),
        content: id.to_string(),
        channel: "guest-channel".to_string(),
        timestamp: 7,
        ..Default::default()
    };
    inbound.enqueue(message("blocked-1", "blocked"));
    inbound.enqueue(message("allowed-1", "allowed"));

    let (tx, mut receiver) = tokio::sync::mpsc::channel(2);
    let listener = zeroclaw_spawn::spawn!(async move { channel.listen(tx).await });
    let delivered = tokio::time::timeout(Duration::from_secs(5), receiver.recv())
        .await
        .expect("authorized poll message arrives")
        .expect("listener remains connected");
    assert_eq!(delivered.id, "allowed-1");
    assert!(receiver.try_recv().is_err());

    listener.abort();
}

#[tokio::test]
async fn channel_webhook_component_authenticates_parses_and_host_stamps() {
    let channel = channel("main")
        .await
        .with_sender_authorizer(Arc::new(|_| true));
    assert!(channel.has_webhook_ingress());
    assert_eq!(
        channel.webhook_path().await.expect("query webhook path"),
        Some("fixture".to_string())
    );
    let (sink, receiver) = tokio::sync::mpsc::channel(4);
    channel.set_webhook_receiver(receiver);
    let (tx, mut inbound) = tokio::sync::mpsc::channel(2);
    let listener = zeroclaw_spawn::spawn!(async move { channel.listen(tx).await });

    let (unauthorized, unauthorized_outcome) = fixture_webhook(
        br#"{"id":"bad","sender":"tester","reply_target":"room","content":"ignored"}"#,
        "wrong-token",
        zeroclaw_api::webhook::WebhookCancellation::new(),
        None,
    );
    sink.send(unauthorized)
        .await
        .expect("webhook queue is open");
    assert!(matches!(
        unauthorized_outcome.await.expect("worker replies"),
        Err(WebhookReject::Unauthorized(detail)) if detail.contains("private signature")
    ));
    assert!(inbound.try_recv().is_err());

    let (malformed, malformed_outcome) = fixture_webhook(
        b"not-json".to_vec(),
        "token-main",
        zeroclaw_api::webhook::WebhookCancellation::new(),
        None,
    );
    sink.send(malformed).await.expect("webhook queue is open");
    assert!(matches!(
        malformed_outcome.await.expect("worker replies"),
        Err(WebhookReject::BadRequest(detail)) if detail.contains("private parser detail")
    ));
    assert!(inbound.try_recv().is_err());

    let (valid, valid_outcome) = fixture_webhook(
        br#"{"id":"event-1","sender":"tester","reply_target":"room","content":"hello"}"#,
        "token-main",
        zeroclaw_api::webhook::WebhookCancellation::new(),
        None,
    );
    sink.send(valid).await.expect("webhook queue is open");
    assert!(valid_outcome.await.expect("worker replies").is_ok());
    let message = tokio::time::timeout(Duration::from_secs(5), inbound.recv())
        .await
        .expect("decoded message arrives")
        .expect("listener remains connected");
    assert_eq!(message.id, "event-1");
    assert_eq!(message.sender, "tester");
    assert_eq!(message.channel, "plugin");
    assert_eq!(message.channel_alias.as_deref(), Some("main"));

    listener.abort();
}

#[tokio::test]
async fn cancelled_webhook_drops_disposable_parser_and_later_request_recovers() {
    let channel = channel("main")
        .await
        .with_sender_authorizer(Arc::new(|_| true));
    let (sink, receiver) = tokio::sync::mpsc::channel(4);
    channel.set_webhook_receiver(receiver);
    let (tx, mut inbound) = tokio::sync::mpsc::channel(2);
    let listener = zeroclaw_spawn::spawn!(async move { channel.listen(tx).await });

    let cancellation = zeroclaw_api::webhook::WebhookCancellation::new();
    let (spinning, spinning_outcome) =
        fixture_webhook(b"spin".to_vec(), "token-main", cancellation.clone(), None);
    sink.send(spinning).await.expect("webhook queue is open");
    tokio::time::sleep(Duration::from_millis(50)).await;
    cancellation.cancel();
    assert!(matches!(
        tokio::time::timeout(Duration::from_secs(5), spinning_outcome)
            .await
            .expect("cancelled parser returns")
            .expect("worker replies"),
        Err(WebhookReject::Timeout)
    ));

    let (valid, valid_outcome) = fixture_webhook(
        br#"{"id":"event-2","sender":"tester","reply_target":"room","content":"recovered"}"#,
        "token-main",
        zeroclaw_api::webhook::WebhookCancellation::new(),
        None,
    );
    sink.send(valid).await.expect("webhook queue is open");
    assert!(
        tokio::time::timeout(Duration::from_secs(5), valid_outcome)
            .await
            .expect("replacement parser completes")
            .expect("worker replies")
            .is_ok()
    );
    let message = tokio::time::timeout(Duration::from_secs(5), inbound.recv())
        .await
        .expect("replacement delivery arrives")
        .expect("listener remains connected");
    assert_eq!(message.id, "event-2");
    assert_eq!(message.content, "recovered");

    listener.abort();
}

#[tokio::test]
async fn host_config_failure_is_unavailable_and_a_later_webhook_recovers() {
    let config = canonical_config("main", "v1", "token-main");
    let services = host_services(Arc::clone(&config));
    let channel = build_channel("main", &services)
        .await
        .with_sender_authorizer(Arc::new(|_| true));
    let (sink, receiver) = tokio::sync::mpsc::channel(2);
    channel.set_webhook_receiver(receiver);
    let (tx, mut inbound) = tokio::sync::mpsc::channel(1);
    let listener = zeroclaw_spawn::spawn!(async move { channel.listen(tx).await });

    config
        .write()
        .expect("lock canonical fixture config")
        .remove("main");
    let (unavailable, unavailable_outcome) = fixture_webhook(
        br#"{"id":"unavailable-1","sender":"tester","reply_target":"room","content":"ignored"}"#,
        "token-main",
        zeroclaw_api::webhook::WebhookCancellation::new(),
        None,
    );
    sink.send(unavailable).await.expect("webhook queue is open");
    assert!(matches!(
        unavailable_outcome.await.expect("worker replies"),
        Err(WebhookReject::Unavailable(_))
    ));
    assert!(inbound.try_recv().is_err());

    config
        .write()
        .expect("lock canonical fixture config")
        .insert("main".to_string(), instance_config("v1", "token-main"));
    let (valid, valid_outcome) = fixture_webhook(
        br#"{"id":"recovered-1","sender":"tester","reply_target":"room","content":"recovered"}"#,
        "token-main",
        zeroclaw_api::webhook::WebhookCancellation::new(),
        None,
    );
    sink.send(valid).await.expect("webhook queue is open");
    assert!(valid_outcome.await.expect("worker replies").is_ok());
    let delivered = tokio::time::timeout(Duration::from_secs(5), inbound.recv())
        .await
        .expect("recovered webhook arrives")
        .expect("listener remains connected");
    assert_eq!(delivered.id, "recovered-1");

    listener.abort();
}

#[tokio::test]
async fn webhook_sender_policy_runs_before_idempotency_reservation() {
    let channel = channel("main")
        .await
        .with_sender_authorizer(Arc::new(|sender| sender == "allowed"));
    let (sink, receiver) = tokio::sync::mpsc::channel(2);
    channel.set_webhook_receiver(receiver);
    let (tx, mut inbound) = tokio::sync::mpsc::channel(1);
    let listener = zeroclaw_spawn::spawn!(async move { channel.listen(tx).await });
    let begin_calls = Arc::new(AtomicUsize::new(0));
    let observed_calls = Arc::clone(&begin_calls);
    let idempotency = WebhookIdempotency::new(
        move |_| {
            observed_calls.fetch_add(1, Ordering::SeqCst);
            zeroclaw_api::webhook::WebhookReservation::Unavailable
        },
        |_| false,
        |_| false,
    );
    let (blocked, outcome) = fixture_webhook(
        br#"{"id":"blocked-1","sender":"blocked","reply_target":"room","content":"ignored"}"#,
        "token-main",
        zeroclaw_api::webhook::WebhookCancellation::new(),
        Some(idempotency),
    );
    sink.send(blocked).await.expect("webhook queue is open");
    assert!(outcome.await.expect("worker replies").is_ok());
    assert_eq!(begin_calls.load(Ordering::SeqCst), 0);
    assert!(
        tokio::time::timeout(Duration::from_millis(200), inbound.recv())
            .await
            .is_err(),
        "unauthorized sender must not reach the channel queue"
    );

    listener.abort();
}

#[tokio::test]
async fn typed_webhook_replies_bypass_message_policy_and_bound_utf8_output() {
    let policy_calls = Arc::new(AtomicUsize::new(0));
    let observed_policy = Arc::clone(&policy_calls);
    let channel = channel("main")
        .await
        .with_sender_authorizer(Arc::new(move |_| {
            observed_policy.fetch_add(1, Ordering::SeqCst);
            true
        }));
    let (sink, receiver) = tokio::sync::mpsc::channel(2);
    channel.set_webhook_receiver(receiver);
    let (tx, mut inbound) = tokio::sync::mpsc::channel(1);
    let listener = zeroclaw_spawn::spawn!(async move { channel.listen(tx).await });
    let idempotency = WebhookIdempotency::new(
        |_| panic!("challenge must not reserve a message ID"),
        |_| panic!("challenge must not commit a message ID"),
        |_| panic!("challenge must not roll back a message ID"),
    );

    for (method, text) in [
        ("GET", "challenge=a%2Bb&part=one&part=two".to_string()),
        ("POST", "λ".repeat(MAX_WEBHOOK_RESPONSE_BODY_BYTES / 2)),
        ("POST", "λ".repeat(MAX_WEBHOOK_RESPONSE_BODY_BYTES / 2 + 1)),
    ] {
        let body =
            serde_json::to_vec(&serde_json::json!({"challenge": text})).expect("encode challenge");
        let (mut request, outcome) = fixture_webhook(
            body,
            "token-main",
            zeroclaw_api::webhook::WebhookCancellation::new(),
            Some(idempotency.clone()),
        );
        request.method = method.to_string();
        request.query = text.clone();
        request.headers.extend([
            ("x-webhook-method".to_string(), "DELETE".to_string()),
            ("x-webhook-query".to_string(), "spoofed".to_string()),
        ]);
        sink.send(request).await.expect("webhook receiver active");
        let result = outcome.await.expect("guest replies");
        if text.len() <= MAX_WEBHOOK_RESPONSE_BODY_BYTES {
            assert!(matches!(result, Ok(WebhookOutcome::Body(body)) if body == text));
        } else {
            assert!(matches!(result, Err(WebhookReject::InvalidResponse)));
        }
        assert!(inbound.try_recv().is_err());
        assert_eq!(policy_calls.load(Ordering::SeqCst), 0);
    }

    let (mut request, outcome) = fixture_webhook(
        b"",
        "wrong-token",
        zeroclaw_api::webhook::WebhookCancellation::new(),
        Some(idempotency),
    );
    request.method = "GET".to_string();
    request.query = "challenge=unauthenticated".to_string();
    sink.send(request).await.expect("webhook receiver active");
    assert!(matches!(
        outcome.await.expect("guest rejects"),
        Err(WebhookReject::Unauthorized(_))
    ));
    assert!(inbound.try_recv().is_err());

    let (request, outcome) = fixture_webhook(
        br#"{"id":"sentinel-1","sender":"tester","reply_target":"room","content":"ordinary message","channel":"__webhook_reply__"}"#,
        "token-main", zeroclaw_api::webhook::WebhookCancellation::new(), None,
    );
    sink.send(request).await.expect("webhook receiver active");
    assert!(matches!(
        outcome.await.expect("message acknowledged"),
        Ok(WebhookOutcome::Ack)
    ));
    let delivered = inbound
        .recv()
        .await
        .expect("sentinel name cannot divert a message to HTTP");
    assert_eq!(delivered.id, "sentinel-1");
    assert_eq!(delivered.channel, "plugin");
    assert_eq!(policy_calls.load(Ordering::SeqCst), 1);
    listener.abort();
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
async fn timed_out_channel_call_releases_lock_and_recreates_instance() {
    let channel = channel_with_timeout("recreate", Duration::from_millis(500)).await;

    // A spinning send outlives the 500ms wall-clock deadline; the host must
    // interrupt it, discard the store, and release the slot lock. The slow
    // operation is guest compute (a `spin` message): the governed `wasi:http`
    // surface is linked but has no egress grant, so it reaches nothing and no
    // network call is involved.
    let error = channel
        .send(&outbound("spin until the deadline", "room"))
        .await
        .expect_err("spinning send must hit the host deadline");
    assert!(
        error.to_string().contains("wall-clock deadline"),
        "unexpected error: {error:#}"
    );

    // The recreated instance handles a normal send instead of stranding behind
    // the discarded store's lock.
    tokio::time::timeout(
        Duration::from_secs(2),
        channel.send(&outbound("v1:token-recreate", "room")),
    )
    .await
    .expect("recreated channel call is not stranded behind the old lock")
    .expect("recreated channel handles a healthy request");
}

#[tokio::test]
async fn externally_cancelled_channel_call_discards_store_and_recreates() {
    // A generous timeout ensures the deadline does not fire; external
    // cancellation, not the wall clock, is what discards the store here.
    let channel = Arc::new(channel_with_timeout("cancel", Duration::from_secs(5)).await);

    // A spinning send is in flight; external cancellation (task abort) must drop
    // the in-flight guest call and discard its store rather than resuming it.
    let cancelled_channel = Arc::clone(&channel);
    let call = ::zeroclaw_spawn::spawn!(async move {
        cancelled_channel
            .send(&outbound("spin until cancelled", "room"))
            .await
    });
    // Give the guest time to enter the spin before cancelling.
    tokio::time::sleep(Duration::from_millis(200)).await;
    call.abort();
    assert!(
        call.await
            .expect_err("call task was aborted")
            .is_cancelled(),
        "external cancellation must drop the in-flight guest call"
    );

    // The fresh instance serves a normal send once the lock is released.
    tokio::time::timeout(
        Duration::from_secs(2),
        channel.send(&outbound("v1:token-cancel", "room")),
    )
    .await
    .expect("cancelled call released the channel lock")
    .expect("fresh channel instance served the healthy call");
}

/// The factory's `configure` snapshot is generation-scoped: reconstruction
/// after an interruption must replay the exact constructor-time config, so
/// config-derived metadata still matches the cached copy and the rebuilt
/// instance is admitted. The fixture surfaces its configured handle through
/// `self-handle`, which the reconstruction metadata check compares.
#[tokio::test]
async fn reconstruction_replays_the_constructor_generation_config_snapshot() {
    let mut config = HashMap::new();
    config.insert("handle".to_string(), "@from-config".to_string());
    let channel = channel_with(
        "config-generation",
        vec![
            zeroclaw_plugins::PluginPermission::HttpClient,
            zeroclaw_plugins::PluginPermission::ConfigRead,
        ],
        &config,
        // u64::MAX fuel so the wall-clock deadline, not fuel exhaustion,
        // interrupts the spinning send and forces reconstruction.
        limits_with(u64::MAX, Duration::from_millis(500)),
    )
    .await;
    assert_eq!(
        channel.self_handle().as_deref(),
        Some("@from-config"),
        "the ConfigRead-granted snapshot must reach the guest's configure"
    );

    // A spinning send outlives the deadline, forcing store discard and
    // reconstruction against the same constructor-generation config snapshot.
    let error = channel
        .send(&outbound("spin until the deadline", "room"))
        .await
        .expect_err("spinning send must hit the host deadline");
    assert!(
        error.to_string().contains("wall-clock deadline"),
        "unexpected error: {error:#}"
    );

    // The reconstructed instance serves a normal send; its config-derived
    // metadata still matches because the constructor-generation snapshot was
    // replayed.
    tokio::time::timeout(
        Duration::from_secs(2),
        channel.send(&outbound("v1:token-config-generation", "room")),
    )
    .await
    .expect("reconstructed channel call is not stranded")
    .expect(
        "reconstruction replayed the constructor-generation config, so the \
         config-derived metadata matched and the instance was admitted",
    );
    assert_eq!(
        channel.self_handle().as_deref(),
        Some("@from-config"),
        "cached metadata remains the constructor-generation view"
    );
}

/// Inbound delivery to the guest is at-most-once across an interruption: a
/// message the guest dequeued via `inbound-poll` before the deadline fired is
/// not requeued, while the still-queued backlog survives reconstruction.
#[tokio::test]
async fn interrupted_poll_preserves_backlog_but_not_the_dequeued_message() {
    // u64::MAX fuel guarantees the wall-clock deadline, not fuel exhaustion,
    // interrupts the spin, which is the store-discard path under test.
    let channel = channel_with(
        "at-most-once",
        vec![zeroclaw_plugins::PluginPermission::HttpClient],
        &HashMap::new(),
        limits_with(u64::MAX, Duration::from_millis(250)),
    )
    .await
    .with_sender_authorizer(Arc::new(|_| true));

    let inbound = channel.inbound();
    let queue_message = |id: &str, content: &str| HostInboundMessage {
        id: id.to_string(),
        sender: "tester".to_string(),
        reply_target: "room".to_string(),
        content: content.to_string(),
        channel: "host-channel".to_string(),
        timestamp: 1,
        ..Default::default()
    };
    // The fixture spins after dequeuing a message whose content starts with
    // "spin", so the first poll is interrupted after the dequeue.
    inbound.enqueue(queue_message("interrupted-1", "spin until the deadline"));
    inbound.enqueue(queue_message("kept-1", "first kept"));
    inbound.enqueue(queue_message("kept-2", "second kept"));

    let (tx, mut rx) = tokio::sync::mpsc::channel(2);
    let listener = ::zeroclaw_spawn::spawn!(async move { channel.listen(tx).await });

    let first = tokio::time::timeout(Duration::from_secs(10), rx.recv())
        .await
        .expect("backlog message arrives after reconstruction")
        .expect("listener remains connected");
    assert_eq!(
        first.id, "kept-1",
        "the dequeued message must not be redelivered; the backlog resumes"
    );
    let second = tokio::time::timeout(Duration::from_secs(5), rx.recv())
        .await
        .expect("second backlog message arrives")
        .expect("listener remains connected");
    assert_eq!(second.id, "kept-2");
    assert_eq!(inbound.pending(), 0, "no message remains queued");
    assert!(
        tokio::time::timeout(Duration::from_millis(400), rx.recv())
            .await
            .is_err(),
        "the interrupted message must not resurface later"
    );
    listener.abort();
}

/// The listener asks the guest's `health-check` from its poll loop, at most
/// once per interval, and `listener_health` reports the latest answer without
/// calling the guest itself. The fixture's health flips during its own polls,
/// the way a real plugin's connection state does.
#[tokio::test]
async fn listener_health_follows_the_guest_health_check_at_a_bounded_cadence() {
    // The host's ask interval, `GUEST_HEALTH_INTERVAL` in `wasm_channel.rs`.
    const ASK_INTERVAL: Duration = Duration::from_secs(30);
    let channel = Arc::new(
        channel("health")
            .await
            .with_sender_authorizer(Arc::new(|_| true)),
    );
    assert_eq!(
        channel.listener_health(),
        Some(ListenerHealth::Pending),
        "the guest has not answered before the listener asks"
    );

    // Virtual time from here on. The fixture's polls and health checks never
    // wait on a timer, so only the poll loop's back-off and the ask interval
    // move the clock, and a minute of polling takes no wall time.
    tokio::time::pause();
    let inbound = channel.inbound();
    let (tx, mut rx) = tokio::sync::mpsc::channel(1);
    let listener_channel = Arc::clone(&channel);
    let listener = zeroclaw_spawn::spawn!(async move { listener_channel.listen(tx).await });

    wait_for_listener_health(&channel, ListenerHealth::Healthy, Duration::from_secs(1)).await;

    // The plugin sees its connection drop during a poll. The host keeps the
    // answer it already has until the next ask, because it does not ask per
    // poll.
    inbound.enqueue(queued("down", "health:down"));
    tokio::time::sleep(ASK_INTERVAL / 2).await;
    assert_eq!(
        inbound.pending(),
        0,
        "the plugin polled the control message"
    );
    assert_eq!(channel.listener_health(), Some(ListenerHealth::Healthy));

    wait_for_listener_health(&channel, ListenerHealth::Unhealthy, ASK_INTERVAL).await;

    inbound.enqueue(queued("up", "health:up"));
    wait_for_listener_health(
        &channel,
        ListenerHealth::Healthy,
        ASK_INTERVAL + Duration::from_secs(1),
    )
    .await;

    // About a minute of polling every 50 to 500 ms drew three asks: one after
    // the first poll, then one per interval.
    inbound.enqueue(queued("count", "health:count"));
    let report = tokio::time::timeout(Duration::from_secs(5), rx.recv())
        .await
        .expect("the ask count arrives")
        .expect("listener remains connected");
    assert_eq!(report.content, "health-checks:3");
    listener.abort();
}

/// A poll bridge that keeps failing reports the listener unhealthy although the
/// guest's own health check still answers `true`, and the listener recovers
/// once polls succeed again, without waiting a full ask interval.
#[tokio::test]
async fn failing_polls_report_the_listener_unhealthy_despite_a_healthy_guest() {
    // u64::MAX fuel so the wall-clock deadline, not fuel exhaustion, ends each
    // spinning poll and discards the instance.
    let channel = Arc::new(
        channel_with(
            "poll-failure",
            vec![PluginPermission::HttpClient],
            &HashMap::new(),
            limits_with(u64::MAX, Duration::from_secs(1)),
        )
        .await
        .with_sender_authorizer(Arc::new(|_| true)),
    );
    let (tx, _rx) = tokio::sync::mpsc::channel(1);
    let listener_channel = Arc::clone(&channel);
    let listener = zeroclaw_spawn::spawn!(async move { listener_channel.listen(tx).await });
    // The first ask answers on the healthy instance. The next one is an
    // interval away, so no ask runs during the failures below.
    wait_for_listener_health(&channel, ListenerHealth::Healthy, Duration::from_secs(5)).await;

    // Each `spin` message holds one poll until the deadline and discards the
    // instance, so a run of them keeps the poll bridge failing for seconds.
    let inbound = channel.inbound();
    for index in 0..3 {
        inbound.enqueue(queued(&format!("spin-{index}"), "spin until the deadline"));
    }
    wait_for_listener_health(&channel, ListenerHealth::Unhealthy, Duration::from_secs(5)).await;
    // The guest's last answer still stands, so the first clean poll on a
    // rebuilt instance restores the verdict without waiting for the next ask.
    wait_for_listener_health(&channel, ListenerHealth::Healthy, Duration::from_secs(15)).await;
    assert_eq!(
        inbound.pending(),
        0,
        "every failing poll consumed its message"
    );
    listener.abort();
}

/// A trap leaves Wasmtime refusing every later call into that store. A
/// `health-check` that traps must not take the channel down with it: the
/// listener discards that instance, keeps delivering on a rebuilt one, reports
/// the failed ask as unhealthy until a later ask succeeds, and backs off before
/// asking again, because every failed ask costs a rebuild.
#[tokio::test]
async fn a_trapping_health_check_reads_unhealthy_without_disabling_the_channel() {
    // The host's ask interval, `GUEST_HEALTH_INTERVAL` in `wasm_channel.rs`.
    const ASK_INTERVAL: Duration = Duration::from_secs(30);
    let channel = Arc::new(
        channel("health-trap")
            .await
            .with_sender_authorizer(Arc::new(|_| true)),
    );
    // Virtual time; see `listener_health_follows_the_guest_health_check_at_a_bounded_cadence`.
    tokio::time::pause();
    let inbound = channel.inbound();
    let (tx, mut rx) = tokio::sync::mpsc::channel(1);
    let listener_channel = Arc::clone(&channel);
    let listener = zeroclaw_spawn::spawn!(async move { listener_channel.listen(tx).await });
    wait_for_listener_health(&channel, ListenerHealth::Healthy, Duration::from_secs(1)).await;

    inbound.enqueue(queued("trap", "health:trap"));
    wait_for_listener_health(
        &channel,
        ListenerHealth::Unhealthy,
        ASK_INTERVAL + Duration::from_secs(1),
    )
    .await;

    inbound.enqueue(queued("after-trap", "delivered after the trap"));
    let delivered = tokio::time::timeout(Duration::from_secs(5), rx.recv())
        .await
        .expect("a rebuilt instance keeps polling after the trap")
        .expect("listener remains connected");
    assert_eq!(delivered.id, "after-trap");

    // After a failed ask the next one waits two intervals, not one.
    tokio::time::sleep(ASK_INTERVAL).await;
    assert_eq!(channel.listener_health(), Some(ListenerHealth::Unhealthy));

    // The rebuilt instance answers that later ask.
    wait_for_listener_health(
        &channel,
        ListenerHealth::Healthy,
        ASK_INTERVAL + Duration::from_secs(5),
    )
    .await;
    listener.abort();
}

/// A trap leaves Wasmtime refusing every later call into that store. A
/// `poll-message` that traps must cost only the message it had dequeued: the
/// host discards the instance and polls a rebuilt one, so later messages still
/// arrive, without waiting for the next health ask.
#[tokio::test]
async fn a_poll_trap_costs_only_its_message_and_later_ones_still_arrive() {
    let channel = Arc::new(
        channel("poll-trap")
            .await
            .with_sender_authorizer(Arc::new(|_| true)),
    );
    let inbound = channel.inbound();
    let (tx, mut rx) = tokio::sync::mpsc::channel(1);
    let listener_channel = Arc::clone(&channel);
    let listener = zeroclaw_spawn::spawn!(async move { listener_channel.listen(tx).await });
    // The first health ask has run, so the next one is 30 seconds away and
    // cannot be what replaces the trapped instance.
    wait_for_listener_health(&channel, ListenerHealth::Healthy, Duration::from_secs(5)).await;

    inbound.enqueue(queued("trap", "poll:trap"));
    inbound.enqueue(queued("after-trap", "delivered after the trap"));
    inbound.enqueue(queued("count", "configure:count"));
    let delivered = tokio::time::timeout(Duration::from_secs(5), rx.recv())
        .await
        .expect("a message queued behind the trap arrives")
        .expect("listener remains connected");
    assert_eq!(
        delivered.id, "after-trap",
        "the trapping poll consumed its own message"
    );
    let report = tokio::time::timeout(Duration::from_secs(5), rx.recv())
        .await
        .expect("the configure count arrives")
        .expect("listener remains connected");
    assert_eq!(
        report.content, "configures:2",
        "one rebuild replaced the trapped instance"
    );
    listener.abort();
}

/// Exports with an error result of their own fail in two ways: the plugin
/// returns an error string and its instance is fine, or the call traps and
/// Wasmtime refuses every later call into the store. The host keeps the
/// instance after the first and replaces it after the second.
#[tokio::test]
async fn a_send_trap_replaces_the_instance_but_a_returned_error_keeps_it() {
    let channel = channel("send-trap")
        .await
        .with_sender_authorizer(Arc::new(|_| true));

    let returned = channel
        .send(&outbound("v0:stale-token", "room"))
        .await
        .expect_err("the fixture rejects a message built from stale config");
    assert_eq!(
        returned.to_string(),
        "message did not use one current config revision",
        "the plugin's own error string reaches the caller unchanged"
    );

    let trapped = channel
        .send(&outbound("send:trap", "room"))
        .await
        .expect_err("a trapping send fails");
    assert!(
        trapped.to_string().starts_with("channel.send trapped"),
        "unexpected error: {trapped:#}"
    );
    channel
        .send(&outbound("v1:token-send-trap", "room"))
        .await
        .expect("a rebuilt instance serves the next send");

    let inbound = channel.inbound();
    inbound.enqueue(queued("count", "configure:count"));
    let (tx, mut rx) = tokio::sync::mpsc::channel(1);
    let listener = zeroclaw_spawn::spawn!(async move { channel.listen(tx).await });
    let report = tokio::time::timeout(Duration::from_secs(5), rx.recv())
        .await
        .expect("the configure count arrives")
        .expect("listener remains connected");
    assert_eq!(
        report.content, "configures:2",
        "the returned error kept the instance and only the trap replaced it"
    );
    listener.abort();
}

/// Each rebuild runs `configure` again, which reconnects a gateway-style
/// plugin. A plugin whose polls keep trapping must not be rebuilt on every
/// poll: once traps outrun the rebuild budget, rebuilds come five minutes
/// apart, and the channel delivers again once polls stop trapping.
#[tokio::test]
async fn rebuilds_after_repeated_poll_traps_are_spaced_out() {
    const TRAPS: usize = 40;
    let channel = Arc::new(
        channel("trap-loop")
            .await
            .with_sender_authorizer(Arc::new(|_| true)),
    );
    // Virtual time; see `listener_health_follows_the_guest_health_check_at_a_bounded_cadence`.
    tokio::time::pause();
    let inbound = channel.inbound();
    for index in 0..TRAPS {
        inbound.enqueue(queued(&format!("trap-{index}"), "poll:trap"));
    }
    let (tx, mut rx) = tokio::sync::mpsc::channel(1);
    let listener_channel = Arc::clone(&channel);
    let listener = zeroclaw_spawn::spawn!(async move { listener_channel.listen(tx).await });

    // Each instance traps on its first poll and consumes one message. The
    // first three traps are rebuilt at once and the next instance waits five
    // minutes, so five instances have polled after seven and a half minutes.
    // Rebuilding on every poll would drain the queue in about twenty seconds.
    tokio::time::sleep(Duration::from_secs(450)).await;
    let polled = TRAPS - inbound.pending() as usize;
    assert_eq!(
        polled, 5,
        "instances that polled in seven and a half minutes"
    );
    assert_eq!(channel.listener_health(), Some(ListenerHealth::Unhealthy));

    // The wait holds back every rebuild, not only the poll loop's: a send in
    // the meantime is refused without running the plugin.
    let refused = channel
        .send(&outbound("v1:token-trap-loop", "room"))
        .await
        .expect_err("a send cannot rebuild the instance early");
    assert!(
        refused.to_string().contains("keeps failing"),
        "unexpected error: {refused:#}"
    );

    // Polls stop trapping. The next rebuild, at most five minutes away,
    // delivers again.
    while inbound.poll().is_some() {}
    inbound.enqueue(queued("recovered", "delivered once polls stop trapping"));
    let delivered = tokio::time::timeout(Duration::from_secs(300), rx.recv())
        .await
        .expect("the channel delivers again within one rebuild interval")
        .expect("listener remains connected");
    assert_eq!(delivered.id, "recovered");
    // The first poll trapped, so the guest's health check has never run, and
    // the listener puts the first ask off until the budget could absorb a
    // trap, one interval later.
    assert_eq!(channel.listener_health(), Some(ListenerHealth::Pending));
    wait_for_listener_health(&channel, ListenerHealth::Healthy, Duration::from_secs(310)).await;
    channel
        .send(&outbound("v1:token-trap-loop", "room"))
        .await
        .expect("the rebuilt instance serves sends");
    listener.abort();
}

/// A trapping health ask discards the instance and counts against the rebuild
/// budget like any trap. The listener waits longer after each failed ask, so
/// a plugin whose health check always traps stays within the budget and keeps
/// delivering, each ask's instance replaced at once.
#[tokio::test]
async fn a_health_check_that_always_traps_does_not_delay_delivery() {
    let channel = Arc::new(
        channel("health-always-traps")
            .await
            .with_sender_authorizer(Arc::new(|_| true)),
    );
    // Virtual time; see `listener_health_follows_the_guest_health_check_at_a_bounded_cadence`.
    tokio::time::pause();
    let inbound = channel.inbound();
    let (tx, mut rx) = tokio::sync::mpsc::channel(1);
    let listener_channel = Arc::clone(&channel);
    let listener = zeroclaw_spawn::spawn!(async move { listener_channel.listen(tx).await });
    wait_for_listener_health(&channel, ListenerHealth::Healthy, Duration::from_secs(1)).await;
    inbound.enqueue(queued("trap", "health:trap-always"));

    // The next five asks, 30, 60, 120, 240 and 480 seconds apart, each trap
    // and cost a rebuild. Had the budget made one of those rebuilds wait, a
    // message arriving meanwhile would have waited with it.
    let mut configures = String::new();
    for probe in 1..=600 {
        inbound.enqueue(queued(&format!("probe-{probe}"), "configure:count"));
        configures = tokio::time::timeout(Duration::from_secs(1), rx.recv())
            .await
            .unwrap_or_else(|_| panic!("probe {probe} waited more than a second"))
            .expect("listener remains connected")
            .content;
        if configures == "configures:6" {
            break;
        }
        tokio::time::sleep(Duration::from_secs(2)).await;
    }
    assert_eq!(
        configures, "configures:6",
        "each of the five failed asks cost one rebuild"
    );
    assert_eq!(channel.listener_health(), Some(ListenerHealth::Unhealthy));
    listener.abort();
}

/// A missed deadline discards the instance and counts against the rebuild
/// budget like a trap, so a hung export cannot reconnect the plugin at every
/// deadline. An export that waits for a person to answer is the exception: a
/// choice or approval prompt that outlasts the deadline is not a broken
/// plugin.
#[tokio::test]
async fn missed_deadlines_spend_the_rebuild_budget_unless_a_person_is_answering() {
    let channel = channel_with_timeout("deadline-budget", Duration::from_millis(250)).await;
    let choices = ["yes".to_string(), "no".to_string()];
    for attempt in 0..5 {
        let error = channel
            .request_choice("spin until the deadline", &choices, Duration::from_secs(60))
            .await
            .expect_err("an unanswered choice prompt misses the deadline");
        assert!(
            error.to_string().contains("wall-clock deadline"),
            "attempt {attempt}: {error:#}"
        );
    }
    channel
        .send(&outbound("v1:token-deadline-budget", "room"))
        .await
        .expect("slow answers left the budget untouched");

    // Three hung sends are rebuilt at once; the fourth makes the next
    // rebuild wait.
    for attempt in 0..4 {
        let error = channel
            .send(&outbound("spin until the deadline", "room"))
            .await
            .expect_err("a hung send misses the deadline");
        assert!(
            error.to_string().contains("wall-clock deadline"),
            "attempt {attempt}: {error:#}"
        );
    }
    let refused = channel
        .send(&outbound("v1:token-deadline-budget", "room"))
        .await
        .expect_err("the budget holds the rebuild back");
    assert!(
        refused.to_string().contains("keeps failing"),
        "unexpected error: {refused:#}"
    );
}

/// A health check that traps now and then must not take the channel down.
/// The listener's ask back-off returns to the regular interval after any
/// answer, so such traps could outrun the rebuild budget; the listener skips
/// an ask the budget could not absorb, and polls and sends never wait on it.
#[tokio::test]
async fn a_health_check_that_traps_now_and_then_does_not_take_the_channel_down() {
    let channel = Arc::new(
        channel("health-traps-now-and-then")
            .await
            .with_sender_authorizer(Arc::new(|_| true)),
    );
    // Virtual time; see `listener_health_follows_the_guest_health_check_at_a_bounded_cadence`.
    tokio::time::pause();
    let inbound = channel.inbound();
    let (tx, mut rx) = tokio::sync::mpsc::channel(1);
    let listener_channel = Arc::clone(&channel);
    let listener = zeroclaw_spawn::spawn!(async move { listener_channel.listen(tx).await });
    wait_for_listener_health(&channel, ListenerHealth::Healthy, Duration::from_secs(1)).await;
    inbound.enqueue(queued("trap", "health:trap-alternate"));

    // Every other ask traps, and asks come 30 to 60 seconds apart. For twenty
    // minutes, every message is still delivered within a second.
    let until = tokio::time::Instant::now() + Duration::from_secs(20 * 60);
    let mut configures = String::new();
    let mut probe = 0;
    while tokio::time::Instant::now() < until {
        probe += 1;
        inbound.enqueue(queued(&format!("probe-{probe}"), "configure:count"));
        configures = tokio::time::timeout(Duration::from_secs(1), rx.recv())
            .await
            .unwrap_or_else(|_| panic!("probe {probe} waited more than a second"))
            .expect("listener remains connected")
            .content;
        tokio::time::sleep(Duration::from_secs(2)).await;
    }
    let rebuilds = configures
        .strip_prefix("configures:")
        .and_then(|count| count.parse::<u32>().ok())
        .expect("the probe reports a configure count")
        - 1;
    // The budget lets about one trapped ask through every five minutes once
    // the first few have spent it: six in twenty minutes.
    assert!(
        (5..=7).contains(&rebuilds),
        "{rebuilds} trapped asks were rebuilt in twenty minutes"
    );
    listener.abort();
}
