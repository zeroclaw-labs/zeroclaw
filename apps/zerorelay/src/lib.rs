//! The ZeroClaw nominated relay: a standalone **blind forwarder**.
//!
//! Each party reaches the relay over an **outer** TLS + WebSocket session
//! (`zeroclaw.relay.v1`). A daemon opens one persistent WS and registers a
//! `node_id` through a signed Ed25519 handshake; many client connections are then
//! multiplexed over that single WS by `conn_id`. A client opens its own WS, names
//! a target `node_id`, and once paired the relay shuttles binary `DATA` messages
//! between client and daemon. Those `DATA` payloads are the **inner** client<->
//! daemon mTLS: the relay terminates only the outer TLS, never the inner session,
//! holds no CA or daemon key material, and routes purely on the opaque `node_id`.
//!
//! Admission (open vs allowlist) is keyed on the daemon's registration pubkey
//! fingerprint; deny always wins. A node-id is bound to the first registrant's
//! pubkey, so a different key cannot hijack a live node-id. These are operational
//! controls on the rendezvous, not RPC authorization, and do not weaken the
//! blind-forwarder property (the inner mTLS still rejects any unauthenticated
//! client at the daemon).
//!
//! `zerorelay` is a standalone networking app (not daemon-path code), so bare
//! `tokio::spawn` is the right primitive here; the `zeroclaw_spawn::spawn!` rule
//! is for in-daemon tasks. Mirrors the `apps/zerocode` exemption.
#![allow(clippy::disallowed_methods)]

mod frontdoor;
mod frontdoor_assets;
mod frontdoor_tls_assets;

use anyhow::Result;
use base64::Engine as _;
use base64::engine::general_purpose::STANDARD as B64;
use futures_util::{SinkExt, StreamExt};
use ring::rand::{SecureRandom, SystemRandom};
use ring::signature::{ED25519, UnparsedPublicKey};
use serde::{Deserialize, Serialize};
use sha2::{Digest, Sha256};
use std::collections::{HashMap, HashSet};
use std::net::IpAddr;
use std::sync::Arc;
use std::sync::atomic::{AtomicU64, Ordering};
use std::time::Duration;
use tokio::io::{AsyncRead, AsyncWrite};
use tokio::net::TcpListener;
use tokio::sync::{Mutex, mpsc};
use tokio_rustls::TlsAcceptor;
use tokio_tungstenite::WebSocketStream;
use tokio_tungstenite::tungstenite::Message;
use zeroclaw_relay_proto::{
    ConnWindow, Control, INITIAL_WINDOW, MAX_DATA_PAYLOAD, PEER_HINT_ENROLL, TokenBucket,
    decode_data, encode_data,
};

/// How far a client may drive its send window negative before the relay treats it
/// as ignoring flow control and tears the conn down. One full window of slack
/// absorbs acks still in flight; beyond that the client is flooding (A6).
const RELAY_OVERRUN_TOLERANCE: u64 = INITIAL_WINDOW as u64;

/// How long a freshly connected client waits to be paired with the daemon before
/// the relay gives up and drops it.
const PAIR_TIMEOUT: Duration = Duration::from_secs(15);

/// Which daemons may register a rendezvous on this relay.
#[derive(Debug, Clone, PartialEq, Eq)]
pub enum Admission {
    /// Any daemon that passes the deny list (and optional relay-token gate) and
    /// completes the signed handshake may register.
    Open,
    /// Only daemons whose pubkey fingerprint is on the allow list may register.
    Allowlist,
}

/// The hot-reloadable slice of relay policy: who may register and the optional
/// shared-secret gate. Swapped atomically on SIGHUP so an operator can edit the
/// allow/deny lists without dropping live connections. Deny always wins.
#[derive(Debug, Clone)]
pub struct AdmissionPolicy {
    pub registration_mode: Admission,
    /// Daemon pubkey fingerprints (sha256 hex) allowed to register (Allowlist).
    pub allow: HashSet<String>,
    /// Daemon pubkey fingerprints always rejected.
    pub deny: HashSet<String>,
    /// Optional shared-secret gate presented in `Hello.relay_token`.
    pub relay_token: Option<String>,
}

impl Default for AdmissionPolicy {
    fn default() -> Self {
        Self {
            registration_mode: Admission::Open,
            allow: HashSet::new(),
            deny: HashSet::new(),
            relay_token: None,
        }
    }
}

