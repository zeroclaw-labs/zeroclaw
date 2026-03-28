use super::traits::{Tool, ToolResult};
use crate::security::SecurityPolicy;
use async_trait::async_trait;
use serde_json::json;
use std::sync::Arc;
use std::time::Duration;

/// Text browser tool: renders web pages as plain text using text-based browsers
/// (lynx, links, w3m). Ideal for headless/SSH environments where graphical
/// browsers are unavailable.
pub struct TextBrowserTool {
    security: Arc<SecurityPolicy>,
    preferred_browser: Option<String>,
    timeout_secs: u64,
    max_response_size: usize,
}

/// The text browsers we support, in order of auto-detection preference.
const SUPPORTED_BROWSERS: &[&str] = &["lynx", "links", "w3m"];

impl TextBrowserTool {
    pub fn new(
        security: Arc<SecurityPolicy>,
        preferred_browser: Option<String>,
        timeout_secs: u64,
    ) -> Self {
        Self {
            security,
            preferred_browser,
            timeout_secs,
            max_response_size: 500_000, // 500KB, consistent with web_fetch
        }
    }

    fn validate_url(url: &str) -> anyhow::Result<String> {
        let url = url.trim();

        if url.is_empty() {
            anyhow::bail!("URL cannot be empty");
        }

        if url.chars().any(char::is_whitespace) {
            anyhow::bail!("URL cannot contain whitespace");
        }

        if !url.starts_with("http://") && !url.starts_with("https://") {
            anyhow::bail!("Only http:// and https:// URLs are allowed");
        }

        Ok(url.to_string())
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

    /// Detect which text browser is available on the system.
    async fn detect_browser() -> Option<String> {
        for browser in SUPPORTED_BROWSERS {
            if let Ok(output) = tokio::process::Command::new("which")
                .arg(browser)
                .output()
                .await
            {
                if output.status.success() {
                    return Some((*browser).to_string());
                }
            }
        }
        None
    }

    /// Resolve which browser to use: prefer configured, then auto-detect.
    async fn resolve_browser(&self, requested: Option<&str>) -> anyhow::Result<String> {
        // If the caller explicitly requested a browser via the tool parameter, use it.
        if let Some(browser) = requested {
            let browser = browser.trim().to_lowercase();
            if !SUPPORTED_BROWSERS.contains(&browser.as_str()) {
                anyhow::bail!(
                    "Unsupported text browser '{browser}'. Supported: {}",
                    SUPPORTED_BROWSERS.join(", ")
                );
            }
            // Verify it's installed
            let installed = tokio::process::Command::new("which")
                .arg(&browser)
                .output()
                .await
                .map(|o| o.status.success())
                .unwrap_or(false);
            if !installed {
                anyhow::bail!("Requested text browser '{browser}' is not installed");
            }
            return Ok(browser);
        }

        // If a preferred browser is set in config, try it first.
        if let Some(ref preferred) = self.preferred_browser {
            let preferred = preferred.trim().to_lowercase();
            if SUPPORTED_BROWSERS.contains(&preferred.as_str()) {
                let installed = tokio::process::Command::new("which")
                    .arg(&preferred)
                    .output()
                    .await
                    .map(|o| o.status.success())
                    .unwrap_or(false);
                if installed {
                    return Ok(preferred);
                }
                tracing::warn!(
                    "Configured preferred text browser '{preferred}' is not installed, falling back to auto-detect"
                );
            }
        }

        // Auto-detect
        Self::detect_browser().await.ok_or_else(|| {
            anyhow::anyhow!(
                "No text browser found. Install one of: {}",
                SUPPORTED_BROWSERS.join(", ")
            )
        })
    }

    /// Build the command arguments for the selected browser with `-dump` flag.
    fn build_dump_args(_browser: &str, url: &str) -> Vec<String> {
        // All supported browsers (lynx, links, w3m) use the same `-dump` flag
        vec!["-dump".to_string(), url.to_string()]
    }
}

#[async_trait]
impl Tool for TextBrowserTool {
    fn name(&self) -> &str {
        "text_browser"
    }

    fn description(&self) -> &str {
        "Render a web page as plain text using a text-based browser (lynx, links, or w3m). \
         Ideal for headless/SSH environments without a graphical browser. \
         Auto-detects available browser or uses a configured preference."
    }

