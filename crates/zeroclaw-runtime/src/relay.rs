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
use std::time::{Duration, Instant};
use tokio::io::{AsyncReadExt, AsyncWriteExt};
use tokio::net::TcpStream;
use tokio::sync::{Mutex, mpsc, oneshot};
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

const BACKOFF_INITIAL: Duration = Duration::from_millis(500);
const BACKOFF_MAX: Duration = Duration::from_secs(30);
/// A session up at least this long resets the backoff (transient drop).
const ESTABLISHED: Duration = Duration::from_secs(5);
/// WS keepalive cadence (below common NAT idle windows).
const KEEPALIVE: Duration = Duration::from_secs(20);
/// Declare the link dead if nothing has been heard for this long.
const DEAD_AFTER: Duration = Duration::from_secs(60);
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
    let (mut ws, _resp) = tokio_tungstenite::client_async(request, tls)
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

    // Connection bookkeeping + the single outbound write path to the relay.
    let (to_relay, mut from_tasks) = mpsc::channel::<tokio_tungstenite::tungstenite::Message>(256);
    let conns: Arc<Mutex<HashMap<u64, mpsc::Sender<ConnMsg>>>> =
        Arc::new(Mutex::new(HashMap::new()));
    let last_seen = Arc::new(Mutex::new(Instant::now()));
    let link_dead = CancellationToken::new();

    let (mut sink, mut stream) = ws.split();
    let writer = zeroclaw_spawn::spawn!(async move {
        while let Some(msg) = from_tasks.recv().await {
            if sink.send(msg).await.is_err() {
                break;
            }
        }
        let _ = sink.close().await;
    });

    // Keepalive watchdog: ping below NAT timeout, declare dead on silence.
    {
        let to_relay = to_relay.clone();
        let last_seen = last_seen.clone();
        let link_dead = link_dead.clone();
        zeroclaw_spawn::spawn!(async move {
            let mut tick = tokio::time::interval(KEEPALIVE);
            tick.tick().await; // immediate first tick; skip
            loop {
                tokio::select! {
                    _ = link_dead.cancelled() => break,
                    _ = tick.tick() => {
                        if to_relay
                            .send(tokio_tungstenite::tungstenite::Message::Ping(Vec::new().into()))
                            .await
                            .is_err()
                        {
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
        });
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
                                    let _ = to_relay
                                        .send(tungstenite_text(&Control::Close {
                                            conn_id,
                                            reason: "rate_limited".into(),
                                        }))
                                        .await;
                                    continue;
                                }
                                let local = match open_route_target(cfg, peer_hint.as_deref()) {
                                    Ok(addr) => addr.to_string(),
                                    Err(reason) => {
                                        let _ = to_relay
                                            .send(tungstenite_text(&Control::Close {
                                                conn_id,
                                                reason: reason.into(),
                                            }))
                                            .await;
                                        continue;
                                    }
                                };
                                let mut cs = conns.lock().await;
                                if cs.len() >= cfg.max_conns {
                                    drop(cs);
                                    let _ = to_relay
                                        .send(tungstenite_text(&Control::Close {
                                            conn_id,
                                            reason: "busy".into(),
                                        }))
                                        .await;
                                } else {
                                    let (tx, rx) = mpsc::channel::<ConnMsg>(256);
                                    cs.insert(conn_id, tx);
                                    drop(cs);
                                    let to_relay = to_relay.clone();
                                    let link_dead = link_dead.clone();
                                    let conns = conns.clone();
                                    zeroclaw_spawn::spawn!(async move {
                                        bridge_conn(conn_id, &local, to_relay, rx, link_dead, conns)
                                            .await;
                                    });
                                }
                            }
                            Ok(Control::Close { conn_id, .. }) => {
                                conns.lock().await.remove(&conn_id);
                            }
                            // Credit-window frames from the client (forwarded by
                            // the relay): route to the conn's bridge task.
                            Ok(Control::Window { conn_id, credit }) => {
                                if let Some(tx) = conns.lock().await.get(&conn_id) {
                                    let _ = tx.send(ConnMsg::Window(credit)).await;
                                }
                            }
                            Ok(Control::DataAck { conn_id, consumed }) => {
                                if let Some(tx) = conns.lock().await.get(&conn_id) {
                                    let _ = tx.send(ConnMsg::Ack(consumed)).await;
                                }
                            }
                            _ => {}
                        }
                    }
                    Ok(tokio_tungstenite::tungstenite::Message::Binary(b)) => {
                        if let Some((conn_id, payload)) = decode_data(&b)
                            && let Some(tx) = conns.lock().await.get(&conn_id) {
                                let _ = tx.send(ConnMsg::Data(payload.to_vec())).await;
                            }
                    }
                    Ok(tokio_tungstenite::tungstenite::Message::Ping(p)) => {
                        let _ = to_relay
                            .send(tokio_tungstenite::tungstenite::Message::Pong(p))
                            .await;
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

fn open_route_target<'a>(
    cfg: &'a RelayBridgeConfig,
    peer_hint: Option<&str>,
) -> std::result::Result<&'a str, &'static str> {
    match peer_hint {
        Some(PEER_HINT_ENROLL) => cfg.local_enroll_addr.as_deref().ok_or("enroll_unavailable"),
        _ => Ok(&cfg.local_wss_addr),
    }
}

/// Bridge one logical connection: dial the selected loopback listener, accept the
/// `Open`, and shuttle bytes both ways until either side ends.
async fn bridge_conn(
    conn_id: u64,
    local_addr: &str,
    to_relay: mpsc::Sender<tokio_tungstenite::tungstenite::Message>,
    mut inbound: mpsc::Receiver<ConnMsg>,
    link_dead: CancellationToken,
    conns: Arc<Mutex<HashMap<u64, mpsc::Sender<ConnMsg>>>>,
) {
    let local = match TcpStream::connect(local_addr).await {
        Ok(s) => s,
        Err(_) => {
            let _ = to_relay
                .send(tungstenite_text(&Control::Close {
                    conn_id,
                    reason: "bridge_dial_failed".into(),
                }))
                .await;
            conns.lock().await.remove(&conn_id);
            return;
        }
    };
    // Accept the connection to the relay (it tells the waiting client).
    let _ = to_relay
        .send(tungstenite_text(&Control::Opened { conn_id }))
        .await;
    // Grant the client our receive window for this conn up front.
    let _ = to_relay
        .send(tungstenite_text(&Control::Window {
            conn_id,
            credit: INITIAL_WINDOW,
        }))
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
            _ = link_dead.cancelled() => break,
            // Pause reading the loopback when the send window is exhausted, until
            // a DataAck replenishes it.
            n = lr.read(&mut buf), if !send_window.is_blocked() => match n {
                Ok(0) | Err(_) => break,
                Ok(n) => {
                    send_window.debit(n);
                    if to_relay
                        .send(tokio_tungstenite::tungstenite::Message::binary(encode_data(conn_id, &buf[..n])))
                        .await
                        .is_err()
                    {
                        break;
                    }
                }
            },
            msg = inbound.recv() => match msg {
                Some(ConnMsg::Data(p)) => {
                    if lw.write_all(&p).await.is_err() {
                        break;
                    }
                    recv_drained = recv_drained.saturating_add(p.len() as u32);
                    if recv_drained >= INITIAL_WINDOW / 2 {
                        let _ = to_relay
                            .send(tungstenite_text(&Control::DataAck {
                                conn_id,
                                consumed: recv_drained,
                            }))
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
    let _ = to_relay
        .send(tungstenite_text(&Control::Close {
            conn_id,
            reason: "bridge_closed".into(),
        }))
        .await;
    conns.lock().await.remove(&conn_id);
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
        assert_eq!(open_route_target(&cfg, None).unwrap(), "127.0.0.1:9781");
        assert_eq!(
            open_route_target(&cfg, Some("unknown")).unwrap(),
            "127.0.0.1:9781"
        );
    }

    #[test]
    fn open_route_selects_enrollment_when_available() {
        let cfg = route_test_config();
        assert_eq!(
            open_route_target(&cfg, Some(PEER_HINT_ENROLL)).unwrap(),
            "127.0.0.1:9782"
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
