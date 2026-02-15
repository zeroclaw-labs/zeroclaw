use super::traits::{Tool, ToolResult};
use async_trait::async_trait;
use reqwest::Client;
use serde::Deserialize;
use serde_json::json;
use std::fmt::Write;
use std::time::Duration;

/// Maximum search request time before timeout.
const SEARCH_TIMEOUT_SECS: u64 = 15;
/// Maximum output size in bytes (50KB).
const MAX_OUTPUT_BYTES: usize = 51_200;

/// Web search tool with Tavily, Brave, and `DuckDuckGo` (free fallback)
pub struct WebSearchTool {
    tavily_key: Option<String>,
    brave_key: Option<String>,
    client: Client,
}

impl WebSearchTool {
    pub fn new() -> Self {
        Self {
            tavily_key: std::env::var("TAVILY_API_KEY")
                .ok()
                .filter(|k| !k.is_empty()),
            brave_key: std::env::var("BRAVE_API_KEY")
                .ok()
                .filter(|k| !k.is_empty()),
            client: Client::builder()
                .timeout(Duration::from_secs(SEARCH_TIMEOUT_SECS))
                .connect_timeout(Duration::from_secs(10))
                .build()
                .unwrap_or_else(|_| Client::new()),
        }
    }

    async fn search_tavily(
        &self,
        query: &str,
        max_results: usize,
        api_key: &str,
    ) -> anyhow::Result<String> {
        let body = json!({
            "api_key": api_key,
            "query": query,
            "max_results": max_results,
            "include_answer": true,
        });

        let resp = self
            .client
            .post("https://api.tavily.com/search")
            .json(&body)
            .send()
            .await?;

        if !resp.status().is_success() {
            let status = resp.status();
            let err = resp.text().await.unwrap_or_default();
            anyhow::bail!("Tavily API error ({status}): {err}");
        }

        let data: TavilyResponse = resp.json().await?;
        let mut output = String::new();

        if let Some(answer) = &data.answer {
            if !answer.is_empty() {
                let _ = writeln!(output, "Answer: {answer}\n");
            }
        }

        for (i, result) in data.results.iter().enumerate() {
            let _ = writeln!(
                output,
                "{}. {}\n   {}\n   {}\n",
                i + 1,
                result.title,
                result.url,
                truncate_content(&result.content, 300),
            );
        }

        if data.results.is_empty() {
            output.push_str("No results found.");
        }

        Ok(output)
    }

    async fn search_brave(
        &self,
        query: &str,
        max_results: usize,
        api_key: &str,
    ) -> anyhow::Result<String> {
        let resp = self
            .client
            .get("https://api.search.brave.com/res/v1/web/search")
            .header("X-Subscription-Token", api_key)
            .header("Accept", "application/json")
            .query(&[("q", query), ("count", &max_results.to_string())])
            .send()
            .await?;

        if !resp.status().is_success() {
            let status = resp.status();
            let err = resp.text().await.unwrap_or_default();
            anyhow::bail!("Brave Search API error ({status}): {err}");
        }

        let data: BraveResponse = resp.json().await?;
        let mut output = String::new();

        let results = data.web.map(|w| w.results).unwrap_or_default();

        for (i, result) in results.iter().enumerate() {
            let _ = writeln!(
                output,
                "{}. {}\n   {}\n   {}\n",
                i + 1,
                result.title,
                result.url,
                truncate_content(result.description.as_deref().unwrap_or(""), 300),
            );
        }

        if results.is_empty() {
            output.push_str("No results found.");
        }

        Ok(output)
    }

    /// Free fallback search via `DuckDuckGo` Instant Answer API
    async fn search_duckduckgo(&self, query: &str, max_results: usize) -> anyhow::Result<String> {
        let resp = self
            .client
            .get("https://api.duckduckgo.com/")
            .query(&[("q", query), ("format", "json"), ("no_html", "1")])
            .send()
            .await?;

        if !resp.status().is_success() {
            let status = resp.status();
            anyhow::bail!("DuckDuckGo API error ({status})");
        }

        let data: DdgResponse = resp.json().await?;
        let mut output = String::new();

        // Abstract is DuckDuckGo's top-level answer
        if !data.r#abstract.is_empty() {
            let _ = writeln!(output, "Answer: {}", data.r#abstract);
            if !data.abstract_url.is_empty() {
                let _ = writeln!(output, "Source: {}\n", data.abstract_url);
            }
        }

        let limit = max_results.min(data.related_topics.len());
        for (i, topic) in data.related_topics.iter().take(limit).enumerate() {
            if let Some(ref text) = topic.text {
                let _ = writeln!(
                    output,
                    "{}. {}\n   {}\n",
                    i + 1,
                    truncate_content(text, 300),
                    topic.first_url.as_deref().unwrap_or(""),
                );
            }
        }

        if output.is_empty() {
            output.push_str("No results found.");
        }

        Ok(output)
    }
}

#[async_trait]
impl Tool for WebSearchTool {
    fn name(&self) -> &str {
        "web_search"
    }

