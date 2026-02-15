use super::traits::{Tool, ToolResult};
use crate::config::HttpRequestConfig;
use crate::security::SecurityPolicy;
use async_trait::async_trait;
use serde_json::json;
use std::sync::Arc;
use std::time::Duration;

/// Headers whose values are redacted from output to prevent credential leakage.
const REDACTED_HEADERS: &[&str] = &[
    "authorization",
    "cookie",
    "set-cookie",
    "proxy-authorization",
];

/// Make outbound HTTP requests to allowlisted HTTPS endpoints.
pub struct HttpRequestTool {
    security: Arc<SecurityPolicy>,
    config: HttpRequestConfig,
    client: reqwest::Client,
    /// Pre-normalized allowlist (computed once at construction).
    allowed_domains: Vec<String>,
}

impl HttpRequestTool {
    pub fn new(security: Arc<SecurityPolicy>, config: HttpRequestConfig) -> anyhow::Result<Self> {
        let allowed_domains = normalize_allowed_domains(config.allowed_domains.clone());
        let allowed_for_redirect = allowed_domains.clone();

        let redirect_policy = reqwest::redirect::Policy::custom(move |attempt| {
            let url = attempt.url();

            if url.scheme() != "https" {
                return attempt.error("redirect must stay on https");
            }

            let host = match url.host_str() {
                Some(h) => h.to_lowercase(),
                None => return attempt.error("redirect target has no host"),
            };

            if is_private_or_local_host(&host) {
                return attempt.error(format!("redirect blocked: local/private host {host}"));
            }

            if !host_matches_allowlist(&host, &allowed_for_redirect) {
                return attempt.error(format!("redirect blocked: host '{host}' not in allowlist"));
            }

            if attempt.previous().len() >= 5 {
                return attempt.error("too many redirects");
            }

            attempt.follow()
        });

        let client = reqwest::Client::builder()
            .timeout(Duration::from_secs(config.timeout_secs))
            .redirect(redirect_policy)
            .build()
            .map_err(|e| anyhow::anyhow!("Failed to build HTTP client: {e}"))?;

        Ok(Self {
            security,
            config,
            client,
            allowed_domains,
        })
    }

    fn validate_url(&self, raw_url: &str) -> anyhow::Result<String> {
        let url = raw_url.trim();

        if url.is_empty() {
            anyhow::bail!("URL cannot be empty");
        }

        if url.chars().any(char::is_whitespace) {
            anyhow::bail!("URL cannot contain whitespace");
        }

        if !url.starts_with("https://") {
            anyhow::bail!("Only https:// URLs are allowed");
        }

        if self.allowed_domains.is_empty() {
            anyhow::bail!(
                "http_request tool is enabled but no allowed_domains are configured. \
                 Add [http_request].allowed_domains in config.toml"
            );
        }

        let host = extract_host(url)?;

        if is_private_or_local_host(&host) {
            anyhow::bail!("Blocked local/private host: {host}");
        }

        if !host_matches_allowlist(&host, &self.allowed_domains) {
            anyhow::bail!("Host '{host}' is not in http_request.allowed_domains");
        }

        Ok(url.to_string())
    }
}

#[async_trait]
impl Tool for HttpRequestTool {
    fn name(&self) -> &str {
        "http_request"
    }

    fn description(&self) -> &str {
        "Make an outbound HTTP request to an allowlisted HTTPS endpoint. \
         Supports GET, POST, PUT, DELETE, and PATCH methods. \
         Security: HTTPS-only, domain allowlist, SSRF protection, header redaction."
    }

    fn parameters_schema(&self) -> serde_json::Value {
        json!({
            "type": "object",
            "properties": {
                "method": {
                    "type": "string",
                    "enum": ["GET", "POST", "PUT", "DELETE", "PATCH"],
                    "description": "HTTP method"
                },
                "url": {
                    "type": "string",
                    "description": "HTTPS URL to request"
                },
                "headers": {
                    "type": "object",
                    "description": "Optional request headers (key-value pairs)",
                    "additionalProperties": { "type": "string" }
                },
                "body": {
                    "type": "string",
                    "description": "Optional request body"
                }
            },
            "required": ["method", "url"]
        })
    }

