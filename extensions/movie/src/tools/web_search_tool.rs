use super::traits::{Tool, ToolResult};
use crate::security::SecurityPolicy;
use async_trait::async_trait;
use regex::Regex;
use reqwest::StatusCode;
use serde_json::json;
use std::collections::HashSet;
use std::sync::atomic::{AtomicUsize, Ordering};
use std::sync::Arc;
use std::time::Duration;

/// Web search tool for searching the internet.
/// Supports providers: DuckDuckGo (free), Brave, Firecrawl, Tavily, Perplexity, Exa, and Jina.
pub struct WebSearchTool {
    security: Arc<SecurityPolicy>,
    provider: String,
    fallback_providers: Vec<String>,
    api_keys: Vec<String>,
    brave_api_keys: Vec<String>,
    perplexity_api_keys: Vec<String>,
    exa_api_keys: Vec<String>,
    jina_api_keys: Vec<String>,
    api_url: Option<String>,
    max_results: usize,
    timeout_secs: u64,
    user_agent: String,
    retries_per_provider: u32,
    retry_backoff_ms: u64,
    domain_filter: Vec<String>,
    language_filter: Vec<String>,
    country: Option<String>,
    recency_filter: Option<String>,
    max_tokens: Option<u32>,
    max_tokens_per_page: Option<u32>,
    exa_search_type: String,
    exa_include_text: bool,
    jina_site_filters: Vec<String>,
    key_index: Arc<AtomicUsize>,
    brave_key_index: Arc<AtomicUsize>,
    perplexity_key_index: Arc<AtomicUsize>,
    exa_key_index: Arc<AtomicUsize>,
    jina_key_index: Arc<AtomicUsize>,
}

