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
use zeroclaw_relay_proto::{Control, SUBPROTOCOL};
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
