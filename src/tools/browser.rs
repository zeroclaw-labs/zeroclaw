//! Browser automation tool using Vercel's agent-browser CLI
//!
//! This tool provides AI-optimized web browsing capabilities via the agent-browser CLI.
//! It supports semantic element selection, accessibility snapshots, and JSON output
//! for efficient LLM integration.

use super::traits::{Tool, ToolResult};
use crate::security::SecurityPolicy;
use async_trait::async_trait;
use serde::{Deserialize, Serialize};
use serde_json::{json, Value};
use std::process::Stdio;
use std::sync::Arc;
use tokio::process::Command;
use tracing::debug;

/// Browser automation tool using agent-browser CLI
pub struct BrowserTool {
    security: Arc<SecurityPolicy>,
    allowed_domains: Vec<String>,
    session_name: Option<String>,
}

/// Response from agent-browser --json commands
#[derive(Debug, Deserialize)]
struct AgentBrowserResponse {
    success: bool,
    data: Option<Value>,
    error: Option<String>,
}

/// Supported browser actions
#[derive(Debug, Clone, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum BrowserAction {
    /// Navigate to a URL
    Open { url: String },
    /// Get accessibility snapshot with refs
    Snapshot {
        #[serde(default)]
        interactive_only: bool,
        #[serde(default)]
        compact: bool,
        #[serde(default)]
        depth: Option<u32>,
    },
    /// Click an element by ref or selector
    Click { selector: String },
    /// Fill a form field
    Fill { selector: String, value: String },
    /// Type text into focused element
    Type { selector: String, text: String },
    /// Get text content of element
    GetText { selector: String },
    /// Get page title
    GetTitle,
    /// Get current URL
    GetUrl,
    /// Take screenshot
    Screenshot {
        #[serde(default)]
        path: Option<String>,
        #[serde(default)]
        full_page: bool,
    },
    /// Wait for element or time
    Wait {
        #[serde(default)]
        selector: Option<String>,
        #[serde(default)]
        ms: Option<u64>,
        #[serde(default)]
        text: Option<String>,
    },
    /// Press a key
    Press { key: String },
    /// Hover over element
    Hover { selector: String },
    /// Scroll page
    Scroll {
        direction: String,
        #[serde(default)]
        pixels: Option<u32>,
    },
    /// Check if element is visible
    IsVisible { selector: String },
    /// Close browser
    Close,
    /// Find element by semantic locator
    Find {
        by: String, // role, text, label, placeholder, testid
        value: String,
        action: String, // click, fill, text, hover
        #[serde(default)]
        fill_value: Option<String>,
    },
}

impl BrowserTool {
    pub fn new(
        security: Arc<SecurityPolicy>,
        allowed_domains: Vec<String>,
        session_name: Option<String>,
    ) -> Self {
        Self {
            security,
            allowed_domains: normalize_domains(allowed_domains),
            session_name,
        }
    }

    /// Check if agent-browser CLI is available
    pub async fn is_available() -> bool {
        Command::new("agent-browser")
            .arg("--version")
            .stdout(Stdio::null())
            .stderr(Stdio::null())
            .status()
            .await
            .map(|s| s.success())
            .unwrap_or(false)
    }

    /// Validate URL against allowlist
    fn validate_url(&self, url: &str) -> anyhow::Result<()> {
        let url = url.trim();

        if url.is_empty() {
            anyhow::bail!("URL cannot be empty");
        }

        // Allow file:// URLs for local testing
        if url.starts_with("file://") {
            return Ok(());
        }

        if !url.starts_with("https://") && !url.starts_with("http://") {
            anyhow::bail!("Only http:// and https:// URLs are allowed");
        }

        if self.allowed_domains.is_empty() {
            anyhow::bail!(
                "Browser tool enabled but no allowed_domains configured. \
                Add [browser].allowed_domains in config.toml"
            );
        }

        let host = extract_host(url)?;

        if is_private_host(&host) {
            anyhow::bail!("Blocked local/private host: {host}");
        }

        if !host_matches_allowlist(&host, &self.allowed_domains) {
            anyhow::bail!("Host '{host}' not in browser.allowed_domains");
        }

        Ok(())
    }

