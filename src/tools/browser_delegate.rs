//! Browser delegation tool.
//!
//! Delegates browser-based tasks to a browser-capable CLI subprocess (e.g.
//! Claude Code with `claude-in-chrome` MCP tools) for interacting with
//! corporate web applications (Teams, Outlook, Jira, Confluence) that lack
//! direct API access.
//!
//! The tool spawns the configured CLI binary in non-interactive mode, passing
//! a structured prompt that instructs it to use browser automation. A
//! persistent Chrome profile can be configured so SSO sessions survive across
//! invocations.

use crate::security::SecurityPolicy;
use crate::tools::traits::{Tool, ToolResult};
use async_trait::async_trait;
use regex::Regex;
use schemars::JsonSchema;
use serde::{Deserialize, Serialize};
use std::sync::Arc;
use tokio::time::{Duration, timeout};

/// Configuration for browser delegation (`[browser_delegate]` section).
#[derive(Debug, Clone, Serialize, Deserialize, JsonSchema)]
pub struct BrowserDelegateConfig {
    /// Enable browser delegation tool.
    #[serde(default)]
    pub enabled: bool,
    /// CLI binary to use for browser tasks (default: `"claude"`).
    #[serde(default = "default_browser_cli")]
    pub cli_binary: String,
    /// Chrome profile directory for persistent SSO sessions.
    #[serde(default)]
    pub chrome_profile_dir: String,
    /// Allowed domains for browser navigation (empty = allow all non-blocked).
    #[serde(default)]
    pub allowed_domains: Vec<String>,
    /// Blocked domains for browser navigation.
    #[serde(default)]
    pub blocked_domains: Vec<String>,
    /// Task timeout in seconds.
    #[serde(default = "default_browser_task_timeout")]
    pub task_timeout_secs: u64,
}

/// Default CLI binary for browser delegation.
fn default_browser_cli() -> String {
    "claude".into()
}

/// Default task timeout in seconds (2 minutes).
fn default_browser_task_timeout() -> u64 {
    120
}

impl Default for BrowserDelegateConfig {
    fn default() -> Self {
        Self {
            enabled: false,
            cli_binary: default_browser_cli(),
            chrome_profile_dir: String::new(),
            allowed_domains: Vec::new(),
            blocked_domains: Vec::new(),
            task_timeout_secs: default_browser_task_timeout(),
        }
    }
}

/// Tool that delegates browser-based tasks to a browser-capable CLI subprocess.
pub struct BrowserDelegateTool {
    security: Arc<SecurityPolicy>,
    config: BrowserDelegateConfig,
}

impl BrowserDelegateTool {
    /// Create a new `BrowserDelegateTool` with the given security policy and config.
    pub fn new(security: Arc<SecurityPolicy>, config: BrowserDelegateConfig) -> Self {
        Self { security, config }
    }

    /// Build the CLI command for a browser task.
    ///
    /// Constructs a `tokio::process::Command` with the configured CLI binary,
    /// `--print` flag for non-interactive mode, and optional Chrome profile env.
    fn build_command(&self, task: &str, url: Option<&str>) -> tokio::process::Command {
        let mut cmd = tokio::process::Command::new(&self.config.cli_binary);

        // Claude Code non-interactive mode
        cmd.arg("--print");

        let prompt = if let Some(url) = url {
            format!(
                "Use your browser tools to navigate to {} and perform the following task: {}",
                url, task
            )
        } else {
            format!(
                "Use your browser tools to perform the following task: {}",
                task
            )
        };

        cmd.arg(&prompt);

        // Set Chrome profile if configured for persistent SSO sessions
        if !self.config.chrome_profile_dir.is_empty() {
            cmd.env("CHROME_USER_DATA_DIR", &self.config.chrome_profile_dir);
        }

        cmd.stdout(std::process::Stdio::piped());
        cmd.stderr(std::process::Stdio::piped());

        cmd
    }