impl AdmissionPolicy {
    /// True when `fpr` may register: not denied, and either open mode or on the
    /// allow list. Deny always wins.
    fn admit(&self, fpr: &str) -> bool {
        if self.deny.contains(fpr) {
            return false;
        }
        match self.registration_mode {
            Admission::Open => true,
            Admission::Allowlist => self.allow.contains(fpr),
        }
    }
}

/// Relay admission + abuse policy. Deny always wins.
#[derive(Debug, Clone)]
pub struct RelayConfig {
    pub registration_mode: Admission,
    /// Daemon pubkey fingerprints (sha256 hex) allowed to register (Allowlist).
    pub allow: HashSet<String>,
    /// Daemon pubkey fingerprints always rejected.
    pub deny: HashSet<String>,
    /// Optional shared-secret gate presented in `Hello.relay_token`.
    pub relay_token: Option<String>,
    /// Lease advertised to daemons; renewal is the persistent WS staying alive
    /// (with keepalive). Advisory in v1; WS liveness is the real cleanup signal.
    pub lease_ttl: Duration,
    /// Cap on simultaneously-open client connections per node-id.
    pub max_conns_per_node: usize,
    /// Drop a client connection after this much inactivity.
    pub idle_timeout: Duration,
    /// Per-source-IP connection-handshake rate cap (A6): burst allowance and
    /// steady refill per second. Excess connections from one IP are dropped
    /// before the WebSocket handshake.
    pub accept_burst_per_ip: u32,
    pub accept_rate_per_ip: f64,
    /// Per-node-id client-connect rate cap (A6): burst + refill per second. Excess
    /// `Connect`s to one node-id get `rate_limited`.
    pub connect_burst_per_node: u32,
    pub connect_rate_per_node: f64,
    /// Outer-mTLS variant: when an outer client cert is presented and its subject
    /// CN names a node-id, route to THAT node, falling back to the `Connect` frame.
    /// Off by default (a client cert whose CN is not a node-id would misroute). The
    /// outer client-cert REQUIREMENT itself is configured on the TLS acceptor.
    pub route_by_client_cert: bool,
}

impl RelayConfig {
    /// The admission slice (the part that hot-reloads on SIGHUP).
    pub fn admission_policy(&self) -> AdmissionPolicy {
        AdmissionPolicy {
            registration_mode: self.registration_mode.clone(),
            allow: self.allow.clone(),
            deny: self.deny.clone(),
            relay_token: self.relay_token.clone(),
        }
    }
}

impl Default for RelayConfig {
    fn default() -> Self {
        Self {
            registration_mode: Admission::Open,
            allow: HashSet::new(),
            deny: HashSet::new(),
            relay_token: None,
            lease_ttl: Duration::from_secs(300),
            max_conns_per_node: 256,
            idle_timeout: Duration::from_secs(300),
            accept_burst_per_ip: 30,
            accept_rate_per_ip: 10.0,
            connect_burst_per_node: 60,
            connect_rate_per_node: 20.0,
            route_by_client_cert: false,
        }
    }
}

/// One control event routed from the daemon link toward a waiting client task.
enum ConnEvent {
    /// Daemon accepted the `Open`; the route is live.
    Opened,
    /// Inner payload bytes from the daemon for this connection.
    Data(Vec<u8>),
    /// Daemon (or relay) is closing this connection.
    Close(String),
    /// Daemon -> client `Window { credit }`: (re)establish the client's send
    /// window for this conn (forwarded to the client; also seeds the relay guard).
    Window(u32),
    /// Daemon -> client `DataAck { consumed }`: replenish the client's send window.
    Ack(u32),
}

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
enum ClientRoute {
    Wss,
    Enrollment,
}

impl ClientRoute {
    fn peer_hint(self) -> Option<&'static str> {
        match self {
            Self::Wss => None,
            Self::Enrollment => Some(PEER_HINT_ENROLL),
        }
    }
}

/// Per-node usage counters for the read-only status surface. Counts only - never
/// payload bytes (the relay must not log or store DATA content).
#[derive(Debug, Default)]
struct NodeMetrics {
    /// Client connections opened against this node over its lifetime.
    conns_total: AtomicU64,
    /// Client connections currently live.
    conns_live: AtomicU64,
    /// `DATA` frames forwarded in either direction (a count, never bytes).
    frames_relayed: AtomicU64,
    /// `Connect`s rejected by the per-node rate cap.
    connects_rejected: AtomicU64,
}

