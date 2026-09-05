//! End-to-end browser enrollment through the frontdoor.
//!
//! No browser is involved and none is needed: the page's enrollment exchange is
//! two JSON POSTs to the relay, so this drives exactly the bytes the page sends
//! and asserts on exactly the bytes it would receive. What sits behind those
//! POSTs is real - a real `RelayServer` with the frontdoor enabled, a real
//! daemon bridge splicing DATA frames into a real `zeroclaw_runtime::enroll`
//! endpoint, which consumes a real pairing code and issues a real certificate
//! from a real CA.
//!
//! The point of using the real endpoint rather than a stub is that a stub can
//! agree with a wrong implementation. The CSR must satisfy `zeroclaw_tls`'s
//! issuer, the pinned TLS leg must satisfy rustls against the daemon's actual
//! certificate, and the pairing code must satisfy the daemon's actual guard.
#![allow(clippy::disallowed_methods)]

use std::sync::Arc;

use base64::Engine as _;
use base64::engine::general_purpose::STANDARD as B64;
use futures_util::{SinkExt, StreamExt};
use ring::signature::{Ed25519KeyPair, KeyPair};
use tokio::io::{AsyncReadExt, AsyncWriteExt};
use tokio::net::TcpListener;
use tokio_rustls::TlsAcceptor;
use tokio_tungstenite::tungstenite::{ClientRequestBuilder, Message};
use zeroclaw_relay_proto::{
    Control, INITIAL_WINDOW, PEER_HINT_ENROLL, SUBPROTOCOL, decode_data, encode_data,
};
use zerorelay::{RelayConfig, RelayServer};

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

fn server_config_from_pem(cert_pem: &str, key_pem: &str) -> rustls::ServerConfig {
    let certs = rustls_pemfile::certs(&mut cert_pem.as_bytes())
        .collect::<Result<Vec<_>, _>>()
        .unwrap();
    let key = rustls_pemfile::private_key(&mut key_pem.as_bytes())
        .unwrap()
        .expect("a private key");
    rustls::ServerConfig::builder_with_provider(Arc::new(rustls::crypto::ring::default_provider()))
        .with_safe_default_protocol_versions()
        .unwrap()
        .with_no_client_auth()
        .with_single_cert(certs, key)
        .unwrap()
}

/// A live daemon enrollment endpoint and everything a test needs to talk to it.
struct Daemon {
    addr: std::net::SocketAddr,
    ca_cert_pem: String,
    pairing_code: String,
    ledger: Arc<zeroclaw_runtime::security::cert_ledger::CertLedger>,
    _cancel: tokio_util::sync::CancellationToken,
}

/// Start the REAL daemon enrollment endpoint on loopback.
async fn start_daemon_enrollment() -> Daemon {
    let _ = rustls::crypto::ring::default_provider().install_default();
    let (ca_cert_pem, ca_key_pem) = zeroclaw_tls::testing::gen_ca();
    // The enrollment leaf must carry the name the relay-routed client verifies
    // against on the pinned leg (`127.0.0.1`).
    let (leaf_pem, leaf_key_pem) =
        zeroclaw_tls::testing::gen_server_cert(&ca_cert_pem, &ca_key_pem, &["127.0.0.1".into()]);
    let acceptor = TlsAcceptor::from(Arc::new(server_config_from_pem(&leaf_pem, &leaf_key_pem)));

    let ledger = Arc::new(
        zeroclaw_runtime::security::cert_ledger::CertLedger::open_in_memory(None).unwrap(),
    );
    let pairing = Arc::new(zeroclaw_config::pairing::PairingGuard::new(true, &[]));
    let pairing_code = pairing.pairing_code().expect("a pairing code");

    let listener = TcpListener::bind("127.0.0.1:0").await.unwrap();
    let addr = listener.local_addr().unwrap();
    let server = Arc::new(zeroclaw_runtime::enroll::EnrollServer {
        bind_addr: addr,
        acceptor,
        ca_cert_pem: ca_cert_pem.clone(),
        ca_key_pem: zeroize::Zeroizing::new(ca_key_pem),
        ledger: ledger.clone(),
        pairing,
        static_client_pins_configured: false,
        allow_unpaired_until: None,
        relay_profile: zeroclaw_runtime::enroll::RelayProfile {
            relay_url: "relay.test:9000".into(),
            node_id: "test-node".into(),
            relay_cert_pin: String::new(),
        },
        bridge_ports: None,
        relay_attempt_bucket: zeroclaw_runtime::enroll::RelayAttemptBucket::default(),
        paircode_admin_data_dir: None,
    });
    let cancel = tokio_util::sync::CancellationToken::new();
    tokio::spawn(zeroclaw_runtime::enroll::serve_on(
        listener,
        server,
        cancel.clone(),
    ));

    Daemon {
        addr,
        ca_cert_pem,
        pairing_code,
        ledger,
        _cancel: cancel,
    }
}

