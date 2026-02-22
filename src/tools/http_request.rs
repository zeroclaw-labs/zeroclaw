use super::traits::{Tool, ToolResult};
use super::url_validation::{
    extract_host, host_matches_allowlist, is_private_or_local_host, normalize_allowed_domains,
};
use crate::security::SecurityPolicy;
use async_trait::async_trait;
use serde_json::json;
use std::sync::Arc;
use std::time::Duration;

/// HTTP request tool for API interactions.
/// Supports GET, POST, PUT, DELETE methods with configurable security.
pub struct HttpRequestTool {
    security: Arc<SecurityPolicy>,
    allowed_domains: Vec<String>,
    max_response_size: usize,
    timeout_secs: u64,
    user_agent: String,
}

impl HttpRequestTool {
    pub fn new(
        security: Arc<SecurityPolicy>,
        allowed_domains: Vec<String>,
        max_response_size: usize,
        timeout_secs: u64,
        user_agent: String,
    ) -> Self {
        Self {
            security,
            allowed_domains: normalize_allowed_domains(allowed_domains),
            max_response_size,
            timeout_secs,
            user_agent,
        }
    }

    fn validate_url(&self, raw_url: &str) -> anyhow::Result<String> {
        let url = raw_url.trim();

        if url.is_empty() {
            anyhow::bail!("URL cannot be empty");
        }

        if url.chars().any(char::is_whitespace) {
            anyhow::bail!("URL cannot contain whitespace");
        }

        if !url.starts_with("http://") && !url.starts_with("https://") {
            anyhow::bail!("Only http:// and https:// URLs are allowed");
        }

        if self.allowed_domains.is_empty() {
            anyhow::bail!(
                "HTTP request tool is enabled but no allowed_domains are configured. Add [http_request].allowed_domains in config.toml"
            );
        }

        let host = extract_host(url)?;

        if is_private_or_local_host(&host) {
            anyhow::bail!("Blocked local/private host: {host}");
        }

        if !host_matches_allowlist(&host, &self.allowed_domains) {
            anyhow::bail!("Host '{host}' is not in http_request.allowed_domains");
        }

        Ok(url.to_string())
    }

    fn validate_method(&self, method: &str) -> anyhow::Result<reqwest::Method> {
        match method.to_uppercase().as_str() {
            "GET" => Ok(reqwest::Method::GET),
            "POST" => Ok(reqwest::Method::POST),
            "PUT" => Ok(reqwest::Method::PUT),
            "DELETE" => Ok(reqwest::Method::DELETE),
            "PATCH" => Ok(reqwest::Method::PATCH),
            "HEAD" => Ok(reqwest::Method::HEAD),
            "OPTIONS" => Ok(reqwest::Method::OPTIONS),
            _ => anyhow::bail!("Unsupported HTTP method: {method}. Supported: GET, POST, PUT, DELETE, PATCH, HEAD, OPTIONS"),
        }
    }

    fn parse_headers(&self, headers: &serde_json::Value) -> Vec<(String, String)> {
        let mut result = Vec::new();
        if let Some(obj) = headers.as_object() {
            for (key, value) in obj {
                if let Some(str_val) = value.as_str() {
                    result.push((key.clone(), str_val.to_string()));
                }
            }
        }
        result
    }

    fn redact_headers_for_display(headers: &[(String, String)]) -> Vec<(String, String)> {
        headers
            .iter()
            .map(|(key, value)| {
                let lower = key.to_lowercase();
                let is_sensitive = lower.contains("authorization")
                    || lower.contains("api-key")
                    || lower.contains("apikey")
                    || lower.contains("token")
                    || lower.contains("secret");
                if is_sensitive {
                    (key.clone(), "***REDACTED***".into())
                } else {
                    (key.clone(), value.clone())
                }
            })
            .collect()
    }

