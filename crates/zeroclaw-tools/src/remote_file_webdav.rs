//! Read-side transport for operator-registered WebDAV resources.

use reqwest::header::{ACCEPT_ENCODING, AUTHORIZATION, CONTENT_LENGTH, HeaderValue};
use reqwest::{Client, Method, Response, StatusCode, Url};
use std::io;
use std::net::{IpAddr, SocketAddr};
use std::time::Duration;
use tokio::io::{AsyncWrite, AsyncWriteExt};
use zeroclaw_config::remote_files::is_relative_resource_path;

#[derive(Debug, thiserror::Error)]
pub enum WebDavError {
    #[error("remote_files_webdav_invalid_endpoint")]
    InvalidEndpoint,
    #[error("remote_files_webdav_invalid_path")]
    InvalidPath,
    #[error("remote_files_webdav_transport")]
    Transport,
    #[error("remote_files_webdav_response_status: {0}")]
    Status(StatusCode),
    #[error("remote_files_byte_limit")]
    ByteLimit,
    #[error("remote_files_webdav_destination_io")]
    DestinationIo(#[source] io::Error),
}

pub struct WebDavRead {
    root: Url,
    client: Client,
}

impl WebDavRead {
    /// Build from an operator-approved root and addresses already checked by
    /// outbound network policy. The caller must resolve current grants before
    /// building the client or supplying an authorization header.
    pub fn from_verified_addresses(
        root: Url,
        addresses: &[SocketAddr],
    ) -> Result<Self, WebDavError> {
        let client = build_client(&root, addresses)?
            .build()
            .map_err(|_| WebDavError::Transport)?;
        Ok(Self { root, client })
    }

    pub async fn file_size(
        &self,
        path: &str,
        authorization: Option<HeaderValue>,
    ) -> Result<Option<u64>, WebDavError> {
        let response = self.request(Method::HEAD, path, authorization).await?;
        Ok(response
            .headers()
            .get(CONTENT_LENGTH)
            .and_then(|value| value.to_str().ok())
            .and_then(|length| length.parse().ok()))
    }

    /// Download into a caller-owned staging writer. The caller owns cleanup,
    /// integrity verification and publication; this method never publishes.
    pub async fn download(
        &self,
        path: &str,
        authorization: Option<HeaderValue>,
        limit: u64,
        mut staging: impl AsyncWrite + Unpin,
    ) -> Result<u64, WebDavError> {
        let mut response = self.request(Method::GET, path, authorization).await?;
        if response
            .content_length()
            .is_some_and(|length| length > limit)
        {
            return Err(WebDavError::ByteLimit);
        }
        let mut written = 0;
        while let Some(chunk) = response.chunk().await.map_err(|_| WebDavError::Transport)? {
            let count = chunk.len() as u64;
            if count > limit - written {
                return Err(WebDavError::ByteLimit);
            }
            staging
                .write_all(&chunk)
                .await
                .map_err(WebDavError::DestinationIo)?;
            written += count;
        }
        staging.flush().await.map_err(WebDavError::DestinationIo)?;
        Ok(written)
    }

    async fn request(
        &self,
        method: Method,
        path: &str,
        authorization: Option<HeaderValue>,
    ) -> Result<Response, WebDavError> {
        let url = resource_url(&self.root, path)?;
        let mut request = self
            .client
            .request(method, url)
            .header(ACCEPT_ENCODING, "identity");
        if let Some(mut authorization) = authorization {
            authorization.set_sensitive(true);
            request = request.header(AUTHORIZATION, authorization);
        }
        let response = request.send().await.map_err(|_| WebDavError::Transport)?;
        if response.status() != StatusCode::OK {
            return Err(WebDavError::Status(response.status()));
        }
        Ok(response)
    }
}

fn build_client(
    root: &Url,
    addresses: &[SocketAddr],
) -> Result<reqwest::ClientBuilder, WebDavError> {
    let (host, port) = validate_endpoint(root, addresses)?;
    let mut builder = Client::builder()
        .https_only(true)
        .no_proxy()
        .redirect(reqwest::redirect::Policy::none())
        .connect_timeout(Duration::from_secs(10))
        .timeout(Duration::from_secs(60));
    if host.parse::<IpAddr>().is_err() {
        debug_assert!(addresses.iter().all(|address| address.port() == port));
        builder = builder.resolve_to_addrs(&host, addresses);
    }
    Ok(builder)
}

fn validate_endpoint(root: &Url, addresses: &[SocketAddr]) -> Result<(String, u16), WebDavError> {
    let host = root.host_str().ok_or(WebDavError::InvalidEndpoint)?;
    let port = root
        .port_or_known_default()
        .ok_or(WebDavError::InvalidEndpoint)?;
    let host = host.trim_matches(['[', ']']);
    let literal = host.parse::<IpAddr>().ok();
    if root.scheme() != "https"
        || !root.username().is_empty()
        || root.password().is_some()
        || root.query().is_some()
        || root.fragment().is_some()
        || !root.path().ends_with('/')
        || addresses.is_empty()
        || addresses
            .iter()
            .any(|address| address.port() != port || literal.is_some_and(|ip| ip != address.ip()))
    {
        return Err(WebDavError::InvalidEndpoint);
    }
    Ok((host.to_owned(), port))
}

fn resource_url(root: &Url, path: &str) -> Result<Url, WebDavError> {
    if !is_relative_resource_path(path) {
        return Err(WebDavError::InvalidPath);
    }
    let mut url = root.clone();
    url.path_segments_mut()
        .map_err(|()| WebDavError::InvalidEndpoint)?
        .pop_if_empty()
        .extend(path.split('/'));
    Ok(url)
}

#[cfg(test)]
mod tests {
    use super::*;

