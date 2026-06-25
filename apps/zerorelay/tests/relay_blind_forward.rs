//! Make-or-break: the inner client<->daemon mTLS session completes end-to-end
//! THROUGH the relay, which only ever pipes opaque bytes.
//!
//! Topology built here (all on loopback):
//!   client --(relay protocol)--> RelayServer <--(relay protocol)-- daemon bridge
//!   client --------------------- inner mTLS ------------------------> daemon mTLS listener
//! The relay pairs the two streams and `copy_bidirectional`s them; the inner
//! mTLS (server config from `zeroclaw_tls`, client cert issued from its CA)
//! handshakes across it unbroken. A second test checks admission control.
#![allow(clippy::disallowed_methods)]

use std::collections::HashSet;
use std::net::SocketAddr;
use std::sync::Arc;

use tokio::io::{AsyncReadExt, AsyncWriteExt};
use tokio::net::{TcpListener, TcpStream};
use tokio_rustls::{TlsAcceptor, TlsConnector};
use zeroclaw_relay_proto::Frame;
use zerorelay::{Admission, RelayConfig, RelayServer};

// ---- inner-mTLS helpers (server config + a cert-presenting client) ----

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

/// Returns (mTLS server config, client config presenting an issued client cert).
fn mtls_pair() -> (rustls::ServerConfig, rustls::ClientConfig) {
    let dir = tempfile::tempdir().unwrap();
    let mats = zeroclaw_tls::ensure_server_materials(dir.path(), &[]).unwrap();
    let server = zeroclaw_tls::build_mtls_server_config(
        mats.server_cert_path.to_str().unwrap(),
        mats.server_key_path.to_str().unwrap(),
        mats.ca_cert_path.to_str().unwrap(),
        &[],
    )
    .unwrap();

    let ca_pem = std::fs::read_to_string(&mats.ca_cert_path).unwrap();
    let ca_key_pem = std::fs::read_to_string(&mats.ca_key_path).unwrap();
    let issued = zeroclaw_tls::issue_client_cert(&ca_pem, &ca_key_pem, "relay-client").unwrap();
    let cert_f = write_temp(&issued.cert_pem);
    let key_f = write_temp(&issued.key_pem);
    let chain = zeroclaw_tls::load_certs(cert_f.path().to_str().unwrap()).unwrap();
    let key = zeroclaw_tls::load_private_key(key_f.path().to_str().unwrap()).unwrap();
    // Keep temp files alive until after the chain/key are loaded (they are).
    drop((cert_f, key_f, dir));

    let client = rustls::ClientConfig::builder_with_provider(Arc::new(
        rustls::crypto::ring::default_provider(),
    ))
    .with_safe_default_protocol_versions()
    .unwrap()
    .dangerous()
    .with_custom_certificate_verifier(Arc::new(NoServerVerify))
    .with_client_auth_cert(chain, key)
    .unwrap();

    (server, client)
}

// ---- relay-protocol helpers (byte-exact control-frame IO) ----

async fn write_frame(sock: &mut TcpStream, frame: &Frame) {
    sock.write_all(frame.to_line().as_bytes()).await.unwrap();
    sock.flush().await.unwrap();
}

async fn read_frame(sock: &mut TcpStream) -> Frame {
    let mut buf = Vec::new();
    let mut byte = [0u8; 1];
    loop {
        let n = sock.read(&mut byte).await.unwrap();
        assert!(n == 1, "connection closed before a control frame");
        if byte[0] == b'\n' {
            break;
        }
        buf.push(byte[0]);
    }
    Frame::from_line(&String::from_utf8(buf).unwrap()).unwrap()
}

async fn start_relay(cfg: RelayConfig) -> SocketAddr {
    let listener = TcpListener::bind("127.0.0.1:0").await.unwrap();
    let addr = listener.local_addr().unwrap();
    tokio::spawn(RelayServer::new(cfg).serve(listener));
    addr
}

/// Start an mTLS "daemon": echoes "pong\n" once the handshake completes.
async fn start_mtls_daemon(server: rustls::ServerConfig) -> SocketAddr {
    let listener = TcpListener::bind("127.0.0.1:0").await.unwrap();
    let addr = listener.local_addr().unwrap();
    let acceptor = TlsAcceptor::from(Arc::new(server));
    tokio::spawn(async move {
        loop {
            let (tcp, _) = listener.accept().await.unwrap();
            let acceptor = acceptor.clone();
            tokio::spawn(async move {
                if let Ok(mut tls) = acceptor.accept(tcp).await {
                    let mut buf = [0u8; 64];
                    if tls.read(&mut buf).await.unwrap_or(0) > 0 {
                        let _ = tls.write_all(b"pong\n").await;
                        let _ = tls.flush().await;
                    }
                }
            });
        }
    });
    addr
}

/// Daemon-side bridge: register `node_id`, and on each `Open` open a data
/// connection and pipe it to the local mTLS daemon.
async fn start_bridge(relay: SocketAddr, daemon: SocketAddr, node_id: &str, token: &str) {
    let mut ctrl = TcpStream::connect(relay).await.unwrap();
    write_frame(
        &mut ctrl,
        &Frame::Register {
            node_id: node_id.to_string(),
            relay_token: token.to_string(),
        },
    )
    .await;
    let reg = read_frame(&mut ctrl).await;
    assert!(
        matches!(reg, Frame::Registered { .. }),
        "bridge registration rejected: {reg:?}"
    );
    tokio::spawn(async move {
        while let Frame::Open { conn_id } = read_frame(&mut ctrl).await {
            tokio::spawn(async move {
                let mut data = TcpStream::connect(relay).await.unwrap();
                write_frame(
                    &mut data,
                    &Frame::Accept {
                        conn_id,
                        relay_token: String::new(),
                    },
                )
                .await;
                let mut local = TcpStream::connect(daemon).await.unwrap();
                let _ = tokio::io::copy_bidirectional(&mut data, &mut local).await;
            });
        }
    });
}

