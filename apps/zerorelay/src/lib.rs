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

mod enroll_proxy;
mod enroll_route;
mod frontdoor;
mod frontdoor_assets;

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
    ConnWindow, Control, INITIAL_WINDOW, MAX_CONTROL_FRAME, MAX_DATA_PAYLOAD, PEER_HINT_ENROLL,
    TokenBucket, decode_data, encode_data,
};

/// How far a client may drive its send window negative before the relay treats it
/// as ignoring flow control and tears the conn down. One full window of slack
/// absorbs acks still in flight; beyond that the client is flooding (A6).
const RELAY_OVERRUN_TOLERANCE: u64 = INITIAL_WINDOW as u64;

/// How long a freshly connected client waits to be paired with the daemon before
/// the relay gives up and drops it.
const PAIR_TIMEOUT: Duration = Duration::from_secs(15);

/// How long the relay will hold a task and its TLS session open purely to
/// deliver a refusal on a connection it is closing anyway.
///
/// The refusal is a courtesy: the connection is going away whether or not the
/// peer ever reads it, so the only question is how long we are willing to wait
/// to say goodbye politely. Five seconds is generous for one small frame to a
/// live peer, and short enough that a peer which never reads cannot
/// meaningfully accumulate relay tasks by tripping refusals on purpose.
///
/// Deliberately NOT the setup deadline: that one is absolute and measured from
/// accept, so on a long-lived admitted connection hitting `daemon_gone` it
/// expired hours ago, and the refusal window would depend on connection age.
/// Deliberately NOT `idle_timeout` either: that is a liveness policy for an
/// established pump, not a bound on a single write.
const REFUSAL_WRITE_BUDGET: Duration = Duration::from_secs(5);

/// How long a write to a REGISTERED DAEMON's socket may make no progress before
/// the relay declares that link dead.
///
/// The daemon link is long-lived and has no configured liveness policy of its
/// own (`idle_timeout` governs client conns), so this is its liveness policy.
/// The value matches the `DEAD_AFTER`/`WRITE_STALL` the daemon end of this same
/// protocol uses to declare the link dead: with both ends on one timescale,
/// neither keeps holding a link the other has already abandoned.
///
/// Unbounded, this write is the relay's worst wedge. `to_daemon` is a bounded
/// channel, so a daemon that stops reading first parks the writer, then fills
/// the channel, and then every path that sends into it - including the SHARED
/// daemon reader - parks behind one unresponsive peer.
const DAEMON_WRITE_STALL: Duration = Duration::from_secs(60);

/// How long a client waits for its `Open` to reach the daemon before the relay
/// gives up and refuses it.
///
/// Distinct from [`DAEMON_WRITE_STALL`] because the waiter is different: the
/// client is still UN-ADMITTED here and holding a pre-admission permit, so a
/// wedged daemon must not be able to pin permits for a minute. A healthy daemon
/// drains a 256-slot channel instantly; needing longer than this means the link
/// is wedged, and the client is refused exactly as if it were gone.
const DAEMON_HANDOFF_BUDGET: Duration = Duration::from_secs(5);

/// Concurrent browser frontdoor HTTP sessions (see `Inner::frontdoor_permits`).
///
/// Small on purpose. Each session can hold an enrollment route open against a
/// daemon, and enrollment is an operator-driven, one-at-a-time act - not a
/// traffic plane. A relay under a browser flood should shed frontdoor sessions
/// long before it degrades the relay plane, which is why this pool is both
/// separate from and much smaller than `max_pending_handshakes`.
const MAX_FRONTDOOR_SESSIONS: usize = 16;

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

/// True when this configuration would run an unguarded open relay on a public
/// address: non-loopback bind + open registration + no shared-secret token.
/// (An allowlist guards regardless of token; a token guards open mode.)
fn public_open_unguarded(bind: &str, mode: &Admission, relay_token: Option<&str>) -> bool {
    if !matches!(mode, Admission::Open) || relay_token.is_some_and(|t| !t.trim().is_empty()) {
        return false;
    }
    let host = bind
        .rsplit_once(':')
        .map_or(bind, |(h, _)| h)
        .trim_matches(['[', ']']);
    match host.parse::<std::net::IpAddr>() {
        Ok(ip) => !ip.is_loopback(),
        // Unparseable host (a name): "localhost" is loopback; anything else is
        // conservatively treated as public.
        Err(_) => !host.eq_ignore_ascii_case("localhost"),
    }
}

/// The fail-closed public-open rule, carried so it applies to EVERY admission
/// policy the relay adopts - not only the one it started with.
///
/// AGENTS.md: new external surfaces default closed. An OPEN, tokenless relay on
/// a public bind admits any daemon on the internet and lets unclaimed node-ids
/// be squatted, so it takes an explicit operator opt-in. Startup enforced that,
/// but a SIGHUP reload swapped a freshly read policy in without it, so editing a
/// token-gated or allowlisted config into an open one and sending a signal
/// reached exactly the state startup refuses. The guard travels with the reload
/// because neither input hot-reloads: `bind` is where the listener already is,
/// and `allow_public_open` is the operator's deliberate opt-in.
#[derive(Debug, Clone)]
pub struct PublicOpenGuard {
    bind: String,
    allow_public_open: bool,
}

impl PublicOpenGuard {
    /// `bind` is the address the relay listens on; `allow_public_open` is the
    /// explicit opt-in (`--allow-public-open` / `[admission] allow_public_open`).
    pub fn new(bind: impl Into<String>, allow_public_open: bool) -> Self {
        Self {
            bind: bind.into(),
            allow_public_open,
        }
    }

    /// `Err` when adopting `policy` would leave an open, tokenless relay on a
    /// public bind without the opt-in. `action` names what is being refused;
    /// the rest of the message is identical wherever the guard fires, so an
    /// operator reads the same instruction at startup and at reload.
    fn check(&self, policy: &AdmissionPolicy, action: &str) -> Result<()> {
        if self.allow_public_open
            || !public_open_unguarded(
                &self.bind,
                &policy.registration_mode,
                policy.relay_token.as_deref(),
            )
        {
            return Ok(());
        }
        let bind = &self.bind;
        anyhow::bail!(
            "{action}: bind {bind} is public, admission mode is open, and no \
             relay_token is set — any daemon on the internet could register and squat \
             unclaimed node-ids. Set [admission] relay_token, use mode = \"allowlist\", \
             or pass --allow-public-open (config: [admission] allow_public_open = true) \
             if an open public relay is genuinely intended."
        )
    }

    /// Startup form of the guard: refuse to come up at all.
    pub fn check_startup(&self, policy: &AdmissionPolicy) -> Result<()> {
        self.check(policy, "refusing to start")
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
    /// Lease TTL advertised to daemons at registration. ADVISORY in v1: the
    /// relay runs no expiry timer and never releases a node-id on elapse. A
    /// registration lives exactly as long as its persistent WebSocket, and
    /// dropping that link is the only thing that frees the node-id.
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
    /// Global cap on sockets that are past accept but not yet ADMITTED: the TLS
    /// handshake, the HTTP/WebSocket upgrade, the first control frame, and
    /// whatever follows it until the peer is either serving a registration or
    /// routed to a daemon - including the reply to a peer that is being refused.
    /// The per-IP token bucket bounds one source; this bounds the SUM, so a
    /// flood spread across many source addresses cannot accumulate unbounded
    /// TLS/parser/task state, whether it stalls in setup or gets itself refused
    /// on purpose. When the pool is exhausted new sockets are shed at accept.
    /// Admitted connections never hold one, so an exhausted pool never stalls
    /// established traffic.
    pub max_pending_handshakes: usize,
    /// ONE absolute deadline for the whole pre-admission sequence: TLS accept,
    /// the HTTP/WebSocket upgrade, the first control frame, and - on the daemon
    /// path - the signed `Challenge`/`Register` exchange that follows `Hello`.
    /// It is a single budget measured from accept, not a fresh window per
    /// phase. `idle_timeout` only starts once a connection is admitted; without
    /// this, a socket could sit in setup forever.
    pub handshake_timeout: Duration,
    /// Ceiling on simultaneously REGISTERED daemons. Admission bounds setup,
    /// but an admitted daemon then occupies a registry entry, a writer task and
    /// a socket for as long as it stays connected. In the supported open and
    /// shared-token modes one permitted party can mint unlimited signing keys
    /// and node-ids, so the registry needs its own aggregate bound.
    pub max_registered_nodes: usize,
    /// Serve the browser enrollment frontdoor (page + enrollment routes) from
    /// this relay.
    ///
    /// OFF by default, and a narrowing of the blind-forwarder guarantee in two
    /// distinct ways. A relay that serves enrollment code is a TRUSTED CODE
    /// ORIGIN for those browsers, and - because a browser cannot speak the
    /// daemon's TLS enrollment protocol - this relay also becomes a PRINCIPAL in
    /// their enrollment: it performs the exchange itself and sees the pairing
    /// code and the issued certificate. It does not see the private key, which
    /// the browser generates and keeps.
    ///
    /// zerocode/native enrollment is relay-blind regardless of this knob, and
    /// the RPC plane is unaffected.
    pub frontdoor_enabled: bool,
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
            max_pending_handshakes: 256,
            handshake_timeout: Duration::from_secs(10),
            max_registered_nodes: 1024,
            frontdoor_enabled: false,
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
    ///
    /// On a GRACEFUL daemon close this carries the route's cancellation handle.
    /// Data the daemon sent just before closing is still queued AHEAD of this
    /// event on the same FIFO, so the cancellation must not fire until the pump
    /// has drained past it - otherwise teardown outruns the connection's own
    /// last bytes and the client loses the tail of its response (a relayed
    /// response whose peer closes right after writing loses its `close_notify`).
    /// Abrupt closes - link death, backpressure shedding - pass `None` and keep
    /// the immediate drop-cancel, because there is nothing worth draining.
    Close(String, Option<tokio::sync::oneshot::Sender<()>>),
    /// Daemon -> client `Window { credit }`: (re)establish the client's send
    /// window for this conn (forwarded to the client; also seeds the relay guard).
    Window(u32),
    /// Daemon -> client `DataAck { consumed }`: replenish the client's send window.
    Ack(u32),
}

/// One live client route on a daemon link: where to deliver the daemon's frames,
/// and the owner-cancellation handle for the task serving it.
///
/// Removing a route from the map drops this whole value, and dropping
/// `_cancel_on_drop` resolves the owning task's cancellation future. That is
/// deliberately structural: "the route is gone" and "the task serving it is
/// cancelled" become the same event, so no removal site can forget to cancel.
/// Without it, dropping the event sender only cancels a task that is actually
/// polling its receiver - a task parked in a peer-controlled `send` never
/// notices, keeps its socket and its `LiveConnGuard`, and (because the map entry
/// is already gone) does not even count against `max_conns_per_node` any more.
struct ConnRoute {
    /// Daemon -> client control/data events for this conn.
    events: mpsc::Sender<ConnEvent>,
    /// Cancels the owning client task when this route leaves the map. Never
    /// read: its `Drop` is the signal.
    _cancel_on_drop: tokio::sync::oneshot::Sender<()>,
}

/// The live client routes multiplexed over one daemon link, keyed by `conn_id`.
type ConnRoutes = HashMap<u64, ConnRoute>;

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
    conns: Arc<Mutex<ConnRoutes>>,
    /// Per-node usage counters (status surface).
    metrics: Arc<NodeMetrics>,
    /// Per-node client-connect rate limiter (A6).
    connect_bucket: Arc<Mutex<TokenBucket>>,
    /// Fires when this link is superseded by a newer registration of the same
    /// node-id, so the old link's reader/writer tasks and conn map are reclaimed
    /// instead of lingering under one registry slot. Without it, one key could
    /// re-register the same node-id repeatedly and accumulate live links while
    /// `max_registered_nodes` stayed at a single map entry.
    supersede: Arc<tokio::sync::Notify>,
}

/// How many distinct source IPs the accept-rate map tracks before it prunes idle
/// (full-bucket) entries, so the map itself cannot grow unboundedly under a
/// spoofed-source flood.
const MAX_TRACKED_IPS: usize = 4096;

/// Longest accepted node-id, in bytes. The control-frame ceiling
/// ([`MAX_CONTROL_FRAME`], 64 KiB) is the only bound a `Register` frame would
/// otherwise hit, which is far too loose for a registry key that is retained
/// per live daemon and echoed into status output and logs.
pub const MAX_NODE_ID_LEN: usize = 128;

/// A node-id is a routing label, not free-form text: bounded, non-empty, and
/// printable ASCII so it cannot smuggle control characters into operator
/// surfaces or bloat the registry.
fn valid_node_id(node_id: &str) -> bool {
    !node_id.is_empty()
        && node_id.len() <= MAX_NODE_ID_LEN
        && node_id.chars().all(|c| c.is_ascii_graphic())
}

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
    /// Pre-admission handshake permits (see `RelayConfig::max_pending_handshakes`).
    handshake_permits: Arc<tokio::sync::Semaphore>,
    handshake_timeout: Duration,
    /// Serve the browser frontdoor on plain HTTP hits (opt-in; see
    /// [`RelayConfig::frontdoor_enabled`]).
    frontdoor_enabled: bool,
    /// Concurrent frontdoor HTTP sessions.
    ///
    /// Deliberately a SEPARATE pool from `handshake_permits`. A frontdoor
    /// session performs a whole enrollment exchange, which is far longer than a
    /// handshake, so charging it to the pre-admission pool would let browser
    /// traffic shed the daemons and clients that pool exists to protect. This
    /// bounds the frontdoor on its own instead.
    frontdoor_permits: Arc<tokio::sync::Semaphore>,
    max_registered_nodes: usize,
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
    ///
    /// The map is HARD-bounded at [`MAX_TRACKED_IPS`]. Pruning idle
    /// (refilled-to-full) entries is only the cheap first pass: a flood that
    /// takes a single token from each of many distinct source addresses leaves
    /// every bucket partially drained, so that pass frees nothing and the map
    /// would grow without limit on what is designed to be a public endpoint.
    /// When it frees nothing we evict the least-recently-active entries
    /// instead. Evicting an idle-ish bucket only restores that source's burst
    /// allowance, which a source-rotating flood already gets for free; the
    /// global bound that actually caps concurrent setup work is
    /// `max_pending_handshakes`.
    fn admit_ip(&self, ip: IpAddr) -> bool {
        let now = std::time::Instant::now();
        let mut map = self.ip_buckets.lock().expect("ip bucket lock");
        if map.len() >= MAX_TRACKED_IPS && !map.contains_key(&ip) {
            map.retain(|_, b| !b.is_full_at(now));
            if map.len() >= MAX_TRACKED_IPS {
                evict_least_recently_active(&mut map, MAX_TRACKED_IPS - MAX_TRACKED_IPS / 4);
            }
        }
        map.entry(ip)
            .or_insert_with(|| {
                TokenBucket::new_at(self.accept_burst_per_ip, self.accept_rate_per_ip, now)
            })
            .try_take_at(now)
    }
}

