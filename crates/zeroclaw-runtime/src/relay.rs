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
use tokio::sync::{Mutex, mpsc};
use tokio_rustls::TlsConnector;
// The runtime depends on tokio-rustls (not rustls directly); use its re-export.
use tokio_rustls::rustls;
use tokio_util::sync::CancellationToken;
use zeroclaw_relay_proto::{Control, MAX_DATA_PAYLOAD, SUBPROTOCOL, decode_data, encode_data};

const BACKOFF_INITIAL: Duration = Duration::from_millis(500);
const BACKOFF_MAX: Duration = Duration::from_secs(30);
/// A session up at least this long resets the backoff (transient drop).
const ESTABLISHED: Duration = Duration::from_secs(5);
/// WS keepalive cadence (below common NAT idle windows).
const KEEPALIVE: Duration = Duration::from_secs(20);
/// Declare the link dead if nothing has been heard for this long.
const DEAD_AFTER: Duration = Duration::from_secs(60);

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
    /// PKCS#8 of the daemon's Ed25519 registration key.
    pub signing_key_pkcs8: Vec<u8>,
    /// PEM CA to trust for the relay's outer cert; `None` uses public roots.
    pub relay_ca_path: Option<String>,
    /// Skip relay outer-cert verification (test only).
    pub relay_insecure: bool,
    /// Cap on simultaneously-bridged client connections (bridge-side DoS cap).
    pub max_conns: usize,
}

/// Load (or create + persist) the daemon's Ed25519 relay-registration key.
///
/// Stored as raw PKCS#8 DER at `<data_dir>/relay/registration.key` (dir 0700,
/// key 0600). This is the daemon's stable rendezvous identity, separate from the
/// ZeroClaw CA: the relay binds the node-id to this key and an allowlist keys on
/// its fingerprint.
pub fn ensure_signing_key(data_dir: &std::path::Path) -> Result<Vec<u8>> {
    use std::io::Write;
    use std::os::unix::fs::{OpenOptionsExt, PermissionsExt};

    let dir = data_dir.join("relay");
    let path = dir.join("registration.key");
    if let Ok(bytes) = std::fs::read(&path)
        && !bytes.is_empty()
    {
        return Ok(bytes);
    }
    std::fs::create_dir_all(&dir).with_context(|| format!("creating {}", dir.display()))?;
    let _ = std::fs::set_permissions(&dir, std::fs::Permissions::from_mode(0o700));
    let rng = ring::rand::SystemRandom::new();
    let pkcs8 = Ed25519KeyPair::generate_pkcs8(&rng)
        .map_err(|e| anyhow::Error::msg(format!("generating relay signing key: {e}")))?;
    let mut f = std::fs::OpenOptions::new()
        .write(true)
        .create(true)
        .truncate(true)
        .mode(0o600)
        .open(&path)
        .with_context(|| format!("writing {}", path.display()))?;
    f.write_all(pkcs8.as_ref())
        .context("writing relay signing key")?;
    Ok(pkcs8.as_ref().to_vec())
}

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
    let dir = data_dir.join("relay");
    let path = dir.join("node_id");
    if let Ok(existing) = std::fs::read_to_string(&path) {
        let existing = existing.trim().to_string();
        if !existing.is_empty() {
            return Ok(existing);
        }
    }
    use ring::rand::SecureRandom;
    let mut bytes = [0u8; 16];
    ring::rand::SystemRandom::new()
        .fill(&mut bytes)
        .map_err(|e| anyhow::Error::msg(format!("generating node_id: {e}")))?;
    let id = hex::encode(bytes);
    std::fs::create_dir_all(&dir).with_context(|| format!("creating {}", dir.display()))?;
    #[cfg(unix)]
    {
        use std::os::unix::fs::PermissionsExt;
        let _ = std::fs::set_permissions(&dir, std::fs::Permissions::from_mode(0o700));
    }
    std::fs::write(&path, &id).with_context(|| format!("writing {}", path.display()))?;
    Ok(id)
}

