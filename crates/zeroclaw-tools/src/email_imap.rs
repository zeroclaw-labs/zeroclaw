/// Shared IMAP connection utility used by email_search and email_read.
use std::fmt;
use std::io;
use std::pin::Pin;
use std::sync::Arc;
use std::task::{Context as TaskContext, Poll};

use anyhow::{Context, Result};
use async_imap::Session;
use rustls::{ClientConfig, RootCertStore};
use rustls_pki_types::pem::PemObject;
use rustls_pki_types::{CertificateDer, DnsName};
use tokio::io::{AsyncRead, AsyncWrite, ReadBuf};
use tokio::net::TcpStream;
use tokio_rustls::TlsConnector;
use tokio_rustls::client::TlsStream;
use zeroclaw_config::scattered_types::EmailConfig;

/// Tolerates TLS streams from servers that omit `close_notify` (e.g. Exchange).
pub struct TlsStreamTolerant(pub TlsStream<TcpStream>);

impl AsyncRead for TlsStreamTolerant {
    fn poll_read(
        mut self: Pin<&mut Self>,
        cx: &mut TaskContext<'_>,
        buf: &mut ReadBuf<'_>,
    ) -> Poll<io::Result<()>> {
        match Pin::new(&mut self.0).poll_read(cx, buf) {
            Poll::Ready(Err(e)) if is_tls_close_notify(&e) => Poll::Ready(Ok(())),
            other => other,
        }
    }
}

impl AsyncWrite for TlsStreamTolerant {
    fn poll_write(
        mut self: Pin<&mut Self>,
        cx: &mut TaskContext<'_>,
        buf: &[u8],
    ) -> Poll<io::Result<usize>> {
        Pin::new(&mut self.0).poll_write(cx, buf)
    }

    fn poll_flush(mut self: Pin<&mut Self>, cx: &mut TaskContext<'_>) -> Poll<io::Result<()>> {
        Pin::new(&mut self.0).poll_flush(cx)
    }

    fn poll_shutdown(mut self: Pin<&mut Self>, cx: &mut TaskContext<'_>) -> Poll<io::Result<()>> {
        Pin::new(&mut self.0).poll_shutdown(cx)
    }
}

impl fmt::Debug for TlsStreamTolerant {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        f.write_str("TlsStreamTolerant")
    }
}

/// Detects the missing-`close_notify` truncation that Exchange-hosted IMAP
/// servers produce. `ErrorKind::UnexpectedEof` is the stable signal; the
/// string match is a fallback for rustls variants that only surface it in the
/// display text.
pub fn is_tls_close_notify(e: &io::Error) -> bool {
    e.kind() == io::ErrorKind::UnexpectedEof || e.to_string().contains("close_notify")
}

pub type ImapSession = Session<TlsStreamTolerant>;

/// The certificates in `tls_ca_cert_path`, or none when it is not set. The
/// file is read with the same rules as an MCP server's `tls_ca_cert_path`;
/// an invalid file or one with no certificate is an error.
pub fn tls_ca_certs(cfg: &EmailConfig) -> Result<Vec<CertificateDer<'static>>> {
    let Some(path) = cfg.tls_ca_cert_path.as_deref() else {
        return Ok(Vec::new());
    };
    let pem = crate::mcp_transport::load_tls_ca_pem("email channel", path)?;
    let certs = CertificateDer::pem_slice_iter(&pem)
        .collect::<std::result::Result<Vec<_>, _>>()
        .with_context(|| format!("email channel: invalid PEM CA certificate at `{path}`"))?;
    if certs.is_empty() {
        anyhow::bail!("email channel: CA certificate file `{path}` contained no certificates");
    }
    Ok(certs)
}

/// Roots trusted for IMAP TLS: the bundled web PKI roots plus
/// `tls_ca_cert_path`.
pub fn imap_root_store(cfg: &EmailConfig) -> Result<RootCertStore> {
    let mut roots = RootCertStore {
        roots: webpki_roots::TLS_SERVER_ROOTS.into(),
    };
    for cert in tls_ca_certs(cfg)? {
        roots
            .add(cert)
            .context("email channel: unusable certificate in tls_ca_cert_path")?;
    }
    Ok(roots)
}

