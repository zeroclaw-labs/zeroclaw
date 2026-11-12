use super::{cap_result_content, no_results_message, render_results, results_header};
use crate::i18n::get_required_tool_string;
use crate::mcp_client::McpServer;
use serde::Deserialize;
use serde_json::{Value, json};
use std::time::Duration;
use zeroclaw_config::schema::{McpServerConfig, McpTransport};

pub(super) async fn search(
    query: &str,
    max_results: usize,
    timeout_secs: u64,
) -> anyhow::Result<String> {
    search_at(
        "https://mcp.parallel.ai/mcp?tools=web_search",
        query,
        max_results,
        timeout_secs,
    )
    .await
}

async fn search_at(
    url: &str,
    query: &str,
    max_results: usize,
    timeout_secs: u64,
) -> anyhow::Result<String> {
    let config = McpServerConfig {
        name: "parallel".into(),
        transport: McpTransport::Http,
        url: Some(url.into()),
        headers: [("User-Agent".into(), "ZeroClaw".into())].into(),
        max_response_bytes: Some(1024 * 1024),
        tool_timeout_secs: Some(timeout_secs),
        ..Default::default()
    };
    // Bound the entire search, including the MCP handshake and discovery.
    let result = tokio::time::timeout(Duration::from_secs(timeout_secs), async {
        let server = McpServer::connect(config).await?;
        server
            .call_tool(
                "web_search",
                json!({"objective": query, "search_queries": [query]}),
            )
            .await
    })
    .await
    .map_err(|_| {
        anyhow::anyhow!(get_required_tool_string(
            "tool-web-search-tool-error-parallel-timeout"
        ))
    })??;
    format_response(query, max_results, result)
}

#[derive(Deserialize)]
struct SearchResponse {
    results: Vec<SearchResult>,
}

#[derive(Deserialize)]
struct SearchResult {
    url: String,
    title: Option<String>,
    excerpts: Vec<String>,
}

fn format_response(query: &str, max_results: usize, mut result: Value) -> anyhow::Result<String> {
    let invalid = || {
        anyhow::anyhow!(get_required_tool_string(
            "tool-web-search-tool-error-parallel-invalid-response"
        ))
    };
    let response: SearchResponse = match result.get_mut("structuredContent") {
        Some(payload) if !payload.is_null() => {
            serde_json::from_value(payload.take()).map_err(|_| invalid())?
        }
        _ => {
            let blocks = result["content"].as_array().ok_or_else(invalid)?;
            let text = blocks
                .iter()
                .filter(|b| b["type"] == "text")
                .filter_map(|b| b["text"].as_str())
                .collect::<Vec<_>>()
                .join("\n");
            serde_json::from_str(&text).map_err(|_| invalid())?
        }
    };
    if response.results.is_empty() {
        return Ok(no_results_message(query));
    }
    let mut blocks = Vec::new();
    for (index, hit) in response.results.into_iter().take(max_results).enumerate() {
        let url = reqwest::Url::parse(&hit.url).map_err(|_| invalid())?;
        anyhow::ensure!(
            matches!(url.scheme(), "https" | "http")
                && url.has_host()
                && !hit.url.chars().any(char::is_control),
            invalid()
        );
        let title = hit.title.as_deref().unwrap_or(&hit.url);
        let mut block = vec![
            format!("{}. {}", index + 1, title),
            format!("   {}", hit.url),
        ];
        if !hit.excerpts.is_empty() {
            block.push(format!(
                "   {}",
                cap_result_content(&hit.excerpts.join("\n"))
            ));
        }
        blocks.push(block);
    }
    Ok(render_results(results_header(query, "Parallel"), blocks))
}

#[cfg(test)]
mod tests {
    use super::*;
    use wiremock::{
        Mock, MockServer, ResponseTemplate,
        matchers::{body_partial_json, header, method},
    };

    #[tokio::test]
    #[ignore = "calls the public Parallel MCP endpoint"]
    async fn live_search_via_configured_provider() {
        use super::super::WebSearchTool;
        use zeroclaw_api::tool::Tool;
        use zeroclaw_config::schema::WebSearchConfig;

        let config: WebSearchConfig = toml::from_str(r#"search_provider = "parallel""#).unwrap();
        let tool = WebSearchTool::new(
            config.search_provider,
            None,
            None,
            config.max_results,
            config.timeout_secs,
        );
        let result = tool
            .execute(json!({"query": "Rust standard library Vec documentation"}))
            .await
            .unwrap();
        assert!(
            result.success
                && result.output.contains("(via Parallel)")
                && result.output.contains("https://")
        );
    }

    #[tokio::test]
    async fn sends_only_search_with_the_project_header() {
        let mock = MockServer::start().await;
        for (request, response) in [
            (
                json!({"method":"initialize"}),
                json!({"id":1,"result":{"capabilities":{"tools":{}}}}),
            ),
            (json!({"method":"notifications/initialized"}), Value::Null),
            (
                json!({"method":"tools/list"}),
                json!({"id":2,"result":{"tools":[{"name":"web_search","inputSchema":{"type":"object"}}]}}),
            ),
            (
                json!({"method":"tools/call","params":{"name":"web_search","arguments":{"objective":"query","search_queries":["query"]}}}),
                json!({"id":3,"result":{"content":[{"type":"text","text":json!({"results":[{"url":"https://example.org","excerpts":["passage"]}]}).to_string()}]}}),
            ),
        ] {
            let reply = if response.is_null() {
                ResponseTemplate::new(202)
            } else {
                let mut response = response;
                response["jsonrpc"] = json!("2.0");
                ResponseTemplate::new(200).set_body_json(response)
            };
            Mock::given(method("POST"))
                .and(header("user-agent", "ZeroClaw"))
                .and(body_partial_json(request))
                .respond_with(reply)
                .expect(1)
                .mount(&mock)
                .await;
        }
        let output = search_at(&mock.uri(), "query", 1, 5).await.unwrap();
        assert!(output.contains("(via Parallel)") && output.contains("passage"));
    }

    #[test]
    fn structured_results_respect_limit_and_reject_malformed_payloads() {
        let hits = json!({"structuredContent":{"results":[
            {"url":"https://example.org/first","title":"First","excerpts":["one","two"]},
            {"url":"https://example.org/second","excerpts":[]}
        ]}});
        let output = format_response("q", 1, hits).unwrap();
        assert!(
            output.contains("First") && output.contains("one\ntwo") && !output.contains("second")
        );
        assert_eq!(
            format_response("q", 1, json!({"structuredContent":{"results":[]}})).unwrap(),
            no_results_message("q")
        );
        assert!(format_response("q", 1, json!({"content":[]})).is_err());
    }
}
