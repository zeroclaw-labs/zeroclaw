use super::traits::{Tool, ToolResult};
use crate::security::SecurityPolicy;
use async_trait::async_trait;
use serde_json::json;
use std::sync::Arc;
use std::time::Duration;

/// Web fetch tool: fetches a web page and converts HTML to plain text for LLM consumption.
///
/// Unlike `http_request` (an API client returning raw responses), this tool:
/// - Only supports GET
/// - Follows redirects (up to 10)
/// - Converts HTML to clean plain text via `nanohtml2text`
/// - Passes through text/plain, text/markdown, and application/json as-is
/// - Sets a descriptive User-Agent
pub struct WebFetchTool {
    security: Arc<SecurityPolicy>,
    allowed_domains: Vec<String>,
    blocked_domains: Vec<String>,
    max_response_size: usize,
    timeout_secs: u64,
}

impl WebFetchTool {
    pub fn new(
        security: Arc<SecurityPolicy>,
        allowed_domains: Vec<String>,
        blocked_domains: Vec<String>,
        max_response_size: usize,
        timeout_secs: u64,
    ) -> Self {
        Self {
            security,
            allowed_domains: normalize_allowed_domains(allowed_domains),
            blocked_domains: normalize_allowed_domains(blocked_domains),
            max_response_size,
            timeout_secs,
        }
    }

    fn validate_url(&self, raw_url: &str) -> anyhow::Result<String> {
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
                "web_fetch tool is enabled but no allowed_domains are configured. \
                 Add [web_fetch].allowed_domains in config.toml"
            );
        }

        let host = extract_host(url)?;

        if is_private_or_local_host(&host) {
            anyhow::bail!("Blocked local/private host: {host}");
        }

        if host_matches_allowlist(&host, &self.blocked_domains) {
            anyhow::bail!("Host '{host}' is in web_fetch.blocked_domains");
        }

        if !host_matches_allowlist(&host, &self.allowed_domains) {
            anyhow::bail!("Host '{host}' is not in web_fetch.allowed_domains");
        }

        Ok(url.to_string())
    }

    fn truncate_response(&self, text: &str) -> String {
        if text.len() > self.max_response_size {
            let mut truncated = text
                .chars()
                .take(self.max_response_size)
                .collect::<String>();
            truncated.push_str("\n\n... [Response truncated due to size limit] ...");
            truncated
        } else {
            text.to_string()
        }
    }
}

#[async_trait]
impl Tool for WebFetchTool {
    fn name(&self) -> &str {
        "web_fetch"
    }

    fn description(&self) -> &str {
        "Fetch a web page and return its content as clean plain text. \
         HTML pages are automatically converted to readable text. \
         JSON and plain text responses are returned as-is. \
         Only GET requests; follows redirects. \
         Security: allowlist-only domains, no local/private hosts."
    }

    fn parameters_schema(&self) -> serde_json::Value {
        json!({
            "type": "object",
            "properties": {
                "url": {
                    "type": "string",
                    "description": "The HTTP or HTTPS URL to fetch"
                }
            },
            "required": ["url"]
        })
    }

