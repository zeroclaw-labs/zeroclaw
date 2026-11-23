//! Read-side transport for operator-registered WebDAV resources.

use crate::remote_file_transfer::PublishResult;
use reqwest::header::{ACCEPT_ENCODING, AUTHORIZATION, CONTENT_LENGTH, HeaderValue, IF_NONE_MATCH};
use reqwest::{Client, Method, Response, StatusCode, Url};
use std::io;
use std::net::{IpAddr, SocketAddr};
use std::sync::{
    Arc,
    atomic::{AtomicBool, Ordering},
};
use std::time::Duration;
use tokio::io::{AsyncRead, AsyncReadExt, AsyncWrite, AsyncWriteExt};
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

    /// Stream a new staging object. The executor owns publication and cleanup.
    /// Size is an optional source observation, not an allocation request.
    pub async fn upload_new_staging_file(
        &self,
        path: &str,
        authorization: Option<HeaderValue>,
        source: impl AsyncRead + Unpin + Send + 'static,
        size: Option<u64>,
        limit: Option<u64>,
    ) -> Result<(), WebDavError> {
        let url = resource_url(&self.root, path)?;
        if let (Some(size), Some(limit)) = (size, limit)
            && size > limit
        {
            return Err(WebDavError::ByteLimit);
        }

        let progress = Arc::new(UploadProgress::default());
        let stream = upload_stream(source, size, limit, Arc::clone(&progress));
        let request = self
            .client
            .put(url)
            .header(IF_NONE_MATCH, "*")
            .body(reqwest::Body::wrap_stream(stream));
        let result = with_authorization(request, authorization).send().await;
        if progress.limit_reached.load(Ordering::Acquire) {
            return Err(WebDavError::ByteLimit);
        }
        let response = require_status(
            result.map_err(|_| WebDavError::Transport)?,
            StatusCode::CREATED,
        )?;
        drop(response);
        if !progress.finished.load(Ordering::Acquire) {
            return Err(WebDavError::Transport);
        }
        Ok(())
    }

    /// Create exactly one directory. Parent creation and authorization are
    /// handled separately by the caller.
    pub async fn create_directory(
        &self,
        path: &str,
        authorization: Option<HeaderValue>,
    ) -> Result<(), WebDavError> {
        let method = Method::from_bytes(b"MKCOL").map_err(|_| WebDavError::Transport)?;
        let response = self
            .send(
                method,
                resource_url(&self.root, path)?,
                authorization,
                None,
                None,
            )
            .await?;
        require_status(response, StatusCode::CREATED).map(|_| ())
    }

    /// Copy a file within this registered resource without routing its bytes
    /// through ZeroClaw. The executor must authorize both paths and verify
    /// that the source is a file. Depth zero forbids recursive collection copy.
    pub async fn copy_file_on_server(
        &self,
        source: &str,
        destination: &str,
        authorization: Option<HeaderValue>,
    ) -> Result<(), WebDavError> {
        if source == destination {
            return Err(WebDavError::InvalidPath);
        }
        let source = resource_url(&self.root, source)?;
        let destination = resource_url(&self.root, destination)?;
        let method = Method::from_bytes(b"COPY").map_err(|_| WebDavError::Transport)?;
        let response = self
            .send(method, source, authorization, Some(destination), Some("0"))
            .await?;
        require_status(response, StatusCode::CREATED).map(|_| ())
    }

    /// Commit the verified staging object in its parent directory. Only a
    /// definite server rejection is reported as rejected. All other failures
    /// leave the executor responsible for reconciling state.
    pub async fn publish_staging(
        &self,
        staging: &str,
        destination: &str,
        authorization: Option<HeaderValue>,
    ) -> PublishResult {
        match self.rename(staging, destination, authorization).await {
            Ok(()) => PublishResult::Confirmed,
            Err(WebDavError::InvalidPath | WebDavError::InvalidEndpoint)
            | Err(WebDavError::Status(StatusCode::PRECONDITION_FAILED)) => PublishResult::Rejected,
            Err(_) => PublishResult::Unknown,
        }
    }

    /// Rename within the same parent and registered WebDAV resource. The
    /// destination is never supplied as a URL by the model. Callers must
    /// authorize both paths and any affected directory subtree first.
    pub async fn rename(
        &self,
        source: &str,
        destination: &str,
        authorization: Option<HeaderValue>,
    ) -> Result<(), WebDavError> {
        let parent = |path: &str| {
            path.rsplit_once('/')
                .map_or("", |(parent, _)| parent)
                .to_owned()
        };
        if source == destination || parent(source) != parent(destination) {
            return Err(WebDavError::InvalidPath);
        }
        let source = resource_url(&self.root, source)?;
        let destination = resource_url(&self.root, destination)?;
        let method = Method::from_bytes(b"MOVE").map_err(|_| WebDavError::Transport)?;
        let response = self
            .send(method, source, authorization, Some(destination), None)
            .await?;
        require_status(response, StatusCode::CREATED).map(|_| ())
    }

    async fn request(
        &self,
        method: Method,
        path: &str,
        authorization: Option<HeaderValue>,
    ) -> Result<Response, WebDavError> {
        let response = self
            .send(
                method,
                resource_url(&self.root, path)?,
                authorization,
                None,
                None,
            )
            .await?;
        require_status(response, StatusCode::OK)
    }

    async fn send(
        &self,
        method: Method,
        url: Url,
        authorization: Option<HeaderValue>,
        destination: Option<Url>,
        depth: Option<&'static str>,
    ) -> Result<Response, WebDavError> {
        let mut request = self
            .client
            .request(method, url)
            .timeout(Duration::from_secs(60))
            .header(ACCEPT_ENCODING, "identity");
        if let Some(destination) = destination {
            request = request
                .header("Destination", destination.as_str())
                .header("Overwrite", "F");
        }
        if let Some(depth) = depth {
            request = request.header("Depth", depth);
        }
        with_authorization(request, authorization)
            .send()
            .await
            .map_err(|_| WebDavError::Transport)
    }
}