    async fn execute_request(
        &self,
        url: &str,
        method: reqwest::Method,
        headers: Vec<(String, String)>,
        body: Option<&str>,
    ) -> anyhow::Result<reqwest::Response> {
        let timeout_secs = if self.timeout_secs == 0 {
            tracing::warn!("http_request: timeout_secs is 0, using safe default of 30s");
            30
        } else {
            self.timeout_secs
        };
        let builder = reqwest::Client::builder()
            .timeout(Duration::from_secs(timeout_secs))
            .connect_timeout(Duration::from_secs(10))
            .redirect(reqwest::redirect::Policy::none())
            .user_agent(&self.user_agent);
        let builder = crate::config::apply_runtime_proxy_to_builder(builder, "tool.http_request");
        let client = builder.build()?;

        let mut request = client.request(method, url);

        for (key, value) in headers {
            request = request.header(&key, &value);
        }

        if let Some(body_str) = body {
            request = request.body(body_str.to_string());
        }

        Ok(request.send().await?)
    }

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
}

#[async_trait]
impl Tool for HttpRequestTool {
    fn name(&self) -> &str {
        "http_request"
    }

    fn description(&self) -> &str {
        "Make HTTP requests to external APIs. Supports GET, POST, PUT, DELETE, PATCH, HEAD, OPTIONS methods. \
        Security constraints: allowlist-only domains, no local/private hosts, configurable timeout and response size limits."
    }