/// Start a relay with the browser frontdoor enabled.
async fn start_relay_with_frontdoor() -> std::net::SocketAddr {
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
    std::mem::forget(dir);
    let listener = TcpListener::bind("127.0.0.1:0").await.unwrap();
    let addr = listener.local_addr().unwrap();
    let server = RelayServer::new(RelayConfig {
        frontdoor_enabled: true,
        ..Default::default()
    });
    tokio::spawn(server.serve(listener, acceptor));
    addr
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

/// Register a daemon on the relay and splice every enrollment route it is asked
/// to open into `target`, exactly as the in-daemon relay bridge does.
///
/// `target` is a plain TCP address and the bytes are forwarded untouched: this
/// is the property that makes browser TLS-in-JS necessary in the first place,
/// and the reason the relay (not the page) terminates TLS in this design.
async fn register_bridge_daemon(
    relay_addr: std::net::SocketAddr,
    node_id: &str,
    target: std::net::SocketAddr,
) {
    let rng = ring::rand::SystemRandom::new();
    let pkcs8 = Ed25519KeyPair::generate_pkcs8(&rng)
        .unwrap()
        .as_ref()
        .to_vec();
    let kp = Ed25519KeyPair::from_pkcs8(&pkcs8).unwrap();

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
            daemon_pubkey: B64.encode(kp.public_key().as_ref()),
            node_id: node_id.to_string(),
            relay_token: None,
        }
        .to_json(),
    ))
    .await
    .unwrap();
    let nonce = match next_control(&mut ws).await {
        Some(Control::Challenge { nonce }) => B64.decode(nonce.as_bytes()).unwrap(),
        other => panic!("expected a challenge, got {other:?}"),
    };
    ws.send(Message::text(
        Control::Register {
            node_id: node_id.to_string(),
            sig: B64.encode(kp.sign(&nonce).as_ref()),
        }
        .to_json(),
    ))
    .await
    .unwrap();
    match next_control(&mut ws).await {
        Some(Control::Registered { .. }) => {}
        other => panic!("registration failed: {other:?}"),
    }

    tokio::spawn(async move {
        let (mut sink, mut stream) = ws.split();
        let mut sockets: std::collections::HashMap<u64, tokio::sync::mpsc::Sender<Vec<u8>>> =
            std::collections::HashMap::new();
        let (inbound_tx, mut inbound_rx) = tokio::sync::mpsc::channel::<Message>(256);
        loop {
            tokio::select! {
                outgoing = inbound_rx.recv() => match outgoing {
                    Some(msg) => {
                        if sink.send(msg).await.is_err() { break; }
                    }
                    None => break,
                },
                incoming = stream.next() => match incoming {
                    Some(Ok(Message::Text(t))) => {
                        match Control::from_json(t.as_str()) {
                            Ok(Control::Open { conn_id, peer_hint }) => {
                                assert_eq!(
                                    peer_hint.as_deref(),
                                    Some(PEER_HINT_ENROLL),
                                    "the frontdoor must route by the enrollment peer hint"
                                );
                                let Ok(sock) = tokio::net::TcpStream::connect(target).await else {
                                    continue;
                                };
                                let (tx, rx) = tokio::sync::mpsc::channel::<Vec<u8>>(64);
                                sockets.insert(conn_id, tx);
                                let out = inbound_tx.clone();
                                let _ = out.send(Message::text(
                                    Control::Opened { conn_id }.to_json(),
                                )).await;
                                let _ = out.send(Message::text(
                                    Control::Window { conn_id, credit: INITIAL_WINDOW }.to_json(),
                                )).await;
                                tokio::spawn(splice(conn_id, sock, rx, out));
                            }
                            Ok(Control::Close { conn_id, .. }) => { sockets.remove(&conn_id); }
                            _ => {}
                        }
                    }
                    Some(Ok(Message::Binary(b))) => {
                        if let Some((conn_id, payload)) = decode_data(&b)
                            && let Some(tx) = sockets.get(&conn_id)
                        {
                            let _ = tx.send(payload.to_vec()).await;
                        }
                    }
                    Some(Ok(Message::Ping(p))) => {
                        let _ = inbound_tx.send(Message::Pong(p)).await;
                    }
                    Some(Ok(_)) => {}
                    Some(Err(_)) | None => break,
                }
            }
        }
    });
}

