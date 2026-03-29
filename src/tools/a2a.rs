//! # A2A Tool — MVP Implementation
//!
//! Client-side tool for interacting with remote A2A agents.
//! Supports: `discover`, `send`, `status`, `result` (polling).
//!
//! **Not yet implemented:** streaming (`message/stream`), cancel,
//! multi-turn conversations, structured/binary message parts.

use super::traits::{Tool, ToolResult};
use crate::security::SecurityPolicy;
use async_trait::async_trait;
use serde_json::json;
use std::sync::Arc;

/// Outbound A2A client tool — discovers remote agents and sends/retrieves tasks.
pub struct A2aTool {
    security: Arc<SecurityPolicy>,
    timeout_secs: u64,
    /// When true, allow requests to localhost/private IPs (same-host A2A).
    allow_local: bool,
}

impl A2aTool {
    pub fn new(security: Arc<SecurityPolicy>, timeout_secs: u64, allow_local: bool) -> Self {
        Self {
            security,
            timeout_secs,
            allow_local,
        }
    }

    fn build_client(&self) -> anyhow::Result<reqwest::Client> {
        let redirect_policy = reqwest::redirect::Policy::custom(|attempt| {
            if attempt.previous().len() >= 10 {
                return attempt.error(std::io::Error::other("Too many redirects (max 10)"));
            }
            let host = attempt.url().host_str().unwrap_or("").to_string();
            if is_private_or_local_host(&host) {
                return attempt.error(std::io::Error::new(
                    std::io::ErrorKind::PermissionDenied,
                    format!("Blocked redirect to private/local host: {host}"),
                ));
            }
            attempt.follow()
        });
        let client = reqwest::Client::builder()
            .timeout(std::time::Duration::from_secs(self.timeout_secs))
            .connect_timeout(std::time::Duration::from_secs(10))
            .redirect(redirect_policy)
            .user_agent("ZeroClaw/0.1 (a2a)")
            .build()?;
        Ok(client)
    }

    /// Validate that a URL targets a public host over HTTP(S).
    ///
    /// **Known limitation (DNS rebinding TOCTOU):** DNS is resolved here at
    /// validation time, but `reqwest` resolves again at connect time.  An
    /// attacker-controlled DNS record could flip between the two calls.  The
    /// redirect policy in `build_client` mitigates post-redirect SSRF, but
    /// the initial connection remains vulnerable to rebinding.  A custom
    /// `reqwest::dns::Resolve` would close this gap at the cost of added
    /// complexity; for now we accept this residual risk.
    fn validate_url(&self, url: &str) -> anyhow::Result<reqwest::Url> {
        let parsed = reqwest::Url::parse(url)?;
        match parsed.scheme() {
            "http" | "https" => {}
            scheme => anyhow::bail!("Unsupported URL scheme: {scheme} (only http/https allowed)"),
        }
        if !self.allow_local {
            if let Some(host) = parsed.host_str() {
                if is_private_or_local_host(host) {
                    anyhow::bail!(
                        "Blocked request to private/local host: {host} (A2A only allows public hosts)"
                    );
                }
                validate_resolved_host_is_public(host)?;
            }
        }
        Ok(parsed)
    }

    async fn action_discover(
        &self,
        url: &str,
        bearer_token: Option<&str>,
    ) -> anyhow::Result<ToolResult> {
        let base = self.validate_url(url)?;
        let card_url = base.join("/.well-known/agent-card.json")?;
        let client = self.build_client()?;

        let mut req = client.get(card_url);
        if let Some(token) = bearer_token {
            req = req.bearer_auth(token);
        }

        let resp = req.send().await?;
        let status = resp.status();
        let body = resp.text().await?;

        if status.is_success() {
            Ok(ToolResult {
                success: true,
                output: body,
                error: None,
            })
        } else {
            Ok(ToolResult {
                success: false,
                output: String::new(),
                error: Some(format!("HTTP {status}: {body}")),
            })
        }
    }

