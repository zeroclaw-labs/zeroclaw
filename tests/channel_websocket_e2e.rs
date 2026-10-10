//! Cross-crate proof of the channel WebSocket lifecycle: one host-mediated
//! socket, held by a channel plugin across many `poll-message` calls, through
//! the whole activation path an operator exercises.
//!
//! `crates/zeroclaw-plugins/tests/websocket_plugin_e2e.rs` proves reach and
//! denial for a tool component, which gets a fresh store per call. A channel
//! keeps one warm store and one connection between calls, so reconnect after a
//! server close, shutdown, and denial on that path are different claims. This
//! file makes them against a loopback WebSocket server and asserts on what the
//! server observed: accepted connections, received frames, and ended streams.
//! Nothing is stubbed. The deny path is the same host code the shipped daemon
//! runs; the allow path opens a real TCP connection to a listener in this
//! process.
//!
//! Every test uses its own channel alias: the host's connection budget is
//! process-wide and keyed by the instance identity, so tests sharing an alias
//! would share, and could exhaust, one budget.

#![cfg(feature = "plugins-wasm-cranelift")]

use std::collections::HashMap;
use std::path::PathBuf;
use std::process::Command;
use std::sync::atomic::{AtomicUsize, Ordering};
use std::sync::{Arc, Mutex, OnceLock};
use std::time::Duration;

use futures_util::{SinkExt, StreamExt};
use tempfile::TempDir;
use tokio::net::{TcpListener, TcpStream};
use tokio::sync::{Notify, mpsc};
use tokio::task::JoinHandle;
use tokio_tungstenite::tungstenite::Message;
use tokio_tungstenite::tungstenite::protocol::CloseFrame;
use tokio_tungstenite::tungstenite::protocol::frame::coding::CloseCode;
use zeroclaw_api::channel::{Channel, ChannelMessage, SendMessage};
use zeroclaw_config::multi_agent::{PeerGroupConfig, PeerUsername};
use zeroclaw_config::providers::{ChannelRef, ModelProviderRef};
use zeroclaw_config::schema::{
    AliasedAgentConfig, AnthropicModelProviderConfig, Config, PluginChannelConfig,
    PluginEntryConfig, RiskProfileConfig,
};
use zeroclaw_plugins::PluginCapability;
use zeroclaw_plugins::host::PluginHost;
use zeroclaw_plugins::instance::PluginInstanceScope;

const PACKAGE: &str = "channel-websocket-fixture";
const MANIFEST: &str =
    "crates/zeroclaw-plugins/tests/fixtures/channel-websocket-fixture/plugin-manifest.toml";

/// Upper bound on every wait for an event that must happen.
const STEP: Duration = Duration::from_secs(10);
/// How long a dropped channel may take to close its socket.
const SHUTDOWN: Duration = Duration::from_secs(5);
/// A window in which several idle polls pass (the host backs off to 500 ms).
const IDLE: Duration = Duration::from_secs(1);

// ── fixture provisioning ──────────────────────────────────────────

/// Build the channel WebSocket component once per test binary.
///
/// The fixture is a workspace member built into its own target directory so the
/// nested Cargo invocation cannot contend with this test process's build lock.
/// A missing wasm target is a failure, never a skip.
fn fixture() -> PathBuf {
    static FIXTURE: OnceLock<PathBuf> = OnceLock::new();
    FIXTURE
        .get_or_init(|| {
            let fixture_dir = PathBuf::from(env!("CARGO_MANIFEST_DIR"))
                .join("crates/zeroclaw-plugins/tests/fixtures/channel-websocket-fixture");
            let target_dir =
                PathBuf::from(env!("CARGO_TARGET_TMPDIR")).join("channel-websocket-fixture");
            let status = Command::new(env!("CARGO"))
                .current_dir(&fixture_dir)
                .args([
                    "build",
                    "--locked",
                    "--quiet",
                    "--package",
                    "zeroclaw-channel-websocket-plugin-fixture",
                    "--target",
                    "wasm32-wasip2",
                    "--target-dir",
                ])
                .arg(&target_dir)
                .status()
                .expect("run Cargo for the channel WebSocket component fixture");
            assert!(
                status.success(),
                "channel WebSocket fixture must build; install the wasm32-wasip2 target"
            );

            let wasm = target_dir
                .join("wasm32-wasip2/debug/zeroclaw_channel_websocket_plugin_fixture.wasm");
            assert!(
                wasm.is_file(),
                "channel WebSocket fixture WASM was not produced"
            );
            wasm
        })
        .clone()
}

