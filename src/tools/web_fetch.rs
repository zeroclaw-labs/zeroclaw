use super::traits::{Tool, ToolResult};
use super::url_validation::{
    normalize_allowed_domains, validate_url, DomainPolicy, UrlSchemePolicy,
};
use crate::config::UrlAccessConfig;
use crate::security::SecurityPolicy;
use async_trait::async_trait;
use serde_json::json;
use std::sync::atomic::{AtomicUsize, Ordering};
use std::sync::Arc;
use std::time::Duration;

/// Canonical provider list for error messages and the tool description.
/// `fast_html2md` is kept as a deprecated alias for `nanohtml2text`.
const WEB_FETCH_PROVIDER_HELP: &str =
    "Supported providers: 'nanohtml2text' (default), 'firecrawl', 'tavily'. \
     Deprecated alias: 'fast_html2md' (maps to 'nanohtml2text').";

/// Web fetch tool: fetches a web page and returns text/markdown content for LLM consumption.
///
/// Providers:
/// - `nanohtml2text` (default): fetch with reqwest, strip noise elements, convert HTML to plaintext
/// - `fast_html2md` (deprecated alias): same as nanohtml2text unless `web-fetch-html2md` feature is compiled in
/// - `firecrawl`: fetch using Firecrawl cloud/self-hosted API
/// - `tavily`: fetch using Tavily Extract API
pub struct WebFetchTool {
    security: Arc<SecurityPolicy>,
    provider: String,
    api_keys: Vec<String>,
    api_url: Option<String>,
    allowed_domains: Vec<String>,
    blocked_domains: Vec<String>,
    url_access: UrlAccessConfig,
    max_response_size: usize,
    timeout_secs: u64,
    user_agent: String,
    key_index: Arc<AtomicUsize>,
}

impl WebFetchTool {
    #[allow(clippy::too_many_arguments)]
    /// Creates a new `WebFetchTool`. `api_key` accepts comma-separated values for round-robin rotation.
    pub fn new(
        security: Arc<SecurityPolicy>,
        provider: String,
        api_key: Option<String>,
        api_url: Option<String>,
        allowed_domains: Vec<String>,
        blocked_domains: Vec<String>,
        url_access: UrlAccessConfig,
        max_response_size: usize,
        timeout_secs: u64,
        user_agent: String,
    ) -> Self {
        let provider = provider.trim().to_lowercase();
        let api_keys = api_key
            .as_ref()
            .map(|raw| {
                raw.split(',')
                    .map(str::trim)
                    .filter(|s| !s.is_empty())
                    .map(ToOwned::to_owned)
                    .collect()
            })
            .unwrap_or_default();
        Self {
            security,
            provider: if provider.is_empty() {
                "nanohtml2text".to_string()
            } else {
                provider
            },
            api_keys,
            api_url,
            allowed_domains: normalize_allowed_domains(allowed_domains),
            blocked_domains: normalize_allowed_domains(blocked_domains),
            url_access,
            max_response_size,
            timeout_secs,
            user_agent,
            key_index: Arc::new(AtomicUsize::new(0)),
        }
    }

    /// Returns the next API key from the rotation pool using round-robin, or `None` if unconfigured.
    fn get_next_api_key(&self) -> Option<String> {
        if self.api_keys.is_empty() {
            return None;
        }
        let idx = self.key_index.fetch_add(1, Ordering::Relaxed) % self.api_keys.len();
        Some(self.api_keys[idx].clone())
    }

