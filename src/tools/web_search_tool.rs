use super::traits::{Tool, ToolResult};
use async_trait::async_trait;
use regex::Regex;
use serde_json::json;
use std::time::Duration;

/// Web search tool for searching the internet.
/// Supports multiple providers: DuckDuckGo (free), Brave (requires API key), and Firecrawl (requires feature + API key).
pub struct WebSearchTool {
    provider: String,
    brave_api_key: Option<String>,
    firecrawl_api_key: Option<String>,
    firecrawl_api_url: Option<String>,
    max_results: usize,
    timeout_secs: u64,
    user_agent: String,
}

impl WebSearchTool {
    pub fn new(
        provider: String,
        brave_api_key: Option<String>,
        firecrawl_api_key: Option<String>,
        firecrawl_api_url: Option<String>,
        max_results: usize,
        timeout_secs: u64,
        user_agent: String,
    ) -> Self {
        Self {
            provider: provider.trim().to_lowercase(),
            brave_api_key,
            firecrawl_api_key,
            firecrawl_api_url,
            max_results: max_results.clamp(1, 10),
            timeout_secs: timeout_secs.max(1),
            user_agent,
        }
    }

    async fn search_duckduckgo(&self, query: &str) -> anyhow::Result<String> {
        let encoded_query = urlencoding::encode(query);
        let search_url = format!("https://html.duckduckgo.com/html/?q={}", encoded_query);

        let client = reqwest::Client::builder()
            .timeout(Duration::from_secs(self.timeout_secs))
            .user_agent(&self.user_agent)
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
        let api_key = self
            .brave_api_key
            .as_ref()
            .ok_or_else(|| anyhow::anyhow!("Brave API key not configured"))?;

        let encoded_query = urlencoding::encode(query);
        let search_url = format!(
            "https://api.search.brave.com/res/v1/web/search?q={}&count={}",
            encoded_query, self.max_results
        );

        let client = reqwest::Client::builder()
            .timeout(Duration::from_secs(self.timeout_secs))
            .build()?;

        let response = client
            .get(&search_url)
            .header("Accept", "application/json")
            .header("X-Subscription-Token", api_key)
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

    #[cfg(feature = "firecrawl")]
    async fn search_firecrawl(&self, query: &str) -> anyhow::Result<String> {
        use firecrawl::search::SearchParams;
        use firecrawl::FirecrawlApp;

        let api_key = self
            .firecrawl_api_key
            .as_deref()
            .filter(|k| !k.is_empty())
            .ok_or_else(|| {
                anyhow::anyhow!(
                    "Firecrawl API key not configured. \
                    Set [web_search].firecrawl_api_key in config.toml"
                )
            })?;

        let app = match self.firecrawl_api_url.as_deref().filter(|u| !u.is_empty()) {
            Some(url) => FirecrawlApp::new_selfhosted(url, Some(api_key)),
            None => FirecrawlApp::new(api_key),
        }
        .map_err(|e| anyhow::anyhow!("Failed to initialize Firecrawl client: {e}"))?;

        let params = SearchParams {
            query: query.to_string(),
            limit: Some(self.max_results as u32),
            timeout: Some(self.timeout_secs as u32),
            ..Default::default()
        };

        let response = app
            .search_with_params(params)
            .await
            .map_err(|e| anyhow::anyhow!("Firecrawl search failed: {e}"))?;

        if response.data.is_empty() {
            return Ok(format!("No results found for: {}", query));
        }

        let mut lines = vec![format!("Search results for: {} (via Firecrawl)", query)];

        for (i, doc) in response.data.into_iter().enumerate() {
            lines.push(format!("{}. {}", i + 1, doc.title));
            lines.push(format!("   {}", doc.url));
            if !doc.description.is_empty() {
                lines.push(format!("   {}", doc.description));
            }
        }

        Ok(lines.join("\n"))
    }

    #[cfg(not(feature = "firecrawl"))]
    #[allow(clippy::unused_async)]
    async fn search_firecrawl(&self, _query: &str) -> anyhow::Result<String> {
        anyhow::bail!(
            "The 'firecrawl' provider requires the 'firecrawl' compile-time feature. \
            Rebuild with: cargo build --features firecrawl"
        )
    }
}

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
            "brave" => self.search_brave(query).await?,
            "firecrawl" => self.search_firecrawl(query).await?,
            _ => anyhow::bail!(
                "Unknown search provider: '{}'. Set tools.web_search.provider to 'duckduckgo', 'brave', or 'firecrawl' in config.toml",
                self.provider
            ),
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

    #[test]
    fn test_tool_name() {
        let tool = WebSearchTool::new(
            "duckduckgo".to_string(),
            None,
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
            "duckduckgo".to_string(),
            None,
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
            "duckduckgo".to_string(),
            None,
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
            "duckduckgo".to_string(),
            None,
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
            "duckduckgo".to_string(),
            None,
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
            "duckduckgo".to_string(),
            None,
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
    fn test_constructor_clamps_web_search_limits() {
        let tool = WebSearchTool::new(
            "duckduckgo".to_string(),
            None,
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

    #[test]
    fn test_constructor_stores_firecrawl_fields() {
        let tool = WebSearchTool::new(
            "firecrawl".to_string(),
            None,
            Some("fc-123".to_string()),
            Some("http://localhost:3000".to_string()),
            5,
            15,
            "test".to_string(),
        );
        assert_eq!(tool.firecrawl_api_key.as_deref(), Some("fc-123"));
        assert_eq!(
            tool.firecrawl_api_url.as_deref(),
            Some("http://localhost:3000")
        );
    }

    #[tokio::test]
    async fn test_execute_missing_query() {
        let tool = WebSearchTool::new(
            "duckduckgo".to_string(),
            None,
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
            "duckduckgo".to_string(),
            None,
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
            "brave".to_string(),
            None,
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
            "firecrawl".to_string(),
            None,
            None,
            None,
            5,
            15,
            "test".to_string(),
        );
        let result = tool.execute(json!({"query": "test"})).await;
        assert!(result.is_err());
        let err_msg = result.unwrap_err().to_string();

        #[cfg(feature = "firecrawl")]
        assert!(err_msg.contains("API key not configured"));

        #[cfg(not(feature = "firecrawl"))]
        assert!(err_msg.contains("requires the 'firecrawl' compile-time feature"));
    }
}