    /// Execute an agent-browser command
    async fn run_command(&self, args: &[&str]) -> anyhow::Result<AgentBrowserResponse> {
        let mut cmd = Command::new("agent-browser");

        // Add session if configured
        if let Some(ref session) = self.session_name {
            cmd.arg("--session").arg(session);
        }

        // Add --json for machine-readable output
        cmd.args(args).arg("--json");

        debug!("Running: agent-browser {} --json", args.join(" "));

        let output = cmd
            .stdout(Stdio::piped())
            .stderr(Stdio::piped())
            .output()
            .await?;

        let stdout = String::from_utf8_lossy(&output.stdout);
        let stderr = String::from_utf8_lossy(&output.stderr);

        if !stderr.is_empty() {
            debug!("agent-browser stderr: {}", stderr);
        }

        // Parse JSON response
        if let Ok(resp) = serde_json::from_str::<AgentBrowserResponse>(&stdout) {
            return Ok(resp);
        }

        // Fallback for non-JSON output
        if output.status.success() {
            Ok(AgentBrowserResponse {
                success: true,
                data: Some(json!({ "output": stdout.trim() })),
                error: None,
            })
        } else {
            Ok(AgentBrowserResponse {
                success: false,
                data: None,
                error: Some(stderr.trim().to_string()),
            })
        }
    }

    /// Execute a browser action
    #[allow(clippy::too_many_lines)]
    async fn execute_action(&self, action: BrowserAction) -> anyhow::Result<ToolResult> {
        match action {
            BrowserAction::Open { url } => {
                self.validate_url(&url)?;
                let resp = self.run_command(&["open", &url]).await?;
                self.to_result(resp)
            }

            BrowserAction::Snapshot {
                interactive_only,
                compact,
                depth,
            } => {
                let mut args = vec!["snapshot"];
                if interactive_only {
                    args.push("-i");
                }
                if compact {
                    args.push("-c");
                }
                let depth_str;
                if let Some(d) = depth {
                    args.push("-d");
                    depth_str = d.to_string();
                    args.push(&depth_str);
                }
                let resp = self.run_command(&args).await?;
                self.to_result(resp)
            }

            BrowserAction::Click { selector } => {
                let resp = self.run_command(&["click", &selector]).await?;
                self.to_result(resp)
            }

            BrowserAction::Fill { selector, value } => {
                let resp = self.run_command(&["fill", &selector, &value]).await?;
                self.to_result(resp)
            }

            BrowserAction::Type { selector, text } => {
                let resp = self.run_command(&["type", &selector, &text]).await?;
                self.to_result(resp)
            }

            BrowserAction::GetText { selector } => {
                let resp = self.run_command(&["get", "text", &selector]).await?;
                self.to_result(resp)
            }

            BrowserAction::GetTitle => {
                let resp = self.run_command(&["get", "title"]).await?;
                self.to_result(resp)
            }

            BrowserAction::GetUrl => {
                let resp = self.run_command(&["get", "url"]).await?;
                self.to_result(resp)
            }

            BrowserAction::Screenshot { path, full_page } => {
                let mut args = vec!["screenshot"];
                if let Some(ref p) = path {
                    args.push(p);
                }
                if full_page {
                    args.push("--full");
                }
                let resp = self.run_command(&args).await?;
                self.to_result(resp)
            }

            BrowserAction::Wait { selector, ms, text } => {
                let mut args = vec!["wait"];
                let ms_str;
                if let Some(sel) = selector.as_ref() {
                    args.push(sel);
                } else if let Some(millis) = ms {
                    ms_str = millis.to_string();
                    args.push(&ms_str);
                } else if let Some(ref t) = text {
                    args.push("--text");
                    args.push(t);
                }
                let resp = self.run_command(&args).await?;
                self.to_result(resp)
            }

            BrowserAction::Press { key } => {
                let resp = self.run_command(&["press", &key]).await?;
                self.to_result(resp)
            }

            BrowserAction::Hover { selector } => {
                let resp = self.run_command(&["hover", &selector]).await?;
                self.to_result(resp)
            }

            BrowserAction::Scroll { direction, pixels } => {
                let mut args = vec!["scroll", &direction];
                let px_str;
                if let Some(px) = pixels {
                    px_str = px.to_string();
                    args.push(&px_str);
                }
                let resp = self.run_command(&args).await?;
                self.to_result(resp)
            }

            BrowserAction::IsVisible { selector } => {
                let resp = self.run_command(&["is", "visible", &selector]).await?;
                self.to_result(resp)
            }

            BrowserAction::Close => {
                let resp = self.run_command(&["close"]).await?;
                self.to_result(resp)
            }

            BrowserAction::Find {
                by,
                value,
                action,
                fill_value,
            } => {
                let mut args = vec!["find", &by, &value, &action];
                if let Some(ref fv) = fill_value {
                    args.push(fv);
                }
                let resp = self.run_command(&args).await?;
                self.to_result(resp)
            }
        }
    }

