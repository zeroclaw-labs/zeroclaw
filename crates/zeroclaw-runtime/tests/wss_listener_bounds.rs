//! Resource bounds on the daemon's mandatory-mTLS WSS listener.
//!
//! The remote WSS plane defaults to binding `0.0.0.0`, so every state an
//! UNAUTHENTICATED peer can reach must be bounded in both time and count.
//! Before these bounds each accepted socket spawned a task that awaited the TLS
//! handshake and the WebSocket upgrade with no deadline and no cap, so a peer
//! that only completed a TCP connect could accumulate sockets, tasks and TLS
//! parser state without limit.
//!
//! These drive the real `run_wss_listener` against a production
//! `build_tls_acceptor`, and cover: a stall during TLS, a stall during the
//! WebSocket upgrade, exhaustion of the pending-handshake budget, exhaustion of
//! the session budget, and that a released permit admits a later valid peer.
//!
//! Test code, not daemon-path: bare `tokio::spawn` is fine here (the
//! `zeroclaw_spawn::spawn!` rule is for production daemon tasks).
#![allow(clippy::disallowed_methods)]

use std::path::Path;
use std::sync::Arc;
use std::sync::atomic::AtomicUsize;
use std::time::Duration;
use tokio::io::AsyncReadExt;
use tokio_util::sync::CancellationToken;
use zeroclaw_runtime::rpc::wss::WssLimits;

/// Server-cert verifier that accepts anything: these tests exercise the
/// listener's CLIENT-side bounds, not server hostname verification.
#[derive(Debug)]
struct NoServerVerify;

