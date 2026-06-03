//! WebSocket Secure (WSS) transport for the RPC layer.
//!
//! Mirrors the Unix socket transport (`unix.rs`) but uses TLS-encrypted
//! WebSocket connections, enabling remote TUI-to-daemon connectivity.

use super::context::RpcContext;
use super::dispatch::RpcDispatcher;
use super::session::SESSION_DISCONNECT_GRACE;
use super::transport::RpcTransport;
use anyhow::{Context, Result};
use async_trait::async_trait;
use futures_util::{SinkExt, StreamExt};
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
///
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
                        ::zeroclaw_log::record!(
                            WARN,
                            ::zeroclaw_log::Event::new(module_path!(), ::zeroclaw_log::Action::Note)
                                .with_outcome(::zeroclaw_log::EventOutcome::Unknown),
                            &format!("WSS accept error: {e}")
                        );
                        continue;
                    }
                };

                let ctx = ctx.clone();
                let count = client_count.clone();
                let acceptor = tls_acceptor.clone();

                count.fetch_add(1, Ordering::Relaxed);

                zeroclaw_spawn::spawn!(async move {
                    // TLS handshake.
                    let tls_stream = match acceptor.accept(tcp_stream).await {
                        Ok(s) => s,
                        Err(e) => {
                            ::zeroclaw_log::record!(
                                WARN,
                                ::zeroclaw_log::Event::new(module_path!(), ::zeroclaw_log::Action::Note)
                                    .with_outcome(::zeroclaw_log::EventOutcome::Unknown),
                                &format!("WSS TLS handshake failed from {remote_addr}: {e}")
                            );
                            count.fetch_sub(1, Ordering::Relaxed);
                            return;
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
                            count.fetch_sub(1, Ordering::Relaxed);
                            return;
                        }
                    };

                    let mut transport = WssTransport::new(ws_stream, remote_addr);
                    let peer = transport.peer_label();
                    let writer_tx = transport.writer();
                    let mut dispatcher = RpcDispatcher::new(ctx.clone(), writer_tx, peer);
                    dispatcher.run(&mut transport).await;

                    // Cleanup: unregister TUI from registry on disconnect.
                    if let Some(tui_id) = dispatcher.tui_id() {
                        ctx.tui_registry.unregister(tui_id);
                        let orphaned = ctx
                            .sessions
                            .mark_orphaned(tui_id, SESSION_DISCONNECT_GRACE)
                            .await;
                        for (session_key, agent_alias) in &orphaned {
                            use ::zeroclaw_log::Instrument as _;
                            let span = ::zeroclaw_log::info_span!(
                                target: "zeroclaw_log_internal_scope",
                                "zeroclaw_scope",
                                session_key = %session_key,
                                agent_alias = %agent_alias,
                                owner_tui_id = %tui_id,
                                channel = "wss",
                            );
                            async {
                                ::zeroclaw_log::record!(
                                    INFO,
                                    ::zeroclaw_log::Event::new(
                                        module_path!(),
                                        ::zeroclaw_log::Action::Note,
                                    )
                                    .with_category(::zeroclaw_log::EventCategory::Agent)
                                    .with_attrs(::serde_json::json!({
                                        "grace_secs": SESSION_DISCONNECT_GRACE.as_secs(),
                                    })),
                                    "WSS TUI disconnected; session queued for eviction"
                                );
                            }
                            .instrument(span)
                            .await;
                        }
                    }

                    count.fetch_sub(1, Ordering::Relaxed);
                });
            }
        }
    }

    Ok(())
}
