use super::traits::{Tool, ToolResult};
use super::web_search_provider_routing::{resolve_web_search_provider, WebSearchProviderRoute};
use async_trait::async_trait;
use regex::Regex;
use serde_json::json;
use std::path::{Path, PathBuf};
use std::time::Duration;

/// Web search tool for searching the internet.
/// Supports multiple providers: DuckDuckGo (free), Brave (requires API key).
///
/// The Brave API key is resolved lazily at execution time: if the boot-time key
/// is missing or still encrypted, the tool re-reads `config.toml`, decrypts the
/// `[web_search] brave_api_key` field, and uses the result. This ensures that
/// keys set or rotated after boot, and encrypted keys, are correctly picked up.
pub struct WebSearchTool {
    /// Provider selector as configured by user. Routed via provider aliases at runtime.
    provider: String,
    /// Boot-time key snapshot (may be `None` if not yet configured at startup).
    boot_brave_api_key: Option<String>,
    boot_searxng_url: Option<String>,
    max_results: usize,
    timeout_secs: u64,
    /// Path to `config.toml` for lazy re-read of keys at execution time.
    config_path: PathBuf,
    /// Whether secret encryption is enabled (needed to create a `SecretStore`).
    secrets_encrypt: bool,
}

impl WebSearchTool {
    pub fn new(
        provider: String,
        brave_api_key: Option<String>,
        searxng_url: Option<String>,
        max_results: usize,
        timeout_secs: u64,
    ) -> Self {
        Self {
            provider: provider.trim().to_lowercase(),
            boot_brave_api_key: brave_api_key,
            boot_searxng_url: searxng_url,
            max_results: max_results.clamp(1, 10),
            timeout_secs: timeout_secs.max(1),
            config_path: PathBuf::new(),
            secrets_encrypt: false,
        }
    }

    /// Create a `WebSearchTool` with config-reload and decryption support.
    ///
    /// `config_path` is the path to `config.toml` so the tool can re-read the
    /// Brave API key at execution time. `secrets_encrypt` controls whether the
    /// key is decrypted via `SecretStore`.
    pub fn new_with_config(
        provider: String,
        brave_api_key: Option<String>,
        searxng_url: Option<String>,
        max_results: usize,
        timeout_secs: u64,
        config_path: PathBuf,
        secrets_encrypt: bool,
    ) -> Self {
        Self {
            provider: provider.trim().to_lowercase(),
            boot_brave_api_key: brave_api_key,
            boot_searxng_url: searxng_url,
            max_results: max_results.clamp(1, 10),
            timeout_secs: timeout_secs.max(1),
            config_path,
            secrets_encrypt,
        }
    }

    /// Resolve the Brave API key, preferring the boot-time value but falling
    /// back to a fresh config read + decryption when the boot-time value is
    /// absent.
    fn resolve_brave_api_key(&self) -> anyhow::Result<String> {
        // Fast path: boot-time key is present and usable (not an encrypted blob).
        if let Some(ref key) = self.boot_brave_api_key {
            if !key.is_empty() && !crate::security::SecretStore::is_encrypted(key) {
                return Ok(key.clone());
            }
        }

        // Slow path: re-read config.toml to pick up keys set/rotated after boot.
        self.reload_brave_api_key()
    }

    /// Re-read `config.toml` and decrypt `[web_search] brave_api_key`.
    fn reload_brave_api_key(&self) -> anyhow::Result<String> {
        let contents = std::fs::read_to_string(&self.config_path).map_err(|e| {
            anyhow::anyhow!(
                "Failed to read config file {} for Brave API key: {e}",
                self.config_path.display()
            )
        })?;

        let config: crate::config::Config = toml::from_str(&contents).map_err(|e| {
            anyhow::anyhow!(
                "Failed to parse config file {} for Brave API key: {e}",
                self.config_path.display()
            )
        })?;

        let raw_key = config
            .web_search
            .brave_api_key
            .filter(|k| !k.is_empty())
            .ok_or_else(|| anyhow::anyhow!("Brave API key not configured"))?;

        // Decrypt if necessary.
        if crate::security::SecretStore::is_encrypted(&raw_key) {
            let zeroclaw_dir = self.config_path.parent().unwrap_or_else(|| Path::new("."));
            let store = crate::security::SecretStore::new(zeroclaw_dir, self.secrets_encrypt);
            let plaintext = store.decrypt(&raw_key)?;
            if plaintext.is_empty() {
                anyhow::bail!("Brave API key not configured (decrypted value is empty)");
            }
            Ok(plaintext)
        } else {
            Ok(raw_key)
        }
    }

