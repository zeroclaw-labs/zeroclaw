//! TLS and mutual TLS (mTLS) support for the gateway server.
//!
//! Builds a [`rustls::ServerConfig`] from the gateway TLS configuration,
//! optionally requiring client certificates verified against a trusted CA
//! with optional certificate pinning (SHA-256 fingerprint matching).

use anyhow::{Context, Result};
use rustls::RootCertStore;
use rustls::pki_types::{CertificateDer, PrivateKeyDer};
use rustls::server::WebPkiClientVerifier;
use rustls::server::danger::{ClientCertVerified, ClientCertVerifier};
use sha2::{Digest, Sha256};
use std::sync::Arc;
use tokio_rustls::TlsAcceptor;
use zeroclaw_config::schema::{GatewayClientAuthConfig, GatewayTlsConfig};

/// Build a [`TlsAcceptor`] from the gateway TLS configuration.
pub fn build_tls_acceptor(config: &GatewayTlsConfig) -> Result<TlsAcceptor> {
    let server_config = build_server_config(config)?;
    Ok(TlsAcceptor::from(Arc::new(server_config)))
}

/// Build a [`rustls::ServerConfig`] from the gateway TLS configuration.
pub fn build_server_config(config: &GatewayTlsConfig) -> Result<rustls::ServerConfig> {
    let certs = load_certs(&config.cert_path).with_context(|| {
        format!(
            "failed to load server certificate from {}",
            config.cert_path
        )
    })?;
    let key = load_private_key(&config.key_path)
        .with_context(|| format!("failed to load private key from {}", config.key_path))?;

    let client_auth_config = config.client_auth.as_ref().filter(|ca| ca.enabled);

    let builder = rustls::ServerConfig::builder();

    let server_config = if let Some(client_auth) = client_auth_config {
        let verifier = build_client_verifier(client_auth)
            .context("failed to build client certificate verifier")?;
        builder
            .with_client_cert_verifier(verifier)
            .with_single_cert(certs, key)
            .context("invalid server certificate or key")?
    } else {
        builder
            .with_no_client_auth()
            .with_single_cert(certs, key)
            .context("invalid server certificate or key")?
    };

    Ok(server_config)
}

/// Build a client certificate verifier from the client auth configuration.
fn build_client_verifier(config: &GatewayClientAuthConfig) -> Result<Arc<dyn ClientCertVerifier>> {
    let ca_certs = load_certs(&config.ca_cert_path)
        .with_context(|| format!("failed to load CA certificate from {}", config.ca_cert_path))?;

    let mut root_store = RootCertStore::empty();
    for cert in &ca_certs {
        root_store
            .add(cert.clone())
            .context("failed to add CA certificate to root store")?;
    }

    let base_verifier = if config.require_client_cert {
        WebPkiClientVerifier::builder(Arc::new(root_store))
            .build()
            .context("failed to build WebPKI client verifier")?
    } else {
        WebPkiClientVerifier::builder(Arc::new(root_store))
            .allow_unauthenticated()
            .build()
            .context("failed to build WebPKI client verifier (optional auth)")?
    };

    if config.pinned_certs.is_empty() {
        Ok(base_verifier)
    } else {
        let normalized: Vec<String> = config
            .pinned_certs
            .iter()
            .map(|fp| fp.replace(':', "").to_lowercase())
            .collect();
        Ok(Arc::new(PinnedCertVerifier {
            inner: base_verifier,
            pinned_fingerprints: normalized,
        }))
    }
}

/// Compute the SHA-256 fingerprint of a DER-encoded certificate.
pub fn cert_sha256_fingerprint(cert_der: &[u8]) -> String {
    let mut hasher = Sha256::new();
    hasher.update(cert_der);
    let hash = hasher.finalize();
    hex::encode(hash)
}

/// A client certificate verifier that delegates to a base verifier and then
/// checks that the presented certificate matches one of the pinned SHA-256
/// fingerprints.
#[derive(Debug)]
struct PinnedCertVerifier {
    inner: Arc<dyn ClientCertVerifier>,
    pinned_fingerprints: Vec<String>,
}

impl ClientCertVerifier for PinnedCertVerifier {
    fn offer_client_auth(&self) -> bool {
        self.inner.offer_client_auth()
    }

    fn client_auth_mandatory(&self) -> bool {
        self.inner.client_auth_mandatory()
    }

    fn root_hint_subjects(&self) -> &[rustls::DistinguishedName] {
        self.inner.root_hint_subjects()
    }