    #[allow(clippy::unnecessary_wraps, clippy::unused_self)]
    fn to_result(&self, resp: AgentBrowserResponse) -> anyhow::Result<ToolResult> {
        if resp.success {
            let output = resp
                .data
                .map(|d| serde_json::to_string_pretty(&d).unwrap_or_default())
                .unwrap_or_default();
            Ok(ToolResult {
                success: true,
                output,
                error: None,
            })
        } else {
            Ok(ToolResult {
                success: false,
                output: String::new(),
                error: resp.error,
            })
        }
    }
}

#[allow(clippy::too_many_lines)]
#[async_trait]
impl Tool for BrowserTool {
    fn name(&self) -> &str {
        "browser"
    }

    fn description(&self) -> &str {
        "Web browser automation using agent-browser. Supports navigation, clicking, \
        filling forms, taking screenshots, and getting accessibility snapshots with refs. \
        Use 'snapshot' to get interactive elements with refs (@e1, @e2), then use refs \
        for precise element interaction. Allowed domains only."
    }

    fn parameters_schema(&self) -> Value {
        json!({
            "type": "object",
            "properties": {
                "action": {
                    "type": "string",
                    "enum": ["open", "snapshot", "click", "fill", "type", "get_text",
                             "get_title", "get_url", "screenshot", "wait", "press",
                             "hover", "scroll", "is_visible", "close", "find"],
                    "description": "Browser action to perform"
                },
                "url": {
                    "type": "string",
                    "description": "URL to navigate to (for 'open' action)"
                },
                "selector": {
                    "type": "string",
                    "description": "Element selector: @ref (e.g. @e1), CSS (#id, .class), or text=..."
                },
                "value": {
                    "type": "string",
                    "description": "Value to fill or type"
                },
                "text": {
                    "type": "string",
                    "description": "Text to type or wait for"
                },
                "key": {
                    "type": "string",
                    "description": "Key to press (Enter, Tab, Escape, etc.)"
                },
                "direction": {
                    "type": "string",
                    "enum": ["up", "down", "left", "right"],
                    "description": "Scroll direction"
                },
                "pixels": {
                    "type": "integer",
                    "description": "Pixels to scroll"
                },
                "interactive_only": {
                    "type": "boolean",
                    "description": "For snapshot: only show interactive elements"
                },
                "compact": {
                    "type": "boolean",
                    "description": "For snapshot: remove empty structural elements"
                },
                "depth": {
                    "type": "integer",
                    "description": "For snapshot: limit tree depth"
                },
                "full_page": {
                    "type": "boolean",
                    "description": "For screenshot: capture full page"
                },
                "path": {
                    "type": "string",
                    "description": "File path for screenshot"
                },
                "ms": {
                    "type": "integer",
                    "description": "Milliseconds to wait"
                },
                "by": {
                    "type": "string",
                    "enum": ["role", "text", "label", "placeholder", "testid"],
                    "description": "For find: semantic locator type"
                },
                "find_action": {
                    "type": "string",
                    "enum": ["click", "fill", "text", "hover", "check"],
                    "description": "For find: action to perform on found element"
                },
                "fill_value": {
                    "type": "string",
                    "description": "For find with fill action: value to fill"
                }
            },
            "required": ["action"]
        })
    }

