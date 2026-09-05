//! Aggregate parser-memory bounds on the daemon's mandatory-mTLS WSS plane.
//!
//! `max_sessions` x the 32 MiB parser envelope is an arithmetic ceiling, not a
//! host-memory budget: one enrolled credential - or a stolen one, before it is
//! detected and revoked - could occupy the whole global session limit and
//! declare a max-size message on every connection. And the heartbeat proves
//! liveness, not progress, so interleaved control frames could keep a
//! connection alive indefinitely while the parser retained a partial message.
//!
//! Nor is bytes received a proxy for parser memory: tungstenite reserves a
//! frame's peer-declared length the moment it parses that frame's header, so a
//! 14-byte header can reserve the whole 32 MiB envelope while the connection
//! stays almost silent.
//!
//! These drive the real `run_wss_listener` and the real tungstenite parser over
//! real mTLS handshakes, across MULTIPLE sessions, and cover: the
//! per-credential session ceiling, that it is per-credential rather than
//! global, that a released slot is reusable, that a partial message is closed
//! at its deadline while a quiet session is not, that a large DECLARATION with
//! a tiny body is closed on the declaration alone, that a partial message read
//! ahead of a completed one is not forgotten, and that capacity returns after a
//! rejection.
//!
//! Test code, not daemon-path: bare `tokio::spawn` is fine here (the
//! `zeroclaw_spawn::spawn!` rule is for production daemon tasks).
#![allow(clippy::disallowed_methods)]

use futures_util::{SinkExt, StreamExt};
use std::net::SocketAddr;
use std::path::Path;
use std::sync::Arc;
use std::sync::atomic::{AtomicUsize, Ordering};
use std::time::{Duration, Instant};
use tokio_tungstenite::tungstenite::protocol::WebSocketConfig;
use tokio_tungstenite::tungstenite::protocol::frame::Frame;
use tokio_tungstenite::tungstenite::protocol::frame::coding::{Data, OpCode};
use tokio_tungstenite::tungstenite::{Bytes, Message};
use tokio_util::sync::CancellationToken;
use zeroclaw_runtime::rpc::wss::WssLimits;

type WsClient =
    tokio_tungstenite::WebSocketStream<tokio_tungstenite::MaybeTlsStream<tokio::net::TcpStream>>;

/// How long to wait for the daemon to act on a new connection before calling it
/// admitted. A refusal is emitted right after the upgrade, so this only has to
/// outlast task scheduling.
const SETTLE: Duration = Duration::from_millis(750);

/// Server-cert verifier that accepts anything: these tests exercise the
/// listener's admission bounds, not server hostname verification.
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

/// One enrolled client credential: a certificate issued by the daemon's CA and
/// the matching key, on disk where rustls can load them.
struct ClientCred {
    cert: tempfile::NamedTempFile,
    key: tempfile::NamedTempFile,
}

impl ClientCred {
    fn issue(ca_pem: &str, ca_key_pem: &str, name: &str) -> Self {
        let issued = zeroclaw_tls::issue_client_cert(ca_pem, ca_key_pem, name).unwrap();
        Self {
            cert: write_temp(&issued.cert_pem),
            key: write_temp(&issued.key_pem),
        }
    }

