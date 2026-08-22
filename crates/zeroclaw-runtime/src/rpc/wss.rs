//! WebSocket Secure (WSS) transport for the RPC layer.
//! Mirrors the Unix socket transport (`unix.rs`) but uses TLS-encrypted
//! WebSocket connections, enabling remote TUI-to-daemon connectivity.

use super::context::RpcContext;
use super::dispatch::RpcDispatcher;
use super::transport::RpcTransport;
use anyhow::{Context, Result};
use async_trait::async_trait;
use futures_util::{SinkExt, StreamExt};
use std::io::ErrorKind;
use std::net::SocketAddr;
use std::sync::Arc;
use std::sync::atomic::{AtomicUsize, Ordering};
use std::time::Duration;
use tokio::net::{TcpListener, TcpStream};
use tokio::sync::mpsc;
use tokio_rustls::TlsAcceptor;
use tokio_tungstenite::WebSocketStream;
use tokio_tungstenite::tungstenite::Message;
use tokio_util::sync::CancellationToken;

type TlsStream = tokio_rustls::server::TlsStream<TcpStream>;

/// How long the read side waits for any frame before sending a liveness Ping.
const HEARTBEAT_IDLE: Duration = Duration::from_secs(20);

/// How long to wait after a Ping for any frame (a Pong, or anything else)
/// before declaring the peer dead and tearing the connection down.
const HEARTBEAT_TIMEOUT: Duration = Duration::from_secs(10);

/// Backoff after a transient `accept()` error so the serve loop does not
/// hot-spin while the condition (e.g. fd exhaustion) clears.
const ACCEPT_ERROR_BACKOFF_MS: u64 = 50;

/// Default ceiling on sockets past `accept()` but not yet through the TLS and
/// WebSocket handshakes. See [`WssLimits::max_pending_handshakes`].
pub const DEFAULT_MAX_PENDING_HANDSHAKES: usize = 256;

/// Default absolute budget for TLS accept plus the WebSocket upgrade.
/// See [`WssLimits::handshake_timeout`].
pub const DEFAULT_HANDSHAKE_TIMEOUT_SECS: u64 = 10;

/// Default ceiling on concurrently established WSS sessions.
/// See [`WssLimits::max_sessions`].
pub const DEFAULT_MAX_SESSIONS: usize = 512;

/// Bounds on the WSS listener's pre-authentication and session state.
///
/// The remote WSS plane is the daemon's mandatory mTLS surface and its default
/// bind is `0.0.0.0`, so every state an *unauthenticated* peer can reach has to
/// be bounded in both time and count. Without these, each accepted TCP socket
/// spawned a task that awaited the TLS handshake and the WebSocket upgrade with
/// no deadline and no cap, so a peer that merely connected - and never proved
/// anything - could accumulate sockets, tasks and TLS parser state without
/// limit. Mirrors the bounds the relay applies to its own admission path.
#[derive(Debug, Clone)]
pub struct WssLimits {
    /// Ceiling on sockets past `accept()` that have not finished the TLS
    /// handshake and WebSocket upgrade. When the pool is exhausted new sockets
    /// are dropped at accept rather than queued, so a slowloris spread across
    /// many source addresses sheds instead of accumulating.
    pub max_pending_handshakes: usize,
    /// One absolute deadline covering TLS accept AND the WebSocket upgrade,
    /// measured from accept. It is a single budget for the whole setup
    /// sequence, not a fresh window per phase: the heartbeat only starts once
    /// a session is established, so without this a peer could stall in either
    /// handshake forever.
    pub handshake_timeout: Duration,
    /// Ceiling on concurrently established WSS sessions. Bounds the steady
    /// state that survives authentication, so an authorized-but-abusive peer
    /// cannot grow dispatcher and transport state without limit.
    pub max_sessions: usize,
}

impl Default for WssLimits {
    fn default() -> Self {
        Self {
            max_pending_handshakes: DEFAULT_MAX_PENDING_HANDSHAKES,
            handshake_timeout: Duration::from_secs(DEFAULT_HANDSHAKE_TIMEOUT_SECS),
            max_sessions: DEFAULT_MAX_SESSIONS,
        }
    }
}

/// Decrements the shared client counter on every exit path of a connection
/// task. The counter drives `--ephemeral` shutdown, so a missed decrement
/// would keep an idle daemon alive forever.
struct ClientCountGuard(Arc<AtomicUsize>);

impl Drop for ClientCountGuard {
    fn drop(&mut self) {
        self.0.fetch_sub(1, Ordering::Relaxed);
    }
}