/// Counts a live client conn against a node for as long as it exists: increments
/// `conns_total` + `conns_live` on construction and decrements `conns_live` on
/// drop, so every exit path (pair timeout, Open failure, normal teardown) keeps
/// the live count exact.
struct LiveConnGuard(Arc<NodeMetrics>);

impl LiveConnGuard {
    fn new(metrics: Arc<NodeMetrics>) -> Self {
        metrics.conns_total.fetch_add(1, Ordering::Relaxed);
        metrics.conns_live.fetch_add(1, Ordering::Relaxed);
        Self(metrics)
    }
}

impl Drop for LiveConnGuard {
    fn drop(&mut self) {
        self.0.conns_live.fetch_sub(1, Ordering::Relaxed);
    }
}

/// A point-in-time view of one node's routing + usage, for `zerorelay status`.
#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct NodeStatus {
    pub node_id: String,
    pub fpr: String,
    pub conns_live: u64,
    pub conns_total: u64,
    pub frames_relayed: u64,
    pub connects_rejected: u64,
}

/// A read-only snapshot of the relay's live routing table + per-node counters.
#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct RelayStatus {
    pub nodes: Vec<NodeStatus>,
}

/// A registered daemon's routing handle.
struct DaemonHandle {
    /// Pubkey fingerprint that owns this node-id (anti-hijack binding).
    fpr: String,
    /// Registration epoch; teardown only deregisters if still current, so a
    /// superseded link cannot evict the daemon that replaced it.
    epoch: u64,
    /// Serialized outbound channel to the daemon's WS writer task.
    to_daemon: mpsc::Sender<Message>,
    /// Live client connections multiplexed over this daemon link.
    conns: Arc<Mutex<HashMap<u64, mpsc::Sender<ConnEvent>>>>,
    /// Per-node usage counters (status surface).
    metrics: Arc<NodeMetrics>,
    /// Per-node client-connect rate limiter (A6).
    connect_bucket: Arc<Mutex<TokenBucket>>,
}

/// How many distinct source IPs the accept-rate map tracks before it prunes idle
/// (full-bucket) entries, so the map itself cannot grow unboundedly under a
/// spoofed-source flood.
const MAX_TRACKED_IPS: usize = 4096;

struct Inner {
    /// Hot-reloadable admission slice (swapped on SIGHUP). A `std::sync::RwLock`
    /// is fine: reads are brief and never held across an await.
    admission: std::sync::RwLock<Arc<AdmissionPolicy>>,
    /// Static operational knobs (not hot-reloaded).
    lease_ttl: Duration,
    max_conns_per_node: usize,
    idle_timeout: Duration,
    /// Per-source-IP connection-handshake rate limiter state + parameters (A6).
    ip_buckets: std::sync::Mutex<HashMap<IpAddr, TokenBucket>>,
    accept_burst_per_ip: u32,
    accept_rate_per_ip: f64,
    connect_burst_per_node: u32,
    connect_rate_per_node: f64,
    /// Outer-mTLS variant: read the target node-id from the client cert CN.
    route_by_client_cert: bool,
    daemons: Mutex<HashMap<String, DaemonHandle>>,
    next_conn: AtomicU64,
    next_epoch: AtomicU64,
}

impl Inner {
    /// Snapshot the current admission policy (cheap `Arc` clone).
    fn admission(&self) -> Arc<AdmissionPolicy> {
        self.admission.read().expect("admission lock").clone()
    }

    /// Admit one connection from `ip` under the per-source handshake rate cap.
    /// Returns false when that IP is over its rate (the caller drops the socket).
    /// Prunes idle (refilled-to-full) entries when the map grows large.
    fn admit_ip(&self, ip: IpAddr) -> bool {
        let now = std::time::Instant::now();
        let mut map = self.ip_buckets.lock().expect("ip bucket lock");
        if map.len() > MAX_TRACKED_IPS {
            map.retain(|_, b| !b.is_full_at(now));
        }
        map.entry(ip)
            .or_insert_with(|| {
                TokenBucket::new_at(self.accept_burst_per_ip, self.accept_rate_per_ip, now)
            })
            .try_take_at(now)
    }
}

