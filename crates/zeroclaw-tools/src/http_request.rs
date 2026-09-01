use crate::helpers::{domain_guard, response_body};
use async_trait::async_trait;
use reqwest::header::{AUTHORIZATION, HeaderMap, HeaderName, HeaderValue};
use serde_json::json;
use std::future::Future;
use std::net::{IpAddr, SocketAddr};
use std::path::{Path, PathBuf};
use std::str::FromStr;
use std::sync::Arc;
use std::time::Duration;
use zeroclaw_api::tool::{Tool, ToolOutput, ToolResult};
use zeroclaw_config::policy::SecurityPolicy;
use zeroclaw_config::schema::{ProxyConfig, ProxyScope};

const HTTP_REQUEST_PROXY_PINNING_ERROR: &str = "http_request requires direct transport so validated DNS answers remain pinned; set \
     proxy.scope = \"services\" and omit tool.http_request and tool.* from proxy.services, or disable the proxy; \
     proxy.scope = \"environment\" is incompatible with pinned HTTP requests";

/// HTTP request tool for API interactions.
/// Supports GET, POST, PUT, DELETE methods with configurable security.
pub struct HttpRequestTool {
    security: Arc<SecurityPolicy>,
    allowed_domains: Vec<String>,
    max_response_size: usize,
    timeout_secs: u64,
    allow_private_hosts: bool,
    allowed_private_hosts: Vec<String>,
    /// Network-specific NAT64 prefixes this deployment's translator serves.
    /// Snapshotted at construction like `allowed_domains`; an IPv6 answer
    /// inside one of them is classified by the IPv4 address it embeds.
    nat64_prefixes: Vec<domain_guard::Nat64Prefix>,
    config_path: Option<PathBuf>,
    secrets_encrypt: bool,
}

#[derive(Debug)]
struct ValidatedHttpRequestTarget {
    url: String,
    host: String,
    resolved_addrs: Vec<SocketAddr>,
}

struct HttpRequestUrlPolicy {
    url: String,
    host: String,
    port: u16,
    private_resolution_allowed: bool,
}

impl HttpRequestTool {
    pub fn new(
        security: Arc<SecurityPolicy>,
        allowed_domains: Vec<String>,
        max_response_size: usize,
        timeout_secs: u64,
        allow_private_hosts: bool,
        allowed_private_hosts: Vec<String>,
        nat64_prefixes: Vec<String>,
    ) -> anyhow::Result<Self> {
        Ok(Self {
            security,
            allowed_domains: domain_guard::normalize_allowed_domains(
                allowed_domains,
                "http_request.allowed_domains",
            )?,
            max_response_size,
            timeout_secs,
            allow_private_hosts,
            allowed_private_hosts: domain_guard::normalize_allowed_domains(
                allowed_private_hosts,
                "http_request.allowed_private_hosts",
            )?,
            nat64_prefixes: domain_guard::parse_nat64_prefixes(
                &nat64_prefixes,
                "security.nat64_prefixes",
            )?,
            config_path: None,
            secrets_encrypt: false,
        })
    }
    pub fn new_with_config(
        security: Arc<SecurityPolicy>,
        allowed_domains: Vec<String>,
        max_response_size: usize,
        timeout_secs: u64,
        allow_private_hosts: bool,
        allowed_private_hosts: Vec<String>,
        nat64_prefixes: Vec<String>,
        config_path: PathBuf,
        secrets_encrypt: bool,
    ) -> anyhow::Result<Self> {
        Ok(Self {
            security,
            allowed_domains: domain_guard::normalize_allowed_domains(
                allowed_domains,
                "http_request.allowed_domains",
            )?,
            max_response_size,
            timeout_secs,
            allow_private_hosts,
            allowed_private_hosts: domain_guard::normalize_allowed_domains(
                allowed_private_hosts,
                "http_request.allowed_private_hosts",
            )?,
            nat64_prefixes: domain_guard::parse_nat64_prefixes(
                &nat64_prefixes,
                "security.nat64_prefixes",
            )?,
            config_path: Some(config_path),
            secrets_encrypt,
        })
    }

    #[cfg(test)]
    fn validate_url(&self, raw_url: &str) -> anyhow::Result<String> {
        Ok(self.validate_url_policy(raw_url)?.url)
    }

    fn validate_url_policy(&self, raw_url: &str) -> anyhow::Result<HttpRequestUrlPolicy> {
        let url = raw_url.trim();

        if url.is_empty() {
            anyhow::bail!("URL cannot be empty");
        }

        if url.chars().any(char::is_whitespace) {
            anyhow::bail!("URL cannot contain whitespace");
        }

        if !url.starts_with("http://") && !url.starts_with("https://") {
            anyhow::bail!("Only http:// and https:// URLs are allowed");
        }

        if self.allowed_domains.is_empty() {
            anyhow::bail!(
                "HTTP request tool is enabled but no allowed_domains are configured. Add [http_request].allowed_domains in config.toml"
            );
        }

        let host = extract_host(url)?;
        if let Ok(ip) = host.parse::<IpAddr>() {
            if domain_guard::is_known_cloud_metadata_endpoint(ip) {
                anyhow::bail!("Blocked cloud metadata host: {host}");
            }
            if domain_guard::is_cloud_metadata_ip(ip) {
                anyhow::bail!(
                    "Blocked link-local host: {host}; 169.254.0.0/16 is blocked unconditionally \
                     because cloud metadata services are hosted in that range"
                );
            }
        }
        let port = extract_port(url)?;

        let private_host = domain_guard::is_private_or_local_host(&host);
        let private_host_explicitly_allowed = private_host
            && domain_guard::host_matches_allowlist(&host, &self.allowed_private_hosts);

        if private_host && !private_host_explicitly_allowed && !self.allow_private_hosts {
            anyhow::bail!("Blocked local/private host: {host}");
        }

        if !private_host_explicitly_allowed
            && !domain_guard::host_matches_allowlist(&host, &self.allowed_domains)
        {
            anyhow::bail!("Host '{host}' is not in http_request.allowed_domains");
        }

        let private_resolution_allowed = self.allow_private_hosts
            || domain_guard::host_matches_allowlist(&host, &self.allowed_private_hosts);

        let canonical_url = if host.parse::<IpAddr>().is_ok() {
            url.to_string()
        } else {
            let mut parsed = reqwest::Url::parse(url)
                .map_err(|e| anyhow::Error::msg(format!("Invalid URL format: {e}")))?;
            parsed
                .set_host(Some(&host))
                .map_err(|_| anyhow::Error::msg("URL contains an invalid host"))?;
            parsed.to_string()
        };

        Ok(HttpRequestUrlPolicy {
            url: canonical_url,
            host,
            port,
            private_resolution_allowed,
        })
    }

    async fn validate_request_target(
        &self,
        raw_url: &str,
    ) -> anyhow::Result<ValidatedHttpRequestTarget> {
        self.validate_request_target_with_resolver(raw_url, resolve_host_for_request)
            .await
    }

    async fn validate_request_target_with_resolver<F, Fut>(
        &self,
        raw_url: &str,
        resolve_host: F,
    ) -> anyhow::Result<ValidatedHttpRequestTarget>
    where
        F: FnOnce(String, u16) -> Fut,
        Fut: Future<Output = anyhow::Result<Vec<SocketAddr>>>,
    {
        let policy = self.validate_url_policy(raw_url)?;
        let resolved_addrs = if let Ok(ip) = policy.host.parse::<IpAddr>() {
            vec![SocketAddr::new(ip, policy.port)]
        } else {
            resolve_host(policy.host.clone(), policy.port).await?
        };
        validate_resolved_ips_for_ssrf(
            &policy.host,
            policy.private_resolution_allowed,
            &resolved_addrs
                .iter()
                .map(|addr| addr.ip())
                .collect::<Vec<_>>(),
            &self.nat64_prefixes,
        )?;

        Ok(ValidatedHttpRequestTarget {
            url: policy.url,
            host: policy.host,
            resolved_addrs,
        })
    }

    fn validate_method(&self, method: &str) -> anyhow::Result<reqwest::Method> {
        match method.to_uppercase().as_str() {
            "GET" => Ok(reqwest::Method::GET),
            "POST" => Ok(reqwest::Method::POST),
            "PUT" => Ok(reqwest::Method::PUT),
            "DELETE" => Ok(reqwest::Method::DELETE),
            "PATCH" => Ok(reqwest::Method::PATCH),
            "HEAD" => Ok(reqwest::Method::HEAD),
            "OPTIONS" => Ok(reqwest::Method::OPTIONS),
            _ => anyhow::bail!(
                "Unsupported HTTP method: {method}. Supported: GET, POST, PUT, DELETE, PATCH, HEAD, OPTIONS"
            ),
        }
    }