/// Run the relay bridge until `cancel` fires, reconnecting with backoff.
pub async fn run_relay_bridge(cfg: RelayBridgeConfig, cancel: CancellationToken) -> Result<()> {
    let mut backoff = BACKOFF_INITIAL;
    loop {
        if cancel.is_cancelled() {
            return Ok(());
        }
        let started = Instant::now();
        match serve_once(&cfg, &cancel).await {
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

async fn serve_once(cfg: &RelayBridgeConfig, cancel: &CancellationToken) -> Result<()> {
    let keypair = Ed25519KeyPair::from_pkcs8(&cfg.signing_key_pkcs8)
        .map_err(|e| anyhow::Error::msg(format!("loading relay signing key: {e}")))?;
    let pubkey = keypair.public_key().as_ref().to_vec();

    // Outer TLS + WS to the relay.
    let tls_config = relay_client_config(cfg.relay_ca_path.as_deref(), cfg.relay_insecure)?;
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
        Some(Control::Registered { .. }) => {}
        Some(Control::Error { code, msg }) => {
            anyhow::bail!("relay rejected registration: {code}: {msg}")
        }
        other => anyhow::bail!("unexpected relay reply to register: {other:?}"),
    }

    // Connection bookkeeping + the single outbound write path to the relay.
    let (to_relay, mut from_tasks) = mpsc::channel::<tokio_tungstenite::tungstenite::Message>(256);
    let conns: Arc<Mutex<HashMap<u64, mpsc::Sender<Vec<u8>>>>> =
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
                            Ok(Control::Open { conn_id, .. }) => {
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
                                    let (tx, rx) = mpsc::channel::<Vec<u8>>(256);
                                    cs.insert(conn_id, tx);
                                    drop(cs);
                                    let to_relay = to_relay.clone();
                                    let local = cfg.local_wss_addr.clone();
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
                            _ => {}
                        }
                    }
                    Ok(tokio_tungstenite::tungstenite::Message::Binary(b)) => {
                        if let Some((conn_id, payload)) = decode_data(&b)
                            && let Some(tx) = conns.lock().await.get(&conn_id) {
                                let _ = tx.send(payload.to_vec()).await;
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

/// Bridge one logical connection: dial the loopback WSS listener, accept the
/// `Open`, and shuttle bytes both ways until either side ends.
async fn bridge_conn(
    conn_id: u64,
    local_wss_addr: &str,
    to_relay: mpsc::Sender<tokio_tungstenite::tungstenite::Message>,
    mut inbound: mpsc::Receiver<Vec<u8>>,
    link_dead: CancellationToken,
    conns: Arc<Mutex<HashMap<u64, mpsc::Sender<Vec<u8>>>>>,
) {
    let local = match TcpStream::connect(local_wss_addr).await {
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

    let (mut lr, mut lw) = local.into_split();
    let mut buf = vec![0u8; MAX_DATA_PAYLOAD];
    loop {
        tokio::select! {
            _ = link_dead.cancelled() => break,
            n = lr.read(&mut buf) => match n {
                Ok(0) | Err(_) => break,
                Ok(n) => {
                    if to_relay
                        .send(tokio_tungstenite::tungstenite::Message::binary(encode_data(conn_id, &buf[..n])))
                        .await
                        .is_err()
                    {
                        break;
                    }
                }
            },
            payload = inbound.recv() => match payload {
                Some(p) => {
                    if lw.write_all(&p).await.is_err() {
                        break;
                    }
                }
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
            Ok(tokio_tungstenite::tungstenite::Message::Text(t)) => {
                return Control::from_json(t.as_str()).ok();
            }
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

/// Build the client TLS config used to verify the relay's outer certificate.
fn relay_client_config(ca_path: Option<&str>, insecure: bool) -> Result<Arc<rustls::ClientConfig>> {
    let builder = rustls::ClientConfig::builder_with_provider(Arc::new(
        rustls::crypto::ring::default_provider(),
    ))
    .with_safe_default_protocol_versions()
    .context("ring provider supports default protocol versions")?;

    let config = if insecure {
        builder
            .dangerous()
            .with_custom_certificate_verifier(Arc::new(NoVerify))
            .with_no_client_auth()
    } else if let Some(ca) = ca_path {
        let mut roots = rustls::RootCertStore::empty();
        for cert in zeroclaw_tls::load_certs(ca)? {
            roots.add(cert).context("adding relay CA to root store")?;
        }
        builder.with_root_certificates(roots).with_no_client_auth()
    } else {
        let mut roots = rustls::RootCertStore::empty();
        roots.extend(webpki_roots::TLS_SERVER_ROOTS.iter().cloned());
        builder.with_root_certificates(roots).with_no_client_auth()
    };
    Ok(Arc::new(config))
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