#[derive(Default)]
struct UploadProgress {
    limit_reached: AtomicBool,
    finished: AtomicBool,
}

fn upload_stream<R: AsyncRead + Unpin>(
    source: R,
    expected_size: Option<u64>,
    limit: Option<u64>,
    progress: Arc<UploadProgress>,
) -> impl futures_util::Stream<Item = io::Result<Vec<u8>>> {
    const CHUNK_SIZE: usize = 64 * 1024;
    futures_util::stream::try_unfold((source, 0_u64), move |(mut source, sent)| {
        let progress = Arc::clone(&progress);
        async move {
            let remaining = limit.map_or(u64::MAX, |limit| limit.saturating_sub(sent));
            let capacity = remaining.saturating_add(1).min(CHUNK_SIZE as u64) as usize;
            let mut buffer = vec![0; capacity];
            let count = tokio::time::timeout(Duration::from_secs(30), source.read(&mut buffer))
                .await
                .map_err(|_| io::ErrorKind::TimedOut)??;
            if count as u64 > remaining {
                progress.limit_reached.store(true, Ordering::Release);
                return Err(io::Error::new(
                    io::ErrorKind::InvalidData,
                    "remote_files_byte_limit",
                ));
            }
            let total = sent
                .checked_add(count as u64)
                .ok_or(io::ErrorKind::InvalidData)?;
            if expected_size.is_some_and(|size| total > size || (count == 0 && total != size)) {
                return Err(io::Error::new(
                    io::ErrorKind::InvalidData,
                    "remote_files_source_size_changed",
                ));
            }
            if count == 0 {
                progress.finished.store(true, Ordering::Release);
                return Ok(None);
            }
            buffer.truncate(count);
            Ok(Some((buffer, (source, total))))
        }
    })
}

fn with_authorization(
    request: reqwest::RequestBuilder,
    authorization: Option<HeaderValue>,
) -> reqwest::RequestBuilder {
    match authorization {
        Some(mut value) => {
            value.set_sensitive(true);
            request.header(AUTHORIZATION, value)
        }
        None => request,
    }
}