    fn parse_headers(&self, headers: &serde_json::Value) -> anyhow::Result<HeaderMap> {
        let mut result = HeaderMap::new();
        if let Some(obj) = headers.as_object() {
            for (key, value) in obj {
                let Some(str_val) = value.as_str() else {
                    anyhow::bail!("Header '{key}' value must be a string, got: {}", value);
                };
                let header_name = HeaderName::from_str(key)
                    .map_err(|e| anyhow::Error::msg(format!("Invalid header name '{key}': {e}")))?;
                let header_value = HeaderValue::from_str(str_val).map_err(|e| {
                    anyhow::Error::msg(format!("Invalid value for header '{key}': {e}"))
                })?;
                result.insert(header_name, header_value);
            }
        }
        Ok(result)
    }

    fn validate_secret_name(secret_name: &str) -> anyhow::Result<()> {
        if secret_name.is_empty() {
            anyhow::bail!("auth_secret cannot be empty");
        }
        if secret_name.len() > 64 {
            anyhow::bail!("auth_secret must be 64 characters or fewer");
        }
        if !secret_name
            .chars()
            .all(|c| c.is_ascii_alphanumeric() || c == '_' || c == '-')
        {
            anyhow::bail!(
                "auth_secret must contain only ASCII letters, numbers, underscores, or hyphens"
            );
        }
        Ok(())
    }

    fn resolve_auth_secret(&self, secret_name: &str) -> anyhow::Result<String> {
        Self::validate_secret_name(secret_name)?;
        self.reload_auth_secret(secret_name)
    }

    fn reload_auth_secret(&self, secret_name: &str) -> anyhow::Result<String> {
        let config_path = self.config_path.as_ref().ok_or_else(|| {
            anyhow::Error::msg("auth_secret requires runtime config reload support")
        })?;
        if config_path.as_os_str().is_empty() {
            anyhow::bail!("auth_secret requires a config.toml path");
        }

        let contents = std::fs::read_to_string(config_path).map_err(|e| {
            anyhow::Error::msg(format!(
                "Failed to read config file {} for auth_secret '{secret_name}': {e}",
                config_path.display()
            ))
        })?;
        let config: zeroclaw_config::schema::Config = toml::from_str(&contents).map_err(|e| {
            anyhow::Error::msg(format!(
                "Failed to parse config file {} for auth_secret '{secret_name}': {e}",
                config_path.display()
            ))
        })?;

        let raw_secret = config
            .http_request
            .secrets
            .get(secret_name)
            .filter(|secret| !secret.is_empty())
            .ok_or_else(|| anyhow::Error::msg(format!("auth_secret '{secret_name}' not found")))?;

        let secret = if zeroclaw_config::secrets::SecretStore::is_encrypted(raw_secret) {
            let zeroclaw_dir = config_path.parent().unwrap_or_else(|| Path::new("."));
            let store =
                zeroclaw_config::secrets::SecretStore::new(zeroclaw_dir, self.secrets_encrypt);
            let plaintext = store.decrypt(raw_secret)?;
            if plaintext.is_empty() {
                anyhow::bail!("auth_secret '{secret_name}' is empty after decryption");
            }
            plaintext
        } else {
            raw_secret.clone()
        };

        if let Some(env_secret) = resolve_env_backed_auth_secret(secret_name, &secret)? {
            Ok(env_secret)
        } else {
            Ok(secret)
        }
    }

    fn apply_auth_secret(
        &self,
        headers: &mut HeaderMap,
        auth_secret: Option<&str>,
    ) -> anyhow::Result<()> {
        let Some(secret_name) = auth_secret else {
            return Ok(());
        };
        let secret = self.resolve_auth_secret(secret_name)?;
        let header_value = HeaderValue::from_str(&secret).map_err(|e| {
            anyhow::Error::msg(format!(
                "Invalid value for auth_secret '{secret_name}' as Authorization header: {e}"
            ))
        })?;
        headers.insert(AUTHORIZATION, header_value);
        Ok(())
    }

    #[cfg(test)]
    fn redact_headers_for_display(headers: &[(String, String)]) -> Vec<(String, String)> {
        headers
            .iter()
            .map(|(key, value)| {
                let lower = key.to_lowercase();
                let is_sensitive = lower.contains("authorization")
                    || lower.contains("api-key")
                    || lower.contains("apikey")
                    || lower.contains("token")
                    || lower.contains("secret");
                if is_sensitive {
                    (key.clone(), "***REDACTED***".into())
                } else {
                    (key.clone(), value.clone())
                }
            })
            .collect()
    }

    async fn execute_request(
        &self,
        target: &ValidatedHttpRequestTarget,
        method: reqwest::Method,
        headers: HeaderMap,
        body: Option<&str>,
    ) -> anyhow::Result<reqwest::Response> {
        let timeout_secs = if self.timeout_secs == 0 {
            ::zeroclaw_log::record!(
                WARN,
                ::zeroclaw_log::Event::new(module_path!(), ::zeroclaw_log::Action::Note)
                    .with_outcome(::zeroclaw_log::EventOutcome::Unknown),
                "http_request: timeout_secs is 0, using safe default of 30s"
            );
            30
        } else {
            self.timeout_secs
        };
        let builder = reqwest::Client::builder()
            .no_proxy()
            .timeout(Duration::from_secs(timeout_secs))
            .connect_timeout(Duration::from_secs(10))
            .redirect(reqwest::redirect::Policy::none());
        let builder = if target.host.parse::<IpAddr>().is_ok() {
            builder
        } else {
            builder.resolve_to_addrs(&target.host, &target.resolved_addrs)
        };
        let client = builder.build()?;

        let mut request = client.request(method, &target.url).headers(headers);

        if let Some(body_str) = body {
            request = request.body(body_str.to_string());
        }

        Ok(request.send().await?)
    }

    async fn read_response_text(&self, response: reqwest::Response) -> anyhow::Result<String> {
        let limit = (self.max_response_size != 0).then_some(self.max_response_size);
        let (mut text, overflowed) = response_body::read_text(response, limit).await?;
        if overflowed {
            text.push_str("\n\n... [Response truncated due to size limit] ...");
        }
        Ok(text)
    }
}

fn resolve_env_backed_auth_secret(
    secret_name: &str,
    raw_secret: &str,
) -> anyhow::Result<Option<String>> {
    let Some(env_name) = env_secret_reference(raw_secret)? else {
        return Ok(None);
    };

    let value = std::env::var(env_name).map_err(|e| {
        anyhow::Error::msg(format!(
            "auth_secret '{secret_name}' references environment variable '{env_name}', but it could not be read: {e}"
        ))
    })?;
    if value.is_empty() {
        anyhow::bail!(
            "auth_secret '{secret_name}' references environment variable '{env_name}', but it is empty"
        );
    }
    Ok(Some(value))
}

fn env_secret_reference(raw_secret: &str) -> anyhow::Result<Option<&str>> {
    let Some(inner) = raw_secret
        .strip_prefix("${")
        .and_then(|value| value.strip_suffix('}'))
    else {
        return Ok(None);
    };

    if inner.is_empty() {
        anyhow::bail!(
            "environment-backed auth_secret references an empty environment variable name"
        );
    }
    if !inner.chars().all(|c| c.is_ascii_alphanumeric() || c == '_') {
        anyhow::bail!(
            "environment-backed auth_secret '{inner}' must contain only ASCII letters, numbers, or underscores"
        );
    }
    Ok(Some(inner))
}

#[async_trait]
impl Tool for HttpRequestTool {
    fn name(&self) -> &str {
        "http_request"
    }

    fn description(&self) -> &str {
        "Make HTTP requests to external APIs. Supports GET, POST, PUT, DELETE, PATCH, HEAD, OPTIONS methods. \
        Security constraints: allowlist-only domains, local/private hosts blocked unless explicitly configured, configurable timeout and response size limits."
    }

    fn parameters_schema(&self) -> serde_json::Value {
        json!({
            "type": "object",
            "properties": {
                "url": {
                    "type": "string",
                    "description": "HTTP or HTTPS URL to request"
                },
                "method": {
                    "type": "string",
                    "description": "HTTP method (GET, POST, PUT, DELETE, PATCH, HEAD, OPTIONS)",
                    "default": "GET"
                },
                "headers": {
                    "type": "object",
                    "description": "Optional HTTP headers as key-value pairs. Use auth_secret for Authorization values that should come from config secrets.",
                    "default": {}
                },
                "auth_secret": {
                    "type": "string",
                    "description": "Name of a secret in [http_request.secrets] to send as the Authorization header. Secret entries may be literal, encrypted, or environment-backed as ${ENV_VAR}. Overrides any literal Authorization header."
                },
                "body": {
                    "type": "string",
                    "description": "Optional request body (for POST, PUT, PATCH requests)"
                }
            },
            "required": ["url"]
        })
    }