impl rustls::client::danger::ServerCertVerifier for NoServerVerify {
    fn verify_server_cert(
        &self,
        _e: &rustls::pki_types::CertificateDer<'_>,
        _i: &[rustls::pki_types::CertificateDer<'_>],
        _n: &rustls::pki_types::ServerName<'_>,
        _o: &[u8],
        _t: rustls::pki_types::UnixTime,
    ) -> Result<rustls::client::danger::ServerCertVerified, rustls::Error> {
        Ok(rustls::client::danger::ServerCertVerified::assertion())
    }
    fn verify_tls12_signature(
        &self,
        _m: &[u8],
        _c: &rustls::pki_types::CertificateDer<'_>,
        _d: &rustls::DigitallySignedStruct,
    ) -> Result<rustls::client::danger::HandshakeSignatureValid, rustls::Error> {
        Ok(rustls::client::danger::HandshakeSignatureValid::assertion())
    }
    fn verify_tls13_signature(
        &self,
        _m: &[u8],
        _c: &rustls::pki_types::CertificateDer<'_>,
        _d: &rustls::DigitallySignedStruct,
    ) -> Result<rustls::client::danger::HandshakeSignatureValid, rustls::Error> {
        Ok(rustls::client::danger::HandshakeSignatureValid::assertion())
    }
    fn supported_verify_schemes(&self) -> Vec<rustls::SignatureScheme> {
        rustls::crypto::ring::default_provider()
            .signature_verification_algorithms
            .supported_schemes()
    }
}

fn install_provider() {
    let _ = rustls::crypto::ring::default_provider().install_default();
}

fn write_temp(content: &str) -> tempfile::NamedTempFile {
    use std::io::Write;
    let mut f = tempfile::NamedTempFile::new().unwrap();
    f.write_all(content.as_bytes()).unwrap();
    f.flush().unwrap();
    f
}

/// Server materials plus a client certificate issued from the same CA.
fn materials(
    dir: &Path,
) -> (
    zeroclaw_tls::ServerMaterials,
    tempfile::NamedTempFile,
    tempfile::NamedTempFile,
) {
    let mats = zeroclaw_tls::ensure_server_materials(dir, &[]).unwrap();
    let ca_pem = std::fs::read_to_string(&mats.ca_cert_path).unwrap();
    let ca_key_pem = std::fs::read_to_string(&mats.ca_key_path).unwrap();
    let client = zeroclaw_tls::issue_client_cert(&ca_pem, &ca_key_pem, "bounds-test").unwrap();
    (
        mats,
        write_temp(&client.cert_pem),
        write_temp(&client.key_pem),
    )
}

fn client_config(client: Option<(&Path, &Path)>) -> rustls::ClientConfig {
    let builder = rustls::ClientConfig::builder_with_provider(Arc::new(
        rustls::crypto::ring::default_provider(),
    ))
    .with_safe_default_protocol_versions()
    .unwrap()
    .dangerous()
    .with_custom_certificate_verifier(Arc::new(NoServerVerify));
    match client {
        Some((cert, key)) => {
            let chain = zeroclaw_tls::load_certs(cert.to_str().unwrap()).unwrap();
            let key = zeroclaw_tls::load_private_key(key.to_str().unwrap()).unwrap();
            builder.with_client_auth_cert(chain, key).unwrap()
        }
        None => builder.with_no_client_auth(),
    }
}

/// A free loopback port. The listener binds it itself, so we reserve and
/// release one rather than reading it back from `run_wss_listener`.
async fn free_port() -> u16 {
    let l = tokio::net::TcpListener::bind("127.0.0.1:0").await.unwrap();
    let port = l.local_addr().unwrap().port();
    drop(l);
    port
}

/// Start the real listener with the given bounds. Returns its address and the
/// cancellation token that stops it.
async fn start_listener(
    dir: &Path,
    limits: WssLimits,
) -> (
    std::net::SocketAddr,
    CancellationToken,
    tempfile::NamedTempFile,
    tempfile::NamedTempFile,
) {
    install_provider();
    let (mats, client_cert, client_key) = materials(dir);
    let acceptor = zeroclaw_runtime::rpc::wss::build_tls_acceptor(
        mats.server_cert_path.to_str().unwrap(),
        mats.server_key_path.to_str().unwrap(),
        mats.ca_cert_path.to_str().unwrap(),
        &[],
        "",
    )
    .unwrap();

    let config = zeroclaw_config::schema::Config {
        data_dir: dir.to_path_buf(),
        ..Default::default()
    };
    let queue = Arc::new(zeroclaw_infra::session_queue::SessionActorQueue::new(
        4, 10, 60,
    ));
    let sessions = Arc::new(zeroclaw_runtime::rpc::session::SessionStore::new(16, queue));
    let ctx = zeroclaw_runtime::rpc::context::RpcContext::for_live_test(config, sessions);

    let port = free_port().await;
    let addr: std::net::SocketAddr = format!("127.0.0.1:{port}").parse().unwrap();
    let cancel = CancellationToken::new();
    let cancel_for_task = cancel.clone();
    tokio::spawn(async move {
        let _ = zeroclaw_runtime::rpc::wss::run_wss_listener(
            ctx,
            cancel_for_task,
            Arc::new(AtomicUsize::new(0)),
            acceptor,
            addr,
            limits,
        )
        .await;
    });
    // Let the listener bind before the first connect.
    tokio::time::sleep(Duration::from_millis(250)).await;
    (addr, cancel, client_cert, client_key)
}

/// Read until EOF (or error), returning how long that took. A shed or reaped
/// connection closes; a retained one keeps the read pending.
async fn time_to_close(sock: &mut tokio::net::TcpStream, budget: Duration) -> Option<Duration> {
    let started = std::time::Instant::now();
    let mut buf = [0u8; 64];
    match tokio::time::timeout(budget, sock.read(&mut buf)).await {
        Ok(Ok(0)) | Ok(Err(_)) => Some(started.elapsed()),
        Ok(Ok(_)) => None, // server sent data: not a close
        Err(_) => None,    // still open at the budget
    }
}

/// (a) A peer that completes the TCP connect and never starts TLS must be
/// reaped at the setup deadline, not held indefinitely.
#[tokio::test]
async fn tls_stall_is_reaped_at_the_setup_deadline() {
    let dir = tempfile::tempdir().unwrap();
    let (addr, cancel, _cc, _ck) = start_listener(
        dir.path(),
        WssLimits {
            max_pending_handshakes: 8,
            handshake_timeout: Duration::from_secs(2),
            max_sessions: 8,
        },
    )
    .await;

    let mut sock = tokio::net::TcpStream::connect(addr).await.unwrap();
    // Never send a ClientHello.
    let closed = time_to_close(&mut sock, Duration::from_secs(8)).await;
    let elapsed = closed.expect("a stalled TLS handshake must be reaped at the deadline");
    assert!(
        elapsed < Duration::from_secs(6),
        "reaped after {elapsed:?}, well past the 2s budget"
    );
    cancel.cancel();
}

/// (b) A peer that completes the mTLS handshake but never sends the HTTP
/// upgrade must also be reaped: the deadline covers BOTH setup phases, and the
/// heartbeat only starts once a session exists.
#[tokio::test]
async fn websocket_upgrade_stall_is_reaped_at_the_setup_deadline() {
    let dir = tempfile::tempdir().unwrap();
    let (addr, cancel, cc, ck) = start_listener(
        dir.path(),
        WssLimits {
            max_pending_handshakes: 8,
            handshake_timeout: Duration::from_secs(2),
            max_sessions: 8,
        },
    )
    .await;

    let connector =
        tokio_rustls::TlsConnector::from(Arc::new(client_config(Some((cc.path(), ck.path())))));
    let tcp = tokio::net::TcpStream::connect(addr).await.unwrap();
    let sni = rustls::pki_types::ServerName::try_from("localhost").unwrap();
    let mut tls = connector
        .connect(sni, tcp)
        .await
        .expect("mTLS must complete");

    // Authenticated, but never send the WebSocket upgrade request.
    let started = std::time::Instant::now();
    let mut buf = [0u8; 64];
    let read = tokio::time::timeout(Duration::from_secs(8), tls.read(&mut buf)).await;
    let elapsed = started.elapsed();
    match read {
        Ok(Ok(0)) | Ok(Err(_)) => {}
        other => panic!("expected the stalled upgrade to be reaped, got {other:?}"),
    }
    assert!(
        elapsed < Duration::from_secs(6),
        "reaped after {elapsed:?}, well past the 2s budget"
    );
    cancel.cancel();
}

/// (c) + (d) The pending-handshake budget sheds new sockets while it is full,
/// and once the stalled peers are reaped a later VALID peer is admitted.
#[tokio::test]
async fn pending_handshake_budget_sheds_then_admits_a_valid_peer() {
    let dir = tempfile::tempdir().unwrap();
    let (addr, cancel, cc, ck) = start_listener(
        dir.path(),
        WssLimits {
            max_pending_handshakes: 2,
            handshake_timeout: Duration::from_secs(3),
            max_sessions: 8,
        },
    )
    .await;

    // Fill the pool with sockets that never start TLS.
    let mut stalled = Vec::new();
    for _ in 0..2 {
        stalled.push(tokio::net::TcpStream::connect(addr).await.unwrap());
    }
    tokio::time::sleep(Duration::from_millis(300)).await;

    // Pool exhausted: the next socket is shed at accept, closing promptly -
    // well before the 3s deadline could explain it.
    let mut shed = tokio::net::TcpStream::connect(addr).await.unwrap();
    let closed = time_to_close(&mut shed, Duration::from_millis(1500)).await;
    assert!(
        closed.is_some(),
        "a socket arriving on a full pending-handshake pool must be shed"
    );

    // Past the deadline the stalled pair is reaped and the permits return: a
    // genuine mTLS + WebSocket client must now complete.
    tokio::time::sleep(Duration::from_secs(4)).await;
    let connector =
        tokio_tungstenite::Connector::Rustls(Arc::new(client_config(Some((cc.path(), ck.path())))));
    let url = format!("wss://127.0.0.1:{}/", addr.port());
    let connected =
        tokio_tungstenite::connect_async_tls_with_config(&url, None, false, Some(connector)).await;
    assert!(
        connected.is_ok(),
        "a released permit must admit a later valid peer, got {:?}",
        connected.err()
    );
    drop(stalled);
    cancel.cancel();
}

/// (c) The session budget bounds the steady state that survives
/// authentication: with one session established, a second is shed.
#[tokio::test]
async fn session_budget_sheds_beyond_its_bound() {
    let dir = tempfile::tempdir().unwrap();
    let (addr, cancel, cc, ck) = start_listener(
        dir.path(),
        WssLimits {
            max_pending_handshakes: 8,
            handshake_timeout: Duration::from_secs(5),
            max_sessions: 1,
        },
    )
    .await;

    let url = format!("wss://127.0.0.1:{}/", addr.port());
    let connector = || {
        tokio_tungstenite::Connector::Rustls(Arc::new(client_config(Some((cc.path(), ck.path())))))
    };

    // First session occupies the only permit and stays open.
    let (_first, _resp) =
        tokio_tungstenite::connect_async_tls_with_config(&url, None, false, Some(connector()))
            .await
            .expect("the first session must be admitted");
    tokio::time::sleep(Duration::from_millis(300)).await;

    // The second connection is shed at accept, before TLS.
    let mut second = tokio::net::TcpStream::connect(addr).await.unwrap();
    let closed = time_to_close(&mut second, Duration::from_millis(1500)).await;
    assert!(
        closed.is_some(),
        "a connection arriving at the session bound must be shed"
    );
    cancel.cancel();
}