/// One installed package plus the isolated data and config paths of a daemon.
struct Deployment {
    root: TempDir,
}

impl Deployment {
    /// Install the fixture as a real plugin package under `manifest`.
    fn install(manifest: &str) -> Self {
        let root = TempDir::new().expect("create deployment root");
        let package = root.path().join("plugins").join(PACKAGE);
        std::fs::create_dir_all(&package).expect("create plugin package");
        std::fs::copy(fixture(), package.join("channel-websocket-fixture.wasm"))
            .expect("copy channel WebSocket component fixture");
        std::fs::write(package.join("manifest.toml"), manifest).expect("install the manifest");
        Self { root }
    }

    fn plugins_dir(&self) -> PathBuf {
        self.root.path().join("plugins")
    }
}

/// The canonical manifest, as the fixture ships it.
fn canonical_manifest() -> String {
    std::fs::read_to_string(PathBuf::from(env!("CARGO_MANIFEST_DIR")).join(MANIFEST))
        .expect("read the canonical fixture manifest")
}

/// The canonical manifest with the WebSocket grant removed and nothing else
/// changed. The assertion keeps a manifest edit from silently turning the
/// ungranted case into a copy of the granted one.
fn manifest_without_websocket_grant() -> String {
    let canonical = canonical_manifest();
    let stripped = canonical.replace(
        "permissions = [\"config_read\", \"websocket_client\"]",
        "permissions = [\"config_read\"]",
    );
    assert_ne!(
        canonical, stripped,
        "the canonical manifest must declare the grant this test removes"
    );
    stripped
}

// ── operator config ───────────────────────────────────────────────

/// The operator's reach decision for one instance.
struct Grant {
    hosts: &'static [&'static str],
    private: &'static [&'static str],
}

/// Loopback reach: the host must be granted and its private address class
/// opened, exactly as an operator would write it for a LAN service.
const LOOPBACK: Grant = Grant {
    hosts: &["127.0.0.1"],
    private: &["127.0.0.1"],
};

/// The instance-key `[[plugins.entries]]` row for the configured channel.
///
/// The key is derived from the same manifest the loader admits, which is what
/// proves the row the operator writes is the row the store resolves.
fn entry_row(deployment: &Deployment, alias: &str, url: &str, grant: &Grant) -> PluginEntryConfig {
    let host =
        PluginHost::from_plugins_dir(&deployment.plugins_dir()).expect("admit fixture package");
    let manifest = host
        .manifest(PACKAGE)
        .expect("fixture manifest is admitted");
    let scope = PluginInstanceScope::from_manifest(
        manifest,
        PluginCapability::Channel,
        alias,
        manifest.permissions.iter().copied(),
    )
    .expect("admit configured logical channel");

    PluginEntryConfig {
        name: scope
            .id()
            .config_entry_key()
            .expect("derive canonical fixture config key"),
        config: HashMap::from([("url".to_string(), url.to_string())]),
        egress_hosts: grant.hosts.iter().map(|host| (*host).to_string()).collect(),
        egress_allow_private: grant
            .private
            .iter()
            .map(|host| (*host).to_string())
            .collect(),
        ..PluginEntryConfig::default()
    }
}

