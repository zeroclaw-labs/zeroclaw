use super::traits::{Tool, ToolResult};
use crate::security::SecurityPolicy;
use async_trait::async_trait;
use serde_json::json;
use std::sync::Arc;
use std::time::Duration;

/// X (formerly Twitter) search tool powered by the xAI Responses API.
/// Searches X posts, users, and threads via the `x_search` server-side tool.
pub struct XSearchTool {
    security: Arc<SecurityPolicy>,
    api_key: Option<String>,
    api_url: Option<String>,
    model: String,
    max_results: usize,
    timeout_secs: u64,
    user_agent: String,
}

impl XSearchTool {
    pub fn new(
        security: Arc<SecurityPolicy>,
        api_key: Option<String>,
        api_url: Option<String>,
        model: String,
        max_results: usize,
        timeout_secs: u64,
        user_agent: String,
    ) -> Self {
        Self {
            security,
            api_key,
            api_url,
            model,
            max_results: max_results.clamp(1, 10),
            timeout_secs: timeout_secs.max(1),
            user_agent,
        }
    }

    async fn search(&self, query: &str) -> anyhow::Result<String> {
        let auth_token = match self.api_key.as_ref() {
            Some(raw) if !raw.trim().is_empty() => raw.trim(),
            _ => anyhow::bail!(
                "xAI API key not configured. Set [x_search].xai_api_key in config.toml"
            ),
        };

        let api_url = self
            .api_url
            .as_deref()
            .map(str::trim)
            .filter(|s| !s.is_empty())
            .unwrap_or("https://api.x.ai/v1/responses");

        let client = reqwest::Client::builder()
            .timeout(Duration::from_secs(self.timeout_secs))
            .user_agent(self.user_agent.as_str())
            .build()?;

        let response = client
            .post(api_url)
            .header(
                reqwest::header::AUTHORIZATION,
                format!("Bearer {auth_token}"),
            )
            .header(reqwest::header::CONTENT_TYPE, "application/json")
            .json(&json!({
                "model": self.model,
                "input": [{"role": "user", "content": query}],
                "tools": [{"type": "x_search"}],
            }))
            .send()
            .await
            .map_err(|e| anyhow::anyhow!("X search request failed: {e}"))?;

        if !response.status().is_success() {
            let status = response.status();
            let body = response.text().await.unwrap_or_default();
            anyhow::bail!("X search failed with status {}: {}", status.as_u16(), body);
        }

        let json: serde_json::Value = response.json().await?;
        self.parse_results(&json, query)
    }

    fn parse_results(&self, json: &serde_json::Value, query: &str) -> anyhow::Result<String> {
        let mut summary = String::new();
        let mut citations: Vec<(String, String)> = Vec::new(); // (title, url)

        if let Some(output) = json.get("output").and_then(|o| o.as_array()) {
            for item in output {
                if item.get("type").and_then(|t| t.as_str()) != Some("message") {
                    continue;
                }
                if let Some(contents) = item.get("content").and_then(|c| c.as_array()) {
                    for content in contents {
                        if content.get("type").and_then(|t| t.as_str()) == Some("output_text") {
                            if let Some(text) = content.get("text").and_then(|t| t.as_str()) {
                                if summary.is_empty() {
                                    summary = text.to_string();
                                }
                            }
                            if let Some(annotations) =
                                content.get("annotations").and_then(|a| a.as_array())
                            {
                                for ann in annotations {
                                    if ann.get("type").and_then(|t| t.as_str())
                                        == Some("url_citation")
                                    {
                                        let url = ann
                                            .get("url")
                                            .and_then(|u| u.as_str())
                                            .unwrap_or("")
                                            .to_string();
                                        let title = ann
                                            .get("title")
                                            .and_then(|t| t.as_str())
                                            .unwrap_or("")
                                            .to_string();
                                        if !url.is_empty()
                                            && !citations.iter().any(|(_, u)| u == &url)
                                        {
                                            citations.push((title, url));
                                        }
                                    }
                                }
                            }
                        }
                    }
                }
            }
        }

        if summary.is_empty() && citations.is_empty() {
            return Ok(format!("No X results found for: {}", query));
        }

        let mut lines = vec![format!("X search results for: {} (via Grok)", query)];

        if !summary.is_empty() {
            lines.push(String::new());
            lines.push(summary);
        }

        if !citations.is_empty() {
            lines.push(String::new());
            lines.push("Sources:".to_string());
            for (i, (title, url)) in citations.iter().take(self.max_results).enumerate() {
                if title.is_empty() {
                    lines.push(format!("{}. {}", i + 1, url));
                } else {
                    lines.push(format!("{}. {}", i + 1, title));
                    lines.push(format!("   {}", url));
                }
            }
        }

        Ok(lines.join("\n"))
    }
}