    /// Extract URLs from free-form text and validate each against domain policy.
    ///
    /// Prevents policy bypass by embedding blocked URLs in the `task` text,
    /// which is forwarded verbatim to the browser CLI subprocess.
    fn validate_task_urls(&self, task: &str) -> anyhow::Result<()> {
        let url_re = Regex::new(r#"https?://[^\s\)\]\},\"'`<>]+"#).expect("valid regex");
        for m in url_re.find_iter(task) {
            self.validate_url(m.as_str())?;
        }
        Ok(())
    }

    /// Validate URL against allowed/blocked domain lists and scheme restrictions.
    ///
    /// Only `http` and `https` schemes are permitted. Blocked domains take
    /// precedence over allowed domains when both lists contain the same entry.
    fn validate_url(&self, url: &str) -> anyhow::Result<()> {
        let parsed = url
            .parse::<reqwest::Url>()
            .map_err(|e| anyhow::anyhow!("invalid URL '{}': {}", url, e))?;

        // Only allow http/https schemes
        let scheme = parsed.scheme();
        if scheme != "http" && scheme != "https" {
            anyhow::bail!("unsupported URL scheme: {}", scheme);
        }

        let domain = parsed.host_str().unwrap_or("").to_string();

        if domain.is_empty() {
            anyhow::bail!("URL has no host: {}", url);
        }

        // Check blocked domains first (deny takes precedence)
        for blocked in &self.config.blocked_domains {
            if domain_matches(&domain, blocked) {
                anyhow::bail!("domain '{}' is blocked by browser_delegate policy", domain);
            }
        }

        // If allowed_domains is non-empty, it acts as an allowlist
        if !self.config.allowed_domains.is_empty() {
            let allowed = self
                .config
                .allowed_domains
                .iter()
                .any(|d| domain_matches(&domain, d));
            if !allowed {
                anyhow::bail!(
                    "domain '{}' is not in browser_delegate allowed_domains",
                    domain
                );
            }
        }

        Ok(())
    }
}

/// Check whether `domain` matches a pattern (exact or suffix match).
fn domain_matches(domain: &str, pattern: &str) -> bool {
    let d = domain.to_lowercase();
    let p = pattern.to_lowercase();
    d == p || d.ends_with(&format!(".{}", p))
}

/// Maximum stderr bytes to capture from the subprocess.
const MAX_STDERR_CHARS: usize = 512;

/// Supported values for the `extract_format` parameter.
const VALID_EXTRACT_FORMATS: &[&str] = &["text", "json", "summary"];

#[async_trait]
impl Tool for BrowserDelegateTool {
    fn name(&self) -> &str {
        "browser_delegate"
    }

    fn description(&self) -> &str {
        "Delegate browser-based tasks to a browser-capable CLI for interacting with web applications like Teams, Outlook, Jira, Confluence"
    }

    fn parameters_schema(&self) -> serde_json::Value {
        serde_json::json!({
            "type": "object",
            "properties": {
                "task": {
                    "type": "string",
                    "description": "Description of the browser task to perform"
                },
                "url": {
                    "type": "string",
                    "description": "Optional URL to navigate to before performing the task"
                },
                "extract_format": {
                    "type": "string",
                    "enum": ["text", "json", "summary"],
                    "description": "Desired output format (default: text)"
                }
            },
            "required": ["task"]
        })
    }

