//! Shared HTTP transport construction for capability adapters.
//!
//! Protocol modules should describe request and response wire formats.  They
//! receive this already-resolved transport instead of rediscovering provider
//! URLs, headers, credentials, and timeout policy from a provider name.

use std::collections::BTreeMap;
use std::hash::{Hash, Hasher};
use std::num::NonZeroU64;
use std::time::Duration;

use anyhow::{Context, Result};
use reqwest::header::{HeaderMap, HeaderName, HeaderValue};
use reqwest::{Client, RequestBuilder};

use zeroclaw_config::provider_registry::{Override, ProviderConnection};

/// Native chat transport policy. The live proxy authority owns cached clients;
/// this value only describes the builder settings used to select a pool.
#[derive(Clone, Copy, Debug, Default)]
pub(crate) struct ChatHttpTimeouts {
    request: Option<Duration>,
    read: Option<Duration>,
}

impl ChatHttpTimeouts {
    pub(crate) fn from_options(options: &crate::ModelProviderRuntimeOptions) -> Self {
        Self {
            request: (!options.provider_timeout_disabled)
                .then(|| Duration::from_secs(options.provider_timeout_secs.unwrap_or(120))),
            read: (!options.provider_read_timeout_disabled)
                .then(|| Duration::from_secs(options.provider_read_timeout_secs.unwrap_or(300))),
        }
    }

    pub(crate) fn client(self, service: &str) -> Result<Client> {
        let profile = format!("chat-timeouts:{:?}:{:?}", self.request, self.read);
        zeroclaw_config::schema::try_build_runtime_proxy_client_with(service, &profile, |builder| {
            let mut builder = builder.connect_timeout(Duration::from_secs(10));
            if let Some(timeout) = self.request {
                builder = builder.timeout(timeout);
            }
            if let Some(timeout) = self.read {
                builder = builder.read_timeout(timeout);
            }
            Ok(builder)
        })
    }
}

#[derive(Default)]
pub(crate) struct ChatHttpPolicy<'a> {
    pub request_timeout: Option<Duration>,
    pub read_timeout: Option<Duration>,
    pub headers: HeaderMap,
    pub redirect_endpoint: Option<&'a str>,
    /// Compatible adapters historically ignore malformed custom PEM and retain
    /// system roots. Keep that policy while including the bytes in pool identity.
    pub tls_ca_cert_pem: Option<&'a [u8]>,
}

impl ChatHttpPolicy<'_> {
    pub fn client(self, service_key: &str) -> Result<Client> {
        let mut fingerprint = std::collections::hash_map::DefaultHasher::new();
        self.request_timeout.hash(&mut fingerprint);
        self.read_timeout.hash(&mut fingerprint);
        self.redirect_endpoint.hash(&mut fingerprint);
        self.tls_ca_cert_pem.hash(&mut fingerprint);
        // Header insertion order is not part of the HTTP policy. Canonicalize
        // the pairs so reloading equivalent maps selects the existing pool.
        let mut headers: Vec<_> = self
            .headers
            .iter()
            .map(|(name, value)| (name.as_str(), value.as_bytes()))
            .collect();
        headers.sort_unstable();
        headers.hash(&mut fingerprint);
        let profile = format!("chat-http:{:016x}", fingerprint.finish());
        zeroclaw_config::schema::try_build_runtime_proxy_client_with(
            service_key,
            &profile,
            |builder| {
                let mut builder = builder
                    .connect_timeout(Duration::from_secs(10))
                    .default_headers(self.headers);
                if let Some(timeout) = self.request_timeout {
                    builder = builder.timeout(timeout);
                }
                if let Some(timeout) = self.read_timeout {
                    builder = builder.read_timeout(timeout);
                }
                if let Some(endpoint) = self.redirect_endpoint {
                    builder = crate::opencode_session::restrict_redirects(builder, endpoint);
                }
                if let Some(pem) = self.tls_ca_cert_pem {
                    match reqwest::Certificate::from_pem(pem) {
                        Ok(cert) => builder = builder.add_root_certificate(cert),
                        Err(error) => ::zeroclaw_log::record!(
                            WARN,
                            ::zeroclaw_log::Event::new(
                                module_path!(),
                                ::zeroclaw_log::Action::Note
                            )
                            .with_outcome(::zeroclaw_log::EventOutcome::Unknown)
                            .with_attrs(::serde_json::json!({"error": error.to_string()})),
                            "Failed to parse CA certificate — TLS will use system roots"
                        ),
                    }
                }
                Ok(builder)
            },
        )
    }
}