/// Shrink `map` to at most `target` entries, dropping the least recently active
/// buckets first. Evicting a batch rather than one entry per insert keeps the
/// O(n) scan amortized to O(1) per admitted connection while the map is at its
/// hard bound.
fn evict_least_recently_active(map: &mut HashMap<IpAddr, TokenBucket>, target: usize) {
    if map.len() <= target {
        return;
    }
    let evict = map.len() - target;
    let mut by_age: Vec<(std::time::Instant, IpAddr)> =
        map.iter().map(|(ip, b)| (b.last_activity(), *ip)).collect();
    // Partition so the `evict` oldest entries occupy the front of the vec.
    by_age.select_nth_unstable(evict - 1);
    for (_, ip) in by_age.into_iter().take(evict) {
        map.remove(&ip);
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
                handshake_permits: Arc::new(tokio::sync::Semaphore::new(
                    cfg.max_pending_handshakes.max(1),
                )),
                handshake_timeout: cfg.handshake_timeout,
                frontdoor_enabled: cfg.frontdoor_enabled,
                frontdoor_permits: Arc::new(tokio::sync::Semaphore::new(MAX_FRONTDOOR_SESSIONS)),
                max_registered_nodes: cfg.max_registered_nodes.max(1),
                daemons: Mutex::new(HashMap::new()),
                next_conn: AtomicU64::new(1),
                next_epoch: AtomicU64::new(1),
            }),
        }
    }

    /// Swap the admission policy live (SIGHUP reload). Existing connections are
    /// untouched; the new policy applies to subsequent registrations.
    ///
    /// The reload is subject to the same fail-closed rule as startup: `guard`
    /// carries the bind and the operator's explicit opt-in, and a policy that
    /// would expose an open, tokenless public relay is REFUSED. On `Err` nothing
    /// is swapped and the relay keeps running the policy it already had, so a
    /// bad edit plus a signal cannot widen admission behind the guard's back.
    pub fn reload_admission(&self, policy: AdmissionPolicy, guard: &PublicOpenGuard) -> Result<()> {
        guard.check(&policy, "refusing to reload admission")?;
        *self.inner.admission.write().expect("admission lock") = Arc::new(policy);
        Ok(())
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
            // Global pre-classification bound: a socket holds a permit from
            // accept until its first control frame is classified. The per-IP
            // bucket above bounds ONE source; this bounds the sum, so stalled
            // handshakes spread across many addresses shed new sockets instead
            // of accumulating unbounded TLS/parser/task state.
            let Ok(permit) = inner.handshake_permits.clone().try_acquire_owned() else {
                drop(sock);
                continue;
            };
            tokio::spawn(async move {
                // ONE absolute deadline for the whole pre-admission sequence:
                // TLS accept, the HTTP/WebSocket upgrade, the first control
                // frame, and - on the daemon path -
                // the signed registration exchange in `handle_daemon`. A fresh
                // relative timeout per phase would let a peer spend the full
                // `handshake_timeout` in EACH phase, so the effective budget was
                // a multiple of the configured value.
                // `idle_timeout` only begins on classified connections.
                let deadline = tokio::time::Instant::now() + inner.handshake_timeout;
                let hs_inner = inner.clone();
                let handshake = async move {
                    let tls = hs_inner.route_by_client_cert;
                    let accepted = acceptor.accept(sock).await.ok()?;
                    // Outer-mTLS variant: read a target node-id from the peer's
                    // outer client cert CN before the TlsStream is consumed by
                    // the WS handshake. None otherwise.
                    let cert_node_id = if tls {
                        accepted
                            .get_ref()
                            .1
                            .peer_certificates()
                            .and_then(|c| c.first())
                            .and_then(|c| zeroclaw_tls::client_cert_node_id(c.as_ref()))
                    } else {
                        None
                    };
                    match frontdoor::accept(accepted, hs_inner.frontdoor_enabled).await {
                        Ok(frontdoor::Accepted::WebSocket(w)) => {
                            Some(Classified::WebSocket(w, cert_node_id))
                        }
                        Ok(frontdoor::Accepted::Http(session)) => {
                            Some(Classified::Frontdoor(session))
                        }
                        Ok(frontdoor::Accepted::Rejected) | Err(_) => None,
                    }
                };
                let Ok(Some(classified)) = tokio::time::timeout_at(deadline, handshake).await
                else {
                    return; // shed: timed out or never became a relay connection
                };
                let (ws, cert_node_id) = match classified {
                    Classified::WebSocket(ws, cert_node_id) => (ws, cert_node_id),
                    // A frontdoor session is served OUTSIDE the handshake
                    // deadline: that budget bounds how long a socket may take to
                    // become a relay connection, while an enrollment exchange is
                    // a whole multi-second conversation with a daemon. It is
                    // bounded instead by its own session budget and its own
                    // permit pool, and it releases the pre-admission permit so
                    // browser traffic cannot shed daemons and clients.
                    Classified::Frontdoor(session) => {
                        drop(permit);
                        let Ok(frontdoor_permit) =
                            inner.frontdoor_permits.clone().try_acquire_owned()
                        else {
                            return; // at frontdoor capacity: shed this session
                        };
                        let _frontdoor_permit = frontdoor_permit;
                        frontdoor::serve_http(session, inner).await;
                        return;
                    }
                };
                let mut ws = *ws;
                let first = match tokio::time::timeout_at(deadline, next_control(&mut ws)).await {
                    Ok(f) => f,
                    Err(_) => return, // stalled before sending a first frame
                };
                // The permit travels INTO `handle_conn`, which releases it at the
                // point its role is fully admitted: immediately for a client, and
                // only after the signed registration completes for a daemon.
                let _ = handle_conn(inner, ws, cert_node_id, first, permit, deadline).await;
            });
        }
    }
}

/// What an accepted socket turned out to be, once its request head was read.
///
/// The two arms have different lifetimes on purpose: a WebSocket continues under
/// the pre-admission handshake deadline, while a frontdoor session is served
/// after it (see the accept loop).
enum Classified<S> {
    /// A relay WebSocket, with the outer-mTLS-derived target node-id if any.
    WebSocket(
        Box<WebSocketStream<frontdoor::PrefixedIo<S>>>,
        Option<String>,
    ),
    /// A plain HTTP request for the opt-in browser frontdoor.
    Frontdoor(frontdoor::HttpSession<S>),
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
///
/// `permit` is the pre-admission handshake permit, and NO branch releases it
/// here. A first frame classifies a peer's role; it does not admit it. A
/// `Connect`/`Enroll` may still be refused (unknown node, rate cap, node at
/// capacity), a `Hello` still has a signed exchange to complete, and a junk
/// frame still gets a reply - and every one of those refusals is a
/// peer-controlled write. Releasing the permit at classification bounded each
/// refusal's DURATION (the write deadlines) but not the NUMBER of refusal tasks:
/// a client could complete the upgrade, name a nonexistent node, stop reading,
/// and hold a task and its TLS session for the whole refusal budget while the
/// permit was already back in the pool for the next flooder. So each callee
/// carries the permit to the point it is genuinely admitted.
/// `deadline` is the absolute end of the setup budget.
async fn handle_conn<S>(
    inner: Arc<Inner>,
    mut ws: WebSocketStream<S>,
    cert_node_id: Option<String>,
    first: Option<Control>,
    permit: tokio::sync::OwnedSemaphorePermit,
    deadline: tokio::time::Instant,
) -> Result<()>
where
    S: AsyncRead + AsyncWrite + Unpin + Send + 'static,
{
    match first {
        Some(Control::Hello {
            daemon_pubkey,
            node_id,
            relay_token,
        }) => {
            handle_daemon(
                inner,
                ws,
                daemon_pubkey,
                node_id,
                relay_token,
                permit,
                deadline,
            )
            .await
        }
        Some(Control::Connect { node_id }) => {
            handle_client(
                inner,
                ws,
                resolve_target_node(cert_node_id, node_id),
                ClientRoute::Wss,
                permit,
            )
            .await
        }
        Some(Control::Enroll { node_id }) => {
            handle_client(
                inner,
                ws,
                resolve_target_node(cert_node_id, node_id),
                ClientRoute::Enrollment,
                permit,
            )
            .await
        }
        Some(other) => {
            // The permit is held across this refusal write and released with the
            // frame: a peer that sends a junk first frame and stops reading
            // occupies a slot until the write lands or its deadline expires,
            // rather than freeing one instantly for the next such peer.
            let _ = send_setup_control(
                &mut ws,
                &Control::error("bad_first_frame", format!("unexpected {other:?}")),
                deadline,
            )
            .await;
            drop(permit);
            Ok(())
        }
        None => Ok(()),
    }
}

/// Send one SETUP frame under the shared absolute setup deadline. The bounded
/// best-effort write primitive; [`send_refusal`] is the same mechanism on the
/// post-classification budget.
///
/// EVERY write the relay makes before a connection is fully admitted goes
/// through here: the `Challenge` and `Registered` confirmations and, just as
/// importantly, every refusal (`forbidden`, `bad_sig`, `node_taken`,
/// `registry_full`, ...). All of them target a peer-controlled sink. Awaited
/// bare, a peer that triggers a refusal and then stops reading parks the relay
/// inside `send` once the socket buffer fills - holding a task, a TLS session
/// and the pre-classification permit past the whole documented
/// `handshake_timeout` budget. The reply is a courtesy; the budget is not.
///
/// Returns false when the frame could not be delivered inside that budget (link
/// error or deadline). Refusal paths ignore the result - they were ending the
/// connection either way - and the caller tears down through its normal cleanup
/// path, releasing the permit with it.
async fn send_setup_control<S>(
    ws: &mut WebSocketStream<S>,
    frame: &Control,
    deadline: tokio::time::Instant,
) -> bool
where
    S: AsyncRead + AsyncWrite + Unpin,
{
    matches!(
        tokio::time::timeout_at(deadline, send_control(ws, frame)).await,
        Ok(Ok(()))
    )
}

/// Best-effort refusal on a connection that is already being torn down, bounded
/// by [`REFUSAL_WRITE_BUDGET`].
///
/// The post-classification counterpart to the setup-phase writes: the same
/// peer-controlled-sink problem (a client that trips a refusal and then stops
/// reading parks a task and its TLS session once the socket buffer fills), on
/// its own budget because the setup deadline no longer means anything here.
/// The result is discarded on purpose - the caller is returning either way.
async fn send_refusal<S>(ws: &mut WebSocketStream<S>, frame: &Control)
where
    S: AsyncRead + AsyncWrite + Unpin,
{
    let deadline = tokio::time::Instant::now() + REFUSAL_WRITE_BUDGET;
    let _ = send_setup_control(ws, frame, deadline).await;
}

/// One write on an ESTABLISHED connection: bounded by that connection's
/// liveness policy AND cancelled if its route is removed while the write pends.
///
/// Both halves are load-bearing. The pump's outer `select!` has an idle timer,
/// but a `select!` only polls its branches between iterations: while a nested
/// write is pending the idle timer is DEAD and the pump is blind. And dropping
/// the event sender cannot interrupt a task already parked in `send`, so route
/// removal alone would leave the task holding its socket and its
/// `LiveConnGuard` while no longer counting against `max_conns_per_node` -
/// letting the same wedge be recreated without bound. A peer that makes zero
/// write progress for `idle_timeout` is already dead by the connection's own
/// policy, so timing out here is not a new judgement about slow readers.
///
/// Returns false when the conn should be torn down (cancelled, stalled, or the
/// link failed).
async fn established_write<F, E>(
    write: F,
    idle: Duration,
    cancelled: &mut tokio::sync::oneshot::Receiver<()>,
) -> bool
where
    F: std::future::Future<Output = std::result::Result<(), E>>,
{
    tokio::select! {
        _ = &mut *cancelled => false,
        result = tokio::time::timeout(idle, write) => matches!(result, Ok(Ok(()))),
    }
}

/// [`send_refusal`] for a connection whose socket has already been split: once
/// the pump owns the halves, the goodbye frame goes out through the write half.
///
/// Only for frames the caller follows with a `break`/`return`. The pump's
/// ordinary data and flow-control writes are NOT refusals: blocking on a slow
/// reader there is legitimate backpressure, and the credit window is what
/// bounds it.
async fn send_refusal_to_sink<K>(sink: &mut K, frame: &Control)
where
    K: futures_util::Sink<Message> + Unpin,
{
    let _ = tokio::time::timeout(
        REFUSAL_WRITE_BUDGET,
        sink.send(Message::text(frame.to_json())),
    )
    .await;
}

/// Drop a registration and tear down its client conns - but ONLY if the registry
/// entry is still the one `epoch` inserted.
///
/// A same-key re-registration replaces the map entry under a new epoch, so an
/// older link (or a rolled-back one) must never evict its replacement. Shared by
/// normal teardown and by [`RegistrationGuard`]'s rollback so both obey the same
/// identity rule.
async fn deregister(
    inner: &Inner,
    node_id: &str,
    epoch: u64,
    conns: &Mutex<ConnRoutes>,
    reason: &str,
) {
    {
        let mut daemons = inner.daemons.lock().await;
        if daemons.get(node_id).map(|h| h.epoch) == Some(epoch) {
            daemons.remove(node_id);
        }
    }
    let drained: Vec<_> = conns.lock().await.drain().collect();
    for (_, route) in drained {
        // Non-blocking: a stalled conn must not delay tearing down the rest.
        // Dropping the route closes its event channel and fires its
        // cancellation, so each task ends whether or not this notice lands.
        let _ = route
            .events
            .try_send(ConnEvent::Close(reason.to_string(), None));
    }
}

/// The registry slot a registration owns between the map insert and the moment
/// its `Registered` confirmation is actually on the wire.
struct RegistrationSlot {
    inner: Arc<Inner>,
    node_id: String,
    epoch: u64,
    conns: Arc<Mutex<ConnRoutes>>,
}

/// Makes registration failure-atomic across the window where the relay is
/// routable but the daemon does not yet know it is registered.
///
/// The entry has to go in before the confirmation is written (a client that
/// arrives in between must find a live route rather than `no_such_node`), so
/// every way out of that window has to put the slot back. Otherwise a
/// confirmation that cannot be delivered leaves a [`DaemonHandle`] whose channel
/// points at a dead link: clients route into it until the relay restarts, and
/// enough failed registrations consume `max_registered_nodes`.
///
/// Armed at insert, disarmed only by [`RegistrationGuard::confirmed`]. Rollback
/// is epoch-identified (see [`deregister`]), so it removes only the entry THIS
/// registration inserted and can never evict a concurrent same-key
/// re-registration that already replaced it.
struct RegistrationGuard {
    slot: Option<RegistrationSlot>,
}

impl RegistrationGuard {
    /// Arm the guard for the entry just inserted under `epoch`.
    fn armed(
        inner: Arc<Inner>,
        node_id: String,
        epoch: u64,
        conns: Arc<Mutex<ConnRoutes>>,
    ) -> Self {
        Self {
            slot: Some(RegistrationSlot {
                inner,
                node_id,
                epoch,
                conns,
            }),
        }
    }

