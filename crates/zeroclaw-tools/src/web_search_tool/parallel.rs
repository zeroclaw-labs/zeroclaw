use super::{
    WebSearchTool, cap_result_content, http_search_failure, no_results_message, render_results,
    results_header,
};
use crate::i18n::get_required_tool_string;
use crate::mcp_client::McpServer;
use crate::mcp_transport::{HttpTransport, McpHttpStatus};
use serde::Deserialize;
use serde_json::{Value, json};
use std::{sync::Arc, time::Duration};
use zeroclaw_config::schema::{McpServerConfig, McpTransport, try_apply_runtime_proxy_to_builder};

pub(super) const URL: &str = "https://mcp.parallel.ai/mcp?tools=web_search";

impl WebSearchTool {
    pub(super) async fn search_parallel(&self, url: &str, query: &str) -> anyhow::Result<String> {
        let config = McpServerConfig {
            name: "parallel".into(),
            transport: McpTransport::Http,
            url: Some(url.into()),
            headers: [("User-Agent".into(), "ZeroClaw".into())].into(),
            max_response_bytes: Some(1024 * 1024),
            tool_timeout_secs: Some(self.timeout_secs),
            ..Default::default()
        };
        let builder =
            try_apply_runtime_proxy_to_builder(reqwest::Client::builder(), "tool.web_search")?;
        let transport = HttpTransport::with_builder(&config, builder)?;
        let call = async {
            let server = McpServer::connect_with_transport(config, Arc::new(transport)).await?;
            server
                .call_tool(
                    "web_search",
                    json!({"objective": query, "search_queries": [query]}),
                )
                .await
        };
        let result = tokio::time::timeout(Duration::from_secs(self.timeout_secs), call)
            .await
            .map_err(|_| {
                anyhow::Error::msg(get_required_tool_string(
                    "tool-web-search-tool-error-parallel-timeout",
                ))
            })?
            .map_err(|err| match err.downcast_ref::<McpHttpStatus>() {
                Some(status) => http_search_failure("parallel", status.0),
                None => err,
            })?;
        format_response(query, self.max_results, &result)
    }
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

fn format_response(query: &str, max_results: usize, result: &Value) -> anyhow::Result<String> {
    let invalid = || {
        anyhow::Error::msg(get_required_tool_string(
            "tool-web-search-tool-error-parallel-invalid-response",
        ))
    };
    let response: SearchResponse = match result.get("structuredContent") {
        Some(payload) if !payload.is_null() => SearchResponse::deserialize(payload),
        _ => {
            let blocks = result["content"].as_array().ok_or_else(invalid)?;
            let text = blocks
                .iter()
                .filter(|b| b["type"] == "text")
                .filter_map(|b| b["text"].as_str())
                .collect::<Vec<_>>()
                .join("\n");
            serde_json::from_str(&text)
        }
    }
    .map_err(|_| invalid())?;
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
        Mock, MockServer, Request, ResponseTemplate,
        matchers::{header, method},
    };
    use zeroclaw_config::schema::{ProxyConfig, ProxyScope, set_runtime_proxy_config};

    #[tokio::test]
    async fn search_uses_service_proxy_and_reports_http_errors() {
        let _guard = crate::test_support::RuntimeProxyStateGuard::acquire().await;
        let proxy = MockServer::start().await;
        set_runtime_proxy_config(ProxyConfig {
            enabled: true,
            scope: ProxyScope::Services,
            services: vec!["tool.web_search".into()],
            all_proxy: Some(proxy.uri()),
            no_proxy: vec!["localhost".into(), "127.0.0.1".into(), "::1".into()],
            ..Default::default()
        });
        Mock::given(method("POST")).and(header("user-agent", "ZeroClaw"))
            .respond_with(|request: &Request| {
                let body: Value = request.body_json().unwrap();
                let result = match body["method"].as_str().unwrap() {
                    "initialize" => json!({"capabilities":{"tools":{}}}),
                    "notifications/initialized" => return ResponseTemplate::new(202),
                    "tools/list" => json!({"tools":[{"name":"web_search", "inputSchema":{"type":"object"}}]}),
                    "tools/call" => {
                        assert_eq!(body["params"]["name"], "web_search");
                        let args = &body["params"]["arguments"];
                        assert_eq!(args["search_queries"], json!([args["objective"]]));
                        if args["objective"] == "limited" {
                            return ResponseTemplate::new(429);
                        }
                        let hit = json!({"results":[{"url":"https://example.org","excerpts":["passage"]}]});
                        json!({"content":[{"type":"text","text":hit.to_string()}]})
                    }
                    other => panic!("unexpected MCP method {other}"),
                };
                ResponseTemplate::new(200).set_body_json(json!({"jsonrpc":"2.0","id":body["id"],"result":result}))
            }).expect(8).mount(&proxy).await;
        let tool = WebSearchTool::new("parallel".into(), None, None, 1, 5);
        let endpoint = "http://search.invalid/mcp";
        let output = tool.search_parallel(endpoint, "example").await.unwrap();
        assert!(output.contains("passage"));
        let err = tool.search_parallel(endpoint, "limited").await.unwrap_err();
        assert!(
            err.to_string()
                .contains("search_status=unavailable, http=429")
        );
    }

    #[test]
    fn structured_results_respect_limit_and_reject_malformed_payloads() {
        let hits = json!({"structuredContent":{"results":[
            {"url":"https://example.org/first","title":"First","excerpts":["one","two"]},
            {"url":"https://example.org/second","excerpts":[]}
        ]}});
        let output = format_response("q", 1, &hits).unwrap();
        assert!(
            output.contains("First") && output.contains("one\ntwo") && !output.contains("second")
        );
        assert_eq!(
            format_response("q", 1, &json!({"structuredContent":{"results":[]}})).unwrap(),
            no_results_message("q")
        );
        assert!(format_response("q", 1, &json!({"content":[]})).is_err());
    }
}
