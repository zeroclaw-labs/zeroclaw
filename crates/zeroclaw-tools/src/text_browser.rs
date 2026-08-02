use async_trait::async_trait;
use serde_json::json;
use std::sync::Arc;
use std::time::Duration;
use zeroclaw_api::tool::{Tool, ToolOutput, ToolResult};
use zeroclaw_config::policy::SecurityPolicy;

use crate::helpers::domain_guard;

/// Text browser tool: renders web pages as plain text using text-based browsers
/// (lynx, links, w3m). Ideal for headless/SSH environments where graphical
/// browsers are unavailable.
pub struct TextBrowserTool {
    security: Arc<SecurityPolicy>,
    preferred_browser: Option<String>,
    timeout_secs: u64,
    max_response_size: usize,
    allowed_private_hosts: Vec<String>,
}

/// The text browsers we support, in order of auto-detection preference.
const SUPPORTED_BROWSERS: &[&str] = &["lynx", "links", "w3m"];

impl TextBrowserTool {
    /// Construct with no private-host opt-in (deny-by-default). Use
    /// [`Self::new_with_private_hosts`] to allow specific private hosts.
    pub fn new(
        security: Arc<SecurityPolicy>,
        preferred_browser: Option<String>,
        timeout_secs: u64,
    ) -> anyhow::Result<Self> {
        Self::new_with_private_hosts(security, preferred_browser, timeout_secs, Vec::new())
    }

    /// Construct with an explicit `allowed_private_hosts` opt-in list (mirrors
    /// the `browser`/`browser_open`/`http_request` pattern from PRsand
    pub fn new_with_private_hosts(
        security: Arc<SecurityPolicy>,
        preferred_browser: Option<String>,
        timeout_secs: u64,
        allowed_private_hosts: Vec<String>,
    ) -> anyhow::Result<Self> {
        Ok(Self {
            security,
            preferred_browser,
            timeout_secs,
            max_response_size: 500_000, // 500KB, consistent with web_fetch
            allowed_private_hosts: domain_guard::normalize_allowed_domains(
                allowed_private_hosts,
                "text_browser.allowed_private_hosts",
            )?,
        })
    }

    fn validate_url(&self, url: &str) -> anyhow::Result<String> {
        self.validate_url_with_dns_check(url, validate_resolved_host_is_public)
    }