#[derive(Clone)]
pub(crate) struct HttpTransportConfig {
    pub(crate) base_url: String,
    pub(crate) api_key: Option<String>,
    pub(crate) extra_headers: BTreeMap<String, String>,
    pub(crate) request_timeout_secs: Option<Override<NonZeroU64>>,
    pub(crate) tls_ca_cert_path: Option<String>,
    /// Existing capability adapters follow provider redirects by default.
    /// Native protocols can opt out when forwarding credentials to a new
    /// origin would violate their protocol boundary.
    pub(crate) follow_redirects: bool,
}

impl std::fmt::Debug for HttpTransportConfig {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        f.debug_struct("HttpTransportConfig")
            .finish_non_exhaustive()
    }
}

impl HttpTransportConfig {
    pub fn from_connection(
        connection: &ProviderConnection,
        request_timeout_secs: Option<Override<NonZeroU64>>,
    ) -> Result<Self> {
        Ok(Self {
            base_url: crate::vendors::base_url(connection)?,
            api_key: connection.api_key().map(ToOwned::to_owned),
            extra_headers: connection.extra_headers().cloned().unwrap_or_default(),
            request_timeout_secs,
            tls_ca_cert_path: connection.tls_ca_cert_path().map(ToOwned::to_owned),
            follow_redirects: true,
        })
    }

    pub fn with_base_url(base_url: impl Into<String>, api_key: Option<String>) -> Self {
        Self {
            base_url: base_url.into(),
            api_key,
            extra_headers: BTreeMap::new(),
            request_timeout_secs: None,
            tls_ca_cert_path: None,
            follow_redirects: true,
        }
    }

    pub(crate) fn without_redirects(mut self) -> Self {
        self.follow_redirects = false;
        self
    }

    pub fn client(&self, default_timeout: Duration, service_key: &str) -> Result<Client> {
        // The shared cache owns proxy-generation invalidation. The profile
        // fingerprint only identifies this adapter's resolved transport policy.
        let timeout = match &self.request_timeout_secs {
            Some(Override::Set(seconds)) => Some(Duration::from_secs(seconds.get())),
            Some(Override::Clear) => None,
            None => Some(default_timeout),
        };
        let certificate = self
            .tls_ca_cert_path
            .as_ref()
            .map(|path| std::fs::read(path).context("reading provider TLS CA certificate"))
            .transpose()?;
        let mut fingerprint = std::collections::hash_map::DefaultHasher::new();
        timeout.hash(&mut fingerprint);
        self.extra_headers.hash(&mut fingerprint);
        certificate.hash(&mut fingerprint);
        self.follow_redirects.hash(&mut fingerprint);
        let profile = format!("provider-http:{:016x}", fingerprint.finish());
        zeroclaw_config::schema::try_build_runtime_proxy_client_with(
            service_key,
            &profile,
            |builder| {
                let mut builder = builder.default_headers(self.headers()?);
                if !self.follow_redirects {
                    builder = builder.redirect(reqwest::redirect::Policy::none());
                }
                if let Some(timeout) = timeout {
                    builder = builder.timeout(timeout);
                }
                if let Some(pem) = certificate {
                    builder = builder.add_root_certificate(
                        reqwest::Certificate::from_pem(&pem)
                            .context("decoding provider TLS CA certificate")?,
                    );
                }
                Ok(builder)
            },
        )
        .with_context(|| format!("building HTTP client for {service_key}"))
    }

    pub fn headers(&self) -> Result<HeaderMap> {
        let mut headers = HeaderMap::new();
        for (name, value) in &self.extra_headers {
            let name = HeaderName::from_bytes(name.as_bytes())
                .with_context(|| format!("invalid provider header name `{name}`"))?;
            let value = HeaderValue::from_str(value)
                .with_context(|| format!("invalid provider header `{name}` value"))?;
            headers.insert(name, value);
        }
        Ok(headers)
    }

    pub fn bearer(&self, request: RequestBuilder) -> RequestBuilder {
        match self.api_key.as_deref().filter(|key| !key.is_empty()) {
            Some(key) => request.bearer_auth(key),
            None => request,
        }
    }

    pub fn endpoint(&self, suffix: &str) -> Result<String> {
        append_endpoint(&self.base_url, suffix)
    }
}