    async fn execute(&self, args: serde_json::Value) -> anyhow::Result<ToolResult> {
        // Security gate
        if !self.security.can_act() {
            return Ok(ToolResult {
                success: false,
                output: String::new(),
                error: Some("browser_delegate tool is denied by security policy".into()),
            });
        }
        if !self.security.record_action() {
            return Ok(ToolResult {
                success: false,
                output: String::new(),
                error: Some("browser_delegate action rate-limited".into()),
            });
        }

        let task = args
            .get("task")
            .and_then(serde_json::Value::as_str)
            .unwrap_or("")
            .trim();

        if task.is_empty() {
            return Ok(ToolResult {
                success: false,
                output: String::new(),
                error: Some("'task' parameter is required and cannot be empty".into()),
            });
        }

        let url = args
            .get("url")
            .and_then(serde_json::Value::as_str)
            .map(str::trim)
            .filter(|u| !u.is_empty());

        // Validate URL if provided
        if let Some(url) = url {
            if let Err(e) = self.validate_url(url) {
                return Ok(ToolResult {
                    success: false,
                    output: String::new(),
                    error: Some(format!("URL validation failed: {e}")),
                });
            }
        }

        // Scan task text for embedded URLs and validate against domain policy.
        // This prevents bypassing domain restrictions by embedding blocked URLs
        // in the task text, which is forwarded verbatim to the browser CLI.
        if let Err(e) = self.validate_task_urls(task) {
            return Ok(ToolResult {
                success: false,
                output: String::new(),
                error: Some(format!("task text contains a disallowed URL: {e}")),
            });
        }

        let extract_format = args
            .get("extract_format")
            .and_then(serde_json::Value::as_str)
            .unwrap_or("text");

        // Validate extract_format against allowed enum values
        if !VALID_EXTRACT_FORMATS.contains(&extract_format) {
            return Ok(ToolResult {
                success: false,
                output: String::new(),
                error: Some(format!(
                    "unsupported extract_format '{}': allowed values are 'text', 'json', 'summary'",
                    extract_format
                )),
            });
        }

        // Append format instruction to the task
        let full_task = match extract_format {
            "json" => format!("{task}. Return the result as structured JSON."),
            "summary" => format!("{task}. Return a concise summary."),
            _ => task.to_string(),
        };

        let mut cmd = self.build_command(&full_task, url);
        // Ensure the subprocess is killed when the future is dropped (e.g. on timeout)
        cmd.kill_on_drop(true);

        let deadline = Duration::from_secs(self.config.task_timeout_secs);
        let result = timeout(deadline, cmd.output()).await;

        match result {
            Ok(Ok(output)) => {
                let stdout = String::from_utf8_lossy(&output.stdout).to_string();
                let stderr = String::from_utf8_lossy(&output.stderr);
                let stderr_truncated: String = stderr.chars().take(MAX_STDERR_CHARS).collect();

                if output.status.success() {
                    Ok(ToolResult {
                        success: true,
                        output: stdout,
                        error: if stderr_truncated.is_empty() {
                            None
                        } else {
                            Some(stderr_truncated)
                        },
                    })
                } else {
                    Ok(ToolResult {
                        success: false,
                        output: stdout,
                        error: Some(format!(
                            "CLI exited with status {}: {}",
                            output.status, stderr_truncated
                        )),
                    })
                }
            }
            Ok(Err(e)) => Ok(ToolResult {
                success: false,
                output: String::new(),
                error: Some(format!("failed to spawn browser CLI: {e}")),
            }),
            Err(_) => Ok(ToolResult {
                success: false,
                output: String::new(),
                error: Some(format!(
                    "browser task timed out after {}s",
                    self.config.task_timeout_secs
                )),
            }),
        }
    }
}

/// Pre-built task templates for common corporate tools.
pub struct BrowserTaskTemplates;

impl BrowserTaskTemplates {
    /// Read messages from a Microsoft Teams channel.
    pub fn read_teams_messages(channel: &str, count: usize) -> String {
        format!(
            "Open Microsoft Teams, navigate to the '{}' channel, \
             read the last {} messages, and return them as a structured \
             summary with sender, timestamp, and message content.",
            channel, count
        )
    }

    /// Read emails from the Outlook Web inbox.
    pub fn read_outlook_inbox(count: usize) -> String {
        format!(
            "Open Outlook Web (outlook.office.com), go to the inbox, \
             read the last {} emails, and return a summary of each with \
             sender, subject, date, and first 2 lines of body.",
            count
        )
    }

    /// Read Jira board for a project.
    pub fn read_jira_board(project: &str) -> String {
        format!(
            "Open Jira, navigate to the '{}' project board, and return \
             the current sprint tickets with their status, assignee, and title.",
            project
        )
    }