    /// Internal entry that accepts a pluggable DNS validator. Mirrors
    /// `web_fetch::validate_target_url_with_dns_check` so unit tests can
    /// drive the resolved-IP SSRF gate without depending on real DNS.
    fn validate_url_with_dns_check(
        &self,
        url: &str,
        validate_dns: impl FnOnce(&str) -> anyhow::Result<()>,
    ) -> anyhow::Result<String> {
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

        let parsed = reqwest::Url::parse(url)
            .map_err(|e| anyhow::Error::msg(format!("Invalid URL format: {e}")))?;

        if !parsed.username().is_empty() || parsed.password().is_some() {
            anyhow::bail!("URL userinfo is not allowed");
        }

        let host_str = parsed
            .host_str()
            .ok_or_else(|| anyhow::Error::msg("URL must include a host"))?;

        let bare_host = host_str.trim_start_matches('[').trim_end_matches(']');
        let is_ipv6 = bare_host.parse::<std::net::Ipv6Addr>().is_ok();
        let (host, display_host) = if is_ipv6 {
            let bare = bare_host.parse::<std::net::Ipv6Addr>().unwrap().to_string();
            (bare.clone(), format!("[{bare}]"))
        } else {
            let h = host_str.to_lowercase();
            (h.clone(), h)
        };

        // SSRF gate: deny by default for private/local hosts unless the operator
        // explicitly listed them. Mirrors `browser`/`http_request`/`web_fetch`.
        let private_host = domain_guard::is_private_or_local_host(&host);
        let host_allowed = domain_guard::host_matches_allowlist(&host, &self.allowed_private_hosts);

        if private_host && !host_allowed {
            anyhow::bail!("Blocked local/private host: {display_host}");
        }

        if !host_allowed {
            validate_dns(&host)?;
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
                && output.status.success()
            {
                return Some((*browser).to_string());
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
                ::zeroclaw_log::record!(
                    WARN,
                    ::zeroclaw_log::Event::new(module_path!(), ::zeroclaw_log::Action::Note)
                        .with_outcome(::zeroclaw_log::EventOutcome::Unknown)
                        .with_attrs(::serde_json::json!({"preferred": preferred})),
                    "Configured preferred text browser '' is not installed, falling back to auto-detect"
                );
            }
        }

        // Auto-detect
        Self::detect_browser().await.ok_or_else(|| {
            let supported = SUPPORTED_BROWSERS.join(", ");
            ::zeroclaw_log::record!(
                ERROR,
                ::zeroclaw_log::Event::new(module_path!(), ::zeroclaw_log::Action::Reject)
                    .with_outcome(::zeroclaw_log::EventOutcome::Failure)
                    .with_attrs(::serde_json::json!({"supported": &supported})),
                "text_browser: no text browser installed"
            );
            anyhow::Error::msg(format!(
                "No text browser found. Install one of: {supported}"
            ))
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
        let url = args.get("url").and_then(|v| v.as_str()).ok_or_else(|| {
            ::zeroclaw_log::record!(
                WARN,
                ::zeroclaw_log::Event::new(module_path!(), ::zeroclaw_log::Action::Reject)
                    .with_outcome(::zeroclaw_log::EventOutcome::Failure)
                    .with_attrs(::serde_json::json!({"param": "url"})),
                "text_browser: missing url parameter"
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

        let requested_browser = args.get("browser").and_then(|v| v.as_str());

        let browser = match self.resolve_browser(requested_browser).await {
            Ok(b) => b,
            Err(e) => {
                return Ok(ToolResult {
                    success: false,
                    output: ToolOutput::default(),
                    error: Some(e.to_string()),
                });
            }
        };

        let dump_args = Self::build_dump_args(&browser, &url);

        let timeout = Duration::from_secs(if self.timeout_secs == 0 {
            ::zeroclaw_log::record!(
                WARN,
                ::zeroclaw_log::Event::new(module_path!(), ::zeroclaw_log::Action::Note)
                    .with_outcome(::zeroclaw_log::EventOutcome::Unknown),
                "text_browser: timeout_secs is 0, using safe default of 30s"
            );
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
                        output: text.into(),
                        error: None,
                    })
                } else {
                    let stderr = String::from_utf8_lossy(&output.stderr);
                    Ok(ToolResult {
                        success: false,
                        output: ToolOutput::default(),
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
                output: ToolOutput::default(),
                error: Some(format!("Failed to execute {browser}: {e}")),
            }),
            Err(_) => Ok(ToolResult {
                success: false,
                output: ToolOutput::default(),
                error: Some(format!(
                    "{browser} timed out after {} seconds",
                    timeout.as_secs()
                )),
            }),
        }
    }
}

// ── Helper functions ────────────────────────────────────────────────────────

#[cfg(not(test))]
fn validate_resolved_host_is_public(host: &str) -> anyhow::Result<()> {
    use std::net::ToSocketAddrs;

    let ips = (host, 0)
        .to_socket_addrs()
        .map_err(|e| {
            ::zeroclaw_log::record!(
                ERROR,
                ::zeroclaw_log::Event::new(module_path!(), ::zeroclaw_log::Action::Fail)
                    .with_outcome(::zeroclaw_log::EventOutcome::Failure)
                    .with_attrs(::serde_json::json!({
                        "host": host,
                        "error": format!("{}", e),
                    })),
                "text_browser: failed to resolve host"
            );
            anyhow::Error::msg(format!("Failed to resolve host '{host}': {e}"))
        })?
        .map(|addr| addr.ip())
        .collect::<Vec<_>>();

    domain_guard::validate_resolved_ips_are_public(host, &ips)
}

#[cfg(test)]
fn validate_resolved_host_is_public(_host: &str) -> anyhow::Result<()> {
    // DNS checks are covered by validate_resolved_ips_are_public unit tests.
    Ok(())
}

#[cfg(test)]
mod tests {
    use super::*;
    use zeroclaw_config::autonomy::AutonomyLevel;
    use zeroclaw_config::policy::SecurityPolicy;

    fn test_tool() -> TextBrowserTool {
        let security = Arc::new(SecurityPolicy {
            autonomy: AutonomyLevel::Supervised,
            ..SecurityPolicy::default()
        });
        TextBrowserTool::new(security, None, 30).unwrap()
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
        let tool = test_tool();
        let got = tool.validate_url("http://example.com/page").unwrap();
        assert_eq!(got, "http://example.com/page");
    }

    #[test]
    fn validate_url_accepts_https() {
        let tool = test_tool();
        let got = tool.validate_url("https://example.com/page").unwrap();
        assert_eq!(got, "https://example.com/page");
    }

    #[test]
    fn validate_url_rejects_empty() {
        let tool = test_tool();
        let err = tool.validate_url("").unwrap_err().to_string();
        assert!(err.contains("empty"));
    }

    #[test]
    fn validate_url_rejects_ftp() {
        let tool = test_tool();
        let err = tool
            .validate_url("ftp://example.com")
            .unwrap_err()
            .to_string();
        assert!(err.contains("http://") || err.contains("https://"));
    }

    #[test]
    fn validate_url_rejects_whitespace() {
        let tool = test_tool();
        let err = tool
            .validate_url("https://example.com/hello world")
            .unwrap_err()
            .to_string();
        assert!(err.contains("whitespace"));
    }

    #[test]
    fn validate_url_with_dns_check_rejects_hostname_resolving_to_private_ip() {
        let tool = test_tool();
        let err = tool
            .validate_url_with_dns_check("http://internal.corp/", |host| {
                domain_guard::validate_resolved_ips_are_public(
                    host,
                    &[std::net::IpAddr::V4(std::net::Ipv4Addr::new(10, 0, 0, 5))],
                )
            })
            .unwrap_err()
            .to_string();
        assert!(
            err.contains("non-global") || err.contains("10.0.0.5"),
            "expected resolved-IP gate to reject 10.0.0.5, got: {err}"
        );
    }

    #[test]
    fn validate_url_with_dns_check_rejects_hostname_resolving_to_metadata_ip() {
        // DNS-rebinding / attacker-controlled NS pointing at the EC2 metadata
        // service: must be rejected even when the host string is not
        // literally private.
        let tool = test_tool();
        let err = tool
            .validate_url_with_dns_check("http://attacker.example.com/", |host| {
                domain_guard::validate_resolved_ips_are_public(
                    host,
                    &[std::net::IpAddr::V4(std::net::Ipv4Addr::new(
                        169, 254, 169, 254,
                    ))],
                )
            })
            .unwrap_err()
            .to_string();
        assert!(
            err.contains("metadata") || err.contains("169.254.169.254"),
            "expected metadata-IP gate to fire, got: {err}"
        );
    }

    #[test]
    fn validate_url_with_dns_check_skips_dns_when_private_host_allowlisted() {
        let security = Arc::new(SecurityPolicy::default());
        let tool =
            TextBrowserTool::new_with_private_hosts(security, None, 30, vec!["10.0.0.5".into()])
                .unwrap();
        // DNS validator should never be called: even if it errors, the call
        // succeeds because the allowlist lifts the gate.
        let got = tool
            .validate_url_with_dns_check("http://10.0.0.5/", |_host| {
                Err(anyhow::Error::msg("DNS validator should not be invoked"))
            })
            .unwrap();
        assert_eq!(got, "http://10.0.0.5/");
    }

    #[test]
    fn validate_url_with_dns_check_skips_dns_when_wildcard_allowlisted() {
        // Operator's `allowed_private_hosts = ["*"]` is the blanket opt-in.
        // The resolved-IP gate is skipped for a literal-private host; the
        // literal-host gate also passes via the wildcard match.
        let security = Arc::new(SecurityPolicy::default());
        let tool =
            TextBrowserTool::new_with_private_hosts(security, None, 30, vec!["*".into()]).unwrap();
        let got = tool
            .validate_url_with_dns_check("http://10.0.0.5/", |_host| {
                Err(anyhow::Error::msg("DNS validator should not be invoked"))
            })
            .unwrap();
        assert_eq!(got, "http://10.0.0.5/");
    }

    #[test]
    fn validate_url_with_dns_check_skips_dns_when_public_looking_hostname_allowlisted() {
        // Regression for Audacity88's 2026-07-04 review of when the
        // operator lists a public-looking hostname (not literally private) in
        // `allowed_private_hosts`, the resolved-IP gate must be skipped even
        // though `is_private_or_local_host` returns false for the host string.
        let security = Arc::new(SecurityPolicy::default());
        let tool = TextBrowserTool::new_with_private_hosts(
            security,
            None,
            30,
            vec!["internal.corp".into()],
        )
        .unwrap();
        let got = tool
            .validate_url_with_dns_check("http://internal.corp/", |_host| {
                Err(anyhow::Error::msg("DNS validator should not be invoked"))
            })
            .unwrap();
        assert_eq!(got, "http://internal.corp/");
    }

    #[test]
    fn validate_url_with_dns_check_skips_dns_when_public_looking_hostname_wildcard() {
        // With `["*"]` wildcard, a public-looking hostname resolving to a
        // private IP must also skip the DNS gate.
        let security = Arc::new(SecurityPolicy::default());
        let tool =
            TextBrowserTool::new_with_private_hosts(security, None, 30, vec!["*".into()]).unwrap();
        let got = tool
            .validate_url_with_dns_check("http://internal.corp/", |_host| {
                Err(anyhow::Error::msg("DNS validator should not be invoked"))
            })
            .unwrap();
        assert_eq!(got, "http://internal.corp/");
    }

    #[test]
    fn validate_url_with_dns_check_accepts_hostname_resolving_to_public_ip() {
        // Sanity: a public-looking name that resolves to a public IP passes
        // both gates.
        let tool = test_tool();
        let got = tool
            .validate_url_with_dns_check("http://example.com/page", |host| {
                domain_guard::validate_resolved_ips_are_public(
                    host,
                    &[std::net::IpAddr::V4(std::net::Ipv4Addr::new(
                        93, 184, 216, 34,
                    ))],
                )
            })
            .unwrap();
        assert_eq!(got, "http://example.com/page");
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
        let mut tool = TextBrowserTool::new(security, None, 30).unwrap();
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
        let tool = TextBrowserTool::new(security, None, 30).unwrap();
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
        let tool = TextBrowserTool::new(security, None, 30).unwrap();
        let result = tool
            .execute(json!({"url": "https://example.com"}))
            .await
            .unwrap();
        assert!(!result.success);
        assert!(result.error.unwrap().contains("rate limit"));
    }

    fn private_tool(allowed_private_hosts: Vec<&str>) -> TextBrowserTool {
        let security = Arc::new(SecurityPolicy {
            autonomy: AutonomyLevel::Supervised,
            ..SecurityPolicy::default()
        });
        TextBrowserTool::new_with_private_hosts(
            security,
            None,
            30,
            allowed_private_hosts
                .into_iter()
                .map(String::from)
                .collect(),
        )
        .unwrap()
    }

    #[test]
    fn rejects_loopback_by_default() {
        let tool = private_tool(vec![]);
        let err = tool
            .validate_url("http://localhost/page")
            .unwrap_err()
            .to_string();
        assert!(err.contains("local/private"), "got: {err}");
    }

    #[test]
    fn rejects_rfc1918_by_default() {
        let tool = private_tool(vec![]);
        for url in ["http://10.0.0.5", "http://192.168.1.5", "http://172.16.0.1"] {
            let err = tool.validate_url(url).unwrap_err().to_string();
            assert!(err.contains("local/private"), "got: {err} for {url}");
        }
    }

    #[test]
    fn rejects_cloud_metadata_by_default() {
        let tool = private_tool(vec![]);
        let err = tool
            .validate_url("http://169.254.169.254/latest/meta-data/")
            .unwrap_err()
            .to_string();
        assert!(err.contains("local/private"), "got: {err}");
    }

    #[test]
    fn rejects_link_local_ipv6_by_default() {
        let tool = private_tool(vec![]);
        let err = tool
            .validate_url("http://[fe80::1]/")
            .unwrap_err()
            .to_string();
        assert!(err.contains("local/private"), "got: {err}");
    }

    #[test]
    fn wildcard_private_allowlist_permits_localhost() {
        let tool = private_tool(vec!["*"]);
        assert!(tool.validate_url("http://localhost/page").is_ok());
        assert!(tool.validate_url("https://localhost:8443/x").is_ok());
    }

    #[test]
    fn wildcard_private_allowlist_permits_rfc1918() {
        let tool = private_tool(vec!["*"]);
        assert!(tool.validate_url("http://10.0.0.1").is_ok());
        assert!(tool.validate_url("http://192.168.1.5").is_ok());
    }

    #[test]
    fn specific_private_host_entry_permits_listed_host() {
        let tool = private_tool(vec!["10.0.0.1"]);
        assert!(tool.validate_url("http://10.0.0.1").is_ok());
    }

    #[test]
    fn specific_ipv6_loopback_allowlist_permits_bracketed_url() {
        // Regression for Audacity88's 2026-07-04 review of an explicit
        // IPv6 allowlist entry like "::1" must match the URL http://[::1]/ even
        // though the URL host is parsed with brackets while the normalized
        // allowlist entry stores the bare IP.
        let tool = private_tool(vec!["::1"]);
        assert!(tool.validate_url("http://[::1]/").is_ok());
        assert!(tool.validate_url("https://[::1]:8443/").is_ok());
    }

    #[test]
    fn specific_ipv6_link_local_allowlist_permits_bracketed_url() {
        let tool = private_tool(vec!["fe80::1"]);
        assert!(tool.validate_url("http://[fe80::1]/").is_ok());
    }

    #[test]
    fn specific_ipv6_allowlist_does_not_match_unlisted() {
        let tool = private_tool(vec!["::1"]);
        let err = tool
            .validate_url("http://[fe80::1]/")
            .unwrap_err()
            .to_string();
        assert!(
            err.contains("local/private") || err.contains("Blocked"),
            "got: {err}"
        );
    }

    #[test]
    fn specific_private_host_entry_does_not_match_unlisted() {
        let tool = private_tool(vec!["10.0.0.1"]);
        let err = tool
            .validate_url("http://10.0.0.2")
            .unwrap_err()
            .to_string();
        assert!(err.contains("local/private"), "got: {err}");
    }

    #[test]
    fn rejects_userinfo_targeting_private_host() {
        // `reqwest::Url` rejects userinfo outright — parser-level SSRF defense
        // (no operator opt-out). Mirrors the `browser` tool fix in
        let tool = private_tool(vec!["*"]);
        let err = tool
            .validate_url("http://example.com@127.0.0.1/")
            .unwrap_err()
            .to_string();
        assert!(err.contains("userinfo"), "got: {err}");
    }

    #[test]
    fn rejects_userinfo_with_password() {
        let tool = private_tool(vec!["*"]);
        let err = tool
            .validate_url("https://user:pass@10.0.0.1/")
            .unwrap_err()
            .to_string();
        assert!(err.contains("userinfo"), "got: {err}");
    }
}
