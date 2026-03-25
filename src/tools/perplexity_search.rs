//! Perplexity Search API tool — pure retrieval layer.
//!
//! Calls the Perplexity Search API to retrieve ranked web search results
//! (URLs, titles, snippets) without generating an LLM answer. The raw
//! results are returned as structured context for the agent's LLM to
//! consume directly.
//!
//! This is intentionally separate from the Sonar-based `web_search_tool`
//! provider: Sonar generates its own answer (duplicating MoA's LLM layer),
//! whereas this tool returns **search index data only**, which is cheaper
//! and fits the MoA "search is a tool, answer is the LLM's job" design.

use super::traits::{Tool, ToolResult};
use crate::security::SecurityPolicy;
use async_trait::async_trait;
use serde::{Deserialize, Serialize};
use serde_json::json;
use std::sync::atomic::{AtomicUsize, Ordering};
use std::sync::Arc;
use std::time::Duration;

/// Perplexity Search API tool configuration.
#[derive(Debug, Clone)]
pub struct PerplexitySearchConfig {
    pub enabled: bool,
    pub api_key: Option<String>,
    pub api_url: String,
    pub max_results: usize,
    pub timeout_secs: u64,
    pub region: Option<String>,
    pub language: Option<String>,
    pub recency_filter: Option<String>,
    pub domain_filter: Vec<String>,
}

impl Default for PerplexitySearchConfig {
    fn default() -> Self {
        Self {
            enabled: false,
            api_key: None,
            api_url: "https://api.perplexity.ai".into(),
            max_results: 5,
            timeout_secs: 30,
            region: None,
            language: None,
            recency_filter: None,
            domain_filter: Vec::new(),
        }
    }
}

/// A single search result from the Perplexity Search API.
#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct SearchResult {
    pub url: String,
    pub title: String,
    pub snippets: Vec<String>,
    #[serde(default)]
    pub score: Option<f64>,
}

/// Perplexity Search API tool for pure web retrieval.
pub struct PerplexitySearchTool {
    security: Arc<SecurityPolicy>,
    api_keys: Vec<String>,
    api_url: String,
    max_results: usize,
    timeout_secs: u64,
    region: Option<String>,
    language: Option<String>,
    recency_filter: Option<String>,
    domain_filter: Vec<String>,
    key_index: Arc<AtomicUsize>,
}