    async fn execute(&self, args: serde_json::Value) -> anyhow::Result<ToolResult> {
        let url = args
            .get("url")
            .and_then(|v| v.as_str())
            .ok_or_else(|| anyhow::anyhow!("Missing 'url' parameter"))?;

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

        let url = match self.validate_url(url) {
            Ok(v) => v,
            Err(e) => {
                return Ok(ToolResult {
                    success: false,
                    output: String::new(),
                    error: Some(e.to_string()),
                })
            }
        };

        // Build client: follow redirects, set timeout, set User-Agent
        let timeout_secs = if self.timeout_secs == 0 {
            tracing::warn!("web_fetch: timeout_secs is 0, using safe default of 30s");
            30
        } else {
            self.timeout_secs
        };

        let builder = reqwest::Client::builder()
            .timeout(Duration::from_secs(timeout_secs))
            .connect_timeout(Duration::from_secs(10))
            .redirect(reqwest::redirect::Policy::limited(10))
            .user_agent("ZeroClaw/0.1 (web_fetch)");
        let builder = crate::config::apply_runtime_proxy_to_builder(builder, "tool.web_fetch");
        let client = match builder.build() {
            Ok(c) => c,
            Err(e) => {
                return Ok(ToolResult {
                    success: false,
                    output: String::new(),
                    error: Some(format!("Failed to build HTTP client: {e}")),
                })
            }
        };

        let response = match client.get(&url).send().await {
            Ok(r) => r,
            Err(e) => {
                return Ok(ToolResult {
                    success: false,
                    output: String::new(),
                    error: Some(format!("HTTP request failed: {e}")),
                })
            }
        };

        let status = response.status();
        if !status.is_success() {
            return Ok(ToolResult {
                success: false,
                output: String::new(),
                error: Some(format!(
                    "HTTP {} {}",
                    status.as_u16(),
                    status.canonical_reason().unwrap_or("Unknown")
                )),
            });
        }

        // Determine content type for processing strategy
        let content_type = response
            .headers()
            .get(reqwest::header::CONTENT_TYPE)
            .and_then(|v| v.to_str().ok())
            .unwrap_or("")
            .to_lowercase();

        let body = match response.text().await {
            Ok(t) => t,
            Err(e) => {
                return Ok(ToolResult {
                    success: false,
                    output: String::new(),
                    error: Some(format!("Failed to read response body: {e}")),
                })
            }
        };

        let text = if content_type.contains("text/html") {
            nanohtml2text::html2text(&body)
        } else if content_type.contains("text/plain")
            || content_type.contains("text/markdown")
            || content_type.contains("application/json")
        {
            body
        } else if content_type.is_empty() {
            // No content-type header; try HTML conversion as best-effort
            nanohtml2text::html2text(&body)
        } else {
            return Ok(ToolResult {
                success: false,
                output: String::new(),
                error: Some(format!(
                    "Unsupported content type: {content_type}. \
                     web_fetch supports text/html, text/plain, text/markdown, and application/json."
                )),
            });
        };

        let output = self.truncate_response(&text);

        Ok(ToolResult {
            success: true,
            output,
            error: None,
        })
    }
}

// ── Helper functions (independent from http_request.rs per DRY rule-of-three) ──

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
        .strip_prefix("http://")
        .or_else(|| url.strip_prefix("https://"))
        .ok_or_else(|| anyhow::anyhow!("Only http:// and https:// URLs are allowed"))?;

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
        anyhow::bail!("IPv6 hosts are not supported in web_fetch");
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
    if allowed_domains.iter().any(|domain| domain == "*") {
        return true;
    }

    allowed_domains.iter().any(|domain| {
        host == domain
            || host
                .strip_suffix(domain)
                .is_some_and(|prefix| prefix.ends_with('.'))
    })
}

fn is_private_or_local_host(host: &str) -> bool {
    let bare = host
        .strip_prefix('[')
        .and_then(|h| h.strip_suffix(']'))
        .unwrap_or(host);

    let has_local_tld = bare
        .rsplit('.')
        .next()
        .is_some_and(|label| label == "local");

    if bare == "localhost" || bare.ends_with(".localhost") || has_local_tld {
        return true;
    }

    if let Ok(ip) = bare.parse::<std::net::IpAddr>() {
        return match ip {
            std::net::IpAddr::V4(v4) => is_non_global_v4(v4),
            std::net::IpAddr::V6(v6) => is_non_global_v6(v6),
        };
    }

    false
}

fn is_non_global_v4(v4: std::net::Ipv4Addr) -> bool {
    let [a, b, c, _] = v4.octets();
    v4.is_loopback()
        || v4.is_private()
        || v4.is_link_local()
        || v4.is_unspecified()
        || v4.is_broadcast()
        || v4.is_multicast()
        || (a == 100 && (64..=127).contains(&b))
        || a >= 240
        || (a == 192 && b == 0 && (c == 0 || c == 2))
        || (a == 198 && b == 51)
        || (a == 203 && b == 0)
        || (a == 198 && (18..=19).contains(&b))
}

