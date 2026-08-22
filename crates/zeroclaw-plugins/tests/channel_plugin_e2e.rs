//! End-to-end fixture for the host's channel-component adapter and scoped secrets.
//!
//! The source fixture is a workspace member and is built on demand into a
//! separate target directory so the nested Cargo invocation cannot contend
//! with the host test process's build lock.

#![cfg(feature = "plugins-wasm-cranelift")]

use std::collections::HashMap;
use std::path::PathBuf;
use std::process::Command;
use std::sync::{Arc, OnceLock, RwLock};
use std::time::Duration;

use tokio::io::{AsyncReadExt, AsyncWriteExt};
use tokio::net::TcpListener;
use zeroclaw_api::attribution::Attributable;
use zeroclaw_api::channel::{Channel, SendMessage};
use zeroclaw_plugins::component::{HostInboundMessage, PluginLimits};
use zeroclaw_plugins::config::{PluginConfigResolver, resolve_plugin_config};
use zeroclaw_plugins::endpoint::PluginChannelEndpoint;
use zeroclaw_plugins::instance::PluginInstanceScope;
use zeroclaw_plugins::services::PluginHostServices;
use zeroclaw_plugins::wasm_channel::WasmChannel;
use zeroclaw_plugins::{PluginCapability, PluginManifest, PluginPermission};

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
        capabilities: vec![PluginCapability::Channel],
        // Every fixture channel is ConfigRead-granted so the typed-config and
        // scoped-secret contract is exercised on every instantiation, and
        // HttpClient-granted so the deadline tests can drive a real outbound
        // request; the HTTP grant is inert unless a send targets an http:// URL.
        permissions: vec![PluginPermission::ConfigRead, PluginPermission::HttpClient],
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
    PluginHostServices::new(resolver)
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

    WasmChannel::from_wasm(endpoint, &fixture(), services, limits())
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
    WasmChannel::from_wasm(endpoint, &fixture(), &services, limits)
        .await
        .expect("instantiate fixture channel")
}

async fn channel_with_timeout(binding: &str, timeout: Duration) -> WasmChannel {
    channel_with(
        binding,
        vec![PluginPermission::HttpClient],
        &HashMap::new(),
        limits_with_timeout(timeout),
    )
    .await
}

/// One-request server that drips a byte every two seconds so a send against
/// it can only end at the host wall-clock deadline.
async fn drip_server() -> (String, tokio::task::JoinHandle<()>) {
    let listener = TcpListener::bind("127.0.0.1:0")
        .await
        .expect("bind drip server");
    let address = listener.local_addr().expect("drip server address");
    let task = ::zeroclaw_spawn::spawn!(async move {
        let (mut stream, _) = listener.accept().await.expect("accept drip request");
        let mut request = [0_u8; 4096];
        let _ = stream.read(&mut request).await;
        stream
            .write_all(b"HTTP/1.1 200 OK\r\nContent-Length: 1000000\r\n\r\nx")
            .await
            .expect("write drip head");
        loop {
            tokio::time::sleep(Duration::from_secs(2)).await;
            if stream.write_all(b"x").await.is_err() {
                break;
            }
        }
    });
    (format!("http://{address}/body"), task)
}

