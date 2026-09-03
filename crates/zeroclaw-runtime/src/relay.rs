//! Daemon-side relay bridge (runtime-owned).
//!
//! Holds one persistent **outer TLS + WebSocket** control connection to a
//! nominated relay, proves the daemon's Ed25519 registration identity over a
//! signed challenge, and claims a `node_id`. The relay then multiplexes client
//! connections to the daemon over that single link by `conn_id`: on each `Open`
//! the bridge dials the daemon's own loopback WSS listener and shuttles binary
//! `DATA` both ways. Those `DATA` payloads are the inner client<->daemon mTLS,
//! which terminates at the loopback listener exactly as on the direct path; the
//! bridge and the relay only move ciphertext.
//!
//! WS keepalive pings (below NAT idle windows) detect a half-open link and force
//! a reconnect; reconnects use capped exponential backoff. Cancellation stops the
//! bridge promptly.

use anyhow::{Context, Result};
use base64::Engine as _;
use base64::engine::general_purpose::STANDARD as B64;
use futures_util::{SinkExt, StreamExt};
use ring::signature::{Ed25519KeyPair, KeyPair};
use std::collections::HashMap;
use std::sync::Arc;
use std::time::Duration;
use tokio::io::{AsyncReadExt, AsyncWriteExt};
use tokio::net::TcpStream;
use tokio::sync::{Mutex, mpsc, oneshot};
// Every deadline in this module is a runtime deadline, so the runtime clock is
// the one that must measure it (and the one a paused-clock test drives).
use tokio::time::Instant;
use tokio_rustls::TlsConnector;
// The runtime depends on tokio-rustls (not rustls directly); use its re-export.
use tokio_rustls::rustls;
use tokio_util::sync::CancellationToken;
use zeroclaw_relay_proto::{
    ConnWindow, Control, INITIAL_WINDOW, MAX_CONTROL_FRAME, MAX_DATA_PAYLOAD, PEER_HINT_ENROLL,
    SUBPROTOCOL, TokenBucket, decode_data, encode_data,
};

/// What the demux loop routes to a per-conn `bridge_conn` task: inbound inner
/// bytes, plus the credit-window control frames (forwarded by the relay) that
/// govern how fast this conn may send.
enum ConnMsg {
    /// Inbound DATA payload to write to the loopback inner stream.
    Data(Vec<u8>),
    /// `Window { credit }`: (re)establish this conn's absolute send window.
    Window(u32),
    /// `DataAck { consumed }`: replenish this conn's send window.
    Ack(u32),
}

/// The link-side handle for one bridged connection: the inbound queue its
/// bridge task reads, and the token that retires that task.
///
/// Cancellation is what lets a route close reach a bridge task that is NOT
/// sitting in its select - one parked writing to the loopback socket, or parked
/// sending into the shared relay queue. Dropping the handle cancels, so every
/// removal from the conn map retires the task with no separate bookkeeping: a
/// relay `Close`, a backpressured conn, or the map itself going away.
struct ConnHandle {
    tx: mpsc::Sender<ConnMsg>,
    cancel: CancellationToken,
}

impl Drop for ConnHandle {
    fn drop(&mut self) {
        self.cancel.cancel();
    }
}

/// Live bridged connections for one relay link, keyed by `conn_id`.
type ConnMap = Arc<Mutex<HashMap<u64, ConnHandle>>>;

const BACKOFF_INITIAL: Duration = Duration::from_millis(500);
const BACKOFF_MAX: Duration = Duration::from_secs(30);
/// A session up at least this long resets the backoff (transient drop).
const ESTABLISHED: Duration = Duration::from_secs(5);
/// WS keepalive cadence (below common NAT idle windows).
const KEEPALIVE: Duration = Duration::from_secs(20);
/// Declare the link dead if nothing has been heard for this long.
const DEAD_AFTER: Duration = Duration::from_secs(60);
/// ONE absolute budget for the entire outbound setup: TCP connect, outer TLS,
/// the WebSocket upgrade, and the signed Hello/Challenge/Register/Registered
/// exchange. A fresh timeout per phase would let an unresponsive relay spend the
/// whole budget in EACH phase, making the effective wait a multiple of it.
/// Sized above the relay's own 10s default handshake budget, so a healthy relay
/// always ends the exchange first, and never above [`ROTATION_READY_TIMEOUT`],
/// so a candidate rotation link cannot still be setting up after the window in
/// which it may still replace the published route.
const SETUP_DEADLINE: Duration = Duration::from_secs(30);
/// Absolute budget for ONE per-`Open` dial of a local loopback listener.
///
/// The target is a listener on this same host, so the only thing this connect
/// waits on is that listener's accept backlog; a healthy dial completes in
/// microseconds. A saturated or wedged local listener otherwise leaves the
/// connect pending in the kernel with nothing able to cancel it, holding the
/// bridge task and its registered source port. Kept well inside the relay's own
/// 15s per-`Open` pair timeout, so the bridge always abandons the dial before
/// the relay abandons the route it is holding open for it.
const LOCAL_DIAL_DEADLINE: Duration = Duration::from_secs(10);
/// Bound on a loopback write that makes NO progress at all.
///
/// A local consumer that is merely slow is legitimate backpressure - the client
/// is streaming faster than the daemon drains it - and killing that would break
/// large requests, so this budget is reset on every byte that moves. What it
/// ends is a consumer that has stopped reading entirely. It is also the budget
/// the daemon's own WSS listener applies to a peer that stops reading, so a
/// loopback consumer the daemon still considers alive is never retired here.
const LOCAL_WRITE_STALL: Duration = Duration::from_secs(30);
/// Bound on one outbound frame write once the link is established. The
/// tungstenite sink reports no partial progress, so from here a relay that has
/// stopped reading is indistinguishable from one that is merely slow; the bound
/// is therefore the same silence budget the keepalive watchdog applies, which no
/// healthy link reaches for a single frame of at most `MAX_WS_MESSAGE`.
const WRITE_STALL: Duration = DEAD_AFTER;
/// During a node-id rotation, keep the OLD id's link alive this long after the
/// NEW id registers, so clients mid-session on the old id are not cut off.
const ROTATION_GRACE: Duration = Duration::from_secs(600);
/// A candidate rotation link must register promptly before it may replace the
/// currently published route. The existing route remains live on timeout.
const ROTATION_READY_TIMEOUT: Duration = Duration::from_secs(30);
/// How often the supervisor polls for an on-demand rotation trigger / checks the
/// scheduled-rotation deadline.
const ROTATE_POLL: Duration = Duration::from_secs(15);

/// Everything the bridge needs to register with, and verify, a relay.
#[derive(Clone)]
pub struct RelayBridgeConfig {
    /// Relay `host:port` to dial.
    pub relay_addr: String,
    /// Server name presented for the relay's outer TLS cert (its SAN).
    pub relay_host: String,
    /// Opaque node-id this daemon claims (clients dial it).
    pub node_id: String,
    /// Optional shared-secret admission gate.
    pub relay_token: Option<String>,
    /// Loopback address of the daemon's own WSS listener (e.g. `127.0.0.1:9781`).
    pub local_wss_addr: String,
    /// Optional loopback address of the daemon's narrow enrollment listener.
    pub local_enroll_addr: Option<String>,
    /// Shared with the enrollment endpoint: the bridge registers each outbound
    /// enroll-dial source port here BEFORE connecting, so the endpoint can
    /// classify those loopback connections as relay-routed (shared-identity
    /// peers) rather than direct clients. See `enroll::BridgePortSet`.
    pub enroll_bridge_ports: Option<crate::enroll::BridgePortSet>,
    /// PKCS#8 of the daemon's Ed25519 registration key.
    pub signing_key_pkcs8: Vec<u8>,
    /// PEM CA to trust for the relay's outer cert; `None` uses public roots.
    pub relay_ca_path: Option<String>,
    /// Skip relay outer-cert verification (test only).
    pub relay_insecure: bool,
    /// Opt-in trust-on-first-use for the relay's OUTER leaf cert (A2): accept the
    /// first leaf and record its pin to `<data_dir>/relay/relay_pin`, pinning it
    /// thereafter. Never silently enabled; ignored when a relay CA is configured.
    pub relay_tofu: bool,
    /// Outer-mTLS variant: PEM cert/key the daemon presents to the relay on the
    /// OUTER layer (needed when the relay sets `outer_client_auth = required`).
    /// `None` presents no outer client cert. Separate from the inner mTLS.
    pub outer_client_cert: Option<String>,
    pub outer_client_key: Option<String>,
    /// Cap on simultaneously-bridged client connections (bridge-side DoS cap).
    pub max_conns: usize,
    /// Per-node `OPEN` handshake-rate cap (A6): burst allowance + steady refill
    /// per second. A flood of `OPEN`s beyond this is fast-rejected with `Close`
    /// BEFORE a loopback mTLS handshake is spun up, so the relay's caps are not
    /// the only line of defense.
    pub open_burst: u32,
    pub open_rate_per_sec: f64,
    /// Daemon data dir; the node-id + rotation-trigger files live under `relay/`.
    pub data_dir: std::path::PathBuf,
    /// Auto-rotate the node-id every N days (0 = never). Only meaningful when the
    /// id is auto-minted (`rotation_allowed`).
    pub node_id_rotation_days: u64,
    /// Whether node-id rotation is permitted: true only when the operator did not
    /// pin `[relay].node_id` (a pinned id is fixed). Gates both scheduled and
    /// on-demand rotation.
    pub rotation_allowed: bool,
}

/// Load (or create + persist) the daemon's Ed25519 relay-registration key.
///
/// Stored as raw PKCS#8 DER at `<data_dir>/relay/registration.key` (dir 0700,
/// key 0600). This is the daemon's stable rendezvous identity, separate from the
/// ZeroClaw CA: the relay binds the node-id to this key and an allowlist keys on
/// its fingerprint.
pub fn ensure_signing_key(data_dir: &std::path::Path) -> Result<Vec<u8>> {
    use std::io::Write;

    let dir = data_dir.join("relay");
    let path = dir.join("registration.key");
    if let Ok(bytes) = std::fs::read(&path)
        && !bytes.is_empty()
    {
        return Ok(bytes);
    }
    std::fs::create_dir_all(&dir).with_context(|| format!("creating {}", dir.display()))?;
    set_private_dir_permissions(&dir);
    let rng = ring::rand::SystemRandom::new();
    let pkcs8 = Ed25519KeyPair::generate_pkcs8(&rng)
        .map_err(|e| anyhow::Error::msg(format!("generating relay signing key: {e}")))?;
    let mut f = private_key_create_options()
        .open(&path)
        .with_context(|| format!("writing {}", path.display()))?;
    f.write_all(pkcs8.as_ref())
        .context("writing relay signing key")?;
    Ok(pkcs8.as_ref().to_vec())
}