/// Pipe one logical connection between the relay link and the loopback target.
async fn splice(
    conn_id: u64,
    sock: tokio::net::TcpStream,
    mut from_relay: tokio::sync::mpsc::Receiver<Vec<u8>>,
    to_relay: tokio::sync::mpsc::Sender<Message>,
) {
    let (mut rd, mut wr) = sock.into_split();
    let writer = tokio::spawn(async move {
        while let Some(chunk) = from_relay.recv().await {
            if wr.write_all(&chunk).await.is_err() {
                break;
            }
        }
        let _ = wr.shutdown().await;
    });
    let mut buf = vec![0u8; 8192];
    let mut consumed: u32 = 0;
    loop {
        match rd.read(&mut buf).await {
            Ok(0) | Err(_) => break,
            Ok(n) => {
                if to_relay
                    .send(Message::binary(encode_data(conn_id, &buf[..n])))
                    .await
                    .is_err()
                {
                    break;
                }
                // Replenish the relay-side sender's window the way the real
                // bridge does, so a response larger than one window still flows.
                consumed = consumed.saturating_add(n as u32);
                if consumed >= INITIAL_WINDOW / 2 {
                    let _ = to_relay
                        .send(Message::text(
                            Control::DataAck { conn_id, consumed }.to_json(),
                        ))
                        .await;
                    consumed = 0;
                }
            }
        }
    }
    let _ = to_relay
        .send(Message::text(
            Control::Close {
                conn_id,
                reason: "eof".into(),
            }
            .to_json(),
        ))
        .await;
    writer.abort();
}

/// Issue one frontdoor HTTP request over the relay's outer TLS - the same thing
/// the page's `fetch()` does.
async fn frontdoor_request(
    relay_addr: std::net::SocketAddr,
    method: &str,
    path: &str,
    body: Option<&str>,
) -> (u16, String) {
    let tcp = tokio::net::TcpStream::connect(relay_addr).await.unwrap();
    let connector = tokio_rustls::TlsConnector::from(insecure_client_config());
    let sni = rustls::pki_types::ServerName::try_from("localhost").unwrap();
    let mut tls = connector.connect(sni, tcp).await.unwrap();

    let request = match body {
        Some(body) => format!(
            "{method} {path} HTTP/1.1\r\nHost: localhost\r\nContent-Type: application/json\r\nContent-Length: {}\r\nConnection: close\r\n\r\n{body}",
            body.len()
        ),
        None => format!("{method} {path} HTTP/1.1\r\nHost: localhost\r\nConnection: close\r\n\r\n"),
    };
    tls.write_all(request.as_bytes()).await.unwrap();
    tls.flush().await.unwrap();

    let mut raw = Vec::new();
    let mut chunk = [0u8; 8192];
    loop {
        match tls.read(&mut chunk).await {
            Ok(0) | Err(_) => break,
            Ok(n) => raw.extend_from_slice(&chunk[..n]),
        }
        if raw.len() > 4 * 1024 * 1024 {
            break;
        }
    }
    let text = String::from_utf8_lossy(&raw).to_string();
    let status = text
        .lines()
        .next()
        .and_then(|l| l.split_whitespace().nth(1))
        .and_then(|c| c.parse::<u16>().ok())
        .unwrap_or(0);
    let body = text
        .split_once("\r\n\r\n")
        .map(|(_, b)| b)
        .unwrap_or("")
        .to_string();
    (status, body)
}