    /// The daemon has the confirmation: the registration now owns its slot for
    /// real, and the reader loop's teardown is what releases it.
    fn confirmed(mut self) {
        self.slot = None;
    }

    /// Undo the registration: remove the entry, drain any conns a client opened
    /// in the meantime, and leave the node-id immediately reclaimable.
    async fn rollback(mut self) {
        let Some(slot) = self.slot.take() else {
            return;
        };
        deregister(
            &slot.inner,
            &slot.node_id,
            slot.epoch,
            &slot.conns,
            "registration_failed",
        )
        .await;
    }
}

impl Drop for RegistrationGuard {
    fn drop(&mut self) {
        let Some(slot) = self.slot.take() else {
            return;
        };
        // Only reached if the window exited without confirming or rolling back -
        // a panic, or a cancelled task. Rollback needs an async lock, so finish
        // it on the runtime rather than leaving a routable entry behind. No
        // runtime means the process is going away with the registry.
        if let Ok(handle) = tokio::runtime::Handle::try_current() {
            handle.spawn(async move {
                deregister(
                    &slot.inner,
                    &slot.node_id,
                    slot.epoch,
                    &slot.conns,
                    "registration_failed",
                )
                .await;
            });
        }
    }
}

/// Daemon control connection: signed admission, then multiplex client conns.
///
/// `permit` (the pre-classification handshake permit) is held for the WHOLE
/// signed registration exchange and released only once the node is registered.
/// A `Hello` proves nothing - any peer that can complete a TLS + WebSocket
/// upgrade can send one - so releasing the permit at classification would let a
/// peer that never sends `Register` retain a task, a TLS session and a parser
/// outside every handshake bound. `deadline` closes the same hole in the time
/// dimension: the wait for `Register` shares the one absolute setup budget.
async fn handle_daemon<S>(
    inner: Arc<Inner>,
    mut ws: WebSocketStream<S>,
    daemon_pubkey: String,
    node_id: String,
    relay_token: Option<String>,
    permit: tokio::sync::OwnedSemaphorePermit,
    deadline: tokio::time::Instant,
) -> Result<()>
where
    S: AsyncRead + AsyncWrite + Unpin + Send + 'static,
{
    // Reject a malformed node-id before spending a nonce and a signature
    // verification on it: it is the registry key, so it must be bounded and
    // printable regardless of who is asking.
    if !valid_node_id(&node_id) {
        let _ = send_setup_control(
            &mut ws,
            &Control::error(
                "bad_node_id",
                format!(
                    "node-id must be 1..={MAX_NODE_ID_LEN} bytes of printable ASCII without spaces"
                ),
            ),
            deadline,
        )
        .await;
        return Ok(());
    }

    // Snapshot the admission policy once so the token gate and the allow/deny
    // check are consistent even if a SIGHUP reload lands mid-registration.
    let policy = inner.admission();

    // Optional shared-secret gate.
    if let Some(required) = &policy.relay_token
        && relay_token.as_deref() != Some(required.as_str())
    {
        let _ = send_setup_control(
            &mut ws,
            &Control::error("forbidden", "bad relay token"),
            deadline,
        )
        .await;
        return Ok(());
    }

    let pubkey = match B64.decode(daemon_pubkey.as_bytes()) {
        Ok(k) => k,
        Err(_) => {
            let _ = send_setup_control(
                &mut ws,
                &Control::error("bad_pubkey", "not base64"),
                deadline,
            )
            .await;
            return Ok(());
        }
    };
    let fpr = hex::encode(Sha256::digest(&pubkey));
    if !policy.admit(&fpr) {
        let _ = send_setup_control(
            &mut ws,
            &Control::error("forbidden", "registration denied"),
            deadline,
        )
        .await;
        return Ok(());
    }

    // Challenge / verify: prove possession of the private key over a fresh nonce.
    let mut nonce = [0u8; 32];
    if SystemRandom::new().fill(&mut nonce).is_err() {
        let _ = send_setup_control(&mut ws, &Control::error("internal", "rng"), deadline).await;
        return Ok(());
    }
    // The write itself shares the setup deadline: nothing has been allocated for
    // this peer yet, so a challenge that cannot be delivered inside the budget
    // just ends the connection (and releases the permit with it).
    if !send_setup_control(
        &mut ws,
        &Control::Challenge {
            nonce: B64.encode(nonce),
        },
        deadline,
    )
    .await
    {
        return Ok(());
    }
    // Bounded by the shared setup deadline: a peer that sends Hello and then
    // withholds Register must not be able to hold this state open forever.
    let (reg_node, sig_b64) = match tokio::time::timeout_at(deadline, next_control(&mut ws)).await {
        Ok(Some(Control::Register { node_id, sig })) => (node_id, sig),
        Err(_) => return Ok(()), // stalled after Hello; reap without a reply
        Ok(_) => {
            let _ = send_setup_control(
                &mut ws,
                &Control::error("bad_register", "expected register"),
                deadline,
            )
            .await;
            return Ok(());
        }
    };
    if reg_node != node_id {
        let _ = send_setup_control(
            &mut ws,
            &Control::error("bad_register", "node_id mismatch"),
            deadline,
        )
        .await;
        return Ok(());
    }
    let sig = match B64.decode(sig_b64.as_bytes()) {
        Ok(s) => s,
        Err(_) => {
            let _ = send_setup_control(&mut ws, &Control::error("bad_sig", "not base64"), deadline)
                .await;
            return Ok(());
        }
    };
    if UnparsedPublicKey::new(&ED25519, &pubkey)
        .verify(&nonce, &sig)
        .is_err()
    {
        let _ = send_setup_control(
            &mut ws,
            &Control::error("bad_sig", "signature invalid"),
            deadline,
        )
        .await;
        return Ok(());
    }

    // node-id <-> pubkey binding + last-writer-wins registration.
    let epoch = inner.next_epoch.fetch_add(1, Ordering::Relaxed);
    let (to_daemon, mut from_clients) = mpsc::channel::<Message>(256);
    let conns: Arc<Mutex<ConnRoutes>> = Arc::new(Mutex::new(HashMap::new()));
    // Owner cancellation for THIS link. A same-key re-registration will fire the
    // superseded link's own `supersede` so it tears down; this link parks on its
    // fresh one in the reader loop below.
    let supersede = Arc::new(tokio::sync::Notify::new());
    // Armed inside the same critical section as the insert below: from that
    // point the slot belongs to this registration until the daemon has its
    // confirmation, and every way out of the window puts it back.
    let registration = {
        let mut daemons = inner.daemons.lock().await;
        if let Some(existing) = daemons.get(&node_id)
            && existing.fpr != fpr
        {
            drop(daemons);
            let _ = send_setup_control(
                &mut ws,
                &Control::error("node_taken", "node-id bound to another key"),
                deadline,
            )
            .await;
            return Ok(());
        }
        // Aggregate registry bound. A daemon RE-registering its own node-id
        // replaces the existing entry and so does not grow the registry; only
        // a genuinely new node-id has to fit under the ceiling.
        if !daemons.contains_key(&node_id) && daemons.len() >= inner.max_registered_nodes {
            drop(daemons);
            let _ = send_setup_control(
                &mut ws,
                &Control::error(
                    "registry_full",
                    format!(
                        "relay is at its {} registered-node limit",
                        inner.max_registered_nodes
                    ),
                ),
                deadline,
            )
            .await;
            return Ok(());
        }
        // Same-key re-registration replaces the map entry; reclaim the superseded
        // link first so its reader/writer tasks and conn map are torn down instead
        // of lingering. Otherwise one key could re-register repeatedly and grow
        // live links/tasks while `max_registered_nodes` stayed at one entry. The
        // teardown epoch guard keeps the old link from evicting this replacement.
        if let Some(existing) = daemons.get(&node_id) {
            existing.supersede.notify_one();
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
                supersede: supersede.clone(),
            },
        );
        RegistrationGuard::armed(inner.clone(), node_id.clone(), epoch, conns.clone())
    };
    // Confirmation, under the same absolute setup deadline as every other step.
    // Until it lands the daemon does not know it is registered, so a send that
    // fails or misses the budget must undo the registration rather than leave a
    // routable entry pointing at a link nobody is serving.
    if !send_setup_control(
        &mut ws,
        &Control::Registered {
            node_id: node_id.clone(),
            lease_ttl_secs: inner.lease_ttl.as_secs(),
        },
        deadline,
    )
    .await
    {
        registration.rollback().await;
        // The node-id is free again and so is the pre-classification permit;
        // dropping `from_clients` with this frame makes any client that raced in
        // see a dead daemon channel and answer `no_such_node`.
        drop(permit);
        return Ok(());
    }
    registration.confirmed();
    // Registered: the signed exchange is complete and this connection is now a
    // long-lived admitted daemon link, not pending setup. Release the permit so
    // it is available to the next connection being classified.
    drop(permit);

    let (mut sink, mut stream) = ws.split();

    // Writer task: the single serialization point for everything sent to the
    // daemon (client Opens/Data/Closes + our Pongs).
    //
    // Every write is bounded by DAEMON_WRITE_STALL. Unbounded, this task is the
    // relay's worst wedge: `to_daemon` is a bounded channel, so a daemon that
    // stops reading parks the writer, fills the channel, and then parks every
    // other path that sends into it - the shared daemon reader included. When a
    // write makes no progress for the budget, the link is declared dead and
    // `link_down` tears it down through the reader's normal deregister path, so
    // the node-id is released and superseding/reclaim keep working.
    let link_down = Arc::new(tokio::sync::Notify::new());
    let writer_down = link_down.clone();
    let writer = tokio::spawn(async move {
        while let Some(msg) = from_clients.recv().await {
            if !matches!(
                tokio::time::timeout(DAEMON_WRITE_STALL, sink.send(msg)).await,
                Ok(Ok(()))
            ) {
                break;
            }
        }
        let _ = tokio::time::timeout(DAEMON_WRITE_STALL, sink.close()).await;
        writer_down.notify_one();
    });

    // Reader loop: demultiplex daemon -> client. Every delivery clones the
    // conn's sender under the map lock, drops the guard, and hands off
    // non-blocking (see `deliver_conn_event`), so one stalled client can never
    // freeze delivery or teardown for the other conns on this node. The loop also
    // breaks when this link is superseded by a newer same-key registration, or
    // when the writer declared the link dead, so the old link is reclaimed
    // rather than accumulating. Nothing in this loop may await a send into
    // `to_daemon`: parking the shared reader on the daemon's own backlog is
    // what would make one unresponsive peer freeze delivery, teardown, and the
    // supersede signal for every other conn on the node.
    loop {
        let msg = tokio::select! {
            _ = supersede.notified() => break,
            _ = link_down.notified() => break,
            m = stream.next() => match m {
                Some(m) => m,
                None => break,
            },
        };
        match msg {
            Ok(Message::Text(t)) => match Control::from_json(&t) {
                Ok(Control::Opened { conn_id }) => {
                    deliver_conn_event(&conns, &to_daemon, conn_id, ConnEvent::Opened).await;
                }
                Ok(Control::Close { conn_id, reason }) => {
                    let removed = conns.lock().await.remove(&conn_id);
                    if let Some(route) = removed {
                        // GRACEFUL close: the daemon's final DATA frames may
                        // still be queued ahead of this on the conn's FIFO, so
                        // the route's cancellation travels INSIDE the close
                        // event. It therefore cannot fire until the pump has
                        // drained everything queued before it - the client gets
                        // the tail of its response, then the close. Dropping the
                        // route here instead cancelled the pump mid-queue and
                        // lost those bytes.
                        //
                        // If the queue has no room the event (and the
                        // cancellation with it) is dropped right here, which is
                        // exactly the abrupt behaviour: a client that far behind
                        // is not going to drain a graceful close either.
                        let ConnRoute {
                            events,
                            _cancel_on_drop: cancel,
                        } = route;
                        let _ = events.try_send(ConnEvent::Close(reason, Some(cancel)));
                        // `events` drops here, so once the queue is drained the
                        // pump sees the channel close and exits regardless.
                    }
                }
                // Daemon -> client credit-window frames: route to the conn so the
                // client task forwards them on (and the relay guard tracks them).
                Ok(Control::Window { conn_id, credit }) => {
                    deliver_conn_event(&conns, &to_daemon, conn_id, ConnEvent::Window(credit))
                        .await;
                }
                Ok(Control::DataAck { conn_id, consumed }) => {
                    deliver_conn_event(&conns, &to_daemon, conn_id, ConnEvent::Ack(consumed)).await;
                }
                _ => {}
            },
            Ok(Message::Binary(b)) => {
                if let Some((conn_id, payload)) = decode_data(&b) {
                    deliver_conn_event(
                        &conns,
                        &to_daemon,
                        conn_id,
                        ConnEvent::Data(payload.to_vec()),
                    )
                    .await;
                }
            }
            Ok(Message::Ping(p)) => {
                // Non-blocking: this runs on the SHARED reader, so it must never
                // wait on the daemon's own backlog. A dropped pong on a daemon
                // that is not draining its socket is immaterial - that link is
                // already on its way to DAEMON_WRITE_STALL.
                let _ = to_daemon.try_send(Message::Pong(p));
            }
            Ok(Message::Pong(_)) => {}
            Ok(Message::Close(_)) | Err(_) => break,
            _ => {}
        }
    }