#[cfg(unix)]
fn set_private_dir_permissions(dir: &std::path::Path) {
    use std::os::unix::fs::PermissionsExt;

    let _ = std::fs::set_permissions(dir, std::fs::Permissions::from_mode(0o700));
}

#[cfg(not(unix))]
fn set_private_dir_permissions(_dir: &std::path::Path) {}

fn private_key_create_options() -> std::fs::OpenOptions {
    let mut options = std::fs::OpenOptions::new();
    options.write(true).create(true).truncate(true);
    private_key_permissions(&mut options);
    options
}

#[cfg(unix)]
fn private_key_permissions(options: &mut std::fs::OpenOptions) {
    use std::os::unix::fs::OpenOptionsExt;

    options.mode(0o600);
}

#[cfg(not(unix))]
fn private_key_permissions(_options: &mut std::fs::OpenOptions) {}

/// Resolve this daemon's relay node-id.
///
/// If the operator set `[relay].node_id` (`configured`), that wins. Otherwise read
/// (or mint + persist) a random 128-bit value at `<data_dir>/relay/node_id`.
///
/// The node-id is an UNGUESSABLE routing CAPABILITY, not a name (design relay/02):
/// high entropy + non-enumerability stops attackers probing which daemons are
/// online or flooding a daemon's inner mTLS by guessing ids (A6/A10). It is kept
/// DECOUPLED from the cert/identity so the relay (a metadata adversary) only ever
/// learns an opaque handle, and so it can be rotated without reissuing certs.
pub fn ensure_node_id(data_dir: &std::path::Path, configured: &str) -> Result<String> {
    let configured = configured.trim();
    if !configured.is_empty() {
        return Ok(configured.to_string());
    }
    let path = data_dir.join("relay").join("node_id");
    if let Ok(existing) = std::fs::read_to_string(&path) {
        let existing = existing.trim().to_string();
        if !existing.is_empty() {
            return Ok(existing);
        }
    }
    let id = mint_node_id()?;
    persist_node_id(data_dir, &id)?;
    Ok(id)
}

/// Mint a fresh, unguessable 128-bit node-id (hex). Decoupled from the cert so a
/// relay compromise leaks only a routing handle, and rotatable without reissuing
/// certs.
pub fn mint_node_id() -> Result<String> {
    use ring::rand::SecureRandom;
    let mut bytes = [0u8; 16];
    ring::rand::SystemRandom::new()
        .fill(&mut bytes)
        .map_err(|e| anyhow::Error::msg(format!("generating node_id: {e}")))?;
    Ok(hex::encode(bytes))
}

/// Atomically persist the effective node-id to `<data_dir>/relay/node_id` (temp +
/// rename), so a concurrent reader (`ensure_node_id` / `relay_profile`) never sees
/// a half-written value. This is what makes a rotated id flow to clients in-band
/// on their next renewal.
pub fn persist_node_id(data_dir: &std::path::Path, id: &str) -> Result<()> {
    let dir = data_dir.join("relay");
    std::fs::create_dir_all(&dir).with_context(|| format!("creating {}", dir.display()))?;
    #[cfg(unix)]
    {
        use std::os::unix::fs::PermissionsExt;
        let _ = std::fs::set_permissions(&dir, std::fs::Permissions::from_mode(0o700));
    }
    let tmp = dir.join("node_id.tmp");
    std::fs::write(&tmp, id).with_context(|| format!("writing {}", tmp.display()))?;
    std::fs::rename(&tmp, dir.join("node_id")).context("atomically replacing node_id")?;
    Ok(())
}

/// The on-demand rotation trigger file. `zeroclaw security relay-rotate-node-id`
/// touches it; the running bridge polls for it and rotates when it appears.
pub fn rotate_trigger_path(data_dir: &std::path::Path) -> std::path::PathBuf {
    data_dir.join("relay").join("rotate-now")
}

/// The relay outer-leaf pin store (`<data_dir>/relay/relay_pin`). Once recorded
/// (explicitly or by TOFU) the bridge pins the relay's outer cert, AND enrollment
/// delivers this value to clients so they pin the same leaf (R-E contract).
pub fn relay_pin_path(data_dir: &std::path::Path) -> std::path::PathBuf {
    data_dir.join("relay").join("relay_pin")
}

/// Persist the relay outer-leaf pin (sha256 hex) atomically at `0600`.
pub fn persist_relay_pin(data_dir: &std::path::Path, pin: &str) -> Result<()> {
    let dir = data_dir.join("relay");
    std::fs::create_dir_all(&dir).with_context(|| format!("creating {}", dir.display()))?;
    let tmp = dir.join("relay_pin.tmp");
    {
        use std::io::Write as _;
        #[cfg(unix)]
        use std::os::unix::fs::OpenOptionsExt as _;
        let mut opts = std::fs::OpenOptions::new();
        opts.write(true).create(true).truncate(true);
        #[cfg(unix)]
        opts.mode(0o600);
        let mut f = opts
            .open(&tmp)
            .with_context(|| format!("writing {}", tmp.display()))?;
        f.write_all(pin.as_bytes())
            .with_context(|| format!("writing {}", tmp.display()))?;
    }
    std::fs::rename(&tmp, dir.join("relay_pin")).context("atomically replacing relay_pin")?;
    Ok(())
}

/// Request an on-demand node-id rotation by touching the trigger file. The running
/// daemon's bridge picks it up within its poll interval (auto-mint mode only).
pub fn request_node_id_rotation(data_dir: &std::path::Path) -> Result<()> {
    let dir = data_dir.join("relay");
    std::fs::create_dir_all(&dir).with_context(|| format!("creating {}", dir.display()))?;
    let path = rotate_trigger_path(data_dir);
    std::fs::write(&path, b"rotate\n").with_context(|| format!("writing {}", path.display()))?;
    Ok(())
}

/// Run the relay bridge until `cancel` fires.
///
/// When node-id rotation is permitted (auto-mint mode) this is a supervisor: it
/// keeps one live link and, on a scheduled cadence or an on-demand trigger, mints
/// a fresh id, registers it ALONGSIDE the old one for a grace window (the relay
/// binds both ids to the same pubkey, so A10 is preserved and clients mid-session
/// on the old id keep working), waits for the new link to register, persists the
/// new id atomically (so it reaches clients in-band on their next renewal), then
/// retires the old link. With
/// rotation off (or an operator-pinned id) it is just a single link.
pub async fn run_relay_bridge(cfg: RelayBridgeConfig, cancel: CancellationToken) -> Result<()> {
    if !cfg.rotation_allowed {
        return serve_link(cfg, cancel, None).await;
    }

    let mut current_id = cfg.node_id.clone();
    let mut link_cancel = cancel.child_token();
    let mut link = {
        let c = cfg.clone();
        let lc = link_cancel.clone();
        zeroclaw_spawn::spawn!(async move { serve_link(c, lc, None).await })
    };

    loop {
        let Some(new_id) = wait_for_rotation(&cfg, &cancel).await else {
            // Cancelled: retire the live link and exit.
            link_cancel.cancel();
            let _ = link.await;
            return Ok(());
        };

        // Bring the new id up alongside the old (grace-window overlap).
        let new_cancel = cancel.child_token();
        let (registered_tx, registered_rx) = oneshot::channel();
        let new_link = {
            let mut c = cfg.clone();
            c.node_id = new_id.clone();
            let lc = new_cancel.clone();
            zeroclaw_spawn::spawn!(async move { serve_link(c, lc, Some(registered_tx)).await })
        };

        match wait_for_new_link_registration(registered_rx, &cancel).await {
            RotationRegistration::Registered => {}
            RotationRegistration::Cancelled => {
                new_cancel.cancel();
                let _ = new_link.await;
                link_cancel.cancel();
                let _ = link.await;
                return Ok(());
            }
            RotationRegistration::Unavailable => {
                new_cancel.cancel();
                let _ = new_link.await;
                ::zeroclaw_log::record!(
                    WARN,
                    ::zeroclaw_log::Event::new(module_path!(), ::zeroclaw_log::Action::Fail)
                        .with_outcome(::zeroclaw_log::EventOutcome::Failure)
                        .with_attrs(::serde_json::json!({ "old": current_id, "new": new_id })),
                    "relay node-id rotation: new link never registered; keeping the old route"
                );
                continue;
            }
        }

        // Publish only a registered route. If persistence fails, abandon the new
        // link before the grace window so clients retain the known-good old id.
        if let Err(e) = persist_node_id(&cfg.data_dir, &new_id) {
            new_cancel.cancel();
            let _ = new_link.await;
            ::zeroclaw_log::record!(
                WARN,
                ::zeroclaw_log::Event::new(module_path!(), ::zeroclaw_log::Action::Fail)
                    .with_outcome(::zeroclaw_log::EventOutcome::Failure)
                    .with_attrs(::serde_json::json!({
                        "old": current_id,
                        "new": new_id,
                        "error": format!("{e:#}"),
                    })),
                "relay node-id rotation: failed to persist the new id; keeping the old route"
            );
            continue;
        }
        ::zeroclaw_log::record!(
            INFO,
            ::zeroclaw_log::Event::new(module_path!(), ::zeroclaw_log::Action::Note)
                .with_attrs(::serde_json::json!({ "old": current_id, "new": new_id })),
            "relay node-id rotating (old id kept alive for the grace window)"
        );

        // Hold the overlap for the grace window (or until cancellation).
        tokio::select! {
            _ = tokio::time::sleep(ROTATION_GRACE) => {}
            _ = cancel.cancelled() => {}
        }

        // Retire the old link; promote the new one.
        link_cancel.cancel();
        let _ = link.await;
        current_id = new_id;
        link = new_link;
        link_cancel = new_cancel;

        if cancel.is_cancelled() {
            link_cancel.cancel();
            let _ = link.await;
            return Ok(());
        }
    }
}

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
enum RotationRegistration {
    Registered,
    Cancelled,
    Unavailable,
}

/// Wait until a candidate link receives the relay's authenticated `Registered`
/// reply. The old route must remain authoritative until then.
async fn wait_for_new_link_registration(
    registered: oneshot::Receiver<()>,
    cancel: &CancellationToken,
) -> RotationRegistration {
    tokio::select! {
        _ = cancel.cancelled() => RotationRegistration::Cancelled,
        result = tokio::time::timeout(ROTATION_READY_TIMEOUT, registered) => match result {
            Ok(Ok(())) => RotationRegistration::Registered,
            Ok(Err(_)) | Err(_) => RotationRegistration::Unavailable,
        },
    }
}