/// The headline path: the page's two POSTs produce a real certificate, issued by
/// the real daemon from a real pairing code, delivered back through the relay.
#[tokio::test]
async fn browser_enrollment_issues_a_real_certificate_through_the_relay() {
    let daemon = start_daemon_enrollment().await;
    let relay = start_relay_with_frontdoor().await;
    register_bridge_daemon(relay, "browser-node", daemon.addr).await;

    // Step 1: the page fetches the CA so it can show the operator a SAS.
    let (status, body) = frontdoor_request(
        relay,
        "POST",
        "/enroll/ca",
        Some(&serde_json::json!({ "node_id": "browser-node" }).to_string()),
    )
    .await;
    assert_eq!(status, 200, "trust preflight failed: {body}");
    let trust: serde_json::Value = serde_json::from_str(&body).unwrap();
    let ca_chain_pem = trust["ca_chain_pem"].as_str().unwrap().to_string();
    assert_eq!(
        ca_chain_pem, daemon.ca_cert_pem,
        "the page must be shown the daemon's own CA"
    );

    // The SAS the page renders must equal the one the daemon console prints.
    let ca_der = rustls_pemfile::certs(&mut ca_chain_pem.as_bytes())
        .next()
        .unwrap()
        .unwrap();
    let expected_sas = zeroclaw_tls::enrollment_sas(
        &daemon.pairing_code,
        &zeroclaw_tls::cert_sha256_fingerprint(ca_der.as_ref()),
    );
    assert!(
        expected_sas.len() == 9 && expected_sas.contains('-'),
        "sanity: SAS shape is XXXX-XXXX, got {expected_sas}"
    );

    // Step 2: the operator confirmed the SAS; the browser sends its CSR. The key
    // is generated client-side and never leaves - only the CSR is transmitted.
    let (csr_pem, _key_pem) = zeroclaw_tls::testing::gen_client_csr("zeroclaw-browser");
    let (status, body) = frontdoor_request(
        relay,
        "POST",
        "/enroll",
        Some(
            &serde_json::json!({
                "node_id": "browser-node",
                "pairing_code": daemon.pairing_code,
                "csr_pem": csr_pem,
                "ca_chain_pem": ca_chain_pem,
            })
            .to_string(),
        ),
    )
    .await;
    assert_eq!(status, 200, "enrollment failed: {body}");
    let issued: serde_json::Value = serde_json::from_str(&body).unwrap();

    let cert_pem = issued["cert_pem"].as_str().expect("a certificate");
    assert!(cert_pem.contains("BEGIN CERTIFICATE"), "got: {cert_pem}");
    let device_id = issued["device_id"].as_str().expect("a device id");
    assert!(!device_id.is_empty());
    assert!(issued["not_after"].as_i64().unwrap() > 0);
    assert_eq!(
        issued["ca_chain_pem"].as_str().unwrap(),
        daemon.ca_cert_pem,
        "the response CA must be the confirmed one"
    );
    // The relay profile reaches the page verbatim, so it can tell the operator
    // where the native client should dial.
    assert_eq!(issued["relay_profile"]["relay_url"], "relay.test:9000");

    // The certificate really was issued and recorded by the DAEMON, so the proxy
    // path completed the real exchange rather than returning plausible JSON.
    //
    // The ledger's `delivered_at` marking is not exposed on `CertLedger`'s public
    // API, so it cannot be asserted directly from here. What is observable is
    // stronger than nothing and is the daemon's own record: the fingerprint the
    // page received is present and Active in the daemon's ledger under the same
    // device id the response carried.
    let cert_der = rustls_pemfile::certs(&mut cert_pem.as_bytes())
        .next()
        .unwrap()
        .unwrap();
    let fingerprint = zeroclaw_tls::cert_sha256_fingerprint(cert_der.as_ref());
    let entry = daemon
        .ledger
        .lookup_by_fingerprint(&fingerprint)
        .unwrap()
        .expect("the issued certificate must be in the daemon ledger");
    assert_eq!(entry.device_id, device_id);
    assert_eq!(
        daemon.ledger.status_of(&fingerprint).unwrap(),
        Some(zeroclaw_runtime::security::cert_ledger::CertStatus::Active),
        "a delivered enrollment must leave an active ledger row"
    );
}

