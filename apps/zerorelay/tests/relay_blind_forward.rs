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
use zeroclaw_relay_proto::{Control, SUBPROTOCOL, decode_data, encode_data};
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
    tokio::spawn(RelayServer::new(cfg).serve(listener, acceptor));
    addr
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