fn is_non_global_v6(v6: std::net::Ipv6Addr) -> bool {
    let segs = v6.segments();
    v6.is_loopback()
        || v6.is_unspecified()
        || v6.is_multicast()
        || (segs[0] & 0xfe00) == 0xfc00
        || (segs[0] & 0xffc0) == 0xfe80
        || (segs[0] == 0x2001 && segs[1] == 0x0db8)
        || v6.to_ipv4_mapped().is_some_and(is_non_global_v4)
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::security::{AutonomyLevel, SecurityPolicy};

    fn test_tool(allowed_domains: Vec<&str>) -> WebFetchTool {
        test_tool_with_blocklist(allowed_domains, vec![])
    }

    fn test_tool_with_blocklist(
        allowed_domains: Vec<&str>,
        blocked_domains: Vec<&str>,
    ) -> WebFetchTool {
        let security = Arc::new(SecurityPolicy {
            autonomy: AutonomyLevel::Supervised,
            ..SecurityPolicy::default()
        });
        WebFetchTool::new(
            security,
            allowed_domains.into_iter().map(String::from).collect(),
            blocked_domains.into_iter().map(String::from).collect(),
            500_000,
            30,
        )
    }

    // ── Name and schema ──────────────────────────────────────────

    #[test]
    fn name_is_web_fetch() {
        let tool = test_tool(vec!["example.com"]);
        assert_eq!(tool.name(), "web_fetch");
    }

    #[test]
    fn parameters_schema_requires_url() {
        let tool = test_tool(vec!["example.com"]);
        let schema = tool.parameters_schema();
        assert!(schema["properties"]["url"].is_object());
        let required = schema["required"].as_array().unwrap();
        assert!(required.iter().any(|v| v.as_str() == Some("url")));
    }

    // ── HTML to text conversion ──────────────────────────────────

    #[test]
    fn html_to_text_conversion() {
        let html = "<html><body><h1>Title</h1><p>Hello <b>world</b></p></body></html>";
        let text = nanohtml2text::html2text(html);
        assert!(text.contains("Title"));
        assert!(text.contains("Hello"));
        assert!(text.contains("world"));
        assert!(!text.contains("<h1>"));
        assert!(!text.contains("<p>"));
    }

    // ── URL validation ───────────────────────────────────────────

    #[test]
    fn validate_accepts_exact_domain() {
        let tool = test_tool(vec!["example.com"]);
        let got = tool.validate_url("https://example.com/page").unwrap();
        assert_eq!(got, "https://example.com/page");
    }

    #[test]
    fn validate_accepts_subdomain() {
        let tool = test_tool(vec!["example.com"]);
        assert!(tool.validate_url("https://docs.example.com/guide").is_ok());
    }

    #[test]
    fn validate_accepts_wildcard() {
        let tool = test_tool(vec!["*"]);
        assert!(tool.validate_url("https://news.ycombinator.com").is_ok());
    }

    #[test]
    fn validate_rejects_empty_url() {
        let tool = test_tool(vec!["example.com"]);
        let err = tool.validate_url("").unwrap_err().to_string();
        assert!(err.contains("empty"));
    }

    #[test]
    fn validate_rejects_missing_url() {
        let tool = test_tool(vec!["example.com"]);
        let err = tool.validate_url("  ").unwrap_err().to_string();
        assert!(err.contains("empty"));
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
    fn validate_rejects_allowlist_miss() {
        let tool = test_tool(vec!["example.com"]);
        let err = tool
            .validate_url("https://google.com")
            .unwrap_err()
            .to_string();
        assert!(err.contains("allowed_domains"));
    }

    #[test]
    fn validate_requires_allowlist() {
        let security = Arc::new(SecurityPolicy::default());
        let tool = WebFetchTool::new(security, vec![], vec![], 500_000, 30);
        let err = tool
            .validate_url("https://example.com")
            .unwrap_err()
            .to_string();
        assert!(err.contains("allowed_domains"));
    }

    // ── SSRF protection ──────────────────────────────────────────

    #[test]
    fn ssrf_blocks_localhost() {
        let tool = test_tool(vec!["localhost"]);
        let err = tool
            .validate_url("https://localhost:8080")
            .unwrap_err()
            .to_string();
        assert!(err.contains("local/private"));
    }

    #[test]
    fn ssrf_blocks_private_ipv4() {
        let tool = test_tool(vec!["192.168.1.5"]);
        let err = tool
            .validate_url("https://192.168.1.5")
            .unwrap_err()
            .to_string();
        assert!(err.contains("local/private"));
    }

    #[test]
    fn ssrf_blocks_loopback() {
        assert!(is_private_or_local_host("127.0.0.1"));
        assert!(is_private_or_local_host("127.0.0.2"));
    }

    #[test]
    fn ssrf_blocks_rfc1918() {
        assert!(is_private_or_local_host("10.0.0.1"));
        assert!(is_private_or_local_host("172.16.0.1"));
        assert!(is_private_or_local_host("192.168.1.1"));
    }

    #[test]
    fn ssrf_wildcard_still_blocks_private() {
        let tool = test_tool(vec!["*"]);
        let err = tool
            .validate_url("https://localhost:8080")
            .unwrap_err()
            .to_string();
        assert!(err.contains("local/private"));
    }

    // ── Security policy ──────────────────────────────────────────

    #[tokio::test]
    async fn blocks_readonly_mode() {
        let security = Arc::new(SecurityPolicy {
            autonomy: AutonomyLevel::ReadOnly,
            ..SecurityPolicy::default()
        });
        let tool = WebFetchTool::new(security, vec!["example.com".into()], vec![], 500_000, 30);
        let result = tool
            .execute(json!({"url": "https://example.com"}))
            .await
            .unwrap();
        assert!(!result.success);
        assert!(result.error.unwrap().contains("read-only"));
    }

    #[tokio::test]
    async fn blocks_rate_limited() {
        let security = Arc::new(SecurityPolicy {
            max_actions_per_hour: 0,
            ..SecurityPolicy::default()
        });
        let tool = WebFetchTool::new(security, vec!["example.com".into()], vec![], 500_000, 30);
        let result = tool
            .execute(json!({"url": "https://example.com"}))
            .await
            .unwrap();
        assert!(!result.success);
        assert!(result.error.unwrap().contains("rate limit"));
    }

    // ── Response truncation ──────────────────────────────────────

    #[test]
    fn truncate_within_limit() {
        let tool = test_tool(vec!["example.com"]);
        let text = "hello world";
        assert_eq!(tool.truncate_response(text), "hello world");
    }

    #[test]
    fn truncate_over_limit() {
        let tool = WebFetchTool::new(
            Arc::new(SecurityPolicy::default()),
            vec!["example.com".into()],
            vec![],
            10,
            30,
        );
        let text = "hello world this is long";
        let truncated = tool.truncate_response(text);
        assert!(truncated.contains("[Response truncated"));
    }

    // ── Domain normalization ─────────────────────────────────────

    #[test]
    fn normalize_domain_strips_scheme_and_case() {
        let got = normalize_domain("  HTTPS://Docs.Example.com/path ").unwrap();
        assert_eq!(got, "docs.example.com");
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

    // ── Blocked domains ──────────────────────────────────────────

    #[test]
    fn blocklist_rejects_exact_match() {
        let tool = test_tool_with_blocklist(vec!["*"], vec!["evil.com"]);
        let err = tool
            .validate_url("https://evil.com/page")
            .unwrap_err()
            .to_string();
        assert!(err.contains("blocked_domains"));
    }

    #[test]
    fn blocklist_rejects_subdomain() {
        let tool = test_tool_with_blocklist(vec!["*"], vec!["evil.com"]);
        let err = tool
            .validate_url("https://api.evil.com/v1")
            .unwrap_err()
            .to_string();
        assert!(err.contains("blocked_domains"));
    }

    #[test]
    fn blocklist_wins_over_allowlist() {
        let tool = test_tool_with_blocklist(vec!["evil.com"], vec!["evil.com"]);
        let err = tool
            .validate_url("https://evil.com")
            .unwrap_err()
            .to_string();
        assert!(err.contains("blocked_domains"));
    }

    #[test]
    fn blocklist_allows_non_blocked() {
        let tool = test_tool_with_blocklist(vec!["*"], vec!["evil.com"]);
        assert!(tool.validate_url("https://example.com").is_ok());
    }
}