    #[tokio::test]
    async fn https_fixture_checks_metadata_transfer_limit_and_redirect() {
        use tokio::io::{AsyncReadExt, AsyncWriteExt};

        let listener = tokio::net::TcpListener::bind("127.0.0.1:0").await.unwrap();
        let addr = listener.local_addr().unwrap();
        let certificate = rcgen::generate_simple_self_signed(vec!["files.test".into()]).unwrap();
        let trust = reqwest::Certificate::from_der(certificate.cert.der()).unwrap();
        let server_config = rustls::ServerConfig::builder_with_provider(std::sync::Arc::new(
            rustls::crypto::ring::default_provider(),
        ))
        .with_safe_default_protocol_versions()
        .unwrap()
        .with_no_client_auth()
        .with_single_cert(
            vec![certificate.cert.der().clone()],
            rustls::pki_types::PrivatePkcs8KeyDer::from(certificate.key_pair.serialize_der())
                .into(),
        )
        .unwrap();
        let acceptor = tokio_rustls::TlsAcceptor::from(std::sync::Arc::new(server_config));
        let server = ::zeroclaw_spawn::spawn!(async move {
            for (method, path, response) in [
                (
                    "HEAD",
                    "report.txt",
                    "HTTP/1.1 200 OK\r\nContent-Length: 4\r\nConnection: close\r\n\r\n",
                ),
                (
                    "GET",
                    "report.txt",
                    "HTTP/1.1 200 OK\r\nContent-Length: 4\r\nConnection: close\r\n\r\ndemo",
                ),
                (
                    "GET",
                    "large.txt",
                    "HTTP/1.1 200 OK\r\nTransfer-Encoding: chunked\r\nConnection: close\r\n\r\n5\r\nlarge\r\n0\r\n\r\n",
                ),
                (
                    "GET",
                    "redirect.txt",
                    "HTTP/1.1 302 Found\r\nLocation: /elsewhere\r\nContent-Length: 0\r\nConnection: close\r\n\r\n",
                ),
            ] {
                let (stream, _) = listener.accept().await.unwrap();
                let mut tls = acceptor.accept(stream).await.unwrap();
                let mut request = Vec::new();
                while !request.ends_with(b"\r\n\r\n") {
                    assert!(request.len() < 8192);
                    request.push(tls.read_u8().await.unwrap());
                }
                let request = String::from_utf8(request).unwrap();
                assert!(request.starts_with(&format!("{method} /dav/{path} HTTP/1.1\r\n")));
                assert!(
                    request
                        .to_ascii_lowercase()
                        .contains("\r\nauthorization: bearer fixture-only\r\n")
                );
                tls.write_all(response.as_bytes()).await.unwrap();
                tls.shutdown().await.unwrap();
            }
        });

        let root = Url::parse(&format!("https://files.test:{}/dav/", addr.port())).unwrap();
        let client = build_client(&root, &[addr])
            .unwrap()
            .add_root_certificate(trust)
            .build()
            .unwrap();
        let adapter = WebDavRead { root, client };
        let authorization = || Some(HeaderValue::from_static("Bearer fixture-only"));
        tokio::time::timeout(Duration::from_secs(10), async {
            assert_eq!(
                adapter
                    .file_size("report.txt", authorization())
                    .await
                    .unwrap(),
                Some(4)
            );
            let mut staged = Vec::new();
            assert_eq!(
                adapter
                    .download("report.txt", authorization(), 4, &mut staged)
                    .await
                    .unwrap(),
                4
            );
            assert_eq!(staged, b"demo");
            staged.clear();
            assert!(matches!(
                adapter
                    .download("large.txt", authorization(), 4, &mut staged)
                    .await,
                Err(WebDavError::ByteLimit)
            ));
            assert!(staged.is_empty());
            assert!(matches!(
                adapter
                    .download("redirect.txt", authorization(), 4, &mut staged)
                    .await,
                Err(WebDavError::Status(StatusCode::FOUND))
            ));
            server.await.unwrap();
        })
        .await
        .unwrap();
    }

    #[test]
    fn url_stays_inside_registered_https_root() {
        let root = Url::parse("https://files.test:9443/dav/reports/").unwrap();
        let url = resource_url(&root, "office/May report.txt").unwrap();
        assert_eq!(
            url.as_str(),
            "https://files.test:9443/dav/reports/office/May%20report.txt"
        );
        for path in ["../other", "%2e%2e/other", "//other.test/path", "a?token=b"] {
            assert!(matches!(
                resource_url(&root, path),
                Err(WebDavError::InvalidPath)
            ));
        }
    }

    #[test]
    fn endpoint_requires_https_clean_root_and_matching_addresses() {
        let address = "192.0.2.22:9443".parse().unwrap();
        let root = Url::parse("https://files.test:9443/dav/").unwrap();
        assert!(WebDavRead::from_verified_addresses(root.clone(), &[address]).is_ok());
        assert!(validate_endpoint(&root, &[]).is_err());
        for value in [
            "http://files.test:9443/dav/",
            "https://user:secret@files.test:9443/dav/",
            "https://files.test:9443/dav/?query",
            "https://files.test:9443/dav/#fragment",
            "https://files.test:9443/dav",
            "https://192.0.2.23:9443/dav/",
            "https://files.test/dav/",
        ] {
            assert!(
                validate_endpoint(&Url::parse(value).unwrap(), &[address]).is_err(),
                "{value}"
            );
        }
    }
}