    async fn execute(&self, args: Value) -> anyhow::Result<ToolResult> {
        // Security checks
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

        // Check if agent-browser is available
        if !Self::is_available().await {
            return Ok(ToolResult {
                success: false,
                output: String::new(),
                error: Some(
                    "agent-browser CLI not found. Install with: npm install -g agent-browser"
                        .into(),
                ),
            });
        }

        // Parse action from args
        let action_str = args
            .get("action")
            .and_then(|v| v.as_str())
            .ok_or_else(|| anyhow::anyhow!("Missing 'action' parameter"))?;

        let action = match action_str {
            "open" => {
                let url = args
                    .get("url")
                    .and_then(|v| v.as_str())
                    .ok_or_else(|| anyhow::anyhow!("Missing 'url' for open action"))?;
                BrowserAction::Open { url: url.into() }
            }
            "snapshot" => BrowserAction::Snapshot {
                interactive_only: args
                    .get("interactive_only")
                    .and_then(serde_json::Value::as_bool)
                    .unwrap_or(true), // Default to interactive for AI
                compact: args
                    .get("compact")
                    .and_then(serde_json::Value::as_bool)
                    .unwrap_or(true),
                depth: args
                    .get("depth")
                    .and_then(serde_json::Value::as_u64)
                    .map(|d| u32::try_from(d).unwrap_or(u32::MAX)),
            },
            "click" => {
                let selector = args
                    .get("selector")
                    .and_then(|v| v.as_str())
                    .ok_or_else(|| anyhow::anyhow!("Missing 'selector' for click"))?;
                BrowserAction::Click {
                    selector: selector.into(),
                }
            }
            "fill" => {
                let selector = args
                    .get("selector")
                    .and_then(|v| v.as_str())
                    .ok_or_else(|| anyhow::anyhow!("Missing 'selector' for fill"))?;
                let value = args
                    .get("value")
                    .and_then(|v| v.as_str())
                    .ok_or_else(|| anyhow::anyhow!("Missing 'value' for fill"))?;
                BrowserAction::Fill {
                    selector: selector.into(),
                    value: value.into(),
                }
            }
            "type" => {
                let selector = args
                    .get("selector")
                    .and_then(|v| v.as_str())
                    .ok_or_else(|| anyhow::anyhow!("Missing 'selector' for type"))?;
                let text = args
                    .get("text")
                    .and_then(|v| v.as_str())
                    .ok_or_else(|| anyhow::anyhow!("Missing 'text' for type"))?;
                BrowserAction::Type {
                    selector: selector.into(),
                    text: text.into(),
                }
            }
            "get_text" => {
                let selector = args
                    .get("selector")
                    .and_then(|v| v.as_str())
                    .ok_or_else(|| anyhow::anyhow!("Missing 'selector' for get_text"))?;
                BrowserAction::GetText {
                    selector: selector.into(),
                }
            }
            "get_title" => BrowserAction::GetTitle,
            "get_url" => BrowserAction::GetUrl,
            "screenshot" => BrowserAction::Screenshot {
                path: args.get("path").and_then(|v| v.as_str()).map(String::from),
                full_page: args
                    .get("full_page")
                    .and_then(serde_json::Value::as_bool)
                    .unwrap_or(false),
            },
            "wait" => BrowserAction::Wait {
                selector: args
                    .get("selector")
                    .and_then(|v| v.as_str())
                    .map(String::from),
                ms: args.get("ms").and_then(serde_json::Value::as_u64),
                text: args.get("text").and_then(|v| v.as_str()).map(String::from),
            },
            "press" => {
                let key = args
                    .get("key")
                    .and_then(|v| v.as_str())
                    .ok_or_else(|| anyhow::anyhow!("Missing 'key' for press"))?;
                BrowserAction::Press { key: key.into() }
            }
            "hover" => {
                let selector = args
                    .get("selector")
                    .and_then(|v| v.as_str())
                    .ok_or_else(|| anyhow::anyhow!("Missing 'selector' for hover"))?;
                BrowserAction::Hover {
                    selector: selector.into(),
                }
            }
            "scroll" => {
                let direction = args
                    .get("direction")
                    .and_then(|v| v.as_str())
                    .ok_or_else(|| anyhow::anyhow!("Missing 'direction' for scroll"))?;
                BrowserAction::Scroll {
                    direction: direction.into(),
                    pixels: args
                        .get("pixels")
                        .and_then(serde_json::Value::as_u64)
                        .map(|p| u32::try_from(p).unwrap_or(u32::MAX)),
                }
            }
            "is_visible" => {
                let selector = args
                    .get("selector")
                    .and_then(|v| v.as_str())
                    .ok_or_else(|| anyhow::anyhow!("Missing 'selector' for is_visible"))?;
                BrowserAction::IsVisible {
                    selector: selector.into(),
                }
            }
            "close" => BrowserAction::Close,
            "find" => {
                let by = args
                    .get("by")
                    .and_then(|v| v.as_str())
                    .ok_or_else(|| anyhow::anyhow!("Missing 'by' for find"))?;
                let value = args
                    .get("value")
                    .and_then(|v| v.as_str())
                    .ok_or_else(|| anyhow::anyhow!("Missing 'value' for find"))?;
                let action = args
                    .get("find_action")
                    .and_then(|v| v.as_str())
                    .ok_or_else(|| anyhow::anyhow!("Missing 'find_action' for find"))?;
                BrowserAction::Find {
                    by: by.into(),
                    value: value.into(),
                    action: action.into(),
                    fill_value: args
                        .get("fill_value")
                        .and_then(|v| v.as_str())
                        .map(String::from),
                }
            }
            _ => {
                return Ok(ToolResult {
                    success: false,
                    output: String::new(),
                    error: Some(format!("Unknown action: {action_str}")),
                });
            }
        };

        self.execute_action(action).await
    }
}