/// Wait for the next rotation, returning the freshly minted id; `None` on cancel.
/// Fires on an on-demand trigger file or, when `node_id_rotation_days > 0`, on the
/// scheduled cadence.
async fn wait_for_rotation(cfg: &RelayBridgeConfig, cancel: &CancellationToken) -> Option<String> {
    let trigger = rotate_trigger_path(&cfg.data_dir);
    let scheduled_deadline = (cfg.node_id_rotation_days > 0).then(|| {
        Instant::now() + Duration::from_secs(cfg.node_id_rotation_days.saturating_mul(86_400))
    });
    let mut poll = tokio::time::interval(ROTATE_POLL);
    loop {
        tokio::select! {
            _ = cancel.cancelled() => return None,
            _ = poll.tick() => {
                if trigger.exists() {
                    let _ = std::fs::remove_file(&trigger);
                    return mint_node_id().ok();
                }
                if let Some(dl) = scheduled_deadline
                    && Instant::now() >= dl
                {
                    return mint_node_id().ok();
                }
            }
        }
    }
}

/// One relay link for `cfg.node_id`: reconnect with capped backoff until `cancel`.
async fn serve_link(
    cfg: RelayBridgeConfig,
    cancel: CancellationToken,
    mut registered: Option<oneshot::Sender<()>>,
) -> Result<()> {
    let mut backoff = BACKOFF_INITIAL;
    loop {
        if cancel.is_cancelled() {
            return Ok(());
        }
        let started = Instant::now();
        match serve_once(&cfg, &cancel, &mut registered).await {
            Ok(()) => return Ok(()), // clean cancellation
            Err(e) => {
                ::zeroclaw_log::record!(
                    WARN,
                    ::zeroclaw_log::Event::new(module_path!(), ::zeroclaw_log::Action::Fail)
                        .with_outcome(::zeroclaw_log::EventOutcome::Failure)
                        .with_attrs(::serde_json::json!({
                            "relay": cfg.relay_addr,
                            "node_id": cfg.node_id,
                            "error": format!("{e:#}"),
                        })),
                    "relay bridge connection lost; will retry"
                );
            }
        }
        if cancel.is_cancelled() {
            return Ok(());
        }
        if started.elapsed() >= ESTABLISHED {
            backoff = BACKOFF_INITIAL;
        }
        tokio::select! {
            _ = cancel.cancelled() => return Ok(()),
            _ = tokio::time::sleep(backoff) => {}
        }
        backoff = backoff.saturating_mul(2).min(BACKOFF_MAX);
    }
}

async fn serve_once(
    cfg: &RelayBridgeConfig,
    cancel: &CancellationToken,
    registered: &mut Option<oneshot::Sender<()>>,
) -> Result<()> {
    let keypair = Ed25519KeyPair::from_pkcs8(&cfg.signing_key_pkcs8)
        .map_err(|e| anyhow::Error::msg(format!("loading relay signing key: {e}")))?;

    // Bounded, cancellable setup. One absolute deadline spans every await from
    // the TCP connect through the `Registered` reply, with the daemon
    // cancellation token biased ahead of it. A relay that accepts a socket and
    // then stops responding can therefore hold neither shutdown nor the
    // reconnect progression: cancellation returns immediately and the deadline
    // fails the attempt so the caller's backoff retries. Dropping the setup
    // future on either path tears down whatever half-built socket, TLS session,
    // or WebSocket it was holding.
    let deadline = Instant::now() + SETUP_DEADLINE;
    let setup = connect_and_register(cfg, &keypair, registered);
    let ws = tokio::select! {
        biased;
        _ = cancel.cancelled() => return Ok(()),
        outcome = tokio::time::timeout_at(deadline, setup) => outcome
            .map_err(|_| anyhow::Error::msg("relay setup exceeded its bounded budget"))??,
    };

    serve_established(cfg, cancel, ws).await
}

/// The entire outbound setup for one link: TCP connect, outer TLS, the relay
/// WebSocket upgrade, and the signed `Hello` -> `Challenge` -> `Register` ->
/// `Registered` exchange. Kept as ONE future so the caller can impose a single
/// absolute budget and cancellation over all of it rather than per phase.
async fn connect_and_register(
    cfg: &RelayBridgeConfig,
    keypair: &Ed25519KeyPair,
    registered: &mut Option<oneshot::Sender<()>>,
) -> Result<tokio_tungstenite::WebSocketStream<tokio_rustls::client::TlsStream<TcpStream>>> {
    let pubkey = keypair.public_key().as_ref().to_vec();

    // Outer TLS + WS to the relay. An operator-configured CA wins over remembered
    // TOFU state; otherwise a stored pin wins, or opt-in TOFU records the leaf for
    // next time (A2).
    let stored_pin = std::fs::read_to_string(relay_pin_path(&cfg.data_dir))
        .ok()
        .map(|s| s.trim().to_string())
        .filter(|s| !s.is_empty());
    let (tls_config, pin_verifier) = relay_client_config(
        cfg.relay_ca_path.as_deref(),
        cfg.relay_insecure,
        stored_pin.as_deref(),
        cfg.relay_tofu,
        cfg.outer_client_cert.as_deref(),
        cfg.outer_client_key.as_deref(),
    )?;
    let connector = TlsConnector::from(tls_config);
    let tcp = TcpStream::connect(&cfg.relay_addr)
        .await
        .with_context(|| format!("connecting to relay {}", cfg.relay_addr))?;
    let server_name = rustls::pki_types::ServerName::try_from(cfg.relay_host.clone())
        .map_err(|_| anyhow::Error::msg(format!("invalid relay host '{}'", cfg.relay_host)))?;
    let tls = connector
        .connect(server_name, tcp)
        .await
        .context("relay outer TLS handshake")?;
    // Persist a TOFU-observed pin so the next connection pins this leaf, and so
    // enrollment can deliver it to clients.
    if let Some(observed) = pin_verifier.as_ref().and_then(|v| v.observed_pin())
        && let Err(e) = persist_relay_pin(&cfg.data_dir, &observed)
    {
        ::zeroclaw_log::record!(
            WARN,
            ::zeroclaw_log::Event::new(module_path!(), ::zeroclaw_log::Action::Fail)
                .with_outcome(::zeroclaw_log::EventOutcome::Failure)
                .with_attrs(::serde_json::json!({ "error": format!("{e:#}") })),
            "relay outer-cert TOFU: failed to persist the observed pin"
        );
    }
    let uri: tokio_tungstenite::tungstenite::http::Uri = format!("wss://{}/", cfg.relay_host)
        .parse()
        .context("building relay ws uri")?;
    let request = tokio_tungstenite::tungstenite::ClientRequestBuilder::new(uri)
        .with_sub_protocol(SUBPROTOCOL);
    // Relay-protocol plane: bound the parser at the protocol budget so an
    // oversized message is refused while framing rather than buffered whole.
    let mut relay_ws_cfg = tokio_tungstenite::tungstenite::protocol::WebSocketConfig::default();
    relay_ws_cfg.max_message_size = Some(zeroclaw_relay_proto::MAX_WS_MESSAGE);
    relay_ws_cfg.max_frame_size = Some(zeroclaw_relay_proto::MAX_WS_MESSAGE);
    let (mut ws, _resp) =
        tokio_tungstenite::client_async_with_config(request, tls, Some(relay_ws_cfg))
            .await
            .context("relay websocket handshake")?;

    // Signed registration handshake: Hello -> Challenge -> Register -> Registered.
    ws.send(tungstenite_text(&Control::Hello {
        daemon_pubkey: B64.encode(&pubkey),
        node_id: cfg.node_id.clone(),
        relay_token: cfg.relay_token.clone(),
    }))
    .await?;
    let nonce = match next_control(&mut ws).await {
        Some(Control::Challenge { nonce }) => B64
            .decode(nonce.as_bytes())
            .context("relay challenge nonce not base64")?,
        Some(Control::Error { code, msg }) => {
            anyhow::bail!("relay refused registration: {code}: {msg}")
        }
        other => anyhow::bail!("unexpected relay reply to hello: {other:?}"),
    };
    let sig = keypair.sign(&nonce);
    ws.send(tungstenite_text(&Control::Register {
        node_id: cfg.node_id.clone(),
        sig: B64.encode(sig.as_ref()),
    }))
    .await?;
    match next_control(&mut ws).await {
        Some(Control::Registered { .. }) => {
            if let Some(ready) = registered.take() {
                let _ = ready.send(());
            }
        }
        Some(Control::Error { code, msg }) => {
            anyhow::bail!("relay rejected registration: {code}: {msg}")
        }
        other => anyhow::bail!("unexpected relay reply to register: {other:?}"),
    }
    Ok(ws)
}