pub async fn imap_connect(
    cfg: &EmailConfig,
    auth_service: Option<&Arc<zeroclaw_providers::auth::AuthService>>,
    alias: &str,
) -> Result<ImapSession> {
    let addr = format!("{}:{}", cfg.imap_host, cfg.imap_port);
    let tcp = TcpStream::connect(&addr)
        .await
        .context("TCP connect failed")?;

    let tls_config = ClientConfig::builder()
        .with_root_certificates(imap_root_store(cfg)?)
        .with_no_client_auth();
    let connector: TlsConnector = Arc::new(tls_config).into();
    let sni: DnsName = cfg
        .imap_host
        .clone()
        .try_into()
        .context("invalid hostname")?;
    let raw = connector
        .connect(sni.into(), tcp)
        .await
        .context("TLS handshake failed")?;
    let stream = TlsStreamTolerant(raw);

    let mut client = async_imap::Client::new(stream);
    client.read_response().await.context("no IMAP greeting")?;

    if let Some(oauth2_cfg) = &cfg.oauth2 {
        let token = if let Some(svc) = auth_service {
            let channel_key = format!("email.{}", alias);
            svc.get_valid_email_oauth2_token(
                &channel_key,
                None,
                &oauth2_cfg.token_url,
                &oauth2_cfg.client_id,
                &oauth2_cfg.scopes,
            )
            .await?
            .ok_or_else(|| anyhow::Error::msg(format!("no OAuth2 token available for {}", alias)))?
        } else {
            anyhow::bail!(
                "oauth2 configured for '{}' but no auth service provided",
                alias
            );
        };

        struct XOAuth2 {
            user: String,
            token: String,
        }
        impl async_imap::Authenticator for XOAuth2 {
            type Response = String;
            fn process(&mut self, _: &[u8]) -> String {
                format!("user={}\x01auth=Bearer {}\x01\x01", self.user, self.token)
            }
        }
        client
            .authenticate(
                "XOAUTH2",
                XOAuth2 {
                    user: cfg.username.clone(),
                    token,
                },
            )
            .await
            .map_err(|(e, _)| anyhow::Error::msg(format!("XOAUTH2 auth failed: {}", e)))
    } else {
        client
            .login(&cfg.username, &cfg.password)
            .await
            .map_err(|(e, _)| anyhow::Error::msg(format!("IMAP login failed: {}", e)))
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn is_tls_close_notify_matches_unexpected_eof() {
        // Exchange-style truncation lands here. The ErrorKind is the stable
        // signal; message text is irrelevant for this branch.
        let e = io::Error::new(io::ErrorKind::UnexpectedEof, "anything");
        assert!(is_tls_close_notify(&e));
    }

    #[test]
    fn is_tls_close_notify_matches_string_marker() {
        // Some rustls variants only surface close_notify via the Display
        // string. The substring match is the fallback for that case.
        let e = io::Error::other("peer closed without sending close_notify alert");
        assert!(is_tls_close_notify(&e));
    }

    #[test]
    fn is_tls_close_notify_rejects_unrelated_errors() {
        // ConnectionReset / InvalidData must NOT be swallowed: those are
        // real failures the caller needs to see.
        let reset = io::Error::new(io::ErrorKind::ConnectionReset, "reset by peer");
        assert!(!is_tls_close_notify(&reset));

        let tls = io::Error::new(io::ErrorKind::InvalidData, "bad certificate");
        assert!(!is_tls_close_notify(&tls));

        // Other kind with a benign message must not match either — only the
        // exact substring `close_notify` triggers the string fallback.
        let benign = io::Error::other("operation successful");
        assert!(!is_tls_close_notify(&benign));
    }

    #[test]
    fn tls_ca_cert_path_adds_its_certificates_to_the_imap_roots() {
        let dir = tempfile::tempdir().unwrap();
        let path = dir.path().join("ca.pem");
        let cert = rcgen::generate_simple_self_signed(vec!["mail.internal".into()]).unwrap();
        std::fs::write(&path, cert.cert.pem()).unwrap();
        let cfg = EmailConfig {
            tls_ca_cert_path: Some(path.display().to_string()),
            ..EmailConfig::default()
        };

        assert_eq!(tls_ca_certs(&cfg).unwrap().len(), 1);
        let base = imap_root_store(&EmailConfig::default()).unwrap().len();
        assert_eq!(imap_root_store(&cfg).unwrap().len(), base + 1);
        assert!(tls_ca_certs(&EmailConfig::default()).unwrap().is_empty());
    }

    #[test]
    fn tls_ca_cert_path_follows_the_mcp_file_rules() {
        let dir = tempfile::tempdir().unwrap();
        let err_for = |path: String| {
            tls_ca_certs(&EmailConfig {
                tls_ca_cert_path: Some(path),
                ..EmailConfig::default()
            })
            .unwrap_err()
            .to_string()
        };

        assert!(err_for("ca.pem".into()).contains("must be absolute"));
        assert!(err_for("~/ca.pem".into()).contains("must be absolute"));
        let missing = dir.path().join("absent.pem").display().to_string();
        assert!(err_for(missing).contains("cannot read TLS CA certificate"));
        // Windows refuses to open a directory for reading at all.
        let dir_err = err_for(dir.path().display().to_string());
        assert!(
            dir_err.contains("must name a regular file")
                || dir_err.contains("cannot read TLS CA certificate"),
            "{dir_err}"
        );

        let big = dir.path().join("big.pem");
        std::fs::write(&big, vec![b'a'; 1024 * 1024 + 1]).unwrap();
        assert!(err_for(big.display().to_string()).contains("byte limit"));

        let empty = dir.path().join("empty.pem");
        std::fs::write(&empty, "not a certificate").unwrap();
        assert!(err_for(empty.display().to_string()).contains("contained no certificates"));
    }

    #[test]
    fn is_tls_close_notify_string_match_is_case_sensitive() {
        // The implementation does a literal `contains("close_notify")`. This
        // test pins that contract so a future case-insensitive refactor is a
        // conscious decision, not an accident.
        let upper = io::Error::other("Close_Notify received");
        assert!(!is_tls_close_notify(&upper));
    }
}
