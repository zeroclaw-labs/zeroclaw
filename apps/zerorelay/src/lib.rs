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

use anyhow::Result;
use base64::Engine as _;
use base64::engine::general_purpose::STANDARD as B64;
use futures_util::{SinkExt, StreamExt};
use ring::rand::{SecureRandom, SystemRandom};
use ring::signature::{ED25519, UnparsedPublicKey};
use sha2::{Digest, Sha256};
use std::collections::{HashMap, HashSet};
use std::sync::Arc;
use std::sync::atomic::{AtomicU64, Ordering};
use std::time::Duration;
use tokio::io::{AsyncRead, AsyncWrite};
use tokio::net::TcpListener;
use tokio::sync::{Mutex, mpsc};
use tokio_rustls::TlsAcceptor;
use tokio_tungstenite::WebSocketStream;
use tokio_tungstenite::tungstenite::Message;
use zeroclaw_relay_proto::{Control, MAX_DATA_PAYLOAD, SUBPROTOCOL, decode_data, encode_data};

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
}

struct Inner {
    cfg: RelayConfig,
    daemons: Mutex<HashMap<String, DaemonHandle>>,
    next_conn: AtomicU64,
    next_epoch: AtomicU64,
}

impl Inner {
    fn admit(&self, fpr: &str) -> bool {
        if self.cfg.deny.contains(fpr) {
            return false;
        }
        match self.cfg.registration_mode {
            Admission::Open => true,
            Admission::Allowlist => self.cfg.allow.contains(fpr),
        }
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
                cfg,
                daemons: Mutex::new(HashMap::new()),
                next_conn: AtomicU64::new(1),
                next_epoch: AtomicU64::new(1),
            }),
        }
    }

    /// Accept TLS + WebSocket connections forever, dispatching daemon vs client.
    pub async fn serve(self, listener: TcpListener, acceptor: TlsAcceptor) -> Result<()> {
        loop {
            let (sock, _peer) = match listener.accept().await {
                Ok(s) => s,
                Err(_) => continue,
            };
            let inner = self.inner.clone();
            let acceptor = acceptor.clone();
            tokio::spawn(async move {
                let tls = match acceptor.accept(sock).await {
                    Ok(t) => t,
                    Err(_) => return,
                };
                // Echo our subprotocol so a client that offered it does not fail
                // the handshake (RFC 6455 / tungstenite enforces this on the client).
                let ws = match tokio_tungstenite::accept_hdr_async(tls, select_subprotocol).await {
                    Ok(w) => w,
                    Err(_) => return,
                };
                let _ = handle_conn(inner, ws).await;
            });
        }
    }
}

/// WebSocket handshake callback: select `zeroclaw.relay.v1` in the response when
/// the client offered it, so the client's required-subprotocol check passes.
// The Result type is dictated by tungstenite's `accept_hdr_async` callback trait
// (its error variant carries an http Response); we cannot box it.
#[allow(clippy::result_large_err)]
fn select_subprotocol(
    req: &tokio_tungstenite::tungstenite::handshake::server::Request,
    mut resp: tokio_tungstenite::tungstenite::handshake::server::Response,
) -> std::result::Result<
    tokio_tungstenite::tungstenite::handshake::server::Response,
    tokio_tungstenite::tungstenite::handshake::server::ErrorResponse,
> {
    let offered = req
        .headers()
        .get_all("Sec-WebSocket-Protocol")
        .iter()
        .any(|v| {
            v.to_str()
                .map(|s| s.split(',').any(|p| p.trim() == SUBPROTOCOL))
                .unwrap_or(false)
        });
    if offered {
        resp.headers_mut().insert(
            "Sec-WebSocket-Protocol",
            tokio_tungstenite::tungstenite::http::HeaderValue::from_static(SUBPROTOCOL),
        );
    }
    Ok(resp)
}

/// Read the first control frame and dispatch by role.
async fn handle_conn<S>(inner: Arc<Inner>, mut ws: WebSocketStream<S>) -> Result<()>
where
    S: AsyncRead + AsyncWrite + Unpin + Send + 'static,
{
    match next_control(&mut ws).await {
        Some(Control::Hello {
            daemon_pubkey,
            node_id,
            relay_token,
        }) => handle_daemon(inner, ws, daemon_pubkey, node_id, relay_token).await,
        Some(Control::Connect { node_id }) => handle_client(inner, ws, node_id).await,
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
    // Optional shared-secret gate.
    if let Some(required) = &inner.cfg.relay_token
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
    if !inner.admit(&fpr) {
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
            },
        );
    }
    send_control(
        &mut ws,
        &Control::Registered {
            node_id: node_id.clone(),
            lease_ttl_secs: inner.cfg.lease_ttl.as_secs(),
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
) -> Result<()>
where
    S: AsyncRead + AsyncWrite + Unpin + Send + 'static,
{
    let (to_daemon, conns) = {
        let daemons = inner.daemons.lock().await;
        match daemons.get(&node_id) {
            Some(h) => (h.to_daemon.clone(), h.conns.clone()),
            None => {
                let _ = send_control(&mut ws, &Control::error("no_such_node", node_id)).await;
                return Ok(());
            }
        }
    };

    let conn_id = inner.next_conn.fetch_add(1, Ordering::Relaxed);
    let (conn_tx, mut conn_rx) = mpsc::channel::<ConnEvent>(256);
    {
        let mut cs = conns.lock().await;
        if cs.len() >= inner.cfg.max_conns_per_node {
            drop(cs);
            let _ = send_control(&mut ws, &Control::error("busy", "node at capacity")).await;
            return Ok(());
        }
        cs.insert(conn_id, conn_tx);
    }

    // Ask the daemon to open the logical connection.
    if to_daemon
        .send(Message::text(
            Control::Open {
                conn_id,
                peer_hint: None,
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
                ConnEvent::Data(_) => {} // shouldn't precede Opened; ignore
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

    // Pump bytes both ways until either side closes or the conn goes idle.
    let idle = inner.cfg.idle_timeout;
    loop {
        let deadline = tokio::time::Instant::now() + idle;
        tokio::select! {
            _ = tokio::time::sleep_until(deadline) => break,
            ev = conn_rx.recv() => match ev {
                Some(ConnEvent::Data(payload)) => {
                    if sink.send(Message::binary(encode_data(conn_id, &payload))).await.is_err() {
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
                    if to_daemon
                        .send(Message::binary(encode_data(conn_id, payload)))
                        .await
                        .is_err()
                    {
                        break;
                    }
                }
                Some(Ok(Message::Text(t))) => {
                    if let Ok(Control::Close { .. }) = Control::from_json(&t) {
                        break;
                    }
                }
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
