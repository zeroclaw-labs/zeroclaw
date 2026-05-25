//! WebSocket Secure (WSS) transport for the RPC layer.
//!
//! Mirrors the Unix socket transport (`unix.rs`) but uses TLS-encrypted
//! WebSocket connections, enabling remote TUI-to-daemon connectivity.

use super::context::RpcContext;
use super::dispatch::RpcDispatcher;
use super::transport::RpcTransport;
use anyhow::{Context, Result};
use async_trait::async_trait;
use futures_util::{SinkExt, StreamExt};
use std::net::SocketAddr;
use std::sync::Arc;
use std::sync::atomic::{AtomicUsize, Ordering};
use tokio::net::{TcpListener, TcpStream};
use tokio::sync::mpsc;
use tokio_rustls::TlsAcceptor;
use tokio_tungstenite::WebSocketStream;
use tokio_tungstenite::tungstenite::Message;
use tokio_util::sync::CancellationToken;

type TlsStream = tokio_rustls::server::TlsStream<TcpStream>;

// ── Transport ────────────────────────────────────────────────────

pub struct WssTransport {
    reader: futures_util::stream::SplitStream<WebSocketStream<TlsStream>>,
    writer_tx: mpsc::Sender<String>,
    peer_label: String,
}

impl WssTransport {
    pub fn new(ws: WebSocketStream<TlsStream>, remote_addr: SocketAddr) -> Self {
        let peer_label = format!("wss:{remote_addr}");
        let (sink, stream) = ws.split();

        let (writer_tx, mut writer_rx) = mpsc::channel::<String>(64);
        zeroclaw_api::spawn!(async move {
            let mut sink = sink;
            while let Some(line) = writer_rx.recv().await {
                if sink.send(Message::Text(line.into())).await.is_err() {
                    break;
                }
            }
        });

        Self {
            reader: stream,
            writer_tx,
            peer_label,
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
            match self.reader.next().await {
                Some(Ok(Message::Text(text))) => return Some(text.to_string()),
                Some(Ok(Message::Close(_))) | None => return None,
                Some(Ok(Message::Ping(_) | Message::Pong(_) | Message::Frame(_))) => continue,
                Some(Ok(Message::Binary(_))) => continue, // Ignore binary frames
                Some(Err(_)) => return None,
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

                zeroclaw_api::spawn!(async move {
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
                    }

                    count.fetch_sub(1, Ordering::Relaxed);
                });
            }
        }
    }

    Ok(())
}
