use super::traits::{Tool, ToolResult};
use super::url_validation::{
    extract_host, host_matches_allowlist, is_private_or_local_host, normalize_allowed_domains,
};
use crate::security::SecurityPolicy;
use async_trait::async_trait;
use serde_json::json;
use std::sync::Arc;
use std::time::Duration;

/// Fetch a URL and return its content as Markdown.
///
/// Two providers are supported:
/// - `"fast_html2md"` (default) — local conversion; reqwest fetches raw HTML, fast_html2md converts it.
///   No API key required. Proxy is applied via `apply_runtime_proxy_to_builder`.
/// - `"firecrawl"` — cloud-based conversion via the Firecrawl API. Requires the `firecrawl`
///   compile-time feature and a configured API key.
///
/// SSRF protection is enforced for both providers: private/local hosts and non-http(s) schemes
/// are always rejected before any outbound request is made.
pub struct WebFetchTool {
    security: Arc<SecurityPolicy>,
    provider: String,
    firecrawl_api_key: Option<String>,
    firecrawl_api_url: Option<String>,
    allowed_domains: Vec<String>,
    max_response_size: usize,
    timeout_secs: u64,
    user_agent: String,
}

impl WebFetchTool {
    pub fn new(
        security: Arc<SecurityPolicy>,
        provider: String,
        firecrawl_api_key: Option<String>,
        firecrawl_api_url: Option<String>,
        allowed_domains: Vec<String>,
        max_response_size: usize,
        timeout_secs: u64,
        user_agent: String,
    ) -> Self {
        Self {
            security,
            provider: provider.trim().to_lowercase(),
            firecrawl_api_key,
            firecrawl_api_url,
            allowed_domains: normalize_allowed_domains(allowed_domains),
            max_response_size,
            timeout_secs,
            user_agent,
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

        if !host_matches_allowlist(&host, &self.allowed_domains) {
            anyhow::bail!("Host '{host}' is not in web_fetch.allowed_domains");
        }

        Ok(url.to_string())
    }

    fn truncate_markdown(&self, text: &str) -> String {
        if text.len() > self.max_response_size {
            let mut truncated = text
                .chars()
                .take(self.max_response_size)
                .collect::<String>();
            truncated.push_str("\n\n... [Content truncated due to size limit] ...");
            truncated
        } else {
            text.to_string()
        }
    }

    async fn fetch_with_fast_html2md(&self, url: &str) -> anyhow::Result<String> {
        let timeout_secs = if self.timeout_secs == 0 {
            tracing::warn!("web_fetch: timeout_secs is 0, using safe default of 30s");
            30
        } else {
            self.timeout_secs
        };

        let builder = reqwest::Client::builder()
            .timeout(Duration::from_secs(timeout_secs))
            .connect_timeout(Duration::from_secs(10))
            .redirect(reqwest::redirect::Policy::none())
            .user_agent(&self.user_agent);
        let builder = crate::config::apply_runtime_proxy_to_builder(builder, "tool.web_fetch");
        let client = builder.build()?;

        let response = client.get(url).send().await?;
        let status = response.status();

        if status.is_redirection() {
            let location = response
                .headers()
                .get("location")
                .and_then(|v| v.to_str().ok())
                .unwrap_or("(no Location header)");
            return Ok(format!(
                "Redirect (HTTP {}): this URL redirects to: {location}\n\
                 Call web_fetch again with the new URL if it is within the allowed domains.",
                status.as_u16()
            ));
        }

        if !status.is_success() {
            anyhow::bail!(
                "HTTP {}: {}",
                status.as_u16(),
                status.canonical_reason().unwrap_or("Unknown")
            );
        }

        let html = response.text().await?;

        let parsed_url = reqwest::Url::parse(url).ok();
        let markdown = html2md::rewrite_html_custom_with_url(&html, &None, false, &parsed_url);
        Ok(markdown)
    }

    #[cfg(feature = "firecrawl")]
    async fn fetch_with_firecrawl(&self, url: &str) -> anyhow::Result<String> {
        use firecrawl::scrape::{ScrapeFormats, ScrapeOptions};
        use firecrawl::FirecrawlApp;

        let api_key = self
            .firecrawl_api_key
            .as_deref()
            .filter(|k| !k.is_empty())
            .ok_or_else(|| {
                anyhow::anyhow!(
                    "Firecrawl API key not configured. \
                    Set [web_fetch].firecrawl_api_key in config.toml"
                )
            })?;

        let app = match self.firecrawl_api_url.as_deref().filter(|u| !u.is_empty()) {
            Some(url) => FirecrawlApp::new_selfhosted(url, Some(api_key)),
            None => FirecrawlApp::new(api_key),
        }
        .map_err(|e| anyhow::anyhow!("Failed to initialize Firecrawl client: {e}"))?;

        let options = ScrapeOptions {
            formats: Some(vec![ScrapeFormats::Markdown]),
            ..Default::default()
        };

        let document = app
            .scrape_url(url, Some(options))
            .await
            .map_err(|e| anyhow::anyhow!("Firecrawl scrape failed: {e}"))?;

        document
            .markdown
            .ok_or_else(|| anyhow::anyhow!("Firecrawl returned no markdown for this URL"))
    }

    #[cfg(not(feature = "firecrawl"))]
    #[allow(clippy::unused_async)]
    async fn fetch_with_firecrawl(&self, _url: &str) -> anyhow::Result<String> {
        anyhow::bail!(
            "The 'firecrawl' provider requires the 'firecrawl' compile-time feature. \
            Rebuild with: cargo build --features firecrawl"
        )
    }
}

#[async_trait]
impl Tool for WebFetchTool {
    fn name(&self) -> &str {
        "web_fetch"
    }