/// File-descriptor exhaustion errno values, stable across the Unix targets
/// we support (Linux, macOS, BSD).
#[cfg(unix)]
const EMFILE: i32 = 24; // too many open files (this process)
#[cfg(unix)]
const ENFILE: i32 = 23; // too many open files (system-wide)

fn is_recoverable_accept_error(e: &std::io::Error) -> bool {
    if matches!(
        e.kind(),
        ErrorKind::ConnectionAborted | ErrorKind::Interrupted | ErrorKind::WouldBlock
    ) {
        return true;
    }
    #[cfg(unix)]
    if matches!(e.raw_os_error(), Some(EMFILE) | Some(ENFILE)) {
        return true;
    }
    false
}

// ── Transport ────────────────────────────────────────────────────

/// Control frames the read side asks the writer task to emit out-of-band
/// from the JSON-RPC text stream.
enum Control {
    Ping,
}

pub struct WssTransport {
    reader: futures_util::stream::SplitStream<WebSocketStream<TlsStream>>,
    writer_tx: mpsc::Sender<String>,
    control_tx: mpsc::Sender<Control>,
    peer_label: String,
    /// Set once a Ping has been sent and we are awaiting any reply. Detects a
    /// peer that went silent on a half-open TCP connection (no FIN/RST).
    awaiting_pong: bool,
}

impl WssTransport {
    pub fn new(ws: WebSocketStream<TlsStream>, remote_addr: SocketAddr) -> Self {
        let peer_label = format!("wss:{remote_addr}");
        let (sink, stream) = ws.split();

        let (writer_tx, mut writer_rx) = mpsc::channel::<String>(64);
        let (control_tx, mut control_rx) = mpsc::channel::<Control>(8);
        zeroclaw_spawn::spawn!(async move {
            let mut sink = sink;
            loop {
                let msg = tokio::select! {
                    line = writer_rx.recv() => match line {
                        Some(line) => Message::Text(line.into()),
                        None => break,
                    },
                    ctrl = control_rx.recv() => match ctrl {
                        Some(Control::Ping) => Message::Ping(Vec::new().into()),
                        None => break,
                    },
                };
                if sink.send(msg).await.is_err() {
                    break;
                }
            }
        });

        Self {
            reader: stream,
            writer_tx,
            control_tx,
            peer_label,
            awaiting_pong: false,
        }
    }
}

#[async_trait]
impl RpcTransport for WssTransport {
    fn writer(&self) -> mpsc::Sender<String> {
        self.writer_tx.clone()
    }

    async fn next_frame(&mut self) -> Option<String> {
        loop {
            let idle = if self.awaiting_pong {
                HEARTBEAT_TIMEOUT
            } else {
                HEARTBEAT_IDLE
            };

            match tokio::time::timeout(idle, self.reader.next()).await {
                Err(_) if self.awaiting_pong => return None,
                Err(_) => {
                    if self.control_tx.send(Control::Ping).await.is_err() {
                        return None;
                    }
                    self.awaiting_pong = true;
                }
                Ok(frame) => {
                    self.awaiting_pong = false;
                    match frame {
                        Some(Ok(Message::Text(text))) => return Some(text.to_string()),
                        Some(Ok(Message::Close(_))) | None => return None,
                        Some(Ok(Message::Ping(_) | Message::Pong(_) | Message::Frame(_))) => {
                            continue;
                        }
                        Some(Ok(Message::Binary(_))) => continue,
                        Some(Err(_)) => return None,
                    }
                }
            }
        }
    }

    fn peer_label(&self) -> String {
        self.peer_label.clone()
    }
}

// ── TLS acceptor ─────────────────────────────────────────────────

/// Build a [`TlsAcceptor`] for the remote WSS RPC plane.
///
/// The remote plane is ALWAYS mutually authenticated and TLS 1.3 only: every
/// client certificate is verified against `ca_cert_path` (optionally pinned to
/// `pinned_certs`). There is deliberately no server-only / no-client-auth path
/// here (threat model A11); the secure-by-construction builder lives in
/// [`zeroclaw_tls::build_mtls_acceptor`].
pub fn build_tls_acceptor(
    cert_path: &str,
    key_path: &str,
    ca_cert_path: &str,
    pinned_certs: &[String],
    crl_path: &str,
) -> Result<TlsAcceptor> {
    zeroclaw_tls::build_mtls_acceptor(cert_path, key_path, ca_cert_path, pinned_certs, crl_path)
}

// ── Listener ─────────────────────────────────────────────────────

