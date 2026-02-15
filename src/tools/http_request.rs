use super::traits::{Tool, ToolResult};
use async_trait::async_trait;
use reqwest::Client;
use serde_json::json;
use std::time::Duration;

/// Maximum HTTP request time before timeout.
const REQUEST_TIMEOUT_SECS: u64 = 30;
/// Maximum response body size in bytes (1MB).
const MAX_RESPONSE_BYTES: usize = 1_048_576;
/// Maximum allowed URL length to prevent abuse.
const MAX_URL_LENGTH: usize = 2048;

/// HTTP request tool for making API calls
pub struct HttpRequestTool {
    client: Client,
}

impl HttpRequestTool {
    pub fn new() -> Self {
        Self {
            client: Client::builder()
                .timeout(Duration::from_secs(REQUEST_TIMEOUT_SECS))
                .connect_timeout(Duration::from_secs(10))
                .redirect(reqwest::redirect::Policy::limited(5))
                .build()
                .unwrap_or_else(|_| Client::new()),
        }
    }

    fn validate_url(url: &str) -> anyhow::Result<()> {
        if url.is_empty() {
            anyhow::bail!("URL cannot be empty");
        }

        if url.len() > MAX_URL_LENGTH {
            anyhow::bail!("URL exceeds maximum length of {MAX_URL_LENGTH} characters");
        }

        if !url.starts_with("https://") && !url.starts_with("http://") {
            anyhow::bail!("Only http:// and https:// URLs are allowed");
        }

        // Block private/internal network access
        let host = extract_host(url);
        if is_private_host(&host) {
            anyhow::bail!("Requests to private/internal networks are blocked: {host}");
        }

        Ok(())
    }
}

#[async_trait]
impl Tool for HttpRequestTool {
    fn name(&self) -> &str {
        "http_request"
    }

    fn description(&self) -> &str {
        "Make HTTP requests to external APIs. Supports GET, POST, PUT, PATCH, DELETE methods. \
         Returns status code, headers, and response body. Blocks requests to private networks."
    }