/// A running relay. Cheap to clone (`Arc` inside).
#[derive(Clone)]
pub struct RelayServer {
    inner: Arc<Inner>,
}

impl RelayServer {
    pub fn new(cfg: RelayConfig) -> Self {
        Self {
            inner: Arc::new(Inner {
                admission: std::sync::RwLock::new(Arc::new(cfg.admission_policy())),
                lease_ttl: cfg.lease_ttl,
                max_conns_per_node: cfg.max_conns_per_node,
                idle_timeout: cfg.idle_timeout,
                ip_buckets: std::sync::Mutex::new(HashMap::new()),
                accept_burst_per_ip: cfg.accept_burst_per_ip,
                accept_rate_per_ip: cfg.accept_rate_per_ip,
                connect_burst_per_node: cfg.connect_burst_per_node,
                connect_rate_per_node: cfg.connect_rate_per_node,
                route_by_client_cert: cfg.route_by_client_cert,
                daemons: Mutex::new(HashMap::new()),
                next_conn: AtomicU64::new(1),
                next_epoch: AtomicU64::new(1),
            }),
        }
    }

    /// Swap the admission policy live (SIGHUP reload). Existing connections are
    /// untouched; the new policy applies to subsequent registrations.
    pub fn reload_admission(&self, policy: AdmissionPolicy) {
        *self.inner.admission.write().expect("admission lock") = Arc::new(policy);
    }

    /// A read-only snapshot of the live routing table + per-node counters. Counts
    /// only (no payloads). Drives `zerorelay status` / the SIGUSR1 dump.
    pub async fn status(&self) -> RelayStatus {
        let daemons = self.inner.daemons.lock().await;
        let mut nodes: Vec<NodeStatus> = daemons
            .iter()
            .map(|(node_id, h)| NodeStatus {
                node_id: node_id.clone(),
                fpr: h.fpr.clone(),
                conns_live: h.metrics.conns_live.load(Ordering::Relaxed),
                conns_total: h.metrics.conns_total.load(Ordering::Relaxed),
                frames_relayed: h.metrics.frames_relayed.load(Ordering::Relaxed),
                connects_rejected: h.metrics.connects_rejected.load(Ordering::Relaxed),
            })
            .collect();
        nodes.sort_by(|a, b| a.node_id.cmp(&b.node_id));
        RelayStatus { nodes }
    }

    /// Accept TLS + WebSocket connections forever, dispatching daemon vs client.
    pub async fn serve(self, listener: TcpListener, acceptor: TlsAcceptor) -> Result<()> {
        loop {
            let (sock, peer) = match listener.accept().await {
                Ok(s) => s,
                Err(_) => continue,
            };
            // Per-source-IP handshake rate cap (A6): drop a flooding IP before
            // spending a TLS handshake on it.
            if !self.inner.admit_ip(peer.ip()) {
                drop(sock);
                continue;
            }
            let inner = self.inner.clone();
            let acceptor = acceptor.clone();
            tokio::spawn(async move {
                let tls = match acceptor.accept(sock).await {
                    Ok(t) => t,
                    Err(_) => return,
                };
                // Outer-mTLS variant: read a target node-id from the peer's outer
                // client cert CN (when configured + a cert was presented), before
                // the TlsStream is consumed by the WS handshake. None otherwise.
                let cert_node_id = if inner.route_by_client_cert {
                    tls.get_ref()
                        .1
                        .peer_certificates()
                        .and_then(|c| c.first())
                        .and_then(|c| zeroclaw_tls::client_cert_node_id(c.as_ref()))
                } else {
                    None
                };
                let ws = match frontdoor::accept_or_serve(tls).await {
                    Ok(frontdoor::Frontdoor::WebSocket(w)) => w,
                    Ok(frontdoor::Frontdoor::ServedHttp) => return,
                    Err(_) => return,
                };
                let _ = handle_conn(inner, *ws, cert_node_id).await;
            });
        }
    }
}

/// The target node-id for a client: the outer client cert CN (outer-mTLS variant)
/// when present, otherwise the `Connect` frame's node-id. Additive - the frame is
/// always the fallback and the inner mTLS is never touched.
fn resolve_target_node(cert_node_id: Option<String>, frame_node_id: String) -> String {
    cert_node_id
        .filter(|s| !s.is_empty())
        .unwrap_or(frame_node_id)
}