    fn verify_client_cert(
        &self,
        end_entity: &CertificateDer<'_>,
        intermediates: &[CertificateDer<'_>],
        now: rustls::pki_types::UnixTime,
    ) -> std::result::Result<ClientCertVerified, rustls::Error> {
        // First, run the standard WebPKI verification.
        self.inner
            .verify_client_cert(end_entity, intermediates, now)?;

        // Then check the fingerprint against the pinned set.
        let fingerprint = cert_sha256_fingerprint(end_entity.as_ref());
        if self.pinned_fingerprints.contains(&fingerprint) {
            Ok(ClientCertVerified::assertion())
        } else {
            Err(rustls::Error::General(format!(
                "client certificate fingerprint {fingerprint} is not in the pinned set"
            )))
        }
    }

    fn verify_tls12_signature(
        &self,
        message: &[u8],
        cert: &CertificateDer<'_>,
        dss: &rustls::DigitallySignedStruct,
    ) -> std::result::Result<rustls::client::danger::HandshakeSignatureValid, rustls::Error> {
        self.inner.verify_tls12_signature(message, cert, dss)
    }

    fn verify_tls13_signature(
        &self,
        message: &[u8],
        cert: &CertificateDer<'_>,
        dss: &rustls::DigitallySignedStruct,
    ) -> std::result::Result<rustls::client::danger::HandshakeSignatureValid, rustls::Error> {
        self.inner.verify_tls13_signature(message, cert, dss)
    }

    fn supported_verify_schemes(&self) -> Vec<rustls::SignatureScheme> {
        self.inner.supported_verify_schemes()
    }
}

/// Load PEM-encoded certificates from a file.
fn load_certs(path: &str) -> Result<Vec<CertificateDer<'static>>> {
    let file = std::fs::File::open(path)
        .with_context(|| format!("cannot open certificate file: {path}"))?;
    let mut reader = std::io::BufReader::new(file);
    let certs: Vec<CertificateDer<'static>> = rustls_pemfile::certs(&mut reader)
        .collect::<std::result::Result<Vec<_>, _>>()
        .with_context(|| format!("failed to parse PEM certificates from {path}"))?;
    if certs.is_empty() {
        anyhow::bail!("no certificates found in {path}");
    }
    Ok(certs)
}

/// Load a PEM-encoded private key from a file.
fn load_private_key(path: &str) -> Result<PrivateKeyDer<'static>> {
    let file = std::fs::File::open(path)
        .with_context(|| format!("cannot open private key file: {path}"))?;
    let mut reader = std::io::BufReader::new(file);
    let key = rustls_pemfile::private_key(&mut reader)
        .with_context(|| format!("failed to parse private key from {path}"))?
        .ok_or_else(|| anyhow::anyhow!("no private key found in {path}"))?;
    Ok(key)
}

#[cfg(test)]
mod tests {
    use super::*;

    /// Ensure the rustls `CryptoProvider` is installed (idempotent).
    fn ensure_crypto_provider() {
        let _ = rustls::crypto::ring::default_provider().install_default();
    }

    /// Generate a self-signed CA cert + key pair.
    /// Returns (cert_pem, key_pem, key_pair) so the key can be reused for signing.
    fn test_ca() -> (String, String, rcgen::KeyPair) {
        let ca_key = rcgen::KeyPair::generate().unwrap();
        let mut ca_params = rcgen::CertificateParams::new(vec!["Test CA".into()]).unwrap();
        ca_params.is_ca = rcgen::IsCa::Ca(rcgen::BasicConstraints::Unconstrained);
        let ca_cert = ca_params.self_signed(&ca_key).unwrap();
        (ca_cert.pem(), ca_key.serialize_pem(), ca_key)
    }

    /// Generate a server certificate signed by the given CA.
    fn test_server_cert(ca_cert_pem: &str, ca_key: &rcgen::KeyPair) -> (String, String) {
        // Re-parse the CA cert for signing.
        let ca_key_clone = rcgen::KeyPair::from_pem(&ca_key.serialize_pem()).unwrap();
        let mut ca_params = rcgen::CertificateParams::new(vec!["Test CA".into()]).unwrap();
        ca_params.is_ca = rcgen::IsCa::Ca(rcgen::BasicConstraints::Unconstrained);
        let ca = ca_params.self_signed(&ca_key_clone).unwrap();

        let mut server_params = rcgen::CertificateParams::new(vec!["localhost".into()]).unwrap();
        server_params.is_ca = rcgen::IsCa::NoCa;
        let server_key = rcgen::KeyPair::generate().unwrap();
        let server_cert = server_params
            .signed_by(&server_key, &ca, &ca_key_clone)
            .unwrap();
        let _ = ca_cert_pem;
        (server_cert.pem(), server_key.serialize_pem())
    }

