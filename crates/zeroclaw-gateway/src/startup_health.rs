//! Answers `GET /health` while the gateway is still starting up.
//!
//! `run_gateway_with_plugin_webhooks` binds its listener first and only starts
//! accepting once the whole router is built. Building it includes the
//! dashboard seed and the per-agent `/api/tools` listings, which connect every
//! granted MCP server once (`McpRegistry::connect_all`; its receive timeout
//! bounds each server that accepts the TCP connection but never answers
//! `initialize`). Until then the kernel queues health-check connections in
//! the listen backlog and nobody answers them, so a liveness probe (for
//! example a Kubernetes `httpGet` probe with a short timeout) fails for as
//! long as an MCP server is unreachable and the process is restarted exactly
//! when it should wait.
//!
//! The responder serves a minimal router on the already-bound listener during
//! that window: `GET {prefix}/health` with the same body as the real handler
//! (including the runtime health components) and `503 Service Unavailable`
//! with `Retry-After` for everything else. The gateway takes the listener
//! back right before its own accept loop starts.
//!
//! Ownership: the listener sits in a slot the responder's task only borrows
//! for each `poll_accept`. Taking it back ([`StartupHealthResponder::into_listener`])
//! or dropping the responder (gateway setup failed or its future was dropped
//! on a daemon reload) removes it from the slot synchronously, so the port is
//! released, or handed over, at that point and not whenever the runtime gets
//! around to cancelling the task.

use std::sync::Arc;
use std::task::{Context, Poll};
use std::time::{Duration, Instant};

use anyhow::Result;
use axum::Router;
use axum::http::{StatusCode, header};
use axum::response::IntoResponse;
use parking_lot::Mutex;
use tokio::net::{TcpListener, TcpStream};
use tokio::sync::watch;
use tokio::task::{JoinHandle, JoinSet};
use tokio_rustls::TlsAcceptor;

/// How long in-flight startup requests may take after the handover before
/// they are cancelled. Each one is a tiny JSON or 503 response.
const STARTUP_RESPONDER_DRAIN: Duration = Duration::from_secs(2);

/// Backoff after a failed `accept()` so the loop does not hot-spin.
const ACCEPT_ERROR_BACKOFF: Duration = Duration::from_millis(50);

/// At most one accept-error warning per this interval.
const ACCEPT_ERROR_LOG_INTERVAL: Duration = Duration::from_secs(10);

type ListenerSlot = Arc<Mutex<Option<TcpListener>>>;

/// The router served while the gateway is starting: `GET {prefix}/health`
/// via `health`, a 503 with `Retry-After` for every other request.
pub(crate) fn startup_router<H, T>(path_prefix: &str, health: H) -> Router
where
    H: axum::handler::Handler<T, ()>,
    T: 'static,
{
    Router::new()
        .route(&format!("{path_prefix}/health"), axum::routing::get(health))
        .fallback(|| async {
            (
                StatusCode::SERVICE_UNAVAILABLE,
                [(header::RETRY_AFTER, "5")],
                "gateway is starting",
            )
                .into_response()
        })
}

/// A running startup responder. [`Self::into_listener`] hands the listener
/// back for the gateway's own accept loop; dropping the responder closes the
/// listener at once and cancels the responder.
pub(crate) struct StartupHealthResponder {
    slot: ListenerSlot,
    stop: watch::Sender<bool>,
    task: Option<JoinHandle<()>>,
}

impl StartupHealthResponder {
    /// Serve `router` on `listener` until the listener is taken back.
    pub(crate) fn spawn(listener: TcpListener, router: Router, tls: Option<TlsAcceptor>) -> Self {
        let slot: ListenerSlot = Arc::new(Mutex::new(Some(listener)));
        let (stop, stop_rx) = watch::channel(false);
        let task_slot = Arc::clone(&slot);
        let task = zeroclaw_spawn::spawn!(serve(task_slot, router, tls, stop_rx));
        Self {
            slot,
            stop,
            task: Some(task),
        }
    }