/// A realistic operator config: one enabled agent routing to one configured
/// channel plugin, a peer group admitting `alice`, and the instance's grant.
/// It passes `Config::validate`, so it is a config an operator could write.
fn activation_config(deployment: &Deployment, alias: &str, url: &str, grant: &Grant) -> Config {
    // Isolate durable plugin state and the encryption key per deployment.
    let mut config = Config {
        data_dir: deployment.root.path().join("data"),
        config_path: deployment.root.path().join("config.toml"),
        ..Config::default()
    };
    config.plugins.enabled = true;
    config.plugins.auto_discover = false;
    config.plugins.max_active_instances = 1;
    config.plugins.plugins_dir = deployment.plugins_dir().display().to_string();
    config
        .risk_profiles
        .insert("default".to_string(), RiskProfileConfig::default());
    config.providers.models.anthropic.insert(
        "default".to_string(),
        AnthropicModelProviderConfig::default(),
    );
    config.channels.plugin.insert(
        alias.to_string(),
        PluginChannelConfig {
            package: PACKAGE.to_string(),
            enabled: true,
        },
    );
    config.agents.insert(
        "operator".to_string(),
        AliasedAgentConfig {
            channels: vec![ChannelRef::new(format!("plugin.{alias}"))],
            model_provider: ModelProviderRef::new("anthropic.default"),
            risk_profile: "default".into(),
            ..AliasedAgentConfig::default()
        },
    );
    // Without a peer group the host drops every inbound message at its sender
    // check, so the positive cases would time out rather than prove anything.
    config.peer_groups.insert(
        "gateway_peers".to_string(),
        PeerGroupConfig {
            channel: ChannelRef::new(format!("plugin.{alias}")),
            external_peers: vec![PeerUsername::new("alice")],
            ..PeerGroupConfig::default()
        },
    );
    config
        .plugins
        .entries
        .push(entry_row(deployment, alias, url, grant));
    config
}

/// Construct every configured channel through the production path.
async fn construct(config: Config) -> Vec<Arc<dyn Channel>> {
    // Validate first so a broken operator config fails as config, not as a
    // mystery empty channel list.
    config
        .validate()
        .expect("the activation declaration is valid operator config");
    zeroclaw_runtime::plugin_runtime::configured_plugin_channels(Arc::new(config), None).await
}

async fn construct_one(config: Config) -> Arc<dyn Channel> {
    let mut channels = construct(config).await;
    assert_eq!(
        channels.len(),
        1,
        "the configured fixture must construct exactly one channel"
    );
    channels.pop().expect("one channel")
}

// ── the supervised listener ───────────────────────────────────────

/// A running `Channel::listen`, driven the way the orchestrator drives it.
struct Listening {
    messages: mpsc::Receiver<ChannelMessage>,
    task: JoinHandle<()>,
}

fn listen(channel: &Arc<dyn Channel>) -> Listening {
    let (tx, messages) = mpsc::channel(8);
    let listener = Arc::clone(channel);
    let task = zeroclaw_spawn::spawn!(async move {
        listener
            .listen(tx)
            .await
            .expect("the plugin listener returns cleanly once its receiver closes");
    });
    Listening { messages, task }
}

impl Listening {
    async fn next(&mut self) -> ChannelMessage {
        tokio::time::timeout(STEP, self.messages.recv())
            .await
            .expect("an inbound message arrives before the deadline")
            .expect("the listener is still connected")
    }

    /// Stop the way a closed receiver stops it: `listen` returns normally.
    async fn close(self) {
        let Listening { messages, task } = self;
        drop(messages);
        tokio::time::timeout(STEP, task)
            .await
            .expect("listener exits after its receiver closes")
            .expect("listener task joins cleanly");
    }

    /// Stop the way the supervisor retires a channel: drop the future mid-poll.
    async fn cancel(self) {
        let Listening { messages, task } = self;
        task.abort();
        let joined = task.await;
        assert!(
            joined.is_err_and(|error| error.is_cancelled()),
            "aborting the listener must cancel it"
        );
        drop(messages);
    }
}

// ── loopback gateway ──────────────────────────────────────────────

enum ServerCommand {
    Text(String),
    Close(CloseCode),
}