impl WebSearchTool {
    fn duckduckgo_status_hint(status: StatusCode) -> &'static str {
        match status {
            StatusCode::FORBIDDEN | StatusCode::TOO_MANY_REQUESTS => {
                " DuckDuckGo may be blocking this network. Try [web_search].provider = \"brave\" with [web_search].brave_api_key, or set provider = \"firecrawl\"."
            }
            StatusCode::SERVICE_UNAVAILABLE | StatusCode::BAD_GATEWAY | StatusCode::GATEWAY_TIMEOUT => {
                " DuckDuckGo may be temporarily unavailable. Retry later or switch providers."
            }
            _ => "",
        }
    }

    pub fn new(
        security: Arc<SecurityPolicy>,
        provider: String,
        api_key: Option<String>,
        api_url: Option<String>,
        max_results: usize,
        timeout_secs: u64,
        user_agent: String,
    ) -> Self {
        Self::new_with_options(
            security,
            provider,
            api_key,
            None,
            None,
            None,
            None,
            api_url,
            max_results,
            timeout_secs,
            user_agent,
            Vec::new(),
            0,
            250,
            Vec::new(),
            Vec::new(),
            None,
            None,
            None,
            None,
            "auto".to_string(),
            false,
            Vec::new(),
        )
    }

    #[allow(clippy::too_many_arguments)]
    pub fn new_with_options(
        security: Arc<SecurityPolicy>,
        provider: String,
        api_key: Option<String>,
        brave_api_key: Option<String>,
        perplexity_api_key: Option<String>,
        exa_api_key: Option<String>,
        jina_api_key: Option<String>,
        api_url: Option<String>,
        max_results: usize,
        timeout_secs: u64,
        user_agent: String,
        fallback_providers: Vec<String>,
        retries_per_provider: u32,
        retry_backoff_ms: u64,
        domain_filter: Vec<String>,
        language_filter: Vec<String>,
        country: Option<String>,
        recency_filter: Option<String>,
        max_tokens: Option<u32>,
        max_tokens_per_page: Option<u32>,
        exa_search_type: String,
        exa_include_text: bool,
        jina_site_filters: Vec<String>,
    ) -> Self {
        let api_keys = Self::parse_api_keys(api_key.as_deref());
        let brave_api_keys = Self::parse_api_keys(brave_api_key.as_deref());
        let perplexity_api_keys = Self::parse_api_keys(perplexity_api_key.as_deref());
        let exa_api_keys = Self::parse_api_keys(exa_api_key.as_deref());
        let jina_api_keys = Self::parse_api_keys(jina_api_key.as_deref());
        Self {
            security,
            provider: provider.trim().to_lowercase(),
            fallback_providers,
            api_keys,
            brave_api_keys,
            perplexity_api_keys,
            exa_api_keys,
            jina_api_keys,
            api_url,
            max_results: max_results.clamp(1, 10),
            timeout_secs: timeout_secs.max(1),
            user_agent,
            retries_per_provider: retries_per_provider.min(5),
            retry_backoff_ms: retry_backoff_ms.max(1),
            domain_filter,
            language_filter,
            country,
            recency_filter,
            max_tokens,
            max_tokens_per_page,
            exa_search_type: exa_search_type.trim().to_ascii_lowercase(),
            exa_include_text,
            jina_site_filters,
            key_index: Arc::new(AtomicUsize::new(0)),
            brave_key_index: Arc::new(AtomicUsize::new(0)),
            perplexity_key_index: Arc::new(AtomicUsize::new(0)),
            exa_key_index: Arc::new(AtomicUsize::new(0)),
            jina_key_index: Arc::new(AtomicUsize::new(0)),
        }
    }

    fn parse_api_keys(raw: Option<&str>) -> Vec<String> {
        raw.map(|value| {
            value
                .split(',')
                .map(str::trim)
                .filter(|s| !s.is_empty())
                .map(ToOwned::to_owned)
                .collect()
        })
        .unwrap_or_default()
    }

    fn get_next_key_from(keys: &[String], index: &AtomicUsize) -> Option<String> {
        if keys.is_empty() {
            return None;
        }
        let idx = index.fetch_add(1, Ordering::Relaxed) % keys.len();
        Some(keys[idx].clone())
    }

    fn get_next_api_key(&self) -> Option<String> {
        Self::get_next_key_from(&self.api_keys, &self.key_index)
    }

    fn get_next_brave_api_key(&self) -> Option<String> {
        Self::get_next_key_from(&self.brave_api_keys, &self.brave_key_index)
            .or_else(|| self.get_next_api_key())
    }

    fn get_next_perplexity_api_key(&self) -> Option<String> {
        Self::get_next_key_from(&self.perplexity_api_keys, &self.perplexity_key_index)
            .or_else(|| self.get_next_api_key())
    }

    fn get_next_exa_api_key(&self) -> Option<String> {
        Self::get_next_key_from(&self.exa_api_keys, &self.exa_key_index)
            .or_else(|| self.get_next_api_key())
    }

    fn get_next_jina_api_key(&self) -> Option<String> {
        Self::get_next_key_from(&self.jina_api_keys, &self.jina_key_index)
            .or_else(|| self.get_next_api_key())
    }

    fn normalize_provider(raw: &str) -> Option<&'static str> {
        match raw.trim().to_ascii_lowercase().as_str() {
            "duckduckgo" | "ddg" => Some("duckduckgo"),
            "brave" => Some("brave"),
            "firecrawl" => Some("firecrawl"),
            "tavily" => Some("tavily"),
            "perplexity" => Some("perplexity"),
            "exa" => Some("exa"),
            "jina" => Some("jina"),
            _ => None,
        }
    }

    fn provider_chain(&self) -> anyhow::Result<Vec<&'static str>> {
        let mut chain: Vec<&'static str> = Vec::new();
        let mut seen: HashSet<&'static str> = HashSet::new();

        for raw in std::iter::once(self.provider.as_str()).chain(
            self.fallback_providers
                .iter()
                .map(std::string::String::as_str),
        ) {
            let normalized = Self::normalize_provider(raw).ok_or_else(|| {
                anyhow::anyhow!(
                    "Unknown search provider '{raw}'. Supported: duckduckgo, brave, firecrawl, tavily, perplexity, exa, jina"
                )
            })?;
            if seen.insert(normalized) {
                chain.push(normalized);
            }
        }

        Ok(chain)
    }

    async fn search_duckduckgo(&self, query: &str) -> anyhow::Result<String> {
        let encoded_query = urlencoding::encode(query);
        let search_url = format!("https://html.duckduckgo.com/html/?q={}", encoded_query);

        let client = reqwest::Client::builder()
            .timeout(Duration::from_secs(self.timeout_secs))
            .user_agent(self.user_agent.as_str())
            .build()?;

        let response = client.get(&search_url).send().await.map_err(|e| {
            anyhow::anyhow!(
                "DuckDuckGo search request failed: {e}. Check outbound network/proxy settings, or switch [web_search].provider to \"brave\"/\"firecrawl\"."
            )
        })?;

        if !response.status().is_success() {
            let status = response.status();
            anyhow::bail!(
                "DuckDuckGo search failed with status: {}.{}",
                status,
                Self::duckduckgo_status_hint(status)
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
        let auth_token = self
            .get_next_brave_api_key()
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
                format!("Bearer {}", auth_token),
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
                "include_images": false
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
        if let Some(error) = parsed.get("error").and_then(serde_json::Value::as_str) {
            anyhow::bail!("Tavily API error: {error}");
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
                .unwrap_or("")
                .trim();

            lines.push(format!("{}. {}", i + 1, title));
            lines.push(format!("   {}", url));
            if !content.is_empty() {
                lines.push(format!("   {}", content));
            }
        }

        Ok(lines.join("\n"))
    }

    async fn search_perplexity(&self, query: &str) -> anyhow::Result<String> {
        let api_key = self.get_next_perplexity_api_key().ok_or_else(|| {
            anyhow::anyhow!(
                "web_search provider 'perplexity' requires [web_search].perplexity_api_key or [web_search].api_key in config.toml"
            )
        })?;

        let api_url = self
            .api_url
            .as_deref()
            .map(str::trim)
            .filter(|s| !s.is_empty())
            .unwrap_or("https://api.perplexity.ai");
        let endpoint = format!("{}/search", api_url.trim_end_matches('/'));
        let client = reqwest::Client::builder()
            .timeout(Duration::from_secs(self.timeout_secs))
            .user_agent(self.user_agent.as_str())
            .build()?;

        let mut body = json!({
            "query": query,
            "max_results": self.max_results,
        });
        if let Some(tokens) = self.max_tokens {
            body["max_tokens"] = json!(tokens);
        }
        if let Some(tokens_per_page) = self.max_tokens_per_page {
            body["max_tokens_per_page"] = json!(tokens_per_page);
        }
        if !self.domain_filter.is_empty() {
            body["search_domain_filter"] = json!(self.domain_filter);
        }
        if !self.language_filter.is_empty() {
            body["search_language_filter"] = json!(self.language_filter);
        }
        if let Some(country) = self
            .country
            .as_deref()
            .map(str::trim)
            .filter(|s| !s.is_empty())
        {
            body["country"] = json!(country);
        }
        if let Some(recency) = self
            .recency_filter
            .as_deref()
            .map(str::trim)
            .filter(|s| !s.is_empty())
        {
            body["search_recency_filter"] = json!(recency);
        }

        let response = client
            .post(&endpoint)
            .header(
                reqwest::header::AUTHORIZATION,
                format!("Bearer {}", api_key),
            )
            .json(&body)
            .send()
            .await
            .map_err(|e| anyhow::anyhow!("Perplexity search failed: {e}"))?;
        let status = response.status();
        let raw = response.text().await?;
        if !status.is_success() {
            anyhow::bail!(
                "Perplexity search failed with status {}: {}",
                status.as_u16(),
                raw
            );
        }

        let parsed: serde_json::Value = serde_json::from_str(&raw)
            .map_err(|e| anyhow::anyhow!("Invalid Perplexity response JSON: {e}"))?;

        let results = parsed
            .get("results")
            .and_then(serde_json::Value::as_array)
            .ok_or_else(|| anyhow::anyhow!("Perplexity response missing results array"))?;

        if results.is_empty() {
            return Ok(format!("No results found for: {}", query));
        }

        let mut lines = vec![format!("Search results for: {} (via Perplexity)", query)];
        for (i, result) in results.iter().take(self.max_results).enumerate() {
            let title = result
                .get("title")
                .and_then(serde_json::Value::as_str)
                .unwrap_or("No title");
            let url = result
                .get("url")
                .and_then(serde_json::Value::as_str)
                .unwrap_or("");
            let snippet = result
                .get("snippet")
                .and_then(serde_json::Value::as_str)
                .unwrap_or("")
                .trim();

            lines.push(format!("{}. {}", i + 1, title));
            lines.push(format!("   {}", url));
            if !snippet.is_empty() {
                lines.push(format!("   {}", snippet));
            }
        }

        Ok(lines.join("\n"))
    }

    async fn search_exa(&self, query: &str) -> anyhow::Result<String> {
        let api_key = self.get_next_exa_api_key().ok_or_else(|| {
            anyhow::anyhow!(
                "web_search provider 'exa' requires [web_search].exa_api_key or [web_search].api_key in config.toml"
            )
        })?;

        let api_url = self
            .api_url
            .as_deref()
            .map(str::trim)
            .filter(|s| !s.is_empty())
            .unwrap_or("https://api.exa.ai");
        let endpoint = format!("{}/search", api_url.trim_end_matches('/'));
        let client = reqwest::Client::builder()
            .timeout(Duration::from_secs(self.timeout_secs))
            .user_agent(self.user_agent.as_str())
            .build()?;

        let mut body = json!({
            "query": query,
            "numResults": self.max_results,
        });

        if !self.exa_search_type.trim().is_empty() {
            body["type"] = json!(self.exa_search_type);
        }
        if self.exa_include_text {
            body["contents"] = json!({"text": true});
        }

        let response = client
            .post(&endpoint)
            .header("x-api-key", api_key)
            .json(&body)
            .send()
            .await
            .map_err(|e| anyhow::anyhow!("Exa search failed: {e}"))?;
        let status = response.status();
        let raw = response.text().await?;
        if !status.is_success() {
            anyhow::bail!("Exa search failed with status {}: {}", status.as_u16(), raw);
        }

        let parsed: serde_json::Value = serde_json::from_str(&raw)
            .map_err(|e| anyhow::anyhow!("Invalid Exa response JSON: {e}"))?;
        let results = parsed
            .get("results")
            .and_then(serde_json::Value::as_array)
            .ok_or_else(|| anyhow::anyhow!("Exa response missing results array"))?;

        if results.is_empty() {
            return Ok(format!("No results found for: {}", query));
        }

        let mut lines = vec![format!("Search results for: {} (via Exa)", query)];
        for (i, result) in results.iter().take(self.max_results).enumerate() {
            let title = result
                .get("title")
                .and_then(serde_json::Value::as_str)
                .unwrap_or("No title");
            let url = result
                .get("url")
                .and_then(serde_json::Value::as_str)
                .unwrap_or("");
            let snippet = result
                .get("summary")
                .or_else(|| result.get("text"))
                .and_then(serde_json::Value::as_str)
                .unwrap_or("")
                .trim();

            lines.push(format!("{}. {}", i + 1, title));
            lines.push(format!("   {}", url));
            if !snippet.is_empty() {
                lines.push(format!("   {}", snippet));
            }
        }

        Ok(lines.join("\n"))
    }

    async fn search_jina(&self, query: &str) -> anyhow::Result<String> {
        let api_url = self
            .api_url
            .as_deref()
            .map(str::trim)
            .filter(|s| !s.is_empty())
            .unwrap_or("https://s.jina.ai");

        let encoded_query = urlencoding::encode(query);
        let mut url = format!("{}/{}", api_url.trim_end_matches('/'), encoded_query);
        if !self.jina_site_filters.is_empty() {
            let site_query = self
                .jina_site_filters
                .iter()
                .map(String::as_str)
                .map(urlencoding::encode)
                .map(|value| format!("site={value}"))
                .collect::<Vec<_>>()
                .join("&");
            url = format!("{url}?{site_query}");
        }

        let client = reqwest::Client::builder()
            .timeout(Duration::from_secs(self.timeout_secs))
            .user_agent(self.user_agent.as_str())
            .build()?;

        let mut request = client.get(url).header("Accept", "text/plain");
        if let Some(api_key) = self.get_next_jina_api_key() {
            let token = api_key.trim().to_string();
            request = request
                .header(reqwest::header::AUTHORIZATION, format!("Bearer {}", token))
                .header("x-api-key", token);
        }

        let response = request
            .send()
            .await
            .map_err(|e| anyhow::anyhow!("Jina search failed: {e}"))?;
        let status = response.status();
        let body = response.text().await?;
        if !status.is_success() {
            anyhow::bail!(
                "Jina search failed with status {}: {}",
                status.as_u16(),
                body
            );
        }

        let trimmed = body.trim();
        if trimmed.is_empty() {
            return Ok(format!("No results found for: {}", query));
        }

        Ok(format!(
            "Search results for: {} (via Jina)\n{}",
            query, trimmed
        ))
    }

    async fn search_with_provider(&self, provider: &str, query: &str) -> anyhow::Result<String> {
        match provider {
            "duckduckgo" => self.search_duckduckgo(query).await,
            "brave" => self.search_brave(query).await,
            "firecrawl" => self.search_firecrawl(query).await,
            "tavily" => self.search_tavily(query).await,
            "perplexity" => self.search_perplexity(query).await,
            "exa" => self.search_exa(query).await,
            "jina" => self.search_jina(query).await,
            _ => anyhow::bail!("Unknown search provider: {provider}"),
        }
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

        let mut provider_errors: Vec<String> = Vec::new();
        let providers = self.provider_chain()?;
        let retry_attempts = self.retries_per_provider + 1;

        let mut result: Option<String> = None;
        for provider in providers {
            let mut attempt = 0u32;
            let mut success = false;
            while attempt < retry_attempts {
                match self.search_with_provider(provider, query).await {
                    Ok(output) => {
                        result = Some(output);
                        success = true;
                        break;
                    }
                    Err(error) => {
                        provider_errors.push(format!(
                            "{provider} attempt {}/{}: {}",
                            attempt + 1,
                            retry_attempts,
                            error
                        ));
                        attempt += 1;
                        if attempt < retry_attempts {
                            tokio::time::sleep(Duration::from_millis(self.retry_backoff_ms)).await;
                        }
                    }
                }
            }
            if success {
                break;
            }
        }

        let result = result.ok_or_else(|| {
            anyhow::anyhow!(
                "All configured web_search providers failed: {}",
                provider_errors.join(" | ")
            )
        })?;

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
    fn duckduckgo_status_hint_for_403_mentions_provider_switch() {
        let hint = WebSearchTool::duckduckgo_status_hint(StatusCode::FORBIDDEN);
        assert!(hint.contains("provider"));
        assert!(hint.contains("brave"));
    }

    #[test]
    fn duckduckgo_status_hint_for_500_is_empty() {
        assert!(
            WebSearchTool::duckduckgo_status_hint(StatusCode::INTERNAL_SERVER_ERROR).is_empty()
        );
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
    fn test_parses_multiple_api_keys() {
        let tool = WebSearchTool::new(
            test_security(),
            "tavily".to_string(),
            Some("key1,key2,key3".to_string()),
            None,
            5,
            15,
            "test".to_string(),
        );
        assert_eq!(tool.api_keys, vec!["key1", "key2", "key3"]);
    }

    #[test]
    fn test_round_robin_api_key_selection_cycles() {
        let tool = WebSearchTool::new(
            test_security(),
            "tavily".to_string(),
            Some("k1,k2".to_string()),
            None,
            5,
            15,
            "test".to_string(),
        );
        assert_eq!(tool.get_next_api_key().as_deref(), Some("k1"));
        assert_eq!(tool.get_next_api_key().as_deref(), Some("k2"));
        assert_eq!(tool.get_next_api_key().as_deref(), Some("k1"));
    }

    #[test]
    fn provider_chain_uses_primary_plus_fallbacks_and_dedupes() {
        let tool = WebSearchTool::new_with_options(
            test_security(),
            "duckduckgo".to_string(),
            None,
            None,
            None,
            None,
            None,
            None,
            5,
            15,
            "test".to_string(),
            vec!["ddg".into(), "tavily".into(), "brave".into()],
            1,
            300,
            Vec::new(),
            Vec::new(),
            None,
            None,
            None,
            None,
            "auto".to_string(),
            false,
            Vec::new(),
        );

        assert_eq!(
            tool.provider_chain().unwrap(),
            vec!["duckduckgo", "tavily", "brave"]
        );
    }

    #[test]
    fn provider_chain_rejects_unknown_provider() {
        let tool = WebSearchTool::new_with_options(
            test_security(),
            "duckduckgo".to_string(),
            None,
            None,
            None,
            None,
            None,
            None,
            5,
            15,
            "test".to_string(),
            vec!["unknown_provider".into()],
            1,
            300,
            Vec::new(),
            Vec::new(),
            None,
            None,
            None,
            None,
            "auto".to_string(),
            false,
            Vec::new(),
        );

        assert!(tool.provider_chain().is_err());
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
}