    fn write_temp_file(content: &str) -> tempfile::NamedTempFile {
        use std::io::Write;
        let mut f = tempfile::NamedTempFile::new().unwrap();
        f.write_all(content.as_bytes()).unwrap();
        f.flush().unwrap();
        f
    }

    #[test]
    fn test_load_valid_cert_and_key() {
        let (ca_cert_pem, _ca_key_pem, ca_key) = test_ca();
        let (server_cert_pem, server_key_pem) = test_server_cert(&ca_cert_pem, &ca_key);

        let cert_file = write_temp_file(&server_cert_pem);
        let key_file = write_temp_file(&server_key_pem);

        let certs = load_certs(cert_file.path().to_str().unwrap()).unwrap();
        assert!(!certs.is_empty());

        let _key = load_private_key(key_file.path().to_str().unwrap()).unwrap();
    }

    #[test]
    fn test_invalid_cert_path_produces_clear_error() {
        let err = load_certs("/nonexistent/path/cert.pem").unwrap_err();
        let msg = format!("{err:#}");
        assert!(
            msg.contains("cannot open certificate file"),
            "unexpected error: {msg}"
        );
    }

    #[test]
    fn test_invalid_key_path_produces_clear_error() {
        let err = load_private_key("/nonexistent/path/key.pem").unwrap_err();
        let msg = format!("{err:#}");
        assert!(
            msg.contains("cannot open private key file"),
            "unexpected error: {msg}"
        );
    }

    #[test]
    fn test_build_server_config_no_client_auth() {
        ensure_crypto_provider();
        let (ca_cert_pem, _ca_key_pem, ca_key) = test_ca();
        let (server_cert_pem, server_key_pem) = test_server_cert(&ca_cert_pem, &ca_key);

        let cert_file = write_temp_file(&server_cert_pem);
        let key_file = write_temp_file(&server_key_pem);

        let tls_config = GatewayTlsConfig {
            enabled: true,
            cert_path: cert_file.path().to_str().unwrap().to_string(),
            key_path: key_file.path().to_str().unwrap().to_string(),
            client_auth: None,
        };

        // Should build successfully without client auth.
        let _server_config = build_server_config(&tls_config).unwrap();
    }

    #[test]
    fn test_build_server_config_with_client_auth() {
        ensure_crypto_provider();
        let (ca_cert_pem, _ca_key_pem, ca_key) = test_ca();
        let (server_cert_pem, server_key_pem) = test_server_cert(&ca_cert_pem, &ca_key);

        let cert_file = write_temp_file(&server_cert_pem);
        let key_file = write_temp_file(&server_key_pem);
        let ca_file = write_temp_file(&ca_cert_pem);

        let tls_config = GatewayTlsConfig {
            enabled: true,
            cert_path: cert_file.path().to_str().unwrap().to_string(),
            key_path: key_file.path().to_str().unwrap().to_string(),
            client_auth: Some(GatewayClientAuthConfig {
                enabled: true,
                ca_cert_path: ca_file.path().to_str().unwrap().to_string(),
                require_client_cert: true,
                pinned_certs: vec![],
            }),
        };

        // Should build successfully with mandatory client auth.
        let _server_config = build_server_config(&tls_config).unwrap();
    }

    #[test]
    fn test_build_server_config_client_auth_optional() {
        ensure_crypto_provider();
        let (ca_cert_pem, _ca_key_pem, ca_key) = test_ca();
        let (server_cert_pem, server_key_pem) = test_server_cert(&ca_cert_pem, &ca_key);

        let cert_file = write_temp_file(&server_cert_pem);
        let key_file = write_temp_file(&server_key_pem);
        let ca_file = write_temp_file(&ca_cert_pem);

        let tls_config = GatewayTlsConfig {
            enabled: true,
            cert_path: cert_file.path().to_str().unwrap().to_string(),
            key_path: key_file.path().to_str().unwrap().to_string(),
            client_auth: Some(GatewayClientAuthConfig {
                enabled: true,
                ca_cert_path: ca_file.path().to_str().unwrap().to_string(),
                require_client_cert: false,
                pinned_certs: vec![],
            }),
        };

        // Should build successfully with optional client auth.
        let _server_config = build_server_config(&tls_config).unwrap();
    }