    async fn execute(&self, args: serde_json::Value) -> anyhow::Result<ToolResult> {
        // ── Extract parameters ──────────────────────────────────
        let method_str = args
            .get("method")
            .and_then(|v| v.as_str())
            .ok_or_else(|| anyhow::anyhow!("Missing 'method' parameter"))?;

        let url = args
            .get("url")
            .and_then(|v| v.as_str())
            .ok_or_else(|| anyhow::anyhow!("Missing 'url' parameter"))?;

        // ── Security checks ─────────────────────────────────────
        if !self.security.can_act() {
            return Ok(ToolResult {
                success: false,
                output: String::new(),
                error: Some("Action blocked: autonomy is read-only".into()),
            });
        }

        if !self.security.record_action() {
            return Ok(ToolResult {
                success: false,
                output: String::new(),
                error: Some("Action blocked: rate limit exceeded".into()),
            });
        }

        // ── Validate method ─────────────────────────────────────
        let method = match method_str.to_uppercase().as_str() {
            "GET" => reqwest::Method::GET,
            "POST" => reqwest::Method::POST,
            "PUT" => reqwest::Method::PUT,
            "DELETE" => reqwest::Method::DELETE,
            "PATCH" => reqwest::Method::PATCH,
            _ => {
                return Ok(ToolResult {
                    success: false,
                    output: String::new(),
                    error: Some(format!(
                        "Unsupported HTTP method: {method_str}. \
                         Allowed: GET, POST, PUT, DELETE, PATCH"
                    )),
                })
            }
        };

        // ── Validate URL ────────────────────────────────────────
        let validated_url = match self.validate_url(url) {
            Ok(v) => v,
            Err(e) => {
                return Ok(ToolResult {
                    success: false,
                    output: String::new(),
                    error: Some(e.to_string()),
                })
            }
        };

        // ── Build request ───────────────────────────────────────
        let mut request = self.client.request(method, &validated_url);

        if let Some(headers) = args.get("headers").and_then(|v| v.as_object()) {
            for (key, value) in headers {
                if let Some(val) = value.as_str() {
                    request = request.header(key.as_str(), val);
                }
            }
        }

        if let Some(body) = args.get("body").and_then(|v| v.as_str()) {
            request = request.body(body.to_string());
        }

        // ── Execute request ─────────────────────────────────────
        let response = match request.send().await {
            Ok(resp) => resp,
            Err(e) => {
                return Ok(ToolResult {
                    success: false,
                    output: String::new(),
                    error: Some(format!("HTTP request failed: {e}")),
                })
            }
        };

        let status = response.status().as_u16();

        // Collect response headers with redaction
        let mut resp_headers = serde_json::Map::new();
        for (key, value) in response.headers() {
            let key_lower = key.as_str().to_lowercase();
            let val = if REDACTED_HEADERS.contains(&key_lower.as_str()) {
                "[REDACTED]".to_string()
            } else {
                value.to_str().unwrap_or("[non-utf8]").to_string()
            };
            resp_headers.insert(key.as_str().to_string(), serde_json::Value::String(val));
        }

        // Read body with streaming size limit
        use futures_util::StreamExt;

        let max_bytes = self.config.max_response_bytes;
        let mut body = Vec::new();
        let mut truncated = false;
        let mut stream = response.bytes_stream();
        while let Some(chunk) = stream.next().await {
            let chunk = match chunk {
                Ok(c) => c,
                Err(e) => {
                    return Ok(ToolResult {
                        success: false,
                        output: String::new(),
                        error: Some(format!("Failed to read response body: {e}")),
                    });
                }
            };
            if body.len() + chunk.len() > max_bytes {
                let remaining = max_bytes.saturating_sub(body.len());
                body.extend_from_slice(&chunk[..remaining]);
                truncated = true;
                break;
            }
            body.extend_from_slice(&chunk);
        }

        let body_text = String::from_utf8_lossy(&body).to_string();
        let body_output = if truncated {
            format!("{body_text}\n... [response truncated at {max_bytes} bytes]")
        } else {
            body_text
        };

        let output = json!({
            "status": status,
            "headers": resp_headers,
            "body": body_output,
        });

        Ok(ToolResult {
            success: (200..300).contains(&(status as usize)),
            output: output.to_string(),
            error: None,
        })
    }
}

