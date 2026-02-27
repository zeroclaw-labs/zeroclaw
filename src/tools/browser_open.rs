use super::traits::{Tool, ToolResult};
use super::url_validation::{
    normalize_allowed_domains, validate_url, DomainPolicy, UrlSchemePolicy,
};
use crate::security::SecurityPolicy;
use async_trait::async_trait;
use serde_json::json;
use std::sync::Arc;

/// Open approved HTTPS URLs in Brave Browser (no scraping, no DOM automation).
pub struct BrowserOpenTool {
    security: Arc<SecurityPolicy>,
    allowed_domains: Vec<String>,
}

impl BrowserOpenTool {
    pub fn new(security: Arc<SecurityPolicy>, allowed_domains: Vec<String>) -> Self {
        Self {
            security,
            allowed_domains: normalize_allowed_domains(allowed_domains),
        }
    }

    fn validate_url(&self, raw_url: &str) -> anyhow::Result<String> {
        validate_url(
            raw_url,
            &DomainPolicy {
                allowed_domains: &self.allowed_domains,
                blocked_domains: &[],
                allowed_field_name: "browser.allowed_domains",
                blocked_field_name: None,
                empty_allowed_message: "Browser tool is enabled but no allowed_domains are configured. Add [browser].allowed_domains in config.toml",
                scheme_policy: UrlSchemePolicy::HttpsOnly,
                ipv6_error_context: "browser_open",
            },
        )
    }
}

#[async_trait]
impl Tool for BrowserOpenTool {
    fn name(&self) -> &str {
        "browser_open"
    }

    fn description(&self) -> &str {
        "Open an approved HTTPS URL in Brave Browser. Security constraints: allowlist-only domains, no local/private hosts, no scraping."
    }

    fn parameters_schema(&self) -> serde_json::Value {
        json!({
            "type": "object",
            "properties": {
                "url": {
                    "type": "string",
                    "description": "HTTPS URL to open in Brave Browser"
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

        match open_in_brave(&url).await {
            Ok(()) => Ok(ToolResult {
                success: true,
                output: format!("Opened in Brave: {url}"),
                error: None,
            }),
            Err(e) => Ok(ToolResult {
                success: false,
                output: String::new(),
                error: Some(format!("Failed to open Brave Browser: {e}")),
            }),
        }
    }
}

async fn open_in_brave(url: &str) -> anyhow::Result<()> {
    #[cfg(target_os = "macos")]
    {
        for app in ["Brave Browser", "Brave"] {
            let status = tokio::process::Command::new("open")
                .arg("-a")
                .arg(app)
                .arg(url)
                .status()
                .await;

            if let Ok(s) = status {
                if s.success() {
                    return Ok(());
                }
            }
        }
        anyhow::bail!(
            "Brave Browser was not found (tried macOS app names 'Brave Browser' and 'Brave')"
        );
    }

    #[cfg(target_os = "linux")]
    {
        let mut last_error = String::new();
        for cmd in ["brave-browser", "brave"] {
            match tokio::process::Command::new(cmd).arg(url).status().await {
                Ok(status) if status.success() => return Ok(()),
                Ok(status) => {
                    last_error = format!("{cmd} exited with status {status}");
                }
                Err(e) => {
                    last_error = format!("{cmd} not runnable: {e}");
                }
            }
        }
        anyhow::bail!("{last_error}");
    }

    #[cfg(target_os = "windows")]
    {
        let status = tokio::process::Command::new("cmd")
            .args(["/C", "start", "", "brave", url])
            .status()
            .await?;

        if status.success() {
            return Ok(());
        }

        anyhow::bail!("cmd start brave exited with status {status}");
    }

    #[cfg(not(any(target_os = "macos", target_os = "linux", target_os = "windows")))]
    {
        let _ = url;
        anyhow::bail!("browser_open is not supported on this OS");
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::security::{AutonomyLevel, SecurityPolicy};
    use crate::tools::url_validation::normalize_domain;

    fn test_tool(allowed_domains: Vec<&str>) -> BrowserOpenTool {
        let security = Arc::new(SecurityPolicy {
            autonomy: AutonomyLevel::Supervised,
            ..SecurityPolicy::default()
        });
        BrowserOpenTool::new(
            security,
            allowed_domains.into_iter().map(String::from).collect(),
        )
    }

    #[test]
    fn normalize_domain_strips_scheme_path_and_case() {
        let got = normalize_domain("  HTTPS://Docs.Example.com/path ").unwrap();
        assert_eq!(got, "docs.example.com");
    }

    #[test]
    fn normalize_allowed_domains_deduplicates() {
        let got = normalize_allowed_domains(vec![
            "example.com".into(),
            "EXAMPLE.COM".into(),
            "https://example.com/".into(),
        ]);
        assert_eq!(got, vec!["example.com".to_string()]);
    }

    #[test]
    fn validate_accepts_exact_domain() {
        let tool = test_tool(vec!["example.com"]);
        let got = tool.validate_url("https://example.com/docs").unwrap();
        assert_eq!(got, "https://example.com/docs");
    }

    #[test]
    fn validate_accepts_subdomain() {
        let tool = test_tool(vec!["example.com"]);
        assert!(tool.validate_url("https://api.example.com/v1").is_ok());
    }

    #[test]
    fn validate_accepts_wildcard_allowlist_for_public_host() {
        let tool = test_tool(vec!["*"]);
        assert!(tool.validate_url("https://www.rust-lang.org").is_ok());
    }

    #[test]
    fn validate_wildcard_allowlist_still_rejects_private_host() {
        let tool = test_tool(vec!["*"]);
        let err = tool
            .validate_url("https://localhost:8443")
            .unwrap_err()
            .to_string();
        assert!(err.contains("local/private"));
    }

    #[test]
    fn validate_accepts_wildcard_subdomain_pattern() {
        let tool = test_tool(vec!["*.example.com"]);
        assert!(tool.validate_url("https://example.com").is_ok());
        assert!(tool.validate_url("https://sub.example.com").is_ok());
        assert!(tool.validate_url("https://other.com").is_err());
    }

    #[test]
    fn validate_rejects_http() {
        let tool = test_tool(vec!["example.com"]);
        let err = tool
            .validate_url("http://example.com")
            .unwrap_err()
            .to_string();
        assert!(err.contains("https://"));
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
    fn validate_rejects_allowlist_miss() {
        let tool = test_tool(vec!["example.com"]);
        let err = tool
            .validate_url("https://google.com")
            .unwrap_err()
            .to_string();
        assert!(err.contains("allowed_domains"));
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
        let tool = BrowserOpenTool::new(security, vec![]);
        let err = tool
            .validate_url("https://example.com")
            .unwrap_err()
            .to_string();
        assert!(err.contains("allowed_domains"));
    }

    #[tokio::test]
    async fn execute_blocks_readonly_mode() {
        let security = Arc::new(SecurityPolicy {
            autonomy: AutonomyLevel::ReadOnly,
            ..SecurityPolicy::default()
        });
        let tool = BrowserOpenTool::new(security, vec!["example.com".into()]);
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
        let tool = BrowserOpenTool::new(security, vec!["example.com".into()]);
        let result = tool
            .execute(json!({"url": "https://example.com"}))
            .await
            .unwrap();
        assert!(!result.success);
        assert!(result.error.unwrap().contains("rate limit"));
    }
}
