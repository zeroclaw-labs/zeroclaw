//! Full real-component path on the DESIGNED relay protocol: a zerocode-style
//! client -> (outer TLS + WS + `conn_id` mux) relay -> real daemon bridge ->
//! daemon WSS mTLS listener, with the inner mutual TLS completing end to end.
//!
//! Drives the REAL [`zeroclaw_runtime::relay::run_relay_bridge`] (signed Ed25519
//! registration, keepalive, loopback bridging) against the REAL
//! [`zerorelay::RelayServer`] (outer TLS, signed admission, multiplexer) and a
//! WSS (TLS + WebSocket) mTLS listener built from the runtime's own
//! `build_tls_acceptor` - the daemon's actual remote-plane stack. The relay only
//! ever forwards opaque DATA frames; it terminates only the outer TLS.
#![allow(clippy::disallowed_methods)]

use std::sync::Arc;
use std::time::Duration;

use futures_util::{SinkExt, StreamExt};
use tokio::io::{AsyncReadExt, AsyncWriteExt};
use tokio::net::TcpListener;
use tokio_rustls::TlsAcceptor;
use tokio_tungstenite::tungstenite::{ClientRequestBuilder, Message};
use tokio_util::sync::CancellationToken;
use zeroclaw_relay_proto::{Control, SUBPROTOCOL, decode_data, encode_data};

/// Accept any server cert: the test pins nothing, it asserts the handshake and
/// byte round-trip, not the PKI (covered by the dedicated mTLS tests).
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

fn write_temp(content: &str) -> tempfile::NamedTempFile {
    use std::io::Write;
    let mut f = tempfile::NamedTempFile::new().unwrap();
    f.write_all(content.as_bytes()).unwrap();
    f.flush().unwrap();
    f
}

/// Build a self-signed outer TLS acceptor for the relay (its own identity).
fn relay_outer_acceptor() -> TlsAcceptor {
    let ck =
        rcgen::generate_simple_self_signed(vec!["localhost".into(), "127.0.0.1".into()]).unwrap();
    let cert = rustls::pki_types::CertificateDer::from(ck.cert.der().to_vec());
    let key = rustls::pki_types::PrivateKeyDer::Pkcs8(rustls::pki_types::PrivatePkcs8KeyDer::from(
        ck.key_pair.serialize_der(),
    ));
    let cfg = rustls::ServerConfig::builder_with_provider(Arc::new(
        rustls::crypto::ring::default_provider(),
    ))
    .with_safe_default_protocol_versions()
    .unwrap()
    .with_no_client_auth()
    .with_single_cert(vec![cert], key)
    .unwrap();
    TlsAcceptor::from(Arc::new(cfg))
}

#[tokio::test]
async fn zerocode_to_relay_to_daemon_full_path() {
    let _ = rustls::crypto::ring::default_provider().install_default();

    // Daemon mTLS materials + an issued client cert (the inner session).
    let dir = tempfile::tempdir().unwrap();
    let mats = zeroclaw_tls::ensure_server_materials(dir.path(), &[]).unwrap();
    let acceptor: TlsAcceptor = zeroclaw_runtime::rpc::wss::build_tls_acceptor(
        mats.server_cert_path.to_str().unwrap(),
        mats.server_key_path.to_str().unwrap(),
        mats.ca_cert_path.to_str().unwrap(),
        &[],
    )
    .unwrap();

    let ca_pem = std::fs::read_to_string(&mats.ca_cert_path).unwrap();
    let ca_key_pem = std::fs::read_to_string(&mats.ca_key_path).unwrap();
    let issued = zeroclaw_tls::issue_client_cert(&ca_pem, &ca_key_pem, "relay-device").unwrap();
    let cert_f = write_temp(&issued.cert_pem);
    let key_f = write_temp(&issued.key_pem);
    let client_chain = zeroclaw_tls::load_certs(cert_f.path().to_str().unwrap()).unwrap();
    let client_key = zeroclaw_tls::load_private_key(key_f.path().to_str().unwrap()).unwrap();

    // Daemon WSS listener (TLS + WebSocket echo): the real remote-plane stack the
    // bridge forwards to over loopback.
    let wss = TcpListener::bind("127.0.0.1:0").await.unwrap();
    let wss_addr = wss.local_addr().unwrap();
    tokio::spawn(async move {
        loop {
            let (tcp, _) = wss.accept().await.unwrap();
            let acceptor = acceptor.clone();
            tokio::spawn(async move {
                let Ok(tls) = acceptor.accept(tcp).await else {
                    return;
                };
                let Ok(mut ws) = tokio_tungstenite::accept_async(tls).await else {
                    return;
                };
                if let Some(Ok(msg)) = ws.next().await {
                    let _ = ws.send(msg).await; // echo
                    let _ = ws.flush().await;
                }
            });
        }
    });

    // Relay with its own outer TLS identity (open admission).
    let relay_listener = TcpListener::bind("127.0.0.1:0").await.unwrap();
    let relay_addr = relay_listener.local_addr().unwrap();
    let relay_acceptor = relay_outer_acceptor();
    tokio::spawn(
        zerorelay::RelayServer::new(zerorelay::RelayConfig::default())
            .serve(relay_listener, relay_acceptor),
    );

    // The REAL daemon-side bridge: signed Ed25519 registration over outer TLS.
    let signing_key = zeroclaw_runtime::relay::ensure_signing_key(dir.path()).unwrap();
    let cancel = CancellationToken::new();
    tokio::spawn(zeroclaw_runtime::relay::run_relay_bridge(
        zeroclaw_runtime::relay::RelayBridgeConfig {
            relay_addr: relay_addr.to_string(),
            relay_host: "localhost".into(),
            node_id: "relay-device".into(),
            relay_token: None,
            local_wss_addr: format!("127.0.0.1:{}", wss_addr.port()),
            signing_key_pkcs8: signing_key,
            relay_ca_path: None,
            relay_insecure: true, // self-signed relay outer cert in the test
            max_conns: 16,
        },
        cancel.clone(),
    ));

    // Client: outer TLS + WS to the relay, request the node-id, then run the inner
    // WSS + mTLS over a duplex bridge to the relay's DATA frames. Retry the whole
    // dial until the asynchronously-spawned bridge has registered the node-id.
    let client_io = dial_relay_with_retry(relay_addr, "relay-device").await;

    let inner_cfg = rustls::ClientConfig::builder_with_provider(Arc::new(
        rustls::crypto::ring::default_provider(),
    ))
    .with_safe_default_protocol_versions()
    .unwrap()
    .dangerous()
    .with_custom_certificate_verifier(Arc::new(NoServerVerify))
    .with_client_auth_cert(client_chain, client_key)
    .unwrap();
    let connector = tokio_tungstenite::Connector::Rustls(Arc::new(inner_cfg));

    let (mut ws, _resp) = tokio_tungstenite::client_async_tls_with_config(
        "wss://relay-device/",
        client_io,
        None,
        Some(connector),
    )
    .await
    .expect("inner WSS + mTLS must complete through the relay");

    ws.send(Message::Text("ping".into())).await.unwrap();
    let echoed = ws.next().await.expect("echo").expect("ws message");
    assert_eq!(
        echoed.into_text().unwrap(),
        "ping",
        "echo did not round-trip via the relay"
    );

    cancel.cancel();
}