/// One-request server that answers immediately with a small 200 response.
async fn ok_server() -> (String, tokio::task::JoinHandle<()>) {
    let listener = TcpListener::bind("127.0.0.1:0")
        .await
        .expect("bind ok server");
    let address = listener.local_addr().expect("ok server address");
    let task = ::zeroclaw_spawn::spawn!(async move {
        let (mut stream, _) = listener.accept().await.expect("accept ok request");
        let mut request = [0_u8; 4096];
        let _ = stream.read(&mut request).await;
        stream
            .write_all(b"HTTP/1.1 200 OK\r\nContent-Length: 2\r\n\r\nok")
            .await
            .expect("write ok response");
    });
    (format!("http://{address}/body"), task)
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
async fn timed_out_channel_call_releases_lock_and_recreates_instance() {
    let channel = channel_with_timeout("recreate", Duration::from_millis(500)).await;

    let drip_listener = TcpListener::bind("127.0.0.1:0")
        .await
        .expect("bind drip server");
    let drip_address = drip_listener.local_addr().expect("drip server address");
    let drip_server = ::zeroclaw_spawn::spawn!(async move {
        let (mut stream, _) = drip_listener.accept().await.expect("accept drip request");
        let mut request = [0_u8; 4096];
        let _ = stream.read(&mut request).await;
        stream
            .write_all(b"HTTP/1.1 200 OK\r\nContent-Length: 1000000\r\n\r\nx")
            .await
            .expect("write drip head");
        loop {
            tokio::time::sleep(Duration::from_secs(2)).await;
            if stream.write_all(b"x").await.is_err() {
                break;
            }
        }
    });
    let mut dripping = outbound("", "room");
    dripping.content = format!("http://{drip_address}/body");
    let error = channel
        .send(&dripping)
        .await
        .expect_err("dripping send must hit the host deadline");
    assert!(
        error.to_string().contains("wall-clock deadline"),
        "unexpected error: {error:#}"
    );
    drip_server.abort();

    let healthy_listener = TcpListener::bind("127.0.0.1:0")
        .await
        .expect("bind healthy server");
    let healthy_address = healthy_listener
        .local_addr()
        .expect("healthy server address");
    let healthy_server = ::zeroclaw_spawn::spawn!(async move {
        let (mut stream, _) = healthy_listener
            .accept()
            .await
            .expect("accept healthy request");
        let mut request = [0_u8; 4096];
        let _ = stream.read(&mut request).await;
        stream
            .write_all(b"HTTP/1.1 200 OK\r\nContent-Length: 2\r\n\r\nok")
            .await
            .expect("write healthy response");
    });
    let mut healthy = outbound("", "room");
    healthy.content = format!("http://{healthy_address}/body");
    tokio::time::timeout(Duration::from_secs(2), channel.send(&healthy))
        .await
        .expect("recreated channel call is not stranded behind the old lock")
        .expect("recreated channel handles a healthy request");
    healthy_server.await.expect("healthy server task");
}

#[tokio::test]
async fn externally_cancelled_channel_call_discards_store_and_recreates() {
    let channel = Arc::new(channel_with_timeout("cancel", Duration::from_secs(5)).await);

    let stalled_listener = TcpListener::bind("127.0.0.1:0")
        .await
        .expect("bind stalled server");
    let stalled_address = stalled_listener
        .local_addr()
        .expect("stalled server address");
    let (accepted_tx, accepted_rx) = tokio::sync::oneshot::channel();
    let stalled_server = ::zeroclaw_spawn::spawn!(async move {
        let (mut stream, _) = stalled_listener
            .accept()
            .await
            .expect("accept stalled request");
        let mut request = [0_u8; 4096];
        let _ = stream.read(&mut request).await;
        let _ = accepted_tx.send(());
        tokio::time::sleep(Duration::from_secs(5)).await;
    });

    let mut stalled = outbound("", "room");
    stalled.content = format!("http://{stalled_address}/body");
    let cancelled_channel = Arc::clone(&channel);
    let call = ::zeroclaw_spawn::spawn!(async move { cancelled_channel.send(&stalled).await });
    accepted_rx
        .await
        .expect("guest call reached the async HTTP host import");
    call.abort();
    assert!(
        call.await
            .expect_err("call task was aborted")
            .is_cancelled(),
        "external cancellation must drop the in-flight host call"
    );
    stalled_server.abort();

    let healthy_listener = TcpListener::bind("127.0.0.1:0")
        .await
        .expect("bind post-cancel server");
    let healthy_address = healthy_listener
        .local_addr()
        .expect("post-cancel server address");
    let healthy_server = ::zeroclaw_spawn::spawn!(async move {
        let (mut stream, _) = healthy_listener
            .accept()
            .await
            .expect("accept post-cancel request");
        let mut request = [0_u8; 4096];
        let _ = stream.read(&mut request).await;
        stream
            .write_all(b"HTTP/1.1 200 OK\r\nContent-Length: 2\r\n\r\nok")
            .await
            .expect("write post-cancel response");
    });
    let mut healthy = outbound("", "room");
    healthy.content = format!("http://{healthy_address}/body");
    tokio::time::timeout(Duration::from_secs(2), channel.send(&healthy))
        .await
        .expect("cancelled call released the channel lock")
        .expect("fresh channel instance served the healthy call");
    healthy_server.await.expect("post-cancel server task");
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
        limits_with_timeout(Duration::from_millis(500)),
    )
    .await;
    assert_eq!(
        channel.self_handle().as_deref(),
        Some("@from-config"),
        "the ConfigRead-granted snapshot must reach the guest's configure"
    );

    let (drip_url, drip_task) = drip_server().await;
    let mut dripping = outbound("", "room");
    dripping.content = drip_url;
    let error = channel
        .send(&dripping)
        .await
        .expect_err("dripping send must hit the host deadline");
    assert!(
        error.to_string().contains("wall-clock deadline"),
        "unexpected error: {error:#}"
    );
    drip_task.abort();

    let (ok_url, ok_task) = ok_server().await;
    let mut healthy = outbound("", "room");
    healthy.content = ok_url;
    tokio::time::timeout(Duration::from_secs(2), channel.send(&healthy))
        .await
        .expect("reconstructed channel call is not stranded")
        .expect(
            "reconstruction replayed the constructor-generation config, so the \
             config-derived metadata matched and the instance was admitted",
        );
    ok_task.await.expect("ok server task");
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
    .await;

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