/// Read the first control frame and dispatch by role. `cert_node_id` is the
/// outer-mTLS-derived target (outer client cert CN), used only on the client path.
async fn handle_conn<S>(
    inner: Arc<Inner>,
    mut ws: WebSocketStream<S>,
    cert_node_id: Option<String>,
) -> Result<()>
where
    S: AsyncRead + AsyncWrite + Unpin + Send + 'static,
{
    match next_control(&mut ws).await {
        Some(Control::Hello {
            daemon_pubkey,
            node_id,
            relay_token,
        }) => handle_daemon(inner, ws, daemon_pubkey, node_id, relay_token).await,
        Some(Control::Connect { node_id }) => {
            handle_client(
                inner,
                ws,
                resolve_target_node(cert_node_id, node_id),
                ClientRoute::Wss,
            )
            .await
        }
        Some(Control::Enroll { node_id }) => {
            handle_client(
                inner,
                ws,
                resolve_target_node(cert_node_id, node_id),
                ClientRoute::Enrollment,
            )
            .await
        }
        Some(other) => {
            let _ = send_control(
                &mut ws,
                &Control::error("bad_first_frame", format!("unexpected {other:?}")),
            )
            .await;
            Ok(())
        }
        None => Ok(()),
    }
}

/// Daemon control connection: signed admission, then multiplex client conns.
async fn handle_daemon<S>(
    inner: Arc<Inner>,
    mut ws: WebSocketStream<S>,
    daemon_pubkey: String,
    node_id: String,
    relay_token: Option<String>,
) -> Result<()>
where
    S: AsyncRead + AsyncWrite + Unpin + Send + 'static,
{
    // Snapshot the admission policy once so the token gate and the allow/deny
    // check are consistent even if a SIGHUP reload lands mid-registration.
    let policy = inner.admission();

    // Optional shared-secret gate.
    if let Some(required) = &policy.relay_token
        && relay_token.as_deref() != Some(required.as_str())
    {
        let _ = send_control(&mut ws, &Control::error("forbidden", "bad relay token")).await;
        return Ok(());
    }

    let pubkey = match B64.decode(daemon_pubkey.as_bytes()) {
        Ok(k) => k,
        Err(_) => {
            let _ = send_control(&mut ws, &Control::error("bad_pubkey", "not base64")).await;
            return Ok(());
        }
    };
    let fpr = hex::encode(Sha256::digest(&pubkey));
    if !policy.admit(&fpr) {
        let _ = send_control(&mut ws, &Control::error("forbidden", "registration denied")).await;
        return Ok(());
    }

    // Challenge / verify: prove possession of the private key over a fresh nonce.
    let mut nonce = [0u8; 32];
    if SystemRandom::new().fill(&mut nonce).is_err() {
        let _ = send_control(&mut ws, &Control::error("internal", "rng")).await;
        return Ok(());
    }
    send_control(
        &mut ws,
        &Control::Challenge {
            nonce: B64.encode(nonce),
        },
    )
    .await?;
    let (reg_node, sig_b64) = match next_control(&mut ws).await {
        Some(Control::Register { node_id, sig }) => (node_id, sig),
        _ => {
            let _ = send_control(
                &mut ws,
                &Control::error("bad_register", "expected register"),
            )
            .await;
            return Ok(());
        }
    };
    if reg_node != node_id {
        let _ = send_control(&mut ws, &Control::error("bad_register", "node_id mismatch")).await;
        return Ok(());
    }
    let sig = match B64.decode(sig_b64.as_bytes()) {
        Ok(s) => s,
        Err(_) => {
            let _ = send_control(&mut ws, &Control::error("bad_sig", "not base64")).await;
            return Ok(());
        }
    };
    if UnparsedPublicKey::new(&ED25519, &pubkey)
        .verify(&nonce, &sig)
        .is_err()
    {
        let _ = send_control(&mut ws, &Control::error("bad_sig", "signature invalid")).await;
        return Ok(());
    }

    // node-id <-> pubkey binding + last-writer-wins registration.
    let epoch = inner.next_epoch.fetch_add(1, Ordering::Relaxed);
    let (to_daemon, mut from_clients) = mpsc::channel::<Message>(256);
    let conns: Arc<Mutex<HashMap<u64, mpsc::Sender<ConnEvent>>>> =
        Arc::new(Mutex::new(HashMap::new()));
    {
        let mut daemons = inner.daemons.lock().await;
        if let Some(existing) = daemons.get(&node_id)
            && existing.fpr != fpr
        {
            drop(daemons);
            let _ = send_control(
                &mut ws,
                &Control::error("node_taken", "node-id bound to another key"),
            )
            .await;
            return Ok(());
        }
        daemons.insert(
            node_id.clone(),
            DaemonHandle {
                fpr: fpr.clone(),
                epoch,
                to_daemon: to_daemon.clone(),
                conns: conns.clone(),
                metrics: Arc::new(NodeMetrics::default()),
                connect_bucket: Arc::new(Mutex::new(TokenBucket::new(
                    inner.connect_burst_per_node,
                    inner.connect_rate_per_node,
                ))),
            },
        );
    }
    send_control(
        &mut ws,
        &Control::Registered {
            node_id: node_id.clone(),
            lease_ttl_secs: inner.lease_ttl.as_secs(),
        },
    )
    .await?;

    let (mut sink, mut stream) = ws.split();

    // Writer task: the single serialization point for everything sent to the
    // daemon (client Opens/Data/Closes + our Pongs).
    let writer = tokio::spawn(async move {
        while let Some(msg) = from_clients.recv().await {
            if sink.send(msg).await.is_err() {
                break;
            }
        }
        let _ = sink.close().await;
    });

    // Reader loop: demultiplex daemon -> client.
    while let Some(msg) = stream.next().await {
        match msg {
            Ok(Message::Text(t)) => match Control::from_json(&t) {
                Ok(Control::Opened { conn_id }) => {
                    if let Some(tx) = conns.lock().await.get(&conn_id) {
                        let _ = tx.send(ConnEvent::Opened).await;
                    }
                }
                Ok(Control::Close { conn_id, reason }) => {
                    if let Some(tx) = conns.lock().await.remove(&conn_id) {
                        let _ = tx.send(ConnEvent::Close(reason)).await;
                    }
                }
                // Daemon -> client credit-window frames: route to the conn so the
                // client task forwards them on (and the relay guard tracks them).
                Ok(Control::Window { conn_id, credit }) => {
                    if let Some(tx) = conns.lock().await.get(&conn_id) {
                        let _ = tx.send(ConnEvent::Window(credit)).await;
                    }
                }
                Ok(Control::DataAck { conn_id, consumed }) => {
                    if let Some(tx) = conns.lock().await.get(&conn_id) {
                        let _ = tx.send(ConnEvent::Ack(consumed)).await;
                    }
                }
                _ => {}
            },
            Ok(Message::Binary(b)) => {
                if let Some((conn_id, payload)) = decode_data(&b)
                    && let Some(tx) = conns.lock().await.get(&conn_id)
                {
                    let _ = tx.send(ConnEvent::Data(payload.to_vec())).await;
                }
            }
            Ok(Message::Ping(p)) => {
                let _ = to_daemon.send(Message::Pong(p)).await;
            }
            Ok(Message::Pong(_)) => {}
            Ok(Message::Close(_)) | Err(_) => break,
            _ => {}
        }
    }

    // Teardown: deregister only if still current (epoch guard), close all conns.
    {
        let mut daemons = inner.daemons.lock().await;
        if daemons.get(&node_id).map(|h| h.epoch) == Some(epoch) {
            daemons.remove(&node_id);
        }
    }
    for (_, tx) in conns.lock().await.drain() {
        let _ = tx.send(ConnEvent::Close("daemon_gone".into())).await;
    }
    writer.abort();
    Ok(())
}