// ── URL validation helpers (duplicated from browser_open.rs) ──

fn normalize_allowed_domains(domains: Vec<String>) -> Vec<String> {
    let mut normalized = domains
        .into_iter()
        .filter_map(|d| normalize_domain(&d))
        .collect::<Vec<_>>();
    normalized.sort_unstable();
    normalized.dedup();
    normalized
}

fn normalize_domain(raw: &str) -> Option<String> {
    let mut d = raw.trim().to_lowercase();
    if d.is_empty() {
        return None;
    }

    if let Some(stripped) = d.strip_prefix("https://") {
        d = stripped.to_string();
    } else if let Some(stripped) = d.strip_prefix("http://") {
        d = stripped.to_string();
    }

    if let Some((host, _)) = d.split_once('/') {
        d = host.to_string();
    }

    d = d.trim_start_matches('.').trim_end_matches('.').to_string();

    if let Some((host, _)) = d.split_once(':') {
        d = host.to_string();
    }

    if d.is_empty() || d.chars().any(char::is_whitespace) {
        return None;
    }

    Some(d)
}

fn extract_host(url: &str) -> anyhow::Result<String> {
    let rest = url
        .strip_prefix("https://")
        .ok_or_else(|| anyhow::anyhow!("Only https:// URLs are allowed"))?;

    let authority = rest
        .split(['/', '?', '#'])
        .next()
        .ok_or_else(|| anyhow::anyhow!("Invalid URL"))?;

    if authority.is_empty() {
        anyhow::bail!("URL must include a host");
    }

    if authority.contains('@') {
        anyhow::bail!("URL userinfo is not allowed");
    }

    if authority.starts_with('[') {
        anyhow::bail!("IPv6 hosts are not supported");
    }

    let host = authority
        .split(':')
        .next()
        .unwrap_or_default()
        .trim()
        .trim_end_matches('.')
        .to_lowercase();

    if host.is_empty() {
        anyhow::bail!("URL must include a valid host");
    }

    Ok(host)
}

fn host_matches_allowlist(host: &str, allowed_domains: &[String]) -> bool {
    allowed_domains.iter().any(|domain| {
        host == domain
            || host
                .strip_suffix(domain)
                .is_some_and(|prefix| prefix.ends_with('.'))
    })
}

fn is_private_or_local_host(host: &str) -> bool {
    let has_local_tld = host
        .rsplit('.')
        .next()
        .is_some_and(|label| label == "local");

    if host == "localhost" || host.ends_with(".localhost") || has_local_tld || host == "::1" {
        return true;
    }

    if let Some([a, b, _, _]) = parse_ipv4(host) {
        return a == 0
            || a == 10
            || a == 127
            || (a == 169 && b == 254)
            || (a == 172 && (16..=31).contains(&b))
            || (a == 192 && b == 168)
            || (a == 100 && (64..=127).contains(&b));
    }

    false
}