    /// Validates and normalises a URL against the allowlist, blocklist, and SSRF policy.
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
                url_access: Some(&self.url_access),
            },
        )
    }

    /// Truncates text to `max_response_size` characters and appends a marker if trimmed.
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

    /// Returns the configured timeout, substituting a safe 30 s default if zero is set.
    fn effective_timeout_secs(&self) -> u64 {
        if self.timeout_secs == 0 {
            tracing::warn!("web_fetch: timeout_secs is 0, using safe default of 30s");
            30
        } else {
            self.timeout_secs
        }
    }

    /// Strips noisy structural HTML elements (nav, scripts, footers, etc.) before text
    /// extraction to reduce boilerplate in the LLM output.
    fn strip_noise_elements(html: &str) -> anyhow::Result<String> {
        // Rust regex does not support backreferences, so run one pass per tag.
        // OnceLock stores Result<_, String> so that a compile failure is surfaced as an
        // error rather than a panic. String is used instead of anyhow::Error because it
        // is Clone + Sync, which OnceLock requires.
        use std::sync::OnceLock;
        static NOISE_RES: OnceLock<Result<Vec<regex::Regex>, String>> = OnceLock::new();
        let regexes = NOISE_RES
            .get_or_init(|| {
                [
                    "script", "style", "nav", "header", "footer", "aside", "noscript", "form",
                    "button",
                ]
                .iter()
                .map(|tag| {
                    regex::Regex::new(&format!(r"(?si)<{tag}[^>]*>.*?</{tag}>"))
                        .map_err(|e| e.to_string())
                })
                .collect::<Result<Vec<_>, _>>()
            })
            .as_ref()
            .map_err(|e| anyhow::anyhow!("noise regex init failed: {e}"))?;
        let mut result = html.to_string();
        for re in regexes {
            result = re.replace_all(&result, " ").into_owned();
        }
        Ok(result)
    }

    /// Strips noise elements then converts HTML to plain text using the configured provider.
    /// `fast_html2md` is a deprecated alias that maps to `nanohtml2text` when the
    /// `web-fetch-html2md` feature is not compiled in.
    fn convert_html_to_output(&self, body: &str) -> anyhow::Result<String> {
        let cleaned = Self::strip_noise_elements(body)?;
        match self.provider.as_str() {
            "fast_html2md" => {
                #[cfg(feature = "web-fetch-html2md")]
                {
                    Ok(html2md::rewrite_html(&cleaned, false))
                }
                #[cfg(not(feature = "web-fetch-html2md"))]
                {
                    // Feature not compiled in; fall through to nanohtml2text.
                    Ok(nanohtml2text::html2text(&cleaned))
                }
            }
            "nanohtml2text" => Ok(nanohtml2text::html2text(&cleaned)),
            _ => anyhow::bail!(
                "Unknown web_fetch provider: '{}'. {}",
                self.provider,
                WEB_FETCH_PROVIDER_HELP
            ),
        }
    }

    /// Builds a `reqwest::Client` with the configured timeout, user-agent, and proxy settings.
    fn build_http_client(&self) -> anyhow::Result<reqwest::Client> {
        let builder = reqwest::Client::builder()
            .timeout(Duration::from_secs(self.effective_timeout_secs()))
            .connect_timeout(Duration::from_secs(10))
            .redirect(reqwest::redirect::Policy::none())
            .user_agent(self.user_agent.as_str());
        let builder = crate::config::apply_runtime_proxy_to_builder(builder, "tool.web_fetch");
        Ok(builder.build()?)
    }

    /// Fetches `url` with reqwest, handles one redirect (re-validated), and converts the
    /// response body to text via the configured HTML provider.
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

    /// Fetches `url` via the Firecrawl scrape API and returns the extracted markdown content.
    #[cfg(feature = "firecrawl")]
    async fn fetch_with_firecrawl(&self, url: &str) -> anyhow::Result<String> {
        let auth_token = self.get_next_api_key().ok_or_else(|| {
            anyhow::anyhow!(
                "web_fetch provider 'firecrawl' requires [web_fetch].api_key in config.toml"
            )
        })?;

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
                format!("Bearer {}", auth_token),
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

    /// Fetches `url` via the Tavily Extract API and returns the raw extracted content.
    async fn fetch_with_tavily(&self, url: &str) -> anyhow::Result<String> {
        let api_key = self.get_next_api_key().ok_or_else(|| {
            anyhow::anyhow!(
                "web_fetch provider 'tavily' requires [web_fetch].api_key in config.toml"
            )
        })?;

        let api_url = self
            .api_url
            .as_deref()
            .map(str::trim)
            .filter(|s| !s.is_empty())
            .unwrap_or("https://api.tavily.com");
        let endpoint = format!("{}/extract", api_url.trim_end_matches('/'));

        let response = self
            .build_http_client()?
            .post(endpoint)
            .json(&json!({
                "api_key": api_key,
                "urls": [url]
            }))
            .send()
            .await?;
        let status = response.status();
        let body = response.text().await?;

        if !status.is_success() {
            anyhow::bail!(
                "Tavily extract failed with status {}: {}",
                status.as_u16(),
                body
            );
        }

        let parsed: serde_json::Value = serde_json::from_str(&body)
            .map_err(|e| anyhow::anyhow!("Invalid Tavily response JSON: {e}"))?;
        if let Some(error) = parsed.get("error").and_then(serde_json::Value::as_str) {
            anyhow::bail!("Tavily API error: {error}");
        }

        let results = parsed
            .get("results")
            .and_then(serde_json::Value::as_array)
            .ok_or_else(|| anyhow::anyhow!("Tavily response missing results array"))?;
        if results.is_empty() {
            anyhow::bail!("Tavily returned no results for URL: {}", url);
        }

        let result = &results[0];
        let output = result
            .get("raw_content")
            .and_then(serde_json::Value::as_str)
            .or_else(|| result.get("content").and_then(serde_json::Value::as_str))
            .unwrap_or("")
            .trim()
            .to_string();

        if output.is_empty() {
            anyhow::bail!("Tavily returned empty content for URL: {}", url);
        }

        Ok(output)
    }
}