/// Serve one registered link until it is cancelled or declared dead.
async fn serve_established(
    cfg: &RelayBridgeConfig,
    cancel: &CancellationToken,
    ws: tokio_tungstenite::WebSocketStream<tokio_rustls::client::TlsStream<TcpStream>>,
) -> Result<()> {
    // Connection bookkeeping + the single outbound write path to the relay.
    let (to_relay, from_tasks) = mpsc::channel::<tokio_tungstenite::tungstenite::Message>(256);
    let conns: ConnMap = Arc::new(Mutex::new(HashMap::new()));
    let last_seen = Arc::new(Mutex::new(Instant::now()));
    let link_dead = CancellationToken::new();

    let (sink, mut stream) = ws.split();
    let writer = {
        let link_dead = link_dead.clone();
        zeroclaw_spawn::spawn!(relay_writer(from_tasks, sink, link_dead))
    };

    // Keepalive watchdog: ping below NAT timeout, declare dead on silence.
    {
        let to_relay = to_relay.clone();
        let last_seen = last_seen.clone();
        let link_dead = link_dead.clone();
        zeroclaw_spawn::spawn!(keepalive_watchdog(to_relay, last_seen, link_dead));
    }

    // Per-node OPEN handshake-rate cap (A6). Single-threaded reader loop, so a
    // plain local bucket suffices (no lock). A flood of OPENs is fast-rejected
    // before any loopback mTLS handshake is dialed.
    let mut open_bucket = TokenBucket::new(cfg.open_burst, cfg.open_rate_per_sec);

    // Reader loop: react to Open/Close + demux DATA to per-conn loopback bridges.
    let result = loop {
        tokio::select! {
            _ = cancel.cancelled() => { break Ok(()); }
            _ = link_dead.cancelled() => { break Err(anyhow::Error::msg("relay link keepalive timed out")); }
            msg = stream.next() => {
                let Some(msg) = msg else { break Err(anyhow::Error::msg("relay closed the control link")); };
                *last_seen.lock().await = Instant::now();
                match msg {
                    Ok(tokio_tungstenite::tungstenite::Message::Text(t)) => {
                        match Control::from_json(t.as_str()) {
                            Ok(Control::Open { conn_id, peer_hint }) => {
                                // Fast-reject an OPEN flood before allocating conn
                                // state or dialing a loopback mTLS handshake (A6).
                                if !open_bucket.try_take() {
                                    // No conn state was allocated, so the refusal
                                    // is the only thing at stake: best-effort.
                                    notify_relay(
                                        &to_relay,
                                        tungstenite_text(&Control::Close {
                                            conn_id,
                                            reason: "rate_limited".into(),
                                        }),
                                    );
                                    continue;
                                }
                                let (local, is_enroll) = match open_route_target(
                                    cfg,
                                    peer_hint.as_deref(),
                                ) {
                                    Ok((addr, is_enroll)) => (addr.to_string(), is_enroll),
                                    Err(reason) => {
                                        // Refused before any conn state exists.
                                        notify_relay(
                                            &to_relay,
                                            tungstenite_text(&Control::Close {
                                                conn_id,
                                                reason: reason.into(),
                                            }),
                                        );
                                        continue;
                                    }
                                };
                                let mut cs = conns.lock().await;
                                if cs.len() >= cfg.max_conns {
                                    drop(cs);
                                    // Nothing was admitted; the cap already held.
                                    notify_relay(
                                        &to_relay,
                                        tungstenite_text(&Control::Close {
                                            conn_id,
                                            reason: "busy".into(),
                                        }),
                                    );
                                } else {
                                    let (tx, rx) = mpsc::channel::<ConnMsg>(256);
                                    // A child of the link token, so this conn is
                                    // retired both by its own route closing and
                                    // by the whole link dying, and the bridge
                                    // task only has to watch the one token.
                                    let conn_cancel = link_dead.child_token();
                                    cs.insert(
                                        conn_id,
                                        ConnHandle { tx, cancel: conn_cancel.clone() },
                                    );
                                    drop(cs);
                                    let to_relay = to_relay.clone();
                                    let conns = conns.clone();
                                    // For enroll-routed conns, share the port set
                                    // so the endpoint can classify the loopback
                                    // peer as relay-routed.
                                    let bridge_ports =
                                        is_enroll.then(|| cfg.enroll_bridge_ports.clone()).flatten();
                                    zeroclaw_spawn::spawn!(async move {
                                        bridge_conn(
                                            conn_id, &local, to_relay, rx, conn_cancel, conns,
                                            bridge_ports,
                                        )
                                        .await;
                                    });
                                }
                            }
                            // Dropping the handle cancels the conn, which reaches
                            // a bridge task parked in a loopback write or in a
                            // send to the shared relay queue - neither of which
                            // the task's own select can interrupt.
                            Ok(Control::Close { conn_id, .. }) => {
                                conns.lock().await.remove(&conn_id);
                            }
                            // Credit-window frames from the client (forwarded by
                            // the relay): route to the conn's bridge task.
                            Ok(Control::Window { conn_id, credit }) => {
                                deliver_conn_msg(&conns, &to_relay, conn_id, ConnMsg::Window(credit))
                                    .await;
                            }
                            Ok(Control::DataAck { conn_id, consumed }) => {
                                deliver_conn_msg(&conns, &to_relay, conn_id, ConnMsg::Ack(consumed))
                                    .await;
                            }
                            _ => {}
                        }
                    }
                    Ok(tokio_tungstenite::tungstenite::Message::Binary(b)) => {
                        if let Some((conn_id, payload)) = decode_data(&b) {
                            deliver_conn_msg(
                                &conns,
                                &to_relay,
                                conn_id,
                                ConnMsg::Data(payload.to_vec()),
                            )
                            .await;
                        }
                    }
                    Ok(tokio_tungstenite::tungstenite::Message::Ping(p)) => {
                        // A liveness courtesy. If the outbound queue is full the
                        // writer is already stalled, and the keepalive watchdog
                        // is what retires that link - parking the reader here to
                        // answer a ping would only delay noticing it.
                        notify_relay(&to_relay, tokio_tungstenite::tungstenite::Message::Pong(p));
                    }
                    Ok(tokio_tungstenite::tungstenite::Message::Pong(_)) => {}
                    Ok(tokio_tungstenite::tungstenite::Message::Close(_)) | Err(_) => {
                        break Err(anyhow::Error::msg("relay control link dropped"));
                    }
                    _ => {}
                }
            }
        }
    };

    link_dead.cancel();
    writer.abort();
    result
}

/// The single outbound write path to the relay, with the liveness bound that
/// stops a wedged link from becoming a permanent one.
///
/// `SinkExt::send` awaits the peer: a relay that accepts frames and then stops
/// reading parks this task once the socket buffer fills, the bounded `to_relay`
/// queue fills behind it, and every producer blocks with it - the reader loop's
/// pong and close writes and the keepalive watchdog alike - leaving nothing able
/// to declare the link dead. Each write is therefore bounded by [`WRITE_STALL`].
///
/// Exiting for any reason means the link is unusable, so this closes the
/// receiver before it returns: producers already parked on the queue fail
/// immediately instead of waiting on a sink nobody is draining, which is what
/// lets the reader loop reach its cancellation arm and tear the link down.
async fn relay_writer<S>(
    mut from_tasks: mpsc::Receiver<tokio_tungstenite::tungstenite::Message>,
    mut sink: S,
    link_dead: CancellationToken,
) where
    S: futures_util::Sink<tokio_tungstenite::tungstenite::Message> + Unpin,
{
    loop {
        let msg = tokio::select! {
            biased;
            _ = link_dead.cancelled() => break,
            msg = from_tasks.recv() => match msg {
                Some(msg) => msg,
                None => break,
            },
        };
        // A timeout here is a stalled peer, not a slow one: no healthy relay
        // takes the whole silence budget to accept a single bounded frame.
        if !matches!(
            tokio::time::timeout(WRITE_STALL, sink.send(msg)).await,
            Ok(Ok(()))
        ) {
            break;
        }
    }
    from_tasks.close();
    link_dead.cancel();
    let _ = tokio::time::timeout(WRITE_STALL, sink.close()).await;
}

/// Keepalive watchdog: ping below the NAT idle window, and declare the link dead
/// once nothing has been heard for [`DEAD_AFTER`].
///
/// The ping is enqueued with `try_send` deliberately. Awaiting a bounded queue
/// that a stalled writer has stopped draining parks the watchdog on its own
/// ping, so the deadline check below - the one check that has to keep running
/// while the outbound path is stuck - would never run again. A momentarily full
/// queue is not by itself fatal, since a healthy but busy link drains it;
/// silence past `DEAD_AFTER` is fatal whether or not the ping went out.
async fn keepalive_watchdog(
    to_relay: mpsc::Sender<tokio_tungstenite::tungstenite::Message>,
    last_seen: Arc<Mutex<Instant>>,
    link_dead: CancellationToken,
) {
    let mut tick = tokio::time::interval(KEEPALIVE);
    tick.tick().await; // immediate first tick; skip
    loop {
        tokio::select! {
            _ = link_dead.cancelled() => break,
            _ = tick.tick() => {
                let ping = tokio_tungstenite::tungstenite::Message::Ping(Vec::new().into());
                if matches!(
                    to_relay.try_send(ping),
                    Err(mpsc::error::TrySendError::Closed(_))
                ) {
                    link_dead.cancel();
                    break;
                }
                if last_seen.lock().await.elapsed() > DEAD_AFTER {
                    link_dead.cancel();
                    break;
                }
            }
        }
    }
}

