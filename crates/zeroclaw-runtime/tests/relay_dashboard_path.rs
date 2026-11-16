//! The relay-served dashboard on real components: a browser's TLS connection
//! to the REAL [`zerorelay::RelayServer`] with `[dashboard]` on, routed by its
//! server name to a daemon registered by the REAL
//! [`zeroclaw_runtime::relay::run_relay_bridge`] with `[relay.dashboard]` on.
//!
//! What it shows end to end: the relay gives a dashboard host only to a daemon
//! that opted in AND that the operator listed, under the label derived from
//! that daemon's registration key; every other name gets "no dashboard here"
//! without the daemon being asked; and the daemon refuses a browser the relay
//! reports at a loopback address instead of serving it as this machine. (The
//! test's browser is on loopback, which is exactly that case; serving a
//! public browser through the gateway is covered by the gateway's own
//! `relay_dashboard_ingress` tests and the browser proof.)
#![allow(clippy::disallowed_methods)]

use std::collections::HashSet;
use std::sync::Arc;
use std::time::Duration;

use ring::signature::{Ed25519KeyPair, KeyPair};
use sha2::{Digest, Sha256};
use tokio::io::{AsyncReadExt, AsyncWriteExt};
use tokio::net::TcpListener;
use tokio_rustls::{TlsAcceptor, TlsConnector};
use tokio_util::sync::CancellationToken;
use zeroclaw_runtime::relay_dashboard::{DashboardIngress, DashboardOrigin};

const DOMAIN: &str = "relay.test";

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

