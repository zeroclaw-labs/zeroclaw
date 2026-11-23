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
        let (host, port) = validate_endpoint(&root, addresses)?;
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
        let client = builder.build().map_err(|_| WebDavError::Transport)?;
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