/// Client connection: route it to the daemon serving `node_id` and pipe `DATA`.
async fn handle_client<S>(
    inner: Arc<Inner>,
    mut ws: WebSocketStream<S>,
    node_id: String,
    route: ClientRoute,
) -> Result<()>
where
    S: AsyncRead + AsyncWrite + Unpin + Send + 'static,
{
    let (to_daemon, conns, metrics, connect_bucket) = {
        let daemons = inner.daemons.lock().await;
        match daemons.get(&node_id) {
            Some(h) => (
                h.to_daemon.clone(),
                h.conns.clone(),
                h.metrics.clone(),
                h.connect_bucket.clone(),
            ),
            None => {
                let _ = send_control(&mut ws, &Control::error("no_such_node", node_id)).await;
                return Ok(());
            }
        }
    };

    // Per-node client-connect rate cap (A6): a flood of Connects to one node-id
    // is rejected before any conn state is allocated.
    if !connect_bucket.lock().await.try_take() {
        metrics.connects_rejected.fetch_add(1, Ordering::Relaxed);
        let _ = send_control(
            &mut ws,
            &Control::error("rate_limited", "too many connects to this node"),
        )
        .await;
        return Ok(());
    }

    let conn_id = inner.next_conn.fetch_add(1, Ordering::Relaxed);
    let (conn_tx, mut conn_rx) = mpsc::channel::<ConnEvent>(256);
    {
        let mut cs = conns.lock().await;
        if cs.len() >= inner.max_conns_per_node {
            drop(cs);
            let _ = send_control(&mut ws, &Control::error("busy", "node at capacity")).await;
            return Ok(());
        }
        cs.insert(conn_id, conn_tx);
    }
    // Account the live conn for every exit path (drops decrement conns_live).
    let _live = LiveConnGuard::new(metrics.clone());

    // Ask the daemon to open the logical connection.
    if to_daemon
        .send(Message::text(
            Control::Open {
                conn_id,
                peer_hint: route.peer_hint().map(str::to_string),
            }
            .to_json(),
        ))
        .await
        .is_err()
    {
        conns.lock().await.remove(&conn_id);
        let _ = send_control(&mut ws, &Control::error("no_such_node", "daemon gone")).await;
        return Ok(());
    }

    let (mut sink, mut stream) = ws.split();

    // Wait to be paired (daemon Opened) within the timeout.
    let paired = tokio::time::timeout(PAIR_TIMEOUT, async {
        while let Some(ev) = conn_rx.recv().await {
            match ev {
                ConnEvent::Opened => return true,
                ConnEvent::Close(_) => return false,
                // None of these should precede Opened; ignore until paired.
                ConnEvent::Data(_) | ConnEvent::Window(_) | ConnEvent::Ack(_) => {}
            }
        }
        false
    })
    .await
    .unwrap_or(false);

    if !paired {
        conns.lock().await.remove(&conn_id);
        let _ = to_daemon
            .send(Message::text(
                Control::Close {
                    conn_id,
                    reason: "pair_timeout".into(),
                }
                .to_json(),
            ))
            .await;
        let _ = sink
            .send(Message::text(
                Control::error("timeout", "daemon did not accept").to_json(),
            ))
            .await;
        return Ok(());
    }
    sink.send(Message::text(Control::Opened { conn_id }.to_json()))
        .await?;

    // Pump bytes both ways until either side closes or the conn goes idle. The
    // idle deadline is reset at the top of every iteration, so ANY frame -
    // including a credit-window grant/ack while a conn is flow-control-paused -
    // keeps the conn alive (idle is decoupled from window-block).
    //
    // `c2d_window` is a blind guard on the client->daemon direction: the daemon
    // grants/replenishes it (forwarded ConnEvent::Window/Ack) and each client
    // DATA frame debits it. A client that drives it far past zero is ignoring
    // flow control and flooding the shared link, so the relay tears the conn
    // down (A6). The relay never originates credit; it only forwards + watches.
    let idle = inner.idle_timeout;
    let mut c2d_window = ConnWindow::new(INITIAL_WINDOW);
    loop {
        let deadline = tokio::time::Instant::now() + idle;
        tokio::select! {
            _ = tokio::time::sleep_until(deadline) => break,
            ev = conn_rx.recv() => match ev {
                Some(ConnEvent::Data(payload)) => {
                    if sink.send(Message::binary(encode_data(conn_id, &payload))).await.is_err() {
                        break;
                    }
                    metrics.frames_relayed.fetch_add(1, Ordering::Relaxed);
                }
                Some(ConnEvent::Window(credit)) => {
                    c2d_window.set(credit);
                    if sink
                        .send(Message::text(Control::Window { conn_id, credit }.to_json()))
                        .await
                        .is_err()
                    {
                        break;
                    }
                }
                Some(ConnEvent::Ack(consumed)) => {
                    c2d_window.ack(consumed);
                    if sink
                        .send(Message::text(Control::DataAck { conn_id, consumed }.to_json()))
                        .await
                        .is_err()
                    {
                        break;
                    }
                }
                Some(ConnEvent::Close(reason)) => {
                    let _ = sink
                        .send(Message::text(Control::Close { conn_id, reason }.to_json()))
                        .await;
                    break;
                }
                Some(ConnEvent::Opened) => {}
                None => break,
            },
            msg = stream.next() => match msg {
                Some(Ok(Message::Binary(b))) => {
                    // Re-stamp the authoritative conn_id so a client cannot inject
                    // bytes into another connection on the shared daemon link.
                    let payload = decode_data(&b).map(|(_, p)| p).unwrap_or(&b);
                    if payload.len() > MAX_DATA_PAYLOAD {
                        break;
                    }
                    c2d_window.debit(payload.len());
                    if c2d_window.overrun() > RELAY_OVERRUN_TOLERANCE {
                        let _ = sink
                            .send(Message::text(
                                Control::error("rate_limited", "flow-control window exceeded")
                                    .to_json(),
                            ))
                            .await;
                        break;
                    }
                    if to_daemon
                        .send(Message::binary(encode_data(conn_id, payload)))
                        .await
                        .is_err()
                    {
                        break;
                    }
                    metrics.frames_relayed.fetch_add(1, Ordering::Relaxed);
                }
                Some(Ok(Message::Text(t))) => match Control::from_json(&t) {
                    Ok(Control::Close { .. }) => break,
                    // Client -> daemon credit-window frames: re-stamp the conn_id
                    // and forward so the daemon's send window stays in sync.
                    Ok(Control::Window { credit, .. }) => {
                        if to_daemon
                            .send(Message::text(Control::Window { conn_id, credit }.to_json()))
                            .await
                            .is_err()
                        {
                            break;
                        }
                    }
                    Ok(Control::DataAck { consumed, .. })
                        if to_daemon
                            .send(Message::text(
                                Control::DataAck { conn_id, consumed }.to_json(),
                            ))
                            .await
                            .is_err() =>
                    {
                        break;
                    }
                    _ => {}
                },
                Some(Ok(Message::Ping(p))) => {
                    let _ = sink.send(Message::Pong(p)).await;
                }
                Some(Ok(Message::Pong(_))) => {}
                Some(Ok(Message::Close(_))) | Some(Err(_)) | None => break,
                _ => {}
            }
        }
    }

    // Tell the daemon to release the conn and unregister it.
    let _ = to_daemon
        .send(Message::text(
            Control::Close {
                conn_id,
                reason: "client_gone".into(),
            }
            .to_json(),
        ))
        .await;
    conns.lock().await.remove(&conn_id);
    Ok(())
}