/// Mirror of zerocode's `dial_through_relay`: outer TLS + WS to the relay, send
/// `Connect`, await `Opened`, then bridge a duplex byte stream to/from the DATA
/// frames. Retries the full dial until the bridge has registered the node-id.
async fn dial_relay_with_retry(
    relay_addr: std::net::SocketAddr,
    node_id: &str,
) -> tokio::io::DuplexStream {
    for _ in 0..100 {
        if let Some(io) = try_dial_relay(relay_addr, node_id).await {
            return io;
        }
        tokio::time::sleep(Duration::from_millis(50)).await;
    }
    panic!("bridge did not register the node-id in time");
}

async fn try_dial_relay(
    relay_addr: std::net::SocketAddr,
    node_id: &str,
) -> Option<tokio::io::DuplexStream> {
    let outer = rustls::ClientConfig::builder_with_provider(Arc::new(
        rustls::crypto::ring::default_provider(),
    ))
    .with_safe_default_protocol_versions()
    .unwrap()
    .dangerous()
    .with_custom_certificate_verifier(Arc::new(NoServerVerify))
    .with_no_client_auth();
    let tcp = tokio::net::TcpStream::connect(relay_addr).await.ok()?;
    let req = ClientRequestBuilder::new("wss://localhost/".parse().unwrap())
        .with_sub_protocol(SUBPROTOCOL);
    let (relay_ws, _) = tokio_tungstenite::client_async_tls_with_config(
        req,
        tcp,
        None,
        Some(tokio_tungstenite::Connector::Rustls(Arc::new(outer))),
    )
    .await
    .ok()?;
    let (mut sink, mut stream) = relay_ws.split();
    sink.send(Message::text(
        Control::Connect {
            node_id: node_id.to_string(),
        }
        .to_json(),
    ))
    .await
    .ok()?;

    let conn_id = loop {
        match stream.next().await? {
            Ok(Message::Text(t)) => match Control::from_json(t.as_str()) {
                Ok(Control::Opened { conn_id }) => break conn_id,
                Ok(Control::Error { .. }) => return None, // not registered yet; retry
                _ => {}
            },
            Ok(Message::Ping(p)) => {
                let _ = sink.send(Message::Pong(p)).await;
            }
            Ok(_) => {}
            Err(_) => return None,
        }
    };

    let (client_io, mut relay_io) = tokio::io::duplex(128 * 1024);
    tokio::spawn(async move {
        let mut buf = vec![0u8; 64 * 1024];
        loop {
            tokio::select! {
                n = relay_io.read(&mut buf) => match n {
                    Ok(0) | Err(_) => break,
                    Ok(n) => {
                        if sink.send(Message::binary(encode_data(conn_id, &buf[..n]))).await.is_err() {
                            break;
                        }
                    }
                },
                msg = stream.next() => match msg {
                    Some(Ok(Message::Binary(b))) => {
                        if let Some((_, payload)) = decode_data(&b)
                            && relay_io.write_all(payload).await.is_err() {
                                break;
                            }
                    }
                    Some(Ok(Message::Ping(p))) => {
                        let _ = sink.send(Message::Pong(p)).await;
                    }
                    Some(Ok(_)) => {}
                    Some(Err(_)) | None => break,
                }
            }
        }
        let _ = relay_io.shutdown().await;
    });
    Some(client_io)
}