impl PerplexitySearchTool {
    pub fn new(security: Arc<SecurityPolicy>, config: &PerplexitySearchConfig) -> Self {
        let api_keys = Self::parse_api_keys(config.api_key.as_deref());
        Self {
            security,
            api_keys,
            api_url: config
                .api_url
                .trim()
                .trim_end_matches('/')
                .to_string(),
            max_results: config.max_results.clamp(1, 20),
            timeout_secs: config.timeout_secs.max(1),
            region: config.region.clone(),
            language: config.language.clone(),
            recency_filter: config.recency_filter.clone(),
            domain_filter: config.domain_filter.clone(),
            key_index: Arc::new(AtomicUsize::new(0)),
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

    fn get_next_api_key(&self) -> Option<String> {
        if self.api_keys.is_empty() {
            return None;
        }
        let idx = self.key_index.fetch_add(1, Ordering::Relaxed) % self.api_keys.len();
        Some(self.api_keys[idx].clone())
    }

    /// Call the Perplexity Search API and return formatted results.
    async fn search(&self, query: &str, num_results: usize) -> anyhow::Result<String> {
        let api_key = self.get_next_api_key().ok_or_else(|| {
            anyhow::anyhow!(
                "perplexity_search requires [perplexity_search].api_key in config.toml \
                 or PERPLEXITY_API_KEY / ZEROCLAW_PERPLEXITY_SEARCH_API_KEY environment variable"
            )
        })?;

        let endpoint = format!("{}/search", self.api_url);
        let client = reqwest::Client::builder()
            .timeout(Duration::from_secs(self.timeout_secs))
            .build()?;

        let mut body = json!({
            "query": query,
            "num_results": num_results.clamp(1, 20),
        });

        if let Some(ref region) = self.region {
            body["region"] = json!(region);
        }
        if let Some(ref language) = self.language {
            body["language"] = json!(language);
        }
        if let Some(ref recency) = self.recency_filter {
            body["recency_filter"] = json!(recency);
        }
        if !self.domain_filter.is_empty() {
            body["search_domain_filter"] = json!(self.domain_filter);
        }

        let response = client
            .post(&endpoint)
            .header(
                reqwest::header::AUTHORIZATION,
                format!("Bearer {}", api_key),
            )
            .header(reqwest::header::CONTENT_TYPE, "application/json")
            .header(reqwest::header::ACCEPT, "application/json")
            .json(&body)
            .send()
            .await
            .map_err(|e| anyhow::anyhow!("Perplexity Search API request failed: {e}"))?;

        let status = response.status();
        let raw = response.text().await?;

        if !status.is_success() {
            // If the /search endpoint is not available, fall back to Sonar
            // chat completions with web_search=true for pure retrieval.
            if status.as_u16() == 404 || status.as_u16() == 405 {
                return self.search_via_sonar(query, &api_key, num_results).await;
            }
            anyhow::bail!(
                "Perplexity Search API error ({}): {}",
                status.as_u16(),
                raw
            );
        }

        let parsed: serde_json::Value = serde_json::from_str(&raw)
            .map_err(|e| anyhow::anyhow!("Invalid Perplexity Search response: {e}"))?;

        self.format_search_results(query, &parsed)
    }

    /// Fallback: use Sonar chat completions with web_search=true, extracting
    /// citations as search results when the dedicated /search endpoint is
    /// unavailable.
    async fn search_via_sonar(
        &self,
        query: &str,
        api_key: &str,
        _num_results: usize,
    ) -> anyhow::Result<String> {
        let endpoint = format!("{}/chat/completions", self.api_url);
        let client = reqwest::Client::builder()
            .timeout(Duration::from_secs(self.timeout_secs))
            .build()?;

        let mut body = json!({
            "model": "sonar",
            "messages": [
                {
                    "role": "system",
                    "content": "Return search results as a structured list. For each result provide: URL, title, and a brief snippet. Do not add commentary."
                },
                {
                    "role": "user",
                    "content": query
                }
            ],
            "web_search": true,
            "return_citations": true
        });

        if !self.domain_filter.is_empty() {
            body["search_domain_filter"] = json!(self.domain_filter);
        }
        if let Some(ref recency) = self.recency_filter {
            body["search_recency_filter"] = json!(recency);
        }

        let response = client
            .post(&endpoint)
            .header(
                reqwest::header::AUTHORIZATION,
                format!("Bearer {}", api_key),
            )
            .header(reqwest::header::CONTENT_TYPE, "application/json")
            .json(&body)
            .send()
            .await
            .map_err(|e| anyhow::anyhow!("Perplexity Sonar fallback failed: {e}"))?;

        let status = response.status();
        let raw = response.text().await?;

        if !status.is_success() {
            anyhow::bail!(
                "Perplexity Sonar fallback error ({}): {}",
                status.as_u16(),
                raw
            );
        }

        let parsed: serde_json::Value = serde_json::from_str(&raw)
            .map_err(|e| anyhow::anyhow!("Invalid Perplexity Sonar response: {e}"))?;

        // Extract answer and citations
        let answer = parsed
            .pointer("/choices/0/message/content")
            .and_then(serde_json::Value::as_str)
            .unwrap_or("")
            .trim();

        if answer.is_empty() {
            return Ok(format!("No search results found for: {}", query));
        }

        let mut out = format!(
            "Search results for: {} (via Perplexity Search)\n\n{}",
            query, answer
        );

        if let Some(citations) = parsed
            .get("citations")
            .and_then(serde_json::Value::as_array)
        {
            if !citations.is_empty() {
                out.push_str("\n\nSources:");
                for (i, cite) in citations.iter().enumerate() {
                    if let Some(url) = cite.as_str() {
                        out.push_str(&format!("\n[{}] {}", i + 1, url));
                    }
                }
            }
        }

        Ok(out)
    }

    /// Format raw JSON search results into a readable string for the LLM.
    fn format_search_results(
        &self,
        query: &str,
        parsed: &serde_json::Value,
    ) -> anyhow::Result<String> {
        let results = parsed
            .get("results")
            .and_then(serde_json::Value::as_array);

        let results = match results {
            Some(arr) if !arr.is_empty() => arr,
            _ => return Ok(format!("No search results found for: {}", query)),
        };

        let mut lines = vec![format!(
            "Search results for: {} (via Perplexity Search API)",
            query
        )];

        for (i, result) in results.iter().enumerate() {
            let url = result
                .get("url")
                .and_then(serde_json::Value::as_str)
                .unwrap_or("(no url)");
            let title = result
                .get("title")
                .and_then(serde_json::Value::as_str)
                .unwrap_or("(no title)");

            lines.push(format!("\n[{}] {}", i + 1, title));
            lines.push(format!("    URL: {}", url));

            if let Some(snippets) = result
                .get("snippets")
                .and_then(serde_json::Value::as_array)
            {
                for snippet in snippets {
                    if let Some(text) = snippet.as_str() {
                        lines.push(format!("    {}", text));
                    }
                }
            } else if let Some(snippet) = result
                .get("snippet")
                .and_then(serde_json::Value::as_str)
            {
                lines.push(format!("    {}", snippet));
            }

            if let Some(score) = result.get("score").and_then(serde_json::Value::as_f64) {
                lines.push(format!("    Relevance: {:.2}", score));
            }
        }

        Ok(lines.join("\n"))
    }
}

#[async_trait]
impl Tool for PerplexitySearchTool {
    fn name(&self) -> &str {
        "perplexity_search"
    }

    fn description(&self) -> &str {
        "Search the web using Perplexity Search API. Returns ranked search results \
         (URLs, titles, snippets) for the given query. Use this for web research, \
         fact-checking, and finding current information. Results are raw search data — \
         use the browser tool for follow-up page navigation, clicking, or form actions."
    }

    fn parameters_schema(&self) -> serde_json::Value {
        json!({
            "type": "object",
            "properties": {
                "query": {
                    "type": "string",
                    "description": "The search query to send to Perplexity Search API"
                },
                "num_results": {
                    "type": "integer",
                    "description": "Number of results to return (1-20, default 5)",
                    "minimum": 1,
                    "maximum": 20
                },
                "region": {
                    "type": "string",
                    "description": "Region code for localized results (e.g. 'KR', 'US', 'JP')"
                },
                "language": {
                    "type": "string",
                    "description": "Language code for results (e.g. 'ko', 'en', 'ja')"
                },
                "recency_filter": {
                    "type": "string",
                    "description": "Recency filter: 'day', 'week', 'month', 'year'"
                }
            },
            "required": ["query"]
        })
    }

    async fn execute(&self, args: serde_json::Value) -> anyhow::Result<ToolResult> {
        // Security policy check
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
            .and_then(serde_json::Value::as_str)
            .unwrap_or("")
            .trim();

        if query.is_empty() {
            return Ok(ToolResult {
                success: false,
                output: String::new(),
                error: Some("Missing required parameter: query".into()),
            });
        }

        let num_results = args
            .get("num_results")
            .and_then(serde_json::Value::as_u64)
            .map(|n| n as usize)
            .unwrap_or(self.max_results);

        match self.search(query, num_results).await {
            Ok(output) => Ok(ToolResult {
                success: true,
                output,
                error: None,
            }),
            Err(e) => Ok(ToolResult {
                success: false,
                output: String::new(),
                error: Some(format!("Perplexity Search failed: {e}")),
            }),
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    fn test_security() -> Arc<SecurityPolicy> {
        Arc::new(SecurityPolicy::default())
    }

    fn test_config() -> PerplexitySearchConfig {
        PerplexitySearchConfig {
            enabled: true,
            api_key: Some("test-key".into()),
            ..Default::default()
        }
    }

    #[test]
    fn spec_returns_expected_name_and_schema() {
        let tool = PerplexitySearchTool::new(test_security(), &test_config());
        let spec = tool.spec();
        assert_eq!(spec.name, "perplexity_search");
        assert!(spec.description.contains("Perplexity Search API"));
        assert_eq!(spec.parameters["properties"]["query"]["type"], "string");
    }

    #[test]
    fn parse_api_keys_handles_comma_separated() {
        let keys = PerplexitySearchTool::parse_api_keys(Some("key1, key2 , key3"));
        assert_eq!(keys, vec!["key1", "key2", "key3"]);
    }

    #[test]
    fn parse_api_keys_handles_none() {
        let keys = PerplexitySearchTool::parse_api_keys(None);
        assert!(keys.is_empty());
    }

    #[tokio::test]
    async fn execute_rejects_empty_query() {
        let tool = PerplexitySearchTool::new(test_security(), &test_config());
        let result = tool.execute(json!({"query": ""})).await.unwrap();
        assert!(!result.success);
        assert!(result.error.unwrap().contains("Missing required parameter"));
    }

    #[test]
    fn format_search_results_handles_empty() {
        let tool = PerplexitySearchTool::new(test_security(), &test_config());
        let result = tool
            .format_search_results("test", &json!({"results": []}))
            .unwrap();
        assert!(result.contains("No search results found"));
    }

    #[test]
    fn format_search_results_structures_output() {
        let tool = PerplexitySearchTool::new(test_security(), &test_config());
        let data = json!({
            "results": [
                {
                    "url": "https://example.com",
                    "title": "Example",
                    "snippets": ["A test snippet"],
                    "score": 0.95
                }
            ]
        });
        let result = tool.format_search_results("test query", &data).unwrap();
        assert!(result.contains("[1] Example"));
        assert!(result.contains("https://example.com"));
        assert!(result.contains("A test snippet"));
        assert!(result.contains("0.95"));
    }
}