#[derive(Default)]
struct Observed {
    /// One command sender per accepted connection, in upgrade order.
    peers: Vec<mpsc::UnboundedSender<ServerCommand>>,
    /// Text frames received, tagged with the 1-based connection number.
    frames: Vec<(usize, String)>,
    /// Connections whose stream ended, by number.
    ended: Vec<usize>,
}

struct Shared {
    /// TCP accepts, counted before the upgrade: a packet reached the listener.
    accepted: AtomicUsize,
    observed: Mutex<Observed>,
    changed: Notify,
}

/// A `ws://` server that records what it sees and does what it is told.
struct Gateway {
    port: u16,
    shared: Arc<Shared>,
}

impl Gateway {
    async fn start() -> Self {
        let listener = TcpListener::bind("127.0.0.1:0")
            .await
            .expect("bind loopback gateway");
        let port = listener.local_addr().expect("local addr").port();
        let shared = Arc::new(Shared {
            accepted: AtomicUsize::new(0),
            observed: Mutex::new(Observed::default()),
            changed: Notify::new(),
        });
        let acceptor = Arc::clone(&shared);
        zeroclaw_spawn::spawn!(async move {
            loop {
                let Ok((stream, _)) = listener.accept().await else {
                    break;
                };
                acceptor.accepted.fetch_add(1, Ordering::SeqCst);
                acceptor.changed.notify_waiters();
                let peer = Arc::clone(&acceptor);
                zeroclaw_spawn::spawn!(async move { serve(stream, peer).await });
            }
        });
        Self { port, shared }
    }

    fn url(&self) -> String {
        format!("ws://127.0.0.1:{}/gateway", self.port)
    }

    fn accepted(&self) -> usize {
        self.shared.accepted.load(Ordering::SeqCst)
    }

    fn frames(&self) -> Vec<(usize, String)> {
        self.observed().frames.clone()
    }

    fn observed(&self) -> std::sync::MutexGuard<'_, Observed> {
        self.shared
            .observed
            .lock()
            .unwrap_or_else(std::sync::PoisonError::into_inner)
    }

    fn command(&self, connection: usize, command: ServerCommand) {
        let observed = self.observed();
        let peer = observed
            .peers
            .get(connection - 1)
            .expect("the connection was accepted");
        peer.send(command)
            .expect("the connection task is still serving");
    }

    fn send(&self, connection: usize, text: &str) {
        self.command(connection, ServerCommand::Text(text.to_string()));
    }

    fn close(&self, connection: usize, code: CloseCode) {
        self.command(connection, ServerCommand::Close(code));
    }

    /// Wait until `predicate` holds over what the server observed.
    async fn wait(&self, what: &str, predicate: impl Fn(&Observed) -> bool) {
        let deadline = tokio::time::Instant::now() + STEP;
        loop {
            // Created before the check so a notification between the check
            // and the await is not lost.
            let notified = self.shared.changed.notified();
            let satisfied = predicate(&self.observed());
            if satisfied {
                return;
            }
            let remaining = deadline.saturating_duration_since(tokio::time::Instant::now());
            assert!(!remaining.is_zero(), "timed out waiting for {what}");
            let _ = tokio::time::timeout(remaining, notified).await;
        }
    }

    async fn connection(&self, number: usize) {
        self.wait(&format!("connection {number}"), |observed| {
            observed.peers.len() >= number
        })
        .await;
    }

    async fn ended(&self, number: usize) {
        self.wait(&format!("the end of connection {number}"), |observed| {
            observed.ended.contains(&number)
        })
        .await;
    }

    async fn frame(&self, number: usize, text: &str) {
        self.wait(
            &format!("frame {text:?} on connection {number}"),
            |observed| {
                observed
                    .frames
                    .iter()
                    .any(|(connection, frame)| *connection == number && frame == text)
            },
        )
        .await;
    }
}