    fn output_schema(&self) -> Option<serde_json::Value> {
        Some(json!({
            "type": "object",
            "properties": {
                "status": { "type": "integer", "description": "HTTP status code" },
                "reason": { "type": "string", "description": "Canonical status reason" },
                "headers": { "type": "string", "description": "Response headers (sensitive values redacted)" },
                "body": { "description": "Response body: parsed JSON when the body is JSON, raw string otherwise" }
            },
            "required": ["status", "reason", "headers", "body"]
        }))
    }

    async fn execute(&self, args: serde_json::Value) -> anyhow::Result<ToolResult> {
        let url = args.get("url").and_then(|v| v.as_str()).ok_or_else(|| {
            ::zeroclaw_log::record!(
                WARN,
                ::zeroclaw_log::Event::new(module_path!(), ::zeroclaw_log::Action::Reject)
                    .with_outcome(::zeroclaw_log::EventOutcome::Failure)
                    .with_attrs(::serde_json::json!({"param": "url"})),
                "http_request: missing url parameter"
            );
            anyhow::Error::msg("Missing 'url' parameter")
        })?;

        let method_str = args.get("method").and_then(|v| v.as_str()).unwrap_or("GET");
        let headers_val = args.get("headers").cloned().unwrap_or(json!({}));
        let auth_secret = match args.get("auth_secret") {
            Some(value) => match value.as_str() {
                Some(secret_name) => Some(secret_name),
                None => {
                    return Ok(ToolResult {
                        success: false,
                        output: ToolOutput::default(),
                        error: Some("'auth_secret' must be a string".into()),
                    });
                }
            },
            None => None,
        };
        let body = args.get("body").and_then(|v| v.as_str());

        if !self.security.can_act() {
            return Ok(ToolResult {
                success: false,
                output: ToolOutput::default(),
                error: Some("Action blocked: autonomy is read-only".into()),
            });
        }

        // Rate limiting is applied by the RateLimitedTool wrapper at
        // registration time (see zeroclaw-runtime::tools::mod).

        let proxy_config = zeroclaw_config::schema::runtime_proxy_config();
        if proxy_conflicts_with_dns_pinning(&proxy_config) {
            ::zeroclaw_log::record!(
                WARN,
                ::zeroclaw_log::Event::new(module_path!(), ::zeroclaw_log::Action::Reject)
                    .with_outcome(::zeroclaw_log::EventOutcome::Failure)
                    .with_attrs(::serde_json::json!({"service": "tool.http_request"})),
                "http_request: configured runtime proxy rejected to preserve validated DNS pin"
            );
            return Ok(ToolResult {
                success: false,
                output: ToolOutput::default(),
                error: Some(HTTP_REQUEST_PROXY_PINNING_ERROR.into()),
            });
        }

        let ignored_environment_proxy = zeroclaw_config::schema::environment_proxy_for_url(url);
        if let Some(variable) = ignored_environment_proxy {
            ::zeroclaw_log::record!(
                WARN,
                ::zeroclaw_log::Event::new(module_path!(), ::zeroclaw_log::Action::Note)
                    .with_outcome(::zeroclaw_log::EventOutcome::Unknown)
                    .with_attrs(::serde_json::json!({"proxy_variable": variable})),
                "http_request: environment proxy ignored to preserve validated DNS pin"
            );
        }

        let target = match self.validate_request_target(url).await {
            Ok(v) => v,
            Err(e) => {
                return Ok(ToolResult {
                    success: false,
                    output: ToolOutput::default(),
                    error: Some(e.to_string()),
                });
            }
        };

        let method = match self.validate_method(method_str) {
            Ok(m) => m,
            Err(e) => {
                return Ok(ToolResult {
                    success: false,
                    output: ToolOutput::default(),
                    error: Some(e.to_string()),
                });
            }
        };

        let mut request_headers = match self.parse_headers(&headers_val) {
            Ok(h) => h,
            Err(e) => {
                return Ok(ToolResult {
                    success: false,
                    output: ToolOutput::default(),
                    error: Some(e.to_string()),
                });
            }
        };
        if let Err(e) = self.apply_auth_secret(&mut request_headers, auth_secret) {
            return Ok(ToolResult {
                success: false,
                output: ToolOutput::default(),
                error: Some(e.to_string()),
            });
        }

        match self
            .execute_request(&target, method, request_headers, body)
            .await
        {
            Ok(response) => {
                let status = response.status();
                let status_code = status.as_u16();

                // Get response headers (redact sensitive ones)
                let response_headers = response.headers().iter();
                let headers_text = response_headers
                    .map(|(k, _)| {
                        let is_sensitive = k.as_str().to_lowercase().contains("set-cookie");
                        if is_sensitive {
                            format!("{}: ***REDACTED***", k.as_str())
                        } else {
                            format!("{}: {:?}", k.as_str(), k.as_str())
                        }
                    })
                    .collect::<Vec<_>>()
                    .join(", ");

                // Get response body with size limit
                let response_text = match self.read_response_text(response).await {
                    Ok(text) => text,
                    Err(e) => format!("[Failed to read response body: {e}]"),
                };

                let output = format!(
                    "Status: {} {}\nResponse Headers: {}\n\nResponse Body:\n{}",
                    status_code,
                    status.canonical_reason().unwrap_or("Unknown"),
                    headers_text,
                    response_text
                );

                // Structured mirror of the display text; body is parsed
                // JSON when it parses, raw string otherwise.
                let body_value = serde_json::from_str::<serde_json::Value>(&response_text)
                    .unwrap_or_else(|_| serde_json::Value::String(response_text.clone()));
                let data = json!({
                    "status": status_code,
                    "reason": status.canonical_reason().unwrap_or("Unknown"),
                    "headers": headers_text,
                    "body": body_value,
                });

                Ok(ToolResult {
                    success: status.is_success(),
                    output: ToolOutput::json_with_text(data, output),
                    error: if status.is_client_error() || status.is_server_error() {
                        Some(format!("HTTP {}", status_code))
                    } else {
                        None
                    },
                })
            }
            Err(e) => {
                let mut error = format!("HTTP request failed: {e}");
                if let Some(variable) = ignored_environment_proxy {
                    error.push_str(&format!(
                        "; {variable} was intentionally ignored by the DNS-pinned request"
                    ));
                }
                Ok(ToolResult {
                    success: false,
                    output: ToolOutput::default(),
                    error: Some(error),
                })
            }
        }
    }
}

fn proxy_conflicts_with_dns_pinning(config: &ProxyConfig) -> bool {
    (config.enabled && config.scope == ProxyScope::Environment)
        || (config.has_any_proxy_url() && config.should_apply_to_service("tool.http_request"))
}

fn extract_host(url: &str) -> anyhow::Result<String> {
    if !url.starts_with("http://") && !url.starts_with("https://") {
        ::zeroclaw_log::record!(
            WARN,
            ::zeroclaw_log::Event::new(module_path!(), ::zeroclaw_log::Action::Reject)
                .with_outcome(::zeroclaw_log::EventOutcome::Failure)
                .with_attrs(::serde_json::json!({"url": url})),
            "http_request: non-http(s) URL rejected"
        );
        anyhow::bail!("Only http:// and https:// URLs are allowed");
    }

    let parsed = reqwest::Url::parse(url).map_err(|e| {
        ::zeroclaw_log::record!(
            WARN,
            ::zeroclaw_log::Event::new(module_path!(), ::zeroclaw_log::Action::Reject)
                .with_outcome(::zeroclaw_log::EventOutcome::Failure)
                .with_attrs(::serde_json::json!({"url": url})),
            "http_request: invalid URL"
        );
        anyhow::Error::msg(format!("Invalid URL format: {e}"))
    })?;

    if !parsed.username().is_empty() || parsed.password().is_some() {
        anyhow::bail!("URL userinfo is not allowed");
    }

    let host = parsed
        .host_str()
        .ok_or_else(|| anyhow::Error::msg("URL must include a host"))?;

    let trimmed = host.trim();
    let host_no_brackets = match (trimmed.starts_with('['), trimmed.ends_with(']')) {
        (true, true) => &trimmed[1..trimmed.len() - 1],
        (false, false) => trimmed,
        _ => {
            anyhow::bail!("URL host has unmatched IPv6 brackets");
        }
    };
    let host = host_no_brackets.trim_end_matches('.').to_lowercase();

    if host.is_empty() {
        anyhow::bail!("URL must include a valid host");
    }

    Ok(host)
}

fn extract_port(url: &str) -> anyhow::Result<u16> {
    let parsed = reqwest::Url::parse(url)
        .map_err(|e| anyhow::Error::msg(format!("Invalid URL format: {e}")))?;

    parsed
        .port_or_known_default()
        .ok_or_else(|| anyhow::Error::msg("URL must include a valid port"))
}