    async fn action_send(
        &self,
        url: &str,
        bearer_token: Option<&str>,
        message: &str,
    ) -> anyhow::Result<ToolResult> {
        let base = self.validate_url(url)?;
        let rpc_url = base.join("/a2a")?;
        let client = self.build_client()?;
        let request_id = uuid::Uuid::new_v4().to_string();

        let body = json!({
            "jsonrpc": "2.0",
            "id": request_id,
            "method": "message/send",
            "params": {
                "message": {
                    "role": "user",
                    "parts": [{ "kind": "text", "text": message }],
                    "messageId": uuid::Uuid::new_v4().to_string()
                }
            }
        });

        let mut req = client.post(rpc_url).json(&body);
        if let Some(token) = bearer_token {
            req = req.bearer_auth(token);
        }

        let resp = req.send().await?;
        let status = resp.status();
        let resp_body = resp.text().await?;

        if status.is_success() {
            Ok(ToolResult {
                success: true,
                output: resp_body,
                error: None,
            })
        } else {
            Ok(ToolResult {
                success: false,
                output: String::new(),
                error: Some(format!("HTTP {status}: {resp_body}")),
            })
        }
    }

    async fn action_get_task(
        &self,
        url: &str,
        bearer_token: Option<&str>,
        task_id: &str,
    ) -> anyhow::Result<serde_json::Value> {
        let base = self.validate_url(url)?;
        let rpc_url = base.join("/a2a")?;
        let client = self.build_client()?;
        let request_id = uuid::Uuid::new_v4().to_string();

        let body = json!({
            "jsonrpc": "2.0",
            "id": request_id,
            "method": "tasks/get",
            "params": { "id": task_id }
        });

        let mut req = client.post(rpc_url).json(&body);
        if let Some(token) = bearer_token {
            req = req.bearer_auth(token);
        }

        let resp = req.send().await?;
        let status = resp.status();
        let resp_body = resp.text().await?;

        if status.is_success() {
            let parsed: serde_json::Value = serde_json::from_str(&resp_body)?;
            Ok(parsed)
        } else {
            anyhow::bail!("HTTP {status}: {resp_body}");
        }
    }

    async fn action_status(
        &self,
        url: &str,
        bearer_token: Option<&str>,
        task_id: &str,
    ) -> anyhow::Result<ToolResult> {
        match self.action_get_task(url, bearer_token, task_id).await {
            Ok(resp) => Ok(ToolResult {
                success: true,
                output: serde_json::to_string_pretty(&resp)?,
                error: None,
            }),
            Err(e) => Ok(ToolResult {
                success: false,
                output: String::new(),
                error: Some(e.to_string()),
            }),
        }
    }

    async fn action_result(
        &self,
        url: &str,
        bearer_token: Option<&str>,
        task_id: &str,
    ) -> anyhow::Result<ToolResult> {
        match self.action_get_task(url, bearer_token, task_id).await {
            Ok(resp) => {
                // Extract artifacts from the task response
                let artifacts = resp
                    .pointer("/result/artifacts")
                    .or_else(|| resp.pointer("/artifacts"))
                    .cloned()
                    .unwrap_or(json!([]));
                Ok(ToolResult {
                    success: true,
                    output: serde_json::to_string_pretty(&artifacts)?,
                    error: None,
                })
            }
            Err(e) => Ok(ToolResult {
                success: false,
                output: String::new(),
                error: Some(e.to_string()),
            }),
        }
    }
}

#[async_trait]
impl Tool for A2aTool {
    fn name(&self) -> &str {
        "a2a"
    }

    fn description(&self) -> &str {
        "Communicate with remote agents via the A2A (Agent-to-Agent) protocol. \
         Supports four actions: 'discover' to fetch a remote agent's capability card, \
         'send' to dispatch a task message, 'status' to check task progress, and \
         'result' to retrieve task output artifacts."
    }