fn require_status(response: Response, expected: StatusCode) -> Result<Response, WebDavError> {
    if response.status() != expected {
        return Err(WebDavError::Status(response.status()));
    }
    Ok(response)
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
        .read_timeout(Duration::from_secs(30));
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
    async fn https_fixture_checks_reads_mutations_limits_and_redirects() {
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
                (
                    "MKCOL",
                    "docs",
                    "HTTP/1.1 201 Created\r\nContent-Length: 0\r\nConnection: close\r\n\r\n",
                ),
                (
                    "MOVE",
                    "docs/old.txt",
                    "HTTP/1.1 201 Created\r\nContent-Length: 0\r\nConnection: close\r\n\r\n",
                ),
                (
                    "MOVE",
                    "docs/new.txt",
                    "HTTP/1.1 412 Precondition Failed\r\nContent-Length: 0\r\nConnection: close\r\n\r\n",
                ),
                (
                    "PUT",
                    "docs/staging.tmp",
                    "HTTP/1.1 201 Created\r\nContent-Length: 0\r\nConnection: close\r\n\r\n",
                ),
                (
                    "PUT",
                    "docs/staging.tmp",
                    "HTTP/1.1 412 Precondition Failed\r\nContent-Length: 0\r\nConnection: close\r\n\r\n",
                ),
                (
                    "COPY",
                    "docs/new.txt",
                    "HTTP/1.1 201 Created\r\nContent-Length: 0\r\nConnection: close\r\n\r\n",
                ),
                (
                    "MOVE",
                    "docs/staging.tmp",
                    "HTTP/1.1 201 Created\r\nContent-Length: 0\r\nConnection: close\r\n\r\n",
                ),
                (
                    "MOVE",
                    "docs/collision.tmp",
                    "HTTP/1.1 412 Precondition Failed\r\nContent-Length: 0\r\nConnection: close\r\n\r\n",
                ),
                (
                    "MOVE",
                    "docs/unknown.tmp",
                    "HTTP/1.1 500 Internal Server Error\r\nContent-Length: 0\r\nConnection: close\r\n\r\n",
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
                if matches!(method, "COPY" | "MOVE") {
                    let request = request.to_ascii_lowercase();
                    let target = match (method, path) {
                        ("COPY", _) => "copy.txt",
                        (_, "docs/old.txt") => "new.txt",
                        (_, "docs/new.txt") => "existing.txt",
                        _ => "final.txt",
                    };
                    assert!(request.contains("\r\noverwrite: f\r\n"));
                    assert!(request.contains(&format!(
                        "\r\ndestination: https://files.test:{}/dav/docs/{target}\r\n",
                        addr.port()
                    )));
                    if method == "COPY" {
                        assert!(request.contains("\r\ndepth: 0\r\n"));
                    }
                }
                if method == "PUT" {
                    let request = request.to_ascii_lowercase();
                    assert!(request.contains("\r\nif-none-match: *\r\n"));
                    assert!(request.contains("\r\ntransfer-encoding: chunked\r\n"));
                    let mut body = [0; 14];
                    tls.read_exact(&mut body).await.unwrap();
                    assert_eq!(&body, b"4\r\ndemo\r\n0\r\n\r\n");
                }
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
            adapter
                .create_directory("docs", authorization())
                .await
                .unwrap();
            adapter
                .rename("docs/old.txt", "docs/new.txt", authorization())
                .await
                .unwrap();
            assert!(matches!(
                adapter
                    .rename("docs/new.txt", "elsewhere/new.txt", authorization())
                    .await,
                Err(WebDavError::InvalidPath)
            ));
            assert!(matches!(
                adapter
                    .rename("docs/new.txt", "docs/existing.txt", authorization())
                    .await,
                Err(WebDavError::Status(StatusCode::PRECONDITION_FAILED))
            ));
            assert!(matches!(
                adapter
                    .upload_new_staging_file(
                        "docs/too-large.tmp",
                        authorization(),
                        &b"large"[..],
                        Some(5),
                        Some(4)
                    )
                    .await,
                Err(WebDavError::ByteLimit)
            ));
            adapter
                .upload_new_staging_file(
                    "docs/staging.tmp",
                    authorization(),
                    &b"demo"[..],
                    None,
                    None,
                )
                .await
                .unwrap();
            assert!(matches!(
                adapter
                    .upload_new_staging_file(
                        "docs/staging.tmp",
                        authorization(),
                        &b"demo"[..],
                        Some(4),
                        Some(4)
                    )
                    .await,
                Err(WebDavError::Status(StatusCode::PRECONDITION_FAILED))
            ));
            adapter
                .copy_file_on_server("docs/new.txt", "docs/copy.txt", authorization())
                .await
                .unwrap();
            for (stage, expected) in [
                ("docs/staging.tmp", PublishResult::Confirmed),
                ("docs/collision.tmp", PublishResult::Rejected),
                ("docs/unknown.tmp", PublishResult::Unknown),
            ] {
                assert_eq!(
                    adapter
                        .publish_staging(stage, "docs/final.txt", authorization())
                        .await,
                    expected
                );
            }
            server.await.unwrap();
        })
        .await
        .unwrap();
    }

    #[tokio::test]
    async fn upload_reads_fixed_blocks_and_checks_optional_limit() {
        use futures_util::{StreamExt, pin_mut};
        let data = vec![7_u8; 256 * 1024];
        let progress = Arc::new(UploadProgress::default());
        let stream = upload_stream(data.as_slice(), None, None, Arc::clone(&progress));
        pin_mut!(stream);
        let mut count = 0;
        while let Some(chunk) = stream.next().await {
            let chunk = chunk.unwrap();
            assert!(chunk.len() <= 64 * 1024);
            count += chunk.len();
        }
        assert_eq!(count, data.len());
        assert!(progress.finished.load(Ordering::Acquire));

        let progress = Arc::new(UploadProgress::default());
        let stream = upload_stream(&b"large"[..], None, Some(4), Arc::clone(&progress));
        pin_mut!(stream);
        assert!(stream.next().await.unwrap().is_err());
        assert!(progress.limit_reached.load(Ordering::Acquire));
        assert!(!progress.finished.load(Ordering::Acquire));
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