    // Teardown: deregister only if still current (epoch guard), close all conns.
    deregister(&inner, &node_id, epoch, &conns, "daemon_gone").await;
    writer.abort();
    Ok(())
}

/// Deliver one demultiplexed daemon frame to a logical conn WITHOUT blocking
/// the shared daemon reader. The sender is cloned
/// under the map lock and the guard dropped before delivery; delivery itself is
/// non-blocking. The per-conn buffer absorbs bursts, and credit-window flow
/// control keeps a well-behaved client inside it, so a full buffer means a
/// stalled or misbehaving client: that ONE conn is torn down (map entry
/// dropped, daemon notified) while every other conn on the node keeps flowing.
///
/// NOTHING here may await a send into `to_daemon`. This runs on the shared
/// reader, and `to_daemon` is bounded: awaiting it would mean one unresponsive
/// daemon could freeze demultiplexing, teardown and the supersede signal for
/// every conn on the node while the relay tried to tell it about a single
/// backpressured client.
async fn deliver_conn_event(
    conns: &Mutex<ConnRoutes>,
    to_daemon: &mpsc::Sender<Message>,
    conn_id: u64,
    ev: ConnEvent,
) {
    let tx = {
        let map = conns.lock().await;
        match map.get(&conn_id) {
            Some(route) => route.events.clone(),
            None => return,
        }
    };
    match tx.try_send(ev) {
        Ok(()) => {}
        Err(mpsc::error::TrySendError::Closed(_)) => {
            // The client task is gone; drop the stale map entry.
            conns.lock().await.remove(&conn_id);
        }
        Err(mpsc::error::TrySendError::Full(_)) => {
            // Backpressured past its credit-sized buffer: close only this conn.
            // Removal comes FIRST and is what actually reclaims the route - it
            // drops the event sender and fires the conn's cancellation, so the
            // client task is torn down even if it is parked in a peer write.
            conns.lock().await.remove(&conn_id);
            // Telling the daemon is then pure best-effort and NON-BLOCKING: on a
            // daemon whose channel is full, awaiting this would park the shared
            // reader on the very peer that caused the backpressure. That link is
            // already headed for DAEMON_WRITE_STALL.
            let _ = to_daemon.try_send(Message::text(
                Control::Close {
                    conn_id,
                    reason: "client_backpressured".into(),
                }
                .to_json(),
            ));
        }
    }
}

/// Client connection: route it to the daemon serving `node_id` and pipe `DATA`.
///
/// `permit` is the pre-admission handshake permit, held until this client is
/// ADMITTED - the point where the relay has allocated conn state and the daemon
/// has taken the `Open`. Every refusal before that (unknown node, per-node rate
/// cap, node at capacity, daemon link gone) needs no daemon and no prior state,
/// so it is floodable from nothing; holding the permit across those replies caps
/// how MANY such tasks can exist at once, which their per-write deadlines do
/// not. Refusal paths simply return, dropping the permit with the frame.
///
/// The permit is released at `Open`, NOT at pairing. A conn that has been handed
/// to the daemon already occupies one of `max_conns_per_node` slots and needs a
/// live registered daemon to exist at all, so the later pair-timeout refusal is
/// bounded by that cap rather than by this pool. Holding through the pairing
/// wait would put honest clients on a slow daemon (up to `PAIR_TIMEOUT`, longer
/// than the refusal budget) in contention for the handshake pool.
///
/// The pump never needs a permit, so an exhausted pool cannot stall established
/// traffic, and permits are taken only at accept - never re-acquired - so there
/// is no path on which holding one waits for another.
async fn handle_client<S>(
    inner: Arc<Inner>,
    mut ws: WebSocketStream<S>,
    node_id: String,
    route: ClientRoute,
    permit: tokio::sync::OwnedSemaphorePermit,
) -> Result<()>
where
    S: AsyncRead + AsyncWrite + Unpin + Send + 'static,
{
    let handle = {
        let daemons = inner.daemons.lock().await;
        daemons.get(&node_id).map(|h| {
            (
                h.to_daemon.clone(),
                h.conns.clone(),
                h.metrics.clone(),
                h.connect_bucket.clone(),
            )
        })
    };
    let Some((to_daemon, conns, metrics, connect_bucket)) = handle else {
        // Reply AFTER the registry guard is released: this send awaits a
        // peer-controlled sink, and a client that stops reading must stall
        // only itself, never the relay-wide daemon registry. Bounded, so it
        // cannot stall even itself indefinitely.
        send_refusal(&mut ws, &Control::error("no_such_node", node_id)).await;
        return Ok(());
    };

    // Per-node client-connect rate cap (A6): a flood of Connects to one node-id
    // is rejected before any conn state is allocated.
    if !connect_bucket.lock().await.try_take() {
        metrics.connects_rejected.fetch_add(1, Ordering::Relaxed);
        send_refusal(
            &mut ws,
            &Control::error("rate_limited", "too many connects to this node"),
        )
        .await;
        return Ok(());
    }

    let conn_id = inner.next_conn.fetch_add(1, Ordering::Relaxed);
    let (conn_tx, mut conn_rx) = mpsc::channel::<ConnEvent>(256);
    // Owner cancellation for THIS conn: the sender lives in the route map, so
    // whoever removes the route cancels this task - even mid-write.
    let (cancel_tx, mut cancelled) = tokio::sync::oneshot::channel::<()>();
    {
        let mut cs = conns.lock().await;
        if cs.len() >= inner.max_conns_per_node {
            drop(cs);
            send_refusal(&mut ws, &Control::error("busy", "node at capacity")).await;
            return Ok(());
        }
        cs.insert(
            conn_id,
            ConnRoute {
                events: conn_tx,
                _cancel_on_drop: cancel_tx,
            },
        );
    }
    // Account the live conn for every exit path (drops decrement conns_live).
    let _live = LiveConnGuard::new(metrics.clone());

    // Ask the daemon to open the logical connection. Bounded: the client is
    // still un-admitted and holding a pre-admission permit, so a daemon that has
    // stopped draining its channel must not be able to pin permits here. On
    // expiry the client is refused exactly as if the link were gone.
    if !matches!(
        tokio::time::timeout(
            DAEMON_HANDOFF_BUDGET,
            to_daemon.send(Message::text(
                Control::Open {
                    conn_id,
                    peer_hint: route.peer_hint().map(str::to_string),
                }
                .to_json(),
            )),
        )
        .await,
        Ok(Ok(()))
    ) {
        conns.lock().await.remove(&conn_id);
        send_refusal(&mut ws, &Control::error("no_such_node", "daemon gone")).await;
        return Ok(());
    }
    // ADMITTED: conn state is allocated and the daemon has the `Open`. From here
    // the conn is bounded by `max_conns_per_node` and a live registration, not
    // by the pre-admission pool, so the permit goes back for the next peer
    // still trying to get in.
    drop(permit);

    let (mut sink, mut stream) = ws.split();

    // Wait to be paired (daemon Opened) within the timeout.
    let paired = tokio::time::timeout(PAIR_TIMEOUT, async {
        while let Some(ev) = conn_rx.recv().await {
            match ev {
                ConnEvent::Opened => return true,
                ConnEvent::Close(..) => return false,
                // None of these should precede Opened; ignore until paired.
                ConnEvent::Data(_) | ConnEvent::Window(_) | ConnEvent::Ack(_) => {}
            }
        }
        false
    })
    .await
    .unwrap_or(false);

    if !paired {
        // Route first, then a bounded best-effort notification: teardown must
        // never depend on how fast the daemon is draining its channel.
        conns.lock().await.remove(&conn_id);
        let _ = tokio::time::timeout(
            DAEMON_HANDOFF_BUDGET,
            to_daemon.send(Message::text(
                Control::Close {
                    conn_id,
                    reason: "pair_timeout".into(),
                }
                .to_json(),
            )),
        )
        .await;
        // Same courtesy, same budget: a client that asked for a route, never got
        // one, and is not reading must not park this task either.
        send_refusal_to_sink(
            &mut sink,
            &Control::error("timeout", "daemon did not accept"),
        )
        .await;
        return Ok(());
    }
    // The first write of an ESTABLISHED conn, not a courtesy: the route is
    // paired and this frame is load-bearing protocol, so it gets the
    // established-connection liveness policy (`idle_timeout`) rather than
    // REFUSAL_WRITE_BUDGET. A client that asked for a route, was paired, and
    // then stops reading is exactly what `idle_timeout` describes, and timing
    // out here behaves as the pump's idle path does: break to teardown.
    //
    // Routed through `release_conn` rather than `?` so the daemon is told to
    // drop its half; returning early here left the pairing half-open.
    let idle = inner.idle_timeout;
    if !established_write(
        sink.send(Message::text(Control::Opened { conn_id }.to_json())),
        idle,
        &mut cancelled,
    )
    .await
    {
        return release_conn(&to_daemon, &conns, conn_id).await;
    }

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
    let mut c2d_window = ConnWindow::new(INITIAL_WINDOW);
    loop {
        let deadline = tokio::time::Instant::now() + idle;
        tokio::select! {
            // Owner cancellation: whoever removed this route from the map ends
            // the task, even if the removal raced a write below.
            _ = &mut cancelled => break,
            _ = tokio::time::sleep_until(deadline) => break,
            ev = conn_rx.recv() => match ev {
                Some(ConnEvent::Data(payload)) => {
                    if !established_write(
                        sink.send(Message::binary(encode_data(conn_id, &payload))),
                        idle,
                        &mut cancelled,
                    )
                    .await
                    {
                        break;
                    }
                    metrics.frames_relayed.fetch_add(1, Ordering::Relaxed);
                }
                Some(ConnEvent::Window(credit)) => {
                    c2d_window.set(credit);
                    if !established_write(
                        sink.send(Message::text(Control::Window { conn_id, credit }.to_json())),
                        idle,
                        &mut cancelled,
                    ).await {
                        break;
                    }
                }
                Some(ConnEvent::Ack(consumed)) => {
                    c2d_window.ack(consumed);
                    if !established_write(
                        sink.send(Message::text(Control::DataAck { conn_id, consumed }.to_json())),
                        idle,
                        &mut cancelled,
                    ).await {
                        break;
                    }
                }
                Some(ConnEvent::Close(reason, _cancel)) => {
                    // Teardown courtesy: this conn is over either way, so the
                    // goodbye gets the refusal budget, not an open-ended await.
                    send_refusal_to_sink(&mut sink, &Control::Close { conn_id, reason }).await;
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
                        // A client is only here because it is flooding past its
                        // window. Bounded on purpose: unbounded, the very frame
                        // that sheds an abusive client would let that client
                        // pin this task by simply not reading it.
                        send_refusal_to_sink(
                            &mut sink,
                            &Control::error("rate_limited", "flow-control window exceeded"),
                        )
                        .await;
                        break;
                    }
                    // Forwarding onto the SHARED daemon link: bounded and
                    // cancellable like every other established write, so one
                    // client cannot park here on a daemon that has stopped
                    // draining its channel.
                    if !established_write(
                        to_daemon.send(Message::binary(encode_data(conn_id, payload))),
                        idle,
                        &mut cancelled,
                    ).await {
                        break;
                    }
                    metrics.frames_relayed.fetch_add(1, Ordering::Relaxed);
                }
                Some(Ok(Message::Text(t))) => {
                    // Client -> daemon credit-window frames, re-stamped with the
                    // authoritative conn_id in ONE place so a client cannot
                    // touch another conn's window on the shared link.
                    let forward = match Control::from_json(&t) {
                        Ok(Control::Close { .. }) => break,
                        Ok(Control::Window { credit, .. }) => {
                            Some(Control::Window { conn_id, credit })
                        }
                        Ok(Control::DataAck { consumed, .. }) => {
                            Some(Control::DataAck { conn_id, consumed })
                        }
                        _ => None,
                    };
                    if let Some(frame) = forward
                        && !established_write(
                            to_daemon.send(Message::text(frame.to_json())),
                            idle,
                            &mut cancelled,
                        )
                        .await
                    {
                        break;
                    }
                }
                Some(Ok(Message::Ping(p))) => {
                    // Peer-triggered and NOT credit-controlled: a client can
                    // ping and then stop reading, so unlike the DATA writes
                    // below this one is not bounded by the send window. Same
                    // rule as the `Opened` write - established-connection
                    // traffic gets the established-connection budget, and
                    // missing it means the peer is gone, so break to teardown.
                    if !established_write(sink.send(Message::Pong(p)), idle, &mut cancelled).await {
                        break;
                    }
                }
                Some(Ok(Message::Pong(_))) => {}
                Some(Ok(Message::Close(_))) | Some(Err(_)) | None => break,
                _ => {}
            }
        }
    }

    release_conn(&to_daemon, &conns, conn_id).await
}