    fn parameters_schema(&self) -> serde_json::Value {
        json!({
            "type": "object",
            "properties": {
                "url": {
                    "type": "string",
                    "description": "The URL to request (must be http:// or https://)"
                },
                "method": {
                    "type": "string",
                    "enum": ["GET", "POST", "PUT", "PATCH", "DELETE", "HEAD"],
                    "description": "HTTP method (default: GET)"
                },
                "headers": {
                    "type": "object",
                    "description": "Request headers as key-value pairs"
                },
                "body": {
                    "type": "string",
                    "description": "Request body (for POST, PUT, PATCH)"
                },
                "json": {
                    "type": "object",
                    "description": "JSON request body (sets Content-Type: application/json)"
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

        if let Err(e) = Self::validate_url(url) {
            return Ok(ToolResult {
                success: false,
                output: String::new(),
                error: Some(e.to_string()),
            });
        }

        let method = args
            .get("method")
            .and_then(|v| v.as_str())
            .unwrap_or("GET")
            .to_uppercase();

        let mut request = match method.as_str() {
            "GET" => self.client.get(url),
            "POST" => self.client.post(url),
            "PUT" => self.client.put(url),
            "PATCH" => self.client.patch(url),
            "DELETE" => self.client.delete(url),
            "HEAD" => self.client.head(url),
            _ => {
                return Ok(ToolResult {
                    success: false,
                    output: String::new(),
                    error: Some(format!("Unsupported HTTP method: {method}")),
                });
            }
        };

        // Add custom headers
        if let Some(headers) = args.get("headers").and_then(|v| v.as_object()) {
            for (key, value) in headers {
                if let Some(val) = value.as_str() {
                    request = request.header(key.as_str(), val);
                }
            }
        }

        // Add body
        if let Some(json_body) = args.get("json") {
            request = request.json(json_body);
        } else if let Some(body) = args.get("body").and_then(|v| v.as_str()) {
            request = request.body(body.to_string());
        }

        match request.send().await {
            Ok(resp) => {
                let status = resp.status();
                let status_code = status.as_u16();

                // Collect response headers
                let headers: Vec<String> = resp
                    .headers()
                    .iter()
                    .take(20)
                    .map(|(k, v)| format!("{}: {}", k, v.to_str().unwrap_or("<binary>")))
                    .collect();

                let mut body = resp.text().await.unwrap_or_default();

                if body.len() > MAX_RESPONSE_BYTES {
                    body.truncate(MAX_RESPONSE_BYTES);
                    body.push_str("\n... [response truncated at 1MB]");
                }

                let output = format!(
                    "HTTP {status_code} {}\n\nHeaders:\n{}\n\nBody:\n{body}",
                    status.canonical_reason().unwrap_or(""),
                    headers.join("\n"),
                );

                Ok(ToolResult {
                    success: status.is_success(),
                    output,
                    error: if status.is_success() {
                        None
                    } else {
                        Some(format!("HTTP {status_code}"))
                    },
                })
            }
            Err(e) => Ok(ToolResult {
                success: false,
                output: String::new(),
                error: Some(format!("Request failed: {e}")),
            }),
        }
    }
}

// ── Helpers ─────────────────────────────────────────────────────

fn extract_host(url: &str) -> String {
    let without_scheme = url
        .strip_prefix("https://")
        .or_else(|| url.strip_prefix("http://"))
        .unwrap_or(url);

    without_scheme
        .split('/')
        .next()
        .unwrap_or(without_scheme)
        .split(':')
        .next()
        .unwrap_or(without_scheme)
        .to_lowercase()
}

fn is_private_host(host: &str) -> bool {
    let private_patterns = [
        "localhost",
        "127.",
        "10.",
        "192.168.",
        "172.16.",
        "172.17.",
        "172.18.",
        "172.19.",
        "172.20.",
        "172.21.",
        "172.22.",
        "172.23.",
        "172.24.",
        "172.25.",
        "172.26.",
        "172.27.",
        "172.28.",
        "172.29.",
        "172.30.",
        "172.31.",
        "0.0.0.0",
        "::1",
        "[::1]",
    ];

    private_patterns
        .iter()
        .any(|p| host.starts_with(p) || host == *p)
}

#[cfg(test)]
mod tests {
    use super::*;

    fn tool() -> HttpRequestTool {
        HttpRequestTool::new()
    }

    #[test]
    fn http_request_tool_name() {
        assert_eq!(tool().name(), "http_request");
    }

    #[test]
    fn http_request_tool_description() {
        assert!(!tool().description().is_empty());
    }

    #[test]
    fn http_request_tool_schema_has_url() {
        let schema = tool().parameters_schema();
        assert!(schema["properties"]["url"].is_object());
        assert!(schema["required"]
            .as_array()
            .unwrap()
            .contains(&json!("url")));
    }

    #[test]
    fn http_request_tool_schema_has_method() {
        let schema = tool().parameters_schema();
        assert!(schema["properties"]["method"].is_object());
    }

    #[test]
    fn http_request_tool_spec_roundtrip() {
        let t = tool();
        let spec = t.spec();
        assert_eq!(spec.name, "http_request");
        assert!(spec.parameters.is_object());
    }

    #[tokio::test]
    async fn http_request_missing_url() {
        let result = tool().execute(json!({})).await;
        assert!(result.is_err());
        assert!(result.unwrap_err().to_string().contains("url"));
    }

    #[tokio::test]
    async fn http_request_wrong_type_url() {
        let result = tool().execute(json!({"url": 123})).await;
        assert!(result.is_err());
    }

    #[tokio::test]
    async fn http_request_empty_url() {
        let result = tool().execute(json!({"url": ""})).await.unwrap();
        assert!(!result.success);
        assert!(result.error.as_ref().unwrap().contains("empty"));
    }

    #[tokio::test]
    async fn http_request_invalid_scheme() {
        let result = tool()
            .execute(json!({"url": "ftp://example.com"}))
            .await
            .unwrap();
        assert!(!result.success);
        assert!(result.error.as_ref().unwrap().contains("http"));
    }

    #[tokio::test]
    async fn http_request_blocks_localhost() {
        let result = tool()
            .execute(json!({"url": "http://localhost:8080/api"}))
            .await
            .unwrap();
        assert!(!result.success);
        assert!(result.error.as_ref().unwrap().contains("private"));
    }

    #[tokio::test]
    async fn http_request_blocks_private_ip() {
        let result = tool()
            .execute(json!({"url": "http://192.168.1.1/admin"}))
            .await
            .unwrap();
        assert!(!result.success);
        assert!(result.error.as_ref().unwrap().contains("private"));
    }

    #[tokio::test]
    async fn http_request_blocks_loopback() {
        let result = tool()
            .execute(json!({"url": "http://127.0.0.1:3000"}))
            .await
            .unwrap();
        assert!(!result.success);
        assert!(result.error.as_ref().unwrap().contains("private"));
    }

    #[tokio::test]
    async fn http_request_unsupported_method() {
        let result = tool()
            .execute(json!({"url": "https://example.com", "method": "TRACE"}))
            .await
            .unwrap();
        assert!(!result.success);
        assert!(result.error.as_ref().unwrap().contains("Unsupported"));
    }

    #[tokio::test]
    async fn http_request_url_too_long() {
        let long_url = format!("https://example.com/{}", "a".repeat(2100));
        let result = tool().execute(json!({"url": long_url})).await.unwrap();
        assert!(!result.success);
        assert!(result.error.as_ref().unwrap().contains("length"));
    }

    #[test]
    fn validate_url_accepts_https() {
        assert!(HttpRequestTool::validate_url("https://api.example.com/v1").is_ok());
    }

    #[test]
    fn validate_url_accepts_http() {
        assert!(HttpRequestTool::validate_url("http://api.example.com/v1").is_ok());
    }

    #[test]
    fn validate_url_rejects_ftp() {
        assert!(HttpRequestTool::validate_url("ftp://files.example.com").is_err());
    }

    #[test]
    fn extract_host_works() {
        assert_eq!(
            extract_host("https://api.example.com/v1"),
            "api.example.com"
        );
        assert_eq!(extract_host("http://HOST:8080/path"), "host");
    }

    #[test]
    fn is_private_host_detects_local() {
        assert!(is_private_host("localhost"));
        assert!(is_private_host("127.0.0.1"));
        assert!(is_private_host("192.168.1.1"));
        assert!(is_private_host("10.0.0.1"));
        assert!(!is_private_host("example.com"));
    }
}