/// Send one control frame as a WS Text message.
async fn send_control<S>(ws: &mut WebSocketStream<S>, frame: &Control) -> Result<()>
where
    S: AsyncRead + AsyncWrite + Unpin,
{
    ws.send(Message::text(frame.to_json())).await?;
    Ok(())
}

/// Read the next control frame, transparently answering pings. Returns `None` on
/// close, error, or a non-text message where a control frame was expected.
async fn next_control<S>(ws: &mut WebSocketStream<S>) -> Option<Control>
where
    S: AsyncRead + AsyncWrite + Unpin,
{
    while let Some(msg) = ws.next().await {
        match msg {
            Ok(Message::Text(t)) => return Control::from_json(&t).ok(),
            Ok(Message::Ping(p)) => {
                let _ = ws.send(Message::Pong(p)).await;
            }
            Ok(Message::Pong(_)) => {}
            _ => return None,
        }
    }
    None
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn outer_cert_cn_routes_else_connect_frame() {
        // Outer-mTLS variant: a cert CN names the target node, else the frame.
        assert_eq!(
            resolve_target_node(Some("from-cert".into()), "from-frame".into()),
            "from-cert"
        );
        // No outer cert (or off) -> the Connect frame is the fallback.
        assert_eq!(resolve_target_node(None, "from-frame".into()), "from-frame");
        // An empty CN is ignored (falls back to the frame).
        assert_eq!(
            resolve_target_node(Some("".into()), "from-frame".into()),
            "from-frame"
        );
    }
}