/// Serve one accepted connection until its stream ends.
async fn serve(stream: TcpStream, shared: Arc<Shared>) {
    let Ok(mut socket) = tokio_tungstenite::accept_async(stream).await else {
        return;
    };
    let (commands, mut inbox) = mpsc::unbounded_channel();
    let number = {
        let mut observed = shared
            .observed
            .lock()
            .unwrap_or_else(std::sync::PoisonError::into_inner);
        observed.peers.push(commands);
        observed.peers.len()
    };
    shared.changed.notify_waiters();

    loop {
        tokio::select! {
            command = inbox.recv() => match command {
                Some(ServerCommand::Text(text)) => {
                    if socket.send(Message::Text(text.into())).await.is_err() {
                        break;
                    }
                }
                Some(ServerCommand::Close(code)) => {
                    let frame = CloseFrame {
                        code,
                        reason: "test close".into(),
                    };
                    if socket.close(Some(frame)).await.is_err() {
                        break;
                    }
                }
                None => break,
            },
            frame = socket.next() => match frame {
                Some(Ok(Message::Text(text))) => {
                    shared
                        .observed
                        .lock()
                        .unwrap_or_else(std::sync::PoisonError::into_inner)
                        .frames
                        .push((number, text.as_str().to_string()));
                    shared.changed.notify_waiters();
                }
                // A peer close, a reset (Windows may send RST), or the end of
                // the stream all mean the same thing here: the socket is gone.
                Some(Ok(Message::Close(_))) | Some(Err(_)) | None => break,
                Some(Ok(_)) => {}
            },
        }
    }

    shared
        .observed
        .lock()
        .unwrap_or_else(std::sync::PoisonError::into_inner)
        .ended
        .push(number);
    shared.changed.notify_waiters();
}

// ── the proofs ────────────────────────────────────────────────────

/// Granted: one socket carries inbound frames and outbound sends across many
/// polls, and it belongs to the channel's warm store rather than to one
/// `listen` call.
#[tokio::test]
async fn a_granted_socket_carries_inbound_and_outbound_across_polls() {
    let deployment = Deployment::install(&canonical_manifest());
    let gateway = Gateway::start().await;
    let config = activation_config(&deployment, "ws_inbound", &gateway.url(), &LOOPBACK);
    let channel = construct_one(config).await;

    assert_eq!(channel.name(), "plugin");
    assert_eq!(channel.alias(), "ws_inbound");
    // The fixture dials lazily, so construction opens no socket. This pins the
    // fixture's contract with the test, not host policy.
    assert_eq!(gateway.accepted(), 0, "construction must not dial");
    assert!(
        !channel.health_check().await,
        "no socket exists before the first poll"
    );

    let mut listening = listen(&channel);
    gateway.connection(1).await;
    gateway.send(1, "msg:alice:hello");
    let first = listening.next().await;
    assert_eq!(first.id, "ws-1-1");
    assert_eq!(first.sender, "alice");
    assert_eq!(first.reply_target, "alice");
    assert_eq!(first.content, "hello");
    assert_eq!(first.channel, "plugin", "routing identity is host-issued");
    assert_eq!(first.channel_alias.as_deref(), Some("ws_inbound"));

    // Several empty polls pass; the socket must survive them.
    tokio::time::sleep(IDLE).await;
    gateway.send(1, "msg:alice:again");
    let second = listening.next().await;
    assert_eq!(second.id, "ws-1-2", "the same socket keeps counting");
    assert!(channel.health_check().await, "the socket is live");

    channel
        .send(&SendMessage::new("pong", "alice"))
        .await
        .expect("send over the live socket");
    gateway.frame(1, "out:alice:pong").await;

    // A new listener on the same channel reuses the same socket: the socket
    // belongs to the store, not to the `listen` call that opened it.
    listening.close().await;
    let mut listening = listen(&channel);
    gateway.send(1, "msg:alice:resumed");
    let third = listening.next().await;
    assert_eq!(
        third.id, "ws-1-3",
        "the socket must outlive the listener that opened it"
    );
    listening.close().await;

    assert_eq!(gateway.accepted(), 1, "one socket for the whole session");
    assert_eq!(gateway.frames(), vec![(1, "out:alice:pong".to_string())]);
}

