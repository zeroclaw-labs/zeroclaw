use super::traits::{Tool, ToolResult};
use super::url_validation::{
    normalize_allowed_domains, validate_url, DomainPolicy, UrlSchemePolicy,
};
use crate::security::SecurityPolicy;
use async_trait::async_trait;
use serde_json::json;
use std::sync::Arc;
use std::time::Duration;

/// Web fetch tool: fetches a web page and returns text/markdown content for LLM consumption.
///
/// Providers:
/// - `fast_html2md`: fetch with reqwest, convert HTML to markdown
/// - `nanohtml2text`: fetch with reqwest, convert HTML to plaintext
/// - `firecrawl`: fetch using Firecrawl cloud/self-hosted API
pub struct WebFetchTool {
    security: Arc<SecurityPolicy>,
    provider: String,
    api_key: Option<String>,
    api_url: Option<String>,
    allowed_domains: Vec<String>,
    blocked_domains: Vec<String>,
    max_response_size: usize,
    timeout_secs: u64,
}

impl WebFetchTool {
    #[allow(clippy::too_many_arguments)]
    pub fn new(
        security: Arc<SecurityPolicy>,
        provider: String,
        api_key: Option<String>,
        api_url: Option<String>,
        allowed_domains: Vec<String>,
        blocked_domains: Vec<String>,
        max_response_size: usize,
        timeout_secs: u64,
    ) -> Self {
        let provider = provider.trim().to_lowercase();
        Self {
            security,
            provider: if provider.is_empty() {
                "fast_html2md".to_string()
            } else {
                provider
            },
            api_key,
            api_url,
            allowed_domains: normalize_allowed_domains(allowed_domains),
            blocked_domains: normalize_allowed_domains(blocked_domains),
            max_response_size,
            timeout_secs,
        }
    }