/// Release one client conn: tell the daemon to drop its half of the pairing and
/// forget the relay-side route.
///
/// EVERY exit from a paired conn goes through here. It used to be inline at the
/// end of the pump only, so the `?` on the `Opened` write returned past it: a
/// client that never took its `Opened` left the daemon believing the logical
/// connection was live and left the relay-side `conns` entry behind, to be
/// reaped lazily only if the daemon happened to send something for that
/// conn_id. Sharing one teardown is what keeps a half-finished pairing from
/// lingering on the daemon side.
///
/// Route removal comes FIRST and the notification second, bounded. Reclaiming
/// the route is the part that must always happen; telling the daemon is a
/// courtesy to a peer that may itself be the reason we are here. Notifying
/// first meant a client that was already gone kept its relay-side route - and
/// so one of the node's `max_conns_per_node` slots - for as long as a stalled
/// daemon took to drain its channel.
async fn release_conn(
    to_daemon: &mpsc::Sender<Message>,
    conns: &Mutex<ConnRoutes>,
    conn_id: u64,
) -> Result<()> {
    conns.lock().await.remove(&conn_id);
    let _ = tokio::time::timeout(
        DAEMON_HANDOFF_BUDGET,
        to_daemon.send(Message::text(
            Control::Close {
                conn_id,
                reason: "client_gone".into(),
            }
            .to_json(),
        )),
    )
    .await;
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
            Ok(Message::Text(t)) => return parse_control_text(&t),
            Ok(Message::Ping(p)) => {
                let _ = ws.send(Message::Pong(p)).await;
            }
            Ok(Message::Pong(_)) => {}
            _ => return None,
        }
    }
    None
}

fn parse_control_text(text: &str) -> Option<Control> {
    if text.len() > MAX_CONTROL_FRAME {
        return None;
    }

    Control::from_json(text).ok()
}

#[cfg(test)]
mod tests {
    use super::*;

    /// Distinct-source flood regression (August review). Pruning only
    /// refilled-to-full buckets cannot bound the tracking map: a flood that
    /// takes ONE token from each of many addresses leaves every bucket
    /// partially drained, so `retain` frees nothing. The map must stay hard
    /// bounded at `MAX_TRACKED_IPS` regardless.
    #[test]
    fn distinct_source_flood_cannot_grow_the_ip_map_past_its_bound() {
        let server = RelayServer::new(RelayConfig {
            // A burst of 1 means every touched bucket is left empty (never
            // "full"), which is exactly the state the old prune could not free.
            accept_burst_per_ip: 1,
            accept_rate_per_ip: 0.0,
            ..RelayConfig::default()
        });
        // Walk far past the bound with a fresh source address each time.
        for i in 0..(MAX_TRACKED_IPS as u32 * 2) {
            server
                .inner
                .admit_ip(IpAddr::from(std::net::Ipv4Addr::from(i)));
        }
        let tracked = server
            .inner
            .ip_buckets
            .lock()
            .expect("ip bucket lock")
            .len();
        assert!(
            tracked <= MAX_TRACKED_IPS,
            "ip bucket map grew to {tracked}, past the {MAX_TRACKED_IPS} bound"
        );
    }

    /// Eviction is by least-recent activity, so an actively flooding source is
    /// not silently forgotten (and handed a fresh burst) ahead of an idle one.
    #[test]
    fn eviction_drops_the_least_recently_active_entries_first() {
        let base = std::time::Instant::now();
        let mut map: HashMap<IpAddr, TokenBucket> = HashMap::new();
        for i in 0..4u32 {
            // Older `last_activity` for lower i.
            let at = base + std::time::Duration::from_millis(u64::from(i) * 10);
            map.insert(
                IpAddr::from(std::net::Ipv4Addr::from(i)),
                TokenBucket::new_at(1, 0.0, at),
            );
        }
        evict_least_recently_active(&mut map, 2);
        assert_eq!(map.len(), 2);
        // The two oldest went; the two most recent stayed.
        assert!(!map.contains_key(&IpAddr::from(std::net::Ipv4Addr::from(0u32))));
        assert!(!map.contains_key(&IpAddr::from(std::net::Ipv4Addr::from(1u32))));
        assert!(map.contains_key(&IpAddr::from(std::net::Ipv4Addr::from(2u32))));
        assert!(map.contains_key(&IpAddr::from(std::net::Ipv4Addr::from(3u32))));
    }

    /// Build a route with its cancellation handle, so a test can watch the
    /// cancellation that removing the route from the map is supposed to fire.
    fn test_route(
        events: mpsc::Sender<ConnEvent>,
    ) -> (ConnRoute, tokio::sync::oneshot::Receiver<()>) {
        let (cancel_tx, cancel_rx) = tokio::sync::oneshot::channel();
        (
            ConnRoute {
                events,
                _cancel_on_drop: cancel_tx,
            },
            cancel_rx,
        )
    }

    #[tokio::test]
    async fn backpressured_conn_is_closed_without_stalling_others() {
        // The shared daemon reader must never block on one conn's full
        // buffer, and only the stalled conn may be closed.
        let conns: Arc<Mutex<ConnRoutes>> = Arc::new(Mutex::new(HashMap::new()));
        let (stalled_tx, mut stalled_rx) = mpsc::channel::<ConnEvent>(1);
        let (healthy_tx, mut healthy_rx) = mpsc::channel::<ConnEvent>(1);
        let (to_daemon, mut daemon_rx) = mpsc::channel::<Message>(8);
        let (stalled_route, stalled_cancel) = test_route(stalled_tx);
        let (healthy_route, healthy_cancel) = test_route(healthy_tx);
        {
            let mut map = conns.lock().await;
            map.insert(1, stalled_route);
            map.insert(2, healthy_route);
            // Fill conn 1's buffer so the next delivery hits Full.
            map.get(&1)
                .unwrap()
                .events
                .try_send(ConnEvent::Opened)
                .unwrap();
        }

        deliver_conn_event(&conns, &to_daemon, 1, ConnEvent::Data(vec![1])).await;
        // The stalled conn is torn down: gone from the map, daemon notified.
        assert!(!conns.lock().await.contains_key(&1));
        let msg = daemon_rx.recv().await.expect("daemon close notify");
        let text = msg.into_text().expect("close notify is a text frame");
        assert!(
            text.contains("client_backpressured") && text.contains("\"conn_id\":1"),
            "got: {text}"
        );

        // The healthy conn still receives: no cross-conn stall.
        deliver_conn_event(&conns, &to_daemon, 2, ConnEvent::Data(vec![2])).await;
        assert!(matches!(healthy_rx.recv().await, Some(ConnEvent::Data(p)) if p == vec![2]));

        // The stalled conn's channel closes once drained: the buffered event,
        // then None (its last sender was dropped with the map entry).
        assert!(matches!(stalled_rx.recv().await, Some(ConnEvent::Opened)));
        assert!(stalled_rx.recv().await.is_none());
        // Channel closure only reaches a task that is POLLING it, so removal
        // must also fire the route's cancellation - that is what reaches a task
        // parked in a peer write. The healthy route keeps its cancellation
        // un-fired, so this is removal and not a blanket teardown.
        let mut stalled_cancel = stalled_cancel;
        assert!(
            stalled_cancel.try_recv().is_err(),
            "the removed route's cancellation must have fired"
        );
        let mut healthy_cancel = healthy_cancel;
        assert!(
            matches!(
                healthy_cancel.try_recv(),
                Err(tokio::sync::oneshot::error::TryRecvError::Empty)
            ),
            "a live route must NOT be cancelled"
        );
        // A delivery to the now-unknown conn id is a silent no-op.
        deliver_conn_event(&conns, &to_daemon, 1, ConnEvent::Opened).await;
        assert!(daemon_rx.try_recv().is_err());
    }

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

    #[test]
    fn oversized_control_frame_is_rejected_before_json_parse() {
        let oversized = Control::Hello {
            daemon_pubkey: "a".repeat(MAX_CONTROL_FRAME),
            node_id: "node".into(),
            relay_token: None,
        }
        .to_json();

        assert!(oversized.len() > MAX_CONTROL_FRAME);
        assert!(parse_control_text(&oversized).is_none());
    }
}

/// The fail-closed public-open rule. Moved here from the binary with the helper
/// itself, because the rule now has to apply to every policy the relay adopts
/// (startup AND SIGHUP reload), not only to the one it parsed at startup.
#[cfg(test)]
mod admission_guard_tests {
    use super::*;

    fn policy(mode: Admission, relay_token: Option<&str>) -> AdmissionPolicy {
        AdmissionPolicy {
            registration_mode: mode,
            relay_token: relay_token.map(str::to_string),
            ..AdmissionPolicy::default()
        }
    }

    #[test]
    fn public_open_tokenless_is_unguarded() {
        assert!(public_open_unguarded(
            "0.0.0.0:8443",
            &Admission::Open,
            None
        ));
        assert!(public_open_unguarded(
            "34.209.38.50:443",
            &Admission::Open,
            Some("  ")
        ));
        // A hostname that is not localhost is conservatively public.
        assert!(public_open_unguarded(
            "relay.example.com:443",
            &Admission::Open,
            None
        ));
    }

    #[test]
    fn loopback_token_or_allowlist_are_guarded() {
        assert!(!public_open_unguarded(
            "127.0.0.1:8443",
            &Admission::Open,
            None
        ));
        assert!(!public_open_unguarded("[::1]:8443", &Admission::Open, None));
        assert!(!public_open_unguarded(
            "localhost:8443",
            &Admission::Open,
            None
        ));
        assert!(!public_open_unguarded(
            "0.0.0.0:8443",
            &Admission::Open,
            Some("secret")
        ));
        assert!(!public_open_unguarded(
            "0.0.0.0:8443",
            &Admission::Allowlist,
            None
        ));
    }

    /// The guard is the same object at startup and at reload: only the wording
    /// of the refusal differs, so an operator cannot get a policy past one entry
    /// point that the other would reject.
    #[test]
    fn startup_and_reload_refuse_the_same_policy() {
        let unguarded = policy(Admission::Open, None);
        let guard = PublicOpenGuard::new("0.0.0.0:8443", false);
        let startup = guard
            .check_startup(&unguarded)
            .expect_err("startup must refuse an unguarded public open policy");
        assert!(startup.to_string().starts_with("refusing to start:"));
        let reload = guard
            .check(&unguarded, "refusing to reload admission")
            .expect_err("reload must refuse the same policy");
        assert!(
            reload
                .to_string()
                .starts_with("refusing to reload admission:")
        );
        // Same condition, same remedy text.
        let tail = |m: &str| m.split_once(": ").expect("prefixed message").1.to_string();
        assert_eq!(tail(&startup.to_string()), tail(&reload.to_string()));

        // The explicit opt-in, an allowlist, and a token each clear the guard.
        assert!(
            PublicOpenGuard::new("0.0.0.0:8443", true)
                .check_startup(&unguarded)
                .is_ok()
        );
        assert!(
            guard
                .check_startup(&policy(Admission::Allowlist, None))
                .is_ok()
        );
        assert!(
            guard
                .check_startup(&policy(Admission::Open, Some("s3cret")))
                .is_ok()
        );
    }
}

/// Registration atomicity + setup-write deadline regressions (August review).
///
/// The registry entry is inserted BEFORE the `Registered` confirmation can be
/// written, and both outbound setup writes go to a peer-controlled sink. These
/// tests drive `handle_daemon` over an in-memory link so the peer's reads and
/// the link's failure mode are exact, rather than racing a real socket's close
/// against the relay's next write.
#[cfg(test)]
mod registration_tests {
    use super::*;
    use ring::signature::{Ed25519KeyPair, KeyPair};
    use std::io;
    use std::pin::Pin;
    use std::sync::atomic::AtomicBool;
    use std::task::{Context as TaskContext, Poll};
    use tokio::io::{DuplexStream, ReadBuf};
    use tokio_tungstenite::tungstenite::protocol::Role;

    /// The relay's end of a link whose WRITE direction can be killed on demand.
    /// A peer that resets its connection leaves the relay exactly here: bytes the
    /// peer already sent are still readable, every write fails. Forcing that
    /// state directly is what makes the regression deterministic - a real peer's
    /// close races the relay's next write, so the failing send is not guaranteed.
    struct KillableWrite {
        inner: DuplexStream,
        dead: Arc<AtomicBool>,
    }

    impl KillableWrite {
        fn gone(&self) -> bool {
            self.dead.load(Ordering::Relaxed)
        }
    }

    impl AsyncRead for KillableWrite {
        fn poll_read(
            mut self: Pin<&mut Self>,
            cx: &mut TaskContext<'_>,
            buf: &mut ReadBuf<'_>,
        ) -> Poll<io::Result<()>> {
            Pin::new(&mut self.inner).poll_read(cx, buf)
        }
    }

    impl AsyncWrite for KillableWrite {
        fn poll_write(
            mut self: Pin<&mut Self>,
            cx: &mut TaskContext<'_>,
            data: &[u8],
        ) -> Poll<io::Result<usize>> {
            if self.gone() {
                return Poll::Ready(Err(io::Error::new(io::ErrorKind::BrokenPipe, "peer gone")));
            }
            Pin::new(&mut self.inner).poll_write(cx, data)
        }

        fn poll_flush(mut self: Pin<&mut Self>, cx: &mut TaskContext<'_>) -> Poll<io::Result<()>> {
            if self.gone() {
                return Poll::Ready(Err(io::Error::new(io::ErrorKind::BrokenPipe, "peer gone")));
            }
            Pin::new(&mut self.inner).poll_flush(cx)
        }

        fn poll_shutdown(
            mut self: Pin<&mut Self>,
            cx: &mut TaskContext<'_>,
        ) -> Poll<io::Result<()>> {
            Pin::new(&mut self.inner).poll_shutdown(cx)
        }
    }

    /// The daemon side of a registration under test.
    pub(super) struct PeerLink {
        pub(super) ws: WebSocketStream<DuplexStream>,
        kp: Ed25519KeyPair,
        /// The signing key in PKCS#8, so a test can re-register the SAME node-id
        /// with the SAME key (the supersede path; a fresh key gets node_taken).
        pub(super) pkcs8: Vec<u8>,
        node_id: String,
        dead: Arc<AtomicBool>,
    }

    impl PeerLink {
        /// Read the relay's `Challenge` and return its nonce. Returning proves
        /// the relay finished its FIRST setup write and is now parked reading,
        /// which is what makes the rest deterministic: the relay cannot write
        /// again until it has read the frame the test sends next.
        pub(super) async fn challenge(&mut self) -> Vec<u8> {
            match next_control(&mut self.ws).await {
                Some(Control::Challenge { nonce }) => B64.decode(nonce.as_bytes()).expect("nonce"),
                other => panic!("expected a challenge, got {other:?}"),
            }
        }