/// A wrong pairing code must be refused by the DAEMON, and the daemon's status
/// must reach the page rather than being flattened into a generic relay error.
#[tokio::test]
async fn a_bad_pairing_code_is_refused_with_the_daemon_status() {
    let daemon = start_daemon_enrollment().await;
    let relay = start_relay_with_frontdoor().await;
    register_bridge_daemon(relay, "browser-node", daemon.addr).await;

    let (_, body) = frontdoor_request(
        relay,
        "POST",
        "/enroll/ca",
        Some(&serde_json::json!({ "node_id": "browser-node" }).to_string()),
    )
    .await;
    let trust: serde_json::Value = serde_json::from_str(&body).unwrap();
    let ca_chain_pem = trust["ca_chain_pem"].as_str().unwrap().to_string();

    let (csr_pem, _key) = zeroclaw_tls::testing::gen_client_csr("zeroclaw-browser");
    let (status, body) = frontdoor_request(
        relay,
        "POST",
        "/enroll",
        Some(
            &serde_json::json!({
                "node_id": "browser-node",
                "pairing_code": "000000",
                "csr_pem": csr_pem,
                "ca_chain_pem": ca_chain_pem,
            })
            .to_string(),
        ),
    )
    .await;
    assert_eq!(
        status, 401,
        "expected the daemon's 401, got {status}: {body}"
    );
}

/// The enroll POST is pinned to the CA the OPERATOR confirmed, not to whatever
/// the daemon offers on the second connection.
///
/// Here the page sends a CA the operator never saw - the shape a substituted or
/// stale confirmation takes. The pinned handshake must fail, so no pairing code
/// is ever written to a peer outside the confirmed trust anchor.
#[tokio::test]
async fn the_enroll_post_refuses_a_ca_the_daemon_cannot_prove() {
    let daemon = start_daemon_enrollment().await;
    let relay = start_relay_with_frontdoor().await;
    register_bridge_daemon(relay, "browser-node", daemon.addr).await;

    // A perfectly valid CA - just not this daemon's.
    let (other_ca_pem, _other_key) = zeroclaw_tls::testing::gen_ca();
    assert_ne!(other_ca_pem, daemon.ca_cert_pem);

    let (csr_pem, _key) = zeroclaw_tls::testing::gen_client_csr("zeroclaw-browser");
    let (status, body) = frontdoor_request(
        relay,
        "POST",
        "/enroll",
        Some(
            &serde_json::json!({
                "node_id": "browser-node",
                "pairing_code": daemon.pairing_code,
                "csr_pem": csr_pem,
                "ca_chain_pem": other_ca_pem,
            })
            .to_string(),
        ),
    )
    .await;
    assert_eq!(
        status, 502,
        "a POST pinned to an unprovable CA must fail the handshake, got {status}: {body}"
    );

    // And the pairing code must still be unspent: the refusal happened before
    // anything reached the daemon's guard.
    let (status, body) = frontdoor_request(
        relay,
        "POST",
        "/enroll/ca",
        Some(&serde_json::json!({ "node_id": "browser-node" }).to_string()),
    )
    .await;
    assert_eq!(status, 200);
    let trust: serde_json::Value = serde_json::from_str(&body).unwrap();
    let (csr_pem, _key) = zeroclaw_tls::testing::gen_client_csr("zeroclaw-browser");
    let (status, body) = frontdoor_request(
        relay,
        "POST",
        "/enroll",
        Some(
            &serde_json::json!({
                "node_id": "browser-node",
                "pairing_code": daemon.pairing_code,
                "csr_pem": csr_pem,
                "ca_chain_pem": trust["ca_chain_pem"],
            })
            .to_string(),
        ),
    )
    .await;
    assert_eq!(
        status, 200,
        "the pairing code must survive a refused pin: {body}"
    );
}