    fn validate_url(&self, raw_url: &str) -> anyhow::Result<String> {
        validate_url(
            raw_url,
            &DomainPolicy {
                allowed_domains: &self.allowed_domains,
                blocked_domains: &self.blocked_domains,
                allowed_field_name: "web_fetch.allowed_domains",
                blocked_field_name: Some("web_fetch.blocked_domains"),
                empty_allowed_message: "web_fetch tool is enabled but no allowed_domains are configured. Add [web_fetch].allowed_domains in config.toml",
                scheme_policy: UrlSchemePolicy::HttpOrHttps,
                ipv6_error_context: "web_fetch",
            },
        )
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

    fn effective_timeout_secs(&self) -> u64 {
        if self.timeout_secs == 0 {
            tracing::warn!("web_fetch: timeout_secs is 0, using safe default of 30s");
            30
        } else {
            self.timeout_secs
        }
    }

    #[allow(unused_variables)]
    fn convert_html_to_output(&self, body: &str) -> anyhow::Result<String> {
        match self.provider.as_str() {
            "fast_html2md" => {
                #[cfg(feature = "web-fetch-html2md")]
                {
                    Ok(html2md::rewrite_html(body, false))
                }
                #[cfg(not(feature = "web-fetch-html2md"))]
                {
                    anyhow::bail!(
                        "web_fetch provider 'fast_html2md' requires Cargo feature 'web-fetch-html2md'"
                    );
                }
            }
            "nanohtml2text" => {
                #[cfg(feature = "web-fetch-plaintext")]
                {
                    Ok(nanohtml2text::html2text(body))
                }
                #[cfg(not(feature = "web-fetch-plaintext"))]
                {
                    anyhow::bail!(
                        "web_fetch provider 'nanohtml2text' requires Cargo feature 'web-fetch-plaintext'"
                    );
                }
            }
            _ => anyhow::bail!(
                "Unknown web_fetch provider: '{}'. Set tools.web_fetch.provider to 'fast_html2md', 'nanohtml2text', or 'firecrawl' in config.toml",
                self.provider
            ),
        }
    }

    fn build_http_client(&self) -> anyhow::Result<reqwest::Client> {
        let builder = reqwest::Client::builder()
            .timeout(Duration::from_secs(self.effective_timeout_secs()))
            .connect_timeout(Duration::from_secs(10))
            .redirect(reqwest::redirect::Policy::none())
            .user_agent("ZeroClaw/0.1 (web_fetch)");
        let builder = crate::config::apply_runtime_proxy_to_builder(builder, "tool.web_fetch");
        Ok(builder.build()?)
    }

    async fn fetch_with_http_provider(&self, url: &str) -> anyhow::Result<String> {
        let client = self.build_http_client()?;
        let response = client.get(url).send().await?;

        if response.status().is_redirection() {
            let location = response
                .headers()
                .get(reqwest::header::LOCATION)
                .and_then(|v| v.to_str().ok())
                .ok_or_else(|| anyhow::anyhow!("Redirect response missing Location header"))?;

            let redirected_url = reqwest::Url::parse(url)
                .and_then(|base| base.join(location))
                .or_else(|_| reqwest::Url::parse(location))
                .map_err(|e| anyhow::anyhow!("Invalid redirect Location header: {e}"))?
                .to_string();

            // Validate redirect target with the same SSRF/allowlist policy.
            self.validate_url(&redirected_url)?;
            return Ok(redirected_url);
        }

        let status = response.status();
        if !status.is_success() {
            anyhow::bail!(
                "HTTP {} {}",
                status.as_u16(),
                status.canonical_reason().unwrap_or("Unknown")
            );
        }

        let content_type = response
            .headers()
            .get(reqwest::header::CONTENT_TYPE)
            .and_then(|v| v.to_str().ok())
            .unwrap_or("")
            .to_lowercase();

        let body = response.text().await?;

        if content_type.contains("text/plain")
            || content_type.contains("text/markdown")
            || content_type.contains("application/json")
        {
            return Ok(body);
        }

        if content_type.contains("text/html") || content_type.is_empty() {
            return self.convert_html_to_output(&body);
        }

        anyhow::bail!(
            "Unsupported content type: {content_type}. web_fetch supports text/html, text/plain, text/markdown, and application/json."
        )
    }

    #[cfg(feature = "firecrawl")]
    async fn fetch_with_firecrawl(&self, url: &str) -> anyhow::Result<String> {
        let auth_token = match self.api_key.as_ref() {
            Some(raw) if !raw.trim().is_empty() => raw.trim(),
            _ => {
                anyhow::bail!(
                    "web_fetch provider 'firecrawl' requires [web_fetch].api_key in config.toml"
                );
            }
        };

        let api_url = self
            .api_url
            .as_deref()
            .map(str::trim)
            .filter(|s| !s.is_empty())
            .unwrap_or("https://api.firecrawl.dev");
        let endpoint = format!("{}/v1/scrape", api_url.trim_end_matches('/'));

        let response = self
            .build_http_client()?
            .post(endpoint)
            .header(
                reqwest::header::AUTHORIZATION,
                format!("Bearer {auth_token}"),
            )
            .json(&json!({
                "url": url,
                "formats": ["markdown"],
                "onlyMainContent": true,
                "timeout": (self.effective_timeout_secs() * 1000) as u64
            }))
            .send()
            .await?;
        let status = response.status();
        let body = response.text().await?;

        if !status.is_success() {
            anyhow::bail!(
                "Firecrawl scrape failed with status {}: {}",
                status.as_u16(),
                body
            );
        }

        let parsed: serde_json::Value = serde_json::from_str(&body)
            .map_err(|e| anyhow::anyhow!("Invalid Firecrawl response JSON: {e}"))?;
        if !parsed
            .get("success")
            .and_then(serde_json::Value::as_bool)
            .unwrap_or(false)
        {
            let error = parsed
                .get("error")
                .and_then(serde_json::Value::as_str)
                .unwrap_or("unknown error");
            anyhow::bail!("Firecrawl scrape failed: {error}");
        }

        let data = parsed
            .get("data")
            .ok_or_else(|| anyhow::anyhow!("Firecrawl response missing data field"))?;
        let output = data
            .get("markdown")
            .and_then(serde_json::Value::as_str)
            .or_else(|| data.get("html").and_then(serde_json::Value::as_str))
            .or_else(|| data.get("rawHtml").and_then(serde_json::Value::as_str))
            .unwrap_or("")
            .to_string();

        if output.trim().is_empty() {
            anyhow::bail!("Firecrawl returned empty content");
        }

        Ok(output)
    }

    #[cfg(not(feature = "firecrawl"))]
    #[allow(clippy::unused_async)]
    async fn fetch_with_firecrawl(&self, _url: &str) -> anyhow::Result<String> {
        anyhow::bail!("web_fetch provider 'firecrawl' requires Cargo feature 'firecrawl'")
    }
}

#[async_trait]
impl Tool for WebFetchTool {
    fn name(&self) -> &str {
        "web_fetch"
    }