        /// Answer a challenge with a valid `Register`, then stop. The peer does
        /// NOT read the confirmation that follows.
        pub(super) async fn register(&mut self, nonce: &[u8]) {
            let sig = self.kp.sign(nonce).as_ref().to_vec();
            self.send_register(&sig).await;
        }

        /// Answer a challenge with a signature that does NOT verify, then stop.
        /// Drives the relay onto its `bad_sig` refusal.
        async fn register_with_invalid_signature(&mut self, nonce: &[u8]) {
            let mut sig = self.kp.sign(nonce).as_ref().to_vec();
            sig[0] ^= 0xff;
            self.send_register(&sig).await;
        }

        async fn send_register(&mut self, sig: &[u8]) {
            self.ws
                .send(Message::text(
                    Control::Register {
                        node_id: self.node_id.clone(),
                        sig: B64.encode(sig),
                    }
                    .to_json(),
                ))
                .await
                .expect("register frame");
        }

        /// Make every later relay write to this link fail, as a reset peer would.
        fn abandon(&self) {
            self.dead.store(true, Ordering::Relaxed);
        }
    }

    /// Start one `handle_daemon` registration over an in-memory link of
    /// `link_buffer` bytes per direction (small buffers let a peer that stops
    /// reading stall the relay's outbound setup writes).
    pub(super) async fn start_registration(
        server: &RelayServer,
        node_id: &str,
        link_buffer: usize,
    ) -> (PeerLink, tokio::task::JoinHandle<Result<()>>) {
        start_registration_with(server, node_id, link_buffer, None).await
    }

    /// As [`start_registration`], with the `relay_token` the peer presented in
    /// its `Hello` (the shared-secret gate the relay checks before anything
    /// else it might reply with).
    async fn start_registration_with(
        server: &RelayServer,
        node_id: &str,
        link_buffer: usize,
        relay_token: Option<String>,
    ) -> (PeerLink, tokio::task::JoinHandle<Result<()>>) {
        start_registration_keyed(server, node_id, link_buffer, relay_token, None).await
    }

    /// As [`start_registration_with`], reusing `pkcs8` when given so a test can
    /// re-register the same node-id with the same key.
    pub(super) async fn start_registration_keyed(
        server: &RelayServer,
        node_id: &str,
        link_buffer: usize,
        relay_token: Option<String>,
        pkcs8: Option<Vec<u8>>,
    ) -> (PeerLink, tokio::task::JoinHandle<Result<()>>) {
        let (peer_io, relay_io) = tokio::io::duplex(link_buffer);
        let dead = Arc::new(AtomicBool::new(false));
        let relay_ws = WebSocketStream::from_raw_socket(
            KillableWrite {
                inner: relay_io,
                dead: dead.clone(),
            },
            Role::Server,
            None,
        )
        .await;
        let peer_ws = WebSocketStream::from_raw_socket(peer_io, Role::Client, None).await;

        let pkcs8 = pkcs8.unwrap_or_else(|| {
            Ed25519KeyPair::generate_pkcs8(&SystemRandom::new())
                .expect("keypair")
                .as_ref()
                .to_vec()
        });
        let kp = Ed25519KeyPair::from_pkcs8(&pkcs8).expect("keypair");
        let pubkey = B64.encode(kp.public_key().as_ref());

        let inner = server.inner.clone();
        let permit = inner
            .handshake_permits
            .clone()
            .try_acquire_owned()
            .expect("a free handshake permit");
        let deadline = tokio::time::Instant::now() + inner.handshake_timeout;
        let node = node_id.to_string();
        let task = tokio::spawn(async move {
            handle_daemon(inner, relay_ws, pubkey, node, relay_token, permit, deadline).await
        });
        (
            PeerLink {
                ws: peer_ws,
                kp,
                pkcs8,
                node_id: node_id.to_string(),
                dead,
            },
            task,
        )
    }

    /// A registration whose `Registered` confirmation cannot be delivered must be
    /// undone: the registry entry the relay just inserted is removed, the permit
    /// is released, and the node-id is immediately reclaimable. Otherwise the
    /// registry keeps a `DaemonHandle` whose channel points at a dead link -
    /// clients route to it until the relay restarts, and enough failed
    /// registrations consume `max_registered_nodes`.
    #[tokio::test]
    async fn failed_confirmation_frees_the_node_id_and_the_permit() {
        let server = RelayServer::new(RelayConfig {
            max_pending_handshakes: 1,
            max_registered_nodes: 1,
            handshake_timeout: Duration::from_secs(5),
            ..RelayConfig::default()
        });
        let (mut peer, task) = start_registration(&server, "node-lost", 64 * 1024).await;
        // The daemon is already gone when the relay answers: the link stops
        // accepting writes while the relay is parked awaiting `Register`, so the
        // confirmation send that follows is guaranteed to be the failing one.
        let nonce = peer.challenge().await;
        peer.abandon();
        peer.register(&nonce).await;

        let _ = tokio::time::timeout(Duration::from_secs(5), task)
            .await
            .expect("the relay must not park on a dead link")
            .expect("the registration task must not panic");

        assert!(
            !server.inner.daemons.lock().await.contains_key("node-lost"),
            "a registration whose confirmation never reached the daemon must not stay in the registry"
        );
        assert_eq!(
            server.inner.handshake_permits.available_permits(),
            1,
            "the pre-classification permit must be released with the failed registration"
        );

        // Reclaimable at once: a fresh key registering the SAME node-id is
        // admitted. A leaked entry would answer `node_taken` (it is bound to the
        // abandoned link's fingerprint) and, at a ceiling of one, would also have
        // consumed the registry.
        let (mut peer, task) = start_registration(&server, "node-lost", 64 * 1024).await;
        let nonce = peer.challenge().await;
        peer.register(&nonce).await;
        match next_control(&mut peer.ws).await {
            Some(Control::Registered { node_id, .. }) => assert_eq!(node_id, "node-lost"),
            other => {
                panic!("the node-id must be reclaimable after a failed confirmation, got {other:?}")
            }
        }
        task.abort();
    }

    /// The `Registered` write goes to a peer-controlled sink, so it must share the
    /// ONE absolute setup deadline rather than awaiting forever. A daemon that
    /// stops reading must be reaped, and its half-finished registration undone.
    #[tokio::test]
    async fn confirmation_that_misses_the_setup_deadline_frees_the_node_id() {
        let server = RelayServer::new(RelayConfig {
            max_pending_handshakes: 1,
            handshake_timeout: Duration::from_millis(400),
            ..RelayConfig::default()
        });
        // A link too small to absorb the confirmation, and a peer that never
        // reads it: the outbound setup write is what is under test.
        let (mut peer, task) = start_registration(&server, "node-stuck", 32).await;
        let nonce = peer.challenge().await;
        peer.register(&nonce).await;

        let finished = tokio::time::timeout(Duration::from_secs(5), task).await;
        assert!(
            finished.is_ok(),
            "the confirmation write must be bounded by the shared setup deadline"
        );
        assert!(
            !server.inner.daemons.lock().await.contains_key("node-stuck"),
            "a confirmation that missed the deadline must not leave a live registry entry"
        );
        assert_eq!(
            server.inner.handshake_permits.available_permits(),
            1,
            "the permit must not be pinned by a peer that stops reading"
        );
        drop(peer);
    }

    /// A REFUSED registration is the same resource story as an accepted one.
    /// The relay's error replies are peer-controlled sink writes too, so a peer
    /// that deliberately trips one and then stops reading would otherwise pin a
    /// `max_pending_handshakes` permit (and its TLS/WebSocket state) for as long
    /// as it likes - the documented setup budget having long since elapsed.
    /// Repeated across enough source addresses that exhausts the global
    /// pre-classification bound using nothing but invalid handshakes.
    ///
    /// Early path: the shared-secret gate, which the relay checks before it has
    /// written anything at all, so the refusal is the FIRST write on the link.
    #[tokio::test]
    async fn refused_hello_does_not_pin_a_permit_past_the_deadline() {
        let server = RelayServer::new(RelayConfig {
            max_pending_handshakes: 1,
            handshake_timeout: Duration::from_millis(400),
            relay_token: Some("expected-secret".into()),
            ..RelayConfig::default()
        });
        let started = tokio::time::Instant::now();
        // Wrong token, and a link too small to absorb the refusal from a peer
        // that never reads it.
        let (peer, task) =
            start_registration_with(&server, "node-badtoken", 8, Some("wrong".into())).await;

        let finished = tokio::time::timeout(Duration::from_secs(5), task).await;
        assert!(
            finished.is_ok(),
            "a refusal write must share the one setup deadline"
        );
        assert!(
            started.elapsed() < Duration::from_secs(2),
            "the refusal was reaped after {:?}, far past the 400ms setup budget",
            started.elapsed()
        );
        assert_eq!(
            server.inner.handshake_permits.available_permits(),
            1,
            "a refused Hello must return its pre-classification permit"
        );
        drop(peer);
    }

    /// Post-Hello path: an invalid signature. The peer takes the challenge
    /// (which is what proves the relay is parked reading and cannot write again
    /// until it consumes the next frame), answers it with a signature that does
    /// not verify, and stops reading - so the `bad_sig` refusal is
    /// deterministically the write that stalls.
    #[tokio::test]
    async fn refused_register_does_not_pin_a_permit_past_the_deadline() {
        let server = RelayServer::new(RelayConfig {
            max_pending_handshakes: 1,
            handshake_timeout: Duration::from_millis(400),
            ..RelayConfig::default()
        });
        let started = tokio::time::Instant::now();
        let (mut peer, task) = start_registration(&server, "node-badsig", 32).await;
        let nonce = peer.challenge().await;
        peer.register_with_invalid_signature(&nonce).await;

        let finished = tokio::time::timeout(Duration::from_secs(5), task).await;
        assert!(
            finished.is_ok(),
            "the bad_sig refusal must share the one setup deadline"
        );
        assert!(
            started.elapsed() < Duration::from_secs(2),
            "the refusal was reaped after {:?}, far past the 400ms setup budget",
            started.elapsed()
        );
        assert_eq!(
            server.inner.handshake_permits.available_permits(),
            1,
            "a refused Register must return its pre-classification permit"
        );
        assert!(
            server.inner.daemons.lock().await.is_empty(),
            "a refused registration must not have touched the registry"
        );
        drop(peer);
    }

    /// The other outbound setup write. A peer that completes the WebSocket
    /// upgrade and then stops reading must not be able to park the relay - and
    /// its permit - inside the `Challenge` send, outside the setup budget.
    #[tokio::test]
    async fn challenge_write_shares_the_setup_deadline() {
        let server = RelayServer::new(RelayConfig {
            max_pending_handshakes: 1,
            handshake_timeout: Duration::from_millis(400),
            ..RelayConfig::default()
        });
        let (peer, task) = start_registration(&server, "node-mute", 8).await;

        let finished = tokio::time::timeout(Duration::from_secs(5), task).await;
        assert!(
            finished.is_ok(),
            "the challenge write must share the one setup deadline"
        );
        assert_eq!(
            server.inner.handshake_permits.available_permits(),
            1,
            "a permit must not be pinned by a peer that stops reading mid-setup"
        );
        drop(peer);
    }
}

/// Post-classification refusal writes (August review, follow-up). A client that
/// is refused a route holds no handshake permit, so it cannot exhaust
/// `max_pending_handshakes` - but nothing else bounded the goodbye frame either.
/// A peer that asks for a route it cannot have and then stops reading parked the
/// relay task and its TLS session with no deadline at all: the setup budget is
/// spent by then, and `idle_timeout` only starts inside the pump loop this
/// connection never reaches. Repeated across source addresses that accumulates
/// relay tasks using nothing but well-formed, promptly-refused requests.
#[cfg(test)]
mod client_refusal_tests {
    use super::*;
    use tokio_tungstenite::tungstenite::protocol::Role;

    /// The test runs on a PAUSED clock: tokio auto-advances it whenever every
    /// task is parked, so the real 5s budget elapses in microseconds of wall
    /// time and `elapsed()` reports the exact virtual budget that fired. If the
    /// refusal write is unbounded, no relay timer exists at all and the only
    /// timer left is this test's own guard - which is what makes the failure
    /// visible instead of hanging the suite.
    #[tokio::test(start_paused = true)]
    async fn refused_client_connect_does_not_park_the_relay() {
        let server = RelayServer::new(RelayConfig::default());
        // A link far too small for the refusal frame, and a peer that never
        // reads it: `peer` is held to keep the link open, never polled.
        let (peer_io, relay_io) = tokio::io::duplex(8);
        let relay_ws = WebSocketStream::from_raw_socket(relay_io, Role::Server, None).await;
        let peer = WebSocketStream::from_raw_socket(peer_io, Role::Client, None).await;

        let inner = server.inner.clone();
        let permit = inner
            .handshake_permits
            .clone()
            .try_acquire_owned()
            .expect("a free handshake permit");
        let started = tokio::time::Instant::now();
        let task = tokio::spawn(async move {
            handle_client(
                inner,
                relay_ws,
                "no-such-node".into(),
                ClientRoute::Wss,
                permit,
            )
            .await
        });

        let finished = tokio::time::timeout(REFUSAL_WRITE_BUDGET * 4, task).await;
        assert!(
            finished.is_ok(),
            "a refused client must not park the relay task on an unread goodbye frame"
        );
        assert!(
            started.elapsed() < REFUSAL_WRITE_BUDGET * 2,
            "the refusal took {:?}, past the {REFUSAL_WRITE_BUDGET:?} courtesy budget",
            started.elapsed()
        );
        drop(peer);
    }
}

/// The success path's one pre-pump write. A client that asks for a route, gets
/// PAIRED, and then stops reading is not a refusal case - the frame is
/// load-bearing protocol - so it is bounded by the established-connection
/// liveness policy (`idle_timeout`) and tears down exactly as the pump's idle
/// path does. Unbounded, it parked the relay task; worse, the `?` that used to
/// end it returned past the teardown, so the daemon was never told to drop its
/// half of the pairing.
#[cfg(test)]
mod paired_client_tests {
    use super::*;
    use tokio_tungstenite::tungstenite::protocol::Role;