fn open_route_target<'a>(
    cfg: &'a RelayBridgeConfig,
    peer_hint: Option<&str>,
) -> std::result::Result<(&'a str, bool), &'static str> {
    match peer_hint {
        Some(PEER_HINT_ENROLL) => cfg
            .local_enroll_addr
            .as_deref()
            .map(|a| (a, true))
            .ok_or("enroll_unavailable"),
        _ => Ok((&cfg.local_wss_addr, false)),
    }
}

/// Drop guard that removes a registered bridge source port from the shared set
/// when the bridged connection ends, however it ends.
struct PortGuard(crate::enroll::BridgePortSet, u16);
impl Drop for PortGuard {
    fn drop(&mut self) {
        self.0.lock().expect("bridge port set lock").remove(&self.1);
    }
}

/// Bind the outbound loopback source socket and, when `bridge_ports` is set,
/// register its OS-assigned ephemeral source port in the set BEFORE the caller
/// connects. Binding first lets us learn and publish the source port while
/// `connect()` has not yet run: the enrollment listener can only accept a
/// connection that `connect()` has already initiated, so registering before the
/// connect call guarantees the accept/classify step observes the port and
/// classifies the peer as relay-routed rather than `Direct`. Registering AFTER
/// `connect()` returned (the previous ordering) left a race window in which the
/// accept task could classify a relay-routed client as `Direct` (B2). Returns
/// the bound socket, the resolved remote to connect to, and the deregister guard
/// (held for the connection's lifetime).
fn bind_and_register(
    local_addr: &str,
    bridge_ports: Option<crate::enroll::BridgePortSet>,
) -> std::io::Result<(
    tokio::net::TcpSocket,
    std::net::SocketAddr,
    Option<PortGuard>,
)> {
    let remote: std::net::SocketAddr = local_addr
        .parse()
        .map_err(|e| std::io::Error::new(std::io::ErrorKind::InvalidInput, e))?;
    let (socket, bind_addr): (tokio::net::TcpSocket, std::net::SocketAddr) = match remote {
        std::net::SocketAddr::V4(_) => (
            tokio::net::TcpSocket::new_v4()?,
            (std::net::Ipv4Addr::LOCALHOST, 0).into(),
        ),
        std::net::SocketAddr::V6(_) => (
            tokio::net::TcpSocket::new_v6()?,
            (std::net::Ipv6Addr::LOCALHOST, 0).into(),
        ),
    };
    socket.bind(bind_addr)?;
    let guard = match bridge_ports {
        Some(ports) => {
            let port = socket.local_addr()?.port();
            ports.lock().expect("bridge port set lock").insert(port);
            Some(PortGuard(ports, port))
        }
        None => None,
    };
    Ok((socket, remote, guard))
}

/// Await `op` unless this conn is retired first. `None` means the route closed
/// (or the link died) while the operation was still parked.
///
/// Every await in a bridge task that can park on something outside the task -
/// the loopback socket, the shared relay queue - goes through here or through
/// [`write_local`]. An await that does not is one a route close cannot reach.
async fn unless_cancelled<F: std::future::Future>(
    cancel: &CancellationToken,
    op: F,
) -> Option<F::Output> {
    tokio::select! {
        biased;
        _ = cancel.cancelled() => None,
        out = op => Some(out),
    }
}

/// Write one inbound payload to the loopback stream, interruptible by a route
/// close and bounded by lack of PROGRESS rather than by total time.
///
/// `write_all` is the wrong primitive here twice over: it cannot be interrupted,
/// and it exposes no progress, so a slow-but-draining consumer and one that has
/// stopped reading look identical from outside it. Writing chunk by chunk gives
/// both - the budget is re-armed on every byte that moves, so legitimate
/// backpressure is untouched, and [`LOCAL_WRITE_STALL`] only ever ends a
/// consumer that has accepted nothing at all.
///
/// `false` means the payload was not fully delivered and the conn is finished.
async fn write_local<W>(lw: &mut W, payload: &[u8], cancel: &CancellationToken) -> bool
where
    W: tokio::io::AsyncWrite + Unpin,
{
    let mut written = 0;
    while written < payload.len() {
        let chunk = tokio::time::timeout(LOCAL_WRITE_STALL, lw.write(&payload[written..]));
        match unless_cancelled(cancel, chunk).await {
            Some(Ok(Ok(n))) if n > 0 => written += n,
            _ => return false,
        }
    }
    true
}

/// Bridge one logical connection: dial the selected loopback listener, accept the
/// `Open`, and shuttle bytes both ways until either side ends.
async fn bridge_conn(
    conn_id: u64,
    local_addr: &str,
    to_relay: mpsc::Sender<tokio_tungstenite::tungstenite::Message>,
    mut inbound: mpsc::Receiver<ConnMsg>,
    // Retires THIS conn: a child of the link token, so it fires both when the
    // relay closes this route and when the whole link dies.
    cancel: CancellationToken,
    conns: ConnMap,
    // Present only for enroll-routed conns: register our outbound source port
    // so the enrollment endpoint classifies this loopback peer as relay-routed.
    // A drop guard deregisters it however this task ends.
    bridge_ports: Option<crate::enroll::BridgePortSet>,
) {
    // Bind the outbound source socket and register its ephemeral source port in
    // BridgePortSet BEFORE connecting, so the enrollment listener cannot accept
    // and classify this loopback peer before the port is visible. Registering
    // after `connect()` returned (the previous ordering) left a window in which
    // the accept task could classify a relay-routed client as `Direct` (B2).
    // `_port_guard` deregisters the port on every exit path.
    // The dial itself is bounded and cancellable. A local listener whose accept
    // backlog is full leaves this connect pending in the kernel, and the relay's
    // per-`Open` pair timeout can only drop its own route entry - it cannot
    // reach a bridge task parked in `connect`. Without this, that task and the
    // source port it registered are held until the kernel gives up. On every
    // exit here `guard` falls out of scope, so the port deregisters whether the
    // dial succeeded, timed out, or was cancelled.
    let local_and_guard = match bind_and_register(local_addr, bridge_ports) {
        Ok((socket, remote, guard)) => {
            let dial = tokio::time::timeout(LOCAL_DIAL_DEADLINE, socket.connect(remote));
            unless_cancelled(&cancel, dial)
                .await
                .and_then(|outcome| outcome.ok())
                .and_then(|s| s.ok())
                .map(|s| (s, guard))
        }
        Err(_) => None,
    };
    let (local, _port_guard) = match local_and_guard {
        Some(v) => v,
        None => {
            let _ = unless_cancelled(
                &cancel,
                to_relay.send(tungstenite_text(&Control::Close {
                    conn_id,
                    reason: "bridge_dial_failed".into(),
                })),
            )
            .await;
            conns.lock().await.remove(&conn_id);
            return;
        }
    };
    // Accept the connection to the relay (it tells the waiting client).
    let _ = unless_cancelled(
        &cancel,
        to_relay.send(tungstenite_text(&Control::Opened { conn_id })),
    )
    .await;
    // Grant the client our receive window for this conn up front.
    let _ = unless_cancelled(
        &cancel,
        to_relay.send(tungstenite_text(&Control::Window {
            conn_id,
            credit: INITIAL_WINDOW,
        })),
    )
    .await;

    // Per-conn credit flow control (mirrors the client pump): `send_window` gates
    // loopback->relay bytes so one conn cannot monopolize the shared relay link
    // (head-of-line); `recv_drained` counts client->daemon bytes written to the
    // loopback so we replenish the client's window.
    let mut send_window = ConnWindow::new(INITIAL_WINDOW);
    let mut recv_drained: u32 = 0;

    let (mut lr, mut lw) = local.into_split();
    let mut buf = vec![0u8; MAX_DATA_PAYLOAD];
    loop {
        tokio::select! {
            _ = cancel.cancelled() => break,
            // Pause reading the loopback when the send window is exhausted, until
            // a DataAck replenishes it.
            n = lr.read(&mut buf), if !send_window.is_blocked() => match n {
                Ok(0) | Err(_) => break,
                Ok(n) => {
                    send_window.debit(n);
                    let queued = unless_cancelled(
                        &cancel,
                        to_relay.send(tokio_tungstenite::tungstenite::Message::binary(
                            encode_data(conn_id, &buf[..n]),
                        )),
                    )
                    .await;
                    if !matches!(queued, Some(Ok(()))) {
                        break;
                    }
                }
            },
            msg = inbound.recv() => match msg {
                Some(ConnMsg::Data(p)) => {
                    if !write_local(&mut lw, &p, &cancel).await {
                        break;
                    }
                    recv_drained = recv_drained.saturating_add(p.len() as u32);
                    if recv_drained >= INITIAL_WINDOW / 2 {
                        let _ = unless_cancelled(
                            &cancel,
                            to_relay.send(tungstenite_text(&Control::DataAck {
                                conn_id,
                                consumed: recv_drained,
                            })),
                        )
                        .await;
                        recv_drained = 0;
                    }
                }
                Some(ConnMsg::Window(credit)) => send_window.set(credit),
                Some(ConnMsg::Ack(consumed)) => send_window.ack(consumed),
                None => break,
            },
        }
    }
    // Skipped when this conn was retired: the relay closed the route, so it is
    // not waiting to be told, and the send could park on a full queue.
    let _ = unless_cancelled(
        &cancel,
        to_relay.send(tungstenite_text(&Control::Close {
            conn_id,
            reason: "bridge_closed".into(),
        })),
    )
    .await;
    conns.lock().await.remove(&conn_id);
}

/// Route one relay frame to a logical conn's bridge task WITHOUT blocking the
/// shared relay-link reader (mirror of the zerorelay-side delivery rule).
/// Sender cloned under the lock, guard dropped, delivery non-blocking.
///
/// A full per-conn buffer means that conn's loopback write side is wedged: tear
/// down only that conn and tell the relay, keeping every other bridged conn (and
/// Open/Close handling) live. Both halves of that have to be non-blocking to be
/// true - the per-conn delivery AND the notification back to the relay. See
/// [`notify_relay`]: awaiting the notification would have made this function
/// freeze the very reader it exists to keep running.
async fn deliver_conn_msg(
    conns: &Mutex<HashMap<u64, ConnHandle>>,
    to_relay: &mpsc::Sender<tokio_tungstenite::tungstenite::Message>,
    conn_id: u64,
    msg: ConnMsg,
) {
    let tx = {
        let map = conns.lock().await;
        match map.get(&conn_id) {
            Some(handle) => handle.tx.clone(),
            None => return,
        }
    };
    match tx.try_send(msg) {
        Ok(()) => {}
        Err(mpsc::error::TrySendError::Closed(_)) => {
            conns.lock().await.remove(&conn_id);
        }
        Err(mpsc::error::TrySendError::Full(_)) => {
            // Route removed first, so this conn's bridge task is already
            // cancelled and its cleanup guaranteed; the notification is then
            // best-effort and never parks the shared reader.
            conns.lock().await.remove(&conn_id);
            notify_relay(
                to_relay,
                tungstenite_text(&Control::Close {
                    conn_id,
                    reason: "conn_backpressured".into(),
                }),
            );
        }
    }
}

/// Best-effort notification to the relay from the SHARED link reader.
///
/// Nothing on the reader path may AWAIT `to_relay`. The queue is bounded and its
/// writer can be parked for a whole [`WRITE_STALL`] budget against a relay that
/// has stopped reading, so one parked send there stops the reader polling its
/// `link_dead` arm, demuxing for every other conn on the link, and observing
/// teardown - turning one backpressured peer into a frozen node.
///
/// Every caller has already completed the authoritative local action before
/// calling this: the `Open` was refused so no conn state exists, or the route
/// was removed from the conn map (which cancels its bridge task). The frame is a
/// courtesy the relay can also infer from its own pair timeout, so dropping it
/// costs promptness, never correctness or cleanup.
fn notify_relay(
    to_relay: &mpsc::Sender<tokio_tungstenite::tungstenite::Message>,
    frame: tokio_tungstenite::tungstenite::Message,
) {
    let _ = to_relay.try_send(frame);
}

fn tungstenite_text(frame: &Control) -> tokio_tungstenite::tungstenite::Message {
    tokio_tungstenite::tungstenite::Message::text(frame.to_json())
}

/// Read the next control frame, transparently answering pings.
async fn next_control<S>(ws: &mut tokio_tungstenite::WebSocketStream<S>) -> Option<Control>
where
    S: tokio::io::AsyncRead + tokio::io::AsyncWrite + Unpin,
{
    while let Some(msg) = ws.next().await {
        match msg {
            Ok(tokio_tungstenite::tungstenite::Message::Text(t)) => return parse_control_text(&t),
            Ok(tokio_tungstenite::tungstenite::Message::Ping(p)) => {
                let _ = ws
                    .send(tokio_tungstenite::tungstenite::Message::Pong(p))
                    .await;
            }
            Ok(tokio_tungstenite::tungstenite::Message::Pong(_)) => {}
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

/// Build the client TLS config used to verify the relay's OUTER certificate, plus
/// the pin verifier handle when one is used (so the caller can persist a
/// TOFU-observed pin after the handshake). Precedence: insecure (test) > a CA
/// file > a stored leaf pin > opt-in TOFU > the built-in public roots.
fn relay_client_config(
    ca_path: Option<&str>,
    insecure: bool,
    pin: Option<&str>,
    tofu: bool,
    outer_cert: Option<&str>,
    outer_key: Option<&str>,
) -> Result<(
    Arc<rustls::ClientConfig>,
    Option<Arc<zeroclaw_tls::RelayPinVerifier>>,
)> {
    let builder = rustls::ClientConfig::builder_with_provider(Arc::new(
        rustls::crypto::ring::default_provider(),
    ))
    .with_safe_default_protocol_versions()
    .context("ring provider supports default protocol versions")?;

    // Server verification choice -> a builder awaiting the client-auth choice.
    let (verified, verifier) = if insecure {
        (
            builder
                .dangerous()
                .with_custom_certificate_verifier(Arc::new(NoVerify)),
            None,
        )
    } else if let Some(ca) = ca_path {
        let mut roots = rustls::RootCertStore::empty();
        for cert in zeroclaw_tls::load_certs(ca)? {
            roots.add(cert).context("adding relay CA to root store")?;
        }
        (builder.with_root_certificates(roots), None)
    } else if let Some(pin) = pin.filter(|p| !p.is_empty()) {
        let v = Arc::new(zeroclaw_tls::RelayPinVerifier::pinned(pin.to_string()));
        (
            builder
                .dangerous()
                .with_custom_certificate_verifier(v.clone()),
            Some(v),
        )
    } else if tofu {
        let v = Arc::new(zeroclaw_tls::RelayPinVerifier::tofu());
        (
            builder
                .dangerous()
                .with_custom_certificate_verifier(v.clone()),
            Some(v),
        )
    } else {
        let mut roots = rustls::RootCertStore::empty();
        roots.extend(webpki_roots::TLS_SERVER_ROOTS.iter().cloned());
        (builder.with_root_certificates(roots), None)
    };

    // Outer-mTLS variant: present a client cert to the relay when configured (so a
    // relay with outer_client_auth = required admits this daemon). Inner mTLS is
    // separate and unaffected.
    let config = match (outer_cert, outer_key) {
        (Some(cert), Some(key)) => {
            let chain = zeroclaw_tls::load_certs(cert)?;
            let key = zeroclaw_tls::load_private_key(key)?;
            verified
                .with_client_auth_cert(chain, key)
                .context("loading the relay outer client cert/key")?
        }
        _ => verified.with_no_client_auth(),
    };
    Ok((Arc::new(config), verifier))
}

/// Skip-verify server verifier for the relay's outer cert (test only).
#[derive(Debug)]
struct NoVerify;

impl rustls::client::danger::ServerCertVerifier for NoVerify {
    fn verify_server_cert(
        &self,
        _end_entity: &rustls::pki_types::CertificateDer<'_>,
        _intermediates: &[rustls::pki_types::CertificateDer<'_>],
        _server_name: &rustls::pki_types::ServerName<'_>,
        _ocsp_response: &[u8],
        _now: rustls::pki_types::UnixTime,
    ) -> std::result::Result<rustls::client::danger::ServerCertVerified, rustls::Error> {
        Ok(rustls::client::danger::ServerCertVerified::assertion())
    }

    fn verify_tls12_signature(
        &self,
        _message: &[u8],
        _cert: &rustls::pki_types::CertificateDer<'_>,
        _dss: &rustls::DigitallySignedStruct,
    ) -> std::result::Result<rustls::client::danger::HandshakeSignatureValid, rustls::Error> {
        Ok(rustls::client::danger::HandshakeSignatureValid::assertion())
    }

    fn verify_tls13_signature(
        &self,
        _message: &[u8],
        _cert: &rustls::pki_types::CertificateDer<'_>,
        _dss: &rustls::DigitallySignedStruct,
    ) -> std::result::Result<rustls::client::danger::HandshakeSignatureValid, rustls::Error> {
        Ok(rustls::client::danger::HandshakeSignatureValid::assertion())
    }

    fn supported_verify_schemes(&self) -> Vec<rustls::SignatureScheme> {
        rustls::crypto::ring::default_provider()
            .signature_verification_algorithms
            .supported_schemes()
    }
}

#[cfg(test)]
mod node_id_tests {
    use super::*;

    fn route_test_config() -> RelayBridgeConfig {
        RelayBridgeConfig {
            relay_addr: "127.0.0.1:8443".into(),
            relay_host: "localhost".into(),
            node_id: "node".into(),
            relay_token: None,
            local_wss_addr: "127.0.0.1:9781".into(),
            local_enroll_addr: Some("127.0.0.1:9782".into()),
            enroll_bridge_ports: None,
            signing_key_pkcs8: Vec::new(),
            relay_ca_path: None,
            relay_insecure: true,
            relay_tofu: false,
            outer_client_cert: None,
            outer_client_key: None,
            max_conns: 16,
            open_burst: 60,
            open_rate_per_sec: 20.0,
            data_dir: std::path::PathBuf::from("/tmp"),
            node_id_rotation_days: 0,
            rotation_allowed: false,
        }
    }

    #[test]
    fn open_route_defaults_to_wss() {
        let cfg = route_test_config();
        assert_eq!(
            open_route_target(&cfg, None).unwrap(),
            ("127.0.0.1:9781", false)
        );
        assert_eq!(
            open_route_target(&cfg, Some("unknown")).unwrap(),
            ("127.0.0.1:9781", false)
        );
    }

    #[test]
    fn open_route_selects_enrollment_when_available() {
        let cfg = route_test_config();
        assert_eq!(
            open_route_target(&cfg, Some(PEER_HINT_ENROLL)).unwrap(),
            ("127.0.0.1:9782", true)
        );
    }

    #[test]
    fn open_route_rejects_enrollment_when_disabled() {
        let mut cfg = route_test_config();
        cfg.local_enroll_addr = None;
        assert_eq!(
            open_route_target(&cfg, Some(PEER_HINT_ENROLL)).unwrap_err(),
            "enroll_unavailable"
        );
    }

    #[test]
    fn relay_ca_path_takes_precedence_over_tofu_state() {
        let dir = tempfile::tempdir().unwrap();
        let missing_ca = dir.path().join("relay-ca.pem");
        let result = relay_client_config(
            Some(missing_ca.to_string_lossy().as_ref()),
            false,
            Some("00"),
            true,
            None,
            None,
        );
        assert!(
            result.is_err(),
            "configured relay CA must be loaded instead of falling back to stored/TOFU pins"
        );
    }

    #[test]
    fn mint_node_id_is_128_bit_hex_and_unique() {
        let a = mint_node_id().unwrap();
        let b = mint_node_id().unwrap();
        assert_eq!(a.len(), 32, "16 bytes => 32 hex chars");
        assert!(a.chars().all(|c| c.is_ascii_hexdigit()));
        assert_ne!(a, b, "ids must be unguessable / distinct");
    }

    #[test]
    fn persist_then_ensure_reads_back_the_rotated_id() {
        let dir = tempfile::tempdir().unwrap();
        let id = mint_node_id().unwrap();
        persist_node_id(dir.path(), &id).unwrap();
        // Auto-mint mode (empty configured) reads the persisted id - this is the
        // path relay_profile() uses, so a rotated id flows to clients.
        assert_eq!(ensure_node_id(dir.path(), "").unwrap(), id);

        // A rotation overwrites it atomically; ensure_node_id sees the new value.
        let rotated = mint_node_id().unwrap();
        persist_node_id(dir.path(), &rotated).unwrap();
        assert_eq!(ensure_node_id(dir.path(), "").unwrap(), rotated);
    }

    #[test]
    fn pinned_node_id_ignores_the_persisted_file() {
        let dir = tempfile::tempdir().unwrap();
        persist_node_id(dir.path(), "0123456789abcdef0123456789abcdef").unwrap();
        // A pinned id wins, so a pinned daemon is never rotated.
        assert_eq!(
            ensure_node_id(dir.path(), "pinned-id").unwrap(),
            "pinned-id"
        );
    }

    #[test]
    fn rotation_trigger_round_trips() {
        let dir = tempfile::tempdir().unwrap();
        let path = rotate_trigger_path(dir.path());
        assert!(!path.exists());
        request_node_id_rotation(dir.path()).unwrap();
        assert!(path.exists(), "the CLI request creates the trigger file");
        // The supervisor consumes it by removing it.
        std::fs::remove_file(&path).unwrap();
        assert!(!path.exists());
    }

    #[tokio::test]
    async fn rotation_keeps_the_published_id_when_new_registration_is_refused() {
        let dir = tempfile::tempdir().unwrap();
        let old_id = mint_node_id().unwrap();
        persist_node_id(dir.path(), &old_id).unwrap();
        let cancel = CancellationToken::new();
        let (sender, receiver) = oneshot::channel();
        drop(sender); // A refused registration closes the readiness channel.

        assert_eq!(
            wait_for_new_link_registration(receiver, &cancel).await,
            RotationRegistration::Unavailable
        );
        assert_eq!(ensure_node_id(dir.path(), "").unwrap(), old_id);
    }

    #[tokio::test]
    async fn rotation_publishes_only_after_new_registration_is_confirmed() {
        let cancel = CancellationToken::new();
        let (sender, receiver) = oneshot::channel();
        sender.send(()).unwrap();

        assert_eq!(
            wait_for_new_link_registration(receiver, &cancel).await,
            RotationRegistration::Registered
        );
    }
}

#[cfg(test)]
mod control_frame_tests {
    use super::*;

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

#[cfg(test)]
// Test code, not daemon-path: bare `tokio::spawn` is fine here (the
// `zeroclaw_spawn::spawn!` attribution rule is for production daemon tasks).
#[allow(clippy::disallowed_methods)]
mod link_liveness_tests {
    use super::*;
    use std::future::Future;
    use std::pin::Pin;
    use std::sync::atomic::{AtomicUsize, Ordering};
    use std::task::{Context as TaskContext, Poll};
    use tokio_tungstenite::tungstenite::Message;

    fn ping() -> Message {
        Message::Ping(Vec::new().into())
    }

    /// Accepts a frame and then never completes the flush: the shape a relay
    /// takes once it stops reading and the socket buffer is full.
    struct StalledSink;

    impl futures_util::Sink<Message> for StalledSink {
        type Error = std::io::Error;

        fn poll_ready(
            self: Pin<&mut Self>,
            _cx: &mut TaskContext<'_>,
        ) -> Poll<std::result::Result<(), Self::Error>> {
            Poll::Ready(Ok(()))
        }

        fn start_send(
            self: Pin<&mut Self>,
            _item: Message,
        ) -> std::result::Result<(), Self::Error> {
            Ok(())
        }

        fn poll_flush(
            self: Pin<&mut Self>,
            _cx: &mut TaskContext<'_>,
        ) -> Poll<std::result::Result<(), Self::Error>> {
            Poll::Pending
        }

        fn poll_close(
            self: Pin<&mut Self>,
            _cx: &mut TaskContext<'_>,
        ) -> Poll<std::result::Result<(), Self::Error>> {
            Poll::Pending
        }
    }

    /// Completes every flush, but only after `delay`: a relay that is slow and
    /// still reading. The liveness bound must not touch this one.
    struct SlowSink {
        delay: Duration,
        sleeping: Option<Pin<Box<tokio::time::Sleep>>>,
        delivered: Arc<AtomicUsize>,
    }

    impl futures_util::Sink<Message> for SlowSink {
        type Error = std::io::Error;

        fn poll_ready(
            self: Pin<&mut Self>,
            _cx: &mut TaskContext<'_>,
        ) -> Poll<std::result::Result<(), Self::Error>> {
            Poll::Ready(Ok(()))
        }

        fn start_send(
            self: Pin<&mut Self>,
            _item: Message,
        ) -> std::result::Result<(), Self::Error> {
            Ok(())
        }

        fn poll_flush(
            mut self: Pin<&mut Self>,
            cx: &mut TaskContext<'_>,
        ) -> Poll<std::result::Result<(), Self::Error>> {
            let this = self.as_mut().get_mut();
            let delay = this.delay;
            let sleeping = this
                .sleeping
                .get_or_insert_with(|| Box::pin(tokio::time::sleep(delay)));
            match sleeping.as_mut().poll(cx) {
                Poll::Pending => Poll::Pending,
                Poll::Ready(()) => {
                    this.sleeping = None;
                    this.delivered.fetch_add(1, Ordering::Relaxed);
                    Poll::Ready(Ok(()))
                }
            }
        }

        fn poll_close(
            self: Pin<&mut Self>,
            _cx: &mut TaskContext<'_>,
        ) -> Poll<std::result::Result<(), Self::Error>> {
            Poll::Ready(Ok(()))
        }
    }

    // A relay that accepts the socket and stops reading parks the writer inside
    // `sink.send`. Unbounded, that write never returns, the queue behind it
    // fills, and every producer parks with it, so nothing is left to declare the
    // link dead.
    #[tokio::test(start_paused = true)]
    async fn writer_declares_the_link_dead_when_the_sink_stalls() {
        let (to_relay, from_tasks) = mpsc::channel::<Message>(4);
        let link_dead = CancellationToken::new();
        let writer = tokio::spawn(relay_writer(from_tasks, StalledSink, link_dead.clone()));

        to_relay.send(ping()).await.expect("queued");

        tokio::time::timeout(WRITE_STALL * 2, link_dead.cancelled())
            .await
            .expect("a stalled sink must declare the link dead within WRITE_STALL");
        assert!(
            to_relay.send(ping()).await.is_err(),
            "the writer must close the queue so parked producers fail instead of waiting on it"
        );

        writer.abort();
    }

    // The bound is on no progress, not on slow progress: a relay that keeps
    // reading, however slowly, stays up and every frame is still delivered.
    #[tokio::test(start_paused = true)]
    async fn writer_keeps_a_slow_but_reading_relay_alive() {
        let delivered = Arc::new(AtomicUsize::new(0));
        let sink = SlowSink {
            delay: WRITE_STALL / 2,
            sleeping: None,
            delivered: delivered.clone(),
        };
        let (to_relay, from_tasks) = mpsc::channel::<Message>(4);
        let link_dead = CancellationToken::new();
        let writer = tokio::spawn(relay_writer(from_tasks, sink, link_dead.clone()));

        for _ in 0..4 {
            to_relay.send(ping()).await.expect("queued");
        }
        while delivered.load(Ordering::Relaxed) < 4 {
            tokio::time::sleep(WRITE_STALL).await;
        }
        assert!(
            !link_dead.is_cancelled(),
            "backpressure from a reading relay must not be treated as a dead link"
        );

        writer.abort();
    }

    // The watchdog's silence check is the last thing still running when the
    // outbound path is stuck, so its own ping must never be able to park it.
    #[tokio::test(start_paused = true)]
    async fn watchdog_declares_the_link_dead_while_the_queue_is_full() {
        let (to_relay, _from_tasks) = mpsc::channel::<Message>(1);
        to_relay.try_send(ping()).expect("prefill");
        let last_seen = Arc::new(Mutex::new(Instant::now()));
        let link_dead = CancellationToken::new();
        tokio::spawn(keepalive_watchdog(to_relay, last_seen, link_dead.clone()));

        tokio::time::timeout(DEAD_AFTER + 2 * KEEPALIVE, link_dead.cancelled())
            .await
            .expect("silence past DEAD_AFTER must kill the link even with the queue full");
    }

    // A full queue on its own is not death: a busy link that is still being
    // heard from survives well past DEAD_AFTER.
    #[tokio::test(start_paused = true)]
    async fn watchdog_leaves_a_link_it_still_hears_from_alive() {
        let (to_relay, mut from_tasks) = mpsc::channel::<Message>(4);
        let last_seen = Arc::new(Mutex::new(Instant::now()));
        let link_dead = CancellationToken::new();
        tokio::spawn(keepalive_watchdog(
            to_relay,
            last_seen.clone(),
            link_dead.clone(),
        ));
        // Stand in for the reader loop, which stamps `last_seen` on every
        // inbound frame while the writer drains the queue.
        let reader = tokio::spawn(async move {
            while from_tasks.recv().await.is_some() {
                *last_seen.lock().await = Instant::now();
            }
        });

        tokio::time::sleep(DEAD_AFTER * 3).await;
        assert!(
            !link_dead.is_cancelled(),
            "a link that is still being heard from must stay up"
        );

        reader.abort();
    }
}

#[cfg(test)]
// Test code, not daemon-path: bare `tokio::spawn` is fine here (the
// `zeroclaw_spawn::spawn!` attribution rule is for production daemon tasks).
#[allow(clippy::disallowed_methods)]
mod conn_cancellation_tests {
    use super::*;
    use std::sync::atomic::{AtomicUsize, Ordering};

    fn empty_port_set() -> crate::enroll::BridgePortSet {
        std::sync::Arc::new(std::sync::Mutex::new(std::collections::HashSet::new()))
    }

    // A consumer that has accepted nothing at all is stopped, not slow, and the
    // conn must not outlive that.
    #[tokio::test(start_paused = true)]
    async fn a_local_write_ends_when_the_consumer_stops_reading() {
        // `_reader` is held: dropping it would close the pipe and end the write
        // for the wrong reason.
        let (mut writer, _reader) = tokio::io::duplex(1024);
        let cancel = CancellationToken::new();
        let payload = vec![0u8; 64 * 1024];

        let delivered = tokio::time::timeout(
            LOCAL_WRITE_STALL * 2,
            write_local(&mut writer, &payload, &cancel),
        )
        .await
        .expect("the write must end on its own budget rather than hang");
        assert!(
            !delivered,
            "a consumer that accepts nothing must not hold the conn open"
        );
    }

    // The budget is on no progress, not on slow progress: a consumer that keeps
    // draining, however little at a time, is legitimate backpressure and every
    // byte still lands.
    #[tokio::test(start_paused = true)]
    async fn a_local_write_survives_a_slow_but_draining_consumer() {
        let (mut writer, mut reader) = tokio::io::duplex(1024);
        let cancel = CancellationToken::new();
        let payload = vec![0u8; 8 * 1024];

        let drainer = tokio::spawn(async move {
            let mut sink = vec![0u8; 256];
            loop {
                tokio::time::sleep(LOCAL_WRITE_STALL / 2).await;
                if reader.read(&mut sink).await.is_err() {
                    break;
                }
            }
        });

        let delivered = tokio::time::timeout(
            LOCAL_WRITE_STALL * 1000,
            write_local(&mut writer, &payload, &cancel),
        )
        .await
        .expect("a draining consumer must not hit the no-progress budget");
        assert!(
            delivered,
            "backpressure from a draining consumer must not end the conn"
        );

        drainer.abort();
    }

    // Real clock: LOCAL_WRITE_STALL cannot expire inside the window asserted
    // here, so a prompt return is cancellation's doing.
    #[tokio::test]
    async fn a_local_write_returns_on_cancellation() {
        let (mut writer, _reader) = tokio::io::duplex(1024);
        let cancel = CancellationToken::new();
        let payload = vec![0u8; 64 * 1024];
        let canceller = {
            let cancel = cancel.clone();
            tokio::spawn(async move {
                tokio::time::sleep(Duration::from_millis(50)).await;
                cancel.cancel();
            })
        };

        let delivered = tokio::time::timeout(
            Duration::from_secs(2),
            write_local(&mut writer, &payload, &cancel),
        )
        .await
        .expect("a retired conn must not wait on the local consumer");
        assert!(!delivered);

        let _ = canceller.await;
    }

    /// Wait, on the real clock, until the feeder stops making progress. That
    /// stall is the parked local write: the bridge task can only stop draining
    /// its inbound queue once it is blocked writing to a peer that has stopped
    /// reading.
    async fn wait_until_stalled(fed: &AtomicUsize) {
        let mut previous = 0;
        let mut stalled = 0;
        for _ in 0..200 {
            tokio::time::sleep(Duration::from_millis(50)).await;
            let now = fed.load(Ordering::Relaxed);
            if now == previous && now > 0 {
                stalled += 1;
                if stalled >= 4 {
                    return;
                }
            } else {
                stalled = 0;
            }
            previous = now;
        }
        panic!("the bridge never parked on the local write");
    }

    // The reviewed hazard end to end: the relay closes a route while the bridge
    // task is inside a local write. The task's own select cannot interrupt that
    // write, so only the per-conn token can retire it - and until it does, the
    // task and its registered source port are held.
    #[tokio::test]
    async fn a_route_close_retires_a_bridge_task_parked_on_the_local_write() {
        // A local peer that accepts and then never reads, with the smallest
        // receive buffer the kernel will grant so the write parks quickly.
        let socket = tokio::net::TcpSocket::new_v4().expect("socket");
        let _ = socket.set_recv_buffer_size(4 * 1024);
        socket
            .bind("127.0.0.1:0".parse().expect("addr"))
            .expect("bind");
        let listener = socket.listen(8).expect("listen");
        let addr = listener.local_addr().expect("local addr").to_string();
        let local_peer = tokio::spawn(async move {
            let (stream, _) = listener.accept().await.expect("accept");
            let _held = stream;
            std::future::pending::<()>().await
        });

        let ports = empty_port_set();
        let (to_relay, _to_relay_rx) = mpsc::channel(64);
        let (inbound_tx, inbound_rx) = mpsc::channel::<ConnMsg>(4);
        let conns: ConnMap = Arc::new(Mutex::new(HashMap::new()));
        let conn_cancel = CancellationToken::new().child_token();
        conns.lock().await.insert(
            7,
            ConnHandle {
                tx: inbound_tx.clone(),
                cancel: conn_cancel.clone(),
            },
        );

        let task = {
            let ports = ports.clone();
            let conns = conns.clone();
            tokio::spawn(async move {
                bridge_conn(
                    7,
                    &addr,
                    to_relay,
                    inbound_rx,
                    conn_cancel,
                    conns,
                    Some(ports),
                )
                .await;
            })
        };

        // Feed until the write parks. The inbound queue is deliberately left
        // open by this clone, so nothing but cancellation can end the task.
        let fed = Arc::new(AtomicUsize::new(0));
        let feeder = {
            let fed = fed.clone();
            tokio::spawn(async move {
                while inbound_tx
                    .send(ConnMsg::Data(vec![0u8; MAX_DATA_PAYLOAD]))
                    .await
                    .is_ok()
                {
                    fed.fetch_add(1, Ordering::Relaxed);
                }
            })
        };
        wait_until_stalled(&fed).await;
        assert!(
            !ports.lock().expect("bridge port set lock").is_empty(),
            "the parked conn must still hold its registered source port"
        );

        // Exactly what the reader loop does on a relay `Close`.
        conns.lock().await.remove(&7);

        tokio::time::timeout(Duration::from_secs(2), task)
            .await
            .expect("a route close must retire a bridge task parked on the local write")
            .expect("bridge task");
        assert!(
            ports.lock().expect("bridge port set lock").is_empty(),
            "retiring the conn must release its source port"
        );

        feeder.abort();
        local_peer.abort();
    }
}

#[cfg(test)]
// Test code, not daemon-path: bare `tokio::spawn` is fine here (the
// `zeroclaw_spawn::spawn!` attribution rule is for production daemon tasks).
#[allow(clippy::disallowed_methods)]
mod local_dial_tests {
    use super::*;
    use tokio::net::{TcpListener, TcpStream};

    fn empty_port_set() -> crate::enroll::BridgePortSet {
        std::sync::Arc::new(std::sync::Mutex::new(std::collections::HashSet::new()))
    }

    /// A loopback listener whose accept backlog is full and which never accepts,
    /// so a further connect to it stays pending in the kernel instead of
    /// completing or being refused. That is the stalled local listener a bridge
    /// dial has to be able to survive. Kernels round `listen(1)` up by different
    /// amounts, so the backlog is filled by probing rather than by assuming a
    /// count. The listener and the connections that fill it are returned because
    /// dropping either would free the backlog.
    async fn stalled_listener() -> (String, TcpListener, Vec<TcpStream>) {
        let socket = tokio::net::TcpSocket::new_v4().expect("socket");
        socket
            .bind("127.0.0.1:0".parse().expect("addr"))
            .expect("bind");
        let listener = socket.listen(1).expect("listen");
        let addr = listener.local_addr().expect("local addr");
        let mut held = Vec::new();
        for _ in 0..512 {
            match tokio::time::timeout(Duration::from_millis(250), TcpStream::connect(addr)).await {
                Ok(Ok(stream)) => held.push(stream),
                Err(_) => return (addr.to_string(), listener, held),
                Ok(Err(_)) => break,
            }
        }
        panic!("could not stall a loopback connect: the listener backlog never filled");
    }

    /// Drive one `bridge_conn` against `addr`, returning its task and the port
    /// set it registers into.
    fn spawn_bridge_dial(
        addr: String,
        link_dead: CancellationToken,
    ) -> (
        tokio::task::JoinHandle<()>,
        crate::enroll::BridgePortSet,
        mpsc::Receiver<tokio_tungstenite::tungstenite::Message>,
    ) {
        let ports = empty_port_set();
        let (to_relay, to_relay_rx) = mpsc::channel(8);
        let (_inbound_tx, inbound_rx) = mpsc::channel(8);
        let conns = Arc::new(Mutex::new(HashMap::new()));
        let task = {
            let ports = ports.clone();
            tokio::spawn(async move {
                bridge_conn(
                    1,
                    &addr,
                    to_relay,
                    inbound_rx,
                    link_dead,
                    conns,
                    Some(ports),
                )
                .await;
            })
        };
        (task, ports, to_relay_rx)
    }

    /// Wait until the dial has registered its source port. From that point the
    /// task is inside `connect` with the deregistering guard held, which is the
    /// state the bound has to be able to end.
    async fn wait_for_registered_port(ports: &crate::enroll::BridgePortSet) -> u16 {
        for _ in 0..400 {
            if let Some(port) = ports
                .lock()
                .expect("bridge port set lock")
                .iter()
                .copied()
                .next()
            {
                return port;
            }
            tokio::time::sleep(Duration::from_millis(10)).await;
        }
        panic!("the bridge dial never registered its source port");
    }

    // The relay's per-`Open` pair timeout drops its own route entry but cannot
    // reach a bridge task parked in `connect`, so the route closing has to be
    // able to end the dial itself. Real clock: LOCAL_DIAL_DEADLINE cannot expire
    // inside the window asserted here, so a prompt return is cancellation's.
    #[tokio::test]
    async fn a_closed_route_releases_a_dial_parked_on_a_stalled_listener() {
        let (addr, _listener, _backlog) = stalled_listener().await;
        let link_dead = CancellationToken::new();
        let (task, ports, _to_relay_rx) = spawn_bridge_dial(addr, link_dead.clone());
        let port = wait_for_registered_port(&ports).await;

        link_dead.cancel();
        tokio::time::timeout(Duration::from_secs(2), task)
            .await
            .expect("a closed route must not wait on the local listener's accept")
            .expect("bridge task");
        assert!(
            !ports.lock().expect("bridge port set lock").contains(&port),
            "the source port must deregister when the dial is abandoned"
        );
    }

    // Nothing closes the route here: the dial has to end on its own budget, and
    // release the source port when it does.
    #[tokio::test]
    async fn a_dial_that_never_connects_ends_on_its_own_budget() {
        let (addr, _listener, _backlog) = stalled_listener().await;
        let (task, ports, _to_relay_rx) = spawn_bridge_dial(addr, CancellationToken::new());
        let port = wait_for_registered_port(&ports).await;

        // The dial holds nothing but its own deadline now, so the budget is
        // spent by advancing the clock rather than by waiting on it.
        tokio::time::pause();
        tokio::time::advance(LOCAL_DIAL_DEADLINE / 2).await;
        assert!(
            !task.is_finished(),
            "a dial must be given its full budget, not abandoned early"
        );

        tokio::time::advance(LOCAL_DIAL_DEADLINE).await;
        tokio::time::resume();
        tokio::time::timeout(Duration::from_secs(2), task)
            .await
            .expect("a dial that never connects must end at its deadline")
            .expect("bridge task");
        assert!(
            !ports.lock().expect("bridge port set lock").contains(&port),
            "the source port must deregister when the dial times out"
        );
    }
}

#[cfg(test)]
// Test code, not daemon-path: bare `tokio::spawn` is fine here (the
// `zeroclaw_spawn::spawn!` attribution rule is for production daemon tasks).
#[allow(clippy::disallowed_methods)]
mod bridge_classification_race_tests {
    use super::*;
    use tokio::net::TcpListener;

    fn empty_port_set() -> crate::enroll::BridgePortSet {
        std::sync::Arc::new(std::sync::Mutex::new(std::collections::HashSet::new()))
    }

    // B2: the outbound source port must be registered in BridgePortSet BEFORE the
    // connection is established, so the enrollment listener's accept/classify step
    // can never observe the loopback peer before its port is present (which would
    // misclassify a relay-routed client as Direct). `bind_and_register` publishes
    // the port at bind time, strictly before the caller's connect() runs.
    #[tokio::test]
    async fn bind_and_register_publishes_source_port_before_connect() {
        let listener = TcpListener::bind("127.0.0.1:0").await.unwrap();
        let addr = listener.local_addr().unwrap().to_string();
        let set = empty_port_set();

        let (socket, remote, _guard) =
            bind_and_register(&addr, Some(set.clone())).expect("bind+register");
        let bound_port = socket.local_addr().unwrap().port();

        // Registered at bind time, before any connect() has run.
        assert!(
            set.lock().unwrap().contains(&bound_port),
            "source port must be registered before connect()"
        );

        // The eventual connection uses exactly that source port, so the
        // accept-side classification keys on the registered value.
        let _stream = socket.connect(remote).await.unwrap();
        let (_srv, peer) = listener.accept().await.unwrap();
        assert_eq!(peer.port(), bound_port);
        assert!(set.lock().unwrap().contains(&peer.port()));
    }

    // End-to-end: with bind-register-connect ordering the enrollment listener's
    // accept always observes the source port already registered, so it classifies
    // the peer as RelayBridge. Holds by happens-before: register() is sequenced
    // before connect(), and accept() can only return a connection that connect()
    // has already initiated.
    #[tokio::test]
    async fn accept_side_classifies_relay_before_any_request() {
        let listener = TcpListener::bind("127.0.0.1:0").await.unwrap();
        let addr = listener.local_addr().unwrap().to_string();
        let set = empty_port_set();
        let set_dialer = set.clone();

        let dialer = tokio::spawn(async move {
            let (socket, remote, guard) =
                bind_and_register(&addr, Some(set_dialer)).expect("bind+register");
            let stream = socket.connect(remote).await.expect("connect");
            // Hold the connection (and its port registration) open.
            (stream, guard)
        });

        let (_srv, peer) = listener.accept().await.unwrap();
        // Mirror the enrollment accept-loop classification predicate.
        let is_relay_class = peer.ip().is_loopback() && set.lock().unwrap().contains(&peer.port());
        assert!(
            is_relay_class,
            "relay-routed peer {peer} must classify as RelayBridge at accept time"
        );

        let _held = dialer.await.unwrap();
    }
}
