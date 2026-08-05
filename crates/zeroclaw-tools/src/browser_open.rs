use crate::helpers::domain_guard;
use async_trait::async_trait;
use serde_json::json;
use std::{process::Stdio, sync::Arc, time::Duration};
use zeroclaw_api::tool::{Tool, ToolOutput, ToolResult};
use zeroclaw_config::policy::SecurityPolicy;

const BROWSER_OPEN_LAUNCH_TIMEOUT: Duration = Duration::from_secs(10);

/// Open approved HTTP/HTTPS URLs in the system default browser (no scraping, no DOM automation).
pub struct BrowserOpenTool {
    security: Arc<SecurityPolicy>,
    allowed_domains: Vec<String>,
    allowed_private_hosts: Vec<String>,
}

impl BrowserOpenTool {
    pub fn new(
        security: Arc<SecurityPolicy>,
        allowed_domains: Vec<String>,
    ) -> anyhow::Result<Self> {
        Self::new_with_private_hosts(security, allowed_domains, Vec::new())
    }

    pub fn new_with_private_hosts(
        security: Arc<SecurityPolicy>,
        allowed_domains: Vec<String>,
        allowed_private_hosts: Vec<String>,
    ) -> anyhow::Result<Self> {
        Ok(Self {
            security,
            allowed_domains: domain_guard::normalize_allowed_domains(
                allowed_domains,
                "browser.allowed_domains",
            )?,
            allowed_private_hosts: domain_guard::normalize_allowed_domains(
                allowed_private_hosts,
                "browser.allowed_private_hosts",
            )?,
        })
    }

    fn validate_url(&self, raw_url: &str) -> anyhow::Result<String> {
        let url = raw_url.trim();

        if url.is_empty() {
            anyhow::bail!("URL cannot be empty");
        }

        if url.chars().any(char::is_whitespace) {
            anyhow::bail!("URL cannot contain whitespace");
        }

        if !(url.starts_with("https://") || url.starts_with("http://")) {
            anyhow::bail!("Only http:// or https:// URLs are allowed");
        }

        if self.allowed_domains.is_empty() && self.allowed_private_hosts.is_empty() {
            anyhow::bail!(
                "Browser tool is enabled but no allowed_domains are configured. Add [browser].allowed_domains in config.toml"
            );
        }

        let host = extract_host(url)?;
        let private_host = domain_guard::is_private_or_local_host(&host);
        let private_host_allowed = private_host
            && domain_guard::host_matches_allowlist(&host, &self.allowed_private_hosts);

        if private_host && !private_host_allowed {
            anyhow::bail!("Blocked local/private host: {host}");
        }

        if private_host_allowed {
            return Ok(url.to_string());
        }

        if !domain_guard::host_matches_allowlist(&host, &self.allowed_domains) {
            anyhow::bail!("Host '{host}' is not in browser.allowed_domains");
        }

        Ok(url.to_string())
    }
}

#[async_trait]
impl Tool for BrowserOpenTool {
    fn name(&self) -> &str {
        "browser_open"
    }

    fn description(&self) -> &str {
        "Open an approved HTTP/HTTPS URL in the system browser. Security constraints: allowlist-only domains, no local/private hosts, no scraping."
    }

    fn parameters_schema(&self) -> serde_json::Value {
        json!({
            "type": "object",
            "properties": {
                "url": {
                    "type": "string",
                    "description": "HTTP or HTTPS URL to open in the system browser"
                }
            },
            "required": ["url"]
        })
    }