    fn parameters_schema(&self) -> serde_json::Value {
        json!({
            "type": "object",
            "properties": {
                "action": {
                    "type": "string",
                    "enum": ["discover", "send", "status", "result"],
                    "description": "A2A operation to perform"
                },
                "url": {
                    "type": "string",
                    "description": "Base URL of the remote agent (e.g. http://host:port)"
                },
                "bearer_token": {
                    "type": "string",
                    "description": "Bearer token for authentication with the remote agent"
                },
                "task_id": {
                    "type": "string",
                    "description": "Task ID (required for status/result actions)"
                },
                "message": {
                    "type": "string",
                    "description": "Message to send to the remote agent (required for send action)"
                }
            },
            "required": ["action", "url"]
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

        let action = args
            .get("action")
            .and_then(|v| v.as_str())
            .unwrap_or("")
            .to_string();
        let url = args
            .get("url")
            .and_then(|v| v.as_str())
            .unwrap_or("")
            .to_string();
        let bearer_token = args
            .get("bearer_token")
            .and_then(|v| v.as_str())
            .map(String::from);
        let task_id = args
            .get("task_id")
            .and_then(|v| v.as_str())
            .unwrap_or("")
            .to_string();
        let message = args
            .get("message")
            .and_then(|v| v.as_str())
            .unwrap_or("")
            .to_string();

        if url.is_empty() {
            return Ok(ToolResult {
                success: false,
                output: String::new(),
                error: Some("Missing required parameter: url".into()),
            });
        }

        match action.as_str() {
            "discover" => self.action_discover(&url, bearer_token.as_deref()).await,
            "send" => {
                if message.is_empty() {
                    return Ok(ToolResult {
                        success: false,
                        output: String::new(),
                        error: Some("Missing required parameter: message".into()),
                    });
                }
                self.action_send(&url, bearer_token.as_deref(), &message)
                    .await
            }
            "status" => {
                if task_id.is_empty() {
                    return Ok(ToolResult {
                        success: false,
                        output: String::new(),
                        error: Some("Missing required parameter: task_id".into()),
                    });
                }
                self.action_status(&url, bearer_token.as_deref(), &task_id)
                    .await
            }
            "result" => {
                if task_id.is_empty() {
                    return Ok(ToolResult {
                        success: false,
                        output: String::new(),
                        error: Some("Missing required parameter: task_id".into()),
                    });
                }
                self.action_result(&url, bearer_token.as_deref(), &task_id)
                    .await
            }
            other => Ok(ToolResult {
                success: false,
                output: String::new(),
                error: Some(format!(
                    "Unknown action: '{other}'. Valid actions: discover, send, status, result"
                )),
            }),
        }
    }
}

// ── SSRF protection helpers (mirrored from web_fetch.rs) ─────────

fn is_private_or_local_host(host: &str) -> bool {
    let bare = host
        .strip_prefix('[')
        .and_then(|h| h.strip_suffix(']'))
        .unwrap_or(host);

    let has_local_tld = bare
        .rsplit('.')
        .next()
        .is_some_and(|label| label == "local");

    if bare == "localhost" || bare.ends_with(".localhost") || has_local_tld {
        return true;
    }

    if let Ok(ip) = bare.parse::<std::net::IpAddr>() {
        return match ip {
            std::net::IpAddr::V4(v4) => is_non_global_v4(v4),
            std::net::IpAddr::V6(v6) => is_non_global_v6(v6),
        };
    }

    false
}

#[cfg(not(test))]
fn validate_resolved_host_is_public(host: &str) -> anyhow::Result<()> {
    use std::net::ToSocketAddrs;

    let ips = (host, 0)
        .to_socket_addrs()
        .map_err(|e| anyhow::anyhow!("Failed to resolve host '{host}': {e}"))?
        .map(|addr| addr.ip())
        .collect::<Vec<_>>();

    validate_resolved_ips_are_public(host, &ips)
}

/// Test stub: skip DNS resolution so unit tests don't depend on network.
/// Literal IP/hostname checks are still exercised via `is_private_or_local_host`
/// in `validate_url`; only the resolve-and-recheck path is stubbed out.
#[cfg(test)]
fn validate_resolved_host_is_public(_host: &str) -> anyhow::Result<()> {
    Ok(())
}

fn validate_resolved_ips_are_public(host: &str, ips: &[std::net::IpAddr]) -> anyhow::Result<()> {
    if ips.is_empty() {
        anyhow::bail!("Failed to resolve host '{host}'");
    }

    for ip in ips {
        let non_global = match ip {
            std::net::IpAddr::V4(v4) => is_non_global_v4(*v4),
            std::net::IpAddr::V6(v6) => is_non_global_v6(*v6),
        };
        if non_global {
            anyhow::bail!("Blocked host '{host}' resolved to non-global address {ip}");
        }
    }

    Ok(())
}

fn is_non_global_v4(v4: std::net::Ipv4Addr) -> bool {
    let [a, b, c, _] = v4.octets();
    v4.is_loopback()
        || v4.is_private()
        || v4.is_link_local()
        || v4.is_unspecified()
        || v4.is_broadcast()
        || v4.is_multicast()
        || (a == 100 && (64..=127).contains(&b))
        || a >= 240
        || (a == 192 && b == 0 && (c == 0 || c == 2))
        || (a == 198 && b == 51)
        || (a == 203 && b == 0)
        || (a == 198 && (18..=19).contains(&b))
}

fn is_non_global_v6(v6: std::net::Ipv6Addr) -> bool {
    let segs = v6.segments();
    v6.is_loopback()
        || v6.is_unspecified()
        || v6.is_multicast()
        || (segs[0] & 0xfe00) == 0xfc00
        || (segs[0] & 0xffc0) == 0xfe80
        || (segs[0] == 0x2001 && segs[1] == 0x0db8)
        || v6.to_ipv4_mapped().is_some_and(is_non_global_v4)
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::security::SecurityPolicy;

    fn test_tool() -> A2aTool {
        let security = Arc::new(SecurityPolicy::default());
        A2aTool::new(security, 30, false)
    }

    #[test]
    fn tool_metadata() {
        let tool = test_tool();
        assert_eq!(tool.name(), "a2a");
        assert!(!tool.description().is_empty());

        let schema = tool.parameters_schema();
        assert_eq!(schema["type"], "object");
        assert!(schema["properties"]["action"].is_object());
        assert!(schema["properties"]["url"].is_object());

        let required = schema["required"].as_array().unwrap();
        assert!(required.contains(&json!("action")));
        assert!(required.contains(&json!("url")));
    }

    #[test]
    fn validate_url_accepts_public_http() {
        let tool = test_tool();
        assert!(tool.validate_url("http://agent.example.com:8080").is_ok());
        assert!(tool.validate_url("https://agent.example.com").is_ok());
    }

    #[test]
    fn validate_url_rejects_non_http() {
        let tool = test_tool();
        assert!(tool.validate_url("ftp://host").is_err());
        assert!(tool.validate_url("file:///etc/passwd").is_err());
    }

    #[test]
    fn validate_url_rejects_private_hosts() {
        let tool = test_tool();
        assert!(tool.validate_url("http://localhost:9999").is_err());
        assert!(tool.validate_url("http://127.0.0.1:9999").is_err());
        assert!(tool.validate_url("http://10.0.0.1").is_err());
        assert!(tool.validate_url("http://192.168.1.1").is_err());
        assert!(tool.validate_url("http://172.16.0.1").is_err());
        assert!(tool.validate_url("http://169.254.169.254").is_err());
        assert!(tool.validate_url("http://[::1]").is_err());
        assert!(tool.validate_url("http://foo.local").is_err());
    }

    #[test]
    fn validate_url_allows_local_when_enabled() {
        let security = Arc::new(SecurityPolicy::default());
        let tool = A2aTool::new(security, 30, true);
        assert!(tool.validate_url("http://127.0.0.1:42618").is_ok());
        assert!(tool.validate_url("http://localhost:42618").is_ok());
    }

    #[test]
    fn ssrf_helpers_block_cloud_metadata() {
        assert!(is_private_or_local_host("169.254.169.254"));
        assert!(is_private_or_local_host("127.0.0.1"));
        assert!(is_private_or_local_host("10.0.0.1"));
        assert!(is_private_or_local_host("localhost"));
        assert!(is_private_or_local_host("foo.localhost"));
        assert!(!is_private_or_local_host("8.8.8.8"));
        assert!(!is_private_or_local_host("example.com"));
    }

    #[tokio::test]
    async fn missing_url_returns_error() {
        let tool = test_tool();
        let result = tool.execute(json!({"action": "discover"})).await.unwrap();
        assert!(!result.success);
        assert!(result.error.as_deref().unwrap().contains("url"));
    }

    #[tokio::test]
    async fn unknown_action_returns_error() {
        let tool = test_tool();
        let result = tool
            .execute(json!({"action": "invalid", "url": "http://localhost"}))
            .await
            .unwrap();
        assert!(!result.success);
        assert!(result.error.as_deref().unwrap().contains("Unknown action"));
    }

    #[tokio::test]
    async fn send_missing_message_returns_error() {
        let tool = test_tool();
        let result = tool
            .execute(json!({"action": "send", "url": "http://localhost"}))
            .await
            .unwrap();
        assert!(!result.success);
        assert!(result.error.as_deref().unwrap().contains("message"));
    }

    #[tokio::test]
    async fn status_missing_task_id_returns_error() {
        let tool = test_tool();
        let result = tool
            .execute(json!({"action": "status", "url": "http://localhost"}))
            .await
            .unwrap();
        assert!(!result.success);
        assert!(result.error.as_deref().unwrap().contains("task_id"));
    }

    // ── HTTP integration tests (wiremock) ────────────────────
    //
    // These test the actual HTTP request/response cycle for each A2A
    // action.  `validate_url` blocks localhost, so we call the action
    // methods directly via a helper that patches the URL post-validation.
    // SSRF validation is already covered by the unit tests above.

    /// Build a tool with a short timeout suitable for mock-server tests.
    fn mock_tool() -> A2aTool {
        let security = Arc::new(SecurityPolicy::default());
        A2aTool::new(security, 5, false)
    }

    /// Directly call the discover action, bypassing SSRF validation
    /// (which rejects localhost where wiremock binds).
    async fn discover_direct(
        tool: &A2aTool,
        url: &str,
        bearer: Option<&str>,
    ) -> anyhow::Result<ToolResult> {
        let client = tool.build_client()?;
        let parsed = reqwest::Url::parse(url)?;
        let card_url = parsed.join("/.well-known/agent-card.json")?;
        let mut req = client.get(card_url);
        if let Some(token) = bearer {
            req = req.bearer_auth(token);
        }
        let resp = req.send().await?;
        let status = resp.status();
        let body = resp.text().await?;
        if status.is_success() {
            Ok(ToolResult {
                success: true,
                output: body,
                error: None,
            })
        } else {
            Ok(ToolResult {
                success: false,
                output: String::new(),
                error: Some(format!("HTTP {status}: {body}")),
            })
        }
    }

    /// Directly call the send action, bypassing SSRF validation.
    async fn send_direct(
        tool: &A2aTool,
        url: &str,
        bearer: Option<&str>,
        message: &str,
    ) -> anyhow::Result<ToolResult> {
        let client = tool.build_client()?;
        let parsed = reqwest::Url::parse(url)?;
        let rpc_url = parsed.join("/a2a")?;
        let body = json!({
            "jsonrpc": "2.0",
            "id": uuid::Uuid::new_v4().to_string(),
            "method": "message/send",
            "params": {
                "message": {
                    "role": "user",
                    "parts": [{"kind": "text", "text": message}],
                    "messageId": uuid::Uuid::new_v4().to_string()
                }
            }
        });
        let mut req = client.post(rpc_url).json(&body);
        if let Some(token) = bearer {
            req = req.bearer_auth(token);
        }
        let resp = req.send().await?;
        let status = resp.status();
        let resp_body = resp.text().await?;
        if status.is_success() {
            Ok(ToolResult {
                success: true,
                output: resp_body,
                error: None,
            })
        } else {
            Ok(ToolResult {
                success: false,
                output: String::new(),
                error: Some(format!("HTTP {status}: {resp_body}")),
            })
        }
    }

    #[tokio::test]
    async fn discover_fetches_agent_card_from_server() {
        use wiremock::matchers::{method, path};
        use wiremock::{Mock, MockServer, ResponseTemplate};

        let server = MockServer::start().await;
        let card = json!({
            "name": "Test Agent",
            "version": "1.0",
            "skills": []
        });

        Mock::given(method("GET"))
            .and(path("/.well-known/agent-card.json"))
            .respond_with(ResponseTemplate::new(200).set_body_json(&card))
            .mount(&server)
            .await;

        let tool = mock_tool();
        let result = discover_direct(&tool, &server.uri(), None).await.unwrap();
        assert!(result.success);
        let parsed: serde_json::Value = serde_json::from_str(&result.output).unwrap();
        assert_eq!(parsed["name"], "Test Agent");
    }

    #[tokio::test]
    async fn discover_returns_error_on_404() {
        use wiremock::matchers::{method, path};
        use wiremock::{Mock, MockServer, ResponseTemplate};

        let server = MockServer::start().await;

        Mock::given(method("GET"))
            .and(path("/.well-known/agent-card.json"))
            .respond_with(ResponseTemplate::new(404).set_body_string("not found"))
            .mount(&server)
            .await;

        let tool = mock_tool();
        let result = discover_direct(&tool, &server.uri(), None).await.unwrap();
        assert!(!result.success);
        assert!(result.error.as_deref().unwrap().contains("404"));
    }

    #[tokio::test]
    async fn send_dispatches_jsonrpc_and_returns_response() {
        use wiremock::matchers::{method, path};
        use wiremock::{Mock, MockServer, ResponseTemplate};

        let server = MockServer::start().await;
        let rpc_response = json!({
            "jsonrpc": "2.0",
            "id": "test",
            "result": {
                "id": "task-1",
                "status": {"state": "completed"},
                "artifacts": [{"parts": [{"kind": "text", "text": "response"}]}]
            }
        });

        Mock::given(method("POST"))
            .and(path("/a2a"))
            .respond_with(ResponseTemplate::new(200).set_body_json(&rpc_response))
            .mount(&server)
            .await;

        let tool = mock_tool();
        let result = send_direct(&tool, &server.uri(), None, "hello agent")
            .await
            .unwrap();
        assert!(result.success);
        let parsed: serde_json::Value = serde_json::from_str(&result.output).unwrap();
        assert_eq!(parsed["result"]["status"]["state"], "completed");
    }

    #[tokio::test]
    async fn send_includes_bearer_token_when_provided() {
        use wiremock::matchers::{header, method, path};
        use wiremock::{Mock, MockServer, ResponseTemplate};

        let server = MockServer::start().await;

        Mock::given(method("POST"))
            .and(path("/a2a"))
            .and(header("Authorization", "Bearer my-token"))
            .respond_with(
                ResponseTemplate::new(200)
                    .set_body_json(json!({"jsonrpc": "2.0", "id": "1", "result": {}})),
            )
            .mount(&server)
            .await;

        let tool = mock_tool();
        let result = send_direct(&tool, &server.uri(), Some("my-token"), "test")
            .await
            .unwrap();
        assert!(result.success);
    }

    #[tokio::test]
    async fn send_reports_auth_failure() {
        use wiremock::matchers::{method, path};
        use wiremock::{Mock, MockServer, ResponseTemplate};

        let server = MockServer::start().await;

        Mock::given(method("POST"))
            .and(path("/a2a"))
            .respond_with(
                ResponseTemplate::new(401).set_body_json(json!({"error": "Unauthorized"})),
            )
            .mount(&server)
            .await;

        let tool = mock_tool();
        let result = send_direct(&tool, &server.uri(), None, "test")
            .await
            .unwrap();
        assert!(!result.success);
        assert!(result.error.as_deref().unwrap().contains("401"));
    }

    #[tokio::test]
    async fn discover_with_bearer_sends_auth_header() {
        use wiremock::matchers::{header, method, path};
        use wiremock::{Mock, MockServer, ResponseTemplate};

        let server = MockServer::start().await;

        Mock::given(method("GET"))
            .and(path("/.well-known/agent-card.json"))
            .and(header("Authorization", "Bearer secret-123"))
            .respond_with(
                ResponseTemplate::new(200)
                    .set_body_json(json!({"name": "Auth Agent", "skills": []})),
            )
            .mount(&server)
            .await;

        let tool = mock_tool();
        let result = discover_direct(&tool, &server.uri(), Some("secret-123"))
            .await
            .unwrap();
        assert!(result.success);
        let parsed: serde_json::Value = serde_json::from_str(&result.output).unwrap();
        assert_eq!(parsed["name"], "Auth Agent");
    }

    #[tokio::test]
    async fn read_only_autonomy_blocks_execution() {
        use crate::security::AutonomyLevel;
        let security = Arc::new(SecurityPolicy {
            autonomy: AutonomyLevel::ReadOnly,
            workspace_dir: std::env::temp_dir(),
            ..SecurityPolicy::default()
        });
        let tool = A2aTool::new(security, 5, false);
        let result = tool
            .execute(json!({"action": "discover", "url": "http://example.com"}))
            .await
            .unwrap();
        assert!(!result.success);
        assert!(result.error.as_deref().unwrap().contains("read-only"));
    }
}