    /// Read a Confluence page.
    pub fn read_confluence_page(url: &str) -> String {
        format!(
            "Open the Confluence page at {}, read the full content, \
             and return a structured summary.",
            url
        )
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    fn default_test_config() -> BrowserDelegateConfig {
        BrowserDelegateConfig::default()
    }

    fn config_with_domains(allowed: Vec<String>, blocked: Vec<String>) -> BrowserDelegateConfig {
        BrowserDelegateConfig {
            enabled: true,
            allowed_domains: allowed,
            blocked_domains: blocked,
            ..BrowserDelegateConfig::default()
        }
    }

    fn test_tool(config: BrowserDelegateConfig) -> BrowserDelegateTool {
        BrowserDelegateTool::new(Arc::new(SecurityPolicy::default()), config)
    }

    // ── Config defaults ─────────────────────────────────────────────

    #[test]
    fn config_defaults_are_sensible() {
        let cfg = default_test_config();
        assert!(!cfg.enabled);
        assert_eq!(cfg.cli_binary, "claude");
        assert!(cfg.chrome_profile_dir.is_empty());
        assert!(cfg.allowed_domains.is_empty());
        assert!(cfg.blocked_domains.is_empty());
        assert_eq!(cfg.task_timeout_secs, 120);
    }

    #[test]
    fn config_serde_roundtrip() {
        let cfg = BrowserDelegateConfig {
            enabled: true,
            cli_binary: "my-cli".into(),
            chrome_profile_dir: "/tmp/profile".into(),
            allowed_domains: vec!["example.com".into()],
            blocked_domains: vec!["evil.com".into()],
            task_timeout_secs: 60,
        };
        let toml_str = toml::to_string(&cfg).unwrap();
        let parsed: BrowserDelegateConfig = toml::from_str(&toml_str).unwrap();
        assert!(parsed.enabled);
        assert_eq!(parsed.cli_binary, "my-cli");
        assert_eq!(parsed.chrome_profile_dir, "/tmp/profile");
        assert_eq!(parsed.allowed_domains, vec!["example.com"]);
        assert_eq!(parsed.blocked_domains, vec!["evil.com"]);
        assert_eq!(parsed.task_timeout_secs, 60);
    }

    // ── URL validation ──────────────────────────────────────────────

    #[test]
    fn validate_url_allows_when_no_restrictions() {
        let tool = test_tool(config_with_domains(vec![], vec![]));
        assert!(tool.validate_url("https://example.com/page").is_ok());
    }

    #[test]
    fn validate_url_rejects_blocked_domain() {
        let tool = test_tool(config_with_domains(vec![], vec!["evil.com".into()]));
        let result = tool.validate_url("https://evil.com/phish");
        assert!(result.is_err());
        assert!(result.unwrap_err().to_string().contains("blocked"));
    }

    #[test]
    fn validate_url_rejects_blocked_subdomain() {
        let tool = test_tool(config_with_domains(vec![], vec!["evil.com".into()]));
        assert!(tool.validate_url("https://sub.evil.com/phish").is_err());
    }

    #[test]
    fn validate_url_allows_listed_domain() {
        let tool = test_tool(config_with_domains(vec!["corp.example.com".into()], vec![]));
        assert!(tool.validate_url("https://corp.example.com/page").is_ok());
    }

    #[test]
    fn validate_url_rejects_unlisted_domain_with_allowlist() {
        let tool = test_tool(config_with_domains(vec!["corp.example.com".into()], vec![]));
        let result = tool.validate_url("https://other.example.com/page");
        assert!(result.is_err());
        assert!(result.unwrap_err().to_string().contains("not in"));
    }

    #[test]
    fn validate_url_blocked_takes_precedence_over_allowed() {
        let tool = test_tool(config_with_domains(
            vec!["example.com".into()],
            vec!["example.com".into()],
        ));
        let result = tool.validate_url("https://example.com/page");
        assert!(result.is_err());
        assert!(result.unwrap_err().to_string().contains("blocked"));
    }

    #[test]
    fn validate_url_rejects_invalid_url() {
        let tool = test_tool(default_test_config());
        assert!(tool.validate_url("not-a-url").is_err());
    }

    // ── Command building ────────────────────────────────────────────

    #[test]
    fn build_command_uses_configured_binary() {
        let config = BrowserDelegateConfig {
            cli_binary: "my-browser-cli".into(),
            ..BrowserDelegateConfig::default()
        };
        let tool = test_tool(config);
        let cmd = tool.build_command("read inbox", None);
        assert_eq!(cmd.as_std().get_program(), "my-browser-cli");
    }

    #[test]
    fn build_command_includes_print_flag() {
        let tool = test_tool(default_test_config());
        let cmd = tool.build_command("read inbox", None);
        let args: Vec<&std::ffi::OsStr> = cmd.as_std().get_args().collect();
        assert!(args.contains(&std::ffi::OsStr::new("--print")));
    }

    #[test]
    fn build_command_includes_url_in_prompt() {
        let tool = test_tool(default_test_config());
        let cmd = tool.build_command("read page", Some("https://example.com"));
        let args: Vec<String> = cmd
            .as_std()
            .get_args()
            .map(|a| a.to_string_lossy().to_string())
            .collect();
        let prompt = args.last().unwrap();
        assert!(prompt.contains("https://example.com"));
        assert!(prompt.contains("read page"));
    }

    #[test]
    fn build_command_sets_chrome_profile_env() {
        let config = BrowserDelegateConfig {
            chrome_profile_dir: "/tmp/chrome-profile".into(),
            ..BrowserDelegateConfig::default()
        };
        let tool = test_tool(config);
        let cmd = tool.build_command("task", None);
        let envs: Vec<_> = cmd.as_std().get_envs().collect();
        let chrome_env = envs
            .iter()
            .find(|(k, _)| k == &std::ffi::OsStr::new("CHROME_USER_DATA_DIR"));
        assert!(chrome_env.is_some());
        assert_eq!(
            chrome_env.unwrap().1,
            Some(std::ffi::OsStr::new("/tmp/chrome-profile"))
        );
    }

    // ── Task templates ──────────────────────────────────────────────

    #[test]
    fn template_teams_includes_channel_and_count() {
        let t = BrowserTaskTemplates::read_teams_messages("engineering", 10);
        assert!(t.contains("engineering"));
        assert!(t.contains("10"));
        assert!(t.contains("Teams"));
    }

    #[test]
    fn template_outlook_includes_count() {
        let t = BrowserTaskTemplates::read_outlook_inbox(5);
        assert!(t.contains('5'));
        assert!(t.contains("Outlook"));
    }

    #[test]
    fn template_jira_includes_project() {
        let t = BrowserTaskTemplates::read_jira_board("PROJ-X");
        assert!(t.contains("PROJ-X"));
        assert!(t.contains("Jira"));
    }

    #[test]
    fn template_confluence_includes_url() {
        let t = BrowserTaskTemplates::read_confluence_page("https://wiki.example.com/page/123");
        assert!(t.contains("https://wiki.example.com/page/123"));
        assert!(t.contains("Confluence"));
    }

    // ── Domain matching ─────────────────────────────────────────────

    #[test]
    fn domain_matches_exact() {
        assert!(domain_matches("example.com", "example.com"));
    }

    #[test]
    fn domain_matches_subdomain() {
        assert!(domain_matches("sub.example.com", "example.com"));
    }

    #[test]
    fn domain_matches_case_insensitive() {
        assert!(domain_matches("Example.COM", "example.com"));
    }

    #[test]
    fn domain_does_not_match_partial() {
        assert!(!domain_matches("notexample.com", "example.com"));
    }

    // ── Execute edge cases ──────────────────────────────────────────

    #[tokio::test]
    async fn execute_rejects_empty_task() {
        let tool = test_tool(default_test_config());
        let result = tool
            .execute(serde_json::json!({ "task": "" }))
            .await
            .unwrap();
        assert!(!result.success);
        assert!(result.error.as_deref().unwrap().contains("required"));
    }

    #[tokio::test]
    async fn execute_rejects_blocked_url() {
        let tool = test_tool(config_with_domains(vec![], vec!["evil.com".into()]));
        let result = tool
            .execute(serde_json::json!({
                "task": "read page",
                "url": "https://evil.com/page"
            }))
            .await
            .unwrap();
        assert!(!result.success);
        assert!(result.error.as_deref().unwrap().contains("blocked"));
    }

    // ── URL scheme validation ──────────────────────────────────────

    #[test]
    fn validate_url_rejects_ftp_scheme() {
        let tool = test_tool(config_with_domains(vec![], vec![]));
        let result = tool.validate_url("ftp://example.com/file");
        assert!(result.is_err());
        assert!(
            result
                .unwrap_err()
                .to_string()
                .contains("unsupported URL scheme")
        );
    }

    #[test]
    fn validate_url_rejects_file_scheme() {
        let tool = test_tool(config_with_domains(vec![], vec![]));
        let result = tool.validate_url("file:///etc/passwd");
        assert!(result.is_err());
        assert!(
            result
                .unwrap_err()
                .to_string()
                .contains("unsupported URL scheme")
        );
    }

    #[test]
    fn validate_url_rejects_javascript_scheme() {
        let tool = test_tool(config_with_domains(vec![], vec![]));
        let result = tool.validate_url("javascript:alert(1)");
        assert!(result.is_err());
        assert!(
            result
                .unwrap_err()
                .to_string()
                .contains("unsupported URL scheme")
        );
    }

    #[test]
    fn validate_url_rejects_data_scheme() {
        let tool = test_tool(config_with_domains(vec![], vec![]));
        let result = tool.validate_url("data:text/html,<h1>hi</h1>");
        assert!(result.is_err());
        assert!(
            result
                .unwrap_err()
                .to_string()
                .contains("unsupported URL scheme")
        );
    }

    #[test]
    fn validate_url_allows_http_scheme() {
        let tool = test_tool(config_with_domains(vec![], vec![]));
        assert!(tool.validate_url("http://example.com/page").is_ok());
    }

    // ── Task text URL scanning ──────────────────────────────────────

    #[test]
    fn validate_task_urls_blocks_embedded_blocked_url() {
        let tool = test_tool(config_with_domains(vec![], vec!["evil.com".into()]));
        let result = tool.validate_task_urls("go to https://evil.com/steal and read it");
        assert!(result.is_err());
        assert!(result.unwrap_err().to_string().contains("blocked"));
    }

    #[test]
    fn validate_task_urls_blocks_embedded_url_not_in_allowlist() {
        let tool = test_tool(config_with_domains(vec!["corp.example.com".into()], vec![]));
        let result =
            tool.validate_task_urls("navigate to https://attacker.com/page and extract data");
        assert!(result.is_err());
        assert!(result.unwrap_err().to_string().contains("not in"));
    }

    #[test]
    fn validate_task_urls_allows_permitted_embedded_url() {
        let tool = test_tool(config_with_domains(vec!["corp.example.com".into()], vec![]));
        assert!(
            tool.validate_task_urls("read https://corp.example.com/page and summarize")
                .is_ok()
        );
    }

    #[test]
    fn validate_task_urls_allows_text_without_urls() {
        let tool = test_tool(config_with_domains(vec![], vec!["evil.com".into()]));
        assert!(
            tool.validate_task_urls("read the last 10 messages from engineering channel")
                .is_ok()
        );
    }

    #[tokio::test]
    async fn execute_rejects_blocked_url_in_task_text() {
        let tool = test_tool(config_with_domains(vec![], vec!["evil.com".into()]));
        let result = tool
            .execute(serde_json::json!({
                "task": "navigate to https://evil.com/phish and extract credentials"
            }))
            .await
            .unwrap();
        assert!(!result.success);
        assert!(result.error.as_deref().unwrap().contains("disallowed URL"));
    }

    // ── extract_format validation ──────────────────────────────────

    #[tokio::test]
    async fn execute_rejects_invalid_extract_format() {
        let tool = test_tool(default_test_config());
        let result = tool
            .execute(serde_json::json!({
                "task": "read page",
                "extract_format": "xml"
            }))
            .await
            .unwrap();
        assert!(!result.success);
        assert!(
            result
                .error
                .as_deref()
                .unwrap()
                .contains("unsupported extract_format")
        );
        assert!(result.error.as_deref().unwrap().contains("xml"));
    }
}