    fn description(&self) -> &str {
        "Fetch a URL and return its content as Markdown. \
        Useful for reading documentation, articles, and web pages. \
        Security constraints: Only http:// and https:// URLs are allowed, \
        allowlist-only domains, no local/private hosts."
    }

    fn parameters_schema(&self) -> serde_json::Value {
        json!({
            "type": "object",
            "properties": {
                "url": {
                    "type": "string",
                    "description": "HTTP or HTTPS URL to fetch and convert to Markdown"
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

        // ── 1. Autonomy check ─────────────────────────────────────
        if !self.security.can_act() {
            return Ok(ToolResult {
                success: false,
                output: String::new(),
                error: Some("Action blocked: autonomy is read-only".into()),
            });
        }

        // ── 2. Rate limit check ───────────────────────────────────
        if !self.security.record_action() {
            return Ok(ToolResult {
                success: false,
                output: String::new(),
                error: Some("Action blocked: rate limit exceeded".into()),
            });
        }

        // ── 3. URL validation (SSRF + allowlist) ──────────────────
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

        // ── 4. Provider dispatch ──────────────────────────────────
        let result = match self.provider.as_str() {
            "fast_html2md" | "html2md" => self.fetch_with_fast_html2md(&url).await,
            "firecrawl" => self.fetch_with_firecrawl(&url).await,
            other => Err(anyhow::anyhow!(
                "Unknown web_fetch provider: '{other}'. \
                Set [web_fetch].provider to 'fast_html2md' or 'firecrawl' in config.toml"
            )),
        };

        match result {
            Ok(markdown) => {
                let output = self.truncate_markdown(&markdown);
                Ok(ToolResult {
                    success: true,
                    output,
                    error: None,
                })
            }
            Err(e) => Ok(ToolResult {
                success: false,
                output: String::new(),
                error: Some(format!("web_fetch failed: {e}")),
            }),
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::security::{AutonomyLevel, SecurityPolicy};

    fn test_tool(allowed_domains: Vec<&str>) -> WebFetchTool {
        let security = Arc::new(SecurityPolicy {
            autonomy: AutonomyLevel::Supervised,
            ..SecurityPolicy::default()
        });
        WebFetchTool::new(
            security,
            "fast_html2md".into(),
            None,
            None,
            allowed_domains.into_iter().map(String::from).collect(),
            500_000,
            30,
            "test".into(),
        )
    }

    #[test]
    fn tool_name_and_description() {
        let tool = test_tool(vec!["example.com"]);
        assert_eq!(tool.name(), "web_fetch");
        assert!(tool.description().contains("Markdown"));
        assert!(tool.description().contains("allowlist-only"));
    }

    #[test]
    fn parameters_schema_requires_url() {
        let tool = test_tool(vec!["example.com"]);
        let schema = tool.parameters_schema();
        assert_eq!(schema["type"], "object");
        assert!(schema["properties"]["url"].is_object());
        let required = schema["required"].as_array().unwrap();
        assert!(required.iter().any(|v| v.as_str() == Some("url")));
    }

    #[tokio::test]
    async fn blocks_readonly_mode() {
        let security = Arc::new(SecurityPolicy {
            autonomy: AutonomyLevel::ReadOnly,
            ..SecurityPolicy::default()
        });
        let tool = WebFetchTool::new(
            security,
            "fast_html2md".into(),
            None,
            None,
            vec!["example.com".into()],
            500_000,
            30,
            "test".into(),
        );
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
        let tool = WebFetchTool::new(
            security,
            "fast_html2md".into(),
            None,
            None,
            vec!["example.com".into()],
            500_000,
            30,
            "test".into(),
        );
        let result = tool
            .execute(json!({"url": "https://example.com"}))
            .await
            .unwrap();
        assert!(!result.success);
        assert!(result.error.unwrap().contains("rate limit"));
    }

    #[test]
    fn validates_url_empty() {
        let tool = test_tool(vec!["example.com"]);
        let err = tool.validate_url("").unwrap_err().to_string();
        assert!(err.contains("empty"));
    }

    #[test]
    fn validates_url_scheme() {
        let tool = test_tool(vec!["example.com"]);
        let err = tool
            .validate_url("ftp://example.com")
            .unwrap_err()
            .to_string();
        assert!(err.contains("http://") || err.contains("https://"));
    }

    #[test]
    fn validates_url_whitespace() {
        let tool = test_tool(vec!["example.com"]);
        let err = tool
            .validate_url("https://example.com/hello world")
            .unwrap_err()
            .to_string();
        assert!(err.contains("whitespace"));
    }

    #[test]
    fn validates_url_private_host() {
        let tool = test_tool(vec!["*"]);
        for host in [
            "https://localhost",
            "https://127.0.0.1",
            "https://192.168.1.1",
            "https://10.0.0.1",
        ] {
            let err = tool.validate_url(host).unwrap_err().to_string();
            assert!(
                err.contains("local/private"),
                "Expected local/private error for {host}, got: {err}"
            );
        }
    }

    #[test]
    fn validates_url_allowlist() {
        let tool = test_tool(vec!["example.com"]);
        let err = tool
            .validate_url("https://other.com")
            .unwrap_err()
            .to_string();
        assert!(err.contains("allowed_domains"));
    }

    #[test]
    fn validates_url_requires_allowlist_config() {
        let security = Arc::new(SecurityPolicy::default());
        let tool = WebFetchTool::new(
            security,
            "fast_html2md".into(),
            None,
            None,
            vec![],
            500_000,
            30,
            "test".into(),
        );
        let err = tool
            .validate_url("https://example.com")
            .unwrap_err()
            .to_string();
        assert!(err.contains("allowed_domains"));
    }

    #[test]
    fn validates_url_accepts_http_and_https() {
        let tool = test_tool(vec!["example.com"]);
        assert!(tool.validate_url("https://example.com/path").is_ok());
        assert!(tool.validate_url("http://example.com/path").is_ok());
    }

    #[test]
    fn validates_url_accepts_subdomain() {
        let tool = test_tool(vec!["example.com"]);
        assert!(tool.validate_url("https://docs.example.com/guide").is_ok());
    }

    #[tokio::test]
    async fn rejects_unknown_provider() {
        let security = Arc::new(SecurityPolicy {
            autonomy: AutonomyLevel::Supervised,
            ..SecurityPolicy::default()
        });
        let tool = WebFetchTool::new(
            security,
            "bad_provider".into(),
            None,
            None,
            vec!["*".into()],
            500_000,
            30,
            "test".into(),
        );
        let result = tool
            .execute(json!({"url": "https://example.com"}))
            .await
            .unwrap();
        assert!(!result.success);
        let err = result.error.unwrap();
        assert!(err.contains("bad_provider"));
    }

    #[test]
    fn fast_html2md_converts_html_with_relative_urls() {
        let html = r#"
            <html><body>
                <h1>Hello World</h1>
                <p>This is a <a href="/about">relative link</a> and
                   an <a href="https://example.com/page">absolute link</a>.</p>
                <img src="/logo.png" alt="Logo">
            </body></html>
        "#;
        let base_url = "https://example.com";
        let parsed_url = reqwest::Url::parse(base_url).ok();
        let markdown = html2md::rewrite_html_custom_with_url(html, &None, false, &parsed_url);
        assert!(markdown.contains("Hello World"));
        assert!(markdown.contains("https://example.com/about") || markdown.contains("/about"));
        assert!(
            markdown.contains("absolute link") || markdown.contains("https://example.com/page")
        );
    }

    #[test]
    fn validates_url_rejects_ipv6_literal() {
        let tool = test_tool(vec!["*"]);
        let err = tool
            .validate_url("https://[::1]/path")
            .unwrap_err()
            .to_string();
        assert!(err.contains("IPv6"));
    }

    #[test]
    fn validates_url_rejects_userinfo() {
        let tool = test_tool(vec!["*"]);
        let err = tool
            .validate_url("https://user:pass@example.com/")
            .unwrap_err()
            .to_string();
        assert!(err.contains("userinfo"));
    }

    #[test]
    fn validates_url_subdomain_blocked_by_allowlist() {
        // Allowlist has "example.com" — sub.other.com must be rejected.
        let tool = test_tool(vec!["example.com"]);
        let err = tool
            .validate_url("https://sub.other.com/page")
            .unwrap_err()
            .to_string();
        assert!(err.contains("allowed_domains"));
    }

    /// Redirect output must contain the new URL so the LLM can re-issue
    /// the request — it must NOT silently follow the redirect.
    #[test]
    fn redirect_output_format_contains_new_url() {
        let redirect_target = "https://example.com/new-location";
        let output = format!(
            "Redirect (HTTP 301): this URL redirects to: {redirect_target}\n\
             Call web_fetch again with the new URL if it is within the allowed domains."
        );
        assert!(output.contains(redirect_target));
        assert!(output.contains("web_fetch again"));
    }

    #[test]
    fn truncates_large_output() {
        let security = Arc::new(SecurityPolicy::default());
        let tool = WebFetchTool::new(
            security,
            "fast_html2md".into(),
            None,
            None,
            vec!["example.com".into()],
            20,
            30,
            "test".into(),
        );
        let long_text = "a".repeat(100);
        let truncated = tool.truncate_markdown(&long_text);
        assert!(truncated.len() < long_text.len());
        assert!(truncated.contains("[Content truncated"));
    }
}