    /// Pair a client, hand it a DATA event, and stall its sink. Returns the
    /// paired `conn_id` and the relay task; the client peer is never read.
    async fn pair_then_stall(
        server: &RelayServer,
        node_id: &str,
        daemon_rx: &mut mpsc::Receiver<Message>,
        conns: &Arc<Mutex<ConnRoutes>>,
        payload: Vec<u8>,
    ) -> (
        WebSocketStream<tokio::io::DuplexStream>,
        u64,
        tokio::task::JoinHandle<Result<()>>,
    ) {
        // Big enough for the small `Opened` frame, far too small for the DATA
        // frame that follows: the pump is entered, then the write stalls.
        let (peer_io, relay_io) = tokio::io::duplex(96);
        let relay_ws = WebSocketStream::from_raw_socket(relay_io, Role::Server, None).await;
        let peer = WebSocketStream::from_raw_socket(peer_io, Role::Client, None).await;
        let inner = server.inner.clone();
        let permit = inner
            .handshake_permits
            .clone()
            .try_acquire_owned()
            .expect("a free handshake permit");
        let node = node_id.to_string();
        let task = tokio::spawn(async move {
            handle_client(inner, relay_ws, node, ClientRoute::Wss, permit).await
        });

        let open = daemon_rx.recv().await.expect("the relay asks for an Open");
        let conn_id = match Control::from_json(&open.into_text().expect("text frame")) {
            Ok(Control::Open { conn_id, .. }) => conn_id,
            other => panic!("expected an Open, got {other:?}"),
        };
        let events = conns
            .lock()
            .await
            .get(&conn_id)
            .map(|route| route.events.clone())
            .expect("the relay registered the conn");
        events
            .send(ConnEvent::Opened)
            .await
            .expect("accept the pairing");
        // Now a DATA frame the stalled client cannot absorb.
        events
            .send(ConnEvent::Data(payload))
            .await
            .expect("queue daemon data");
        for _ in 0..8 {
            tokio::task::yield_now().await;
        }
        (peer, conn_id, task)
    }

    /// The gap the `Opened`-only test left: a client that pairs, then stops
    /// reading once DATA starts flowing. The pump's outer `select!` has an idle
    /// timer, but a `select!` stops polling its branches while a nested write is
    /// pending - so the idle policy was DEAD exactly when it was needed, and the
    /// task kept its socket, its route and its `LiveConnGuard` indefinitely.
    #[tokio::test(start_paused = true)]
    async fn paired_client_that_stalls_on_data_is_reclaimed() {
        let idle = Duration::from_secs(2);
        let server = RelayServer::new(RelayConfig {
            idle_timeout: idle,
            max_pending_handshakes: 4,
            ..RelayConfig::default()
        });
        let (mut daemon_rx, conns) = register_stub_daemon(&server, "node-data").await;

        let started = tokio::time::Instant::now();
        let (peer, conn_id, task) = pair_then_stall(
            &server,
            "node-data",
            &mut daemon_rx,
            &conns,
            vec![0u8; 4096],
        )
        .await;

        let finished = tokio::time::timeout(idle * 8, task).await;
        assert!(
            finished.is_ok(),
            "a paired client stalled mid-DATA must be reaped by the idle policy"
        );
        assert!(
            started.elapsed() < idle * 2,
            "reclaimed after {:?}, past the {idle:?} liveness budget",
            started.elapsed()
        );

        // Resources: the route, the daemon's half, and the live-conn accounting.
        assert!(
            !conns.lock().await.contains_key(&conn_id),
            "the relay-side route must be reclaimed"
        );
        let status = server.status().await;
        let node = status
            .nodes
            .iter()
            .find(|n| n.node_id == "node-data")
            .expect("the node is still registered");
        assert_eq!(
            node.conns_live, 0,
            "LiveConnGuard must have released the live-conn count"
        );
        drop(peer);
    }

    /// Route removal must reach a task that is parked in a peer write. Dropping
    /// the event sender only wakes a task that is polling its receiver; the
    /// route's cancellation is what reaches one that is not. Without it the task
    /// lingered to its idle timeout while no longer counting against
    /// `max_conns_per_node` - so the same wedge could be recreated without bound.
    #[tokio::test(start_paused = true)]
    async fn removing_a_route_cancels_a_task_parked_in_a_write() {
        let idle = Duration::from_secs(60);
        let server = RelayServer::new(RelayConfig {
            idle_timeout: idle,
            max_pending_handshakes: 4,
            ..RelayConfig::default()
        });
        let (mut daemon_rx, conns) = register_stub_daemon(&server, "node-cancel").await;
        let (peer, conn_id, task) = pair_then_stall(
            &server,
            "node-cancel",
            &mut daemon_rx,
            &conns,
            vec![0u8; 4096],
        )
        .await;

        // The task is parked in the DATA write with a long idle budget. Removing
        // the route must end it now, not in a minute.
        let removed_at = tokio::time::Instant::now();
        conns.lock().await.remove(&conn_id);
        let finished = tokio::time::timeout(idle / 2, task).await;
        assert!(
            finished.is_ok(),
            "removing a route must cancel its task even mid-write"
        );
        assert!(
            removed_at.elapsed() < idle / 2,
            "cancellation took {:?}; it must not wait out the idle budget",
            removed_at.elapsed()
        );
        drop(peer);
    }

    /// Register a routable node directly, so the test can play the daemon
    /// without standing up a second handshake.
    async fn register_stub_daemon(
        server: &RelayServer,
        node_id: &str,
    ) -> (mpsc::Receiver<Message>, Arc<Mutex<ConnRoutes>>) {
        let (to_daemon, daemon_rx) = mpsc::channel::<Message>(8);
        let conns: Arc<Mutex<ConnRoutes>> = Arc::new(Mutex::new(HashMap::new()));
        server.inner.daemons.lock().await.insert(
            node_id.to_string(),
            DaemonHandle {
                fpr: "stub-fingerprint".into(),
                epoch: 1,
                to_daemon,
                conns: conns.clone(),
                metrics: Arc::new(NodeMetrics::default()),
                connect_bucket: Arc::new(Mutex::new(TokenBucket::new(60, 20.0))),
                supersede: Arc::new(tokio::sync::Notify::new()),
            },
        );
        (daemon_rx, conns)
    }

    /// Paused clock (see `client_refusal_tests`): the virtual `idle_timeout`
    /// elapses in no wall time, and `elapsed()` reports which budget fired.
    /// A 2s idle timeout against a 4s assertion also pins the CHOICE of budget:
    /// the 5s courtesy budget would overshoot it.
    #[tokio::test(start_paused = true)]
    async fn paired_client_that_never_reads_is_reaped_and_releases_the_daemon() {
        let idle = Duration::from_secs(2);
        let server = RelayServer::new(RelayConfig {
            idle_timeout: idle,
            // A pool of one, so "the admitted client gave its permit back" is a
            // crisp assertion below rather than arithmetic on the default.
            max_pending_handshakes: 1,
            ..RelayConfig::default()
        });
        let (mut daemon_rx, conns) = register_stub_daemon(&server, "node-paired").await;

        // A link far too small for the `Opened` frame, and a client that never
        // reads it: `peer` is held to keep the link open, never polled.
        let (peer_io, relay_io) = tokio::io::duplex(8);
        let relay_ws = WebSocketStream::from_raw_socket(relay_io, Role::Server, None).await;
        let peer = WebSocketStream::from_raw_socket(peer_io, Role::Client, None).await;

        let inner = server.inner.clone();
        let permit = inner
            .handshake_permits
            .clone()
            .try_acquire_owned()
            .expect("a free handshake permit");
        let started = tokio::time::Instant::now();
        let task = tokio::spawn(async move {
            handle_client(
                inner,
                relay_ws,
                "node-paired".into(),
                ClientRoute::Wss,
                permit,
            )
            .await
        });

        // Play the daemon: take the Open and accept it, so the conn is PAIRED
        // before the relay tries to tell the client about it.
        let open = daemon_rx.recv().await.expect("the relay asks for an Open");
        let conn_id = match Control::from_json(&open.into_text().expect("text frame")) {
            Ok(Control::Open { conn_id, .. }) => conn_id,
            other => panic!("expected an Open, got {other:?}"),
        };
        // The other half of the admission contract: a client the relay has
        // routed to a daemon is ADMITTED, so it must have handed its
        // pre-admission permit back - a paired conn is bounded by
        // `max_conns_per_node`, not by the setup pool, and an exhausted pool
        // must never be able to stall established traffic.
        assert_eq!(
            server.inner.handshake_permits.available_permits(),
            1,
            "an admitted client must not keep holding a pre-admission permit"
        );

        let conn_tx = conns
            .lock()
            .await
            .get(&conn_id)
            .map(|route| route.events.clone())
            .expect("the relay registered the conn");
        conn_tx
            .send(ConnEvent::Opened)
            .await
            .expect("accept the pairing");

        let finished = tokio::time::timeout(idle * 8, task).await;
        assert!(
            finished.is_ok(),
            "a paired client that never reads its Opened must not park the relay task"
        );
        assert!(
            started.elapsed() < idle * 2,
            "the conn was reaped after {:?}, past the {idle:?} idle budget",
            started.elapsed()
        );

        // The daemon side must not half-linger: it is told to release its half,
        // and the relay-side route is forgotten.
        let close = daemon_rx
            .recv()
            .await
            .expect("the daemon is told to release its half");
        let text = close.into_text().expect("text frame");
        assert!(
            text.contains("client_gone") && text.contains(&format!("\"conn_id\":{conn_id}")),
            "expected a Close for this conn, got: {text}"
        );
        assert!(
            !conns.lock().await.contains_key(&conn_id),
            "the relay-side route must be forgotten with the client"
        );
        drop(peer);
    }
}

/// The COUNT of refusal tasks, not their duration. Each refusal write is
/// deadline-bounded, but releasing the admission permit at classification meant
/// a peer could name a nonexistent node, stop reading, and hold a task and a TLS
/// session for the whole refusal budget while its permit was already back in the
/// pool for the next one. Nothing in that path needs a daemon or any prior
/// state, so it is floodable from nothing across many source addresses. The
/// permit now spans the refusal, which is what bounds how many can exist at once.
#[cfg(test)]
mod refusal_bound_tests {
    use super::*;
    use tokio_tungstenite::tungstenite::protocol::Role;

    /// One client that completes the upgrade, names a node that does not exist,
    /// and never reads the refusal. Returns its end of the link (held open so
    /// the link stays up, never polled) and the relay task.
    #[allow(clippy::type_complexity)]
    async fn park_a_refusal(
        server: &RelayServer,
        node_id: &str,
    ) -> (
        WebSocketStream<tokio::io::DuplexStream>,
        tokio::task::JoinHandle<Result<()>>,
    ) {
        // A link far too small for the refusal frame, so the write stalls.
        let (peer_io, relay_io) = tokio::io::duplex(8);
        let relay_ws = WebSocketStream::from_raw_socket(relay_io, Role::Server, None).await;
        let peer = WebSocketStream::from_raw_socket(peer_io, Role::Client, None).await;

        let inner = server.inner.clone();
        let permit = inner
            .handshake_permits
            .clone()
            .try_acquire_owned()
            .expect("a free handshake permit");
        let node = node_id.to_string();
        let task = tokio::spawn(async move {
            handle_client(inner, relay_ws, node, ClientRoute::Wss, permit).await
        });
        (peer, task)
    }

    /// Paused clock (see `client_refusal_tests`): the refusal budget elapses in
    /// no wall time.
    #[tokio::test(start_paused = true)]
    async fn refusal_tasks_are_capped_by_the_pre_admission_pool() {
        let server = RelayServer::new(RelayConfig {
            max_pending_handshakes: 2,
            ..RelayConfig::default()
        });

        // Two distinct clients take the whole pool asking for nodes that do not
        // exist, then stop reading.
        let (peer_a, task_a) = park_a_refusal(&server, "ghost-a").await;
        let (peer_b, task_b) = park_a_refusal(&server, "ghost-b").await;
        // Let both reach the stalled refusal write. Yielding never lets the
        // paused clock advance: this task is re-queued ready each time, so the
        // runtime is never idle.
        for _ in 0..8 {
            tokio::task::yield_now().await;
        }

        // A third connection finds the pool empty - and an empty pool is exactly
        // what makes the accept loop shed a socket before spending a TLS
        // handshake on it. Before the fix both permits were already back and
        // this third peer (and any number after it) would be accepted.
        assert!(
            server
                .inner
                .handshake_permits
                .clone()
                .try_acquire_owned()
                .is_err(),
            "a refusal must hold its slot while it holds a task and a TLS session"
        );

        // Capacity returns when the refusal budget expires.
        for task in [task_a, task_b] {
            let finished = tokio::time::timeout(REFUSAL_WRITE_BUDGET * 4, task).await;
            assert!(
                finished.is_ok(),
                "a parked refusal must still be reaped at its budget"
            );
        }
        assert_eq!(
            server.inner.handshake_permits.available_permits(),
            2,
            "the whole pool must come back once the refusals are reaped"
        );
        assert!(
            server
                .inner
                .handshake_permits
                .clone()
                .try_acquire_owned()
                .is_ok(),
            "a later peer must be able to enter setup again"
        );
        drop((peer_a, peer_b));
    }
}

/// A registered daemon that stops reading must not be able to wedge the relay
/// (August review, follow-up). `to_daemon` is a bounded channel behind a writer
/// task, so an unbounded writer let one unresponsive peer park the writer, fill
/// the channel, and then park every path that sends into it - the SHARED daemon
/// reader included. That would have cost per-connection isolation, teardown, and
/// the supersede signal for every other conn on the node.
#[cfg(test)]
mod wedged_daemon_tests {
    use super::registration_tests::{PeerLink, start_registration, start_registration_keyed};
    use super::*;
    use tokio_tungstenite::tungstenite::protocol::Role;