pub(crate) fn append_endpoint(base_url: &str, suffix: &str) -> Result<String> {
    let mut url = reqwest::Url::parse(base_url).context("invalid provider base URL")?;
    anyhow::ensure!(
        matches!(url.scheme(), "http" | "https"),
        "provider URL must use HTTP or HTTPS"
    );
    let base = url.path().trim_end_matches('/');
    let suffix = suffix.trim_start_matches('/');
    let suffix_with_slash = format!("/{suffix}");
    let path = if base.ends_with(&suffix_with_slash) {
        return Ok(url.to_string());
    } else if base.is_empty() {
        format!("/v1/{suffix}")
    } else {
        format!("{base}/{suffix}")
    };
    url.set_path(&path);
    Ok(url.into())
}

#[cfg(test)]
mod tests {
    use super::{ChatHttpPolicy, HttpTransportConfig};
    use std::{net::SocketAddr, time::Duration};

    #[tokio::test]
    async fn native_chat_reuses_connections_and_respects_policy_and_proxy_changes() {
        use axum::{Router, extract::ConnectInfo, routing::get};

        let _proxy_guard = crate::RuntimeProxyTestGuard::acquire().await;
        let listener = tokio::net::TcpListener::bind("127.0.0.1:0").await.unwrap();
        let url = format!("http://{}/", listener.local_addr().unwrap());
        let app = Router::new().route(
            "/",
            get(|ConnectInfo(address): ConnectInfo<SocketAddr>| async move { address.to_string() }),
        );
        let server = zeroclaw_spawn::spawn!(async move {
            axum::serve(
                listener,
                app.into_make_service_with_connect_info::<SocketAddr>(),
            )
            .await
            .unwrap();
        });
        let _server_guard = scopeguard::guard(server, |server| server.abort());

        let build = |seconds| {
            ChatHttpPolicy {
                request_timeout: Some(Duration::from_secs(seconds)),
                ..Default::default()
            }
            .client("model_provider.openai")
            .unwrap()
        };
        let peer = async |client: &reqwest::Client| {
            client.get(&url).send().await.unwrap().text().await.unwrap()
        };
        let first = build(60);
        let first_peer = peer(&first).await;
        let second = build(60);
        assert_eq!(
            first_peer,
            peer(&second).await,
            "same policy reuses its TCP connection"
        );
        let changed = build(61);
        assert_ne!(
            first_peer,
            peer(&changed).await,
            "different timeouts select a different pool"
        );
        let header_client = |reversed| {
            let mut pairs = vec![("x-first", "one"), ("x-second", "two")];
            if reversed {
                pairs.reverse();
            }
            let mut headers = reqwest::header::HeaderMap::new();
            for (name, value) in pairs {
                headers.insert(name, value.parse().unwrap());
            }
            ChatHttpPolicy {
                headers,
                ..Default::default()
            }
            .client("model_provider.openai")
            .unwrap()
        };
        assert_eq!(
            peer(&header_client(false)).await,
            peer(&header_client(true)).await,
            "equivalent headers reuse the pool regardless of insertion order"
        );
        zeroclaw_config::schema::set_runtime_proxy_config(Default::default());
        let after_proxy_publication = build(60);
        assert_ne!(
            first_peer,
            peer(&after_proxy_publication).await,
            "proxy publication invalidates the old pool"
        );
    }

    #[tokio::test]
    async fn capability_redirect_policy_preserves_legacy_and_native_boundaries() {
        use axum::{Router, response::Redirect, routing::get};

        let _proxy_guard = crate::RuntimeProxyTestGuard::acquire().await;
        let listener = tokio::net::TcpListener::bind("127.0.0.1:0").await.unwrap();
        let url = format!("http://{}/redirect", listener.local_addr().unwrap());
        let app = Router::new()
            .route(
                "/redirect",
                get(|| async { Redirect::temporary("/target") }),
            )
            .route("/target", get(|| async { "reached" }));
        let server = zeroclaw_spawn::spawn!(async move {
            axum::serve(listener, app).await.unwrap();
        });
        let _server_guard = scopeguard::guard(server, |server| server.abort());
        let transport = HttpTransportConfig::with_base_url(&url, None);
        let client = transport
            .client(Duration::from_secs(10), "transcription")
            .unwrap();
        assert_eq!(
            client.get(&url).send().await.unwrap().text().await.unwrap(),
            "reached"
        );
        let restricted = transport
            .without_redirects()
            .client(Duration::from_secs(10), "transcription")
            .unwrap();
        assert_eq!(
            restricted.get(&url).send().await.unwrap().status(),
            reqwest::StatusCode::TEMPORARY_REDIRECT,
            "redirect policy must also distinguish cached clients"
        );
    }
}