#[async_trait]
impl Tool for WebFetchTool {
    fn name(&self) -> &str {
        "web_fetch"
    }

    fn description(&self) -> &str {
        "Fetch a web page and return text content for LLM consumption. Strips navigation, scripts, and boilerplate before extraction. Providers: nanohtml2text (default), firecrawl, tavily. Deprecated alias: fast_html2md. Security: allowlist-only domains, blocked_domains, and no local/private hosts."
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
            "tavily" => self.fetch_with_tavily(&url).await,
            _ => Err(anyhow::anyhow!(
                "Unknown web_fetch provider: '{}'. {}",
                self.provider,
                WEB_FETCH_PROVIDER_HELP
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
            UrlAccessConfig::default(),
            500_000,
            30,
            "ZeroClaw/1.0".to_string(),
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

    // Previously gated on cfg(feature = "web-fetch-html2md") / cfg(feature = "web-fetch-plaintext")
    // — neither feature was declared in Cargo.toml so these tests never ran.
    // Now always-on: fast_html2md falls back to nanohtml2text when uncompiled.
    #[test]
    fn html_conversion_removes_tags() {
        let tool = test_tool(vec!["example.com"]);
        let html = "<html><body><h1>Title</h1><p>Hello <b>world</b></p></body></html>";
        let text = tool.convert_html_to_output(html).unwrap();
        assert!(text.contains("Title"));
        assert!(text.contains("Hello"));
        assert!(!text.contains("<h1>"));
    }

    #[test]
    fn strip_noise_removes_nav_scripts_footer() {
        let tool = test_tool(vec!["example.com"]);
        let html = "<html><body>\
            <nav><a>Home</a><a>Menu</a></nav>\
            <script>var x = 1;</script>\
            <article><p>Real content here</p></article>\
            <footer>Copyright 2025</footer>\
            </body></html>";
        let text = tool.convert_html_to_output(html).unwrap();
        assert!(text.contains("Real content"));
        assert!(!text.contains("var x"));
        assert!(!text.contains("Copyright 2025"));
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
            UrlAccessConfig::default(),
            500_000,
            30,
            "test".to_string(),
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
            UrlAccessConfig::default(),
            500_000,
            30,
            "test".to_string(),
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
            UrlAccessConfig::default(),
            500_000,
            30,
            "test".to_string(),
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
            UrlAccessConfig::default(),
            10,
            30,
            "test".to_string(),
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

    #[tokio::test]
    async fn tavily_provider_requires_api_key() {
        let tool = test_tool_with_provider(vec!["*"], vec![], "tavily", None, None);
        let result = tool
            .execute(json!({"url": "https://example.com"}))
            .await
            .unwrap();
        assert!(!result.success);
        let error = result.error.unwrap_or_default();
        assert!(error.contains("requires [web_fetch].api_key"));
    }

    #[test]
    fn parses_multiple_api_keys() {
        let tool =
            test_tool_with_provider(vec!["*"], vec![], "tavily", Some("key1,key2,key3"), None);
        assert_eq!(tool.api_keys, vec!["key1", "key2", "key3"]);
    }

    #[test]
    fn round_robin_api_key_selection_cycles() {
        let tool = test_tool_with_provider(vec!["*"], vec![], "tavily", Some("k1,k2"), None);
        assert_eq!(tool.get_next_api_key().as_deref(), Some("k1"));
        assert_eq!(tool.get_next_api_key().as_deref(), Some("k2"));
        assert_eq!(tool.get_next_api_key().as_deref(), Some("k1"));
    }
}