/// Run the WSS RPC listener as a daemon subsystem.
/// `client_count` is incremented on connect, decremented on disconnect —
/// shared with the Unix socket listener for `--ephemeral` shutdown logic.
pub async fn run_wss_listener(
    ctx: Arc<RpcContext>,
    cancel: CancellationToken,
    client_count: Arc<AtomicUsize>,
    tls_acceptor: TlsAcceptor,
    bind_addr: SocketAddr,
    limits: WssLimits,
) -> Result<()> {
    let listener = TcpListener::bind(bind_addr)
        .await
        .with_context(|| format!("binding WSS listener on {bind_addr}"))?;

    // Bounds on unauthenticated setup work and on established sessions. A
    // permit is held from accept until the peer is through both handshakes;
    // the session permit is held for the life of the dispatcher.
    let handshake_permits = Arc::new(tokio::sync::Semaphore::new(
        limits.max_pending_handshakes.max(1),
    ));
    let session_permits = Arc::new(tokio::sync::Semaphore::new(limits.max_sessions.max(1)));

    ::zeroclaw_log::record!(
        INFO,
        ::zeroclaw_log::Event::new(module_path!(), ::zeroclaw_log::Action::Note)
            .with_attrs(::serde_json::json!({"addr": bind_addr.to_string()})),
        "RPC WSS listener started"
    );

    loop {
        tokio::select! {
            _ = cancel.cancelled() => {
                ::zeroclaw_log::record!(
                    INFO,
                    ::zeroclaw_log::Event::new(module_path!(), ::zeroclaw_log::Action::Note),
                    "RPC WSS listener shutting down"
                );
                break;
            }
            accept = listener.accept() => {
                let (tcp_stream, remote_addr) = match accept {
                    Ok(v) => v,
                    Err(e) => {
                        if is_recoverable_accept_error(&e) {
                            // Transient (e.g. EMFILE under fd pressure):
                            // the listener is still valid. Back off briefly
                            // to avoid hot-spinning, then keep serving
                            // rather than killing the daemon
                            ::zeroclaw_log::record!(
                                WARN,
                                ::zeroclaw_log::Event::new(module_path!(), ::zeroclaw_log::Action::Note)
                                    .with_outcome(::zeroclaw_log::EventOutcome::Unknown),
                                &format!("WSS accept() transient error: {e}")
                            );
                            tokio::time::sleep(Duration::from_millis(ACCEPT_ERROR_BACKOFF_MS)).await;
                            continue;
                        }
                        return Err(e).context("WSS accept error");
                    }
                };

                // Shed before spending any TLS/task state on this socket when
                // either budget is exhausted. Dropping the stream closes it, so
                // a client sees a prompt EOF instead of an indefinite stall.
                let Ok(handshake_permit) =
                    handshake_permits.clone().try_acquire_owned()
                else {
                    drop(tcp_stream);
                    ::zeroclaw_log::record!(
                        WARN,
                        ::zeroclaw_log::Event::new(module_path!(), ::zeroclaw_log::Action::Note)
                            .with_outcome(::zeroclaw_log::EventOutcome::Unknown),
                        &format!(
                            "WSS shedding connection from {remote_addr}: {} pending handshakes \
                             already in flight",
                            limits.max_pending_handshakes
                        )
                    );
                    continue;
                };
                let Ok(session_permit) = session_permits.clone().try_acquire_owned() else {
                    drop(tcp_stream);
                    ::zeroclaw_log::record!(
                        WARN,
                        ::zeroclaw_log::Event::new(module_path!(), ::zeroclaw_log::Action::Note)
                            .with_outcome(::zeroclaw_log::EventOutcome::Unknown),
                        &format!(
                            "WSS shedding connection from {remote_addr}: {} sessions already \
                             established",
                            limits.max_sessions
                        )
                    );
                    continue;
                };

                let ctx = ctx.clone();
                let count = client_count.clone();
                let acceptor = tls_acceptor.clone();
                let handshake_timeout = limits.handshake_timeout;

                count.fetch_add(1, Ordering::Relaxed);

                zeroclaw_spawn::spawn!(async move {
                    // Guarantees the `--ephemeral` counter is decremented on
                    // every exit path below, including the new timeout one.
                    let _count_guard = ClientCountGuard(count);
                    // Released only when the dispatcher finishes.
                    let _session_permit = session_permit;

                    // ONE absolute deadline over TLS accept AND the WebSocket
                    // upgrade, measured from accept. A fresh per-phase window
                    // would let a peer spend the full budget twice.
                    let deadline = tokio::time::Instant::now() + handshake_timeout;

                    let setup = async {
                    // TLS handshake.
                    let tls_stream = match acceptor.accept(tcp_stream).await {
                        Ok(s) => s,
                        Err(e) => {
                            // The WSS plane is always mutually authenticated, so a
                            // client with no certificate (un-migrated) or a revoked
                            // one fails here. Surface it actionably rather than as a
                            // bare TLS error so the operator knows to enroll it.
                            ::zeroclaw_log::record!(
                                WARN,
                                ::zeroclaw_log::Event::new(module_path!(), ::zeroclaw_log::Action::Note)
                                    .with_outcome(::zeroclaw_log::EventOutcome::Unknown),
                                &format!(
                                    "WSS TLS handshake failed from {remote_addr}: {e}. The WSS plane \
                                     requires a client certificate; an un-migrated client must enroll \
                                     first (zerocode --enroll), and a revoked cert is refused."
                                )
                            );
                            return None;
                        }
                    };

                    // WebSocket upgrade.
                    let ws_stream = match tokio_tungstenite::accept_async(tls_stream).await {
                        Ok(ws) => ws,
                        Err(e) => {
                            ::zeroclaw_log::record!(
                                WARN,
                                ::zeroclaw_log::Event::new(module_path!(), ::zeroclaw_log::Action::Note)
                                    .with_outcome(::zeroclaw_log::EventOutcome::Unknown),
                                &format!("WSS WebSocket upgrade failed from {remote_addr}: {e}")
                            );
                            return None;
                        }
                    };
                        Some(ws_stream)
                    };

                    let ws_stream = match tokio::time::timeout_at(deadline, setup).await {
                        Ok(Some(ws)) => ws,
                        Ok(None) => return, // logged above
                        Err(_) => {
                            ::zeroclaw_log::record!(
                                WARN,
                                ::zeroclaw_log::Event::new(module_path!(), ::zeroclaw_log::Action::Note)
                                    .with_outcome(::zeroclaw_log::EventOutcome::Unknown),
                                &format!(
                                    "WSS setup from {remote_addr} exceeded the {}s handshake \
                                     budget; connection dropped",
                                    handshake_timeout.as_secs()
                                )
                            );
                            return;
                        }
                    };

                    // Through both handshakes: this peer presented a valid
                    // client certificate, so release the pre-authentication
                    // permit for the next connection being set up.
                    drop(handshake_permit);

                    // The client cert was verified against the CA during the
                    // mTLS handshake; capture its SHA-256 fingerprint (the ledger
                    // key) before the stream is consumed by the transport.
                    let peer_cert_fp = ws_stream
                        .get_ref()
                        .get_ref()
                        .1
                        .peer_certificates()
                        .and_then(|certs| certs.first())
                        .map(|der| zeroclaw_tls::cert_sha256_fingerprint(der.as_ref()));

                    let mut transport = WssTransport::new(ws_stream, remote_addr);
                    let peer = transport.peer_label();
                    let writer_tx = transport.writer();
                    let mut dispatcher = RpcDispatcher::new(ctx.clone(), writer_tx, peer)
                        .with_peer_cert_fingerprint(peer_cert_fp);
                    dispatcher.run(&mut transport).await;

                    if let Some(tui_id) = dispatcher.tui_id() {
                        ctx.tui_registry.unregister(tui_id);
                        use ::zeroclaw_log::Instrument as _;
                        let span = ::zeroclaw_log::info_span!(
                            target: "zeroclaw_log_internal_scope",
                            "zeroclaw_scope",
                            owner_tui_id = %tui_id,
                            channel = "wss",
                        );
                        async {
                            ::zeroclaw_log::record!(
                                INFO,
                                ::zeroclaw_log::Event::new(module_path!(), ::zeroclaw_log::Action::Note)
                                    .with_category(::zeroclaw_log::EventCategory::Agent),
                                "WSS TUI disconnected; sessions retained (persistent)"
                            );
                        }
                        .instrument(span)
                        .await;
                    }
                });
            }
        }
    }

    Ok(())
}

#[cfg(test)]
mod accept_error_tests {
    use super::is_recoverable_accept_error;
    use std::io::{Error, ErrorKind};

    #[cfg(unix)]
    #[test]
    fn fd_exhaustion_accept_errors_are_recoverable() {
        // EMFILE/ENFILE must not terminate the daemon.
        assert!(is_recoverable_accept_error(&Error::from_raw_os_error(24))); // EMFILE
        assert!(is_recoverable_accept_error(&Error::from_raw_os_error(23))); // ENFILE
    }

    #[test]
    fn transient_kinds_recover_but_fatal_propagates() {
        assert!(is_recoverable_accept_error(&Error::from(
            ErrorKind::ConnectionAborted
        )));
        assert!(is_recoverable_accept_error(&Error::from(
            ErrorKind::Interrupted
        )));
        // A non-transient error is not swallowed (loop will propagate it).
        assert!(!is_recoverable_accept_error(&Error::from(
            ErrorKind::InvalidInput
        )));
    }
}