/// A server-initiated close is followed by exactly one redial on the same warm
/// store, and the guest sees the close code the server sent. With the
/// instance's connection budget at one (the shutdown case proves that ceiling
/// binds), the redial can only succeed if the guest released the closed
/// socket's lease.
#[tokio::test]
async fn a_server_close_is_followed_by_one_reconnect_on_the_same_store() {
    let deployment = Deployment::install(&canonical_manifest());
    let gateway = Gateway::start().await;
    let mut config = activation_config(&deployment, "ws_reconnect", &gateway.url(), &LOOPBACK);
    config.plugins.limits.max_connections_per_instance = 1;
    let channel = construct_one(config).await;
    let mut listening = listen(&channel);

    gateway.connection(1).await;
    gateway.send(1, "msg:alice:before");
    let before = listening.next().await;
    assert_eq!(before.id, "ws-1-1");
    assert_eq!(before.subject, None, "no socket has closed yet");

    gateway.close(1, CloseCode::Restart);
    gateway.ended(1).await;
    gateway.connection(2).await;
    gateway.send(2, "msg:alice:after");
    let after = listening.next().await;
    assert_eq!(
        after.id, "ws-2-2",
        "a second socket on the same store: the message counter kept going"
    );
    assert_eq!(
        after.subject.as_deref(),
        Some("close:service-restart"),
        "1012 reaches the guest as its named close code"
    );
    assert_eq!(gateway.accepted(), 2);

    // An application close code reaches the guest as `other(<code>)`, and a
    // second redial still fits the budget of one.
    gateway.close(2, CloseCode::Library(4000));
    gateway.ended(2).await;
    gateway.connection(3).await;
    gateway.send(3, "msg:alice:third");
    let third = listening.next().await;
    assert_eq!(third.id, "ws-3-3");
    assert_eq!(third.subject.as_deref(), Some("close:other:4000"));

    listening.close().await;
}

/// Retiring the listener and dropping the channel closes its socket and frees
/// its connection lease. Under a budget of one, a replacement built while the
/// first channel holds the slot cannot dial, which proves the ceiling binds
/// across independently built channels of one instance; once the first
/// channel is gone, the replacement dials.
#[tokio::test]
async fn retiring_listen_and_the_channel_closes_the_socket_and_frees_its_lease() {
    let deployment = Deployment::install(&canonical_manifest());
    let gateway = Gateway::start().await;
    let mut config = activation_config(&deployment, "ws_shutdown", &gateway.url(), &LOOPBACK);
    config.plugins.limits.max_connections_per_instance = 1;
    let channel = construct_one(config.clone()).await;
    let mut listening = listen(&channel);

    gateway.connection(1).await;
    gateway.send(1, "msg:alice:live");
    assert_eq!(listening.next().await.id, "ws-1-1");

    // The control: the live socket holds the instance's only slot.
    let replacement = construct_one(config).await;
    let refused = replacement
        .send(&SendMessage::new("hello", "alice"))
        .await
        .expect_err("the instance's only connection slot is taken");
    assert!(
        format!("{refused:#}").contains("connection-limit"),
        "the budget, not the grant, refuses the second socket; got: {refused:#}"
    );
    assert_eq!(
        gateway.accepted(),
        1,
        "a refused dial never reaches the server"
    );

    // The supervisor's shutdown: the poll future is dropped wherever it is,
    // then the channel itself goes away.
    listening.cancel().await;
    drop(channel);
    tokio::time::timeout(SHUTDOWN, gateway.ended(1))
        .await
        .expect("dropping the channel must close its socket");

    // With the lease released, the replacement's first dial succeeds. It is a
    // fresh store, so its ids start again.
    let mut listening = listen(&replacement);
    gateway.connection(2).await;
    gateway.send(2, "msg:alice:back");
    assert_eq!(
        listening.next().await.id,
        "ws-1-1",
        "the replacement is a fresh store"
    );
    listening.close().await;
    assert_eq!(gateway.accepted(), 2);
}