// ── Helper functions ─────────────────────────────────────────────

fn normalize_domains(domains: Vec<String>) -> Vec<String> {
    domains
        .into_iter()
        .map(|d| d.trim().to_lowercase())
        .filter(|d| !d.is_empty())
        .collect()
}

fn extract_host(url_str: &str) -> anyhow::Result<String> {
    // Simple host extraction without url crate
    let url = url_str.trim();
    let without_scheme = url
        .strip_prefix("https://")
        .or_else(|| url.strip_prefix("http://"))
        .or_else(|| url.strip_prefix("file://"))
        .unwrap_or(url);

    // Extract host — handle bracketed IPv6 addresses like [::1]:8080
    let authority = without_scheme.split('/').next().unwrap_or(without_scheme);

    let host = if authority.starts_with('[') {
        // IPv6: take everything up to and including the closing ']'
        authority.find(']').map_or(authority, |i| &authority[..=i])
    } else {
        // IPv4 or hostname: take everything before the port separator
        authority.split(':').next().unwrap_or(authority)
    };

    if host.is_empty() {
        anyhow::bail!("Invalid URL: no host");
    }

    Ok(host.to_lowercase())
}

fn is_private_host(host: &str) -> bool {
    // Strip brackets from IPv6 addresses like [::1]
    let bare = host
        .strip_prefix('[')
        .and_then(|h| h.strip_suffix(']'))
        .unwrap_or(host);

    if bare == "localhost" {
        return true;
    }

    // Parse as IP address to catch all representations (decimal, hex, octal, mapped)
    if let Ok(ip) = bare.parse::<std::net::IpAddr>() {
        return match ip {
            std::net::IpAddr::V4(v4) => {
                v4.is_loopback()
                    || v4.is_private()
                    || v4.is_link_local()
                    || v4.is_unspecified()
                    || v4.is_broadcast()
            }
            std::net::IpAddr::V6(v6) => {
                let segs = v6.segments();
                v6.is_loopback()
                    || v6.is_unspecified()
                    // Unique-local (fc00::/7) — IPv6 equivalent of RFC 1918
                    || (segs[0] & 0xfe00) == 0xfc00
                    // Link-local (fe80::/10)
                    || (segs[0] & 0xffc0) == 0xfe80
                    // IPv4-mapped addresses (::ffff:127.0.0.1)
                    || v6.to_ipv4_mapped().is_some_and(|v4| {
                        v4.is_loopback()
                            || v4.is_private()
                            || v4.is_link_local()
                            || v4.is_unspecified()
                            || v4.is_broadcast()
                    })
            }
        };
    }

    // Fallback string patterns for hostnames that look like IPs but don't parse
    // (e.g., partial addresses used in DNS names).
    let string_patterns = [
        "127.", "10.", "192.168.", "0.0.0.0", "172.16.", "172.17.", "172.18.", "172.19.",
        "172.20.", "172.21.", "172.22.", "172.23.", "172.24.", "172.25.", "172.26.", "172.27.",
        "172.28.", "172.29.", "172.30.", "172.31.",
    ];

    string_patterns.iter().any(|p| bare.starts_with(p))
}