    /// Take the listener back. From here on no new connection is accepted by
    /// the responder: connections queue in the backlog for the gateway's
    /// accept loop. Connections the responder already accepted are not cut:
    /// each finishes its request in flight (an accepted connection that has
    /// not sent its request yet still gets that one answered) and closes,
    /// bounded by [`STARTUP_RESPONDER_DRAIN`], in the background.
    pub(crate) fn into_listener(mut self) -> Result<TcpListener> {
        let listener = self.slot.lock().take();
        let _ = self.stop.send(true);
        // Detach: the task drains its connections on its own and exits.
        self.task.take();
        listener
            .ok_or_else(|| anyhow::Error::msg("gateway startup health responder lost its listener"))
    }
}

impl Drop for StartupHealthResponder {
    fn drop(&mut self) {
        // Close the listener now (port released synchronously), then cancel.
        drop(self.slot.lock().take());
        if let Some(task) = self.task.take() {
            task.abort();
        }
    }
}

/// Accept on the listener in `slot`; `Ready(None)` once it has been taken.
fn poll_accept_slot(
    slot: &ListenerSlot,
    cx: &mut Context<'_>,
) -> Poll<Option<std::io::Result<(TcpStream, std::net::SocketAddr)>>> {
    match slot.lock().as_ref() {
        Some(listener) => listener.poll_accept(cx).map(Some),
        None => Poll::Ready(None),
    }
}

async fn serve(
    slot: ListenerSlot,
    router: Router,
    tls: Option<TlsAcceptor>,
    mut stop: watch::Receiver<bool>,
) {
    let mut connections = JoinSet::new();
    let mut last_accept_warning: Option<Instant> = None;
    loop {
        // Reap finished connections so the set does not grow for the whole
        // startup window.
        while connections.try_join_next().is_some() {}
        tokio::select! {
            biased;
            // A send or a dropped sender both end the accept loop.
            _ = stop.changed() => break,
            accepted = std::future::poll_fn(|cx| poll_accept_slot(&slot, cx)) => {
                let tcp_stream = match accepted {
                    None => break,
                    Some(Ok((tcp_stream, _remote))) => tcp_stream,
                    Some(Err(e)) => {
                        if last_accept_warning
                            .is_none_or(|at| at.elapsed() >= ACCEPT_ERROR_LOG_INTERVAL)
                        {
                            last_accept_warning = Some(Instant::now());
                            ::zeroclaw_log::record!(
                                WARN,
                                ::zeroclaw_log::Event::new(
                                    module_path!(),
                                    ::zeroclaw_log::Action::Note
                                )
                                .with_outcome(::zeroclaw_log::EventOutcome::Unknown)
                                .with_attrs(::serde_json::json!({"error": format!("{e}")})),
                                "gateway startup health responder: accept() failed; backing off"
                            );
                        }
                        tokio::time::sleep(ACCEPT_ERROR_BACKOFF).await;
                        continue;
                    }
                };
                connections.spawn(serve_connection(
                    tcp_stream,
                    router.clone(),
                    tls.clone(),
                    stop.clone(),
                ));
            }
        }
    }
    // Give in-flight requests a moment, then cancel whatever is left
    // (dropping the JoinSet aborts its tasks).
    let _ = tokio::time::timeout(STARTUP_RESPONDER_DRAIN, async {
        while connections.join_next().await.is_some() {}
    })
    .await;
}