    fn parameters_schema(&self) -> serde_json::Value {
        json!({
            "type": "object",
            "properties": {
                "url": {
                    "type": "string",
                    "description": "The HTTP or HTTPS URL to render as plain text"
                },
                "browser": {
                    "type": "string",
                    "description": "Text browser to use: \"lynx\", \"links\", or \"w3m\". If omitted, auto-detects an available browser.",
                    "enum": ["lynx", "links", "w3m"]
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

        let url = match Self::validate_url(url) {
            Ok(v) => v,
            Err(e) => {
                return Ok(ToolResult {
                    success: false,
                    output: String::new(),
                    error: Some(e.to_string()),
                });
            }
        };

        let requested_browser = args.get("browser").and_then(|v| v.as_str());

        let browser = match self.resolve_browser(requested_browser).await {
            Ok(b) => b,
            Err(e) => {
                return Ok(ToolResult {
                    success: false,
                    output: String::new(),
                    error: Some(e.to_string()),
                });
            }
        };

        let dump_args = Self::build_dump_args(&browser, &url);

        let timeout = Duration::from_secs(if self.timeout_secs == 0 {
            tracing::warn!("text_browser: timeout_secs is 0, using safe default of 30s");
            30
        } else {
            self.timeout_secs
        });

        let result = tokio::time::timeout(
            timeout,
            tokio::process::Command::new(&browser)
                .args(&dump_args)
                .output(),
        )
        .await;

        match result {
            Ok(Ok(output)) => {
                if output.status.success() {
                    let text = String::from_utf8_lossy(&output.stdout).into_owned();
                    let text = self.truncate_response(&text);
                    Ok(ToolResult {
                        success: true,
                        output: text,
                        error: None,
                    })
                } else {
                    let stderr = String::from_utf8_lossy(&output.stderr);
                    Ok(ToolResult {
                        success: false,
                        output: String::new(),
                        error: Some(format!(
                            "{browser} exited with status {}: {}",
                            output.status,
                            stderr.trim()
                        )),
                    })
                }
            }
            Ok(Err(e)) => Ok(ToolResult {
                success: false,
                output: String::new(),
                error: Some(format!("Failed to execute {browser}: {e}")),
            }),
            Err(_) => Ok(ToolResult {
                success: false,
                output: String::new(),
                error: Some(format!(
                    "{browser} timed out after {} seconds",
                    timeout.as_secs()
                )),
            }),
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::security::{AutonomyLevel, SecurityPolicy};

    fn test_tool() -> TextBrowserTool {
        let security = Arc::new(SecurityPolicy {
            autonomy: AutonomyLevel::Supervised,
            ..SecurityPolicy::default()
        });
        TextBrowserTool::new(security, None, 30)
    }

    #[test]
    fn name_is_text_browser() {
        let tool = test_tool();
        assert_eq!(tool.name(), "text_browser");
    }

    #[test]
    fn parameters_schema_requires_url() {
        let tool = test_tool();
        let schema = tool.parameters_schema();
        assert!(schema["properties"]["url"].is_object());
        let required = schema["required"].as_array().unwrap();
        assert!(required.iter().any(|v| v.as_str() == Some("url")));
    }

    #[test]
    fn parameters_schema_has_optional_browser() {
        let tool = test_tool();
        let schema = tool.parameters_schema();
        assert!(schema["properties"]["browser"].is_object());
        let required = schema["required"].as_array().unwrap();
        assert!(!required.iter().any(|v| v.as_str() == Some("browser")));
    }

    #[test]
    fn validate_url_accepts_http() {
        let got = TextBrowserTool::validate_url("http://example.com/page").unwrap();
        assert_eq!(got, "http://example.com/page");
    }

    #[test]
    fn validate_url_accepts_https() {
        let got = TextBrowserTool::validate_url("https://example.com/page").unwrap();
        assert_eq!(got, "https://example.com/page");
    }

    #[test]
    fn validate_url_rejects_empty() {
        let err = TextBrowserTool::validate_url("").unwrap_err().to_string();
        assert!(err.contains("empty"));
    }

    #[test]
    fn validate_url_rejects_ftp() {
        let err = TextBrowserTool::validate_url("ftp://example.com")
            .unwrap_err()
            .to_string();
        assert!(err.contains("http://") || err.contains("https://"));
    }

    #[test]
    fn validate_url_rejects_whitespace() {
        let err = TextBrowserTool::validate_url("https://example.com/hello world")
            .unwrap_err()
            .to_string();
        assert!(err.contains("whitespace"));
    }

    #[test]
    fn truncate_within_limit() {
        let tool = test_tool();
        let text = "hello world";
        assert_eq!(tool.truncate_response(text), "hello world");
    }

    #[test]
    fn truncate_over_limit() {
        let security = Arc::new(SecurityPolicy::default());
        let mut tool = TextBrowserTool::new(security, None, 30);
        tool.max_response_size = 10;
        let text = "hello world this is long";
        let truncated = tool.truncate_response(text);
        assert!(truncated.contains("[Response truncated"));
    }

    #[test]
    fn build_dump_args_lynx() {
        let args = TextBrowserTool::build_dump_args("lynx", "https://example.com");
        assert_eq!(args, vec!["-dump", "https://example.com"]);
    }

    #[test]
    fn build_dump_args_links() {
        let args = TextBrowserTool::build_dump_args("links", "https://example.com");
        assert_eq!(args, vec!["-dump", "https://example.com"]);
    }

    #[test]
    fn build_dump_args_w3m() {
        let args = TextBrowserTool::build_dump_args("w3m", "https://example.com");
        assert_eq!(args, vec!["-dump", "https://example.com"]);
    }

    #[tokio::test]
    async fn blocks_readonly_mode() {
        let security = Arc::new(SecurityPolicy {
            autonomy: AutonomyLevel::ReadOnly,
            ..SecurityPolicy::default()
        });
        let tool = TextBrowserTool::new(security, None, 30);
        let result = tool
            .execute(json!({"url": "https://example.com"}))
            .await
            .unwrap();
        assert!(!result.success);
        assert!(result.error.unwrap().contains("read-only"));
    }

    #[tokio::test]
    async fn blocks_rate_limited() {
        let security = Arc::new(SecurityPolicy {
            max_actions_per_hour: 0,
            ..SecurityPolicy::default()
        });
        let tool = TextBrowserTool::new(security, None, 30);
        let result = tool
            .execute(json!({"url": "https://example.com"}))
            .await
            .unwrap();
        assert!(!result.success);
        assert!(result.error.unwrap().contains("rate limit"));
    }
}