/// The relay's outer TLS identity: one self-signed certificate for its own
/// name and the dashboard wildcard.
fn relay_acceptor() -> TlsAcceptor {
    let ck = rcgen::generate_simple_self_signed(vec!["localhost".into(), format!("*.{DOMAIN}")])
        .unwrap();
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

fn fingerprint(signing_key: &[u8]) -> String {
    let keypair = Ed25519KeyPair::from_pkcs8(signing_key).unwrap();
    Sha256::digest(keypair.public_key().as_ref())
        .iter()
        .map(|b| format!("{b:02x}"))
        .collect()
}

struct Scenario {
    relay_addr: std::net::SocketAddr,
    label: String,
    gateway_received: Arc<std::sync::atomic::AtomicUsize>,
    cancel: CancellationToken,
    _dir: tempfile::TempDir,
}

/// A relay with `[dashboard]` listing (or not) a daemon that opts in (or not),
/// whose gateway counts every connection it is handed.
async fn scenario(listed: bool, opted_in: bool) -> Scenario {
    let _ = rustls::crypto::ring::default_provider().install_default();
    let dir = tempfile::tempdir().unwrap();
    let signing_key = zeroclaw_runtime::relay::ensure_signing_key(dir.path()).unwrap();
    let label = zeroclaw_runtime::relay::dashboard_label(&signing_key).unwrap();

    let allow: HashSet<String> = if listed {
        HashSet::from([fingerprint(&signing_key)])
    } else {
        HashSet::from(["00".repeat(32)])
    };
    let relay_listener = TcpListener::bind("127.0.0.1:0").await.unwrap();
    let relay_addr = relay_listener.local_addr().unwrap();
    let relay = zerorelay::RelayServer::new(zerorelay::RelayConfig {
        dashboard: Some(zerorelay::DashboardConfig {
            domain: DOMAIN.into(),
            allow,
            max_conns_per_node: 8,
            max_conns_per_ip: 8,
        }),
        ..zerorelay::RelayConfig::default()
    });
    let probe = relay.clone();
    tokio::spawn(relay.serve(relay_listener, relay_acceptor()));

    // The gateway: attached, counting what it is handed.
    let ingress = DashboardIngress::new();
    let mut receiver = ingress.attach();
    let gateway_received = Arc::new(std::sync::atomic::AtomicUsize::new(0));
    {
        let gateway_received = gateway_received.clone();
        tokio::spawn(async move {
            while receiver.recv().await.is_some() {
                gateway_received.fetch_add(1, std::sync::atomic::Ordering::SeqCst);
            }
        });
    }
    let dashboard = opted_in.then(|| zeroclaw_runtime::relay::DashboardBridge {
        ingress,
        origin: DashboardOrigin::new(&format!("{label}.{DOMAIN}"), relay_addr.port()).unwrap(),
        max_conns: 8,
    });

    let cancel = CancellationToken::new();
    tokio::spawn(zeroclaw_runtime::relay::run_relay_bridge(
        zeroclaw_runtime::relay::RelayBridgeConfig {
            relay_addr: relay_addr.to_string(),
            relay_host: "localhost".into(),
            node_id: "dashboard-node".into(),
            relay_token: None,
            local_wss_addr: "127.0.0.1:9".into(),
            local_enroll_addr: None,
            enroll_bridge_ports: None,
            signing_key_pkcs8: signing_key,
            relay_ca_path: None,
            relay_insecure: true,
            relay_tofu: false,
            outer_client_cert: None,
            outer_client_key: None,
            max_conns: 16,
            open_burst: 60,
            open_rate_per_sec: 20.0,
            data_dir: dir.path().to_path_buf(),
            node_id_rotation_days: 0,
            rotation_allowed: false,
            dashboard,
        },
        cancel.clone(),
    ));
    // Registration is asynchronous: wait until the relay routes the node.
    tokio::time::timeout(Duration::from_secs(20), async {
        while !probe
            .status()
            .await
            .nodes
            .iter()
            .any(|node| node.node_id == "dashboard-node")
        {
            tokio::time::sleep(Duration::from_millis(20)).await;
        }
    })
    .await
    .expect("the daemon registers with the relay");
    Scenario {
        relay_addr,
        label,
        gateway_received,
        cancel,
        _dir: dir,
    }
}

/// A browser request to `server_name` on the relay: the status line it gets.
async fn browse(relay_addr: std::net::SocketAddr, server_name: &str) -> String {
    let mut cfg = rustls::ClientConfig::builder_with_provider(Arc::new(
        rustls::crypto::ring::default_provider(),
    ))
    .with_safe_default_protocol_versions()
    .unwrap()
    .dangerous()
    .with_custom_certificate_verifier(Arc::new(NoServerVerify))
    .with_no_client_auth();
    cfg.alpn_protocols = vec![b"h2".to_vec(), b"http/1.1".to_vec()];
    let tcp = tokio::net::TcpStream::connect(relay_addr).await.unwrap();
    let name = rustls::pki_types::ServerName::try_from(server_name.to_string()).unwrap();
    let mut tls = TlsConnector::from(Arc::new(cfg))
        .connect(name, tcp)
        .await
        .unwrap();
    tls.write_all(
        format!("GET / HTTP/1.1\r\nHost: {server_name}\r\nConnection: close\r\n\r\n").as_bytes(),
    )
    .await
    .unwrap();
    let mut reply = Vec::new();
    let _ = tokio::time::timeout(Duration::from_secs(20), tls.read_to_end(&mut reply)).await;
    String::from_utf8_lossy(&reply)
        .lines()
        .next()
        .unwrap_or_default()
        .to_string()
}

#[tokio::test]
async fn a_listed_daemon_that_opted_in_is_reached_and_refuses_a_loopback_browser() {
    let s = scenario(true, true).await;
    let status = browse(s.relay_addr, &format!("{}.{DOMAIN}", s.label)).await;
    // The relay routed it to the daemon (not "no dashboard here"), and the
    // daemon refused to serve a browser at a loopback address.
    assert!(status.starts_with("HTTP/1.1 503 "), "{status}");
    assert_eq!(
        s.gateway_received.load(std::sync::atomic::Ordering::SeqCst),
        0,
        "a loopback browser never reaches the gateway"
    );
    s.cancel.cancel();
}

#[tokio::test]
async fn a_daemon_the_operator_did_not_list_gets_no_dashboard_host() {
    let s = scenario(false, true).await;
    let status = browse(s.relay_addr, &format!("{}.{DOMAIN}", s.label)).await;
    assert!(status.starts_with("HTTP/1.1 404 "), "{status}");
    s.cancel.cancel();
}

#[tokio::test]
async fn a_daemon_that_did_not_opt_in_gets_no_dashboard_host() {
    let s = scenario(true, false).await;
    let status = browse(s.relay_addr, &format!("{}.{DOMAIN}", s.label)).await;
    assert!(status.starts_with("HTTP/1.1 404 "), "{status}");
    s.cancel.cancel();
}

#[tokio::test]
async fn an_unknown_dashboard_host_is_refused() {
    let s = scenario(true, true).await;
    let other = "a".repeat(zeroclaw_relay_proto::DASHBOARD_LABEL_LEN);
    assert_ne!(other, s.label);
    for name in [format!("{other}.{DOMAIN}"), format!("not-a-label.{DOMAIN}")] {
        let status = browse(s.relay_addr, &name).await;
        assert!(status.starts_with("HTTP/1.1 404 "), "{name}: {status}");
    }
    s.cancel.cancel();
}