    /// Register a daemon over a link too small to drain, then STOP reading it
    /// and fill its outbound channel to capacity. That is the real wedge: the
    /// writer is parked on the socket and every further send into the channel
    /// would block. Returns the peer and the relay task.
    async fn wedge_a_daemon(
        server: &RelayServer,
        node_id: &str,
    ) -> (PeerLink, tokio::task::JoinHandle<Result<()>>) {
        let (mut peer, task) = start_registration(server, node_id, 96).await;
        let nonce = peer.challenge().await;
        peer.register(&nonce).await;
        match next_control(&mut peer.ws).await {
            Some(Control::Registered { .. }) => {}
            other => panic!("the daemon must register before it wedges, got {other:?}"),
        }

        // Fill the daemon's outbound channel. The writer is stuck on a socket
        // nobody is draining, so nothing leaves the queue.
        let to_daemon = server
            .inner
            .daemons
            .lock()
            .await
            .get(node_id)
            .map(|h| h.to_daemon.clone())
            .expect("the daemon registered");
        // Repeat: the writer pulls a message off the queue and only parks once
        // the socket buffer itself is saturated, so a single fill would leave
        // the slots it drained free. Once it is parked mid-write nothing else
        // leaves the queue and the channel stays full.
        for _ in 0..8 {
            let mut queued = 0;
            while to_daemon.try_send(Message::text("filler")).is_ok() {
                queued += 1;
                assert!(queued < 10_000, "the daemon channel should be bounded");
            }
            for _ in 0..8 {
                tokio::task::yield_now().await;
            }
        }
        assert!(
            to_daemon.try_send(Message::text("filler")).is_err(),
            "the wedge must leave the daemon channel full and its writer parked"
        );
        (peer, task)
    }

    /// Spawn a client against `node_id` over a link of `link_buffer` bytes.
    pub(super) async fn spawn_client_sized(
        server: &RelayServer,
        node_id: &str,
        link_buffer: usize,
    ) -> (
        WebSocketStream<tokio::io::DuplexStream>,
        tokio::task::JoinHandle<Result<()>>,
    ) {
        let (peer_io, relay_io) = tokio::io::duplex(link_buffer);
        let relay_ws = WebSocketStream::from_raw_socket(relay_io, Role::Server, None).await;
        let peer = WebSocketStream::from_raw_socket(peer_io, Role::Client, None).await;
        let inner = server.inner.clone();
        let permit = inner
            .handshake_permits
            .clone()
            .try_acquire_owned()
            .expect("a free handshake permit");
        let node = node_id.to_string();
        let task = tokio::spawn(async move {
            handle_client(inner, relay_ws, node, ClientRoute::Wss, permit).await
        });
        (peer, task)
    }

    /// Spawn a client against `node_id` over a link big enough not to be the
    /// bottleneck; the test cares about the DAEMON side stalling.
    async fn spawn_client(
        server: &RelayServer,
        node_id: &str,
    ) -> (
        WebSocketStream<tokio::io::DuplexStream>,
        tokio::task::JoinHandle<Result<()>>,
    ) {
        let (peer_io, relay_io) = tokio::io::duplex(64 * 1024);
        let relay_ws = WebSocketStream::from_raw_socket(relay_io, Role::Server, None).await;
        let peer = WebSocketStream::from_raw_socket(peer_io, Role::Client, None).await;
        let inner = server.inner.clone();
        let permit = inner
            .handshake_permits
            .clone()
            .try_acquire_owned()
            .expect("a free handshake permit");
        let node = node_id.to_string();
        let task = tokio::spawn(async move {
            handle_client(inner, relay_ws, node, ClientRoute::Wss, permit).await
        });
        (peer, task)
    }

    /// Paused clock: the wedge budgets elapse in no wall time.
    #[tokio::test(start_paused = true)]
    async fn a_daemon_that_stops_reading_cannot_wedge_the_relay() {
        let server = RelayServer::new(RelayConfig {
            idle_timeout: Duration::from_secs(2),
            max_pending_handshakes: 16,
            ..RelayConfig::default()
        });

        // Two wedged daemons: one is superseded below, the other is left alone
        // so its own writer budget has to reclaim it.
        let (superseded_peer, superseded_task) = wedge_a_daemon(&server, "wedged-superseded").await;
        let superseded_key = superseded_peer.pkcs8.clone();
        let (abandoned_peer, abandoned_task) = wedge_a_daemon(&server, "wedged-abandoned").await;

        // A healthy daemon on ANOTHER node, to prove the wedge does not spread.
        let (mut healthy, healthy_task) = start_registration(&server, "healthy", 64 * 1024).await;
        let nonce = healthy.challenge().await;
        healthy.register(&nonce).await;
        match next_control(&mut healthy.ws).await {
            Some(Control::Registered { .. }) => {}
            other => panic!("the healthy daemon must register, got {other:?}"),
        }

        // 1. A client asking for a wedged node is REFUSED within the handoff
        //    budget instead of parking forever on the full channel - and gets
        //    its pre-admission permit back.
        let free_before = server.inner.handshake_permits.available_permits();
        let (mut refused_peer, refused_task) = spawn_client(&server, "wedged-abandoned").await;
        let asked_at = tokio::time::Instant::now();
        let refused = tokio::time::timeout(PAIR_TIMEOUT * 4, refused_task).await;
        assert!(
            refused.is_ok(),
            "a client must not park on a wedged daemon's full channel"
        );
        // Bounded by the HANDOFF budget specifically: the client never got as
        // far as waiting to be paired, so this must land well inside
        // PAIR_TIMEOUT.
        assert!(
            asked_at.elapsed() < PAIR_TIMEOUT,
            "the client waited {:?} - that is the pairing timeout, not the handoff budget",
            asked_at.elapsed()
        );
        match next_control(&mut refused_peer).await {
            Some(Control::Error { code, .. }) => assert_eq!(code, "no_such_node"),
            other => panic!("expected the daemon-gone refusal, got {other:?}"),
        }
        assert_eq!(
            server.inner.handshake_permits.available_permits(),
            free_before,
            "a client refused by a wedged daemon must return its permit"
        );

        // 2. SIBLING TRAFFIC: a client on the healthy node still pairs while two
        //    other nodes are wedged.
        let (mut sibling_peer, sibling_task) = spawn_client(&server, "healthy").await;
        let conn_id = loop {
            match next_control(&mut healthy.ws).await {
                Some(Control::Open { conn_id, .. }) => break conn_id,
                Some(_) => {}
                None => panic!("the healthy daemon never saw the sibling's Open"),
            }
        };
        healthy
            .ws
            .send(Message::text(Control::Opened { conn_id }.to_json()))
            .await
            .expect("accept the sibling pairing");
        match next_control(&mut sibling_peer).await {
            Some(Control::Opened { conn_id: c }) => assert_eq!(c, conn_id),
            other => panic!(
                "a client on a healthy node must be served while others are wedged, got {other:?}"
            ),
        }

        // 3. SUPERSEDE: the wedged daemon's reader is not parked, so a same-key
        //    re-registration still reclaims the old link.
        let (mut replacement, replacement_task) = start_registration_keyed(
            &server,
            "wedged-superseded",
            64 * 1024,
            None,
            Some(superseded_key),
        )
        .await;
        let nonce = replacement.challenge().await;
        replacement.register(&nonce).await;
        match next_control(&mut replacement.ws).await {
            Some(Control::Registered { .. }) => {}
            other => {
                panic!("a same-key re-registration must supersede a wedged link, got {other:?}")
            }
        }
        assert!(
            tokio::time::timeout(DAEMON_WRITE_STALL * 2, superseded_task)
                .await
                .is_ok(),
            "the superseded wedged link must be reclaimed, not left parked in its writer"
        );

        // 4. The abandoned wedge reclaims ITSELF: nobody supersedes it and its
        //    daemon never speaks again, so only the writer's own stall budget
        //    can declare the link dead and release the node-id. Unbounded, that
        //    registration - and its node-id - would be held forever.
        assert!(
            tokio::time::timeout(DAEMON_WRITE_STALL * 2, abandoned_task)
                .await
                .is_ok(),
            "a wedged daemon nobody supersedes must be reclaimed by its own write budget"
        );
        assert!(
            !server
                .inner
                .daemons
                .lock()
                .await
                .contains_key("wedged-abandoned"),
            "the wedged node-id must be released back to the registry"
        );

        drop((sibling_peer, sibling_task, refused_peer));
        drop((superseded_peer, abandoned_peer, replacement));
        healthy_task.abort();
        replacement_task.abort();
    }
}

/// A graceful daemon close must not outrun the connection's own last bytes.
/// The daemon's final DATA frames sit AHEAD of its `Close` on one FIFO, so
/// retiring the route by dropping it - and firing the route cancellation -
/// jumped that queue and cut the pump off mid-drain. Any relayed client whose
/// peer closes right after writing lost the tail of its response.
#[cfg(test)]
mod graceful_close_tests {
    use super::registration_tests::start_registration;
    use super::wedged_daemon_tests::spawn_client_sized;
    use super::*;

    /// Read the next real message from a client link, answering pings.
    async fn next_message(ws: &mut WebSocketStream<tokio::io::DuplexStream>) -> Option<Message> {
        while let Some(msg) = ws.next().await {
            match msg {
                Ok(Message::Ping(p)) => {
                    let _ = ws.send(Message::Pong(p)).await;
                }
                Ok(Message::Pong(_)) => {}
                Ok(m) => return Some(m),
                Err(_) => return None,
            }
        }
        None
    }

    /// Register a daemon and pair one client to it. Returns both ends and the
    /// paired conn_id.
    async fn pair(
        server: &RelayServer,
        node_id: &str,
        client_buffer: usize,
    ) -> (
        super::registration_tests::PeerLink,
        WebSocketStream<tokio::io::DuplexStream>,
        u64,
        tokio::task::JoinHandle<Result<()>>,
        tokio::task::JoinHandle<Result<()>>,
    ) {
        let (mut daemon, daemon_task) = start_registration(server, node_id, 64 * 1024).await;
        let nonce = daemon.challenge().await;
        daemon.register(&nonce).await;
        match next_control(&mut daemon.ws).await {
            Some(Control::Registered { .. }) => {}
            other => panic!("the daemon must register, got {other:?}"),
        }

        let (mut client, client_task) = spawn_client_sized(server, node_id, client_buffer).await;
        let conn_id = loop {
            match next_control(&mut daemon.ws).await {
                Some(Control::Open { conn_id, .. }) => break conn_id,
                Some(_) => {}
                None => panic!("the daemon never saw the Open"),
            }
        };
        daemon
            .ws
            .send(Message::text(Control::Opened { conn_id }.to_json()))
            .await
            .expect("accept the pairing");
        match next_control(&mut client).await {
            Some(Control::Opened { conn_id: c }) => assert_eq!(c, conn_id),
            other => panic!("the client never saw Opened: {other:?}"),
        }
        (daemon, client, conn_id, daemon_task, client_task)
    }

    /// The ordering guarantee: bytes written just before a close still arrive,
    /// and arrive BEFORE the close.
    #[tokio::test(start_paused = true)]
    async fn a_graceful_close_delivers_the_tail_before_the_close() {
        let server = RelayServer::new(RelayConfig {
            idle_timeout: Duration::from_secs(2),
            ..RelayConfig::default()
        });
        let (mut daemon, mut client, conn_id, daemon_task, client_task) =
            pair(&server, "node-tail", 64 * 1024).await;

        // The daemon writes its last bytes and closes immediately after, with
        // no chance for the client to have drained in between.
        daemon
            .ws
            .send(Message::binary(encode_data(conn_id, b"tail-bytes")))
            .await
            .expect("send the tail");
        daemon
            .ws
            .send(Message::text(
                Control::Close {
                    conn_id,
                    reason: "done".into(),
                }
                .to_json(),
            ))
            .await
            .expect("close the conn");

        match next_message(&mut client).await {
            Some(Message::Binary(b)) => {
                let (c, payload) = decode_data(&b).expect("a DATA frame");
                assert_eq!(c, conn_id);
                assert_eq!(
                    payload, b"tail-bytes",
                    "the tail written before the close must not be lost"
                );
            }
            other => panic!("expected the tail DATA frame first, got {other:?}"),
        }
        match next_message(&mut client).await {
            Some(Message::Text(t)) => assert!(
                matches!(Control::from_json(&t), Ok(Control::Close { .. })),
                "expected the close after the tail, got {t}"
            ),
            other => panic!("expected a Close after the tail, got {other:?}"),
        }

        assert!(
            tokio::time::timeout(Duration::from_secs(10), client_task)
                .await
                .is_ok(),
            "the client task must retire once the graceful close is drained"
        );
        drop((daemon, client));
        daemon_task.abort();
    }

    /// The graceful path must not become a new wedge: a client that stops
    /// reading during the drain is still reclaimed by the idle budget, exactly
    /// as an abrupt teardown would be.
    #[tokio::test(start_paused = true)]
    async fn a_graceful_close_to_a_non_reading_client_is_still_bounded() {
        let idle = Duration::from_secs(2);
        let server = RelayServer::new(RelayConfig {
            idle_timeout: idle,
            ..RelayConfig::default()
        });
        // A link that fits the small `Opened` frame but not the DATA that
        // follows; the client never reads again after pairing.
        let (mut daemon, client, conn_id, daemon_task, client_task) =
            pair(&server, "node-stuck-tail", 96).await;

        let started = tokio::time::Instant::now();
        daemon
            .ws
            .send(Message::binary(encode_data(conn_id, &[0u8; 4096])))
            .await
            .expect("send the tail");
        daemon
            .ws
            .send(Message::text(
                Control::Close {
                    conn_id,
                    reason: "done".into(),
                }
                .to_json(),
            ))
            .await
            .expect("close the conn");

        assert!(
            tokio::time::timeout(idle * 8, client_task).await.is_ok(),
            "a graceful close must not let a non-reading client park the relay"
        );
        assert!(
            started.elapsed() < idle * 2,
            "reclaimed after {:?}, past the {idle:?} liveness budget",
            started.elapsed()
        );
        drop((daemon, client));
        daemon_task.abort();
    }
}
