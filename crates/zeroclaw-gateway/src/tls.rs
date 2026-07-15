//! TLS and mutual TLS (mTLS) support for the gateway server.
//!
//! Thin adapter over the shared [`zeroclaw_tls`] crate: it maps the gateway's
//! [`GatewayTlsConfig`] onto the transport-neutral
//! [`zeroclaw_tls::ServerConfigParams`] and delegates construction. The reusable
//! verifier / certificate-pinning / PEM-loading logic lives in `zeroclaw-tls` so
//! it can also be consumed by `zeroclaw-runtime` without an upward dependency on
//! the gateway.

use anyhow::Result;
use tokio_rustls::TlsAcceptor;
use zeroclaw_config::schema::GatewayTlsConfig;
use zeroclaw_tls::{ClientAuthParams, ServerConfigParams};

/// Build a [`TlsAcceptor`] from the gateway TLS configuration.
pub fn build_tls_acceptor(config: &GatewayTlsConfig) -> Result<TlsAcceptor> {
    zeroclaw_tls::build_tls_acceptor(&to_params(config))
}

/// Build a [`rustls::ServerConfig`] from the gateway TLS configuration.
pub fn build_server_config(config: &GatewayTlsConfig) -> Result<rustls::ServerConfig> {
    zeroclaw_tls::build_server_config(&to_params(config))
}

/// Map the gateway TLS config onto the transport-neutral params.
///
/// Client authentication is enabled only when the `[gateway.tls.client_auth]`
/// block is present AND `enabled = true`, preserving the previous
/// `.filter(|ca| ca.enabled)` behavior (a `client_auth` with `enabled = false`
/// is treated as server-only TLS, skipping CA loading).
fn to_params(config: &GatewayTlsConfig) -> ServerConfigParams {
    let client_auth = config
        .client_auth
        .as_ref()
        .filter(|ca| ca.enabled)
        .map(|ca| ClientAuthParams {
            ca_cert_path: ca.ca_cert_path.clone(),
            require_client_cert: ca.require_client_cert,
            pinned_certs: ca.pinned_certs.clone(),
            crl_path: ca.crl_path.clone(),
        });

    ServerConfigParams {
        cert_path: config.cert_path.clone(),
        key_path: config.key_path.clone(),
        client_auth,
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use zeroclaw_config::schema::{GatewayClientAuthConfig, GatewayTlsConfig};

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
                crl_path: String::new(),
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
                crl_path: String::new(),
            }),
        };

        // Should build successfully with optional client auth.
        let _server_config = build_server_config(&tls_config).unwrap();
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
                crl_path: String::new(),
            }),
        };

        // Should build successfully - pinning is checked at connection time, not config time.
        let _server_config = build_server_config(&tls_config).unwrap();
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
                crl_path: String::new(),
            }),
        };

        // Should succeed because client_auth.enabled=false skips the CA loading.
        let _server_config = build_server_config(&tls_config).unwrap();
    }
}
