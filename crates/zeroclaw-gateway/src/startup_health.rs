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
//! The responder accepts on the already-bound listener during that window and
//! answers `GET {prefix}/health` with the same body as the real handler
//! (including the runtime health components), closing the connection after
//! the response. Every other plain-HTTP connection is left unread and handed
//! to the gateway's accept loop ([`DeferredConnections`]), so a request for a
//! real route waits for the real router exactly as it waited in the listen
//! backlog before, instead of failing. Over TLS the request path is only
//! visible after the handshake, so a non-health TLS request is answered with
//! `503 Service Unavailable` and `Retry-After`.
//!
//! Ownership: the listener sits in a slot the responder's task only borrows
//! for each `poll_accept`. Taking it back ([`StartupHealthResponder::into_listener`])
//! or dropping the responder (gateway setup failed or its future was dropped
//! on a daemon reload) removes it from the slot synchronously, so the port is
//! released, or handed over, at that point and not whenever the runtime gets
//! around to cancelling the task.

use std::net::SocketAddr;
use std::sync::Arc;
use std::task::{Context, Poll};
use std::time::{Duration, Instant};

use anyhow::Result;
use axum::Router;
use axum::http::{StatusCode, header};
use axum::response::IntoResponse;
use parking_lot::Mutex;
use tokio::net::{TcpListener, TcpStream};
use tokio::sync::{mpsc, watch};
use tokio::task::{JoinHandle, JoinSet};
use tokio_rustls::TlsAcceptor;

/// How long in-flight startup requests may take after the handover before
/// they are cancelled. Each one is a tiny JSON or 503 response.
const STARTUP_RESPONDER_DRAIN: Duration = Duration::from_secs(2);

/// Backoff after a failed `accept()` so the loop does not hot-spin.
const ACCEPT_ERROR_BACKOFF: Duration = Duration::from_millis(50);

/// At most one accept-error warning per this interval.
const ACCEPT_ERROR_LOG_INTERVAL: Duration = Duration::from_secs(10);

/// Upper bound on connections held for the gateway's accept loop, like the
/// listen backlog bounds them without the responder. A connection beyond it
/// is closed.
const DEFERRED_CONNECTIONS_MAX: usize = 1024;

/// How much of a connection's first bytes is inspected for its request line.
const REQUEST_LINE_PEEK_MAX: usize = 2048;

/// Pause between peeks while a request line is still incomplete: `peek`
/// returns at once while any byte is buffered, so it cannot wait for more.
const REQUEST_LINE_PEEK_RETRY: Duration = Duration::from_millis(5);

type ListenerSlot = Arc<Mutex<Option<TcpListener>>>;

/// Plain-HTTP connections the responder accepted but did not serve, unread,
/// in accept order. The gateway's accept loop serves them like connections
/// it accepted itself (see [`accept_next`]).
pub(crate) type DeferredConnections = mpsc::Receiver<(TcpStream, SocketAddr)>;

/// The router served while the gateway is starting: `GET {prefix}/health`
/// via `health`, a 503 with `Retry-After` for every other request that
/// reaches it (only TLS connections do; see the module docs).
pub(crate) fn startup_router<H, T>(path_prefix: &str, health: H) -> Router
where
    H: axum::handler::Handler<T, ()>,
    T: 'static,
{
    Router::new()
        .route(&health_path(path_prefix), axum::routing::get(health))
        .fallback(|| async {
            (
                StatusCode::SERVICE_UNAVAILABLE,
                [(header::RETRY_AFTER, "5")],
                "gateway is starting",
            )
                .into_response()
        })
}

fn health_path(path_prefix: &str) -> String {
    format!("{path_prefix}/health")
}

/// A running startup responder. [`Self::into_listener`] hands the listener
/// back for the gateway's own accept loop; dropping the responder closes the
/// listener at once and cancels the responder.
pub(crate) struct StartupHealthResponder {
    slot: ListenerSlot,
    stop: watch::Sender<bool>,
    task: Option<JoinHandle<()>>,
    deferred: Option<DeferredConnections>,
}

impl StartupHealthResponder {
    /// Serve `router` on `listener` until the listener is taken back.
    /// `path_prefix` must be the one `router` was built with.
    pub(crate) fn spawn(
        listener: TcpListener,
        path_prefix: &str,
        router: Router,
        tls: Option<TlsAcceptor>,
    ) -> Self {
        let slot: ListenerSlot = Arc::new(Mutex::new(Some(listener)));
        let (stop, stop_rx) = watch::channel(false);
        let (deferred_tx, deferred) = mpsc::channel(DEFERRED_CONNECTIONS_MAX);
        let task_slot = Arc::clone(&slot);
        let health_path: Arc<str> = Arc::from(health_path(path_prefix));
        let task = zeroclaw_spawn::spawn!(serve(
            task_slot,
            health_path,
            router,
            tls,
            deferred_tx,
            stop_rx
        ));
        Self {
            slot,
            stop,
            task: Some(task),
            deferred: Some(deferred),
        }
    }

