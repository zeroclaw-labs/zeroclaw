use super::traits::{Tool, ToolResult};
use crate::security::SecurityPolicy;
use async_trait::async_trait;
use regex::Regex;
use serde_json::json;
use std::sync::atomic::{AtomicUsize, Ordering};
use std::sync::Arc;
use std::time::Duration;

/// Web search tool for searching the internet.
#[cfg_attr(
    feature = "firecrawl",
    doc = "Supports providers: DuckDuckGo (free), Bing (free), Brave, Firecrawl, Tavily."
)]
#[cfg_attr(
    not(feature = "firecrawl"),
    doc = "Supports providers: DuckDuckGo (free), Bing (free), Brave, Tavily."
)]
pub struct WebSearchTool {
    security: Arc<SecurityPolicy>,
    provider: String,
    api_keys: Vec<String>,
    api_url: Option<String>,
    max_results: usize,
    timeout_secs: u64,
    user_agent: String,
    key_index: Arc<AtomicUsize>,
}

impl WebSearchTool {
    /// Create a new WebSearchTool instance.
    ///
    /// # Arguments
    /// * `security` - Security policy
    /// * `provider` - Search provider (duckduckgo, bing, brave, firecrawl, tavily)
    /// * `api_key` - API key (supports comma-separated multiple keys for round-robin)
    /// * `api_url` - Optional API URL override
    /// * `max_results` - Maximum number of results to return (1-10)
    /// * `timeout_secs` - Request timeout in seconds
    /// * `user_agent` - HTTP user agent string
    pub fn new(
        security: Arc<SecurityPolicy>,
        provider: String,
        api_key: Option<String>,
        api_url: Option<String>,
        max_results: usize,
        timeout_secs: u64,
        user_agent: String,
    ) -> Self {
        // Parse comma-separated API keys for round-robin support
        let api_keys = api_key
            .as_ref()
            .map(|keys| {
                keys.split(',')
                    .map(|k| k.trim().to_string())
                    .filter(|k| !k.is_empty())
                    .collect()
            })
            .unwrap_or_default();

        Self {
            security,
            provider: provider.trim().to_lowercase(),
            api_keys,
            api_url,
            max_results: max_results.clamp(1, 10),
            timeout_secs: timeout_secs.max(1),
            user_agent,
            key_index: Arc::new(AtomicUsize::new(0)),
        }
    }

    /// Get the next API key using round-robin strategy.
    /// Returns None if no keys are configured.
    fn get_next_api_key(&self) -> Option<String> {
        if self.api_keys.is_empty() {
            return None;
        }
        let idx = self.key_index.fetch_add(1, Ordering::Relaxed) % self.api_keys.len();
        Some(self.api_keys[idx].clone())
    }

    /// Perform web search using DuckDuckGo HTML interface (free, no API key required).
    ///
    /// # Arguments
    /// * `query` - The search query string
    ///
    /// # Returns
    /// Formatted search results with title, URL, and snippet
    async fn search_duckduckgo(&self, query: &str) -> anyhow::Result<String> {
        let encoded_query = urlencoding::encode(query);
        let search_url = format!("https://html.duckduckgo.com/html/?q={}", encoded_query);

        let client = reqwest::Client::builder()
            .timeout(Duration::from_secs(self.timeout_secs))
            .user_agent(self.user_agent.as_str())
            .build()?;

        let response = client.get(&search_url).send().await?;

        if !response.status().is_success() {
            anyhow::bail!(
                "DuckDuckGo search failed with status: {}",
                response.status()
            );
        }

        let html = response.text().await?;
        self.parse_duckduckgo_results(&html, query)
    }