    async fn execute(&self, args: serde_json::Value) -> anyhow::Result<ToolResult> {
        let url = args.get("url").and_then(|v| v.as_str()).ok_or_else(|| {
            ::zeroclaw_log::record!(
                WARN,
                ::zeroclaw_log::Event::new(module_path!(), ::zeroclaw_log::Action::Reject)
                    .with_outcome(::zeroclaw_log::EventOutcome::Failure)
                    .with_attrs(::serde_json::json!({"param": "url"})),
                "browser_open: missing url parameter"
            );
            anyhow::Error::msg("Missing 'url' parameter")
        })?;

        if !self.security.can_act() {
            return Ok(ToolResult {
                success: false,
                output: ToolOutput::default(),
                error: Some("Action blocked: autonomy is read-only".into()),
            });
        }

        if !self.security.record_action() {
            return Ok(ToolResult {
                success: false,
                output: ToolOutput::default(),
                error: Some("Action blocked: rate limit exceeded".into()),
            });
        }

        let url = match self.validate_url(url) {
            Ok(v) => v,
            Err(e) => {
                return Ok(ToolResult {
                    success: false,
                    output: ToolOutput::default(),
                    error: Some(e.to_string()),
                });
            }
        };

        match open_in_system_browser(&url).await {
            Ok(()) => Ok(ToolResult {
                success: true,
                output: format!("Opened in system browser: {url}").into(),
                error: None,
            }),
            Err(e) => Ok(ToolResult {
                success: false,
                output: ToolOutput::default(),
                error: Some(format!("Failed to open system browser: {e}")),
            }),
        }
    }
}

async fn run_browser_launcher(command: tokio::process::Command, label: &str) -> Result<(), String> {
    run_browser_launcher_with_timeout(command, label, BROWSER_OPEN_LAUNCH_TIMEOUT).await
}

async fn run_browser_launcher_with_timeout(
    mut command: tokio::process::Command,
    label: &str,
    deadline: Duration,
) -> Result<(), String> {
    command
        .stdin(Stdio::null())
        .stdout(Stdio::null())
        .stderr(Stdio::null())
        .kill_on_drop(true);

    match tokio::time::timeout(deadline, command.status()).await {
        Ok(Ok(status)) if status.success() => Ok(()),
        Ok(Ok(status)) => Err(format!("{label} exited with status {status}")),
        Ok(Err(error)) => Err(format!("{label} not runnable: {error}")),
        Err(_) => Err(format!("{label} timed out after {deadline:?}")),
    }
}

async fn open_in_system_browser(url: &str) -> anyhow::Result<()> {
    #[cfg(target_os = "macos")]
    {
        let mut command = tokio::process::Command::new("open");
        command.arg(url);
        let primary_error = match run_browser_launcher(command, "open").await {
            Ok(()) => return Ok(()),
            Err(error) => error,
        };

        // TODO(compat): remove Brave fallback after default-browser launch has been stable across macOS environments.
        let mut brave_error = String::new();
        for app in ["Brave Browser", "Brave"] {
            let mut command = tokio::process::Command::new("open");
            command.arg("-a").arg(app).arg(url);
            match run_browser_launcher(command, &format!("open -a '{app}'")).await {
                Ok(()) => return Ok(()),
                Err(error) => brave_error = error,
            }
        }

        anyhow::bail!(
            "Failed to open URL with default browser launcher: {primary_error}. Brave compatibility fallback also failed: {brave_error}"
        );
    }

    #[cfg(target_os = "linux")]
    {
        let mut last_error = String::new();
        for cmd in [
            "xdg-open",
            "gio",
            "sensible-browser",
            "brave-browser",
            "brave",
        ] {
            let mut command = tokio::process::Command::new(cmd);
            if cmd == "gio" {
                command.arg("open");
            }
            command.arg(url);
            let label = if cmd == "gio" { "gio open" } else { cmd };
            match run_browser_launcher(command, label).await {
                Ok(()) => return Ok(()),
                Err(error) => last_error = error,
            }
        }

        // TODO(compat): remove Brave fallback commands (brave-browser/brave) once default launcher coverage is validated.
        anyhow::bail!(
            "Failed to open URL with default browser launchers; Brave compatibility fallback also failed. Last error: {last_error}"
        );
    }

    #[cfg(target_os = "windows")]
    {
        // Use direct process invocation (not `cmd /C start`) to avoid shell
        // metacharacter interpretation in URLs (e.g. `&` in query strings).
        let mut command = tokio::process::Command::new("rundll32");
        command.arg("url.dll,FileProtocolHandler").arg(url);
        let primary_error =
            match run_browser_launcher(command, "rundll32 default-browser launcher").await {
                Ok(()) => return Ok(()),
                Err(error) => error,
            };

        // TODO(compat): remove Brave fallback after default-browser launch has been stable across Windows environments.
        let mut brave_error = String::new();
        for cmd in ["brave", "brave.exe"] {
            let mut command = tokio::process::Command::new(cmd);
            command.arg(url);
            match run_browser_launcher(command, cmd).await {
                Ok(()) => return Ok(()),
                Err(error) => brave_error = error,
            }
        }

        anyhow::bail!(
            "Failed to open URL with default browser launcher: {primary_error}. Brave compatibility fallback also failed: {brave_error}"
        );
    }

    #[cfg(not(any(target_os = "macos", target_os = "linux", target_os = "windows")))]
    {
        let _ = url;
        anyhow::bail!("browser_open is not supported on this OS");
    }
}

