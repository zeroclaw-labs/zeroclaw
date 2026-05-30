/// Shared IMAP connection utility used by email_search and email_read.
use std::fmt;
use std::io;
use std::pin::Pin;
use std::sync::Arc;
use std::task::{Context as TaskContext, Poll};

use anyhow::{Context, Result};
use async_imap::Session;
use rustls::{ClientConfig, RootCertStore};
use rustls_pki_types::DnsName;
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

fn is_tls_close_notify(e: &io::Error) -> bool {
    let msg = e.to_string();
    msg.contains("close_notify") || msg.contains("UnexpectedEof")
}

pub type ImapSession = Session<TlsStreamTolerant>;

pub async fn imap_connect(
    cfg: &EmailConfig,
    auth_service: Option<&Arc<zeroclaw_providers::auth::AuthService>>,
    alias: &str,
) -> Result<ImapSession> {
    let addr = format!("{}:{}", cfg.imap_host, cfg.imap_port);
    let tcp = TcpStream::connect(&addr)
        .await
        .context("TCP connect failed")?;

    let certs = RootCertStore {
        roots: webpki_roots::TLS_SERVER_ROOTS.into(),
    };
    let tls_config = ClientConfig::builder()
        .with_root_certificates(certs)
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