    /// Take the listener back, together with the connections handed over
    /// unread. From here on no new connection is accepted by the responder:
    /// connections queue in the backlog for the gateway's accept loop. A
    /// plain-HTTP connection that has not sent its request line yet is handed
    /// over too. Health and TLS connections the responder already serves
    /// finish their request in flight and close, bounded by
    /// [`STARTUP_RESPONDER_DRAIN`], in the background.
    pub(crate) fn into_listener(mut self) -> Result<(TcpListener, DeferredConnections)> {
        let listener = self.slot.lock().take();
        let _ = self.stop.send(true);
        // Detach: the task drains its connections on its own and exits.
        self.task.take();
        match (listener, self.deferred.take()) {
            (Some(listener), Some(deferred)) => Ok((listener, deferred)),
            _ => Err(anyhow::Error::msg(
                "gateway startup health responder lost its listener",
            )),
        }
    }
}

impl Drop for StartupHealthResponder {
    fn drop(&mut self) {
        // Close the listener now (port released synchronously), then cancel.
        // Connections still waiting for the accept loop close with it.
        drop(self.slot.lock().take());
        drop(self.deferred.take());
        if let Some(task) = self.task.take() {
            task.abort();
        }
    }
}

/// The next connection for the gateway's accept loop: one the startup
/// responder handed over, else a new one from `listener`. `deferred` is
/// cleared once the responder has handed over its last connection.
/// Cancel-safe, like `TcpListener::accept`.
pub(crate) async fn accept_next(
    listener: &TcpListener,
    deferred: &mut Option<DeferredConnections>,
) -> std::io::Result<(TcpStream, SocketAddr)> {
    let drained = match deferred.as_mut() {
        Some(handed_over) => tokio::select! {
            biased;
            connection = handed_over.recv() => match connection {
                Some(connection) => return Ok(connection),
                None => true,
            },
            accepted = listener.accept() => return accepted,
        },
        None => false,
    };
    if drained {
        *deferred = None;
    }
    listener.accept().await
}

/// Accept on the listener in `slot`; `Ready(None)` once it has been taken.
fn poll_accept_slot(
    slot: &ListenerSlot,
    cx: &mut Context<'_>,
) -> Poll<Option<std::io::Result<(TcpStream, SocketAddr)>>> {
    match slot.lock().as_ref() {
        Some(listener) => listener.poll_accept(cx).map(Some),
        None => Poll::Ready(None),
    }
}

