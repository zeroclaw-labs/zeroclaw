//! End-to-end transport test for the daemon's mutually-authenticated WSS plane.
//!
//! Drives a real TLS 1.3 + WebSocket handshake against an acceptor built by the
//! runtime's production `build_tls_acceptor` (the mandatory-mTLS path), using
//! auto-generated server materials (`zeroclaw_tls::ensure_server_materials`) and
//! a client certificate issued from that CA (`zeroclaw_tls::issue_client_cert`).
//! The accept sequence (TLS accept -> WebSocket upgrade) mirrors what
//! `run_wss_listener` does per connection.
//!
//! Proves: a client presenting a valid issued certificate completes the
//! handshake, and a client presenting none is rejected at the TLS layer.
//!
//! Test code, not daemon-path: bare `tokio::spawn` is fine here (the
//! `zeroclaw_spawn::spawn!` rule is for production daemon tasks).
#![allow(clippy::disallowed_methods)]

use std::path::Path;
use std::sync::Arc;
use tokio::net::TcpListener;

/// Server-cert verifier that accepts anything. The daemon under test enforces
/// CLIENT authentication; server-cert hostname verification is out of scope here
/// (standard rustls), so the test client skips it to isolate the mTLS invariant.
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

fn client_config(client_cert: Option<(&Path, &Path)>) -> rustls::ClientConfig {
    let builder = rustls::ClientConfig::builder_with_provider(Arc::new(
        rustls::crypto::ring::default_provider(),
    ))
    .with_safe_default_protocol_versions()
    .unwrap()
    .dangerous()
    .with_custom_certificate_verifier(Arc::new(NoServerVerify));

    match client_cert {
        Some((cert, key)) => {
            let chain = zeroclaw_tls::load_certs(cert.to_str().unwrap()).unwrap();
            let key = zeroclaw_tls::load_private_key(key.to_str().unwrap()).unwrap();
            builder.with_client_auth_cert(chain, key).unwrap()
        }
        None => builder.with_no_client_auth(),
    }
}

/// Auto-generate server materials and issue a client cert; write the client
/// cert/key to temp files. Returns (materials, client_cert_file, client_key_file).
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
    let client = zeroclaw_tls::issue_client_cert(&ca_pem, &ca_key_pem, "test-device").unwrap();
    (
        mats,
        write_temp(&client.cert_pem),
        write_temp(&client.key_pem),
    )
}

fn write_temp(content: &str) -> tempfile::NamedTempFile {
    use std::io::Write;
    let mut f = tempfile::NamedTempFile::new().unwrap();
    f.write_all(content.as_bytes()).unwrap();
    f.flush().unwrap();
    f
}

#[tokio::test]
async fn valid_client_cert_completes_tls_and_ws_handshake() {
    install_provider();
    let dir = tempfile::tempdir().unwrap();
    let (mats, client_cert, client_key) = materials(dir.path());

    let acceptor = zeroclaw_runtime::rpc::wss::build_tls_acceptor(
        mats.server_cert_path.to_str().unwrap(),
        mats.server_key_path.to_str().unwrap(),
        mats.ca_cert_path.to_str().unwrap(),
        &[],
    )
    .unwrap();

    let listener = TcpListener::bind("127.0.0.1:0").await.unwrap();
    let addr = listener.local_addr().unwrap();

    // Server: accept one connection exactly as run_wss_listener does, and report
    // the negotiated protocol version.
    let server = tokio::spawn(async move {
        let (tcp, _) = listener.accept().await.unwrap();
        let tls = acceptor.accept(tcp).await?; // mandatory mTLS handshake
        let version = tls.get_ref().1.protocol_version();
        let _ws = tokio_tungstenite::accept_async(tls).await?; // WS upgrade
        Ok::<Option<rustls::ProtocolVersion>, anyhow::Error>(version)
    });

    let connector = tokio_tungstenite::Connector::Rustls(Arc::new(client_config(Some((
        client_cert.path(),
        client_key.path(),
    )))));
    let url = format!("wss://127.0.0.1:{}/", addr.port());
    let (_ws, _resp) =
        tokio_tungstenite::connect_async_tls_with_config(&url, None, false, Some(connector))
            .await
            .expect("valid client cert must complete the mTLS + WS handshake");

    let version = server
        .await
        .unwrap()
        .expect("server side handshake must succeed");
    assert_eq!(
        version,
        Some(rustls::ProtocolVersion::TLSv1_3),
        "the remote plane must negotiate TLS 1.3"
    );
}

#[tokio::test]
async fn missing_client_cert_is_rejected() {
    install_provider();
    let dir = tempfile::tempdir().unwrap();
    let (mats, _cc, _ck) = materials(dir.path());

    let acceptor = zeroclaw_runtime::rpc::wss::build_tls_acceptor(
        mats.server_cert_path.to_str().unwrap(),
        mats.server_key_path.to_str().unwrap(),
        mats.ca_cert_path.to_str().unwrap(),
        &[],
    )
    .unwrap();

    let listener = TcpListener::bind("127.0.0.1:0").await.unwrap();
    let addr = listener.local_addr().unwrap();

    // Server: the mandatory client-auth handshake must fail for a certless client.
    let server = tokio::spawn(async move {
        let (tcp, _) = listener.accept().await.unwrap();
        acceptor.accept(tcp).await
    });

    // Client presents NO certificate.
    let connector = tokio_tungstenite::Connector::Rustls(Arc::new(client_config(None)));
    let url = format!("wss://127.0.0.1:{}/", addr.port());
    let connect_result =
        tokio_tungstenite::connect_async_tls_with_config(&url, None, false, Some(connector)).await;

    assert!(
        connect_result.is_err(),
        "a client presenting no certificate must be rejected"
    );
    // The rejection must be the MANDATORY-CLIENT-AUTH rejection, not an unrelated
    // failure: assert the server-side error names a certificate requirement.
    let server_result = server.await.unwrap();
    let err = match server_result {
        Ok(_) => panic!("the daemon accepted a client that presented no certificate"),
        Err(e) => e,
    };
    let msg = format!("{err:?}").to_lowercase();
    assert!(
        msg.contains("certificate") || msg.contains("certrequired") || msg.contains("required"),
        "expected a client-certificate rejection at the TLS layer, got: {msg}"
    );
}