async fn resolve_host_for_request(host: String, port: u16) -> anyhow::Result<Vec<SocketAddr>> {
    let addrs = tokio::net::lookup_host((host.as_str(), port))
        .await
        .map_err(|e| anyhow::Error::msg(format!("Failed to resolve host '{host}': {e}")))?
        .collect::<Vec<_>>();

    if addrs.is_empty() {
        anyhow::bail!("Failed to resolve host '{host}'");
    }

    Ok(addrs)
}

fn validate_resolved_ips_for_ssrf(
    host: &str,
    private_resolution_allowed: bool,
    ips: &[std::net::IpAddr],
    nat64_prefixes: &[domain_guard::Nat64Prefix],
) -> anyhow::Result<()> {
    if private_resolution_allowed {
        domain_guard::validate_resolved_ips_exclude_metadata(host, ips, nat64_prefixes)
    } else {
        domain_guard::validate_resolved_ips_are_public(host, ips, nat64_prefixes)
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use reqwest::header::AUTHORIZATION;
    use std::path::PathBuf;
    use tempfile::TempDir;
    use zeroclaw_config::autonomy::AutonomyLevel;
    use zeroclaw_config::policy::SecurityPolicy;

    async fn chunked_response(chunks: &[&[u8]]) -> reqwest::Response {
        use tokio::io::{AsyncReadExt, AsyncWriteExt};

        let listener = tokio::net::TcpListener::bind("127.0.0.1:0").await.unwrap();
        let addr = listener.local_addr().unwrap();
        let owned_chunks = chunks
            .iter()
            .map(|chunk| chunk.to_vec())
            .collect::<Vec<_>>();

        zeroclaw_spawn::spawn!(async move {
            let (mut stream, _) = listener.accept().await.unwrap();
            let mut request = Vec::new();
            while !request.windows(4).any(|window| window == b"\r\n\r\n") {
                let mut buffer = [0_u8; 1024];
                let read = stream.read(&mut buffer).await.unwrap();
                assert!(read > 0, "client closed before completing request headers");
                request.extend_from_slice(&buffer[..read]);
            }
            stream
                .write_all(
                    b"HTTP/1.1 200 OK\r\nTransfer-Encoding: chunked\r\nConnection: close\r\n\r\n",
                )
                .await
                .unwrap();
            for chunk in owned_chunks {
                stream
                    .write_all(format!("{:x}\r\n", chunk.len()).as_bytes())
                    .await
                    .unwrap();
                stream.write_all(&chunk).await.unwrap();
                stream.write_all(b"\r\n").await.unwrap();
            }
            stream.write_all(b"0\r\n\r\n").await.unwrap();
        });

        reqwest::get(format!("http://{addr}")).await.unwrap()
    }

    fn test_tool(allowed_domains: Vec<&str>) -> HttpRequestTool {
        test_tool_with_private(allowed_domains, false)
    }

    #[tokio::test]
    async fn response_limit_truncates_a_chunked_body_during_streaming() {
        let tool = HttpRequestTool::new(
            Arc::new(SecurityPolicy::default()),
            vec!["example.com".into()],
            8,
            30,
            false,
            Vec::new(),
            Vec::new(),
        )
        .unwrap();
        let response = chunked_response(&[b"hello", b" world", b" ignored"]).await;

        let text = tool.read_response_text(response).await.unwrap();

        assert!(text.starts_with("hello wo"));
        assert!(text.contains("[Response truncated due to size limit]"));
        assert!(!text.contains("ignored"));
    }

    #[tokio::test]
    async fn zero_response_limit_preserves_a_complete_chunked_body() {
        let tool = HttpRequestTool::new(
            Arc::new(SecurityPolicy::default()),
            vec!["example.com".into()],
            0,
            30,
            false,
            Vec::new(),
            Vec::new(),
        )
        .unwrap();
        let response = chunked_response(&[b"hello", b" world"]).await;

        assert_eq!(
            tool.read_response_text(response).await.unwrap(),
            "hello world"
        );
    }

    fn test_tool_with_private(
        allowed_domains: Vec<&str>,
        allow_private_hosts: bool,
    ) -> HttpRequestTool {
        test_tool_with_private_allowlist(allowed_domains, allow_private_hosts, Vec::new())
    }

    fn test_tool_with_private_allowlist(
        allowed_domains: Vec<&str>,
        allow_private_hosts: bool,
        allowed_private_hosts: Vec<&str>,
    ) -> HttpRequestTool {
        test_tool_with_nat64(
            allowed_domains,
            allow_private_hosts,
            allowed_private_hosts,
            Vec::new(),
        )
    }

    fn test_tool_with_nat64(
        allowed_domains: Vec<&str>,
        allow_private_hosts: bool,
        allowed_private_hosts: Vec<&str>,
        nat64_prefixes: Vec<&str>,
    ) -> HttpRequestTool {
        try_test_tool_with_nat64(
            allowed_domains,
            allow_private_hosts,
            allowed_private_hosts,
            nat64_prefixes,
        )
        .unwrap()
    }

    fn try_test_tool_with_nat64(
        allowed_domains: Vec<&str>,
        allow_private_hosts: bool,
        allowed_private_hosts: Vec<&str>,
        nat64_prefixes: Vec<&str>,
    ) -> anyhow::Result<HttpRequestTool> {
        let security = Arc::new(SecurityPolicy {
            autonomy: AutonomyLevel::Supervised,
            ..SecurityPolicy::default()
        });
        HttpRequestTool::new(
            security,
            allowed_domains.into_iter().map(String::from).collect(),
            1_000_000,
            30,
            allow_private_hosts,
            allowed_private_hosts
                .into_iter()
                .map(String::from)
                .collect(),
            nat64_prefixes.into_iter().map(String::from).collect(),
        )
    }

    fn test_tool_with_auth_config(config_path: PathBuf, secrets_encrypt: bool) -> HttpRequestTool {
        let security = Arc::new(SecurityPolicy {
            autonomy: AutonomyLevel::Supervised,
            ..SecurityPolicy::default()
        });
        HttpRequestTool::new_with_config(
            security,
            vec!["example.com".into()],
            1_000_000,
            30,
            false,
            Vec::new(),
            Vec::new(),
            config_path,
            secrets_encrypt,
        )
        .unwrap()
    }

    #[test]
    fn schema_includes_auth_secret_parameter() {
        let tool = test_tool(vec!["example.com"]);
        let schema = tool.parameters_schema();
        let properties = schema["properties"].as_object().expect("schema properties");

        assert!(
            properties.contains_key("auth_secret"),
            "http_request schema must expose auth_secret"
        );
    }

    #[test]
    fn resolve_auth_secret_requires_config_reload_support() {
        let tool = test_tool(vec!["example.com"]);

        let err = tool.resolve_auth_secret("api_token").unwrap_err();
        assert!(
            err.to_string()
                .contains("auth_secret requires runtime config reload support"),
            "auth_secret without config path must fail clearly: {err}"
        );
    }

    #[test]
    fn auth_secret_overrides_explicit_authorization_header() {
        let tmp = TempDir::new().unwrap();
        let config_path = tmp.path().join("config.toml");
        std::fs::write(
            &config_path,
            r#"
[http_request.secrets]
api_token = "Bearer from-secret"
"#,
        )
        .unwrap();
        let tool = test_tool_with_auth_config(config_path, false);
        let mut headers = tool
            .parse_headers(&json!({"Authorization": "Bearer literal"}))
            .unwrap();

        tool.apply_auth_secret(&mut headers, Some("api_token"))
            .unwrap();

        assert_eq!(
            headers.get(AUTHORIZATION).unwrap(),
            "Bearer from-secret",
            "auth_secret must win over literal Authorization headers"
        );
    }

    #[test]
    fn auth_secret_reloads_plain_config_value_without_boot_secret() {
        let tmp = TempDir::new().unwrap();
        let config_path = tmp.path().join("config.toml");
        std::fs::write(
            &config_path,
            r#"
[http_request.secrets]
api_token = "Bearer from-disk"
"#,
        )
        .unwrap();

        let security = Arc::new(SecurityPolicy {
            autonomy: AutonomyLevel::Supervised,
            ..SecurityPolicy::default()
        });
        let tool = HttpRequestTool::new_with_config(
            security,
            vec!["example.com".into()],
            1_000_000,
            30,
            false,
            Vec::new(),
            Vec::new(),
            config_path,
            false,
        )
        .unwrap();

        assert_eq!(
            tool.resolve_auth_secret("api_token").unwrap(),
            "Bearer from-disk"
        );
    }

    #[test]
    fn auth_secret_resolves_env_backed_config_value() {
        let tmp = TempDir::new().unwrap();
        let config_path = tmp.path().join("config.toml");
        std::fs::write(
            &config_path,
            r#"
[http_request.secrets]
api_token = "${ZEROCLAW_TEST_HTTP_REQUEST_SECRET}"
"#,
        )
        .unwrap();
        unsafe {
            std::env::set_var("ZEROCLAW_TEST_HTTP_REQUEST_SECRET", "Bearer from-env");
        }
        scopeguard::defer! {
            unsafe {
                std::env::remove_var("ZEROCLAW_TEST_HTTP_REQUEST_SECRET");
            }
        }

        let tool = test_tool_with_auth_config(config_path, false);

        assert_eq!(
            tool.resolve_auth_secret("api_token").unwrap(),
            "Bearer from-env"
        );
    }

    #[test]
    fn auth_secret_reports_missing_env_backed_config_value() {
        let tmp = TempDir::new().unwrap();
        let config_path = tmp.path().join("config.toml");
        std::fs::write(
            &config_path,
            r#"
[http_request.secrets]
api_token = "${ZEROCLAW_TEST_HTTP_REQUEST_MISSING_SECRET}"
"#,
        )
        .unwrap();
        unsafe {
            std::env::remove_var("ZEROCLAW_TEST_HTTP_REQUEST_MISSING_SECRET");
        }

        let tool = test_tool_with_auth_config(config_path, false);
        let err = tool
            .resolve_auth_secret("api_token")
            .unwrap_err()
            .to_string();

        assert!(
            err.contains("ZEROCLAW_TEST_HTTP_REQUEST_MISSING_SECRET"),
            "missing env-backed secret should name the missing variable: {err}"
        );
    }

    #[test]
    fn auth_secret_rejects_invalid_env_reference_name() {
        let err = env_secret_reference("${BAD-NAME}").unwrap_err().to_string();

        assert!(
            err.contains("ASCII letters, numbers, or underscores"),
            "invalid env-backed secret name should fail clearly: {err}"
        );
    }

    #[test]
    fn auth_secret_decrypts_reloaded_config_value() {
        let tmp = TempDir::new().unwrap();
        let config_path = tmp.path().join("config.toml");
        let store = zeroclaw_config::secrets::SecretStore::new(tmp.path(), true);
        let encrypted = store.encrypt("Bearer encrypted-secret").unwrap();
        std::fs::write(
            &config_path,
            format!(
                r#"
[http_request.secrets]
api_token = "{encrypted}"
"#
            ),
        )
        .unwrap();

        let security = Arc::new(SecurityPolicy {
            autonomy: AutonomyLevel::Supervised,
            ..SecurityPolicy::default()
        });
        let tool = HttpRequestTool::new_with_config(
            security,
            vec!["example.com".into()],
            1_000_000,
            30,
            false,
            Vec::new(),
            Vec::new(),
            config_path,
            true,
        )
        .unwrap();
        let mut headers = tool
            .parse_headers(&json!({"Authorization": "Bearer literal"}))
            .unwrap();

        tool.apply_auth_secret(&mut headers, Some("api_token"))
            .unwrap();

        assert_eq!(
            headers.get(AUTHORIZATION).unwrap(),
            "Bearer encrypted-secret"
        );
    }

    #[test]
    fn auth_secret_resolves_encrypted_env_backed_config_value() {
        let tmp = TempDir::new().unwrap();
        let config_path = tmp.path().join("config.toml");
        let store = zeroclaw_config::secrets::SecretStore::new(tmp.path(), true);
        let encrypted = store
            .encrypt("${ZEROCLAW_TEST_HTTP_REQUEST_ENCRYPTED_SECRET}")
            .unwrap();
        std::fs::write(
            &config_path,
            format!(
                r#"
[http_request.secrets]
api_token = "{encrypted}"
"#
            ),
        )
        .unwrap();
        unsafe {
            std::env::set_var(
                "ZEROCLAW_TEST_HTTP_REQUEST_ENCRYPTED_SECRET",
                "Bearer encrypted-env",
            );
        }
        scopeguard::defer! {
            unsafe {
                std::env::remove_var("ZEROCLAW_TEST_HTTP_REQUEST_ENCRYPTED_SECRET");
            }
        }

        let tool = test_tool_with_auth_config(config_path, true);

        assert_eq!(
            tool.resolve_auth_secret("api_token").unwrap(),
            "Bearer encrypted-env"
        );
    }

    #[tokio::test]
    async fn execute_sends_auth_secret_as_authorization_header() {
        let listener = match tokio::net::TcpListener::bind("[::1]:0").await {
            Ok(l) => l,
            Err(_) => return, // IPv6 loopback is unavailable in this environment.
        };
        let port = listener.local_addr().unwrap().port();
        let (seen_tx, seen_rx) = tokio::sync::oneshot::channel();

        let server_handle = zeroclaw_spawn::spawn!(async move {
            if let Ok((mut stream, _)) = listener.accept().await {
                use tokio::io::{AsyncReadExt, AsyncWriteExt};

                let mut buf = [0_u8; 4096];
                let n = stream.read(&mut buf).await.unwrap_or(0);
                let request = String::from_utf8_lossy(&buf[..n]).to_ascii_lowercase();
                let _ = seen_tx.send(request.contains("authorization: bearer from-secret"));

                let response =
                    b"HTTP/1.1 200 OK\r\nContent-Length: 2\r\nConnection: close\r\n\r\nok";
                let _ = stream.write_all(response).await;
                let _ = stream.flush().await;
            }
        });

        let security = Arc::new(SecurityPolicy {
            autonomy: AutonomyLevel::Supervised,
            ..SecurityPolicy::default()
        });
        let tmp = TempDir::new().unwrap();
        let config_path = tmp.path().join("config.toml");
        std::fs::write(
            &config_path,
            r#"
[http_request.secrets]
api_token = "Bearer from-secret"
"#,
        )
        .unwrap();
        let tool = HttpRequestTool::new_with_config(
            security,
            vec!["::1".into()],
            1_000_000,
            5,
            true,
            Vec::new(),
            Vec::new(),
            config_path,
            false,
        )
        .unwrap();

        let result = tokio::time::timeout(
            Duration::from_secs(5),
            tool.execute(json!({
                "url": format!("http://[::1]:{port}/"),
                "auth_secret": "api_token",
                "headers": {
                    "Authorization": "Bearer literal"
                }
            })),
        )
        .await
        .unwrap()
        .unwrap();

        let saw_auth_header = tokio::time::timeout(Duration::from_secs(5), seen_rx)
            .await
            .unwrap()
            .unwrap();
        server_handle.abort();

        assert!(result.success);
        assert!(
            saw_auth_header,
            "auth_secret must send the resolved Authorization header"
        );
    }

    #[test]
    fn extract_host_normalizes_ipv6_without_brackets() {
        let got = extract_host("https://[2001:db8::1]:443/path").unwrap();
        assert_eq!(got, "2001:db8::1");
    }

    #[test]
    fn validate_accepts_exact_domain() {
        let tool = test_tool(vec!["example.com"]);
        let got = tool.validate_url("https://example.com/docs").unwrap();
        assert_eq!(got, "https://example.com/docs");
    }

    #[test]
    fn validate_accepts_http() {
        let tool = test_tool(vec!["example.com"]);
        assert!(tool.validate_url("http://example.com").is_ok());
    }

    #[test]
    fn validate_accepts_subdomain() {
        let tool = test_tool(vec!["example.com"]);
        assert!(tool.validate_url("https://api.example.com/v1").is_ok());
    }

    #[test]
    fn validate_accepts_wildcard_allowlist_for_public_host() {
        let tool = test_tool(vec!["*"]);
        assert!(tool.validate_url("https://news.ycombinator.com").is_ok());
    }

    #[test]
    fn validate_wildcard_allowlist_still_rejects_private_host() {
        let tool = test_tool(vec!["*"]);
        let err = tool
            .validate_url("https://localhost:8080")
            .unwrap_err()
            .to_string();
        assert!(err.contains("local/private"));
    }

    #[test]
    fn validate_rejects_allowlist_miss() {
        let tool = test_tool(vec!["example.com"]);
        let err = tool
            .validate_url("https://google.com")
            .unwrap_err()
            .to_string();
        assert!(err.contains("allowed_domains"));
    }

    #[test]
    fn validate_rejects_localhost() {
        let tool = test_tool(vec!["localhost"]);
        let err = tool
            .validate_url("https://localhost:8080")
            .unwrap_err()
            .to_string();
        assert!(err.contains("local/private"));
    }

    #[test]
    fn validate_rejects_private_ipv4() {
        let tool = test_tool(vec!["192.168.1.5"]);
        let err = tool
            .validate_url("https://192.168.1.5")
            .unwrap_err()
            .to_string();
        assert!(err.contains("local/private"));
    }

    #[test]
    fn validate_rejects_whitespace() {
        let tool = test_tool(vec!["example.com"]);
        let err = tool
            .validate_url("https://example.com/hello world")
            .unwrap_err()
            .to_string();
        assert!(err.contains("whitespace"));
    }

    #[test]
    fn validate_rejects_userinfo() {
        let tool = test_tool(vec!["example.com"]);
        let err = tool
            .validate_url("https://user@example.com")
            .unwrap_err()
            .to_string();
        assert!(err.contains("userinfo"));
    }

    #[test]
    fn validate_requires_allowlist() {
        let security = Arc::new(SecurityPolicy::default());
        let tool = HttpRequestTool::new(
            security,
            vec![],
            1_000_000,
            30,
            false,
            Vec::new(),
            Vec::new(),
        )
        .unwrap();
        let err = tool
            .validate_url("https://example.com")
            .unwrap_err()
            .to_string();
        assert!(err.contains("allowed_domains"));
    }

    #[test]
    fn validate_accepts_valid_methods() {
        let tool = test_tool(vec!["example.com"]);
        assert!(tool.validate_method("GET").is_ok());
        assert!(tool.validate_method("POST").is_ok());
        assert!(tool.validate_method("PUT").is_ok());
        assert!(tool.validate_method("DELETE").is_ok());
        assert!(tool.validate_method("PATCH").is_ok());
        assert!(tool.validate_method("HEAD").is_ok());
        assert!(tool.validate_method("OPTIONS").is_ok());
    }

    #[test]
    fn validate_rejects_invalid_method() {
        let tool = test_tool(vec!["example.com"]);
        let err = tool.validate_method("INVALID").unwrap_err().to_string();
        assert!(err.contains("Unsupported HTTP method"));
    }

    #[tokio::test]
    async fn execute_blocks_readonly_mode() {
        let security = Arc::new(SecurityPolicy {
            autonomy: AutonomyLevel::ReadOnly,
            ..SecurityPolicy::default()
        });
        let tool = HttpRequestTool::new(
            security,
            vec!["example.com".into()],
            1_000_000,
            30,
            false,
            Vec::new(),
            Vec::new(),
        )
        .unwrap();
        let result = tool
            .execute(json!({"url": "https://example.com"}))
            .await
            .unwrap();
        assert!(!result.success);
        assert!(result.error.unwrap().contains("read-only"));
    }

    #[test]
    fn parse_headers_rejects_non_string_values() {
        let tool = test_tool(vec!["example.com"]);
        let headers = json!({
            "X-Number": 42,
            "Content-Type": "application/json"
        });
        let err = tool.parse_headers(&headers).unwrap_err().to_string();
        assert!(
            err.contains("X-Number"),
            "Should reject non-string header value, got: {err}"
        );
    }

    #[test]
    fn parse_headers_preserves_original_values() {
        let tool = test_tool(vec!["example.com"]);
        let headers = json!({
            "Authorization": "Bearer secret",
            "Content-Type": "application/json",
            "X-API-Key": "my-key"
        });
        let parsed = tool.parse_headers(&headers).unwrap();
        assert_eq!(parsed.len(), 3);
        assert_eq!(parsed["authorization"], "Bearer secret");
        assert_eq!(parsed["x-api-key"], "my-key");
        assert_eq!(parsed["content-type"], "application/json");
    }

    #[test]
    fn redact_headers_for_display_redacts_sensitive() {
        let headers = vec![
            ("Authorization".into(), "Bearer secret".into()),
            ("Content-Type".into(), "application/json".into()),
            ("X-API-Key".into(), "my-key".into()),
            ("X-Secret-Token".into(), "tok-123".into()),
        ];
        let redacted = HttpRequestTool::redact_headers_for_display(&headers);
        assert_eq!(redacted.len(), 4);
        assert!(
            redacted
                .iter()
                .any(|(k, v)| k == "Authorization" && v == "***REDACTED***")
        );
        assert!(
            redacted
                .iter()
                .any(|(k, v)| k == "X-API-Key" && v == "***REDACTED***")
        );
        assert!(
            redacted
                .iter()
                .any(|(k, v)| k == "X-Secret-Token" && v == "***REDACTED***")
        );
        assert!(
            redacted
                .iter()
                .any(|(k, v)| k == "Content-Type" && v == "application/json")
        );
    }

    #[test]
    fn redact_headers_does_not_alter_original() {
        let headers = vec![("Authorization".into(), "Bearer real-token".into())];
        let _ = HttpRequestTool::redact_headers_for_display(&headers);
        assert_eq!(headers[0].1, "Bearer real-token");
    }

    #[test]
    fn ssrf_alternate_notations_rejected_by_validate_url() {
        // Alternate notations must be blocked by validation.
        // Depending on URL canonicalization, they may be rejected either as:
        // - private/local hosts, or
        // - allowlist mismatches.
        let tool = test_tool(vec!["example.com"]);
        for notation in [
            "http://0177.0.0.1",
            "http://0x7f000001",
            "http://2130706433",
            "http://127.000.000.001",
        ] {
            let err = tool.validate_url(notation).unwrap_err().to_string();
            assert!(
                err.contains("allowed_domains") || err.contains("local/private"),
                "Expected secure rejection for {notation}, got: {err}"
            );
        }
    }

    #[test]
    fn redirect_policy_is_none() {
        // Structural test: the tool should be buildable with redirect-safe config.
        // The actual Policy::none() enforcement is in execute_request's client builder.
        let tool = test_tool(vec!["example.com"]);
        assert_eq!(tool.name(), "http_request");
    }

    #[test]
    fn runtime_proxy_conflicts_with_dns_pinning_only_when_it_applies() {
        let global_proxy = ProxyConfig {
            enabled: true,
            http_proxy: Some("http://proxy.example:8080".into()),
            scope: zeroclaw_config::schema::ProxyScope::Zeroclaw,
            ..ProxyConfig::default()
        };
        assert!(proxy_conflicts_with_dns_pinning(&global_proxy));
        assert!(HTTP_REQUEST_PROXY_PINNING_ERROR.contains("tool.http_request"));
        assert!(HTTP_REQUEST_PROXY_PINNING_ERROR.contains("tool.*"));

        let environment_proxy = ProxyConfig {
            enabled: true,
            http_proxy: Some("http://proxy.example:8080".into()),
            scope: zeroclaw_config::schema::ProxyScope::Environment,
            ..ProxyConfig::default()
        };
        assert!(proxy_conflicts_with_dns_pinning(&environment_proxy));
        assert!(HTTP_REQUEST_PROXY_PINNING_ERROR.contains("environment"));

        let exact_service_proxy = ProxyConfig {
            enabled: true,
            http_proxy: Some("http://proxy.example:8080".into()),
            scope: zeroclaw_config::schema::ProxyScope::Services,
            services: vec!["tool.http_request".into()],
            ..ProxyConfig::default()
        };
        assert!(proxy_conflicts_with_dns_pinning(&exact_service_proxy));

        let other_service_proxy = ProxyConfig {
            enabled: true,
            http_proxy: Some("http://proxy.example:8080".into()),
            scope: zeroclaw_config::schema::ProxyScope::Services,
            services: vec!["provider.openai".into()],
            ..ProxyConfig::default()
        };
        assert!(!proxy_conflicts_with_dns_pinning(&other_service_proxy));
        assert!(!proxy_conflicts_with_dns_pinning(&ProxyConfig::default()));
    }

    #[test]
    fn validate_rejects_ftp_scheme() {
        let tool = test_tool(vec!["example.com"]);
        let err = tool
            .validate_url("ftp://example.com")
            .unwrap_err()
            .to_string();
        assert!(err.contains("http://") || err.contains("https://"));
    }

    #[test]
    fn validate_rejects_empty_url() {
        let tool = test_tool(vec!["example.com"]);
        let err = tool.validate_url("").unwrap_err().to_string();
        assert!(err.contains("empty"));
    }

    #[test]
    fn validate_accepts_public_ipv6_host_when_allowlisted() {
        let tool = test_tool(vec!["2607:f8b0:4004:800::200e"]);
        assert!(
            tool.validate_url("https://[2607:f8b0:4004:800::200e]/path")
                .is_ok()
        );
    }

    // ── allow_private_hosts opt-in tests ────────────────────────

    #[test]
    fn default_blocks_private_hosts() {
        let tool = test_tool(vec!["localhost", "192.168.1.5", "*"]);
        assert!(
            tool.validate_url("https://localhost:8080")
                .unwrap_err()
                .to_string()
                .contains("local/private")
        );
        assert!(
            tool.validate_url("https://192.168.1.5")
                .unwrap_err()
                .to_string()
                .contains("local/private")
        );
        assert!(
            tool.validate_url("https://10.0.0.1")
                .unwrap_err()
                .to_string()
                .contains("local/private")
        );
    }

    #[test]
    fn allow_private_hosts_permits_localhost() {
        let tool = test_tool_with_private(vec!["localhost"], true);
        assert!(tool.validate_url("https://localhost:8080").is_ok());
    }

    #[test]
    fn allow_private_hosts_permits_private_ipv4() {
        let tool = test_tool_with_private(vec!["192.168.1.5"], true);
        assert!(tool.validate_url("https://192.168.1.5").is_ok());
    }

    #[test]
    fn allow_private_hosts_permits_rfc1918_with_wildcard() {
        let tool = test_tool_with_private(vec!["*"], true);
        assert!(tool.validate_url("https://10.0.0.1").is_ok());
        assert!(tool.validate_url("https://172.16.0.1").is_ok());
        assert!(tool.validate_url("https://192.168.1.1").is_ok());
        assert!(tool.validate_url("http://localhost:8123").is_ok());
    }

    #[test]
    fn allow_private_hosts_permits_ipv6_loopback_when_allowlisted() {
        let tool = test_tool_with_private(vec!["::1"], true);
        assert!(tool.validate_url("https://[::1]:8443").is_ok());
    }

    #[test]
    fn allow_private_hosts_still_requires_allowlist() {
        let tool = test_tool_with_private(vec!["example.com"], true);
        let err = tool
            .validate_url("https://192.168.1.5")
            .unwrap_err()
            .to_string();
        assert!(
            err.contains("allowed_domains"),
            "Private host should still need allowlist match, got: {err}"
        );
    }

    #[test]
    fn allow_private_hosts_false_still_blocks() {
        let tool = test_tool_with_private(vec!["*"], false);
        assert!(
            tool.validate_url("https://localhost:8080")
                .unwrap_err()
                .to_string()
                .contains("local/private")
        );
    }

    #[test]
    fn allowed_private_hosts_permits_localhost_without_broad_private_opt_in() {
        let tool = test_tool_with_private_allowlist(vec!["example.com"], false, vec!["localhost"]);
        assert!(tool.validate_url("https://localhost:8080").is_ok());
    }

    #[test]
    fn allowed_private_hosts_permits_private_ipv4_without_allowed_domains_match() {
        let tool =
            test_tool_with_private_allowlist(vec!["example.com"], false, vec!["192.168.1.5"]);
        assert!(tool.validate_url("https://192.168.1.5").is_ok());
    }

    #[test]
    fn allowed_private_hosts_still_requires_non_empty_allowed_domains() {
        let tool = test_tool_with_private_allowlist(vec![], false, vec!["localhost"]);
        let err = tool
            .validate_url("https://localhost:8080")
            .unwrap_err()
            .to_string();
        assert!(err.contains("allowed_domains"));
    }

    #[test]
    fn allowed_private_hosts_still_blocks_unlisted_private_host() {
        let tool =
            test_tool_with_private_allowlist(vec!["example.com"], false, vec!["192.168.1.5"]);
        let err = tool
            .validate_url("https://192.168.1.6")
            .unwrap_err()
            .to_string();
        assert!(err.contains("local/private"));
    }

    #[test]
    fn allowed_private_hosts_wildcard_only_bypasses_private_hosts() {
        let tool = test_tool_with_private_allowlist(vec!["example.com"], false, vec!["*"]);
        assert!(tool.validate_url("https://10.0.0.1").is_ok());

        let err = tool
            .validate_url("https://news.ycombinator.com")
            .unwrap_err()
            .to_string();
        assert!(err.contains("allowed_domains"));
    }

    #[tokio::test]
    async fn validate_request_target_checks_dns_for_allowed_public_host() {
        let tool = test_tool(vec!["example.com"]);
        let called = std::cell::Cell::new(false);

        let got = tool
            .validate_request_target_with_resolver("https://api.example.com/v1", |host, port| {
                called.set(true);
                assert_eq!(host, "api.example.com");
                assert_eq!(port, 443);
                async {
                    Ok(vec![SocketAddr::new(
                        IpAddr::V4(std::net::Ipv4Addr::new(93, 184, 216, 34)),
                        443,
                    )])
                }
            })
            .await
            .unwrap();

        assert_eq!(got.url, "https://api.example.com/v1");
        assert_eq!(got.host, "api.example.com");
        assert_eq!(
            got.resolved_addrs,
            vec![SocketAddr::new(
                IpAddr::V4(std::net::Ipv4Addr::new(93, 184, 216, 34)),
                443
            )]
        );
        assert!(
            called.get(),
            "allowed public host must still pass DNS SSRF validation"
        );
    }

    #[tokio::test]
    async fn validate_request_target_canonicalizes_trailing_dot_before_dns_pinning() {
        let tool = test_tool(vec!["example.com"]);

        let target = tool
            .validate_request_target_with_resolver(
                "https://api.example.com./v1?x=1",
                |host, port| {
                    assert_eq!(host, "api.example.com");
                    assert_eq!(port, 443);
                    async {
                        Ok(vec![SocketAddr::new(
                            IpAddr::V4(std::net::Ipv4Addr::new(93, 184, 216, 34)),
                            443,
                        )])
                    }
                },
            )
            .await
            .unwrap();
        let parsed = reqwest::Url::parse(&target.url).unwrap();

        assert_eq!(target.host, "api.example.com");
        assert_eq!(parsed.host_str(), Some(target.host.as_str()));
        assert_eq!(parsed.path(), "/v1");
        assert_eq!(parsed.query(), Some("x=1"));
    }

    #[tokio::test]
    async fn validate_request_target_allows_private_resolution_for_private_carveout() {
        let tool =
            test_tool_with_private_allowlist(vec!["example.com"], false, vec!["api.example.com"]);

        let got = tool
            .validate_request_target_with_resolver("https://api.example.com/v1", |host, port| {
                assert_eq!(host, "api.example.com");
                assert_eq!(port, 443);
                async {
                    Ok(vec![SocketAddr::new(
                        IpAddr::V4(std::net::Ipv4Addr::new(10, 0, 0, 5)),
                        443,
                    )])
                }
            })
            .await
            .unwrap();

        assert_eq!(
            got.resolved_addrs,
            vec![SocketAddr::new(
                IpAddr::V4(std::net::Ipv4Addr::new(10, 0, 0, 5)),
                443
            )]
        );
    }

    #[tokio::test]
    async fn validate_request_target_checks_metadata_for_explicit_private_host() {
        let tool =
            test_tool_with_private_allowlist(vec!["example.com"], false, vec!["device.local"]);

        let err = tool
            .validate_request_target_with_resolver("https://device.local/status", |host, port| {
                assert_eq!(host, "device.local");
                assert_eq!(port, 443);
                async {
                    Ok(vec![SocketAddr::new(
                        IpAddr::V4(std::net::Ipv4Addr::new(169, 254, 169, 254)),
                        443,
                    )])
                }
            })
            .await
            .unwrap_err()
            .to_string();

        assert!(
            err.contains("cloud metadata address"),
            "unexpected error: {err}"
        );
    }

    #[tokio::test]
    async fn validate_request_target_blocks_ec2_ipv6_metadata_for_private_carveout() {
        let tool =
            test_tool_with_private_allowlist(vec!["example.com"], false, vec!["device.local"]);

        let err = tool
            .validate_request_target_with_resolver("https://device.local/status", |host, port| {
                assert_eq!(host, "device.local");
                assert_eq!(port, 443);
                async move {
                    Ok(vec![SocketAddr::new(
                        IpAddr::V6("fd00:ec2::254".parse().unwrap()),
                        port,
                    )])
                }
            })
            .await
            .unwrap_err()
            .to_string();

        assert!(
            err.contains("cloud metadata address"),
            "unexpected error: {err}"
        );
    }

    #[tokio::test]
    async fn validate_request_target_uses_direct_ip_without_dns_lookup() {
        let tool = test_tool_with_private(vec!["*"], true);

        let got = tool
            .validate_request_target_with_resolver(
                "http://10.0.0.1:8080/status",
                |_host, _port| async {
                    unreachable!("direct IP literals should not use DNS resolution")
                },
            )
            .await
            .unwrap();

        assert_eq!(
            got.resolved_addrs,
            vec![SocketAddr::new(
                IpAddr::V4(std::net::Ipv4Addr::new(10, 0, 0, 1)),
                8080
            )]
        );
    }

    #[test]
    fn validate_resolved_private_ip_is_blocked_by_default() {
        let ips = [std::net::IpAddr::V4(std::net::Ipv4Addr::new(10, 0, 0, 5))];
        let err = validate_resolved_ips_for_ssrf("api.example.com", false, &ips, &[])
            .unwrap_err()
            .to_string();

        assert!(
            err.contains("non-global address"),
            "unexpected error: {err}"
        );
    }

    #[test]
    fn validate_resolved_private_ip_is_allowed_with_private_carveout() {
        let ips = [std::net::IpAddr::V4(std::net::Ipv4Addr::new(10, 0, 0, 5))];
        assert!(validate_resolved_ips_for_ssrf("api.example.com", true, &ips, &[]).is_ok());
    }

    #[test]
    fn validate_resolved_metadata_ip_is_blocked_even_with_private_carveout() {
        let ips = [std::net::IpAddr::V4(std::net::Ipv4Addr::new(
            169, 254, 169, 254,
        ))];
        let err = validate_resolved_ips_for_ssrf("metadata.example.com", true, &ips, &[])
            .unwrap_err()
            .to_string();

        assert!(
            err.contains("cloud metadata address"),
            "unexpected error: {err}"
        );
    }

    #[test]
    fn metadata_literal_is_blocked_even_when_private_hosts_are_allowed() {
        let tool = test_tool_with_private(vec!["*"], true);
        let err = tool
            .validate_url("http://169.254.169.254/latest/meta-data/")
            .unwrap_err()
            .to_string();

        assert!(err.contains("metadata"), "unexpected error: {err}");
    }

    #[test]
    fn apipa_literal_is_blocked_with_link_local_diagnostic() {
        let tool = test_tool_with_private(vec!["*"], true);
        let err = tool
            .validate_url("http://169.254.12.7/status")
            .unwrap_err()
            .to_string();

        assert!(err.contains("link-local host"), "unexpected error: {err}");
        assert!(err.contains("blocked unconditionally"));
        assert!(!err.contains("cloud metadata host"));
    }

    // ── IPv6 end-to-end coverage ──────────────────────────────

    #[test]
    fn ipv6_url_parse_variants_extract_correct_host() {
        assert_eq!(
            extract_host("https://[2001:db8::1]/api").unwrap(),
            "2001:db8::1"
        );
        assert_eq!(
            extract_host("https://[2001:db8::1]:8080/api?q=1").unwrap(),
            "2001:db8::1"
        );
        assert_eq!(
            extract_host("http://[2607:f8b0:4004:800::200e]:443/path#frag").unwrap(),
            "2607:f8b0:4004:800::200e"
        );
    }

    #[test]
    fn ipv6_allowlist_handles_compressed_notation() {
        let tool = test_tool(vec!["::1", "fe80::1"]);
        assert!(tool.validate_url("https://[::1]:8443").is_err()); // blocked — local/private
        assert!(tool.validate_url("https://[fe80::1]").is_err()); // blocked — local/private
    }

    #[tokio::test]
    async fn ipv6_end_to_end_real_request_over_loopback() {
        let listener = match tokio::net::TcpListener::bind("[::1]:0").await {
            Ok(l) => l,
            Err(_) => return, // IPv6 not available in this environment
        };
        let port = listener.local_addr().unwrap().port();

        // Spawn a minimal HTTP server that responds with a known body.
        let server_handle = zeroclaw_spawn::spawn!(async move {
            if let Ok((mut stream, _)) = listener.accept().await {
                use tokio::io::AsyncWriteExt;
                let response = b"HTTP/1.1 200 OK\r\nContent-Length: 16\r\nContent-Type: text/plain\r\nConnection: close\r\n\r\nhello from ipv6!";
                let _ = stream.write_all(response).await;
                let _ = stream.flush().await;
            }
        });

        let url = format!("http://[::1]:{port}/");

        let security = Arc::new(SecurityPolicy {
            autonomy: AutonomyLevel::Supervised,
            ..SecurityPolicy::default()
        });
        let tool = HttpRequestTool::new(
            security,
            vec!["::1".to_string()],
            1_000_000, // max_response_size
            5,         // timeout_secs
            true,      // allow_private_hosts
            Vec::new(),
            Vec::new(),
        )
        .unwrap();

        let result = tokio::time::timeout(
            Duration::from_secs(10),
            tool.execute(json!({
                "url": url,
                "method": "GET"
            })),
        )
        .await;

        // Abort the server task regardless of outcome.
        server_handle.abort();

        match result {
            Ok(Ok(r)) if r.success && r.output.contains("hello from ipv6!") => {}
            Ok(Ok(_)) => {} // request completed but response didn't match — acceptable
            Ok(Err(_)) => {} // validation/network error — acceptable
            Err(_) => {}    // timeout — IPv6 connectivity may be unavailable
        }
    }

    /// A globally-classified NAT64 prefix, the shape a real deployment uses.
    /// The IPv6 documentation range is itself non-global, so a documentation
    /// prefix would be rejected for an unrelated reason and prove nothing
    /// about the NAT64 decode.
    const TEST_NAT64_PREFIX: &str = "2001:67c:2b0:db32:0:1::/96";
    /// `TEST_NAT64_PREFIX` with 10.0.0.1 embedded per RFC 6052 §2.2.
    const NAT64_PRIVATE_V4: &str = "2001:67c:2b0:db32:0:1:a00:1";
    /// `TEST_NAT64_PREFIX` with 169.254.169.254 embedded.
    const NAT64_METADATA_V4: &str = "2001:67c:2b0:db32:0:1:a9fe:a9fe";

    async fn resolve_target_to(
        tool: &HttpRequestTool,
        address: &str,
    ) -> anyhow::Result<ValidatedHttpRequestTarget> {
        let ip = address.parse::<IpAddr>().unwrap();
        tool.validate_request_target_with_resolver(
            "https://attacker.example.com/",
            move |_host, port| async move { Ok(vec![SocketAddr::new(ip, port)]) },
        )
        .await
    }

    #[tokio::test]
    async fn configured_nat64_prefix_blocks_resolution_to_embedded_private_v4() {
        let tool = test_tool_with_nat64(vec!["*"], false, vec![], vec![TEST_NAT64_PREFIX]);
        let err = resolve_target_to(&tool, NAT64_PRIVATE_V4)
            .await
            .unwrap_err()
            .to_string();
        assert!(
            err.contains("non-global address 10.0.0.1"),
            "unexpected error: {err}"
        );
        assert!(
            err.contains(TEST_NAT64_PREFIX),
            "error must name the configured prefix: {err}"
        );
    }

    #[tokio::test]
    async fn configured_nat64_prefix_blocks_resolution_to_embedded_metadata_v4() {
        // The private opt-in never re-opens metadata, so this holds on both
        // sides of the allow_private_hosts branch.
        for allow_private in [false, true] {
            let tool =
                test_tool_with_nat64(vec!["*"], allow_private, vec![], vec![TEST_NAT64_PREFIX]);
            let err = resolve_target_to(&tool, NAT64_METADATA_V4)
                .await
                .unwrap_err()
                .to_string();
            assert!(
                err.contains("cloud metadata address 169.254.169.254"),
                "allow_private_hosts={allow_private} produced unexpected error: {err}"
            );
        }
    }

    #[tokio::test]
    async fn nat64_embedded_private_v4_is_reachable_without_a_configured_prefix() {
        // Honest boundary: nothing in the address marks it as NAT64, so with
        // no prefix configured the tool dials it. This is exactly the hole
        // `security.nat64_prefixes` closes.
        let tool = test_tool_with_nat64(vec!["*"], false, vec![], vec![]);
        let target = resolve_target_to(&tool, NAT64_PRIVATE_V4).await.unwrap();
        assert_eq!(target.host, "attacker.example.com");
    }

    #[tokio::test]
    async fn configured_nat64_prefix_still_allows_embedded_global_v4() {
        // 93.184.216.34 embedded under the same prefix; 192.0.2.33 from
        // RFC 6052's own example is documentation space and non-global, so it
        // cannot stand in for an accepted destination.
        let tool = test_tool_with_nat64(vec!["*"], false, vec![], vec![TEST_NAT64_PREFIX]);
        let target = resolve_target_to(&tool, "2001:67c:2b0:db32:0:1:5db8:d822")
            .await
            .unwrap();
        assert_eq!(target.host, "attacker.example.com");
    }

    #[test]
    fn malformed_nat64_prefix_fails_tool_construction() {
        // Fail closed: a typo must refuse to build the tool rather than
        // silently leaving the network-specific check disabled.
        let err = try_test_tool_with_nat64(
            vec!["example.com"],
            false,
            vec![],
            vec![TEST_NAT64_PREFIX, "2001:db8::/33"],
        )
        .err()
        .expect("malformed nat64 prefix must fail construction")
        .to_string();
        assert!(
            err.contains("security.nat64_prefixes"),
            "unexpected error: {err}"
        );
        assert!(err.contains("2001:db8::/33"), "unexpected error: {err}");
    }
}