fn extract_host(url: &str) -> anyhow::Result<String> {
    let rest = url
        .strip_prefix("https://")
        .or_else(|| url.strip_prefix("http://"))
        .ok_or_else(|| {
            ::zeroclaw_log::record!(
                WARN,
                ::zeroclaw_log::Event::new(module_path!(), ::zeroclaw_log::Action::Reject)
                    .with_outcome(::zeroclaw_log::EventOutcome::Failure)
                    .with_attrs(::serde_json::json!({"url": url})),
                "browser_open: unsupported URL scheme rejected"
            );
            anyhow::Error::msg("Only http:// or https:// URLs are allowed")
        })?;

    let authority = rest.split(['/', '?', '#']).next().ok_or_else(|| {
        ::zeroclaw_log::record!(
            WARN,
            ::zeroclaw_log::Event::new(module_path!(), ::zeroclaw_log::Action::Reject)
                .with_outcome(::zeroclaw_log::EventOutcome::Failure)
                .with_attrs(::serde_json::json!({"url": url})),
            "browser_open: invalid URL"
        );
        anyhow::Error::msg("Invalid URL")
    })?;

    if authority.is_empty() {
        anyhow::bail!("URL must include a host");
    }

    if authority.contains('@') {
        anyhow::bail!("URL userinfo is not allowed");
    }

    if authority.starts_with('[') {
        anyhow::bail!("IPv6 hosts are not supported in browser_open");
    }

    let host = authority
        .split(':')
        .next()
        .unwrap_or_default()
        .trim()
        .trim_end_matches('.')
        .to_lowercase();

    if host.is_empty() {
        anyhow::bail!("URL must include a valid host");
    }

    Ok(host)
}

#[cfg(test)]
mod tests {
    use super::*;
    use zeroclaw_config::autonomy::AutonomyLevel;
    use zeroclaw_config::policy::SecurityPolicy;

    fn test_tool(allowed_domains: Vec<&str>) -> BrowserOpenTool {
        let security = Arc::new(SecurityPolicy {
            autonomy: AutonomyLevel::Supervised,
            ..SecurityPolicy::default()
        });
        BrowserOpenTool::new(
            security,
            allowed_domains.into_iter().map(String::from).collect(),
        )
        .unwrap()
    }