#[async_trait]
impl Tool for XSearchTool {
    fn name(&self) -> &str {
        "x_search_tool"
    }

    fn description(&self) -> &str {
        "Search X (formerly Twitter) for posts, users, and threads. Returns relevant results with summaries and source links. Use this to find current discussions, opinions, or news on X."
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

        tracing::info!("Searching X for: {}", query);

        let result = self.search(query).await?;

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

    fn test_tool() -> XSearchTool {
        XSearchTool::new(
            test_security(),
            None,
            None,
            "grok-4-1-fast-non-reasoning".to_string(),
            5,
            15,
            "test".to_string(),
        )
    }

    #[test]
    fn test_tool_name() {
        assert_eq!(test_tool().name(), "x_search_tool");
    }

    #[test]
    fn test_tool_description() {
        assert!(test_tool().description().contains("X (formerly Twitter)"));
    }

    #[test]
    fn test_parameters_schema() {
        let schema = test_tool().parameters_schema();
        assert_eq!(schema["type"], "object");
        assert!(schema["properties"]["query"].is_object());
    }

    #[tokio::test]
    async fn test_execute_missing_query() {
        let result = test_tool().execute(json!({})).await;
        assert!(result.is_err());
    }

    #[tokio::test]
    async fn test_execute_empty_query() {
        let result = test_tool().execute(json!({"query": ""})).await;
        assert!(result.is_err());
    }

    #[tokio::test]
    async fn test_execute_without_api_key() {
        let result = test_tool().execute(json!({"query": "test"})).await;
        assert!(result.is_err());
        assert!(result.unwrap_err().to_string().contains("xAI API key"));
    }

    #[tokio::test]
    async fn test_execute_blocked_in_read_only_mode() {
        let security = Arc::new(SecurityPolicy {
            autonomy: AutonomyLevel::ReadOnly,
            ..SecurityPolicy::default()
        });
        let tool = XSearchTool::new(
            security,
            None,
            None,
            "grok-4-1-fast-non-reasoning".to_string(),
            5,
            15,
            "test".to_string(),
        );
        let result = tool.execute(json!({"query": "rust"})).await.unwrap();
        assert!(!result.success);
        assert!(result.error.unwrap().contains("read-only"));
    }

    #[test]
    fn test_parse_results_empty() {
        let result = test_tool().parse_results(&serde_json::json!({}), "test").unwrap();
        assert!(result.contains("No X results found"));
    }

    #[test]
    fn test_parse_results_with_annotations() {
        let json = serde_json::json!({
            "output": [{
                "type": "message",
                "role": "assistant",
                "content": [{
                    "type": "output_text",
                    "text": "Rust is a systems programming language.",
                    "annotations": [{
                        "type": "url_citation",
                        "url": "https://x.com/zeroclaw_user/status/1",
                        "title": "zeroclaw_user on X",
                        "start_index": 0,
                        "end_index": 4
                    }]
                }]
            }]
        });
        let result = test_tool().parse_results(&json, "rust").unwrap();
        assert!(result.contains("via Grok"));
        assert!(result.contains("Rust is a systems programming language."));
        assert!(result.contains("zeroclaw_user on X"));
        assert!(result.contains("https://x.com/zeroclaw_user/status/1"));
    }

    #[test]
    fn test_constructor_clamps_limits() {
        let tool = XSearchTool::new(
            test_security(),
            None,
            None,
            "grok-4-1-fast-non-reasoning".to_string(),
            0,
            0,
            "test".to_string(),
        );
        assert_eq!(tool.max_results, 1);
        assert_eq!(tool.timeout_secs, 1);
    }
}