/// Refused before the network: the channel constructs (the import is linked),
/// `send` reports the policy's verdict, health stays false, and the listener
/// never sees a packet.
async fn assert_refused_before_the_network(alias: &str, grant: Grant) {
    let deployment = Deployment::install(&canonical_manifest());
    let gateway = Gateway::start().await;
    let config = activation_config(&deployment, alias, &gateway.url(), &grant);
    let channel = construct_one(config).await;

    let refused = channel
        .send(&SendMessage::new("hello", "alice"))
        .await
        .expect_err("an ungranted destination must be refused");
    assert!(
        format!("{refused:#}").contains("destination-denied"),
        "the guest must see the policy verdict; got: {refused:#}"
    );

    let listening = listen(&channel);
    tokio::time::sleep(IDLE).await;
    // The fixture reports health as "holds a socket", so this pins the
    // fixture's contract with the test: no refused poll left a socket behind.
    assert!(
        !channel.health_check().await,
        "a refused channel must not hold a socket"
    );
    assert_eq!(
        gateway.accepted(),
        0,
        "a refused connect must never reach the listener"
    );
    listening.close().await;
}

/// The grant names a different host, so the same store reaches nothing.
#[tokio::test]
async fn a_grant_for_a_different_host_is_refused_before_the_network() {
    assert_refused_before_the_network(
        "ws_other_host",
        Grant {
            hosts: &["gateway.example.com"],
            private: &[],
        },
    )
    .await;
}

/// The host is granted but its private address class is not, which is what
/// the positive cases have to open explicitly.
#[tokio::test]
async fn a_granted_host_without_the_private_carveout_is_refused() {
    assert_refused_before_the_network(
        "ws_no_private",
        Grant {
            hosts: &["127.0.0.1"],
            private: &[],
        },
    )
    .await;
}

/// The same component bytes under a manifest without `websocket_client` never
/// construct: the linker leaves the import out, so instantiation fails and the
/// loader skips the channel. Admission does not inspect imports, so this is
/// the first point where the missing grant bites.
#[tokio::test]
async fn a_manifest_without_websocket_client_never_constructs() {
    let gateway = Gateway::start().await;

    let granted = Deployment::install(&canonical_manifest());
    let config = activation_config(&granted, "ws_ungranted", &gateway.url(), &LOOPBACK);
    assert_eq!(
        construct(config).await.len(),
        1,
        "the control deployment constructs its channel"
    );

    let ungranted = Deployment::install(&manifest_without_websocket_grant());
    let config = activation_config(&ungranted, "ws_ungranted", &gateway.url(), &LOOPBACK);
    assert!(
        construct(config).await.is_empty(),
        "a component importing websocket must not construct without the grant"
    );
    assert_eq!(gateway.accepted(), 0, "neither deployment dialed");
}

/// A frame from a sender outside the peer group is mapped by the guest and
/// dropped by the host, so the next delivered id has a gap.
#[tokio::test]
async fn a_sender_outside_the_peer_group_is_dropped_at_the_host() {
    let deployment = Deployment::install(&canonical_manifest());
    let gateway = Gateway::start().await;
    let config = activation_config(&deployment, "ws_senders", &gateway.url(), &LOOPBACK);
    let channel = construct_one(config).await;
    let mut listening = listen(&channel);

    gateway.connection(1).await;
    gateway.send(1, "msg:mallory:let me in");
    gateway.send(1, "msg:alice:hello");
    let delivered = listening.next().await;
    assert_eq!(delivered.sender, "alice");
    assert_eq!(
        delivered.id, "ws-1-2",
        "the guest mapped mallory's frame first; the host dropped it"
    );
    assert!(
        listening.messages.try_recv().is_err(),
        "nothing else reaches the agent queue"
    );
    listening.close().await;
}