    /// Parse HTML response from DuckDuckGo search into formatted results.
    ///
    /// # Arguments
    /// * `html` - The HTML response from DuckDuckGo
    /// * `query` - The original search query (for context)
    ///
    /// # Returns
    /// Formatted search results as a string
    fn parse_duckduckgo_results(&self, html: &str, query: &str) -> anyhow::Result<String> {
        // Extract result links: <a class="result__a" href="...">Title</a>
        let link_regex = Regex::new(
            r#"<a[^>]*class="[^"]*result__a[^"]*"[^>]*href="([^"]+)"[^>]*>([\s\S]*?)</a>"#,
        )?;

        // Extract snippets: <a class="result__snippet">...</a>
        let snippet_regex = Regex::new(r#"<a class="result__snippet[^"]*"[^>]*>([\s\S]*?)</a>"#)?;

        let link_matches: Vec<_> = link_regex
            .captures_iter(html)
            .take(self.max_results + 2)
            .collect();

        let snippet_matches: Vec<_> = snippet_regex
            .captures_iter(html)
            .take(self.max_results + 2)
            .collect();

        if link_matches.is_empty() {
            return Ok(format!("No results found for: {}", query));
        }

        let mut lines = vec![format!("Search results for: {} (via DuckDuckGo)", query)];

        let count = link_matches.len().min(self.max_results);

        for i in 0..count {
            let caps = &link_matches[i];
            let url_str = decode_ddg_redirect_url(&caps[1]);
            let title = strip_tags(&caps[2]);

            lines.push(format!("{}. {}", i + 1, title.trim()));
            lines.push(format!("   {}", url_str.trim()));

            // Add snippet if available
            if i < snippet_matches.len() {
                let snippet = strip_tags(&snippet_matches[i][1]);
                let snippet = snippet.trim();
                if !snippet.is_empty() {
                    lines.push(format!("   {}", snippet));
                }
            }
        }

        Ok(lines.join("\n"))
    }

    async fn search_brave(&self, query: &str) -> anyhow::Result<String> {
        let auth_token = self
            .get_next_api_key()
            .ok_or_else(|| anyhow::anyhow!("Brave API key not configured"))?;

        let encoded_query = urlencoding::encode(query);
        let search_url = format!(
            "https://api.search.brave.com/res/v1/web/search?q={}&count={}",
            encoded_query, self.max_results
        );

        let client = reqwest::Client::builder()
            .timeout(Duration::from_secs(self.timeout_secs))
            .user_agent(self.user_agent.as_str())
            .build()?;

        let response = client
            .get(&search_url)
            .header("Accept", "application/json")
            .header("X-Subscription-Token", auth_token)
            .send()
            .await?;

        if !response.status().is_success() {
            anyhow::bail!("Brave search failed with status: {}", response.status());
        }

        let json: serde_json::Value = response.json().await?;
        self.parse_brave_results(&json, query)
    }

    fn parse_brave_results(&self, json: &serde_json::Value, query: &str) -> anyhow::Result<String> {
        let results = json
            .get("web")
            .and_then(|w| w.get("results"))
            .and_then(|r| r.as_array())
            .ok_or_else(|| anyhow::anyhow!("Invalid Brave API response"))?;

        if results.is_empty() {
            return Ok(format!("No results found for: {}", query));
        }

        let mut lines = vec![format!("Search results for: {} (via Brave)", query)];

        for (i, result) in results.iter().take(self.max_results).enumerate() {
            let title = result
                .get("title")
                .and_then(|t| t.as_str())
                .unwrap_or("No title");
            let url = result.get("url").and_then(|u| u.as_str()).unwrap_or("");
            let description = result
                .get("description")
                .and_then(|d| d.as_str())
                .unwrap_or("");

            lines.push(format!("{}. {}", i + 1, title));
            lines.push(format!("   {}", url));
            if !description.is_empty() {
                lines.push(format!("   {}", description));
            }
        }

        Ok(lines.join("\n"))
    }

    async fn search_bing(&self, query: &str) -> anyhow::Result<String> {
        let encoded_query = urlencoding::encode(query);
        let search_url = format!(
            "https://www.bing.com/search?q={}&count={}",
            encoded_query, self.max_results
        );

        let client = reqwest::Client::builder()
            .timeout(Duration::from_secs(self.timeout_secs))
            .user_agent(self.user_agent.as_str())
            .build()?;

        let response = client.get(&search_url).send().await?;

        if !response.status().is_success() {
            anyhow::bail!("Bing search failed with status: {}", response.status());
        }

        let html = response.text().await?;
        self.parse_bing_results(&html, query)
    }

    fn parse_bing_results(&self, html: &str, query: &str) -> anyhow::Result<String> {
        // Extract item blocks from Bing result list.
        let item_regex =
            Regex::new(r#"<li[^>]*class="[^"]*\bb_algo\b[^"]*"[^>]*>([\s\S]*?)</li>"#)?;
        // Extract primary headline link from each block (`h2 > a`).
        let headline_link_regex = Regex::new(
            r#"<h2[^>]*>\s*<a[^>]*href="(https?://[^"]+)"[^>]*>([\s\S]*?)</a>\s*</h2>"#,
        )?;
        // Extract first snippet paragraph if available.
        let snippet_regex = Regex::new(r#"<p[^>]*>([\s\S]*?)</p>"#)?;

        let mut lines = vec![format!("Search results for: {} (via Bing)", query)];
        let mut found = 0;

        for block_caps in item_regex.captures_iter(html) {
            if found >= self.max_results {
                break;
            }

            let block = &block_caps[1];
            let Some(link_caps) = headline_link_regex.captures(block) else {
                continue;
            };

            let title = strip_tags(&link_caps[2]);
            let title = title.trim();
            if title.is_empty() {
                continue;
            }

            let url = link_caps[1].replace("&amp;", "&");
            found += 1;
            lines.push(format!("{}. {}", found, title));
            lines.push(format!("   {}", url.trim()));

            if let Some(snippet_caps) = snippet_regex.captures(block) {
                let snippet = strip_tags(&snippet_caps[1]);
                let snippet = snippet.trim();
                if !snippet.is_empty() {
                    lines.push(format!("   {}", snippet));
                }
            }
        }

        if found == 0 {
            return Ok(format!("No results found for: {}", query));
        }

        Ok(lines.join("\n"))
    }

    #[cfg(feature = "firecrawl")]
    async fn search_firecrawl(&self, query: &str) -> anyhow::Result<String> {
        let auth_token = self.get_next_api_key().ok_or_else(|| {
            anyhow::anyhow!(
                "web_search provider 'firecrawl' requires [web_search].api_key in config.toml"
            )
        })?;

        let api_url = self
            .api_url
            .as_deref()
            .map(str::trim)
            .filter(|s| !s.is_empty())
            .unwrap_or("https://api.firecrawl.dev");
        let endpoint = format!("{}/v1/search", api_url.trim_end_matches('/'));
        let client = reqwest::Client::builder()
            .timeout(Duration::from_secs(self.timeout_secs))
            .user_agent(self.user_agent.as_str())
            .build()?;

        let response = client
            .post(endpoint)
            .header(
                reqwest::header::AUTHORIZATION,
                format!("Bearer {auth_token}"),
            )
            .json(&json!({
                "query": query,
                "limit": self.max_results,
                "timeout": (self.timeout_secs * 1000) as u64,
            }))
            .send()
            .await
            .map_err(|e| anyhow::anyhow!("Firecrawl search failed: {e}"))?;
        let status = response.status();
        let body = response.text().await?;

        if !status.is_success() {
            anyhow::bail!(
                "Firecrawl search failed with status {}: {}",
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
            anyhow::bail!("Firecrawl search failed: {error}");
        }

        let results = parsed
            .get("data")
            .and_then(serde_json::Value::as_array)
            .ok_or_else(|| anyhow::anyhow!("Firecrawl response missing data array"))?;

        if results.is_empty() {
            return Ok(format!("No results found for: {}", query));
        }

        let mut lines = vec![format!("Search results for: {} (via Firecrawl)", query)];

        for (i, result) in results.iter().take(self.max_results).enumerate() {
            let title = result
                .get("title")
                .and_then(serde_json::Value::as_str)
                .unwrap_or("No title");
            let url = result
                .get("url")
                .and_then(serde_json::Value::as_str)
                .unwrap_or("");
            let description = result
                .get("description")
                .and_then(serde_json::Value::as_str)
                .unwrap_or("");

            lines.push(format!("{}. {}", i + 1, title));
            lines.push(format!("   {}", url));
            if !description.trim().is_empty() {
                lines.push(format!("   {}", description.trim()));
            }
        }

        Ok(lines.join("\n"))
    }

    #[cfg(not(feature = "firecrawl"))]
    #[allow(clippy::unused_async)]
    async fn search_firecrawl(&self, _query: &str) -> anyhow::Result<String> {
        anyhow::bail!("web_search provider 'firecrawl' requires Cargo feature 'firecrawl'")
    }

    /// Perform web search using Tavily Search API.
    ///
    /// # Arguments
    /// * `query` - The search query string
    ///
    /// # Returns
    /// Formatted search results with title, URL, and content snippets
    async fn search_tavily(&self, query: &str) -> anyhow::Result<String> {
        let api_key = self.get_next_api_key().ok_or_else(|| {
            anyhow::anyhow!(
                "web_search provider 'tavily' requires [web_search].api_key in config.toml"
            )
        })?;

        let api_url = self
            .api_url
            .as_deref()
            .map(str::trim)
            .filter(|s| !s.is_empty())
            .unwrap_or("https://api.tavily.com");

        let endpoint = format!("{}/search", api_url.trim_end_matches('/'));
        let client = reqwest::Client::builder()
            .timeout(Duration::from_secs(self.timeout_secs))
            .user_agent(self.user_agent.as_str())
            .build()?;

        let response = client
            .post(&endpoint)
            .json(&json!({
                "api_key": api_key,
                "query": query,
                "max_results": self.max_results,
                "search_depth": "basic",
                "include_answer": false,
                "include_raw_content": false,
                "include_images": false,
            }))
            .send()
            .await
            .map_err(|e| anyhow::anyhow!("Tavily search failed: {e}"))?;

        let status = response.status();
        let body = response.text().await?;

        if !status.is_success() {
            anyhow::bail!(
                "Tavily search failed with status {}: {}",
                status.as_u16(),
                body
            );
        }

        let parsed: serde_json::Value = serde_json::from_str(&body)
            .map_err(|e| anyhow::anyhow!("Invalid Tavily response JSON: {e}"))?;

        // Check for API error in response
        if let Some(error) = parsed.get("error").and_then(|e| e.as_str()) {
            anyhow::bail!("Tavily API error: {}", error);
        }

        let results = parsed
            .get("results")
            .and_then(serde_json::Value::as_array)
            .ok_or_else(|| anyhow::anyhow!("Tavily response missing results array"))?;

        if results.is_empty() {
            return Ok(format!("No results found for: {}", query));
        }

        let mut lines = vec![format!("Search results for: {} (via Tavily)", query)];

        for (i, result) in results.iter().take(self.max_results).enumerate() {
            let title = result
                .get("title")
                .and_then(serde_json::Value::as_str)
                .unwrap_or("No title");
            let url = result
                .get("url")
                .and_then(serde_json::Value::as_str)
                .unwrap_or("");
            let content = result
                .get("content")
                .and_then(serde_json::Value::as_str)
                .unwrap_or("");

            lines.push(format!("{}. {}", i + 1, title));
            lines.push(format!("   {}", url));
            if !content.trim().is_empty() {
                lines.push(format!("   {}", content.trim()));
            }
        }

        Ok(lines.join("\n"))
    }
}

/// Decode DuckDuckGo redirect URL to extract the actual destination URL.
///
/// # Arguments
/// * `raw_url` - The redirect URL from DuckDuckGo
///
/// # Returns
/// The decoded destination URL, or the original URL if decoding fails
fn decode_ddg_redirect_url(raw_url: &str) -> String {
    if let Some(index) = raw_url.find("uddg=") {
        let encoded = &raw_url[index + 5..];
        let encoded = encoded.split('&').next().unwrap_or(encoded);
        if let Ok(decoded) = urlencoding::decode(encoded) {
            return decoded.into_owned();
        }
    }

    raw_url.to_string()
}

/// Remove HTML tags from content, leaving only plain text.
///
/// # Arguments
/// * `content` - The HTML content to strip
///
/// # Returns
/// Plain text without HTML tags
fn strip_tags(content: &str) -> String {
    let re = Regex::new(r"<[^>]+>").unwrap();
    re.replace_all(content, "").to_string()
}

#[async_trait]
impl Tool for WebSearchTool {
    fn name(&self) -> &str {
        "web_search_tool"
    }

    fn description(&self) -> &str {
        "Search the web for information. Returns relevant search results with titles, URLs, and descriptions. Use this to find current information, news, or research topics."
    }

    fn parameters_schema(&self) -> serde_json::Value {
        json!({
            "type": "object",
            "properties": {
                "query": {
                    "type": "string",
                    "description": "The search query. Be specific for better results."
                }
            },
            "required": ["query"]
        })
    }

    async fn execute(&self, args: serde_json::Value) -> anyhow::Result<ToolResult> {
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

        let query = args
            .get("query")
            .and_then(|q| q.as_str())
            .ok_or_else(|| anyhow::anyhow!("Missing required parameter: query"))?;

        if query.trim().is_empty() {
            anyhow::bail!("Search query cannot be empty");
        }

        tracing::info!("Searching web for: {}", query);

        let result = match self.provider.as_str() {
            "duckduckgo" | "ddg" => self.search_duckduckgo(query).await?,
            "bing" => self.search_bing(query).await?,
            "brave" => self.search_brave(query).await?,
            "firecrawl" => self.search_firecrawl(query).await?,
            "tavily" => self.search_tavily(query).await?,
            _ => {
                let supported = if cfg!(feature = "firecrawl") {
                    "'duckduckgo', 'bing', 'brave', 'firecrawl', or 'tavily'"
                } else {
                    "'duckduckgo', 'bing', 'brave', or 'tavily'"
                };
                anyhow::bail!(
                    "Unknown search provider: '{}'. Set [web_search].provider to {} in config.toml",
                    self.provider,
                    supported
                )
            }
        };

        Ok(ToolResult {
            success: true,
            output: result,
            error: None,
        })
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::security::{AutonomyLevel, SecurityPolicy};
    use std::fmt::Write as _;

    fn test_security() -> Arc<SecurityPolicy> {
        Arc::new(SecurityPolicy {
            autonomy: AutonomyLevel::Supervised,
            ..SecurityPolicy::default()
        })
    }

    #[test]
    fn test_tool_name() {
        let tool = WebSearchTool::new(
            test_security(),
            "duckduckgo".to_string(),
            None,
            None,
            5,
            15,
            "test".to_string(),
        );
        assert_eq!(tool.name(), "web_search_tool");
    }

    #[test]
    fn test_tool_description() {
        let tool = WebSearchTool::new(
            test_security(),
            "duckduckgo".to_string(),
            None,
            None,
            5,
            15,
            "test".to_string(),
        );
        assert!(tool.description().contains("Search the web"));
    }

    #[test]
    fn test_parameters_schema() {
        let tool = WebSearchTool::new(
            test_security(),
            "duckduckgo".to_string(),
            None,
            None,
            5,
            15,
            "test".to_string(),
        );
        let schema = tool.parameters_schema();
        assert_eq!(schema["type"], "object");
        assert!(schema["properties"]["query"].is_object());
    }

    #[test]
    fn test_strip_tags() {
        let html = "<b>Hello</b> <i>World</i>";
        assert_eq!(strip_tags(html), "Hello World");
    }

    #[test]
    fn test_parse_duckduckgo_results_empty() {
        let tool = WebSearchTool::new(
            test_security(),
            "duckduckgo".to_string(),
            None,
            None,
            5,
            15,
            "test".to_string(),
        );
        let result = tool
            .parse_duckduckgo_results("<html>No results here</html>", "test")
            .unwrap();
        assert!(result.contains("No results found"));
    }

    #[test]
    fn test_parse_duckduckgo_results_with_data() {
        let tool = WebSearchTool::new(
            test_security(),
            "duckduckgo".to_string(),
            None,
            None,
            5,
            15,
            "test".to_string(),
        );
        let html = r#"
            <a class="result__a" href="https://example.com">Example Title</a>
            <a class="result__snippet">This is a description</a>
        "#;
        let result = tool.parse_duckduckgo_results(html, "test").unwrap();
        assert!(result.contains("Example Title"));
        assert!(result.contains("https://example.com"));
    }

    #[test]
    fn test_parse_duckduckgo_results_decodes_redirect_url() {
        let tool = WebSearchTool::new(
            test_security(),
            "duckduckgo".to_string(),
            None,
            None,
            5,
            15,
            "test".to_string(),
        );
        let html = r#"
            <a class="result__a" href="https://duckduckgo.com/l/?uddg=https%3A%2F%2Fexample.com%2Fpath%3Fa%3D1&amp;rut=test">Example Title</a>
            <a class="result__snippet">This is a description</a>
        "#;
        let result = tool.parse_duckduckgo_results(html, "test").unwrap();
        assert!(result.contains("https://example.com/path?a=1"));
        assert!(!result.contains("rut=test"));
    }

    #[test]
    fn test_parse_bing_results_empty() {
        let tool = WebSearchTool::new(
            test_security(),
            "bing".to_string(),
            None,
            None,
            5,
            15,
            "test".to_string(),
        );
        let result = tool
            .parse_bing_results("<html>No results here</html>", "test")
            .unwrap();
        assert!(result.contains("No results found"));
    }

    #[test]
    fn test_parse_bing_results_with_data() {
        let tool = WebSearchTool::new(
            test_security(),
            "bing".to_string(),
            None,
            None,
            5,
            15,
            "test".to_string(),
        );
        let html = r#"
            <li class="b_algo">
                <h2><a href="https://example.com/path?a=1&amp;b=2">Example Title</a></h2>
                <div class="b_caption"><p>This is a description</p></div>
            </li>
        "#;
        let result = tool.parse_bing_results(html, "test").unwrap();
        assert!(result.contains("Example Title"));
        assert!(result.contains("https://example.com/path?a=1&b=2"));
        assert!(result.contains("This is a description"));
    }

    #[test]
    fn test_parse_bing_results_skips_non_http_urls() {
        let tool = WebSearchTool::new(
            test_security(),
            "bing".to_string(),
            None,
            None,
            5,
            15,
            "test".to_string(),
        );
        let html = r#"
            <li class="b_algo">
                <h2><a href="/search?q=internal">Internal</a></h2>
                <div class="b_caption"><p>Should be ignored</p></div>
            </li>
        "#;
        let result = tool.parse_bing_results(html, "test").unwrap();
        assert!(result.contains("No results found"));
    }

    #[test]
    fn test_parse_bing_results_prefers_headline_link() {
        let tool = WebSearchTool::new(
            test_security(),
            "bing".to_string(),
            None,
            None,
            5,
            15,
            "test".to_string(),
        );
        let html = r#"
            <li class="b_algo">
                <div class="b_attribution"><a href="https://tracking.example.com/redirect">tracking link</a></div>
                <h2><a href="https://example.com/headline">Headline Result</a></h2>
                <div class="b_caption"><p>Primary snippet</p></div>
            </li>
        "#;
        let result = tool.parse_bing_results(html, "test").unwrap();
        assert!(result.contains("Headline Result"));
        assert!(result.contains("https://example.com/headline"));
        assert!(!result.contains("https://tracking.example.com/redirect"));
    }

    #[test]
    fn test_parse_bing_results_max_results_boundary() {
        let tool = WebSearchTool::new(
            test_security(),
            "bing".to_string(),
            None,
            None,
            5,
            15,
            "test".to_string(),
        );
        let mut html = String::new();
        for i in 1..=6 {
            let _ = write!(
                &mut html,
                r#"<li class="b_algo"><h2><a href="https://example.com/{i}">Title {i}</a></h2><div class="b_caption"><p>Snippet {i}</p></div></li>"#
            );
        }

        let result = tool.parse_bing_results(&html, "test").unwrap();

        for i in 1..=5 {
            assert!(result.contains(&format!("Title {i}")));
            assert!(result.contains(&format!("https://example.com/{i}")));
            assert!(result.contains(&format!("Snippet {i}")));
        }

        assert!(!result.contains("Title 6"));
        assert!(!result.contains("https://example.com/6"));
        assert!(!result.contains("Snippet 6"));
    }

    #[test]
    fn test_constructor_clamps_web_search_limits() {
        let tool = WebSearchTool::new(
            test_security(),
            "duckduckgo".to_string(),
            None,
            None,
            0,
            0,
            "test".to_string(),
        );
        let html = r#"
            <a class="result__a" href="https://example.com">Example Title</a>
            <a class="result__snippet">This is a description</a>
        "#;
        let result = tool.parse_duckduckgo_results(html, "test").unwrap();
        assert!(result.contains("Example Title"));
    }

    #[tokio::test]
    async fn test_execute_missing_query() {
        let tool = WebSearchTool::new(
            test_security(),
            "duckduckgo".to_string(),
            None,
            None,
            5,
            15,
            "test".to_string(),
        );
        let result = tool.execute(json!({})).await;
        assert!(result.is_err());
    }

    #[tokio::test]
    async fn test_execute_empty_query() {
        let tool = WebSearchTool::new(
            test_security(),
            "duckduckgo".to_string(),
            None,
            None,
            5,
            15,
            "test".to_string(),
        );
        let result = tool.execute(json!({"query": ""})).await;
        assert!(result.is_err());
    }

    #[tokio::test]
    async fn test_execute_brave_without_api_key() {
        let tool = WebSearchTool::new(
            test_security(),
            "brave".to_string(),
            None,
            None,
            5,
            15,
            "test".to_string(),
        );
        let result = tool.execute(json!({"query": "test"})).await;
        assert!(result.is_err());
        assert!(result.unwrap_err().to_string().contains("API key"));
    }

    #[tokio::test]
    async fn test_execute_firecrawl_without_api_key() {
        let tool = WebSearchTool::new(
            test_security(),
            "firecrawl".to_string(),
            None,
            None,
            5,
            15,
            "test".to_string(),
        );
        let result = tool.execute(json!({"query": "test"})).await;
        assert!(result.is_err());
        let error = result.unwrap_err().to_string();
        if cfg!(feature = "firecrawl") {
            assert!(error.contains("api_key"));
        } else {
            assert!(error.contains("requires Cargo feature 'firecrawl'"));
        }
    }

    #[tokio::test]
    async fn test_execute_unknown_provider_lists_bing() {
        let tool = WebSearchTool::new(
            test_security(),
            "unknown-provider".to_string(),
            None,
            None,
            5,
            15,
            "test".to_string(),
        );
        let result = tool.execute(json!({"query": "test"})).await;
        assert!(result.is_err());
        let error = result.unwrap_err().to_string();
        if cfg!(feature = "firecrawl") {
            assert!(error.contains("'duckduckgo', 'bing', 'brave', 'firecrawl', or 'tavily'"));
        } else {
            assert!(error.contains("'duckduckgo', 'bing', 'brave', or 'tavily'"));
        }
    }

    #[tokio::test]
    async fn test_execute_blocked_in_read_only_mode() {
        let security = Arc::new(SecurityPolicy {
            autonomy: AutonomyLevel::ReadOnly,
            ..SecurityPolicy::default()
        });
        let tool = WebSearchTool::new(
            security,
            "duckduckgo".to_string(),
            None,
            None,
            5,
            15,
            "test".to_string(),
        );
        let result = tool.execute(json!({"query": "rust"})).await.unwrap();
        assert!(!result.success);
        assert!(result.error.unwrap().contains("read-only"));
    }

    #[tokio::test]
    async fn test_execute_tavily_without_api_key() {
        let tool = WebSearchTool::new(
            test_security(),
            "tavily".to_string(),
            None,
            None,
            5,
            15,
            "test".to_string(),
        );
        let result = tool.execute(json!({"query": "test"})).await;
        assert!(result.is_err());
        assert!(result.unwrap_err().to_string().contains("api_key"));
    }

    #[test]
    fn test_multiple_api_keys_parsing() {
        let tool = WebSearchTool::new(
            test_security(),
            "tavily".to_string(),
            Some("key1,key2,key3".to_string()),
            None,
            5,
            15,
            "test".to_string(),
        );
        assert_eq!(tool.api_keys.len(), 3);
        assert_eq!(tool.api_keys[0], "key1");
        assert_eq!(tool.api_keys[1], "key2");
        assert_eq!(tool.api_keys[2], "key3");
    }

    #[test]
    fn test_multiple_api_keys_with_spaces() {
        let tool = WebSearchTool::new(
            test_security(),
            "tavily".to_string(),
            Some("key1, key2 , key3".to_string()),
            None,
            5,
            15,
            "test".to_string(),
        );
        assert_eq!(tool.api_keys.len(), 3);
        assert_eq!(tool.api_keys[0], "key1");
        assert_eq!(tool.api_keys[1], "key2");
        assert_eq!(tool.api_keys[2], "key3");
    }

    #[test]
    fn test_round_robin_api_key_selection() {
        let tool = WebSearchTool::new(
            test_security(),
            "tavily".to_string(),
            Some("key1,key2,key3".to_string()),
            None,
            5,
            15,
            "test".to_string(),
        );

        assert_eq!(tool.get_next_api_key().unwrap(), "key1");
        assert_eq!(tool.get_next_api_key().unwrap(), "key2");
        assert_eq!(tool.get_next_api_key().unwrap(), "key3");
        assert_eq!(tool.get_next_api_key().unwrap(), "key1"); // wraps around
    }

    #[test]
    fn test_empty_api_key_returns_none() {
        let tool = WebSearchTool::new(
            test_security(),
            "tavily".to_string(),
            None,
            None,
            5,
            15,
            "test".to_string(),
        );
        assert!(tool.get_next_api_key().is_none());
    }

    #[test]
    fn test_single_api_key_works() {
        let tool = WebSearchTool::new(
            test_security(),
            "tavily".to_string(),
            Some("single-key".to_string()),
            None,
            5,
            15,
            "test".to_string(),
        );
        assert_eq!(tool.api_keys.len(), 1);
        assert_eq!(tool.get_next_api_key().unwrap(), "single-key");
    }
}