    fn description(&self) -> &str {
        "Search the web for information. Uses Tavily or Brave if API keys are set, \
         otherwise falls back to DuckDuckGo (no key required)."
    }

    fn parameters_schema(&self) -> serde_json::Value {
        json!({
            "type": "object",
            "properties": {
                "query": {
                    "type": "string",
                    "description": "The search query"
                },
                "max_results": {
                    "type": "integer",
                    "description": "Maximum number of results to return (1-10, default 5)"
                },
                "provider": {
                    "type": "string",
                    "enum": ["tavily", "brave", "duckduckgo"],
                    "description": "Search provider (default: auto — tries tavily, brave, then duckduckgo)"
                }
            },
            "required": ["query"]
        })
    }

    async fn execute(&self, args: serde_json::Value) -> anyhow::Result<ToolResult> {
        let query = args
            .get("query")
            .and_then(serde_json::Value::as_str)
            .ok_or_else(|| anyhow::anyhow!("Missing 'query' parameter"))?;

        if query.trim().is_empty() {
            return Ok(ToolResult {
                success: false,
                output: String::new(),
                error: Some("Search query cannot be empty".into()),
            });
        }

        let max_results = args
            .get("max_results")
            .and_then(serde_json::Value::as_u64)
            .map_or(5, |n| usize::try_from(n).unwrap_or(5).clamp(1, 10));

        let preferred = args
            .get("provider")
            .and_then(serde_json::Value::as_str)
            .unwrap_or("tavily");

        // Resolve provider: try preferred, then fallback chain -> duckduckgo
        let result = match preferred {
            "brave" if self.brave_key.is_some() => {
                self.search_brave(query, max_results, self.brave_key.as_ref().unwrap())
                    .await
            }
            "tavily" if self.tavily_key.is_some() => {
                self.search_tavily(query, max_results, self.tavily_key.as_ref().unwrap())
                    .await
            }
            "duckduckgo" => self.search_duckduckgo(query, max_results).await,
            _ => {
                // Auto: try tavily -> brave -> duckduckgo
                if let Some(ref key) = self.tavily_key {
                    self.search_tavily(query, max_results, key).await
                } else if let Some(ref key) = self.brave_key {
                    self.search_brave(query, max_results, key).await
                } else {
                    self.search_duckduckgo(query, max_results).await
                }
            }
        };

        match result {
            Ok(mut output) => {
                if output.len() > MAX_OUTPUT_BYTES {
                    output.truncate(MAX_OUTPUT_BYTES);
                    output.push_str("\n... [output truncated at 50KB]");
                }
                Ok(ToolResult {
                    success: true,
                    output,
                    error: None,
                })
            }
            Err(e) => Ok(ToolResult {
                success: false,
                output: String::new(),
                error: Some(format!("Search failed: {e}")),
            }),
        }
    }
}

// ── API response types ──────────────────────────────────────────

#[derive(Debug, Deserialize)]
struct TavilyResponse {
    #[serde(default)]
    answer: Option<String>,
    #[serde(default)]
    results: Vec<TavilyResult>,
}

#[derive(Debug, Deserialize)]
struct TavilyResult {
    #[serde(default)]
    title: String,
    #[serde(default)]
    url: String,
    #[serde(default)]
    content: String,
}

#[derive(Debug, Deserialize)]
struct BraveResponse {
    web: Option<BraveWebResults>,
}

#[derive(Debug, Deserialize)]
struct BraveWebResults {
    #[serde(default)]
    results: Vec<BraveResult>,
}

#[derive(Debug, Deserialize)]
struct BraveResult {
    #[serde(default)]
    title: String,
    #[serde(default)]
    url: String,
    description: Option<String>,
}

#[derive(Debug, Deserialize)]
struct DdgResponse {
    #[serde(rename = "Abstract", default)]
    r#abstract: String,
    #[serde(rename = "AbstractURL", default)]
    abstract_url: String,
    #[serde(rename = "RelatedTopics", default)]
    related_topics: Vec<DdgTopic>,
}

#[derive(Debug, Deserialize)]
struct DdgTopic {
    #[serde(rename = "Text")]
    text: Option<String>,
    #[serde(rename = "FirstURL")]
    first_url: Option<String>,
}

// ── Helpers ─────────────────────────────────────────────────────

fn truncate_content(s: &str, max_chars: usize) -> &str {
    if s.len() <= max_chars {
        return s;
    }
    // Find a safe UTF-8 boundary
    let mut end = max_chars;
    while end > 0 && !s.is_char_boundary(end) {
        end -= 1;
    }
    &s[..end]
}

#[cfg(test)]
mod tests {
    use super::*;

    fn tool() -> WebSearchTool {
        // Construct without env vars for testing
        WebSearchTool {
            tavily_key: None,
            brave_key: None,
            client: Client::new(),
        }
    }

    #[test]
    fn web_search_tool_name() {
        assert_eq!(tool().name(), "web_search");
    }

    #[test]
    fn web_search_tool_description() {
        assert!(!tool().description().is_empty());
    }