fn host_matches_allowlist(host: &str, allowed: &[String]) -> bool {
    allowed.iter().any(|pattern| {
        if pattern == "*" {
            return true;
        }
        if pattern.starts_with("*.") {
            // Wildcard subdomain match
            let suffix = &pattern[1..]; // ".example.com"
            host.ends_with(suffix) || host == &pattern[2..]
        } else {
            // Exact match or subdomain
            host == pattern || host.ends_with(&format!(".{pattern}"))
        }
    })
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn normalize_domains_works() {
        let domains = vec![
            "  Example.COM  ".into(),
            "docs.example.com".into(),
            String::new(),
        ];
        let normalized = normalize_domains(domains);
        assert_eq!(normalized, vec!["example.com", "docs.example.com"]);
    }

    #[test]
    fn extract_host_works() {
        assert_eq!(
            extract_host("https://example.com/path").unwrap(),
            "example.com"
        );
        assert_eq!(
            extract_host("https://Sub.Example.COM:8080/").unwrap(),
            "sub.example.com"
        );
    }

    #[test]
    fn extract_host_handles_ipv6() {
        // IPv6 with brackets (required for URLs with ports)
        assert_eq!(extract_host("https://[::1]/path").unwrap(), "[::1]");
        // IPv6 with brackets and port
        assert_eq!(
            extract_host("https://[2001:db8::1]:8080/path").unwrap(),
            "[2001:db8::1]"
        );
        // IPv6 with brackets, trailing slash
        assert_eq!(extract_host("https://[fe80::1]/").unwrap(), "[fe80::1]");
    }

    #[test]
    fn is_private_host_detects_local() {
        assert!(is_private_host("localhost"));
        assert!(is_private_host("127.0.0.1"));
        assert!(is_private_host("192.168.1.1"));
        assert!(is_private_host("10.0.0.1"));
        assert!(!is_private_host("example.com"));
        assert!(!is_private_host("google.com"));
    }

    #[test]
    fn is_private_host_catches_ipv6() {
        assert!(is_private_host("::1"));
        assert!(is_private_host("[::1]"));
        assert!(is_private_host("0.0.0.0"));
    }

    #[test]
    fn is_private_host_catches_mapped_ipv4() {
        // IPv4-mapped IPv6 addresses
        assert!(is_private_host("::ffff:127.0.0.1"));
        assert!(is_private_host("::ffff:10.0.0.1"));
        assert!(is_private_host("::ffff:192.168.1.1"));
    }

    #[test]
    fn is_private_host_catches_ipv6_private_ranges() {
        // Unique-local (fc00::/7)
        assert!(is_private_host("fd00::1"));
        assert!(is_private_host("fc00::1"));
        // Link-local (fe80::/10)
        assert!(is_private_host("fe80::1"));
        // Public IPv6 should pass
        assert!(!is_private_host("2001:db8::1"));
    }

    #[test]
    fn validate_url_blocks_ipv6_ssrf() {
        let security = Arc::new(SecurityPolicy::default());
        let tool = BrowserTool::new(security, vec!["*".into()], None);
        assert!(tool.validate_url("https://[::1]/").is_err());
        assert!(tool.validate_url("https://[::ffff:127.0.0.1]/").is_err());
        assert!(tool
            .validate_url("https://[::ffff:10.0.0.1]:8080/")
            .is_err());
    }

    #[test]
    fn host_matches_allowlist_exact() {
        let allowed = vec!["example.com".into()];
        assert!(host_matches_allowlist("example.com", &allowed));
        assert!(host_matches_allowlist("sub.example.com", &allowed));
        assert!(!host_matches_allowlist("notexample.com", &allowed));
    }

    #[test]
    fn host_matches_allowlist_wildcard() {
        let allowed = vec!["*.example.com".into()];
        assert!(host_matches_allowlist("sub.example.com", &allowed));
        assert!(host_matches_allowlist("example.com", &allowed));
        assert!(!host_matches_allowlist("other.com", &allowed));
    }

    #[test]
    fn host_matches_allowlist_star() {
        let allowed = vec!["*".into()];
        assert!(host_matches_allowlist("anything.com", &allowed));
        assert!(host_matches_allowlist("example.org", &allowed));
    }

    #[test]
    fn browser_tool_name() {
        let security = Arc::new(SecurityPolicy::default());
        let tool = BrowserTool::new(security, vec!["example.com".into()], None);
        assert_eq!(tool.name(), "browser");
    }

    #[test]
    fn browser_tool_validates_url() {
        let security = Arc::new(SecurityPolicy::default());
        let tool = BrowserTool::new(security, vec!["example.com".into()], None);

        // Valid
        assert!(tool.validate_url("https://example.com").is_ok());
        assert!(tool.validate_url("https://sub.example.com/path").is_ok());

        // Invalid - not in allowlist
        assert!(tool.validate_url("https://other.com").is_err());

        // Invalid - private host
        assert!(tool.validate_url("https://localhost").is_err());
        assert!(tool.validate_url("https://127.0.0.1").is_err());

        // Invalid - not https
        assert!(tool.validate_url("ftp://example.com").is_err());

        // File URLs allowed
        assert!(tool.validate_url("file:///tmp/test.html").is_ok());
    }

    #[test]
    fn browser_tool_empty_allowlist_blocks() {
        let security = Arc::new(SecurityPolicy::default());
        let tool = BrowserTool::new(security, vec![], None);
        assert!(tool.validate_url("https://example.com").is_err());
    }
}