    fn test_tool_with_private(
        allowed_domains: Vec<&str>,
        allowed_private_hosts: Vec<&str>,
    ) -> BrowserOpenTool {
        let security = Arc::new(SecurityPolicy {
            autonomy: AutonomyLevel::Supervised,
            ..SecurityPolicy::default()
        });
        BrowserOpenTool::new_with_private_hosts(
            security,
            allowed_domains.into_iter().map(String::from).collect(),
            allowed_private_hosts
                .into_iter()
                .map(String::from)
                .collect(),
        )
        .unwrap()
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
    fn validate_accepts_http() {
        let tool = test_tool(vec!["example.com"]);
        let got = tool.validate_url("http://example.com/docs").unwrap();
        assert_eq!(got, "http://example.com/docs");
    }

    #[test]
    fn validate_accepts_http_with_port() {
        let tool = test_tool(vec!["example.com"]);
        let got = tool
            .validate_url("http://example.com:8080/path?q=1")
            .unwrap();
        assert_eq!(got, "http://example.com:8080/path?q=1");
    }

    #[test]
    fn validate_accepts_http_for_wildcard_allowlist() {
        let tool = test_tool(vec!["*"]);
        let got = tool.validate_url("http://example.com/page").unwrap();
        assert_eq!(got, "http://example.com/page");
    }

    #[test]
    fn validate_rejects_unsupported_scheme() {
        let tool = test_tool(vec!["example.com"]);
        let err = tool
            .validate_url("ftp://example.com")
            .unwrap_err()
            .to_string();
        assert!(err.contains("http://"));
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
        let tool = BrowserOpenTool::new(security, vec![]).unwrap();
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
        let tool = BrowserOpenTool::new(security, vec!["example.com".into()]).unwrap();
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
        let tool = BrowserOpenTool::new(security, vec!["example.com".into()]).unwrap();
        let result = tool
            .execute(json!({"url": "https://example.com"}))
            .await
            .unwrap();
        assert!(!result.success);
        assert!(result.error.unwrap().contains("rate limit"));
    }

    #[cfg(unix)]
    #[tokio::test]
    async fn launcher_helper_times_out_stalled_process() {
        let mut command = tokio::process::Command::new("sleep");
        command.arg("60");

        let started = std::time::Instant::now();
        let error =
            run_browser_launcher_with_timeout(command, "test launcher", Duration::from_millis(20))
                .await
                .unwrap_err();

        assert!(error.contains("timed out"));
        assert!(
            started.elapsed() < Duration::from_secs(2),
            "timeout helper waited too long: {:?}",
            started.elapsed()
        );
    }

    #[cfg(unix)]
    #[tokio::test]
    async fn launcher_helper_allows_successful_process() {
        let command = tokio::process::Command::new("true");

        run_browser_launcher_with_timeout(command, "test launcher", Duration::from_secs(1))
            .await
            .unwrap();
    }

    // ── allowed_private_hosts opt-in tests ──────────────────────

    #[test]
    fn wildcard_private_allowlist_permits_localhost() {
        let tool = test_tool_with_private(vec![], vec!["*"]);
        assert!(tool.validate_url("https://localhost:8443").is_ok());
    }

    #[test]
    fn wildcard_private_allowlist_permits_private_ipv4() {
        let tool = test_tool_with_private(vec![], vec!["*"]);
        assert!(tool.validate_url("https://192.168.1.5").is_ok());
    }

    #[test]
    fn allowed_private_hosts_entry_permits_listed_host() {
        let tool = test_tool_with_private(vec![], vec!["10.0.0.1"]);
        assert!(tool.validate_url("https://10.0.0.1").is_ok());
    }

    #[test]
    fn allowed_private_hosts_does_not_permit_unlisted_host() {
        let tool = test_tool_with_private(vec![], vec!["10.0.0.1"]);
        let err = tool
            .validate_url("https://10.0.0.2")
            .unwrap_err()
            .to_string();
        assert!(err.contains("local/private"));
    }

    #[test]
    fn empty_private_allowlist_still_rejects_private() {
        let tool = test_tool_with_private(vec!["*"], vec![]);
        let err = tool
            .validate_url("https://localhost")
            .unwrap_err()
            .to_string();
        assert!(err.contains("local/private"));
    }

    #[test]
    fn wildcard_private_allowlist_alone_satisfies_allowlist_requirement() {
        // allowed_domains empty + allowed_private_hosts=["*"] should not surface
        // the "no allowed_domains configured" error for private hosts.
        let tool = test_tool_with_private(vec![], vec!["*"]);
        assert!(tool.validate_url("https://localhost").is_ok());
    }

    #[test]
    fn specific_private_host_alone_satisfies_allowlist_requirement() {
        let tool = test_tool_with_private(vec![], vec!["192.168.1.5"]);
        assert!(tool.validate_url("https://192.168.1.5").is_ok());
    }

    #[test]
    fn listed_private_host_permits_http_scheme() {
        let tool = test_tool_with_private(vec![], vec!["10.0.0.1"]);
        assert!(tool.validate_url("http://10.0.0.1").is_ok());
    }
}