/// Serve one HTTP/1.1 connection (the gateway's TLS acceptor advertises no
/// ALPN, so TLS clients speak HTTP/1.1 too).
///
/// On the handover the connection is shut down gracefully, but only once it
/// has a request in flight: hyper closes a connection that is told to shut
/// down before it has read anything (hyper 1.11 `Dispatcher::disable_keep_alive`,
/// and the auto builder drops it in `ReadVersion`), which would leave a
/// probe that connected just before the handover without an answer. A
/// connection that never sends a request is cut by the drain bound.
async fn serve_connection(
    tcp_stream: TcpStream,
    router: Router,
    tls: Option<TlsAcceptor>,
    mut stopping: watch::Receiver<bool>,
) {
    let (request_seen_tx, mut request_seen) = watch::channel(false);
    let svc = hyper::service::service_fn(move |req: hyper::Request<hyper::body::Incoming>| {
        let router = router.clone();
        request_seen_tx.send_replace(true);
        async move {
            use tower::ServiceExt as _;
            router.oneshot(req).await
        }
    });
    let mut builder = hyper::server::conn::http1::Builder::new();
    builder.timer(hyper_util::rt::TokioTimer::new());
    macro_rules! serve_until_stopped {
        ($io:expr) => {{
            let connection = builder.serve_connection($io, svc);
            tokio::pin!(connection);
            tokio::select! {
                _ = connection.as_mut() => {}
                _ = async {
                    // A send or a dropped sender both mean "stop".
                    let _ = stopping.changed().await;
                    let _ = request_seen.wait_for(|seen| *seen).await;
                } => {
                    // In flight or idle after a response: finish and close.
                    connection.as_mut().graceful_shutdown();
                    let _ = connection.as_mut().await;
                }
            }
        }};
    }
    match tls {
        Some(tls) => {
            let Ok(stream) = tls.accept(tcp_stream).await else {
                return;
            };
            serve_until_stopped!(hyper_util::rt::TokioIo::new(stream));
        }
        None => serve_until_stopped!(hyper_util::rt::TokioIo::new(tcp_stream)),
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use tokio::io::{AsyncRead, AsyncReadExt as _, AsyncWrite, AsyncWriteExt as _};

    async fn request_on<S: AsyncRead + AsyncWrite + Unpin>(stream: &mut S, path: &str) -> String {
        let request =
            format!("GET {path} HTTP/1.1\r\nHost: localhost\r\nConnection: close\r\n\r\n");
        stream.write_all(request.as_bytes()).await.unwrap();
        let mut response = Vec::new();
        // A TLS peer may close without close_notify; keep what was read.
        let _ = stream.read_to_end(&mut response).await;
        String::from_utf8_lossy(&response).into_owned()
    }

    async fn get(addr: std::net::SocketAddr, path: &str) -> String {
        tokio::time::timeout(Duration::from_secs(1), async {
            let mut stream = TcpStream::connect(addr).await.unwrap();
            request_on(&mut stream, path).await
        })
        .await
        .expect("startup responder must answer within the 1s probe timeout")
    }

    #[tokio::test]
    async fn serves_health_and_503_then_hands_the_listener_back() {
        let listener = TcpListener::bind("127.0.0.1:0").await.unwrap();
        let addr = listener.local_addr().unwrap();
        let responder = StartupHealthResponder::spawn(
            listener,
            startup_router("/pfx", || async { "healthy" }),
            None,
        );

        let health = get(addr, "/pfx/health").await;
        assert!(health.starts_with("HTTP/1.1 200"), "{health}");
        assert!(health.ends_with("healthy"), "{health}");
        let other = get(addr, "/api/tools").await;
        assert!(other.starts_with("HTTP/1.1 503"), "{other}");
        assert!(
            other.to_ascii_lowercase().contains("retry-after: 5"),
            "{other}"
        );

        // Handover: the gateway gets the very same socket back, and a
        // connection made after it waits for the gateway's accept loop.
        let listener = responder.into_listener().expect("listener handed back");
        assert_eq!(listener.local_addr().unwrap(), addr);
        let client = zeroclaw_spawn::spawn!(async move { get(addr, "/pfx/health").await });
        let (mut accepted, _) = tokio::time::timeout(Duration::from_secs(1), listener.accept())
            .await
            .expect("connection must reach the handed-back listener")
            .unwrap();
        // Read the whole request head: closing with unread bytes would RST.
        let mut head = Vec::new();
        let mut buf = [0u8; 256];
        while !head.windows(4).any(|w| w == b"\r\n\r\n") {
            let n = accepted.read(&mut buf).await.unwrap();
            assert!(n > 0, "client closed before sending its request");
            head.extend_from_slice(&buf[..n]);
        }
        assert!(head.starts_with(b"GET /pfx/health"));
        accepted
            .write_all(b"HTTP/1.1 200 OK\r\nContent-Length: 4\r\nConnection: close\r\n\r\nmain")
            .await
            .unwrap();
        drop(accepted);
        assert!(client.await.unwrap().ends_with("main"));
    }

    /// A probe whose connection the responder accepted just before the
    /// handover, but which had not sent its request yet, still gets answered.
    #[tokio::test]
    async fn a_connection_accepted_before_the_handover_is_still_answered() {
        let listener = TcpListener::bind("127.0.0.1:0").await.unwrap();
        let addr = listener.local_addr().unwrap();
        let responder = StartupHealthResponder::spawn(
            listener,
            startup_router("", || async { "healthy" }),
            None,
        );
        let mut early = TcpStream::connect(addr).await.unwrap();
        // Let the responder accept it (no bytes sent yet).
        tokio::time::sleep(Duration::from_millis(100)).await;
        let _listener = responder.into_listener().expect("listener handed back");
        tokio::time::sleep(Duration::from_millis(50)).await;
        let response =
            tokio::time::timeout(Duration::from_secs(1), request_on(&mut early, "/health"))
                .await
                .expect("answer within the probe timeout");
        assert!(response.starts_with("HTTP/1.1 200"), "{response:?}");
        assert!(response.ends_with("healthy"), "{response:?}");
    }

    /// Dropping the responder (setup failed, or the gateway future was
    /// dropped on a reload) frees the port synchronously, so the next
    /// gateway generation can bind it straight away.
    #[tokio::test]
    async fn dropping_the_responder_releases_the_port_synchronously() {
        let listener = TcpListener::bind("127.0.0.1:0").await.unwrap();
        let addr = listener.local_addr().unwrap();
        let responder =
            StartupHealthResponder::spawn(listener, startup_router("", || async { "ok" }), None);
        // Let the responder park in accept() first.
        tokio::time::sleep(Duration::from_millis(50)).await;
        drop(responder);
        // No await in between: the port must already be free.
        let rebound = std::net::TcpListener::bind(addr);
        assert!(rebound.is_ok(), "port still held: {rebound:?}");
    }

    #[tokio::test]
    async fn serves_health_over_tls() {
        let tmp = tempfile::TempDir::new().unwrap();
        let key = rcgen::KeyPair::generate().unwrap();
        let cert = rcgen::CertificateParams::new(vec!["localhost".into()])
            .unwrap()
            .self_signed(&key)
            .unwrap();
        let cert_path = tmp.path().join("gateway.crt");
        let key_path = tmp.path().join("gateway.key");
        std::fs::write(&cert_path, cert.pem()).unwrap();
        std::fs::write(&key_path, key.serialize_pem()).unwrap();
        let acceptor = crate::tls::build_tls_acceptor(&zeroclaw_config::schema::GatewayTlsConfig {
            enabled: true,
            cert_path: cert_path.display().to_string(),
            key_path: key_path.display().to_string(),
            ..Default::default()
        })
        .unwrap();
        let mut roots = rustls::RootCertStore::empty();
        roots.add(cert.der().clone()).unwrap();
        let client = rustls::ClientConfig::builder_with_provider(Arc::new(
            rustls::crypto::ring::default_provider(),
        ))
        .with_safe_default_protocol_versions()
        .unwrap()
        .with_root_certificates(roots)
        .with_no_client_auth();
        let connector = tokio_rustls::TlsConnector::from(Arc::new(client));

        let listener = TcpListener::bind("127.0.0.1:0").await.unwrap();
        let addr = listener.local_addr().unwrap();
        let responder = StartupHealthResponder::spawn(
            listener,
            startup_router("", || async { "healthy" }),
            Some(acceptor),
        );
        let response = tokio::time::timeout(Duration::from_secs(2), async {
            let tcp = TcpStream::connect(addr).await.unwrap();
            let name = rustls::pki_types::ServerName::try_from("localhost").unwrap();
            let mut tls = connector.connect(name, tcp).await.expect("TLS handshake");
            request_on(&mut tls, "/health").await
        })
        .await
        .expect("TLS startup responder must answer");
        assert!(response.starts_with("HTTP/1.1 200"), "{response}");
        assert!(response.ends_with("healthy"), "{response}");
        drop(responder);
    }
}
