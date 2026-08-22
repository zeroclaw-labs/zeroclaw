//! Admission + node-id binding tests for the designed relay protocol: the signed
//! Ed25519 `Hello`/`Challenge`/`Register` handshake over the outer TLS + WS, the
//! open/allowlist policy (keyed on pubkey fingerprint, deny wins), and the
//! node-id<->pubkey binding that stops a different key hijacking a live node-id.
#![allow(clippy::disallowed_methods)]

use std::collections::HashSet;
use std::sync::Arc;

use base64::Engine as _;
use base64::engine::general_purpose::STANDARD as B64;
use futures_util::{SinkExt, StreamExt};
use ring::signature::{Ed25519KeyPair, KeyPair};
use sha2::{Digest, Sha256};
use tokio::net::TcpListener;
use tokio_rustls::TlsAcceptor;
use tokio_tungstenite::tungstenite::{ClientRequestBuilder, Message};
use zeroclaw_relay_proto::{Control, PEER_HINT_ENROLL, SUBPROTOCOL, decode_data, encode_data};
use zerorelay::{Admission, RelayConfig, RelayServer};

type RelayWs =
    tokio_tungstenite::WebSocketStream<tokio_rustls::client::TlsStream<tokio::net::TcpStream>>;

#[derive(Debug)]
struct NoVerify;
impl rustls::client::danger::ServerCertVerifier for NoVerify {
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

fn gen_key() -> Vec<u8> {
    let rng = ring::rand::SystemRandom::new();
    Ed25519KeyPair::generate_pkcs8(&rng)
        .unwrap()
        .as_ref()
        .to_vec()
}

fn fingerprint(pkcs8: &[u8]) -> String {
    let kp = Ed25519KeyPair::from_pkcs8(pkcs8).unwrap();
    hex::encode(Sha256::digest(kp.public_key().as_ref()))
}

/// Start a relay with the given policy and its own (self-signed) outer TLS cert.
async fn start_relay(cfg: RelayConfig) -> std::net::SocketAddr {
    start_relay_handle(cfg).await.0
}

/// Like [`start_relay`] but also returns the live `RelayServer` handle so a test
/// can read its status snapshot.
async fn start_relay_handle(cfg: RelayConfig) -> (std::net::SocketAddr, RelayServer) {
    let _ = rustls::crypto::ring::default_provider().install_default();
    let dir = tempfile::tempdir().unwrap();
    let mats = zeroclaw_tls::ensure_server_materials(dir.path(), &[]).unwrap();
    let certs = zeroclaw_tls::load_certs(mats.server_cert_path.to_str().unwrap()).unwrap();
    let key = zeroclaw_tls::load_private_key(mats.server_key_path.to_str().unwrap()).unwrap();
    let server_cfg = rustls::ServerConfig::builder_with_provider(Arc::new(
        rustls::crypto::ring::default_provider(),
    ))
    .with_safe_default_protocol_versions()
    .unwrap()
    .with_no_client_auth()
    .with_single_cert(certs, key)
    .unwrap();
    let acceptor = TlsAcceptor::from(Arc::new(server_cfg));
    // Keep the tempdir (and its cert files) alive for the relay's lifetime.
    std::mem::forget(dir);
    let listener = TcpListener::bind("127.0.0.1:0").await.unwrap();
    let addr = listener.local_addr().unwrap();
    let server = RelayServer::new(cfg);
    tokio::spawn(server.clone().serve(listener, acceptor));
    (addr, server)
}

fn insecure_client_config() -> Arc<rustls::ClientConfig> {
    Arc::new(
        rustls::ClientConfig::builder_with_provider(Arc::new(
            rustls::crypto::ring::default_provider(),
        ))
        .with_safe_default_protocol_versions()
        .unwrap()
        .dangerous()
        .with_custom_certificate_verifier(Arc::new(NoVerify))
        .with_no_client_auth(),
    )
}

async fn next_control(ws: &mut RelayWs) -> Option<Control> {
    while let Some(msg) = ws.next().await {
        match msg {
            Ok(Message::Text(t)) => return Control::from_json(t.as_str()).ok(),
            Ok(Message::Ping(p)) => {
                let _ = ws.send(Message::Pong(p)).await;
            }
            Ok(Message::Pong(_)) => {}
            _ => return None,
        }
    }
    None
}

/// Run the signed registration handshake. When `valid_sig` is false a corrupted
/// signature is sent. Returns the live socket and the terminal control frame.
async fn handshake(
    relay_addr: std::net::SocketAddr,
    node_id: &str,
    pkcs8: &[u8],
    token: Option<&str>,
    valid_sig: bool,
) -> (RelayWs, Control) {
    let kp = Ed25519KeyPair::from_pkcs8(pkcs8).unwrap();
    let pubkey = kp.public_key().as_ref().to_vec();
    let tcp = tokio::net::TcpStream::connect(relay_addr).await.unwrap();
    let connector = tokio_rustls::TlsConnector::from(insecure_client_config());
    let sni = rustls::pki_types::ServerName::try_from("localhost").unwrap();
    let tls = connector.connect(sni, tcp).await.unwrap();
    let req = ClientRequestBuilder::new("wss://localhost/".parse().unwrap())
        .with_sub_protocol(SUBPROTOCOL);
    let (mut ws, _) = tokio_tungstenite::client_async_with_config(req, tls, None)
        .await
        .unwrap();

    ws.send(Message::text(
        Control::Hello {
            daemon_pubkey: B64.encode(&pubkey),
            node_id: node_id.to_string(),
            relay_token: token.map(|s| s.to_string()),
        }
        .to_json(),
    ))
    .await
    .unwrap();

    let nonce = match next_control(&mut ws).await {
        Some(Control::Challenge { nonce }) => B64.decode(nonce.as_bytes()).unwrap(),
        Some(other) => return (ws, other), // e.g. forbidden before challenge
        None => panic!("relay closed before challenge"),
    };
    let sig = kp.sign(&nonce);
    let sig_b64 = if valid_sig {
        B64.encode(sig.as_ref())
    } else {
        let mut bad = sig.as_ref().to_vec();
        bad[0] ^= 0xff;
        B64.encode(bad)
    };
    ws.send(Message::text(
        Control::Register {
            node_id: node_id.to_string(),
            sig: sig_b64,
        }
        .to_json(),
    ))
    .await
    .unwrap();

    let term = next_control(&mut ws).await.expect("terminal frame");
    (ws, term)
}

/// Complete the outer TLS + WS upgrade and send a syntactically valid `Hello`,
/// then STOP - never sending the `Register` that completes the signed exchange.
/// A `Hello` is unauthenticated, so this is the cheapest state an unauthorized
/// peer can park the relay in.
async fn hello_only(relay_addr: std::net::SocketAddr, node_id: &str, pkcs8: &[u8]) -> RelayWs {
    let kp = Ed25519KeyPair::from_pkcs8(pkcs8).unwrap();
    let mut ws = connect_ws(relay_addr).await;
    ws.send(Message::text(
        Control::Hello {
            daemon_pubkey: B64.encode(kp.public_key().as_ref()),
            node_id: node_id.to_string(),
            relay_token: None,
        }
        .to_json(),
    ))
    .await
    .unwrap();
    ws
}

/// Open an outer TLS + WS connection to the relay WITHOUT registering (the client
/// role: it only sends a `Connect`).
async fn connect_ws(relay_addr: std::net::SocketAddr) -> RelayWs {
    let tcp = tokio::net::TcpStream::connect(relay_addr).await.unwrap();
    let connector = tokio_rustls::TlsConnector::from(insecure_client_config());
    let sni = rustls::pki_types::ServerName::try_from("localhost").unwrap();
    let tls = connector.connect(sni, tcp).await.unwrap();
    let req = ClientRequestBuilder::new("wss://localhost/".parse().unwrap())
        .with_sub_protocol(SUBPROTOCOL);
    let (ws, _) = tokio_tungstenite::client_async_with_config(req, tls, None)
        .await
        .unwrap();
    ws
}

/// A wire message read off a relay socket: either a binary DATA frame
/// `(conn_id, payload)` or a control frame.
#[derive(Debug)]
enum Wire {
    Data(u64, Vec<u8>),
    Ctrl(Control),
}

/// Read the next DATA or control message (answering pings), with a timeout so a
/// stuck test fails fast instead of hanging.
async fn next_wire(ws: &mut RelayWs) -> Option<Wire> {
    let read = async {
        while let Some(msg) = ws.next().await {
            match msg {
                Ok(Message::Text(t)) => return Control::from_json(t.as_str()).ok().map(Wire::Ctrl),
                Ok(Message::Binary(b)) => {
                    return decode_data(&b).map(|(c, p)| Wire::Data(c, p.to_vec()));
                }
                Ok(Message::Ping(p)) => {
                    let _ = ws.send(Message::Pong(p)).await;
                }
                Ok(Message::Pong(_)) => {}
                _ => return None,
            }
        }
        None
    };
    tokio::time::timeout(std::time::Duration::from_secs(5), read)
        .await
        .unwrap_or(None)
}

/// Register a daemon, connect a client, and drive the Connect -> Open -> Opened
/// pairing. Returns the live daemon + client sockets and the paired `conn_id`.
async fn pair_daemon_and_client(
    relay_addr: std::net::SocketAddr,
    node_id: &str,
) -> (RelayWs, RelayWs, u64) {
    let key = gen_key();
    let (mut daemon, term) = handshake(relay_addr, node_id, &key, None, true).await;
    assert!(matches!(term, Control::Registered { .. }), "got {term:?}");

    let mut client = connect_ws(relay_addr).await;
    client
        .send(Message::text(
            Control::Connect {
                node_id: node_id.to_string(),
            }
            .to_json(),
        ))
        .await
        .unwrap();

    // The relay asks the daemon to open a logical conn; accept it.
    let conn_id = loop {
        match next_wire(&mut daemon).await {
            Some(Wire::Ctrl(Control::Open { conn_id, .. })) => break conn_id,
            Some(_) => {}
            None => panic!("daemon never received Open"),
        }
    };
    daemon
        .send(Message::text(Control::Opened { conn_id }.to_json()))
        .await
        .unwrap();
    // Client must see the route open before exchanging bytes.
    match next_wire(&mut client).await {
        Some(Wire::Ctrl(Control::Opened { conn_id: c })) => assert_eq!(c, conn_id),
        other => panic!("client did not see Opened: {other:?}"),
    }
    (daemon, client, conn_id)
}

#[tokio::test]
async fn enrollment_client_opens_daemon_with_enroll_hint() {
    let addr = start_relay(RelayConfig::default()).await;
    let key = gen_key();
    let (mut daemon, term) = handshake(addr, "node-enroll", &key, None, true).await;
    assert!(matches!(term, Control::Registered { .. }), "got {term:?}");

    let mut client = connect_ws(addr).await;
    client
        .send(Message::text(
            Control::Enroll {
                node_id: "node-enroll".into(),
            }
            .to_json(),
        ))
        .await
        .unwrap();

    let conn_id = match next_wire(&mut daemon).await {
        Some(Wire::Ctrl(Control::Open { conn_id, peer_hint })) => {
            assert_eq!(peer_hint.as_deref(), Some(PEER_HINT_ENROLL));
            conn_id
        }
        other => panic!("daemon did not receive an enrollment Open: {other:?}"),
    };
    daemon
        .send(Message::text(Control::Opened { conn_id }.to_json()))
        .await
        .unwrap();
    match next_wire(&mut client).await {
        Some(Wire::Ctrl(Control::Opened { conn_id: c })) => assert_eq!(c, conn_id),
        other => panic!("client did not see enrollment Opened: {other:?}"),
    }
}

#[tokio::test]
async fn relay_forwards_flow_control_frames_both_ways() {
    let addr = start_relay(RelayConfig::default()).await;
    let (mut daemon, mut client, conn_id) = pair_daemon_and_client(addr, "node-fc").await;

    // Daemon -> client: a Window grant must reach the client unchanged.
    daemon
        .send(Message::text(
            Control::Window {
                conn_id,
                credit: 8192,
            }
            .to_json(),
        ))
        .await
        .unwrap();
    match next_wire(&mut client).await {
        Some(Wire::Ctrl(Control::Window { conn_id: c, credit })) => {
            assert_eq!(c, conn_id);
            assert_eq!(credit, 8192);
        }
        other => panic!("client did not receive the forwarded Window: {other:?}"),
    }

    // Client -> daemon: a DATA frame is blind-forwarded with the authoritative
    // conn_id, and a DataAck control frame is forwarded too.
    client
        .send(Message::binary(encode_data(conn_id, b"ping")))
        .await
        .unwrap();
    client
        .send(Message::text(
            Control::DataAck {
                conn_id,
                consumed: 4,
            }
            .to_json(),
        ))
        .await
        .unwrap();

    match next_wire(&mut daemon).await {
        Some(Wire::Data(c, p)) => {
            assert_eq!(c, conn_id, "conn_id re-stamped authoritatively");
            assert_eq!(p, b"ping");
        }
        other => panic!("daemon did not receive forwarded DATA: {other:?}"),
    }
    match next_wire(&mut daemon).await {
        Some(Wire::Ctrl(Control::DataAck {
            conn_id: c,
            consumed,
        })) => {
            assert_eq!(c, conn_id);
            assert_eq!(consumed, 4);
        }
        other => panic!("daemon did not receive forwarded DataAck: {other:?}"),
    }
}

#[tokio::test]
async fn relay_closes_a_client_that_floods_past_its_window() {
    // A1/A6: a client that ships far more than its granted send window (the daemon
    // never acks) is ignoring flow control. The relay tears the conn down rather
    // than buffering unboundedly onto the shared daemon link.
    let addr = start_relay(RelayConfig::default()).await;
    let (_daemon, mut client, conn_id) = pair_daemon_and_client(addr, "node-flood").await;

    // Flood ~1 MiB in 64 KiB frames without ever acking; the seeded window plus
    // tolerance is 2 * INITIAL_WINDOW (512 KiB), so this must trip the guard.
    let chunk = vec![0u8; 64 * 1024];
    let mut tripped = false;
    for _ in 0..20 {
        if client
            .send(Message::binary(encode_data(conn_id, &chunk)))
            .await
            .is_err()
        {
            tripped = true;
            break;
        }
        if let Some(Wire::Ctrl(Control::Error { code, .. })) = next_wire_nowait(&mut client).await {
            assert_eq!(code, "rate_limited");
            tripped = true;
            break;
        }
    }
    assert!(
        tripped,
        "the relay must rate-limit / close a client that overruns its window"
    );
}

/// A non-blocking peek for an already-queued frame (200ms budget), used to notice
/// the relay's rate-limit error mid-flood without stalling the send loop.
async fn next_wire_nowait(ws: &mut RelayWs) -> Option<Wire> {
    let read = async {
        match ws.next().await {
            Some(Ok(Message::Text(t))) => Control::from_json(t.as_str()).ok().map(Wire::Ctrl),
            Some(Ok(Message::Binary(b))) => decode_data(&b).map(|(c, p)| Wire::Data(c, p.to_vec())),
            _ => None,
        }
    };
    tokio::time::timeout(std::time::Duration::from_millis(200), read)
        .await
        .unwrap_or(None)
}

#[tokio::test]
async fn signed_daemon_registers_in_open_mode() {
    let addr = start_relay(RelayConfig::default()).await;
    let key = gen_key();
    let (_ws, term) = handshake(addr, "node-a", &key, None, true).await;
    assert!(
        matches!(term, Control::Registered { ref node_id, .. } if node_id == "node-a"),
        "expected Registered, got {term:?}"
    );
}

#[tokio::test]
async fn bad_signature_is_rejected() {
    let addr = start_relay(RelayConfig::default()).await;
    let key = gen_key();
    let (_ws, term) = handshake(addr, "node-a", &key, None, false).await;
    assert!(
        matches!(term, Control::Error { ref code, .. } if code == "bad_sig"),
        "expected bad_sig error, got {term:?}"
    );
}

#[tokio::test]
async fn allowlist_admits_listed_and_rejects_unlisted() {
    let listed = gen_key();
    let unlisted = gen_key();
    let mut allow = HashSet::new();
    allow.insert(fingerprint(&listed));
    let addr = start_relay(RelayConfig {
        registration_mode: Admission::Allowlist,
        allow,
        ..Default::default()
    })
    .await;

    let (_ws1, ok) = handshake(addr, "node-listed", &listed, None, true).await;
    assert!(
        matches!(ok, Control::Registered { .. }),
        "listed fingerprint must register, got {ok:?}"
    );

    let (_ws2, denied) = handshake(addr, "node-unlisted", &unlisted, None, true).await;
    assert!(
        matches!(denied, Control::Error { ref code, .. } if code == "forbidden"),
        "unlisted fingerprint must be forbidden, got {denied:?}"
    );
}

#[tokio::test]
async fn node_id_is_bound_to_pubkey() {
    let addr = start_relay(RelayConfig::default()).await;
    let key_a = gen_key();
    let key_b = gen_key();

    // Daemon A registers "shared" and HOLDS the connection open.
    let (mut ws_a, term_a) = handshake(addr, "shared", &key_a, None, true).await;
    assert!(matches!(term_a, Control::Registered { .. }));
    tokio::spawn(async move {
        // Keep the registration live (answer pings) for the duration of the test.
        while next_control(&mut ws_a).await.is_some() {}
    });

    // Daemon B (different key) tries to claim the same node-id -> node_taken.
    let (_ws_b, term_b) = handshake(addr, "shared", &key_b, None, true).await;
    assert!(
        matches!(term_b, Control::Error { ref code, .. } if code == "node_taken"),
        "a different key must not hijack a live node-id, got {term_b:?}"
    );
}

#[tokio::test]
async fn replayed_register_over_a_stale_nonce_is_rejected() {
    // A9 (replay): the REGISTER signature is bound to the relay's per-handshake
    // challenge nonce. A signature captured from a prior/forged session (over a
    // stale nonce) does not verify against the fresh challenge, so a replayed
    // REGISTER is refused with bad_sig rather than accepted.
    let addr = start_relay(RelayConfig::default()).await;
    let pkcs8 = gen_key();
    let kp = Ed25519KeyPair::from_pkcs8(&pkcs8).unwrap();
    let pubkey = kp.public_key().as_ref().to_vec();

    let tcp = tokio::net::TcpStream::connect(addr).await.unwrap();
    let connector = tokio_rustls::TlsConnector::from(insecure_client_config());
    let sni = rustls::pki_types::ServerName::try_from("localhost").unwrap();
    let tls = connector.connect(sni, tcp).await.unwrap();
    let req = ClientRequestBuilder::new("wss://localhost/".parse().unwrap())
        .with_sub_protocol(SUBPROTOCOL);
    let (mut ws, _) = tokio_tungstenite::client_async_with_config(req, tls, None)
        .await
        .unwrap();

    ws.send(Message::text(
        Control::Hello {
            daemon_pubkey: B64.encode(&pubkey),
            node_id: "node-replay".into(),
            relay_token: None,
        }
        .to_json(),
    ))
    .await
    .unwrap();

    // Consume the FRESH challenge but sign a stale/captured nonce instead.
    match next_control(&mut ws).await {
        Some(Control::Challenge { .. }) => {}
        other => panic!("expected a challenge, got {other:?}"),
    }
    let stale_nonce = [7u8; 32]; // a nonce from a prior session / forged
    let sig = kp.sign(&stale_nonce);
    ws.send(Message::text(
        Control::Register {
            node_id: "node-replay".into(),
            sig: B64.encode(sig.as_ref()),
        }
        .to_json(),
    ))
    .await
    .unwrap();

    let term = next_control(&mut ws).await.expect("terminal frame");
    assert!(
        matches!(term, Control::Error { ref code, .. } if code == "bad_sig"),
        "a signature over a stale nonce (replay) must be refused, got {term:?}"
    );
}

/// Try to open an outer TLS + WS connection, returning Err when the relay refuses
/// it (e.g. the per-source-IP rate cap drops the socket pre-handshake).
async fn try_connect_ws(
    relay_addr: std::net::SocketAddr,
) -> Result<RelayWs, Box<dyn std::error::Error>> {
    let tcp = tokio::net::TcpStream::connect(relay_addr).await?;
    let connector = tokio_rustls::TlsConnector::from(insecure_client_config());
    let sni = rustls::pki_types::ServerName::try_from("localhost")?;
    let tls = connector.connect(sni, tcp).await?;
    let req = ClientRequestBuilder::new("wss://localhost/".parse()?).with_sub_protocol(SUBPROTOCOL);
    let (ws, _) = tokio_tungstenite::client_async_with_config(req, tls, None).await?;
    Ok(ws)
}

#[tokio::test]
async fn per_source_ip_accept_cap_drops_excess() {
    // A6: a single source IP cannot open unbounded handshakes. With a burst of 3
    // and no refill, only the first few connections from 127.0.0.1 complete; the
    // rest are dropped before the WebSocket handshake.
    let addr = start_relay(RelayConfig {
        accept_burst_per_ip: 3,
        accept_rate_per_ip: 0.0,
        ..Default::default()
    })
    .await;

    let mut ok = 0;
    let mut refused = 0;
    for _ in 0..10 {
        match try_connect_ws(addr).await {
            Ok(ws) => {
                ok += 1;
                drop(ws);
            }
            Err(_) => refused += 1,
        }
    }
    assert!(
        ok <= 3,
        "the per-IP burst (3) must cap completed handshakes, got {ok}"
    );
    assert!(
        refused > 0,
        "excess connections from one IP must be refused"
    );
}

#[tokio::test]
async fn status_counts_live_conns_per_node() {
    // The read-only status surface reflects a live client conn (counts only).
    let (addr, server) = start_relay_handle(RelayConfig::default()).await;
    let (_daemon, _client, _conn_id) = pair_daemon_and_client(addr, "node-metered").await;

    let status = server.status().await;
    let node = status
        .nodes
        .iter()
        .find(|n| n.node_id == "node-metered")
        .expect("the registered node appears in status");
    assert!(node.conns_live >= 1, "a paired client is counted live");
    assert!(
        node.conns_total >= 1,
        "the conn is counted in the lifetime total"
    );
}

#[tokio::test]
async fn per_node_connect_cap_rate_limits() {
    // A6: a flood of Connects to one node-id is rate-limited. With burst 0 the
    // per-node bucket rejects the first connect outright (a 0 allowance disables
    // connects), proving the cap is wired on the Connect path.
    let addr = start_relay(RelayConfig {
        connect_burst_per_node: 0,
        connect_rate_per_node: 0.0,
        ..Default::default()
    })
    .await;
    let key = gen_key();
    let (mut daemon, term) = handshake(addr, "node-capped", &key, None, true).await;
    assert!(matches!(term, Control::Registered { .. }));
    tokio::spawn(async move { while next_control(&mut daemon).await.is_some() {} });

    let mut client = connect_ws(addr).await;
    client
        .send(Message::text(
            Control::Connect {
                node_id: "node-capped".into(),
            }
            .to_json(),
        ))
        .await
        .unwrap();
    match next_wire(&mut client).await {
        Some(Wire::Ctrl(Control::Error { code, .. })) => assert_eq!(code, "rate_limited"),
        other => panic!("expected rate_limited, got {other:?}"),
    }
}

/// Slowloris regression (July review, finding: stalled handshakes had no
/// deadline or global cap). Sockets that stall before classification must
/// (a) shed NEW sockets while the pre-classification pool is full, rather than
/// queueing unbounded TLS/parser/task state, and (b) be reaped at the
/// handshake deadline so the pool recovers and a well-behaved client succeeds.
#[tokio::test]
async fn stalled_handshakes_shed_then_recover_at_the_deadline() {
    let cfg = RelayConfig {
        max_pending_handshakes: 4,
        handshake_timeout: std::time::Duration::from_secs(2),
        // Keep the per-IP bucket out of the way: this test exercises the
        // GLOBAL bound, and every socket here shares 127.0.0.1.
        accept_burst_per_ip: 1000,
        accept_rate_per_ip: 1000.0,
        ..RelayConfig::default()
    };
    let addr = start_relay(cfg).await;

    // Fill the pool with sockets that never even start TLS.
    let mut stalled = Vec::new();
    for _ in 0..4 {
        stalled.push(tokio::net::TcpStream::connect(addr).await.unwrap());
    }
    // Give the accept loop a beat to hand each socket its permit.
    tokio::time::sleep(std::time::Duration::from_millis(200)).await;

    // Pool exhausted: the next socket is shed at accept — it sees EOF quickly,
    // long before the 2s deadline could be the explanation.
    let mut shed = tokio::net::TcpStream::connect(addr).await.unwrap();
    let mut buf = [0u8; 1];
    let read = tokio::time::timeout(
        std::time::Duration::from_millis(1000),
        tokio::io::AsyncReadExt::read(&mut shed, &mut buf),
    )
    .await;
    match read {
        Ok(Ok(0)) => {} // clean EOF: shed
        other => panic!("expected the 5th socket to be shed with EOF, got {other:?}"),
    }

    // Past the deadline the stalled four are reaped and the pool recovers:
    // a genuine TLS+WS client must now complete its handshake.
    tokio::time::sleep(std::time::Duration::from_secs(3)).await;
    let mut ws = connect_ws(addr).await;
    ws.send(Message::text(
        Control::Connect {
            node_id: "no-such-node".into(),
        }
        .to_json(),
    ))
    .await
    .unwrap();
    // Any control reply proves the full handshake path is live again; this
    // node-id does not exist, so `no_such_node` is the expected answer.
    match next_control(&mut ws).await {
        Some(Control::Error { code, .. }) => assert_eq!(code, "no_such_node"),
        other => panic!("expected a control reply after recovery, got {other:?}"),
    }
    drop(stalled);
}

/// Post-Hello slowloris regression (August review). A `Hello` proves nothing:
/// any peer that can finish the TLS + WebSocket upgrade can send one. Releasing
/// the pre-classification permit at classification, and then awaiting `Register`
/// with no deadline, left an unauthenticated peer holding a task, a TLS session
/// and a parser OUTSIDE every handshake bound - a distinct resource state from
/// the pre-classification slowloris above.
///
/// The permit must be held for the whole signed registration exchange (so these
/// peers exhaust the pool and new sockets are shed) and the wait for `Register`
/// must share the setup deadline (so they are reaped and the pool recovers).
#[tokio::test]
async fn stalled_registrations_hold_their_permit_and_are_reaped() {
    let cfg = RelayConfig {
        max_pending_handshakes: 2,
        handshake_timeout: std::time::Duration::from_secs(3),
        // This test exercises the GLOBAL bound; every socket shares 127.0.0.1.
        accept_burst_per_ip: 1000,
        accept_rate_per_ip: 1000.0,
        ..RelayConfig::default()
    };
    let addr = start_relay(cfg).await;

    // Park both permits in the post-Hello registration state.
    let mut stalled = Vec::new();
    for i in 0..2 {
        let key = gen_key();
        let mut ws = hello_only(addr, &format!("stall-{i}"), &key).await;
        // Draining the Challenge proves the relay has advanced to awaiting
        // `Register` - the exact state under test.
        match next_control(&mut ws).await {
            Some(Control::Challenge { .. }) => {}
            other => panic!("expected a challenge, got {other:?}"),
        }
        stalled.push(ws);
    }

    // Pool exhausted by the two parked registrations: a new socket is shed at
    // accept. Before the fix the permits were already released at this point and
    // this socket was accepted normally.
    let mut shed = tokio::net::TcpStream::connect(addr).await.unwrap();
    let mut buf = [0u8; 1];
    let read = tokio::time::timeout(
        std::time::Duration::from_millis(1000),
        tokio::io::AsyncReadExt::read(&mut shed, &mut buf),
    )
    .await;
    match read {
        Ok(Ok(0)) => {} // clean EOF: shed
        other => {
            panic!("expected a socket to be shed while registrations are parked, got {other:?}")
        }
    }

    // Past the shared setup deadline the parked registrations are reaped and the
    // pool recovers for a well-behaved client.
    tokio::time::sleep(std::time::Duration::from_secs(4)).await;
    let mut ws = connect_ws(addr).await;
    ws.send(Message::text(
        Control::Connect {
            node_id: "no-such-node".into(),
        }
        .to_json(),
    ))
    .await
    .unwrap();
    match next_control(&mut ws).await {
        Some(Control::Error { code, .. }) => assert_eq!(code, "no_such_node"),
        other => panic!("expected a control reply after recovery, got {other:?}"),
    }
    drop(stalled);
}

/// `handshake_timeout` is documented as ONE deadline covering TLS accept, the
/// HTTP/WebSocket upgrade and the first control frame. It was applied as a fresh
/// relative timeout per phase, so a peer that spent most of one window on the
/// TLS/upgrade phase got a full second window for the first frame - roughly
/// twice the configured budget. Measured from accept, a peer that dawdles before
/// TLS and then never sends a control frame must still be reaped at ~one
/// `handshake_timeout`, not two.
#[tokio::test]
async fn handshake_timeout_is_one_budget_across_setup_phases() {
    let cfg = RelayConfig {
        handshake_timeout: std::time::Duration::from_secs(3),
        accept_burst_per_ip: 1000,
        accept_rate_per_ip: 1000.0,
        ..RelayConfig::default()
    };
    let addr = start_relay(cfg).await;

    let started = std::time::Instant::now();
    let tcp = tokio::net::TcpStream::connect(addr).await.unwrap();
    // Burn most of the budget BEFORE the TLS handshake, so the two phases are
    // distinguishable: the upgrade lands at ~2s of the 3s budget.
    tokio::time::sleep(std::time::Duration::from_secs(2)).await;
    let connector = tokio_rustls::TlsConnector::from(insecure_client_config());
    let sni = rustls::pki_types::ServerName::try_from("localhost").unwrap();
    let tls = connector.connect(sni, tcp).await.unwrap();
    let req = ClientRequestBuilder::new("wss://localhost/".parse().unwrap())
        .with_sub_protocol(SUBPROTOCOL);
    let (mut ws, _) = tokio_tungstenite::client_async_with_config(req, tls, None)
        .await
        .unwrap();

    // Never send a control frame; wait for the relay to reap the connection.
    let closed = tokio::time::timeout(std::time::Duration::from_secs(10), async {
        while ws.next().await.is_some() {}
    })
    .await;
    assert!(closed.is_ok(), "relay never reaped the stalled setup");
    let elapsed = started.elapsed();
    // One budget is ~3s from accept. Two independent windows put this at ~5s.
    assert!(
        elapsed < std::time::Duration::from_secs(4),
        "setup survived {elapsed:?}, past the single {:?} budget",
        std::time::Duration::from_secs(3)
    );
}

/// Post-admission registry bound (August human review). Admission bounds SETUP;
/// an admitted daemon then holds a registry entry, a writer task and a socket
/// for as long as it stays connected. In the supported open / shared-token
/// modes one permitted party can mint unlimited keys and node-ids, so the
/// registry needs its own aggregate ceiling - and the N+1 registration must be
/// refused with a reason on the wire, not silently dropped.
#[tokio::test]
async fn registry_is_bounded_and_rejects_the_next_node() {
    let cfg = RelayConfig {
        max_registered_nodes: 2,
        ..RelayConfig::default()
    };
    let addr = start_relay(cfg).await;

    // Two distinct daemons fill the registry and stay connected.
    let mut live = Vec::new();
    for i in 0..2 {
        let key = gen_key();
        let (ws, term) = handshake(addr, &format!("node-{i}"), &key, None, true).await;
        assert!(
            matches!(term, Control::Registered { .. }),
            "daemon {i} must register, got {term:?}"
        );
        live.push(ws);
    }

    // A third distinct node-id is refused with a wire-visible reason.
    let key = gen_key();
    let (_ws, term) = handshake(addr, "node-overflow", &key, None, true).await;
    match term {
        Control::Error { code, .. } => assert_eq!(code, "registry_full"),
        other => panic!("expected registry_full past the bound, got {other:?}"),
    }

    // Re-registering an ALREADY registered node-id replaces its entry rather
    // than growing the registry, so it is still admitted at the ceiling.
    let key0 = gen_key();
    let (_ws, term) = handshake(addr, "node-0", &key0, None, true).await;
    match term {
        // A different key for a live node-id is refused by the binding rule,
        // which proves the capacity check did not fire first.
        Control::Error { code, .. } => assert_eq!(code, "node_taken"),
        other => panic!("expected the node-id binding rule, got {other:?}"),
    }
    drop(live);
}

/// A node-id is the registry key, echoed into status output and logs. Without
/// its own limit the only ceiling is the 64 KiB control-frame bound, which is
/// far too loose. Oversized and non-printable ids are refused at registration.
#[tokio::test]
async fn oversized_and_nonprintable_node_ids_are_rejected() {
    let addr = start_relay(RelayConfig::default()).await;

    let oversized = "n".repeat(zerorelay::MAX_NODE_ID_LEN + 1);
    let key = gen_key();
    let (_ws, term) = handshake(addr, &oversized, &key, None, true).await;
    match term {
        Control::Error { code, .. } => assert_eq!(code, "bad_node_id"),
        other => panic!("expected bad_node_id for an oversized id, got {other:?}"),
    }

    // Exactly at the limit is still accepted: the bound is inclusive.
    let at_limit = "n".repeat(zerorelay::MAX_NODE_ID_LEN);
    let key = gen_key();
    let (_ws, term) = handshake(addr, &at_limit, &key, None, true).await;
    assert!(
        matches!(term, Control::Registered { .. }),
        "a node-id at exactly the limit must register, got {term:?}"
    );

    // Control characters must not reach the registry or operator surfaces.
    let key = gen_key();
    let (_ws, term) = handshake(addr, "node\u{7}with\u{0}control", &key, None, true).await;
    match term {
        Control::Error { code, .. } => assert_eq!(code, "bad_node_id"),
        other => panic!("expected bad_node_id for control characters, got {other:?}"),
    }
}