/// A daemon that authenticates correctly and THEN hands back a different CA for
/// the client to trust must be refused.
///
/// The pinned handshake only proves the peer holds a key the confirmed CA
/// issued. It says nothing about the `ca_chain_pem` inside the response body,
/// which is the anchor the client would go on to pin for the RPC plane. This
/// stub is signed by the confirmed CA - so the pin succeeds - and then returns a
/// different CA, which is the only shape that reaches the response check.
#[tokio::test]
async fn a_daemon_that_swaps_the_ca_in_its_response_is_refused() {
    let _ = rustls::crypto::ring::default_provider().install_default();
    let (ca_cert_pem, ca_key_pem) = zeroclaw_tls::testing::gen_ca();
    let (other_ca_pem, _other_key) = zeroclaw_tls::testing::gen_ca();
    let (leaf_pem, leaf_key_pem) =
        zeroclaw_tls::testing::gen_server_cert(&ca_cert_pem, &ca_key_pem, &["127.0.0.1".into()]);
    let acceptor = TlsAcceptor::from(Arc::new(server_config_from_pem(&leaf_pem, &leaf_key_pem)));

    // A stub endpoint: correct identity, dishonest body.
    let listener = TcpListener::bind("127.0.0.1:0").await.unwrap();
    let stub_addr = listener.local_addr().unwrap();
    let honest_ca = ca_cert_pem.clone();
    let swapped_ca = other_ca_pem.clone();
    tokio::spawn(async move {
        loop {
            let Ok((tcp, _)) = listener.accept().await else {
                return;
            };
            let acceptor = acceptor.clone();
            let honest_ca = honest_ca.clone();
            let swapped_ca = swapped_ca.clone();
            tokio::spawn(async move {
                let Ok(mut tls) = acceptor.accept(tcp).await else {
                    return;
                };
                let mut head = Vec::new();
                let mut chunk = [0u8; 1024];
                while !head.windows(4).any(|w| w == b"\r\n\r\n") {
                    match tls.read(&mut chunk).await {
                        Ok(0) | Err(_) => return,
                        Ok(n) => head.extend_from_slice(&chunk[..n]),
                    }
                }
                let text = String::from_utf8_lossy(&head).to_string();
                // The preflight is honest, so the operator confirms the real CA.
                let body = if text.starts_with("GET /enroll/ca") {
                    serde_json::json!({ "ca_chain_pem": honest_ca }).to_string()
                } else {
                    // ... and the enrollment response then swaps it.
                    serde_json::json!({
                        "cert_pem": "-----BEGIN CERTIFICATE-----\nZm9v\n-----END CERTIFICATE-----\n",
                        "ca_chain_pem": swapped_ca,
                        "device_id": "swapped",
                        "not_after": 4102444800i64,
                        "relay_profile": { "relay_url": "", "node_id": "", "relay_cert_pin": "" },
                    })
                    .to_string()
                };
                let response = format!(
                    "HTTP/1.1 200 OK\r\ncontent-type: application/json\r\ncontent-length: {}\r\nconnection: close\r\n\r\n{body}",
                    body.len()
                );
                let _ = tls.write_all(response.as_bytes()).await;
                let _ = tls.flush().await;
                // Close cleanly (close_notify) so the reader sees EOF rather
                // than a reset it would report as a transport failure.
                let _ = tls.shutdown().await;
            });
        }
    });

    let relay = start_relay_with_frontdoor().await;
    register_bridge_daemon(relay, "swapper", stub_addr).await;

    // The preflight is honest: this is the CA the operator confirms by SAS.
    let (status, body) = frontdoor_request(
        relay,
        "POST",
        "/enroll/ca",
        Some(&serde_json::json!({ "node_id": "swapper" }).to_string()),
    )
    .await;
    assert_eq!(status, 200, "{body}");
    let trust: serde_json::Value = serde_json::from_str(&body).unwrap();
    assert_eq!(trust["ca_chain_pem"].as_str().unwrap(), ca_cert_pem);

    let (csr_pem, _key) = zeroclaw_tls::testing::gen_client_csr("zeroclaw-browser");
    let (status, body) = frontdoor_request(
        relay,
        "POST",
        "/enroll",
        Some(
            &serde_json::json!({
                "node_id": "swapper",
                "pairing_code": "whatever",
                "csr_pem": csr_pem,
                "ca_chain_pem": ca_cert_pem,
            })
            .to_string(),
        ),
    )
    .await;
    assert_eq!(status, 502, "a swapped response CA must be refused: {body}");
    assert!(
        body.contains("different CA than the one confirmed"),
        "the refusal must name the cause: {body}"
    );
}