#[tokio::test]
async fn inner_mtls_completes_through_blind_relay() {
    let _ = rustls::crypto::ring::default_provider().install_default();
    let (server, client) = mtls_pair();

    let daemon = start_mtls_daemon(server).await;
    let relay = start_relay(RelayConfig::default()).await;
    start_bridge(relay, daemon, "node-1", "").await;

    // Client: ask the relay for node-1, then run the inner mTLS over the stream.
    let mut sock = TcpStream::connect(relay).await.unwrap();
    write_frame(
        &mut sock,
        &Frame::Connect {
            node_id: "node-1".into(),
        },
    )
    .await;
    let opened = read_frame(&mut sock).await;
    assert!(matches!(opened, Frame::Opened { .. }), "got {opened:?}");

    let connector = TlsConnector::from(Arc::new(client));
    let server_name = rustls::pki_types::ServerName::try_from("localhost")
        .unwrap()
        .to_owned();
    let mut tls = connector
        .connect(server_name, sock)
        .await
        .expect("inner mTLS must complete through the blind relay");

    // Exchange application bytes to prove the full duplex pipe works post-handshake.
    tls.write_all(b"ping\n").await.unwrap();
    tls.flush().await.unwrap();
    let mut buf = vec![0u8; 8];
    let n = tls.read(&mut buf).await.unwrap();
    assert_eq!(
        &buf[..n],
        b"pong\n",
        "echo did not round-trip through the relay"
    );
}

#[tokio::test]
async fn allowlist_rejects_unlisted_daemon() {
    let _ = rustls::crypto::ring::default_provider().install_default();
    let relay = start_relay(RelayConfig {
        registration_mode: Admission::Allowlist,
        allow: HashSet::from(["good-token".to_string()]),
        ..Default::default()
    })
    .await;

    // A daemon with an unlisted token is refused (forbidden), not registered.
    let mut ctrl = TcpStream::connect(relay).await.unwrap();
    write_frame(
        &mut ctrl,
        &Frame::Register {
            node_id: "node-x".into(),
            relay_token: "bad-token".into(),
        },
    )
    .await;
    let reply = read_frame(&mut ctrl).await;
    match reply {
        Frame::Error { code, .. } => assert_eq!(code, "forbidden"),
        other => panic!("expected forbidden, got {other:?}"),
    }
}

#[tokio::test]
async fn reconnect_does_not_self_evict() {
    // An honest daemon whose connection drops and reconnects (re-registering the
    // same node-id) must remain reachable: the stale connection's teardown must
    // not evict the live reconnection.
    let _ = rustls::crypto::ring::default_provider().install_default();
    let relay = start_relay(RelayConfig::default()).await;

    let mut a = TcpStream::connect(relay).await.unwrap();
    write_frame(
        &mut a,
        &Frame::Register {
            node_id: "node-1".into(),
            relay_token: String::new(),
        },
    )
    .await;
    assert!(matches!(read_frame(&mut a).await, Frame::Registered { .. }));

    // Reconnect (same node-id) wins via last-writer-wins.
    let mut b = TcpStream::connect(relay).await.unwrap();
    write_frame(
        &mut b,
        &Frame::Register {
            node_id: "node-1".into(),
            relay_token: String::new(),
        },
    )
    .await;
    assert!(matches!(read_frame(&mut b).await, Frame::Registered { .. }));

    // The stale connection goes away; its teardown must NOT remove node-1.
    drop(a);
    tokio::time::sleep(std::time::Duration::from_millis(200)).await;

    let mut c = TcpStream::connect(relay).await.unwrap();
    write_frame(
        &mut c,
        &Frame::Connect {
            node_id: "node-1".into(),
        },
    )
    .await;
    assert!(
        matches!(read_frame(&mut c).await, Frame::Opened { .. }),
        "live reconnection was self-evicted by the stale connection's teardown"
    );
}

#[tokio::test]
async fn pending_client_is_reaped_when_unpaired() {
    // A client parked for a daemon that never opens a data connection must be
    // reaped (its socket dropped) within the configured window, not leaked.
    let _ = rustls::crypto::ring::default_provider().install_default();
    let relay = start_relay(RelayConfig {
        pending_timeout: std::time::Duration::from_millis(200),
        ..Default::default()
    })
    .await;

    // Daemon registers but will never send Accept.
    let mut d = TcpStream::connect(relay).await.unwrap();
    write_frame(
        &mut d,
        &Frame::Register {
            node_id: "n".into(),
            relay_token: String::new(),
        },
    )
    .await;
    assert!(matches!(read_frame(&mut d).await, Frame::Registered { .. }));

    let mut c = TcpStream::connect(relay).await.unwrap();
    write_frame(
        &mut c,
        &Frame::Connect {
            node_id: "n".into(),
        },
    )
    .await;
    assert!(matches!(read_frame(&mut c).await, Frame::Opened { .. }));

    // After the reap window the relay drops the parked socket -> client sees EOF.
    let mut buf = [0u8; 1];
    let n = tokio::time::timeout(std::time::Duration::from_secs(3), c.read(&mut buf))
        .await
        .expect("reaper should close the unpaired parked socket")
        .unwrap();
    assert_eq!(n, 0, "parked client socket was not reaped");
}
