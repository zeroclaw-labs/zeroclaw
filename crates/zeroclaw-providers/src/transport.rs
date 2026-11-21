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

/// Immutable transport policy materialized for one admitted chat model.
/// Clients remain owned by the shared cache so live proxy updates take effect.
#[derive(Clone, Copy, Debug)]
pub(crate) struct ChatHttpTimeouts {
    request: Option<Duration>,
    read: Option<Duration>,
}

impl Default for ChatHttpTimeouts {
    fn default() -> Self {
        Self {
            request: Some(Duration::from_secs(120)),
            read: Some(crate::STREAM_IDLE_TIMEOUT),
        }
    }
}

impl ChatHttpTimeouts {
    pub(crate) fn from_options(options: &crate::ModelProviderRuntimeOptions) -> Self {
        Self {
            request: (!options.provider_timeout_disabled)
                .then(|| Duration::from_secs(options.provider_timeout_secs.unwrap_or(120))),
            read: (!options.provider_read_timeout_disabled).then(|| {
                Duration::from_secs(
                    options
                        .provider_read_timeout_secs
                        .unwrap_or(crate::STREAM_IDLE_TIMEOUT.as_secs()),
                )
            }),
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

#[derive(Clone)]
pub(crate) struct HttpTransportConfig {
    pub(crate) base_url: String,
    pub(crate) api_key: Option<String>,
    pub(crate) extra_headers: BTreeMap<String, String>,
    pub(crate) request_timeout_secs: Option<Override<NonZeroU64>>,
    pub(crate) tls_ca_cert_path: Option<String>,
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
        })
    }

    pub fn with_base_url(base_url: impl Into<String>, api_key: Option<String>) -> Self {
        Self {
            base_url: base_url.into(),
            api_key,
            extra_headers: BTreeMap::new(),
            request_timeout_secs: None,
            tls_ca_cert_path: None,
        }
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
        let profile = format!("provider-http:{:016x}", fingerprint.finish());
        zeroclaw_config::schema::try_build_runtime_proxy_client_with(
            service_key,
            &profile,
            |builder| {
                let mut builder = builder
                    .redirect(reqwest::redirect::Policy::none())
                    .default_headers(self.headers()?);
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
    use super::*;

    #[tokio::test]
    async fn chat_transport_enforces_total_and_read_deadlines_and_honors_clear() {
        use wiremock::{Mock, MockServer, ResponseTemplate, matchers::method};
        let server = MockServer::start().await;
        Mock::given(method("GET"))
            .respond_with(ResponseTemplate::new(200).set_delay(Duration::from_millis(150)))
            .mount(&server)
            .await;
        let short = Some(Duration::from_millis(20));
        for policy in [
            ChatHttpTimeouts {
                request: short,
                read: None,
            },
            ChatHttpTimeouts {
                request: None,
                read: short,
            },
        ] {
            let error = policy
                .client("model_provider.gemini")
                .unwrap()
                .get(server.uri())
                .send()
                .await
                .unwrap_err();
            assert!(error.is_timeout(), "{error:#}");
        }
        let cleared = ChatHttpTimeouts::from_options(&crate::ModelProviderRuntimeOptions {
            provider_timeout_secs: Some(1),
            provider_timeout_disabled: true,
            provider_read_timeout_secs: Some(1),
            provider_read_timeout_disabled: true,
            ..Default::default()
        });
        assert_eq!(cleared.request, None);
        assert_eq!(cleared.read, None);
        assert_eq!(
            cleared
                .client("model_provider.gemini")
                .unwrap()
                .get(server.uri())
                .send()
                .await
                .unwrap()
                .status(),
            200
        );
    }
}