    fn resolve_searxng_url(&self) -> anyhow::Result<String> {
        if let Some(ref url) = self.boot_searxng_url {
            if !url.is_empty() {
                return Ok(url.clone());
            }
        }

        let contents = std::fs::read_to_string(&self.config_path).map_err(|e| {
            anyhow::anyhow!(
                "Failed to read config file {} for SearXNG URL: {e}",
                self.config_path.display()
            )
        })?;

        let config: crate::config::Config = toml::from_str(&contents).map_err(|e| {
            anyhow::anyhow!(
                "Failed to parse config file {} for SearXNG URL: {e}",
                self.config_path.display()
            )
        })?;

        let raw_url = config
            .web_search
            .searxng_url
            .filter(|u| !u.is_empty())
            .ok_or_else(|| anyhow::anyhow!("SearXNG URL not configured"))?;

        Ok(raw_url)
    }

    async fn search_searxng(&self, query: &str) -> anyhow::Result<String> {
        let searxng_url = self.resolve_searxng_url()?;
        let encoded_query = urlencoding::encode(query);
        let search_url = format!(
            "{}/search?q={}&format=json",
            searxng_url.trim_end_matches('/'),
            encoded_query
        );

        let client = reqwest::Client::builder()
            .timeout(Duration::from_secs(self.timeout_secs))
            .build()?;

        let response = client.get(&search_url).send().await?;

        if !response.status().is_success() {
            anyhow::bail!(
                "SearXNG search failed with status: {} (configured URL: {})",
                response.status(),
                searxng_url
            );
        }

        let json: serde_json::Value = response.json().await?;
        self.parse_searxng_results(&json, query)
    }

    fn parse_searxng_results(
        &self,
        json: &serde_json::Value,
        query: &str,
    ) -> anyhow::Result<String> {
        let results = json
            .get("results")
            .and_then(|r| r.as_array())
            .ok_or_else(|| {
                anyhow::anyhow!(
                    "Invalid SearXNG API response (results key missing or not an array)"
                )
            })?;

        if results.is_empty() {
            return Ok(format!("No results found for: {}", query));
        }

        let mut lines = vec![format!("Search results for: {} (via SearXNG)", query)];

        for (i, result) in results.iter().take(self.max_results).enumerate() {
            let title = result
                .get("title")
                .and_then(|t| t.as_str())
                .unwrap_or("No title");
            let url = result.get("url").and_then(|u| u.as_str()).unwrap_or("");
            let content = result.get("content").and_then(|c| c.as_str()).unwrap_or("");

            lines.push(format!("{}. {}", i + 1, title));
            lines.push(format!("   {}", url));
            if !content.is_empty() {
                lines.push(format!("   {}", content));
            }
        }

        Ok(lines.join("\n"))
    }

