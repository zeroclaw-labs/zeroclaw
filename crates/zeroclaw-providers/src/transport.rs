//! Shared HTTP transport construction for capability adapters.
//!
//! Protocol modules should describe request and response wire formats.  They
//! receive this already-resolved transport instead of rediscovering provider
//! URLs, headers, credentials, and timeout policy from a provider name.

use std::collections::BTreeMap;
use std::num::NonZeroU64;
use std::time::Duration;

use anyhow::{Context, Result};
use reqwest::header::{HeaderMap, HeaderName, HeaderValue};
use reqwest::{Client, RequestBuilder};

use zeroclaw_config::provider_registry::{Override, ProviderConnection};

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
        let mut builder = zeroclaw_config::schema::apply_runtime_proxy_to_builder(
            Client::builder().redirect(reqwest::redirect::Policy::none()),
            service_key,
        );
        match &self.request_timeout_secs {
            Some(Override::Set(seconds)) => {
                builder = builder.timeout(Duration::from_secs(seconds.get()));
            }
            Some(Override::Clear) => {}
            None => {
                builder = builder.timeout(default_timeout);
            }
        }
        builder = builder.default_headers(self.headers()?);
        if let Some(path) = &self.tls_ca_cert_path {
            let pem = std::fs::read(path).context("reading provider TLS CA certificate")?;
            let certificate = reqwest::Certificate::from_pem(&pem)
                .context("decoding provider TLS CA certificate")?;
            builder = builder.add_root_certificate(certificate);
        }
        builder
            .build()
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