    fn description(&self) -> &str {
        "Fetch a web page and return markdown/text content for LLM consumption. Providers: fast_html2md, nanohtml2text, firecrawl. Security: allowlist-only domains, blocked_domains, and no local/private hosts."
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
                });
            }
        };

        let result = match self.provider.as_str() {
            "fast_html2md" | "nanohtml2text" => self.fetch_with_http_provider(&url).await,
            "firecrawl" => self.fetch_with_firecrawl(&url).await,
            _ => Err(anyhow::anyhow!(
                "Unknown web_fetch provider: '{}'. Set tools.web_fetch.provider to 'fast_html2md', 'nanohtml2text', or 'firecrawl' in config.toml",
                self.provider
            )),
        };

        match result {
            Ok(output) => Ok(ToolResult {
                success: true,
                output: self.truncate_response(&output),
                error: None,
            }),
            Err(e) => Ok(ToolResult {
                success: false,
                output: String::new(),
                error: Some(e.to_string()),
            }),
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::security::{AutonomyLevel, SecurityPolicy};
    use crate::tools::url_validation::{is_private_or_local_host, normalize_domain};

    fn test_tool(allowed_domains: Vec<&str>) -> WebFetchTool {
        test_tool_with_provider(allowed_domains, vec![], "fast_html2md", None, None)
    }

    fn test_tool_with_blocklist(
        allowed_domains: Vec<&str>,
        blocked_domains: Vec<&str>,
    ) -> WebFetchTool {
        test_tool_with_provider(allowed_domains, blocked_domains, "fast_html2md", None, None)
    }

    fn test_tool_with_provider(
        allowed_domains: Vec<&str>,
        blocked_domains: Vec<&str>,
        provider: &str,
        provider_key: Option<&str>,
        api_url: Option<&str>,
    ) -> WebFetchTool {
        let security = Arc::new(SecurityPolicy {
            autonomy: AutonomyLevel::Supervised,
            ..SecurityPolicy::default()
        });
        WebFetchTool::new(
            security,
            provider.to_string(),
            provider_key.map(ToOwned::to_owned),
            api_url.map(ToOwned::to_owned),
            allowed_domains.into_iter().map(String::from).collect(),
            blocked_domains.into_iter().map(String::from).collect(),
            500_000,
            30,
        )
    }

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

    #[cfg(feature = "web-fetch-html2md")]
    #[test]
    fn html_to_markdown_conversion_preserves_structure() {
        let tool = test_tool(vec!["example.com"]);
        let html = "<html><body><h1>Title</h1><ul><li>Hello</li></ul></body></html>";
        let markdown = tool.convert_html_to_output(html).unwrap();
        assert!(markdown.contains("Title"));
        assert!(markdown.contains("Hello"));
        assert!(!markdown.contains("<h1>"));
    }

    #[cfg(feature = "web-fetch-plaintext")]
    #[test]
    fn html_to_plaintext_conversion_removes_html_tags() {
        let tool =
            test_tool_with_provider(vec!["example.com"], vec![], "nanohtml2text", None, None);
        let html = "<html><body><h1>Title</h1><p>Hello <b>world</b></p></body></html>";
        let text = tool.convert_html_to_output(html).unwrap();
        assert!(text.contains("Title"));
        assert!(text.contains("Hello"));
        assert!(!text.contains("<h1>"));
    }

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
        let tool = WebFetchTool::new(
            security,
            "fast_html2md".into(),
            None,
            None,
            vec![],
            vec![],
            500_000,
            30,
        );
        let err = tool
            .validate_url("https://example.com")
            .unwrap_err()
            .to_string();
        assert!(err.contains("allowed_domains"));
    }

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
            vec![],
            500_000,
            30,
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
            vec![],
            500_000,
            30,
        );
        let result = tool
            .execute(json!({"url": "https://example.com"}))
            .await
            .unwrap();
        assert!(!result.success);
        assert!(result.error.unwrap().contains("rate limit"));
    }

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
            "fast_html2md".into(),
            None,
            None,
            vec!["example.com".into()],
            vec![],
            10,
            30,
        );
        let text = "hello world this is long";
        let truncated = tool.truncate_response(text);
        assert!(truncated.contains("[Response truncated"));
    }

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

    #[tokio::test]
    async fn firecrawl_provider_requires_api_key() {
        let tool = test_tool_with_provider(vec!["*"], vec![], "firecrawl", None, None);
        let result = tool
            .execute(json!({"url": "https://example.com"}))
            .await
            .unwrap();
        assert!(!result.success);
        let error = result.error.unwrap_or_default();
        if cfg!(feature = "firecrawl") {
            assert!(error.contains("requires [web_fetch].api_key"));
        } else {
            assert!(error.contains("requires Cargo feature 'firecrawl'"));
        }
    }
}