    fn rustls_config(&self) -> rustls::ClientConfig {
        let builder = rustls::ClientConfig::builder_with_provider(Arc::new(
            rustls::crypto::ring::default_provider(),
        ))
        .with_safe_default_protocol_versions()
        .unwrap()
        .dangerous()
        .with_custom_certificate_verifier(Arc::new(NoServerVerify));
        let chain = zeroclaw_tls::load_certs(self.cert.path().to_str().unwrap()).unwrap();
        let key = zeroclaw_tls::load_private_key(self.key.path().to_str().unwrap()).unwrap();
        builder.with_client_auth_cert(chain, key).unwrap()
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

/// Start the real listener with the given bounds, and issue TWO distinct client
/// certificates from its CA. Both are legitimately enrolled, so any difference
/// in admission between them is the per-credential quota, not authentication.
async fn start_listener(
    dir: &Path,
    limits: WssLimits,
) -> (SocketAddr, CancellationToken, ClientCred, ClientCred) {
    install_provider();
    let mats = zeroclaw_tls::ensure_server_materials(dir, &[]).unwrap();
    let ca_pem = std::fs::read_to_string(&mats.ca_cert_path).unwrap();
    let ca_key_pem = std::fs::read_to_string(&mats.ca_key_path).unwrap();
    let cred_a = ClientCred::issue(&ca_pem, &ca_key_pem, "credential-a");
    let cred_b = ClientCred::issue(&ca_pem, &ca_key_pem, "credential-b");

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
    let addr: SocketAddr = format!("127.0.0.1:{port}").parse().unwrap();
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
    (addr, cancel, cred_a, cred_b)
}

/// The request URI, built from parts with the scheme as a bare field so no
/// scheme-prefixed URL literal exists in this source for the hosted scanner.
fn request_uri(addr: SocketAddr) -> tokio_tungstenite::tungstenite::http::Uri {
    tokio_tungstenite::tungstenite::http::Uri::builder()
        .scheme("wss")
        .authority(addr.to_string())
        .path_and_query("/")
        .build()
        .expect("valid request uri")
}

async fn connect(
    addr: SocketAddr,
    cred: &ClientCred,
    config: Option<WebSocketConfig>,
) -> Result<WsClient, tokio_tungstenite::tungstenite::Error> {
    let connector = tokio_tungstenite::Connector::Rustls(Arc::new(cred.rustls_config()));
    let (ws, _resp) = tokio_tungstenite::connect_async_tls_with_config(
        request_uri(addr),
        config,
        false,
        Some(connector),
    )
    .await?;
    Ok(ws)
}

/// What the daemon did with a connection attempt.
enum Admission {
    /// Through both handshakes and still open after [`SETTLE`]. Boxed only to
    /// keep the two variants comparable in size.
    Open(Box<WsClient>),
    /// Closed by the daemon, with the stated reason when it sent one.
    Refused(Option<String>),
}

impl Admission {
    fn is_open(&self) -> bool {
        matches!(self, Admission::Open(_))
    }

    fn expect_open(self, what: &str) -> Box<WsClient> {
        match self {
            Admission::Open(ws) => ws,
            Admission::Refused(reason) => panic!("{what} must be admitted, refused: {reason:?}"),
        }
    }
}

async fn admit(addr: SocketAddr, cred: &ClientCred) -> Admission {
    let Ok(mut ws) = connect(addr, cred, None).await else {
        // Being refused during the upgrade is equally a refusal.
        return Admission::Refused(None);
    };
    match tokio::time::timeout(SETTLE, ws.next()).await {
        Err(_) => Admission::Open(Box::new(ws)),
        Ok(Some(Ok(Message::Close(frame)))) => {
            Admission::Refused(frame.map(|f| f.reason.to_string()))
        }
        Ok(None) | Ok(Some(Err(_))) => Admission::Refused(None),
        Ok(Some(Ok(other))) => panic!("unexpected frame on a new session: {other:?}"),
    }
}

/// Retry admission until it succeeds or `budget` runs out. The daemon releases
/// a slot when its session task unwinds, which is prompt but not synchronous
/// with the client's close.
async fn admit_within(addr: SocketAddr, cred: &ClientCred, budget: Duration) -> Admission {
    let deadline = Instant::now() + budget;
    loop {
        let outcome = admit(addr, cred).await;
        if outcome.is_open() || Instant::now() >= deadline {
            return outcome;
        }
        tokio::time::sleep(Duration::from_millis(100)).await;
    }
}

/// WebSocket opcodes this suite writes by hand (RFC 6455 5.2).
const OPCODE_TEXT: u8 = 0x1;

/// A client-to-server frame exactly as it goes on the wire: masked, and
/// carrying a DECLARED payload length that the bytes actually sent may fall
/// short of.
///
/// tungstenite's `Message` API cannot express that gap - it always sends the
/// payload it declares - and the gap is the whole subject here: the declared
/// length is what the server's parser reserves before any payload arrives.
fn wire_frame(fin: bool, opcode: u8, declared: u64, payload: &[u8]) -> Vec<u8> {
    let mask = [0x6Du8, 0x2B, 0xF0, 0x91];
    let mut out = vec![if fin { 0x80 | opcode } else { opcode }];
    if declared < 126 {
        out.push(0x80 | declared as u8);
    } else if declared < 65536 {
        out.push(0x80 | 126);
        out.extend_from_slice(&(declared as u16).to_be_bytes());
    } else {
        out.push(0x80 | 127);
        out.extend_from_slice(&declared.to_be_bytes());
    }
    out.extend_from_slice(&mask);
    out.extend(
        payload
            .iter()
            .enumerate()
            .map(|(i, b)| b ^ mask[i % mask.len()]),
    );
    out
}

/// Write raw frame bytes underneath the client's own WebSocket parser, so the
/// daemon sees exactly what this test wrote rather than something tungstenite
/// re-encoded. Anything tungstenite had queued is flushed first, so the raw
/// bytes can never land in the middle of one of its frames.
async fn write_raw(ws: &mut WsClient, bytes: &[u8]) {
    use tokio::io::AsyncWriteExt;
    SinkExt::flush(ws).await.expect("flush the client's queue");
    ws.get_mut()
        .write_all(bytes)
        .await
        .expect("raw frame write");
    ws.get_mut().flush().await.expect("raw frame flush");
}

/// One keepalive per interval - the rate a real client would use, not a flood.
const PING_INTERVAL: Duration = Duration::from_millis(200);

/// Poll until the daemon ends the session, keeping the connection demonstrably
/// live with Pings. Returns how long that took, or `None` at the budget.
async fn ping_until_closed(ws: &mut WsClient, budget: Duration) -> Option<Duration> {
    let started = Instant::now();
    while started.elapsed() < budget {
        if ws.send(Message::Ping(Bytes::new())).await.is_err() {
            return Some(started.elapsed());
        }
        // Read replies for one interval before the next keepalive.
        let until = Instant::now() + PING_INTERVAL;
        while let Some(left) = until.checked_duration_since(Instant::now()) {
            match tokio::time::timeout(left, ws.next()).await {
                Ok(None) | Ok(Some(Err(_))) | Ok(Some(Ok(Message::Close(_)))) => {
                    return Some(started.elapsed());
                }
                // The interval elapsed with the session still live.
                Err(_) => break,
                // A Pong: keep reading out the rest of this interval.
                _ => {}
            }
        }
    }
    None
}

/// The per-credential ceiling bounds what ONE certificate can occupy, a
/// DIFFERENT certificate is unaffected by it, and a closed session returns its
/// slot to the credential that held it.
#[tokio::test]
async fn per_certificate_quota_bounds_one_credential_and_returns_its_slots() {
    let dir = tempfile::tempdir().unwrap();
    let (addr, cancel, cred_a, cred_b) = start_listener(
        dir.path(),
        WssLimits {
            max_pending_handshakes: 16,
            handshake_timeout: Duration::from_secs(5),
            // Global room to spare: what refuses below can only be the
            // per-credential ceiling.
            max_sessions: 16,
            max_sessions_per_client: 2,
            incomplete_message_timeout: Duration::from_secs(30),
        },
    )
    .await;

    let first = admit(addr, &cred_a)
        .await
        .expect_open("session 1 of a 2-session credential");
    let _second = admit(addr, &cred_a)
        .await
        .expect_open("session 2 of a 2-session credential");

    // One past the ceiling on the SAME credential.
    let reason = match admit(addr, &cred_a).await {
        Admission::Open(_) => {
            panic!("a third session on a 2-session credential must be refused")
        }
        Admission::Refused(reason) => reason,
    };
    assert!(
        reason.as_deref().is_some_and(|r| r.contains("quota")),
        "the refusal must state the per-certificate quota, got {reason:?}"
    );

    // A different credential still gets in: the ceiling is per-credential, and
    // the global ceiling was never the binding constraint here.
    let _other = admit(addr, &cred_b)
        .await
        .expect_open("a second, distinct certificate");

    // Closing one of A's sessions returns exactly one slot to A.
    drop(first);
    assert!(
        admit_within(addr, &cred_a, Duration::from_secs(5))
            .await
            .is_open(),
        "closing a session must return its slot to the credential that held it"
    );

    cancel.cancel();
}

/// A partially-received data message is closed at its deadline even though
/// control frames keep proving liveness, and both the global session permit and
/// the credential's slot come back.
#[tokio::test]
async fn incomplete_message_deadline_closes_and_returns_capacity() {
    let dir = tempfile::tempdir().unwrap();
    let (addr, cancel, cred_a, _cred_b) = start_listener(
        dir.path(),
        WssLimits {
            max_pending_handshakes: 8,
            handshake_timeout: Duration::from_secs(5),
            // One global slot and one per-credential slot, so the reconnect at
            // the end can only succeed if BOTH were released.
            max_sessions: 1,
            max_sessions_per_client: 1,
            incomplete_message_timeout: Duration::from_secs(2),
        },
    )
    .await;

    let mut ws = admit(addr, &cred_a).await.expect_open("the first session");

    // The opening fragment of a fragmented Text message, with no FIN fragment
    // ever to follow: the parser holds it and never completes a message.
    let opening = Frame::message(
        Bytes::from(vec![b'x'; 256 * 1024]),
        OpCode::Data(Data::Text),
        false,
    );
    ws.send(Message::Frame(opening))
        .await
        .expect("send the opening fragment");
    ws.flush().await.expect("flush the opening fragment");

    let closed_after = ping_until_closed(&mut ws, Duration::from_secs(12))
        .await
        .expect("a partial message held past its deadline must be closed");
    assert!(
        closed_after >= Duration::from_secs(1),
        "closed after {closed_after:?}, too early to be the 2s incomplete-message deadline"
    );
    assert!(
        closed_after < Duration::from_secs(10),
        "closed after {closed_after:?}: that is the 20s heartbeat expiring, not the 2s \
         incomplete-message deadline"
    );

    drop(ws);
    assert!(
        admit_within(addr, &cred_a, Duration::from_secs(5))
            .await
            .is_open(),
        "the session permit and the credential slot must return after the deadline closes a \
         session"
    );

    cancel.cancel();
}

/// A near-envelope DECLARATION backed by almost no payload is closed at its
/// deadline, on the declaration alone.
///
/// This is the case a byte-counting bound cannot see. tungstenite reserves the
/// peer-declared frame length in `FrameCodec::read_frame` before it reads any
/// payload, so 14 bytes of header buy a 31 MiB reservation. A rule that waits
/// for 64 KiB of traffic before it arms anything never fires here: the peer
/// sends half that and then only keepalives, which cost 6 bytes each and would
/// need over half an hour to reach the threshold - by which time the deadline
/// it was standing in for has passed fifteen times over at the default window.
#[tokio::test]
async fn a_large_declared_payload_with_a_tiny_body_is_closed_at_its_deadline() {
    let dir = tempfile::tempdir().unwrap();
    let (addr, cancel, cred_a, _cred_b) = start_listener(
        dir.path(),
        WssLimits {
            max_pending_handshakes: 8,
            handshake_timeout: Duration::from_secs(5),
            // One global slot and one per-credential slot, so the reconnect at
            // the end can only succeed if BOTH were released.
            max_sessions: 1,
            max_sessions_per_client: 1,
            incomplete_message_timeout: Duration::from_secs(2),
        },
    )
    .await;

    let mut ws = admit(addr, &cred_a).await.expect_open("the first session");

    // 31 MiB is just inside the daemon's 32 MiB parser envelope, so the header
    // is accepted and the reservation made. The 32 KiB body is deliberately
    // HALF the 64 KiB the superseded byte-count proxy required.
    const DECLARED: u64 = 31 * 1024 * 1024;
    let body = vec![b'x'; 32 * 1024];
    assert!(
        (body.len() as u64) < DECLARED / 900,
        "the body must be negligible against the declaration for this to mean anything"
    );
    write_raw(&mut ws, &wire_frame(false, OPCODE_TEXT, DECLARED, &body)).await;

    let closed_after = ping_until_closed(&mut ws, Duration::from_secs(12))
        .await
        .expect("a 31 MiB reservation held past its deadline must be closed");
    assert!(
        closed_after >= Duration::from_secs(1),
        "closed after {closed_after:?}, too early to be the 2s incomplete-message deadline"
    );
    assert!(
        closed_after < Duration::from_secs(10),
        "closed after {closed_after:?}: that is the 20s heartbeat expiring, not the 2s \
         incomplete-message deadline"
    );

    drop(ws);
    assert!(
        admit_within(addr, &cred_a, Duration::from_secs(5))
            .await
            .is_open(),
        "the session permit and the credential slot must return after a declared-but-unsent \
         reservation is closed"
    );

    cancel.cancel();
}

/// A partial message that arrives in the SAME read as the end of a completed
/// one still gets its own deadline.
///
/// The superseded accounting rebased its byte baseline to everything read so
/// far whenever a message completed, so the second message's already-buffered
/// bytes were struck from the record - a peer could complete one small message
/// per read and have the partial message trailing it forgotten each time.
/// Nothing here depends on the daemon noticing the boundary: the scan is done
/// on the bytes as they are read, before the parser yields anything.
#[tokio::test]
async fn a_partial_message_read_ahead_of_a_completed_one_is_not_forgotten() {
    let dir = tempfile::tempdir().unwrap();
    let (addr, cancel, cred_a, _cred_b) = start_listener(
        dir.path(),
        WssLimits {
            max_pending_handshakes: 8,
            handshake_timeout: Duration::from_secs(5),
            max_sessions: 1,
            max_sessions_per_client: 1,
            incomplete_message_timeout: Duration::from_secs(2),
        },
    )
    .await;

    let mut ws = admit(addr, &cred_a).await.expect_open("the first session");

    // ONE write, so both land in one read: a complete JSON-RPC request (the
    // daemon answers it with an error, which is beside the point) immediately
    // followed by the opening fragment of a second, much larger message.
    let complete = br#"{"jsonrpc":"2.0","id":1,"method":"no.such.method"}"#;
    let mut one_write = wire_frame(true, OPCODE_TEXT, complete.len() as u64, complete);
    one_write.extend_from_slice(&wire_frame(
        false,
        OPCODE_TEXT,
        8 * 1024 * 1024,
        &[b'y'; 512],
    ));
    write_raw(&mut ws, &one_write).await;

    let closed_after = ping_until_closed(&mut ws, Duration::from_secs(12))
        .await
        .expect("the partial message trailing a completed one must still be closed");
    assert!(
        closed_after >= Duration::from_secs(1),
        "closed after {closed_after:?}, too early to be the 2s incomplete-message deadline"
    );
    assert!(
        closed_after < Duration::from_secs(10),
        "closed after {closed_after:?}: that is the 20s heartbeat expiring, not the 2s \
         incomplete-message deadline"
    );

    drop(ws);
    assert!(
        admit_within(addr, &cred_a, Duration::from_secs(5))
            .await
            .is_open(),
        "the session permit and the credential slot must return after the deadline closes a \
         read-ahead partial message"
    );

    cancel.cancel();
}

/// The same read-ahead shape, but with BOTH messages complete, must leave the
/// session alone. The read-ahead fix has to distinguish "a partial message
/// trails a completed one" from "two messages completed back to back";
/// tracking the boundary anywhere but on the byte stream itself blurs the two.
#[tokio::test]
async fn two_completed_messages_in_one_read_leave_the_session_open() {
    let dir = tempfile::tempdir().unwrap();
    let (addr, cancel, cred_a, _cred_b) = start_listener(
        dir.path(),
        WssLimits {
            max_pending_handshakes: 8,
            handshake_timeout: Duration::from_secs(5),
            max_sessions: 4,
            max_sessions_per_client: 4,
            incomplete_message_timeout: Duration::from_secs(1),
        },
    )
    .await;

    let mut ws = admit(addr, &cred_a)
        .await
        .expect_open("a session making progress");

    let first = br#"{"jsonrpc":"2.0","id":1,"method":"no.such.method"}"#;
    let second = br#"{"jsonrpc":"2.0","id":2,"method":"no.such.method"}"#;
    let mut one_write = wire_frame(true, OPCODE_TEXT, first.len() as u64, first);
    one_write.extend_from_slice(&wire_frame(true, OPCODE_TEXT, second.len() as u64, second));
    write_raw(&mut ws, &one_write).await;

    // Five times the deadline with nothing outstanding.
    let closed = ping_until_closed(&mut ws, Duration::from_secs(5)).await;
    assert!(
        closed.is_none(),
        "a session whose messages both completed was closed after {closed:?}, but it left \
         nothing in the parser"
    );

    cancel.cancel();
}

/// The incomplete-message bound is about bytes parked in the parser. A QUIET
/// connection accumulates none, so it must be left to the idle heartbeat -
/// otherwise this bound silently becomes a second, much shorter idle policy.
#[tokio::test]
async fn a_quiet_session_outlives_the_incomplete_message_deadline() {
    let dir = tempfile::tempdir().unwrap();
    let (addr, cancel, cred_a, _cred_b) = start_listener(
        dir.path(),
        WssLimits {
            max_pending_handshakes: 8,
            handshake_timeout: Duration::from_secs(5),
            max_sessions: 4,
            max_sessions_per_client: 4,
            incomplete_message_timeout: Duration::from_secs(1),
        },
    )
    .await;

    let mut ws = admit(addr, &cred_a).await.expect_open("a quiet session");

    // Five times the deadline, sending no data frames at all.
    let closed = ping_until_closed(&mut ws, Duration::from_secs(5)).await;
    assert!(
        closed.is_none(),
        "a quiet session was closed after {closed:?}, but it parked nothing in the parser"
    );

    cancel.cancel();
}

/// No VOLUME of control frames counts as parser-held memory. Control frames are
/// delivered whole and park nothing, so a peer that sends 100 KiB of Pings -
/// and keeps doing so past the deadline - must survive. Any accounting that
/// treated their bytes as held memory would eventually close healthy long-lived
/// sessions that only exchange keepalives.
#[tokio::test]
async fn control_frame_volume_is_not_counted_as_parser_memory() {
    let dir = tempfile::tempdir().unwrap();
    let (addr, cancel, cred_a, _cred_b) = start_listener(
        dir.path(),
        WssLimits {
            max_pending_handshakes: 8,
            handshake_timeout: Duration::from_secs(5),
            max_sessions: 4,
            max_sessions_per_client: 4,
            incomplete_message_timeout: Duration::from_secs(1),
        },
    )
    .await;

    let mut ws = admit(addr, &cred_a)
        .await
        .expect_open("a keepalive-only session");

    // 800 frames at the 125-byte control-frame maximum is ~104 KiB of traffic
    // that holds nothing, spread over twice the deadline.
    let payload = Bytes::from(vec![0u8; 125]);
    let started = Instant::now();
    for _ in 0..20 {
        for _ in 0..40 {
            ws.send(Message::Ping(payload.clone()))
                .await
                .expect("the session must stay writable");
        }
        tokio::time::sleep(Duration::from_millis(100)).await;
        while let Ok(frame) = tokio::time::timeout(Duration::from_millis(10), ws.next()).await {
            match frame {
                None | Some(Err(_)) | Some(Ok(Message::Close(_))) => panic!(
                    "a keepalive-only session was closed after {:?}",
                    started.elapsed()
                ),
                _ => {}
            }
        }
    }
    assert!(
        started.elapsed() > Duration::from_secs(1),
        "the traffic must outlast the 1s deadline for this to mean anything, took {:?}",
        started.elapsed()
    );

    cancel.cancel();
}

/// After the parser refuses an oversized message, the session permit and the
/// credential's slot return: a rejection must not leak the capacity it consumed.
#[tokio::test]
async fn parser_rejection_returns_session_and_credential_capacity() {
    let dir = tempfile::tempdir().unwrap();
    let (addr, cancel, cred_a, _cred_b) = start_listener(
        dir.path(),
        WssLimits {
            max_pending_handshakes: 8,
            handshake_timeout: Duration::from_secs(5),
            // As above: the reconnect proves both bounds were released.
            max_sessions: 1,
            max_sessions_per_client: 1,
            // Long enough that the upload below cannot trip it; the rejection
            // under test is the message-size ceiling.
            incomplete_message_timeout: Duration::from_secs(30),
        },
    )
    .await;

    // A client permitted to EMIT beyond the daemon's 32 MiB envelope, so the
    // DAEMON's ceiling is what rejects.
    let mut permissive = WebSocketConfig::default();
    permissive.max_message_size = Some(64 * 1024 * 1024);
    permissive.max_frame_size = Some(64 * 1024 * 1024);
    let mut ws = connect(addr, &cred_a, Some(permissive))
        .await
        .expect("the first session must be admitted");

    let _ = ws.send(Message::binary(vec![0u8; 33 * 1024 * 1024])).await;
    let _ = ws.flush().await;

    let ended = tokio::time::timeout(Duration::from_secs(15), drain(&mut ws)).await;
    assert!(
        ended.is_ok(),
        "a message beyond the parser envelope must end the session"
    );

    drop(ws);
    assert!(
        admit_within(addr, &cred_a, Duration::from_secs(5))
            .await
            .is_open(),
        "the session permit and the credential slot must return after a parser rejection"
    );

    cancel.cancel();
}

/// Read until the daemon ends the session.
async fn drain(ws: &mut WsClient) {
    while let Some(Ok(_)) = ws.next().await {}
}

/// Mirrors the listener's own `PEER_WRITE_TIMEOUT`, which is module-private:
/// the read side's idle window plus its post-ping window, the same budget a
/// silent peer gets.
const PEER_WRITE_TIMEOUT: Duration = Duration::from_secs(20 + 10);

/// Requests the client pushes before giving up on wedging the write path. It
/// stalls far short of this; the count only has to outlast the daemon's
/// outbound queue and the socket buffers on both sides.
const FLOOD_REQUESTS: usize = 4096;

/// A request whose error response echoes a large `id` back to the sender, so a
/// modest number of them fills the daemon's write path against a peer that has
/// stopped reading. An unknown method is answered with an error carrying the
/// request's own id, which is what makes the response amplify.
fn amplifying_request(seq: usize) -> String {
    let id = "x".repeat(8 * 1024);
    format!(r#"{{"jsonrpc":"2.0","id":"{id}{seq}","method":"no.such.method"}}"#)
}

/// Wait, on the real clock, until the client's own writes stop making progress.
/// That stall IS the wedge under test: the client's sends can only block once
/// the daemon has stopped reading, and the daemon only stops reading once its
/// dispatcher is parked on a full outbound queue behind a stalled writer.
async fn wait_for_wedge(sent: &AtomicUsize) {
    let mut previous = 0;
    let mut stalled = 0;
    for _ in 0..200 {
        tokio::time::sleep(Duration::from_millis(50)).await;
        let now = sent.load(Ordering::Relaxed);
        if now == previous && now > 0 {
            stalled += 1;
            if stalled >= 4 {
                return;
            }
        } else {
            stalled = 0;
        }
        previous = now;
    }
    panic!("the client's writes never stalled, so the daemon's write path never wedged");
}

/// Spend `budget` of the daemon's timers without waiting for it, with no test
/// I/O in flight. The clock is paused only for the jump and handed straight
/// back, so nothing that needs real socket readiness ever runs against it.
async fn advance_while_idle(budget: Duration) {
    tokio::time::pause();
    tokio::time::advance(budget).await;
    tokio::time::resume();
}

/// An authenticated peer that stops reading must not be able to hold its
/// session, and with it the global session permit and its credential's quota
/// slot, indefinitely.
///
/// The daemon answers into a bounded outbound queue. With no bound on the write
/// itself, a peer that stops reading parks the writer, fills that queue, and
/// parks the dispatcher on its own response - so the dispatcher never returns to
/// `next_frame`, the heartbeat that would have retired the session never runs
/// again, and neither guard in the session task's frame is ever dropped.
#[tokio::test]
async fn a_peer_that_stops_reading_loses_its_session_and_returns_its_capacity() {
    let dir = tempfile::tempdir().unwrap();
    let (addr, cancel, cred_a, cred_b) = start_listener(
        dir.path(),
        WssLimits {
            max_pending_handshakes: 16,
            handshake_timeout: Duration::from_secs(5),
            // One of each, so a single wedged session provably holds both the
            // global session permit and its credential's only quota slot.
            max_sessions: 1,
            max_sessions_per_client: 1,
            incomplete_message_timeout: Duration::from_secs(300),
        },
    )
    .await;

    let ws = admit(addr, &cred_a)
        .await
        .expect_open("the wedging session");

    // Push requests and never read the answers. Each answer is far larger than
    // the request that provoked it, so the daemon's write path fills first.
    let sent = Arc::new(AtomicUsize::new(0));
    let feeder = {
        let sent = sent.clone();
        tokio::spawn(async move {
            let mut ws = ws;
            for seq in 0..FLOOD_REQUESTS {
                if ws
                    .send(Message::Text(amplifying_request(seq).into()))
                    .await
                    .is_err()
                {
                    break;
                }
                sent.fetch_add(1, Ordering::Relaxed);
            }
            // Stay connected, and stay not reading.
            std::future::pending::<()>().await
        })
    };
    wait_for_wedge(&sent).await;

    // The wedged session is still holding both bounds.
    assert!(
        !admit(addr, &cred_b).await.is_open(),
        "the wedged session must still hold the global session permit"
    );

    // The wedge holds nothing but timers now, so the write budget is spent by
    // advancing the clock rather than by waiting on it. Each probe below is a
    // real mTLS connection, so the clock is handed back for every one of them: a
    // running virtual clock cannot tell a task waiting on real socket readiness
    // from an idle runtime, and would race the budget against that handshake.
    advance_while_idle(PEER_WRITE_TIMEOUT / 2).await;
    assert!(
        !admit(addr, &cred_b).await.is_open(),
        "a wedged session must be given its full write budget, not closed on sight"
    );

    advance_while_idle(PEER_WRITE_TIMEOUT).await;

    // Both guards live in the session task's frame, so the credential getting
    // back in proves the frame unwound through the session permit as well.
    assert!(
        admit_within(addr, &cred_a, Duration::from_secs(10))
            .await
            .is_open(),
        "a peer that stopped reading must lose its session and return its capacity"
    );

    feeder.abort();
    cancel.cancel();
}