/// Enrolling against an unknown node must not open anything.
#[tokio::test]
async fn an_unknown_node_is_refused_before_any_route_is_opened() {
    let relay = start_relay_with_frontdoor().await;
    let (status, body) = frontdoor_request(
        relay,
        "POST",
        "/enroll/ca",
        Some(&serde_json::json!({ "node_id": "nobody" }).to_string()),
    )
    .await;
    assert_eq!(status, 404, "got {status}: {body}");
    assert!(body.contains("no_such_node"), "got: {body}");
}

/// The page and its script are served, and the routes the deleted TLS-in-JS
/// frontdoor exposed are gone.
#[tokio::test]
async fn the_frontdoor_serves_the_page_and_not_the_deleted_tls_routes() {
    let relay = start_relay_with_frontdoor().await;

    let (status, body) = frontdoor_request(relay, "GET", "/", None).await;
    assert_eq!(status, 200);
    assert!(body.contains("ZeroClaw browser enrollment"), "got: {body}");
    assert!(body.contains("/app.js"));

    let (status, body) = frontdoor_request(relay, "GET", "/app.js", None).await;
    assert_eq!(status, 200);
    assert!(body.contains("createEnrollmentMaterial"));

    // Every route the TLS-in-JS frontdoor served is gone, not merely emptied.
    for path in ["/tls-engine.js", "/tunnel-worker.js", "/sw.js", "/webui/"] {
        let (status, _) = frontdoor_request(relay, "GET", path, None).await;
        assert_eq!(status, 404, "{path} must not be served any more");
    }
}

/// With the frontdoor off nothing is served, and the relay plane is unchanged.
#[tokio::test]
async fn a_disabled_frontdoor_serves_nothing() {
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
    std::mem::forget(dir);
    let listener = TcpListener::bind("127.0.0.1:0").await.unwrap();
    let addr = listener.local_addr().unwrap();
    tokio::spawn(
        RelayServer::new(RelayConfig::default())
            .serve(listener, TlsAcceptor::from(Arc::new(server_cfg))),
    );

    let (status, body) = frontdoor_request(addr, "GET", "/", None).await;
    assert_eq!(status, 404);
    assert!(body.contains("zeroclaw.relay.v1"), "got: {body}");
    assert!(!body.contains("ZeroClaw browser enrollment"));

    let (status, _) = frontdoor_request(
        addr,
        "POST",
        "/enroll",
        Some(&serde_json::json!({ "node_id": "x" }).to_string()),
    )
    .await;
    assert_eq!(status, 404, "the enrollment routes must not exist when off");
}
