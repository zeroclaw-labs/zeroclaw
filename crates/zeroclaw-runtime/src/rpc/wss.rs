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

/// Build a `TlsAcceptor` from PEM-encoded cert and key files.
pub fn build_tls_acceptor(cert_path: &str, key_path: &str) -> Result<TlsAcceptor> {
    use rustls::ServerConfig;
    use rustls_pemfile::{certs, private_key};
    use std::fs::File;
    use std::io::BufReader;

    let cert_file =
        File::open(cert_path).with_context(|| format!("opening TLS cert: {cert_path}"))?;
    let key_file = File::open(key_path).with_context(|| format!("opening TLS key: {key_path}"))?;

    let certs: Vec<_> = certs(&mut BufReader::new(cert_file))
        .collect::<Result<Vec<_>, _>>()
        .context("parsing TLS certificates")?;

    let key = private_key(&mut BufReader::new(key_file))
        .context("parsing TLS private key")?
        .context("no private key found in key file")?;

    let config = ServerConfig::builder()
        .with_no_client_auth()
        .with_single_cert(certs, key)
        .context("building TLS server config")?;

    Ok(TlsAcceptor::from(Arc::new(config)))
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
) -> Result<()> {
    let listener = TcpListener::bind(bind_addr)
        .await
        .with_context(|| format!("binding WSS listener on {bind_addr}"))?;

    ::zeroclaw_log::record!(
        INFO,
        ::zeroclaw_log::Event::new(module_path!(), ::zeroclaw_log::Action::Note)
            .with_attrs(::serde_json::json!({"addr": bind_addr.to_string()})),
        "RPC WSS listener started"
    );

    let connections_cancel = cancel.child_token();
    let mut connections = tokio::task::JoinSet::new();
    let mut listener_result: Result<()> = Ok(());
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
            completed = connections.join_next(), if !connections.is_empty() => {
                if let Some(Err(error)) = completed {
                    ::zeroclaw_log::record!(
                        WARN,
                        ::zeroclaw_log::Event::new(module_path!(), ::zeroclaw_log::Action::Note)
                            .with_outcome(::zeroclaw_log::EventOutcome::Unknown),
                        &format!("RPC WSS connection task failed: {error}")
                    );
                }
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
                        listener_result = Err(e).context("WSS accept error");
                        break;
                    }
                };

                let ctx = ctx.clone();
                let count = client_count.clone();
                let acceptor = tls_acceptor.clone();
                let connection_cancel = connections_cancel.clone();

                count.fetch_add(1, Ordering::Relaxed);

                connections.spawn(async move {
                    let ws_stream = tokio::select! {
                        _ = connection_cancel.cancelled() => None,
                        ws_stream = async {
                            let tls_stream = match acceptor.accept(tcp_stream).await {
                                Ok(s) => s,
                                Err(e) => {
                                    ::zeroclaw_log::record!(
                                        WARN,
                                        ::zeroclaw_log::Event::new(module_path!(), ::zeroclaw_log::Action::Note)
                                            .with_outcome(::zeroclaw_log::EventOutcome::Unknown),
                                        &format!("WSS TLS handshake failed from {remote_addr}: {e}")
                                    );
                                    return None;
                                }
                            };

                            match tokio_tungstenite::accept_async(tls_stream).await {
                                Ok(ws) => Some(ws),
                                Err(e) => {
                                    ::zeroclaw_log::record!(
                                        WARN,
                                        ::zeroclaw_log::Event::new(module_path!(), ::zeroclaw_log::Action::Note)
                                            .with_outcome(::zeroclaw_log::EventOutcome::Unknown),
                                        &format!("WSS WebSocket upgrade failed from {remote_addr}: {e}")
                                    );
                                    None
                                }
                            }
                        } => ws_stream,
                    };
                    let Some(ws_stream) = ws_stream else {
                        count.fetch_sub(1, Ordering::Relaxed);
                        return;
                    };

                    let mut transport = WssTransport::new(ws_stream, remote_addr);
                    let peer = transport.peer_label();
                    let writer_tx = transport.writer();
                    let mut dispatcher = RpcDispatcher::new(ctx.clone(), writer_tx, peer);
                    tokio::select! {
                        _ = connection_cancel.cancelled() => {}
                        _ = dispatcher.run(&mut transport) => {}
                    }

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

                    count.fetch_sub(1, Ordering::Relaxed);
                });
            }
        }
    }

    connections_cancel.cancel();
    while let Some(completed) = connections.join_next().await {
        if let Err(error) = completed {
            ::zeroclaw_log::record!(
                WARN,
                ::zeroclaw_log::Event::new(module_path!(), ::zeroclaw_log::Action::Note)
                    .with_outcome(::zeroclaw_log::EventOutcome::Unknown),
                &format!("RPC WSS connection task failed during shutdown: {error}")
            );
        }
    }

    listener_result
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