async fn serve(
    slot: ListenerSlot,
    health_path: Arc<str>,
    router: Router,
    tls: Option<TlsAcceptor>,
    deferred: mpsc::Sender<(TcpStream, SocketAddr)>,
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
                let (tcp_stream, remote) = match accepted {
                    None => break,
                    Some(Ok(accepted)) => accepted,
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
                match &tls {
                    Some(tls) => connections.spawn(serve_connection(
                        tcp_stream,
                        router.clone(),
                        Some(tls.clone()),
                        stop.clone(),
                    )),
                    None => connections.spawn(serve_or_defer_plain(
                        tcp_stream,
                        remote,
                        Arc::clone(&health_path),
                        router.clone(),
                        deferred.clone(),
                        stop.clone(),
                    )),
                };
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

/// Serve a plain-HTTP connection whose request line asks for the health
/// path; hand any other connection to the gateway's accept loop unread. A
/// connection still without a complete request line at the handover is
/// handed over too, so the real router reads it.
async fn serve_or_defer_plain(
    tcp_stream: TcpStream,
    remote: SocketAddr,
    health_path: Arc<str>,
    router: Router,
    deferred: mpsc::Sender<(TcpStream, SocketAddr)>,
    mut stopping: watch::Receiver<bool>,
) {
    let is_health = tokio::select! {
        biased;
        request_line = peek_request_line(&tcp_stream) => match request_line {
            Some(line) => is_health_request(&line, &health_path),
            // Closed before sending a request line.
            None => return,
        },
        // A send or a dropped sender both mean "stop".
        _ = stopping.wait_for(|stopped| *stopped) => false,
    };
    if is_health {
        serve_connection(tcp_stream, router, None, stopping).await;
    } else {
        // If the accept loop is gone or the hand-over queue is full, the
        // connection is dropped (closed), as a full listen backlog would
        // have refused it.
        let _ = deferred.try_send((tcp_stream, remote));
    }
}

/// The connection's buffered bytes up to and including the first line
/// break, without consuming them; `None` once the peer closed first.
async fn peek_request_line(tcp_stream: &TcpStream) -> Option<Vec<u8>> {
    let mut buf = vec![0u8; REQUEST_LINE_PEEK_MAX];
    loop {
        let n = tcp_stream.peek(&mut buf).await.ok()?;
        if n == 0 {
            return None;
        }
        if let Some(end) = buf[..n].windows(2).position(|w| w == b"\r\n") {
            buf.truncate(end);
            return Some(buf);
        }
        if n == buf.len() {
            // No line break in the inspected window: not a health check.
            buf.truncate(n);
            return Some(buf);
        }
        tokio::time::sleep(REQUEST_LINE_PEEK_RETRY).await;
    }
}

/// Whether an HTTP/1 request line is `GET` or `HEAD` for `health_path`,
/// with or without a query string.
fn is_health_request(request_line: &[u8], health_path: &str) -> bool {
    let mut parts = request_line.split(|b| *b == b' ');
    let (Some(method), Some(target)) = (parts.next(), parts.next()) else {
        return false;
    };
    let path = target
        .iter()
        .position(|b| *b == b'?')
        .map_or(target, |query| &target[..query]);
    matches!(method, b"GET" | b"HEAD") && path == health_path.as_bytes()
}

/// Serve one HTTP/1.1 connection (the gateway's TLS acceptor advertises no
/// ALPN, so TLS clients speak HTTP/1.1 too). Keep-alive is off: the
/// connection closes after its first response, so a follow-up request is
/// made on a new connection and reaches the real router once it is ready.
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
    builder.keep_alive(false);
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

    async fn send_request<S: AsyncWrite + Unpin>(stream: &mut S, path: &str) {
        let request =
            format!("GET {path} HTTP/1.1\r\nHost: localhost\r\nConnection: close\r\n\r\n");
        stream.write_all(request.as_bytes()).await.unwrap();
    }

    async fn read_response<S: AsyncRead + Unpin>(stream: &mut S) -> String {
        let mut response = Vec::new();
        // A TLS peer may close without close_notify; keep what was read.
        let _ = stream.read_to_end(&mut response).await;
        String::from_utf8_lossy(&response).into_owned()
    }

    async fn request_on<S: AsyncRead + AsyncWrite + Unpin>(stream: &mut S, path: &str) -> String {
        send_request(stream, path).await;
        read_response(stream).await
    }

    async fn get(addr: SocketAddr, path: &str) -> String {
        tokio::time::timeout(Duration::from_secs(1), async {
            let mut stream = TcpStream::connect(addr).await.unwrap();
            request_on(&mut stream, path).await
        })
        .await
        .expect("startup responder must answer within the 1s probe timeout")
    }

    /// Stand in for the gateway's accept loop: take the next connection, read
    /// its whole request head and answer it.
    async fn answer_next(
        listener: &TcpListener,
        deferred: &mut Option<DeferredConnections>,
        expected_head: &[u8],
    ) {
        let (mut accepted, _) =
            tokio::time::timeout(Duration::from_secs(1), accept_next(listener, deferred))
                .await
                .expect("connection must reach the gateway's accept loop")
                .unwrap();
        // Read the whole request head: closing with unread bytes would RST.
        let mut head = Vec::new();
        let mut buf = [0u8; 256];
        while !head.windows(4).any(|w| w == b"\r\n\r\n") {
            let n = accepted.read(&mut buf).await.unwrap();
            assert!(n > 0, "client closed before sending its request");
            head.extend_from_slice(&buf[..n]);
        }
        assert!(
            head.starts_with(expected_head),
            "{}",
            String::from_utf8_lossy(&head)
        );
        accepted
            .write_all(b"HTTP/1.1 200 OK\r\nContent-Length: 4\r\nConnection: close\r\n\r\nmain")
            .await
            .unwrap();
    }

    #[tokio::test]
    async fn serves_health_and_hands_other_requests_to_the_accept_loop_unread() {
        let listener = TcpListener::bind("127.0.0.1:0").await.unwrap();
        let addr = listener.local_addr().unwrap();
        let responder = StartupHealthResponder::spawn(
            listener,
            "/pfx",
            startup_router("/pfx", || async { "healthy" }),
            None,
        );

        let health = get(addr, "/pfx/health?verbose=1").await;
        assert!(health.starts_with("HTTP/1.1 200"), "{health}");
        assert!(health.ends_with("healthy"), "{health}");

        // A real route is not answered by the responder: it waits for the
        // gateway's router, as it waited in the listen backlog before.
        let mut early = TcpStream::connect(addr).await.unwrap();
        send_request(&mut early, "/api/tools").await;
        let mut byte = [0u8; 1];
        assert!(
            tokio::time::timeout(Duration::from_millis(200), early.read(&mut byte))
                .await
                .is_err(),
            "a non-health request must not be answered before the handover"
        );

        // Handover: the gateway gets the very same socket back, serves the
        // held connection first, then new ones from the listener.
        let (listener, deferred) = responder.into_listener().expect("listener handed back");
        assert_eq!(listener.local_addr().unwrap(), addr);
        let mut deferred = Some(deferred);
        answer_next(&listener, &mut deferred, b"GET /api/tools").await;
        assert!(read_response(&mut early).await.ends_with("main"));

        let client = zeroclaw_spawn::spawn!(async move { get(addr, "/pfx/health").await });
        answer_next(&listener, &mut deferred, b"GET /pfx/health").await;
        assert!(client.await.unwrap().ends_with("main"));
    }

    /// A connection the responder accepted just before the handover, but
    /// which had not sent its request yet, is served by the gateway.
    #[tokio::test]
    async fn a_connection_accepted_before_the_handover_reaches_the_gateway() {
        let listener = TcpListener::bind("127.0.0.1:0").await.unwrap();
        let addr = listener.local_addr().unwrap();
        let responder = StartupHealthResponder::spawn(
            listener,
            "",
            startup_router("", || async { "healthy" }),
            None,
        );
        let mut early = TcpStream::connect(addr).await.unwrap();
        // Let the responder accept it (no bytes sent yet).
        tokio::time::sleep(Duration::from_millis(100)).await;
        let (listener, deferred) = responder.into_listener().expect("listener handed back");
        let mut deferred = Some(deferred);
        send_request(&mut early, "/health").await;
        answer_next(&listener, &mut deferred, b"GET /health").await;
        let response = tokio::time::timeout(Duration::from_secs(1), read_response(&mut early))
            .await
            .expect("answer within the probe timeout");
        assert!(response.ends_with("main"), "{response:?}");
    }

    /// Dropping the responder (setup failed, or the gateway future was
    /// dropped on a reload) frees the port synchronously, so the next
    /// gateway generation can bind it straight away.
    #[tokio::test]
    async fn dropping_the_responder_releases_the_port_synchronously() {
        let listener = TcpListener::bind("127.0.0.1:0").await.unwrap();
        let addr = listener.local_addr().unwrap();
        let responder = StartupHealthResponder::spawn(
            listener,
            "",
            startup_router("", || async { "ok" }),
            None,
        );
        // Let the responder park in accept() first.
        tokio::time::sleep(Duration::from_millis(50)).await;
        drop(responder);
        // No await in between: the port must already be free.
        let rebound = std::net::TcpListener::bind(addr);
        assert!(rebound.is_ok(), "port still held: {rebound:?}");
    }

    #[test]
    fn recognizes_health_request_lines() {
        assert!(is_health_request(b"GET /health HTTP/1.1", "/health"));
        assert!(is_health_request(b"HEAD /health HTTP/1.1", "/health"));
        assert!(is_health_request(
            b"GET /gw/health?x=1 HTTP/1.1",
            "/gw/health"
        ));
        assert!(!is_health_request(b"GET /health HTTP/1.1", "/gw/health"));
        assert!(!is_health_request(b"POST /health HTTP/1.1", "/health"));
        assert!(!is_health_request(b"GET /healthz HTTP/1.1", "/health"));
        assert!(!is_health_request(b"GET /api/health HTTP/1.1", "/health"));
        assert!(!is_health_request(b"GET", "/health"));
    }

    #[tokio::test]
    async fn serves_health_over_tls() {
        // The gateway binary installs the process-level provider at startup.
        let _ = rustls::crypto::ring::default_provider().install_default();
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
            "",
            startup_router("", || async { "healthy" }),
            Some(acceptor),
        );
        let tls_get = |path: &'static str| {
            let connector = connector.clone();
            async move {
                tokio::time::timeout(Duration::from_secs(2), async {
                    let tcp = TcpStream::connect(addr).await.unwrap();
                    let name = rustls::pki_types::ServerName::try_from("localhost").unwrap();
                    let mut tls = connector.connect(name, tcp).await.expect("TLS handshake");
                    request_on(&mut tls, path).await
                })
                .await
                .expect("TLS startup responder must answer")
            }
        };
        let response = tls_get("/health").await;
        assert!(response.starts_with("HTTP/1.1 200"), "{response}");
        assert!(response.ends_with("healthy"), "{response}");
        // Over TLS the path is only known after the handshake: 503.
        let other = tls_get("/api/tools").await;
        assert!(other.starts_with("HTTP/1.1 503"), "{other}");
        assert!(
            other.to_ascii_lowercase().contains("retry-after: 5"),
            "{other}"
        );
        drop(responder);
    }
}