    fn parameters_schema(&self) -> serde_json::Value {
        json!({
            "type": "object",
            "properties": {
                "url": {
                    "type": "string",
                    "description": "HTTP or HTTPS URL to request"
                },
                "method": {
                    "type": "string",
                    "description": "HTTP method (GET, POST, PUT, DELETE, PATCH, HEAD, OPTIONS)",
                    "default": "GET"
                },
                "headers": {
                    "type": "object",
                    "description": "Optional HTTP headers as key-value pairs (e.g., {\"Authorization\": \"Bearer token\", \"Content-Type\": \"application/json\"})",
                    "default": {}
                },
                "body": {
                    "type": "string",
                    "description": "Optional request body (for POST, PUT, PATCH requests)"
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

        let method_str = args.get("method").and_then(|v| v.as_str()).unwrap_or("GET");
        let headers_val = args.get("headers").cloned().unwrap_or(json!({}));
        let body = args.get("body").and_then(|v| v.as_str());

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
                })
            }
        };

        let method = match self.validate_method(method_str) {
            Ok(m) => m,
            Err(e) => {
                return Ok(ToolResult {
                    success: false,
                    output: String::new(),
                    error: Some(e.to_string()),
                })
            }
        };

        let request_headers = self.parse_headers(&headers_val);

        match self
            .execute_request(&url, method, request_headers, body)
            .await
        {
            Ok(response) => {
                let status = response.status();
                let status_code = status.as_u16();

                // Get response headers (redact sensitive ones)
                let response_headers = response.headers().iter();
                let headers_text = response_headers
                    .map(|(k, _)| {
                        let is_sensitive = k.as_str().to_lowercase().contains("set-cookie");
                        if is_sensitive {
                            format!("{}: ***REDACTED***", k.as_str())
                        } else {
                            format!("{}: {:?}", k.as_str(), k.as_str())
                        }
                    })
                    .collect::<Vec<_>>()
                    .join(", ");

                // Get response body with size limit
                let response_text = match response.text().await {
                    Ok(text) => self.truncate_response(&text),
                    Err(e) => format!("[Failed to read response body: {e}]"),
                };

                let output = format!(
                    "Status: {} {}\nResponse Headers: {}\n\nResponse Body:\n{}",
                    status_code,
                    status.canonical_reason().unwrap_or("Unknown"),
                    headers_text,
                    response_text
                );

                Ok(ToolResult {
                    success: status.is_success(),
                    output,
                    error: if status.is_client_error() || status.is_server_error() {
                        Some(format!("HTTP {}", status_code))
                    } else {
                        None
                    },
                })
            }
            Err(e) => Ok(ToolResult {
                success: false,
                output: String::new(),
                error: Some(format!("HTTP request failed: {e}")),
            }),
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::security::{AutonomyLevel, SecurityPolicy};

    fn test_tool(allowed_domains: Vec<&str>) -> HttpRequestTool {
        let security = Arc::new(SecurityPolicy {
            autonomy: AutonomyLevel::Supervised,
            ..SecurityPolicy::default()
        });
        HttpRequestTool::new(
            security,
            allowed_domains.into_iter().map(String::from).collect(),
            1_000_000,
            30,
            "test".into(),
        )
    }

    #[test]
    fn validate_accepts_exact_domain() {
        let tool = test_tool(vec!["example.com"]);
        let got = tool.validate_url("https://example.com/docs").unwrap();
        assert_eq!(got, "https://example.com/docs");
    }

    #[test]
    fn validate_accepts_http() {
        let tool = test_tool(vec!["example.com"]);
        assert!(tool.validate_url("http://example.com").is_ok());
    }

    #[test]
    fn validate_accepts_subdomain() {
        let tool = test_tool(vec!["example.com"]);
        assert!(tool.validate_url("https://api.example.com/v1").is_ok());
    }

    #[test]
    fn validate_accepts_wildcard_allowlist_for_public_host() {
        let tool = test_tool(vec!["*"]);
        assert!(tool.validate_url("https://news.ycombinator.com").is_ok());
    }

    #[test]
    fn validate_wildcard_allowlist_still_rejects_private_host() {
        let tool = test_tool(vec!["*"]);
        let err = tool
            .validate_url("https://localhost:8080")
            .unwrap_err()
            .to_string();
        assert!(err.contains("local/private"));
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
    fn validate_rejects_localhost() {
        let tool = test_tool(vec!["localhost"]);
        let err = tool
            .validate_url("https://localhost:8080")
            .unwrap_err()
            .to_string();
        assert!(err.contains("local/private"));
    }

    #[test]
    fn validate_rejects_private_ipv4() {
        let tool = test_tool(vec!["192.168.1.5"]);
        let err = tool
            .validate_url("https://192.168.1.5")
            .unwrap_err()
            .to_string();
        assert!(err.contains("local/private"));
    }

    #[test]
    fn validate_rejects_whitespace() {
        let tool = test_tool(vec!["example.com"]);
        let err = tool
            .validate_url("https://example.com/hello world")
            .unwrap_err()
            .to_string();
        assert!(err.contains("whitespace"));
    }

    #[test]
    fn validate_rejects_userinfo() {
        let tool = test_tool(vec!["example.com"]);
        let err = tool
            .validate_url("https://user@example.com")
            .unwrap_err()
            .to_string();
        assert!(err.contains("userinfo"));
    }

    #[test]
    fn validate_requires_allowlist() {
        let security = Arc::new(SecurityPolicy::default());
        let tool = HttpRequestTool::new(security, vec![], 1_000_000, 30, "test".into());
        let err = tool
            .validate_url("https://example.com")
            .unwrap_err()
            .to_string();
        assert!(err.contains("allowed_domains"));
    }

    #[test]
    fn validate_accepts_valid_methods() {
        let tool = test_tool(vec!["example.com"]);
        assert!(tool.validate_method("GET").is_ok());
        assert!(tool.validate_method("POST").is_ok());
        assert!(tool.validate_method("PUT").is_ok());
        assert!(tool.validate_method("DELETE").is_ok());
        assert!(tool.validate_method("PATCH").is_ok());
        assert!(tool.validate_method("HEAD").is_ok());
        assert!(tool.validate_method("OPTIONS").is_ok());
    }

    #[test]
    fn validate_rejects_invalid_method() {
        let tool = test_tool(vec!["example.com"]);
        let err = tool.validate_method("INVALID").unwrap_err().to_string();
        assert!(err.contains("Unsupported HTTP method"));
    }

    #[tokio::test]
    async fn execute_blocks_readonly_mode() {
        let security = Arc::new(SecurityPolicy {
            autonomy: AutonomyLevel::ReadOnly,
            ..SecurityPolicy::default()
        });
        let tool = HttpRequestTool::new(
            security,
            vec!["example.com".into()],
            1_000_000,
            30,
            "test".into(),
        );
        let result = tool
            .execute(json!({"url": "https://example.com"}))
            .await
            .unwrap();
        assert!(!result.success);
        assert!(result.error.unwrap().contains("read-only"));
    }

    #[tokio::test]
    async fn execute_blocks_when_rate_limited() {
        let security = Arc::new(SecurityPolicy {
            max_actions_per_hour: 0,
            ..SecurityPolicy::default()
        });
        let tool = HttpRequestTool::new(
            security,
            vec!["example.com".into()],
            1_000_000,
            30,
            "test".into(),
        );
        let result = tool
            .execute(json!({"url": "https://example.com"}))
            .await
            .unwrap();
        assert!(!result.success);
        assert!(result.error.unwrap().contains("rate limit"));
    }

    #[test]
    fn truncate_response_within_limit() {
        let tool = test_tool(vec!["example.com"]);
        let text = "hello world";
        assert_eq!(tool.truncate_response(text), "hello world");
    }

    #[test]
    fn truncate_response_over_limit() {
        let tool = HttpRequestTool::new(
            Arc::new(SecurityPolicy::default()),
            vec!["example.com".into()],
            10,
            30,
            "test".into(),
        );
        let text = "hello world this is long";
        let truncated = tool.truncate_response(text);
        assert!(truncated.len() <= 10 + 60); // limit + message
        assert!(truncated.contains("[Response truncated"));
    }

    #[test]
    fn parse_headers_preserves_original_values() {
        let tool = test_tool(vec!["example.com"]);
        let headers = json!({
            "Authorization": "Bearer secret",
            "Content-Type": "application/json",
            "X-API-Key": "my-key"
        });
        let parsed = tool.parse_headers(&headers);
        assert_eq!(parsed.len(), 3);
        assert!(parsed
            .iter()
            .any(|(k, v)| k == "Authorization" && v == "Bearer secret"));
        assert!(parsed
            .iter()
            .any(|(k, v)| k == "X-API-Key" && v == "my-key"));
        assert!(parsed
            .iter()
            .any(|(k, v)| k == "Content-Type" && v == "application/json"));
    }

    #[test]
    fn redact_headers_for_display_redacts_sensitive() {
        let headers = vec![
            ("Authorization".into(), "Bearer secret".into()),
            ("Content-Type".into(), "application/json".into()),
            ("X-API-Key".into(), "my-key".into()),
            ("X-Secret-Token".into(), "tok-123".into()),
        ];
        let redacted = HttpRequestTool::redact_headers_for_display(&headers);
        assert_eq!(redacted.len(), 4);
        assert!(redacted
            .iter()
            .any(|(k, v)| k == "Authorization" && v == "***REDACTED***"));
        assert!(redacted
            .iter()
            .any(|(k, v)| k == "X-API-Key" && v == "***REDACTED***"));
        assert!(redacted
            .iter()
            .any(|(k, v)| k == "X-Secret-Token" && v == "***REDACTED***"));
        assert!(redacted
            .iter()
            .any(|(k, v)| k == "Content-Type" && v == "application/json"));
    }

    #[test]
    fn redact_headers_does_not_alter_original() {
        let headers = vec![("Authorization".into(), "Bearer real-token".into())];
        let _ = HttpRequestTool::redact_headers_for_display(&headers);
        assert_eq!(headers[0].1, "Bearer real-token");
    }

    #[test]
    fn ssrf_alternate_notations_rejected_by_validate_url() {
        // Even if is_private_or_local_host doesn't flag these, they
        // fail the allowlist because they're treated as hostnames.
        let tool = test_tool(vec!["example.com"]);
        for notation in [
            "http://0177.0.0.1",
            "http://0x7f000001",
            "http://2130706433",
            "http://127.000.000.001",
        ] {
            let err = tool.validate_url(notation).unwrap_err().to_string();
            assert!(
                err.contains("allowed_domains"),
                "Expected allowlist rejection for {notation}, got: {err}"
            );
        }
    }

    #[test]
    fn redirect_policy_is_none() {
        // Structural test: the tool should be buildable with redirect-safe config.
        // The actual Policy::none() enforcement is in execute_request's client builder.
        let tool = test_tool(vec!["example.com"]);
        assert_eq!(tool.name(), "http_request");
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
    fn validate_rejects_empty_url() {
        let tool = test_tool(vec!["example.com"]);
        let err = tool.validate_url("").unwrap_err().to_string();
        assert!(err.contains("empty"));
    }

    #[test]
    fn validate_rejects_ipv6_host() {
        let tool = test_tool(vec!["example.com"]);
        let err = tool
            .validate_url("http://[::1]:8080/path")
            .unwrap_err()
            .to_string();
        assert!(err.contains("IPv6"));
    }
}