#[cfg(test)]
mod connection_tests {
    use super::*;
    use crate::rpc::session::SessionStore;
    use rcgen::{CertifiedKey, generate_simple_self_signed};
    use tokio::io::AsyncReadExt;
    use zeroclaw_infra::session_queue::SessionActorQueue;

    fn test_ctx(tmp: &std::path::Path) -> Arc<RpcContext> {
        let config = zeroclaw_config::schema::Config {
            data_dir: tmp.to_path_buf(),
            config_path: tmp.join("config.toml"),
            ..Default::default()
        };
        let session_queue = Arc::new(SessionActorQueue::new(4, 10, 60));
        let sessions = Arc::new(SessionStore::new(64, session_queue));
        RpcContext::minimal(config, sessions)
    }

    fn test_tls_acceptor(tmp: &std::path::Path) -> TlsAcceptor {
        let _ = rustls::crypto::ring::default_provider().install_default();
        let CertifiedKey { cert, key_pair } =
            generate_simple_self_signed(vec!["localhost".to_string()]).unwrap();
        let cert_path = tmp.join("cert.pem");
        let key_path = tmp.join("key.pem");
        std::fs::write(&cert_path, cert.pem()).unwrap();
        std::fs::write(&key_path, key_pair.serialize_pem()).unwrap();
        build_tls_acceptor(cert_path.to_str().unwrap(), key_path.to_str().unwrap()).unwrap()
    }

    async fn wait_for_client_count(count: &Arc<AtomicUsize>, expected: usize) {
        for _ in 0..250 {
            if count.load(Ordering::Relaxed) == expected {
                return;
            }
            tokio::time::sleep(Duration::from_millis(20)).await;
        }
        panic!(
            "client count never reached {expected}; last observed {}",
            count.load(Ordering::Relaxed)
        );
    }

    #[tokio::test]
    async fn cancel_interrupts_and_joins_inflight_wss_handshake() {
        let tmp = tempfile::tempdir().unwrap();
        let ctx = test_ctx(tmp.path());
        let acceptor = test_tls_acceptor(tmp.path());
        let probe = TcpListener::bind("127.0.0.1:0").await.unwrap();
        let bind_addr = probe.local_addr().unwrap();
        drop(probe);

        let cancel = CancellationToken::new();
        let count = Arc::new(AtomicUsize::new(0));
        let server_cancel = cancel.clone();
        let server_count = count.clone();
        let server = zeroclaw_spawn::spawn!(async move {
            run_wss_listener(ctx, server_cancel, server_count, acceptor, bind_addr).await
        });

        let mut client = None;
        for _ in 0..50 {
            match TcpStream::connect(bind_addr).await {
                Ok(stream) => {
                    client = Some(stream);
                    break;
                }
                Err(_) => tokio::time::sleep(Duration::from_millis(20)).await,
            }
        }
        let mut client = client.expect("WSS listener never accepted a TCP client");
        wait_for_client_count(&count, 1).await;

        cancel.cancel();
        tokio::time::timeout(Duration::from_secs(2), server)
            .await
            .expect("WSS listener should join an in-flight handshake after cancellation")
            .expect("WSS listener task should not panic")
            .expect("WSS listener should stop cleanly");

        assert_eq!(count.load(Ordering::Relaxed), 0);
        let mut byte = [0_u8; 1];
        let bytes = tokio::time::timeout(Duration::from_secs(2), client.read(&mut byte))
            .await
            .expect("cancelled WSS client should observe EOF")
            .expect("client read should not fail");
        assert_eq!(bytes, 0, "cancelled WSS connection should be closed");
    }
}