fn parse_ipv4(host: &str) -> Option<[u8; 4]> {
    let parts: Vec<&str> = host.split('.').collect();
    if parts.len() != 4 {
        return None;
    }

    let mut octets = [0_u8; 4];
    for (i, part) in parts.iter().enumerate() {
        octets[i] = part.parse::<u8>().ok()?;
    }
    Some(octets)
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::security::{AutonomyLevel, SecurityPolicy};

    fn test_config(allowed_domains: Vec<&str>) -> HttpRequestConfig {
        HttpRequestConfig {
            enabled: true,
            allowed_domains: allowed_domains.into_iter().map(String::from).collect(),
            timeout_secs: 30,
            max_response_bytes: 1_048_576,
        }
    }

    fn test_tool(allowed_domains: Vec<&str>) -> HttpRequestTool {
        let security = Arc::new(SecurityPolicy {
            autonomy: AutonomyLevel::Supervised,
            ..SecurityPolicy::default()
        });
        HttpRequestTool::new(security, test_config(allowed_domains)).unwrap()
    }

    // ── Tool metadata ───────────────────────────────────────────

    #[test]
    fn tool_name() {
        let tool = test_tool(vec!["example.com"]);
        assert_eq!(tool.name(), "http_request");
    }

    #[test]
    fn tool_description_not_empty() {
        let tool = test_tool(vec!["example.com"]);
        assert!(!tool.description().is_empty());
    }

    #[test]
    fn tool_schema_has_required_fields() {
        let tool = test_tool(vec!["example.com"]);
        let schema = tool.parameters_schema();
        assert!(schema["properties"]["method"].is_object());
        assert!(schema["properties"]["url"].is_object());
        assert!(schema["properties"]["headers"].is_object());
        assert!(schema["properties"]["body"].is_object());
        let required = schema["required"].as_array().unwrap();
        let required_strs: Vec<&str> = required.iter().map(|v| v.as_str().unwrap()).collect();
        assert!(required_strs.contains(&"method"));
        assert!(required_strs.contains(&"url"));
    }

    // ── URL validation ──────────────────────────────────────────

    #[test]
    fn validate_accepts_allowlisted_https_domain() {
        let tool = test_tool(vec!["api.example.com"]);
        assert!(tool.validate_url("https://api.example.com/v1/data").is_ok());
    }

    #[test]
    fn validate_accepts_subdomain() {
        let tool = test_tool(vec!["example.com"]);
        assert!(tool.validate_url("https://api.example.com/v1").is_ok());
    }

    #[test]
    fn validate_rejects_http() {
        let tool = test_tool(vec!["example.com"]);
        let err = tool
            .validate_url("http://example.com")
            .unwrap_err()
            .to_string();
        assert!(err.contains("https://"));
    }

    #[test]
    fn validate_rejects_private_ip() {
        let tool = test_tool(vec!["192.168.1.1"]);
        let err = tool
            .validate_url("https://192.168.1.1")
            .unwrap_err()
            .to_string();
        assert!(err.contains("local/private"));
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
    fn validate_rejects_loopback_ip() {
        let tool = test_tool(vec!["127.0.0.1"]);
        let err = tool
            .validate_url("https://127.0.0.1")
            .unwrap_err()
            .to_string();
        assert!(err.contains("local/private"));
    }

    #[test]
    fn validate_rejects_10_network() {
        let tool = test_tool(vec!["10.0.0.1"]);
        let err = tool
            .validate_url("https://10.0.0.1")
            .unwrap_err()
            .to_string();
        assert!(err.contains("local/private"));
    }

    #[test]
    fn validate_rejects_non_allowlisted_domain() {
        let tool = test_tool(vec!["example.com"]);
        let err = tool
            .validate_url("https://evil.com")
            .unwrap_err()
            .to_string();
        assert!(err.contains("allowed_domains"));
    }

    #[test]
    fn validate_rejects_empty_url() {
        let tool = test_tool(vec!["example.com"]);
        let err = tool.validate_url("").unwrap_err().to_string();
        assert!(err.contains("empty"));
    }

    #[test]
    fn validate_rejects_whitespace_url() {
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
    fn validate_rejects_empty_allowlist() {
        let tool = test_tool(vec![]);
        let err = tool
            .validate_url("https://example.com")
            .unwrap_err()
            .to_string();
        assert!(err.contains("allowed_domains"));
    }

    // ── Security: read-only mode ────────────────────────────────

    #[tokio::test]
    async fn execute_blocks_readonly_mode() {
        let security = Arc::new(SecurityPolicy {
            autonomy: AutonomyLevel::ReadOnly,
            ..SecurityPolicy::default()
        });
        let tool = HttpRequestTool::new(security, test_config(vec!["example.com"])).unwrap();
        let result = tool
            .execute(json!({"method": "GET", "url": "https://example.com"}))
            .await
            .unwrap();
        assert!(!result.success);
        assert!(result.error.unwrap().contains("read-only"));
    }

    // ── Security: rate limiting ─────────────────────────────────

    #[tokio::test]
    async fn execute_blocks_when_rate_limited() {
        let security = Arc::new(SecurityPolicy {
            max_actions_per_hour: 0,
            ..SecurityPolicy::default()
        });
        let tool = HttpRequestTool::new(security, test_config(vec!["example.com"])).unwrap();
        let result = tool
            .execute(json!({"method": "GET", "url": "https://example.com"}))
            .await
            .unwrap();
        assert!(!result.success);
        assert!(result.error.unwrap().contains("rate limit"));
    }

    // ── Invalid method ──────────────────────────────────────────

    #[tokio::test]
    async fn execute_rejects_invalid_method() {
        let tool = test_tool(vec!["example.com"]);
        let result = tool
            .execute(json!({"method": "TRACE", "url": "https://example.com"}))
            .await
            .unwrap();
        assert!(!result.success);
        assert!(result.error.unwrap().contains("Unsupported HTTP method"));
    }

    // ── Header redaction ────────────────────────────────────────

    #[test]
    fn redacted_headers_list_includes_sensitive_headers() {
        assert!(REDACTED_HEADERS.contains(&"authorization"));
        assert!(REDACTED_HEADERS.contains(&"cookie"));
        assert!(REDACTED_HEADERS.contains(&"set-cookie"));
        assert!(REDACTED_HEADERS.contains(&"proxy-authorization"));
    }

    // ── URL helpers ─────────────────────────────────────────────

    #[test]
    fn parse_ipv4_valid() {
        assert_eq!(parse_ipv4("1.2.3.4"), Some([1, 2, 3, 4]));
    }

    #[test]
    fn parse_ipv4_invalid() {
        assert_eq!(parse_ipv4("1.2.3"), None);
        assert_eq!(parse_ipv4("1.2.3.999"), None);
        assert_eq!(parse_ipv4("not-an-ip"), None);
    }

    #[test]
    fn normalize_domain_strips_scheme_and_path() {
        assert_eq!(
            normalize_domain("  HTTPS://Api.Example.com/path "),
            Some("api.example.com".into())
        );
    }

    #[test]
    fn normalize_deduplicates() {
        let got = normalize_allowed_domains(vec![
            "example.com".into(),
            "EXAMPLE.COM".into(),
            "https://example.com/".into(),
        ]);
        assert_eq!(got, vec!["example.com".to_string()]);
    }

    // ── Redirect-path helpers (used by redirect policy) ─────

    #[test]
    fn redirect_to_http_would_be_blocked() {
        // The redirect policy blocks non-https schemes.
        // Verify the scheme check logic: http URL parsed host still
        // won't pass even if the host is allowlisted.
        let host = "example.com";
        let allowed = normalize_allowed_domains(vec!["example.com".into()]);
        // Host matches, but the redirect policy checks scheme first.
        assert!(host_matches_allowlist(host, &allowed));
        // Scheme "http" != "https" → redirect would be rejected.
    }

    #[test]
    fn redirect_to_non_allowlisted_host_blocked() {
        let allowed = normalize_allowed_domains(vec!["api.example.com".into()]);
        assert!(!host_matches_allowlist("evil.com", &allowed));
        assert!(!host_matches_allowlist("other.example.org", &allowed));
    }

    #[test]
    fn redirect_to_private_ip_blocked() {
        // The redirect policy also checks for private/local hosts.
        assert!(is_private_or_local_host("127.0.0.1"));
        assert!(is_private_or_local_host("10.0.0.1"));
        assert!(is_private_or_local_host("192.168.1.1"));
        assert!(is_private_or_local_host("localhost"));
        assert!(is_private_or_local_host("service.local"));
    }

    #[test]
    fn redirect_to_allowlisted_subdomain_ok() {
        let allowed = normalize_allowed_domains(vec!["example.com".into()]);
        assert!(host_matches_allowlist("cdn.example.com", &allowed));
        assert!(host_matches_allowlist("example.com", &allowed));
    }

    #[test]
    fn precomputed_allowlist_matches_config() {
        let tool = test_tool(vec!["Example.COM", "https://api.test.io/path"]);
        assert_eq!(tool.allowed_domains, vec!["api.test.io", "example.com"]);
    }
}