    #[test]
    fn test_cert_fingerprint_matching() {
        let (ca_cert_pem, _ca_key_pem, _ca_key) = test_ca();
        let ca_file = write_temp_file(&ca_cert_pem);
        let certs = load_certs(ca_file.path().to_str().unwrap()).unwrap();
        let fingerprint = cert_sha256_fingerprint(certs[0].as_ref());

        // Fingerprint should be a 64-char hex string (SHA-256).
        assert_eq!(fingerprint.len(), 64);
        assert!(fingerprint.chars().all(|c| c.is_ascii_hexdigit()));

        // Same cert should produce the same fingerprint.
        let fingerprint2 = cert_sha256_fingerprint(certs[0].as_ref());
        assert_eq!(fingerprint, fingerprint2);
    }

    #[test]
    fn test_fingerprint_differs_for_different_certs() {
        let (ca_cert_pem1, _, _) = test_ca();
        let (ca_cert_pem2, _, _) = test_ca();
        let f1 = write_temp_file(&ca_cert_pem1);
        let f2 = write_temp_file(&ca_cert_pem2);
        let certs1 = load_certs(f1.path().to_str().unwrap()).unwrap();
        let certs2 = load_certs(f2.path().to_str().unwrap()).unwrap();
        let fp1 = cert_sha256_fingerprint(certs1[0].as_ref());
        let fp2 = cert_sha256_fingerprint(certs2[0].as_ref());
        assert_ne!(fp1, fp2);
    }

    #[test]
    fn test_config_defaults_deserialization() {
        let toml_str = r#"
            cert_path = "/tmp/cert.pem"
            key_path = "/tmp/key.pem"
        "#;
        let config: GatewayTlsConfig = toml::from_str(toml_str).unwrap();
        assert!(!config.enabled);
        assert!(config.client_auth.is_none());
    }

    #[test]
    fn test_client_auth_config_defaults() {
        let toml_str = r#"
            ca_cert_path = "/tmp/ca.pem"
        "#;
        let config: GatewayClientAuthConfig = toml::from_str(toml_str).unwrap();
        assert!(!config.enabled);
        assert!(config.require_client_cert);
        assert!(config.pinned_certs.is_empty());
    }

    #[test]
    fn test_build_server_config_with_pinning() {
        ensure_crypto_provider();
        let (ca_cert_pem, _ca_key_pem, ca_key) = test_ca();
        let (server_cert_pem, server_key_pem) = test_server_cert(&ca_cert_pem, &ca_key);

        let cert_file = write_temp_file(&server_cert_pem);
        let key_file = write_temp_file(&server_key_pem);
        let ca_file = write_temp_file(&ca_cert_pem);

        let tls_config = GatewayTlsConfig {
            enabled: true,
            cert_path: cert_file.path().to_str().unwrap().to_string(),
            key_path: key_file.path().to_str().unwrap().to_string(),
            client_auth: Some(GatewayClientAuthConfig {
                enabled: true,
                ca_cert_path: ca_file.path().to_str().unwrap().to_string(),
                require_client_cert: true,
                pinned_certs: vec!["aabbccdd".to_string()],
            }),
        };

        // Should build successfully - pinning is checked at connection time, not config time.
        let _server_config = build_server_config(&tls_config).unwrap();
    }

    #[test]
    fn test_empty_cert_file_produces_error() {
        let empty_file = write_temp_file("");
        let err = load_certs(empty_file.path().to_str().unwrap()).unwrap_err();
        let msg = format!("{err:#}");
        assert!(
            msg.contains("no certificates found"),
            "unexpected error: {msg}"
        );
    }

    #[test]
    fn test_disabled_client_auth_skipped() {
        ensure_crypto_provider();
        let (ca_cert_pem, _ca_key_pem, ca_key) = test_ca();
        let (server_cert_pem, server_key_pem) = test_server_cert(&ca_cert_pem, &ca_key);

        let cert_file = write_temp_file(&server_cert_pem);
        let key_file = write_temp_file(&server_key_pem);

        // client_auth present but enabled=false should be treated as no client auth.
        let tls_config = GatewayTlsConfig {
            enabled: true,
            cert_path: cert_file.path().to_str().unwrap().to_string(),
            key_path: key_file.path().to_str().unwrap().to_string(),
            client_auth: Some(GatewayClientAuthConfig {
                enabled: false,
                ca_cert_path: "/nonexistent".to_string(),
                require_client_cert: true,
                pinned_certs: vec![],
            }),
        };

        // Should succeed because client_auth.enabled=false skips the CA loading.
        let _server_config = build_server_config(&tls_config).unwrap();
    }
}