    async fn search_duckduckgo(&self, query: &str) -> anyhow::Result<String> {
        let encoded_query = urlencoding::encode(query);
        let search_url = format!("https://html.duckduckgo.com/html/?q={}", encoded_query);

        let client = reqwest::Client::builder()
            .timeout(Duration::from_secs(self.timeout_secs))
            .user_agent("Mozilla/5.0 (Windows NT 10.0; Win64; x64) AppleWebKit/537.36 (KHTML, like Gecko) Chrome/120.0.0.0 Safari/537.36")
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
        let api_key = self.resolve_brave_api_key()?;

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
            .header("X-Subscription-Token", &api_key)
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

        let resolution = resolve_web_search_provider(&self.provider);
        if resolution.used_fallback {
            tracing::warn!(
                "Unknown web search provider '{}'; falling back to '{}'",
                self.provider,
                resolution.canonical_provider
            );
        }

        let result = match resolution.route {
            WebSearchProviderRoute::DuckDuckGo => self.search_duckduckgo(query).await?,
            WebSearchProviderRoute::Brave => self.search_brave(query).await?,
            WebSearchProviderRoute::SearXNG => self.search_searxng(query).await?,
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
        let tool = WebSearchTool::new("duckduckgo".to_string(), None, None, 5, 15);
        assert_eq!(tool.name(), "web_search_tool");
    }

    #[test]
    fn test_tool_description() {
        let tool = WebSearchTool::new("duckduckgo".to_string(), None, None, 5, 15);
        assert!(tool.description().contains("Search the web"));
    }

    #[test]
    fn test_parameters_schema() {
        let tool = WebSearchTool::new("duckduckgo".to_string(), None, None, 5, 15);
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
        let tool = WebSearchTool::new("duckduckgo".to_string(), None, None, 5, 15);
        let result = tool
            .parse_duckduckgo_results("<html>No results here</html>", "test")
            .unwrap();
        assert!(result.contains("No results found"));
    }

    #[test]
    fn test_parse_duckduckgo_results_with_data() {
        let tool = WebSearchTool::new("duckduckgo".to_string(), None, None, 5, 15);
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
        let tool = WebSearchTool::new("duckduckgo".to_string(), None, None, 5, 15);
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
        let tool = WebSearchTool::new("duckduckgo".to_string(), None, None, 0, 0);
        let html = r#"
            <a class="result__a" href="https://example.com">Example Title</a>
            <a class="result__snippet">This is a description</a>
        "#;
        let result = tool.parse_duckduckgo_results(html, "test").unwrap();
        assert!(result.contains("Example Title"));
    }

    #[tokio::test]
    async fn test_execute_missing_query() {
        let tool = WebSearchTool::new("duckduckgo".to_string(), None, None, 5, 15);
        let result = tool.execute(json!({})).await;
        assert!(result.is_err());
    }

    #[tokio::test]
    async fn test_execute_empty_query() {
        let tool = WebSearchTool::new("duckduckgo".to_string(), None, None, 5, 15);
        let result = tool.execute(json!({"query": ""})).await;
        assert!(result.is_err());
    }

    #[tokio::test]
    async fn test_execute_brave_without_api_key() {
        let tool = WebSearchTool::new("brave".to_string(), None, None, 5, 15);
        let result = tool.execute(json!({"query": "test"})).await;
        assert!(result.is_err());
        assert!(result.unwrap_err().to_string().contains("API key"));
    }

    #[test]
    fn test_resolve_brave_api_key_uses_boot_key() {
        let tool = WebSearchTool::new(
            "brave".to_string(),
            Some("sk-plaintext-key".to_string()),
            None,
            5,
            15,
        );
        let key = tool.resolve_brave_api_key().unwrap();
        assert_eq!(key, "sk-plaintext-key");
    }

    #[test]
    fn test_resolve_brave_api_key_reloads_from_config() {
        let tmp = tempfile::TempDir::new().unwrap();
        let config_path = tmp.path().join("config.toml");
        std::fs::write(
            &config_path,
            "[web_search]\nbrave_api_key = \"fresh-key-from-disk\"\n",
        )
        .unwrap();

        // No boot key -- forces reload from config
        let tool = WebSearchTool::new_with_config(
            "brave".to_string(),
            None,
            None,
            5,
            15,
            config_path,
            false,
        );
        let key = tool.resolve_brave_api_key().unwrap();
        assert_eq!(key, "fresh-key-from-disk");
    }

    #[test]
    fn test_resolve_brave_api_key_decrypts_encrypted_key() {
        let tmp = tempfile::TempDir::new().unwrap();
        let store = crate::security::SecretStore::new(tmp.path(), true);
        let encrypted = store.encrypt("brave-secret-key").unwrap();

        let config_path = tmp.path().join("config.toml");
        std::fs::write(
            &config_path,
            format!("[web_search]\nbrave_api_key = \"{}\"\n", encrypted),
        )
        .unwrap();

        // Boot key is the encrypted blob -- should trigger reload + decrypt
        let tool = WebSearchTool::new_with_config(
            "brave".to_string(),
            Some(encrypted),
            None,
            5,
            15,
            config_path,
            true,
        );
        let key = tool.resolve_brave_api_key().unwrap();
        assert_eq!(key, "brave-secret-key");
    }

    #[test]
    fn test_resolve_brave_api_key_picks_up_runtime_update() {
        let tmp = tempfile::TempDir::new().unwrap();
        let config_path = tmp.path().join("config.toml");

        // Start with no key in config
        std::fs::write(&config_path, "[web_search]\n").unwrap();

        let tool = WebSearchTool::new_with_config(
            "brave".to_string(),
            None,
            None,
            5,
            15,
            config_path.clone(),
            false,
        );

        // Key not configured yet -- should fail
        assert!(tool.resolve_brave_api_key().is_err());

        // Simulate runtime config update (e.g. via web_search_config set)
        std::fs::write(
            &config_path,
            "[web_search]\nbrave_api_key = \"runtime-updated-key\"\n",
        )
        .unwrap();

        // Now should succeed with the updated key
        let key = tool.resolve_brave_api_key().unwrap();
        assert_eq!(key, "runtime-updated-key");
    }

    #[test]
    fn test_resolve_searxng_url_uses_boot_url() {
        let tool = WebSearchTool::new(
            "searxng".to_string(),
            None,
            Some("http://localhost:8080".to_string()),
            5,
            15,
        );
        let url = tool.resolve_searxng_url().unwrap();
        assert_eq!(url, "http://localhost:8080");
    }

    #[test]
    fn test_resolve_searxng_url_reloads_from_config() {
        let tmp = tempfile::TempDir::new().unwrap();
        let config_path = tmp.path().join("config.toml");
        std::fs::write(
            &config_path,
            "[web_search]\nsearxng_url = \"http://config-url:8080\"\n",
        )
        .unwrap();

        let tool = WebSearchTool::new_with_config(
            "searxng".to_string(),
            None,
            None,
            5,
            15,
            config_path,
            false,
        );
        let url = tool.resolve_searxng_url().unwrap();
        assert_eq!(url, "http://config-url:8080");
    }

    #[test]
    fn test_resolve_searxng_url_missing_returns_error() {
        let tmp = tempfile::TempDir::new().unwrap();
        let config_path = tmp.path().join("config.toml");
        std::fs::write(&config_path, "[web_search]\n").unwrap();

        let tool = WebSearchTool::new_with_config(
            "searxng".to_string(),
            None,
            None,
            5,
            15,
            config_path,
            false,
        );
        assert!(tool.resolve_searxng_url().is_err());
    }

    #[test]
    fn test_parse_searxng_results_empty() {
        let tool = WebSearchTool::new("searxng".to_string(), None, None, 5, 15);
        let json = json!({ "results": [] });
        let result = tool.parse_searxng_results(&json, "test query").unwrap();
        assert!(result.contains("No results found"));
    }

    #[test]
    fn test_parse_searxng_results_with_data() {
        let tool = WebSearchTool::new("searxng".to_string(), None, None, 5, 15);
        let json = json!({
            "results": [
                {
                    "title": "Example",
                    "url": "https://example.com",
                    "content": "Description info"
                }
            ]
        });
        let result = tool.parse_searxng_results(&json, "test query").unwrap();
        assert!(result.contains("1. Example"));
        assert!(result.contains("https://example.com"));
        assert!(result.contains("Description info"));
    }

    #[test]
    fn test_parse_searxng_results_missing_key() {
        let tool = WebSearchTool::new("searxng".to_string(), None, None, 5, 15);
        let json = json!({ "other": "data" });
        let result = tool.parse_searxng_results(&json, "test query");
        assert!(result.is_err());
        assert!(result
            .unwrap_err()
            .to_string()
            .contains("results key missing"));
    }

    #[tokio::test]
    async fn test_execute_searxng_without_url() {
        let tool = WebSearchTool::new("searxng".to_string(), None, None, 5, 15);
        let result = tool.execute(json!({"query": "test"})).await;
        assert!(result.is_err());
        assert!(result.unwrap_err().to_string().contains("SearXNG URL"));
    }
}