    #[test]
    fn web_search_tool_schema_has_query() {
        let schema = tool().parameters_schema();
        assert!(schema["properties"]["query"].is_object());
        assert!(schema["required"]
            .as_array()
            .unwrap()
            .contains(&json!("query")));
    }

    #[test]
    fn web_search_tool_schema_has_optional_fields() {
        let schema = tool().parameters_schema();
        assert!(schema["properties"]["max_results"].is_object());
        assert!(schema["properties"]["provider"].is_object());
    }

    #[test]
    fn web_search_tool_spec_roundtrip() {
        let t = tool();
        let spec = t.spec();
        assert_eq!(spec.name, "web_search");
        assert!(spec.parameters.is_object());
    }

    #[tokio::test]
    async fn web_search_missing_query_param() {
        let result = tool().execute(json!({})).await;
        assert!(result.is_err());
        assert!(result.unwrap_err().to_string().contains("query"));
    }

    #[tokio::test]
    async fn web_search_wrong_type_param() {
        let result = tool().execute(json!({"query": 123})).await;
        assert!(result.is_err());
    }

    #[tokio::test]
    async fn web_search_empty_query() {
        let result = tool().execute(json!({"query": "  "})).await.unwrap();
        assert!(!result.success);
        assert!(result.error.as_ref().unwrap().contains("empty"));
    }

    #[tokio::test]
    async fn web_search_no_api_key_falls_back_to_ddg() {
        // Without API keys, should attempt DuckDuckGo (not error)
        let result = tool()
            .execute(json!({"query": "rust programming"}))
            .await
            .unwrap();
        // Either succeeds via DDG or fails with network error — never "API key" error
        if !result.success {
            assert!(!result
                .error
                .as_ref()
                .unwrap_or(&String::new())
                .contains("API key"));
        }
    }

    #[tokio::test]
    async fn web_search_explicit_duckduckgo_provider() {
        let result = tool()
            .execute(json!({"query": "test", "provider": "duckduckgo"}))
            .await
            .unwrap();
        // Should attempt DDG regardless of API keys
        if !result.success {
            assert!(!result
                .error
                .as_ref()
                .unwrap_or(&String::new())
                .contains("API key"));
        }
    }

    #[test]
    fn truncate_content_short_string() {
        assert_eq!(truncate_content("hello", 10), "hello");
    }

    #[test]
    fn truncate_content_exact_length() {
        assert_eq!(truncate_content("hello", 5), "hello");
    }

    #[test]
    fn truncate_content_long_string() {
        let result = truncate_content("hello world", 5);
        assert_eq!(result, "hello");
    }

    #[test]
    fn truncate_content_unicode_safe() {
        // "café" — the 'é' is 2 bytes, so truncating at byte 4 would split it
        let s = "caf\u{00e9}";
        let result = truncate_content(s, 4);
        assert!(result.len() <= 4);
        // Must still be valid UTF-8
        let _ = result.to_string();
    }

    #[test]
    fn tavily_response_deserializes() {
        let json_str = r#"{"answer": "Rust is great", "results": [{"title": "Rust Lang", "url": "https://rust-lang.org", "content": "Systems programming language"}]}"#;
        let resp: TavilyResponse = serde_json::from_str(json_str).unwrap();
        assert_eq!(resp.answer.as_deref(), Some("Rust is great"));
        assert_eq!(resp.results.len(), 1);
        assert_eq!(resp.results[0].title, "Rust Lang");
    }

    #[test]
    fn tavily_response_empty() {
        let json_str = r#"{"results": []}"#;
        let resp: TavilyResponse = serde_json::from_str(json_str).unwrap();
        assert!(resp.answer.is_none());
        assert!(resp.results.is_empty());
    }

    #[test]
    fn brave_response_deserializes() {
        let json_str = r#"{"web": {"results": [{"title": "Test", "url": "https://example.com", "description": "A test result"}]}}"#;
        let resp: BraveResponse = serde_json::from_str(json_str).unwrap();
        let results = resp.web.unwrap().results;
        assert_eq!(results.len(), 1);
        assert_eq!(results[0].title, "Test");
    }

    #[test]
    fn brave_response_no_web() {
        let json_str = r#"{}"#;
        let resp: BraveResponse = serde_json::from_str(json_str).unwrap();
        assert!(resp.web.is_none());
    }

    #[test]
    fn ddg_response_deserializes() {
        let json_str = r#"{"Abstract": "Rust is a language", "AbstractURL": "https://rust-lang.org", "RelatedTopics": [{"Text": "Rust programming", "FirstURL": "https://example.com"}]}"#;
        let resp: DdgResponse = serde_json::from_str(json_str).unwrap();
        assert_eq!(resp.r#abstract, "Rust is a language");
        assert_eq!(resp.related_topics.len(), 1);
    }

    #[test]
    fn ddg_response_empty() {
        let json_str = r#"{"Abstract": "", "AbstractURL": "", "RelatedTopics": []}"#;
        let resp: DdgResponse = serde_json::from_str(json_str).unwrap();
        assert!(resp.r#abstract.is_empty());
        assert!(resp.related_topics.is_empty());
    }
}
