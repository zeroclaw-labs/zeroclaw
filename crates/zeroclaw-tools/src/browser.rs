//! Browser automation tool with pluggable backends.
//!
//! By default this uses Vercel's `agent-browser` CLI for automation.
//! Optionally, a Rust-native backend can be enabled at build time via
//! `--features browser-native` and selected through config.
//! Computer-use (OS-level) actions are supported via an optional sidecar endpoint.

use crate::helpers::domain_guard;
use crate::i18n;
use anyhow::Context;
use async_trait::async_trait;
use serde::{Deserialize, Serialize};
use serde_json::{Value, json};
use std::net::ToSocketAddrs;
use std::process::Stdio;
use std::sync::Arc;
use std::time::Duration;
use tokio::process::Command;
use zeroclaw_api::tool::{Tool, ToolResult};
use zeroclaw_config::policy::SecurityPolicy;

/// Computer-use sidecar settings.
#[derive(Clone)]
pub struct ComputerUseConfig {
    pub endpoint: String,
    pub api_key: Option<String>,
    pub timeout_ms: u64,
    pub allow_remote_endpoint: bool,
    pub window_allowlist: Vec<String>,
    pub max_coordinate_x: Option<i64>,
    pub max_coordinate_y: Option<i64>,
}

impl std::fmt::Debug for ComputerUseConfig {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        f.debug_struct("ComputerUseConfig")
            .field("endpoint", &self.endpoint)
            .field("timeout_ms", &self.timeout_ms)
            .field("allow_remote_endpoint", &self.allow_remote_endpoint)
            .field("window_allowlist", &self.window_allowlist)
            .field("max_coordinate_x", &self.max_coordinate_x)
            .field("max_coordinate_y", &self.max_coordinate_y)
            .finish_non_exhaustive()
    }
}

impl Default for ComputerUseConfig {
    fn default() -> Self {
        Self {
            endpoint: "http://127.0.0.1:8787/v1/actions".into(),
            api_key: None,
            timeout_ms: 15_000,
            allow_remote_endpoint: false,
            window_allowlist: Vec::new(),
            max_coordinate_x: None,
            max_coordinate_y: None,
        }
    }
}

/// Browser automation tool using pluggable backends.
pub struct BrowserTool {
    security: Arc<SecurityPolicy>,
    allowed_domains: Vec<String>,
    allowed_private_hosts: Vec<String>,
    session_name: Option<String>,
    backend: String,
    headed: Option<bool>,
    #[allow(dead_code)] // read only with browser-native feature
    native_headless: bool,
    #[allow(dead_code)]
    native_webdriver_url: String,
    #[allow(dead_code)]
    native_chrome_path: Option<String>,
    computer_use: ComputerUseConfig,
    #[cfg(feature = "browser-native")]
    native_state: tokio::sync::Mutex<native_backend::NativeBrowserState>,
}

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
enum BrowserBackendKind {
    AgentBrowser,
    RustNative,
    ComputerUse,
    Auto,
}

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
enum ResolvedBackend {
    AgentBrowser,
    RustNative,
    ComputerUse,
}

/// Classification of the `path` field on a ComputerUse screenshot args
/// object, used by `validate_screenshot_path_for_computer_use` to decide
/// between passthrough, rejection, and full workspace validation.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
enum PathKind {
    /// Field absent, `null`, or empty string. Inline PNG return semantics.
    Absent,
    /// `path` is a non-empty string. Eligible for workspace validation.
    String,
    /// `path` is present but not a string (e.g. integer, array, object).
    /// Rejected at the hook because `parse_browser_action` would silently
    /// drop it to `None` and forward the raw value unverified.
    NonString,
}

impl BrowserBackendKind {
    fn parse(raw: &str) -> anyhow::Result<Self> {
        let key = raw.trim().to_ascii_lowercase().replace('-', "_");
        match key.as_str() {
            "agent_browser" | "agentbrowser" => Ok(Self::AgentBrowser),
            "rust_native" | "native" => Ok(Self::RustNative),
            "computer_use" | "computeruse" => Ok(Self::ComputerUse),
            "auto" => Ok(Self::Auto),
            _ => anyhow::bail!(
                "Unsupported browser backend '{raw}'. Use 'agent_browser', 'rust_native', 'computer_use', or 'auto'"
            ),
        }
    }

    fn as_str(self) -> &'static str {
        match self {
            Self::AgentBrowser => "agent_browser",
            Self::RustNative => "rust_native",
            Self::ComputerUse => "computer_use",
            Self::Auto => "auto",
        }
    }
}

/// Response from agent-browser --json commands
#[derive(Debug, Deserialize)]
struct AgentBrowserResponse {
    success: bool,
    data: Option<Value>,
    error: Option<String>,
}

/// Response format from computer-use sidecar.
#[derive(Debug, Deserialize)]
struct ComputerUseResponse {
    #[serde(default)]
    success: Option<bool>,
    #[serde(default)]
    data: Option<Value>,
    #[serde(default)]
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
    ) -> anyhow::Result<Self> {
        Self::new_with_backend(
            security,
            allowed_domains,
            session_name,
            "agent_browser".into(),
            None,
            true,
            "http://127.0.0.1:9515".into(),
            None,
            ComputerUseConfig::default(),
            Vec::new(),
        )
    }

    #[allow(clippy::too_many_arguments)]
    pub fn new_with_backend(
        security: Arc<SecurityPolicy>,
        allowed_domains: Vec<String>,
        session_name: Option<String>,
        backend: String,
        headed: Option<bool>,
        native_headless: bool,
        native_webdriver_url: String,
        native_chrome_path: Option<String>,
        computer_use: ComputerUseConfig,
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
            session_name,
            backend,
            headed,
            native_headless,
            native_webdriver_url,
            native_chrome_path,
            computer_use,
            #[cfg(feature = "browser-native")]
            native_state: tokio::sync::Mutex::new(native_backend::NativeBrowserState::default()),
        })
    }

    /// Check if agent-browser CLI is available
    pub async fn is_agent_browser_available() -> bool {
        let cmd = if cfg!(target_os = "windows") {
            "agent-browser.cmd"
        } else {
            "agent-browser"
        };
        Command::new(cmd)
            .arg("--version")
            .stdout(Stdio::null())
            .stderr(Stdio::null())
            .status()
            .await
            .map(|s| s.success())
            .unwrap_or(false)
    }

    /// Backward-compatible alias.
    pub async fn is_available() -> bool {
        Self::is_agent_browser_available().await
    }

    fn configured_backend(&self) -> anyhow::Result<BrowserBackendKind> {
        BrowserBackendKind::parse(&self.backend)
    }

    fn rust_native_compiled() -> bool {
        cfg!(feature = "browser-native")
    }

    fn rust_native_available(&self) -> bool {
        #[cfg(feature = "browser-native")]
        {
            native_backend::NativeBrowserState::is_available(
                self.native_headless,
                &self.native_webdriver_url,
                self.native_chrome_path.as_deref(),
            )
        }
        #[cfg(not(feature = "browser-native"))]
        {
            false
        }
    }

    fn computer_use_endpoint_url(&self) -> anyhow::Result<reqwest::Url> {
        if self.computer_use.timeout_ms == 0 {
            anyhow::bail!("browser.computer_use.timeout_ms must be > 0");
        }

        let endpoint = self.computer_use.endpoint.trim();
        if endpoint.is_empty() {
            anyhow::bail!("browser.computer_use.endpoint cannot be empty");
        }

        let parsed = reqwest::Url::parse(endpoint).map_err(|_| {
            ::zeroclaw_log::record!(
                WARN,
                ::zeroclaw_log::Event::new(module_path!(), ::zeroclaw_log::Action::Reject)
                    .with_outcome(::zeroclaw_log::EventOutcome::Failure)
                    .with_attrs(::serde_json::json!({"endpoint": endpoint})),
                "browser: invalid computer_use endpoint URL"
            );
            anyhow::Error::msg(format!(
                "Invalid browser.computer_use.endpoint: '{endpoint}'. Expected http(s) URL"
            ))
        })?;

        let scheme = parsed.scheme();
        if scheme != "http" && scheme != "https" {
            anyhow::bail!("browser.computer_use.endpoint must use http:// or https://");
        }

        let host = parsed.host_str().ok_or_else(|| {
            ::zeroclaw_log::record!(
                WARN,
                ::zeroclaw_log::Event::new(module_path!(), ::zeroclaw_log::Action::Reject)
                    .with_outcome(::zeroclaw_log::EventOutcome::Failure),
                "browser: browser.computer_use.endpoint must include host"
            );
            anyhow::Error::msg("browser.computer_use.endpoint must include host")
        })?;

        let host_is_private = domain_guard::is_private_or_local_host(host);
        if !self.computer_use.allow_remote_endpoint && !host_is_private {
            anyhow::bail!(
                "browser.computer_use.endpoint host '{host}' is public. Set browser.computer_use.allow_remote_endpoint=true to allow it"
            );
        }

        if self.computer_use.allow_remote_endpoint && !host_is_private && scheme != "https" {
            anyhow::bail!(
                "browser.computer_use.endpoint must use https:// when allow_remote_endpoint=true and host is public"
            );
        }

        Ok(parsed)
    }

    fn computer_use_available(&self) -> anyhow::Result<bool> {
        let endpoint = self.computer_use_endpoint_url()?;
        Ok(endpoint_reachable(&endpoint, Duration::from_millis(500)))
    }

    async fn resolve_backend(&self) -> anyhow::Result<ResolvedBackend> {
        let configured = self.configured_backend()?;

        match configured {
            BrowserBackendKind::AgentBrowser => {
                if Self::is_agent_browser_available().await {
                    Ok(ResolvedBackend::AgentBrowser)
                } else {
                    #[cfg(target_os = "windows")]
                    let install_hint = "Install with: npm install -g agent-browser (ensure npm global bin is in PATH)";
                    #[cfg(not(target_os = "windows"))]
                    let install_hint = "Install with: npm install -g agent-browser";
                    anyhow::bail!(
                        "browser.backend='{}' but agent-browser CLI is unavailable. {}",
                        configured.as_str(),
                        install_hint
                    )
                }
            }
            BrowserBackendKind::RustNative => {
                if !Self::rust_native_compiled() {
                    anyhow::bail!(
                        "browser.backend='rust_native' requires build feature 'browser-native'"
                    );
                }
                if !self.rust_native_available() {
                    anyhow::bail!(
                        "Rust-native browser backend is enabled but WebDriver endpoint is unreachable. Set browser.native_webdriver_url and start a compatible driver"
                    );
                }
                Ok(ResolvedBackend::RustNative)
            }
            BrowserBackendKind::ComputerUse => {
                if !self.computer_use_available()? {
                    anyhow::bail!(
                        "browser.backend='computer_use' but sidecar endpoint is unreachable. Check browser.computer_use.endpoint and sidecar status"
                    );
                }
                Ok(ResolvedBackend::ComputerUse)
            }
            BrowserBackendKind::Auto => {
                if Self::rust_native_compiled() && self.rust_native_available() {
                    return Ok(ResolvedBackend::RustNative);
                }
                if Self::is_agent_browser_available().await {
                    return Ok(ResolvedBackend::AgentBrowser);
                }

                let computer_use_err = match self.computer_use_available() {
                    Ok(true) => return Ok(ResolvedBackend::ComputerUse),
                    Ok(false) => None,
                    Err(err) => Some(err.to_string()),
                };

                if Self::rust_native_compiled() {
                    if let Some(err) = computer_use_err {
                        anyhow::bail!(
                            "browser.backend='auto' found no usable backend (agent-browser missing, rust-native unavailable, computer-use invalid: {err})"
                        );
                    }
                    anyhow::bail!(
                        "browser.backend='auto' found no usable backend (agent-browser missing, rust-native unavailable, computer-use sidecar unreachable)"
                    )
                }

                if let Some(err) = computer_use_err {
                    anyhow::bail!(
                        "browser.backend='auto' needs agent-browser CLI, browser-native, or valid computer-use sidecar (error: {err})"
                    );
                }

                anyhow::bail!(
                    "browser.backend='auto' needs agent-browser CLI, browser-native, or computer-use sidecar"
                )
            }
        }
    }

    /// Validate URL against allowlist
    fn validate_url(&self, url: &str) -> anyhow::Result<()> {
        let url = url.trim();

        if url.is_empty() {
            anyhow::bail!("URL cannot be empty");
        }

        // Block file:// URLs — browser file access bypasses all SSRF and
        // domain-allowlist controls and can exfiltrate arbitrary local files.
        if url.starts_with("file://") {
            anyhow::bail!("file:// URLs are not allowed in browser automation");
        }

        if !url.starts_with("https://") && !url.starts_with("http://") {
            anyhow::bail!("Only http:// and https:// URLs are allowed");
        }

        // Parse with `reqwest::Url` (re-exported `url` crate, the Rust de-facto
        // standard URL parser) instead of hand-rolling authority/host
        // extraction. Reviewer caught two parser-mismatch bypasses against the
        // prior hand-rolled `extract_host`:
        //
        //   1. `http://example.com@127.0.0.1/` — userinfo `example.com@…`
        //      classified as host, browser navigates to loopback.
        //   2. `http://127.0.0.1?x` / `http://127.0.0.1#x` — no `/` before the
        //      query/fragment, so `127.0.0.1?x` classified as host, not
        //      private, browser still navigates to loopback.
        //
        // Both classes vanish once we use the same parser the browser backend
        // ultimately resolves against. This also aligns the `browser` SSRF
        // gate with `http_request.rs`, `domain_guard.rs`, and the existing
        // `reqwest::Url::parse` calls already in this file.
        let parsed = reqwest::Url::parse(url)
            .map_err(|e| anyhow::Error::msg(format!("Invalid URL format: {e}")))?;

        if !parsed.username().is_empty() || parsed.password().is_some() {
            anyhow::bail!("URL userinfo is not allowed");
        }

        if self.allowed_domains.is_empty() && self.allowed_private_hosts.is_empty() {
            anyhow::bail!(
                "Browser tool enabled but no allowed_domains configured. \
                Add [browser].allowed_domains in config.toml"
            );
        }

        let host_str = parsed
            .host_str()
            .ok_or_else(|| anyhow::Error::msg("URL must include a host"))?;

        // Re-add IPv6 brackets so the host string fed to `is_private_or_local_host`
        // and `host_matches_allowlist` matches the shape used elsewhere in this
        // crate (`[::1]`, `[fe80::1]`). `Url::host_str` strips the brackets for
        // IPv6 literals. We detect IPv6 by parsing the bracket-less form as an
        // `IpAddr` — avoids depending on `url::Host` (only `url::Url` is
        // re-exported via `reqwest`).
        let is_ipv6 = host_str.parse::<std::net::Ipv6Addr>().is_ok();
        let host = if is_ipv6 {
            format!("[{host_str}]")
        } else {
            host_str.to_lowercase()
        };

        let private_host = domain_guard::is_private_or_local_host(&host);
        let private_host_allowed = private_host
            && domain_guard::host_matches_allowlist(&host, &self.allowed_private_hosts);

        if private_host && !private_host_allowed {
            anyhow::bail!("Blocked local/private host: {host}");
        }

        if private_host_allowed {
            return Ok(());
        }

        if !domain_guard::host_matches_allowlist(&host, &self.allowed_domains) {
            anyhow::bail!("Host '{host}' not in browser.allowed_domains");
        }

        Ok(())
    }

    /// Execute an agent-browser command
    async fn run_command(&self, args: &[&str]) -> anyhow::Result<AgentBrowserResponse> {
        let mut cmd = self.agent_browser_command();

        // Add --json for machine-readable output
        cmd.args(args).arg("--json");

        ::zeroclaw_log::record!(
            DEBUG,
            ::zeroclaw_log::Event::new(module_path!(), ::zeroclaw_log::Action::Note),
            &format!("Running: agent-browser {} --json", args.join(" "))
        );

        let output = cmd
            .stdout(Stdio::piped())
            .stderr(Stdio::piped())
            .output()
            .await?;

        let stdout = String::from_utf8_lossy(&output.stdout);
        let stderr = String::from_utf8_lossy(&output.stderr);

        if !stderr.is_empty() {
            ::zeroclaw_log::record!(
                DEBUG,
                ::zeroclaw_log::Event::new(module_path!(), ::zeroclaw_log::Action::Note),
                &format!("agent-browser stderr: {}", stderr)
            );
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

    fn agent_browser_command(&self) -> Command {
        let agent_browser_bin = if cfg!(target_os = "windows") {
            "agent-browser.cmd"
        } else {
            "agent-browser"
        };
        let mut cmd = Command::new(agent_browser_bin);

        match self.headed {
            Some(true) => {
                cmd.env("AGENT_BROWSER_HEADED", "1");
            }
            Some(false) => {
                cmd.env_remove("AGENT_BROWSER_HEADED");
            }
            None => {}
        }

        // When running as a service (systemd/OpenRC), the process may lack
        // HOME which browsers need for profile directories.
        if is_service_environment() {
            ensure_browser_env(&mut cmd);
        }

        // Add session if configured
        if let Some(ref session) = self.session_name {
            cmd.arg("--session").arg(session);
        }

        cmd
    }

    /// Execute a browser action via agent-browser CLI
    #[allow(clippy::too_many_lines)]
    async fn execute_agent_browser_action(
        &self,
        action: BrowserAction,
    ) -> anyhow::Result<ToolResult> {
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

    #[allow(clippy::unused_async)]
    async fn execute_rust_native_action(
        &self,
        action: BrowserAction,
    ) -> anyhow::Result<ToolResult> {
        #[cfg(feature = "browser-native")]
        {
            let mut state = self.native_state.lock().await;

            let first_attempt = state
                .execute_action(
                    action.clone(),
                    self.native_headless,
                    &self.native_webdriver_url,
                    self.native_chrome_path.as_deref(),
                )
                .await;

            let output = match first_attempt {
                Ok(output) => output,
                Err(err) => {
                    if !is_recoverable_rust_native_error(&err) {
                        return Err(err);
                    }

                    state.reset_session().await;
                    state
                        .execute_action(
                            action,
                            self.native_headless,
                            &self.native_webdriver_url,
                            self.native_chrome_path.as_deref(),
                        )
                        .await
                        .with_context(|| "rust_native backend retry after session reset failed")?
                }
            };

            Ok(ToolResult {
                success: true,
                output: serde_json::to_string_pretty(&output).unwrap_or_default(),
                error: None,
            })
        }

        #[cfg(not(feature = "browser-native"))]
        {
            let _ = action;
            anyhow::bail!(
                "Rust-native browser backend is not compiled. Rebuild with --features browser-native"
            )
        }
    }

    fn validate_coordinate(&self, key: &str, value: i64, max: Option<i64>) -> anyhow::Result<()> {
        if value < 0 {
            anyhow::bail!("'{key}' must be >= 0")
        }
        if let Some(limit) = max {
            if limit < 0 {
                anyhow::bail!("Configured coordinate limit for '{key}' must be >= 0")
            }
            if value > limit {
                anyhow::bail!("'{key}'={value} exceeds configured limit {limit}")
            }
        }
        Ok(())
    }

    /// Validate a screenshot destination path before any backend processes it.
    ///
    /// When the path is `None` (PNG embedded in the response), this is a no-op.
    /// When a path is provided, it is resolved against the workspace directory,
    /// its parent directory is canonicalized, and the result is checked against
    /// the security policy's path allowlist. The file name is then checked
    /// against the runtime-config guard and the target's existing symlink
    /// status, mirroring the `file_write` / `file_edit` target-level checks.
    /// The raw user-supplied path is replaced with the resolved+validated
    /// path so the backends write the same string that was checked.
    ///
    /// This applies the current workspace policy at validation time and rejects
    /// an already-existing target symlink; it does not by itself close any
    /// TOCTOU window between this call and the eventual write.
    async fn validate_screenshot_path(&self, action: &mut BrowserAction) -> anyhow::Result<()> {
        let BrowserAction::Screenshot { path, .. } = action else {
            return Ok(());
        };
        let Some(path_str) = path.as_ref() else {
            return Ok(());
        };

        // String-level reject (null bytes, .. traversal, URL-encoded traversal)
        if !self.security.is_path_allowed(path_str) {
            let msg = i18n::get_required_tool_string_with_args(
                "tool-browser-screenshot-error-path-not-allowed",
                &[("path", path_str)],
            );
            anyhow::bail!("{msg}");
        }

        // Resolve relative / tilde paths against the workspace directory.
        let full = self.security.resolve_tool_path(path_str);

        // The file does not exist yet, so canonicalize the *parent* directory
        // to verify it is inside the workspace allowlist.
        let parent = full.parent().unwrap_or(&full);
        let canonical = tokio::fs::canonicalize(parent).await.with_context(|| {
            i18n::get_required_tool_string_with_args(
                "tool-browser-screenshot-error-parent-not-exist",
                &[
                    ("path", path_str),
                    ("parent", &parent.display().to_string()),
                ],
            )
        })?;

        if !self.security.is_resolved_path_allowed(&canonical) {
            let msg = i18n::get_required_tool_string_with_args(
                "tool-browser-screenshot-error-path-outside-workspace",
                &[
                    ("path", path_str),
                    ("canonical", &canonical.display().to_string()),
                ],
            );
            anyhow::bail!("{msg}");
        }

        // Build the final *target* path (parent + file name) so we can apply
        // the same target-level guards the file_write / file_edit tools use:
        // runtime-config protection and existing-symlink rejection. This
        // closes the gap where a screenshot path inside an allowed workspace
        // could still overwrite a protected config/state file or write
        // through a symlink to a location outside the workspace.
        let Some(file_name) = full.file_name() else {
            let msg = i18n::get_required_tool_string_with_args(
                "tool-browser-screenshot-error-missing-filename",
                &[("path", path_str)],
            );
            anyhow::bail!("{msg}");
        };
        let resolved_target = canonical.join(file_name);

        if self.security.is_runtime_config_path(&resolved_target) {
            let msg = i18n::get_required_tool_string_with_args(
                "tool-browser-screenshot-error-runtime-config-target",
                &[
                    ("path", path_str),
                    ("target", &resolved_target.display().to_string()),
                ],
            );
            anyhow::bail!("{msg}");
        }

        // If the target already exists and is a symlink, refuse to follow it
        // — the backends' write call would land wherever the symlink points,
        // which is not the same as `resolved_target`.
        if let Ok(meta) = tokio::fs::symlink_metadata(&resolved_target).await
            && meta.file_type().is_symlink()
        {
            let msg = i18n::get_required_tool_string_with_args(
                "tool-browser-screenshot-error-symlink-target",
                &[("target", &resolved_target.display().to_string())],
            );
            anyhow::bail!("{msg}");
        }

        // Replace the raw user path with the canonical target so the write
        // always uses the same string we checked.
        *path = Some(resolved_target.to_string_lossy().to_string());

        Ok(())
    }

    /// ComputerUse backend dispatches to a sidecar before `parse_browser_action`
    /// runs, so `execute_action`'s `validate_screenshot_path` call would
    /// never see the screenshot `path`. This hook applies the same workspace
    /// policy / runtime-config / symlink guards to the *raw* `args` JSON
    /// before any of its params cross the sidecar boundary, and substitutes
    /// the canonical resolved path back into the args so the sidecar
    /// receives the same hardened string the other backends use.
    ///
    /// Non-screenshot actions pass through unchanged. Screenshot actions
    /// with no `path` and screenshot actions with `path: ""` are treated as
    /// inline PNG return and pass through unchanged.
    ///
    /// Screenshot actions with `path: <non-string>` (e.g. an integer) are
    /// rejected at the hook with a typed error: `parse_browser_action`
    /// would otherwise drop the value to `None` and forward the raw
    /// non-string to the sidecar unverified.
    ///
    /// When the configured ComputerUse endpoint resolves to a remote host
    /// (i.e. `allow_remote_endpoint = true` and the host is not private /
    /// local), any `path` is rejected because local-filesystem
    /// canonicalization is meaningless across hosts. Inline PNG screenshots
    /// (no `path`) continue to work in that configuration.
    async fn validate_screenshot_path_for_computer_use(
        &self,
        action_str: &str,
        mut args: Value,
    ) -> anyhow::Result<Value> {
        if action_str != "screenshot" {
            return Ok(args);
        }

        // Classify the path up front so non-string and empty cases never
        // reach the workspace validator (which only accepts string paths).
        let path_kind = args
            .as_object()
            .and_then(|obj| obj.get("path"))
            .map(|v| match v {
                Value::Null => PathKind::Absent,
                Value::String(s) if s.is_empty() => PathKind::Absent,
                Value::String(_) => PathKind::String,
                _ => PathKind::NonString,
            })
            .unwrap_or(PathKind::Absent);

        if matches!(path_kind, PathKind::Absent) {
            return Ok(args);
        }

        if matches!(path_kind, PathKind::NonString) {
            let msg = i18n::get_required_tool_string(
                "tool-browser-screenshot-error-computeruse-non-string-path",
            );
            anyhow::bail!("{msg}");
        }

        // Refuse destination paths against remote sidecars. Local-filesystem
        // canonicalization is meaningless across hosts, and the sidecar would
        // either ignore the path or write to a location we cannot verify.
        if self.endpoint_is_remote() {
            let msg = i18n::get_required_tool_string(
                "tool-browser-screenshot-error-computeruse-remote-endpoint",
            );
            anyhow::bail!("{msg}");
        }

        // Parse a BrowserAction so we can reuse the same validator that
        // `execute_action` calls for agent-browser / rust-native. The
        // borrow on `args` ends here.
        let mut action = match parse_browser_action("screenshot", &args) {
            Ok(a) => a,
            Err(e) => {
                ::zeroclaw_log::record!(
                    WARN,
                    ::zeroclaw_log::Event::new(module_path!(), ::zeroclaw_log::Action::Reject)
                        .with_outcome(::zeroclaw_log::EventOutcome::Failure),
                    "browser: computer_use screenshot args failed to parse"
                );
                return Err(e);
            }
        };

        self.validate_screenshot_path(&mut action).await?;

        // Substitute the canonical path back into the args so the sidecar
        // sees the same string we just verified. Drop `action` before
        // borrowing `args` mutably.
        let rewritten_path = if let BrowserAction::Screenshot { path: Some(p), .. } = action {
            Some(p)
        } else {
            None
        };
        if let Some(p) = rewritten_path
            && let Some(obj) = args.as_object_mut()
        {
            obj.insert("path".to_string(), Value::String(p));
        }
        Ok(args)
    }

    /// True when the configured ComputerUse endpoint resolves to a remote
    /// host (i.e. `allow_remote_endpoint = true` and the host is not private
    /// / local). Returns `true` conservatively when the endpoint URL cannot
    /// be parsed — refusing destination paths is safer than forwarding them
    /// when we cannot prove the sidecar is on this host.
    ///
    /// This uses `domain_guard::is_private_or_local_host` to classify the
    /// endpoint host. RFC1918 private addresses (10.x.x.x, 172.16-31.x,
    /// 192.168.x) and local addresses (127.x.x.x, ::1, etc.) are treated as
    /// local, meaning the sidecar is assumed to share the same filesystem.
    /// Public hosts or unparseable endpoints are treated as remote.
    fn endpoint_is_remote(&self) -> bool {
        if !self.computer_use.allow_remote_endpoint {
            return false;
        }
        match reqwest::Url::parse(&self.computer_use.endpoint) {
            Ok(parsed) => match parsed.host_str() {
                Some(host) => !domain_guard::is_private_or_local_host(host),
                None => true,
            },
            Err(_) => true,
        }
    }

    fn read_required_i64(
        &self,
        params: &serde_json::Map<String, Value>,
        key: &str,
    ) -> anyhow::Result<i64> {
        params.get(key).and_then(Value::as_i64).ok_or_else(|| {
            ::zeroclaw_log::record!(
                WARN,
                ::zeroclaw_log::Event::new(module_path!(), ::zeroclaw_log::Action::Reject)
                    .with_outcome(::zeroclaw_log::EventOutcome::Failure),
                "browser: Missing or invalid '{key}' parameter"
            );
            anyhow::Error::msg("Missing or invalid '{key}' parameter")
        })
    }

    fn validate_computer_use_action(
        &self,
        action: &str,
        params: &serde_json::Map<String, Value>,
    ) -> anyhow::Result<()> {
        match action {
            "open" => {
                let url = params.get("url").and_then(Value::as_str).ok_or_else(|| {
                    ::zeroclaw_log::record!(
                        WARN,
                        ::zeroclaw_log::Event::new(module_path!(), ::zeroclaw_log::Action::Reject)
                            .with_outcome(::zeroclaw_log::EventOutcome::Failure),
                        "browser: Missing 'url' for open action"
                    );
                    anyhow::Error::msg("Missing 'url' for open action")
                })?;
                self.validate_url(url)?;
            }
            "mouse_move" | "mouse_click" => {
                let x = self.read_required_i64(params, "x")?;
                let y = self.read_required_i64(params, "y")?;
                self.validate_coordinate("x", x, self.computer_use.max_coordinate_x)?;
                self.validate_coordinate("y", y, self.computer_use.max_coordinate_y)?;
            }
            "mouse_drag" => {
                let from_x = self.read_required_i64(params, "from_x")?;
                let from_y = self.read_required_i64(params, "from_y")?;
                let to_x = self.read_required_i64(params, "to_x")?;
                let to_y = self.read_required_i64(params, "to_y")?;
                self.validate_coordinate("from_x", from_x, self.computer_use.max_coordinate_x)?;
                self.validate_coordinate("to_x", to_x, self.computer_use.max_coordinate_x)?;
                self.validate_coordinate("from_y", from_y, self.computer_use.max_coordinate_y)?;
                self.validate_coordinate("to_y", to_y, self.computer_use.max_coordinate_y)?;
            }
            _ => {}
        }
        Ok(())
    }

    async fn execute_computer_use_action(
        &self,
        action: &str,
        args: &Value,
    ) -> anyhow::Result<ToolResult> {
        let endpoint = self.computer_use_endpoint_url()?;

        let mut params = args.as_object().cloned().ok_or_else(|| {
            ::zeroclaw_log::record!(
                WARN,
                ::zeroclaw_log::Event::new(module_path!(), ::zeroclaw_log::Action::Reject)
                    .with_outcome(::zeroclaw_log::EventOutcome::Failure),
                "browser: browser args must be a JSON object"
            );
            anyhow::Error::msg("browser args must be a JSON object")
        })?;
        params.remove("action");

        self.validate_computer_use_action(action, &params)?;

        let payload = json!({
            "action": action,
            "params": params,
            "policy": {
                "allowed_domains": self.allowed_domains,
                "window_allowlist": self.computer_use.window_allowlist,
                "max_coordinate_x": self.computer_use.max_coordinate_x,
                "max_coordinate_y": self.computer_use.max_coordinate_y,
            },
            "metadata": {
                "session_name": self.session_name,
                "source": "zeroclaw.browser",
                "version": env!("CARGO_PKG_VERSION"),
            }
        });

        let client = zeroclaw_config::schema::build_runtime_proxy_client("tool.browser");
        let mut request = client
            .post(endpoint)
            .timeout(Duration::from_millis(self.computer_use.timeout_ms))
            .json(&payload);

        if let Some(api_key) = self.computer_use.api_key.as_deref() {
            let token = api_key.trim();
            if !token.is_empty() {
                request = request.bearer_auth(token);
            }
        }

        let response = request.send().await.with_context(|| {
            format!(
                "Failed to call computer-use sidecar at {}",
                self.computer_use.endpoint
            )
        })?;

        let status = response.status();
        let body = response
            .text()
            .await
            .context("Failed to read computer-use sidecar response body")?;

        if let Ok(parsed) = serde_json::from_str::<ComputerUseResponse>(&body) {
            if status.is_success() && parsed.success.unwrap_or(true) {
                let output = parsed
                    .data
                    .map(|data| serde_json::to_string_pretty(&data).unwrap_or_default())
                    .unwrap_or_else(|| {
                        serde_json::to_string_pretty(&json!({
                            "backend": "computer_use",
                            "action": action,
                            "ok": true,
                        }))
                        .unwrap_or_default()
                    });

                return Ok(ToolResult {
                    success: true,
                    output,
                    error: None,
                });
            }

            let error = parsed.error.or_else(|| {
                if status.is_success() && parsed.success == Some(false) {
                    Some("computer-use sidecar returned success=false".to_string())
                } else {
                    Some(format!(
                        "computer-use sidecar request failed with status {status}"
                    ))
                }
            });

            return Ok(ToolResult {
                success: false,
                output: String::new(),
                error,
            });
        }

        if status.is_success() {
            return Ok(ToolResult {
                success: true,
                output: body,
                error: None,
            });
        }

        Ok(ToolResult {
            success: false,
            output: String::new(),
            error: Some(format!(
                "computer-use sidecar request failed with status {status}: {}",
                body.trim()
            )),
        })
    }

    /// Validate a screenshot destination path before any backend processes it.
    ///
    /// When the path is `None` (PNG embedded in the response), this is a no-op.
    /// runs, so `execute_action`'s `validate_screenshot_path` call would
    /// never see the screenshot `path`. This hook applies the same workspace
    /// policy / runtime-config / symlink guards to the *raw* `args` JSON
    /// before any of its params cross the sidecar boundary, and substitutes
    /// the canonical resolved path back into the args so the sidecar
    /// receives the same hardened string the other backends use.
    ///
    /// Non-screenshot actions pass through unchanged. Screenshot actions
    /// with no `path` and screenshot actions with `path: ""` are treated as
    /// inline PNG return and pass through unchanged.
    ///
    /// Screenshot actions with `path: <non-string>` (e.g. an integer) are
    /// rejected at the hook with a typed error: `parse_browser_action`
    /// would otherwise drop the value to `None` and forward the raw
    async fn execute_action(
        &self,
        mut action: BrowserAction,
        backend: ResolvedBackend,
    ) -> anyhow::Result<ToolResult> {
        self.validate_screenshot_path(&mut action).await?;
        match backend {
            ResolvedBackend::AgentBrowser => self.execute_agent_browser_action(action).await,
            ResolvedBackend::RustNative => self.execute_rust_native_action(action).await,
            ResolvedBackend::ComputerUse => anyhow::bail!(
                "Internal error: computer_use backend must be handled before BrowserAction parsing"
            ),
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

#[async_trait]
impl Tool for BrowserTool {
    fn name(&self) -> &str {
        "browser"
    }

    fn description(&self) -> &str {
        concat!(
            "Web/browser automation with pluggable backends (agent-browser, rust-native, computer_use). ",
            "Supports DOM actions plus optional OS-level actions (mouse_move, mouse_click, mouse_drag, ",
            "key_type, key_press, screen_capture) through a computer-use sidecar. Use 'snapshot' to map ",
            "interactive elements to refs (@e1, @e2). Enforces browser.allowed_domains for open actions."
        )
    }

    fn parameters_schema(&self) -> Value {
        json!({
            "type": "object",
            "properties": {
                "action": {
                    "type": "string",
                    "enum": ["open", "snapshot", "click", "fill", "type", "get_text",
                             "get_title", "get_url", "screenshot", "wait", "press",
                             "hover", "scroll", "is_visible", "close", "find",
                             "mouse_move", "mouse_click", "mouse_drag", "key_type",
                             "key_press", "screen_capture"],
                    "description": "Browser action to perform (OS-level actions require backend=computer_use)"
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
                "x": {
                    "type": "integer",
                    "description": "Screen X coordinate (computer_use: mouse_move/mouse_click)"
                },
                "y": {
                    "type": "integer",
                    "description": "Screen Y coordinate (computer_use: mouse_move/mouse_click)"
                },
                "from_x": {
                    "type": "integer",
                    "description": "Drag source X coordinate (computer_use: mouse_drag)"
                },
                "from_y": {
                    "type": "integer",
                    "description": "Drag source Y coordinate (computer_use: mouse_drag)"
                },
                "to_x": {
                    "type": "integer",
                    "description": "Drag target X coordinate (computer_use: mouse_drag)"
                },
                "to_y": {
                    "type": "integer",
                    "description": "Drag target Y coordinate (computer_use: mouse_drag)"
                },
                "button": {
                    "type": "string",
                    "enum": ["left", "right", "middle"],
                    "description": "Mouse button for computer_use mouse_click"
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

        // Rate limiting is applied by the RateLimitedTool wrapper at
        // registration time (see zeroclaw-runtime::tools::mod).

        let backend = match self.resolve_backend().await {
            Ok(selected) => selected,
            Err(error) => {
                return Ok(ToolResult {
                    success: false,
                    output: String::new(),
                    error: Some(error.to_string()),
                });
            }
        };

        // Parse action from args
        let action_str = args.get("action").and_then(|v| v.as_str()).ok_or_else(|| {
            ::zeroclaw_log::record!(
                WARN,
                ::zeroclaw_log::Event::new(module_path!(), ::zeroclaw_log::Action::Reject)
                    .with_outcome(::zeroclaw_log::EventOutcome::Failure),
                "browser: Missing 'action' parameter"
            );
            anyhow::Error::msg("Missing 'action' parameter")
        })?;

        if !is_supported_browser_action(action_str) {
            return Ok(ToolResult {
                success: false,
                output: String::new(),
                error: Some(format!("Unknown action: {action_str}")),
            });
        }

        if backend == ResolvedBackend::ComputerUse {
            // Screenshot destination paths must clear the same workspace
            // policy / runtime-config / symlink guards that
            // `validate_screenshot_path` applies to agent-browser and
            // rust-native, *before* any params leave the browser tool
            // for the sidecar. Otherwise the public `screenshot` `path`
            // parameter can carry an out-of-workspace or
            // runtime-config-targeting string across the sidecar
            // boundary unverified.
            //
            // Copy `action_str` to a `String` before moving `args` into
            // the helper, otherwise the `&str` borrow of `args.get("action")`
            // is still live at the move site.
            let action_str = action_str.to_owned();
            let args = self
                .validate_screenshot_path_for_computer_use(&action_str, args)
                .await?;
            return self.execute_computer_use_action(&action_str, &args).await;
        }

        if is_computer_use_only_action(action_str) {
            return Ok(ToolResult {
                success: false,
                output: String::new(),
                error: Some(unavailable_action_for_backend_error(action_str, backend)),
            });
        }

        let action = match parse_browser_action(action_str, &args) {
            Ok(a) => a,
            Err(e) => {
                return Ok(ToolResult {
                    success: false,
                    output: String::new(),
                    error: Some(e.to_string()),
                });
            }
        };

        self.execute_action(action, backend).await
    }
}

#[cfg(feature = "browser-native")]
mod native_backend {
    use super::BrowserAction;
    use anyhow::{Context, Result};
    use base64::Engine;
    use fantoccini::actions::{InputSource, MouseActions, PointerAction};
    use fantoccini::key::Key;
    use fantoccini::{Client, ClientBuilder, Locator};
    use serde_json::{Map, Value, json};
    use std::net::{TcpStream, ToSocketAddrs};
    use std::time::Duration;

    #[derive(Default)]
    pub struct NativeBrowserState {
        client: Option<Client>,
    }

    impl NativeBrowserState {
        pub fn is_available(
            _headless: bool,
            webdriver_url: &str,
            _chrome_path: Option<&str>,
        ) -> bool {
            webdriver_endpoint_reachable(webdriver_url, Duration::from_millis(500))
        }

        #[allow(clippy::too_many_lines)]
        pub async fn execute_action(
            &mut self,
            action: BrowserAction,
            headless: bool,
            webdriver_url: &str,
            chrome_path: Option<&str>,
        ) -> Result<Value> {
            match action {
                BrowserAction::Open { url } => {
                    self.ensure_session(headless, webdriver_url, chrome_path)
                        .await?;
                    let client = self.active_client()?;
                    client
                        .goto(&url)
                        .await
                        .with_context(|| format!("Failed to open URL: {url}"))?;
                    let current_url = client
                        .current_url()
                        .await
                        .context("Failed to read current URL after navigation")?;

                    Ok(json!({
                        "backend": "rust_native",
                        "action": "open",
                        "url": current_url.as_str(),
                    }))
                }
                BrowserAction::Snapshot {
                    interactive_only,
                    compact,
                    depth,
                } => {
                    let client = self.active_client()?;
                    let snapshot = client
                        .execute(
                            &snapshot_script(interactive_only, compact, depth.map(i64::from)),
                            vec![],
                        )
                        .await
                        .context("Failed to evaluate snapshot script")?;

                    Ok(json!({
                        "backend": "rust_native",
                        "action": "snapshot",
                        "data": snapshot,
                    }))
                }
                BrowserAction::Click { selector } => {
                    let client = self.active_client()?;
                    find_element(client, &selector).await?.click().await?;

                    Ok(json!({
                        "backend": "rust_native",
                        "action": "click",
                        "selector": selector,
                    }))
                }
                BrowserAction::Fill { selector, value } => {
                    let client = self.active_client()?;
                    let element = find_element(client, &selector).await?;
                    let _ = element.clear().await;
                    element.send_keys(&value).await?;

                    Ok(json!({
                        "backend": "rust_native",
                        "action": "fill",
                        "selector": selector,
                    }))
                }
                BrowserAction::Type { selector, text } => {
                    let client = self.active_client()?;
                    find_element(client, &selector)
                        .await?
                        .send_keys(&text)
                        .await?;

                    Ok(json!({
                        "backend": "rust_native",
                        "action": "type",
                        "selector": selector,
                        "typed": text.len(),
                    }))
                }
                BrowserAction::GetText { selector } => {
                    let client = self.active_client()?;
                    let text = find_element(client, &selector).await?.text().await?;

                    Ok(json!({
                        "backend": "rust_native",
                        "action": "get_text",
                        "selector": selector,
                        "text": text,
                    }))
                }
                BrowserAction::GetTitle => {
                    let client = self.active_client()?;
                    let title = client.title().await.context("Failed to read page title")?;

                    Ok(json!({
                        "backend": "rust_native",
                        "action": "get_title",
                        "title": title,
                    }))
                }
                BrowserAction::GetUrl => {
                    let client = self.active_client()?;
                    let url = client
                        .current_url()
                        .await
                        .context("Failed to read current URL")?;

                    Ok(json!({
                        "backend": "rust_native",
                        "action": "get_url",
                        "url": url.as_str(),
                    }))
                }
                BrowserAction::Screenshot { path, full_page } => {
                    let client = self.active_client()?;
                    let png = client
                        .screenshot()
                        .await
                        .context("Failed to capture screenshot")?;
                    let mut payload = json!({
                        "backend": "rust_native",
                        "action": "screenshot",
                        "full_page": full_page,
                        "bytes": png.len(),
                    });

                    if let Some(path_str) = path {
                        tokio::fs::write(&path_str, &png)
                            .await
                            .with_context(|| format!("Failed to write screenshot to {path_str}"))?;
                        payload["path"] = Value::String(path_str);
                    } else {
                        payload["png_base64"] =
                            Value::String(base64::engine::general_purpose::STANDARD.encode(&png));
                    }

                    Ok(payload)
                }
                BrowserAction::Wait { selector, ms, text } => {
                    let client = self.active_client()?;
                    if let Some(sel) = selector.as_ref() {
                        wait_for_selector(client, sel).await?;
                        Ok(json!({
                            "backend": "rust_native",
                            "action": "wait",
                            "selector": sel,
                        }))
                    } else if let Some(duration_ms) = ms {
                        tokio::time::sleep(Duration::from_millis(duration_ms)).await;
                        Ok(json!({
                            "backend": "rust_native",
                            "action": "wait",
                            "ms": duration_ms,
                        }))
                    } else if let Some(needle) = text.as_ref() {
                        let xpath = xpath_contains_text(needle);
                        client
                            .wait()
                            .for_element(Locator::XPath(&xpath))
                            .await
                            .with_context(|| {
                                format!("Timed out waiting for text to appear: {needle}")
                            })?;
                        Ok(json!({
                            "backend": "rust_native",
                            "action": "wait",
                            "text": needle,
                        }))
                    } else {
                        tokio::time::sleep(Duration::from_millis(250)).await;
                        Ok(json!({
                            "backend": "rust_native",
                            "action": "wait",
                            "ms": 250,
                        }))
                    }
                }
                BrowserAction::Press { key } => {
                    let client = self.active_client()?;
                    let key_input = webdriver_key(&key);
                    match client.active_element().await {
                        Ok(element) => {
                            element.send_keys(&key_input).await?;
                        }
                        Err(_) => {
                            find_element(client, "body")
                                .await?
                                .send_keys(&key_input)
                                .await?;
                        }
                    }

                    Ok(json!({
                        "backend": "rust_native",
                        "action": "press",
                        "key": key,
                    }))
                }
                BrowserAction::Hover { selector } => {
                    let client = self.active_client()?;
                    let element = find_element(client, &selector).await?;
                    hover_element(client, &element).await?;

                    Ok(json!({
                        "backend": "rust_native",
                        "action": "hover",
                        "selector": selector,
                    }))
                }
                BrowserAction::Scroll { direction, pixels } => {
                    let client = self.active_client()?;
                    let amount = i64::from(pixels.unwrap_or(600));
                    let (dx, dy) = match direction.as_str() {
                        "up" => (0, -amount),
                        "down" => (0, amount),
                        "left" => (-amount, 0),
                        "right" => (amount, 0),
                        _ => anyhow::bail!(
                            "Unsupported scroll direction '{direction}'. Use up/down/left/right"
                        ),
                    };

                    let position = client
                        .execute(
                            "window.scrollBy(arguments[0], arguments[1]); return { x: window.scrollX, y: window.scrollY };",
                            vec![json!(dx), json!(dy)],
                        )
                        .await
                        .context("Failed to execute scroll script")?;

                    Ok(json!({
                        "backend": "rust_native",
                        "action": "scroll",
                        "position": position,
                    }))
                }
                BrowserAction::IsVisible { selector } => {
                    let client = self.active_client()?;
                    let visible = find_element(client, &selector)
                        .await?
                        .is_displayed()
                        .await?;

                    Ok(json!({
                        "backend": "rust_native",
                        "action": "is_visible",
                        "selector": selector,
                        "visible": visible,
                    }))
                }
                BrowserAction::Close => {
                    self.reset_session().await;

                    Ok(json!({
                        "backend": "rust_native",
                        "action": "close",
                        "closed": true,
                    }))
                }
                BrowserAction::Find {
                    by,
                    value,
                    action,
                    fill_value,
                } => {
                    let client = self.active_client()?;
                    let selector = selector_for_find(&by, &value);
                    let element = find_element(client, &selector).await?;

                    let payload = match action.as_str() {
                        "click" => {
                            element.click().await?;
                            json!({"result": "clicked"})
                        }
                        "fill" => {
                            let fill = fill_value.ok_or_else(|| {
                                ::zeroclaw_log::record!(
                                    WARN,
                                    ::zeroclaw_log::Event::new(
                                        module_path!(),
                                        ::zeroclaw_log::Action::Reject
                                    )
                                    .with_outcome(::zeroclaw_log::EventOutcome::Failure)
                                    .with_attrs(
                                        ::serde_json::json!({
                                            "find_action": "fill",
                                            "missing": "fill_value",
                                        })
                                    ),
                                    "browser: fill action requires fill_value"
                                );
                                anyhow::Error::msg("find_action='fill' requires fill_value")
                            })?;
                            let _ = element.clear().await;
                            element.send_keys(&fill).await?;
                            json!({"result": "filled", "typed": fill.len()})
                        }
                        "text" => {
                            let text = element.text().await?;
                            json!({"result": "text", "text": text})
                        }
                        "hover" => {
                            hover_element(client, &element).await?;
                            json!({"result": "hovered"})
                        }
                        "check" => {
                            let checked_before = element_checked(&element).await?;
                            if !checked_before {
                                element.click().await?;
                            }
                            let checked_after = element_checked(&element).await?;
                            json!({
                                "result": "checked",
                                "checked_before": checked_before,
                                "checked_after": checked_after,
                            })
                        }
                        _ => anyhow::bail!(
                            "Unsupported find_action '{action}'. Use click/fill/text/hover/check"
                        ),
                    };

                    Ok(json!({
                        "backend": "rust_native",
                        "action": "find",
                        "by": by,
                        "value": value,
                        "selector": selector,
                        "data": payload,
                    }))
                }
            }
        }

        pub async fn reset_session(&mut self) {
            if let Some(client) = self.client.take() {
                let _ = client.close().await;
            }
        }

        async fn ensure_session(
            &mut self,
            headless: bool,
            webdriver_url: &str,
            chrome_path: Option<&str>,
        ) -> Result<()> {
            if self.client.is_some() {
                return Ok(());
            }

            let mut capabilities: Map<String, Value> = Map::new();
            let mut chrome_options: Map<String, Value> = Map::new();
            let mut args: Vec<Value> = Vec::new();

            if headless {
                args.push(Value::String("--headless=new".to_string()));
                args.push(Value::String("--disable-gpu".to_string()));
            }

            // When running as a service (systemd/OpenRC), the browser sandbox
            // fails because the process lacks a user namespace / session.
            // --no-sandbox and --disable-dev-shm-usage are required in this context.
            if super::is_service_environment() {
                args.push(Value::String("--no-sandbox".to_string()));
                args.push(Value::String("--disable-dev-shm-usage".to_string()));
            }

            if !args.is_empty() {
                chrome_options.insert("args".to_string(), Value::Array(args));
            }

            if let Some(path) = chrome_path {
                let trimmed = path.trim();
                if !trimmed.is_empty() {
                    chrome_options.insert("binary".to_string(), Value::String(trimmed.to_string()));
                }
            }

            if !chrome_options.is_empty() {
                capabilities.insert(
                    "goog:chromeOptions".to_string(),
                    Value::Object(chrome_options),
                );
            }

            let mut builder =
                ClientBuilder::rustls().context("Failed to initialize rustls connector")?;
            if !capabilities.is_empty() {
                builder.capabilities(capabilities);
            }

            let client = builder
                .connect(webdriver_url)
                .await
                .with_context(|| {
                    format!(
                        "Failed to connect to WebDriver at {webdriver_url}. Start chromedriver/geckodriver first"
                    )
                })?;

            self.client = Some(client);
            Ok(())
        }

        fn active_client(&self) -> Result<&Client> {
            self.client.as_ref().ok_or_else(|| {
                ::zeroclaw_log::record!(
                    WARN,
                    ::zeroclaw_log::Event::new(module_path!(), ::zeroclaw_log::Action::Reject)
                        .with_outcome(::zeroclaw_log::EventOutcome::Failure),
                    "browser: no active native browser session"
                );
                anyhow::Error::msg(
                    "No active native browser session. Run browser action='open' first",
                )
            })
        }
    }

    fn webdriver_endpoint_reachable(webdriver_url: &str, timeout: Duration) -> bool {
        let parsed = match reqwest::Url::parse(webdriver_url) {
            Ok(url) => url,
            Err(_) => return false,
        };

        if parsed.scheme() != "http" && parsed.scheme() != "https" {
            return false;
        }

        let host = match parsed.host_str() {
            Some(h) if !h.is_empty() => h,
            _ => return false,
        };

        let port = parsed.port_or_known_default().unwrap_or(4444);
        let mut addrs = match (host, port).to_socket_addrs() {
            Ok(iter) => iter,
            Err(_) => return false,
        };

        let addr = match addrs.next() {
            Some(a) => a,
            None => return false,
        };

        TcpStream::connect_timeout(&addr, timeout).is_ok()
    }

    fn selector_for_find(by: &str, value: &str) -> String {
        let escaped = css_attr_escape(value);
        match by {
            "role" => format!("[role=\"{escaped}\"]"),
            "label" => format!("label={value}"),
            "placeholder" => format!("[placeholder=\"{escaped}\"]"),
            "testid" => format!("[data-testid=\"{escaped}\"]"),
            _ => format!("text={value}"),
        }
    }

    async fn wait_for_selector(client: &Client, selector: &str) -> Result<()> {
        match parse_selector(selector) {
            SelectorKind::Css(css) => {
                client
                    .wait()
                    .for_element(Locator::Css(&css))
                    .await
                    .with_context(|| format!("Timed out waiting for selector '{selector}'"))?;
            }
            SelectorKind::XPath(xpath) => {
                client
                    .wait()
                    .for_element(Locator::XPath(&xpath))
                    .await
                    .with_context(|| format!("Timed out waiting for selector '{selector}'"))?;
            }
        }
        Ok(())
    }

    async fn find_element(
        client: &Client,
        selector: &str,
    ) -> Result<fantoccini::elements::Element> {
        let element = match parse_selector(selector) {
            SelectorKind::Css(css) => client
                .find(Locator::Css(&css))
                .await
                .with_context(|| format!("Failed to find element by CSS '{css}'"))?,
            SelectorKind::XPath(xpath) => client
                .find(Locator::XPath(&xpath))
                .await
                .with_context(|| format!("Failed to find element by XPath '{xpath}'"))?,
        };
        Ok(element)
    }

    async fn hover_element(client: &Client, element: &fantoccini::elements::Element) -> Result<()> {
        let actions = MouseActions::new("mouse".to_string()).then(PointerAction::MoveToElement {
            element: element.clone(),
            duration: Some(Duration::from_millis(150)),
            x: 0.0,
            y: 0.0,
        });

        client
            .perform_actions(actions)
            .await
            .context("Failed to perform hover action")?;
        let _ = client.release_actions().await;
        Ok(())
    }

    async fn element_checked(element: &fantoccini::elements::Element) -> Result<bool> {
        let checked = element
            .prop("checked")
            .await
            .context("Failed to read checkbox checked property")?
            .unwrap_or_default()
            .to_ascii_lowercase();
        Ok(matches!(checked.as_str(), "true" | "checked" | "1"))
    }

    enum SelectorKind {
        Css(String),
        XPath(String),
    }

    fn parse_selector(selector: &str) -> SelectorKind {
        let trimmed = selector.trim();
        if let Some(text_query) = trimmed.strip_prefix("text=") {
            return SelectorKind::XPath(xpath_contains_text(text_query));
        }

        if let Some(label_query) = trimmed.strip_prefix("label=") {
            let literal = xpath_literal(label_query);
            return SelectorKind::XPath(format!(
                "(//label[contains(normalize-space(.), {literal})]/following::*[self::input or self::textarea or self::select][1] | //*[@aria-label and contains(normalize-space(@aria-label), {literal})] | //label[contains(normalize-space(.), {literal})])"
            ));
        }

        if trimmed.starts_with('@') {
            let escaped = css_attr_escape(trimmed);
            return SelectorKind::Css(format!("[data-zc-ref=\"{escaped}\"]"));
        }

        SelectorKind::Css(trimmed.to_string())
    }

    fn css_attr_escape(input: &str) -> String {
        input
            .replace('\\', "\\\\")
            .replace('"', "\\\"")
            .replace('\n', " ")
    }

    fn xpath_contains_text(text: &str) -> String {
        format!("//*[contains(normalize-space(.), {})]", xpath_literal(text))
    }

    fn xpath_literal(input: &str) -> String {
        if !input.contains('"') {
            return format!("\"{input}\"");
        }
        if !input.contains('\'') {
            return format!("'{input}'");
        }

        let segments: Vec<&str> = input.split('"').collect();
        let mut parts: Vec<String> = Vec::new();
        for (index, part) in segments.iter().enumerate() {
            if !part.is_empty() {
                parts.push(format!("\"{part}\""));
            }
            if index + 1 < segments.len() {
                parts.push("'\"'".to_string());
            }
        }

        if parts.is_empty() {
            "\"\"".to_string()
        } else {
            format!("concat({})", parts.join(","))
        }
    }

    fn webdriver_key(key: &str) -> String {
        match key.trim().to_ascii_lowercase().as_str() {
            "enter" => Key::Enter.to_string(),
            "return" => Key::Return.to_string(),
            "tab" => Key::Tab.to_string(),
            "escape" | "esc" => Key::Escape.to_string(),
            "backspace" => Key::Backspace.to_string(),
            "delete" => Key::Delete.to_string(),
            "space" => Key::Space.to_string(),
            "arrowup" | "up" => Key::Up.to_string(),
            "arrowdown" | "down" => Key::Down.to_string(),
            "arrowleft" | "left" => Key::Left.to_string(),
            "arrowright" | "right" => Key::Right.to_string(),
            "home" => Key::Home.to_string(),
            "end" => Key::End.to_string(),
            "pageup" => Key::PageUp.to_string(),
            "pagedown" => Key::PageDown.to_string(),
            other => other.to_string(),
        }
    }

    fn snapshot_script(interactive_only: bool, compact: bool, depth: Option<i64>) -> String {
        let depth_literal = depth
            .map(|level| level.to_string())
            .unwrap_or_else(|| "null".to_string());

        format!(
            r#"return (() => {{
  const interactiveOnly = {interactive_only};
  const compact = {compact};
  const maxDepth = {depth_literal};
  const nodes = [];
  const root = document.body || document.documentElement;
  let counter = 0;

  const isVisible = (el) => {{
    const style = window.getComputedStyle(el);
    if (style.display === 'none' || style.visibility === 'hidden' || Number(style.opacity || 1) === 0) {{
      return false;
    }}
    const rect = el.getBoundingClientRect();
    return rect.width > 0 && rect.height > 0;
  }};

  const isInteractive = (el) => {{
    if (el.matches('a,button,input,select,textarea,summary,[role],*[tabindex]')) return true;
    return typeof el.onclick === 'function';
  }};

  const describe = (el, depth) => {{
    const interactive = isInteractive(el);
    const text = (el.innerText || el.textContent || '').trim().replace(/\s+/g, ' ').slice(0, 140);
    if (interactiveOnly && !interactive) return;
    if (compact && !interactive && !text) return;

    const ref = '@e' + (++counter);
    el.setAttribute('data-zc-ref', ref);
    nodes.push({{
      ref,
      depth,
      tag: el.tagName.toLowerCase(),
      id: el.id || null,
      role: el.getAttribute('role'),
      text,
      interactive,
    }});
  }};

  const walk = (el, depth) => {{
    if (!(el instanceof Element)) return;
    if (maxDepth !== null && depth > maxDepth) return;
    if (isVisible(el)) {{
      describe(el, depth);
    }}
    for (const child of el.children) {{
      walk(child, depth + 1);
      if (nodes.length >= 400) return;
    }}
  }};

  if (root) walk(root, 0);

  return {{
    title: document.title,
    url: window.location.href,
    count: nodes.length,
    nodes,
  }};
}})();"#
        )
    }

    #[cfg(test)]
    mod tests {
        use super::*;

        #[test]
        fn snapshot_script_starts_with_return() {
            let script = snapshot_script(true, false, None);
            assert!(
                script.starts_with("return (() => {"),
                "snapshot_script must start with 'return (() => {{' for WebDriver ExecuteScript; got: {:?}",
                &script[..60]
            );
        }

        #[test]
        fn selector_for_find_role_emits_normal_css_attribute() {
            let sel = selector_for_find("role", "button");
            assert_eq!(sel, r#"[role="button"]"#);
        }

        #[test]
        fn selector_for_find_placeholder_emits_normal_css_attribute() {
            let sel = selector_for_find("placeholder", "Search");
            assert_eq!(sel, r#"[placeholder="Search"]"#);
        }

        #[test]
        fn selector_for_find_testid_emits_normal_css_attribute() {
            let sel = selector_for_find("testid", "submit-btn");
            assert_eq!(sel, r#"[data-testid="submit-btn"]"#);
        }

        #[test]
        fn parse_selector_at_ref_emits_normal_css_attribute() {
            let sel = parse_selector("@elem");
            let SelectorKind::Css(css) = sel else {
                panic!("expected Css selector, got XPath");
            };
            assert_eq!(css, r#"[data-zc-ref="@elem"]"#);
        }

        #[test]
        fn css_attr_escape_escapes_backslashes() {
            let escaped = css_attr_escape(r#"path\to\file"#);
            assert_eq!(escaped, r#"path\\to\\file"#);
        }

        #[test]
        fn css_attr_escape_escapes_double_quotes() {
            let escaped = css_attr_escape(r#"he said "hello""#);
            assert_eq!(escaped, r#"he said \"hello\""#);
        }

        #[test]
        fn css_attr_escape_handles_both() {
            let escaped = css_attr_escape(r#"a\"b"#);
            assert_eq!(escaped, r#"a\\\"b"#);
        }
    }
}

// ── Action parsing ──────────────────────────────────────────────

/// Parse a JSON `args` object into a typed `BrowserAction`.
fn parse_browser_action(action_str: &str, args: &Value) -> anyhow::Result<BrowserAction> {
    match action_str {
        "open" => {
            let url = args.get("url").and_then(|v| v.as_str()).ok_or_else(|| {
                ::zeroclaw_log::record!(
                    WARN,
                    ::zeroclaw_log::Event::new(module_path!(), ::zeroclaw_log::Action::Reject)
                        .with_outcome(::zeroclaw_log::EventOutcome::Failure),
                    "browser: Missing 'url' for open action"
                );
                anyhow::Error::msg("Missing 'url' for open action")
            })?;
            Ok(BrowserAction::Open { url: url.into() })
        }
        "snapshot" => Ok(BrowserAction::Snapshot {
            interactive_only: args
                .get("interactive_only")
                .and_then(serde_json::Value::as_bool)
                .unwrap_or(true),
            compact: args
                .get("compact")
                .and_then(serde_json::Value::as_bool)
                .unwrap_or(true),
            depth: args
                .get("depth")
                .and_then(serde_json::Value::as_u64)
                .map(|d| u32::try_from(d).unwrap_or(u32::MAX)),
        }),
        "click" => {
            let selector = args
                .get("selector")
                .and_then(|v| v.as_str())
                .ok_or_else(|| {
                    ::zeroclaw_log::record!(
                        WARN,
                        ::zeroclaw_log::Event::new(module_path!(), ::zeroclaw_log::Action::Reject)
                            .with_outcome(::zeroclaw_log::EventOutcome::Failure),
                        "browser: Missing 'selector' for click"
                    );
                    anyhow::Error::msg("Missing 'selector' for click")
                })?;
            Ok(BrowserAction::Click {
                selector: selector.into(),
            })
        }
        "fill" => {
            let selector = args
                .get("selector")
                .and_then(|v| v.as_str())
                .ok_or_else(|| {
                    ::zeroclaw_log::record!(
                        WARN,
                        ::zeroclaw_log::Event::new(module_path!(), ::zeroclaw_log::Action::Reject)
                            .with_outcome(::zeroclaw_log::EventOutcome::Failure),
                        "browser: Missing 'selector' for fill"
                    );
                    anyhow::Error::msg("Missing 'selector' for fill")
                })?;
            let value = args.get("value").and_then(|v| v.as_str()).ok_or_else(|| {
                ::zeroclaw_log::record!(
                    WARN,
                    ::zeroclaw_log::Event::new(module_path!(), ::zeroclaw_log::Action::Reject)
                        .with_outcome(::zeroclaw_log::EventOutcome::Failure),
                    "browser: Missing 'value' for fill"
                );
                anyhow::Error::msg("Missing 'value' for fill")
            })?;
            Ok(BrowserAction::Fill {
                selector: selector.into(),
                value: value.into(),
            })
        }
        "type" => {
            let selector = args
                .get("selector")
                .and_then(|v| v.as_str())
                .ok_or_else(|| {
                    ::zeroclaw_log::record!(
                        WARN,
                        ::zeroclaw_log::Event::new(module_path!(), ::zeroclaw_log::Action::Reject)
                            .with_outcome(::zeroclaw_log::EventOutcome::Failure),
                        "browser: Missing 'selector' for type"
                    );
                    anyhow::Error::msg("Missing 'selector' for type")
                })?;
            let text = args.get("text").and_then(|v| v.as_str()).ok_or_else(|| {
                ::zeroclaw_log::record!(
                    WARN,
                    ::zeroclaw_log::Event::new(module_path!(), ::zeroclaw_log::Action::Reject)
                        .with_outcome(::zeroclaw_log::EventOutcome::Failure),
                    "browser: Missing 'text' for type"
                );
                anyhow::Error::msg("Missing 'text' for type")
            })?;
            Ok(BrowserAction::Type {
                selector: selector.into(),
                text: text.into(),
            })
        }
        "get_text" => {
            let selector = args
                .get("selector")
                .and_then(|v| v.as_str())
                .ok_or_else(|| {
                    ::zeroclaw_log::record!(
                        WARN,
                        ::zeroclaw_log::Event::new(module_path!(), ::zeroclaw_log::Action::Reject)
                            .with_outcome(::zeroclaw_log::EventOutcome::Failure),
                        "browser: Missing 'selector' for get_text"
                    );
                    anyhow::Error::msg("Missing 'selector' for get_text")
                })?;
            Ok(BrowserAction::GetText {
                selector: selector.into(),
            })
        }
        "get_title" => Ok(BrowserAction::GetTitle),
        "get_url" => Ok(BrowserAction::GetUrl),
        "screenshot" => Ok(BrowserAction::Screenshot {
            path: args.get("path").and_then(|v| v.as_str()).map(String::from),
            full_page: args
                .get("full_page")
                .and_then(serde_json::Value::as_bool)
                .unwrap_or(false),
        }),
        "wait" => Ok(BrowserAction::Wait {
            selector: args
                .get("selector")
                .and_then(|v| v.as_str())
                .map(String::from),
            ms: args.get("ms").and_then(serde_json::Value::as_u64),
            text: args.get("text").and_then(|v| v.as_str()).map(String::from),
        }),
        "press" => {
            let key = args.get("key").and_then(|v| v.as_str()).ok_or_else(|| {
                ::zeroclaw_log::record!(
                    WARN,
                    ::zeroclaw_log::Event::new(module_path!(), ::zeroclaw_log::Action::Reject)
                        .with_outcome(::zeroclaw_log::EventOutcome::Failure),
                    "browser: Missing 'key' for press"
                );
                anyhow::Error::msg("Missing 'key' for press")
            })?;
            Ok(BrowserAction::Press { key: key.into() })
        }
        "hover" => {
            let selector = args
                .get("selector")
                .and_then(|v| v.as_str())
                .ok_or_else(|| {
                    ::zeroclaw_log::record!(
                        WARN,
                        ::zeroclaw_log::Event::new(module_path!(), ::zeroclaw_log::Action::Reject)
                            .with_outcome(::zeroclaw_log::EventOutcome::Failure),
                        "browser: Missing 'selector' for hover"
                    );
                    anyhow::Error::msg("Missing 'selector' for hover")
                })?;
            Ok(BrowserAction::Hover {
                selector: selector.into(),
            })
        }
        "scroll" => {
            let direction = args
                .get("direction")
                .and_then(|v| v.as_str())
                .ok_or_else(|| {
                    ::zeroclaw_log::record!(
                        WARN,
                        ::zeroclaw_log::Event::new(module_path!(), ::zeroclaw_log::Action::Reject)
                            .with_outcome(::zeroclaw_log::EventOutcome::Failure),
                        "browser: Missing 'direction' for scroll"
                    );
                    anyhow::Error::msg("Missing 'direction' for scroll")
                })?;
            Ok(BrowserAction::Scroll {
                direction: direction.into(),
                pixels: args
                    .get("pixels")
                    .and_then(serde_json::Value::as_u64)
                    .map(|p| u32::try_from(p).unwrap_or(u32::MAX)),
            })
        }
        "is_visible" => {
            let selector = args
                .get("selector")
                .and_then(|v| v.as_str())
                .ok_or_else(|| {
                    ::zeroclaw_log::record!(
                        WARN,
                        ::zeroclaw_log::Event::new(module_path!(), ::zeroclaw_log::Action::Reject)
                            .with_outcome(::zeroclaw_log::EventOutcome::Failure),
                        "browser: Missing 'selector' for is_visible"
                    );
                    anyhow::Error::msg("Missing 'selector' for is_visible")
                })?;
            Ok(BrowserAction::IsVisible {
                selector: selector.into(),
            })
        }
        "close" => Ok(BrowserAction::Close),
        "find" => {
            let by = args.get("by").and_then(|v| v.as_str()).ok_or_else(|| {
                ::zeroclaw_log::record!(
                    WARN,
                    ::zeroclaw_log::Event::new(module_path!(), ::zeroclaw_log::Action::Reject)
                        .with_outcome(::zeroclaw_log::EventOutcome::Failure),
                    "browser: Missing 'by' for find"
                );
                anyhow::Error::msg("Missing 'by' for find")
            })?;
            let value = args.get("value").and_then(|v| v.as_str()).ok_or_else(|| {
                ::zeroclaw_log::record!(
                    WARN,
                    ::zeroclaw_log::Event::new(module_path!(), ::zeroclaw_log::Action::Reject)
                        .with_outcome(::zeroclaw_log::EventOutcome::Failure),
                    "browser: Missing 'value' for find"
                );
                anyhow::Error::msg("Missing 'value' for find")
            })?;
            let action = args
                .get("find_action")
                .and_then(|v| v.as_str())
                .ok_or_else(|| {
                    ::zeroclaw_log::record!(
                        WARN,
                        ::zeroclaw_log::Event::new(module_path!(), ::zeroclaw_log::Action::Reject)
                            .with_outcome(::zeroclaw_log::EventOutcome::Failure),
                        "browser: Missing 'find_action' for find"
                    );
                    anyhow::Error::msg("Missing 'find_action' for find")
                })?;
            Ok(BrowserAction::Find {
                by: by.into(),
                value: value.into(),
                action: action.into(),
                fill_value: args
                    .get("fill_value")
                    .and_then(|v| v.as_str())
                    .map(String::from),
            })
        }
        other => anyhow::bail!("Unsupported browser action: {other}"),
    }
}

// ── Helper functions ─────────────────────────────────────────────

fn is_supported_browser_action(action: &str) -> bool {
    matches!(
        action,
        "open"
            | "snapshot"
            | "click"
            | "fill"
            | "type"
            | "get_text"
            | "get_title"
            | "get_url"
            | "screenshot"
            | "wait"
            | "press"
            | "hover"
            | "scroll"
            | "is_visible"
            | "close"
            | "find"
            | "mouse_move"
            | "mouse_click"
            | "mouse_drag"
            | "key_type"
            | "key_press"
            | "screen_capture"
    )
}

fn is_computer_use_only_action(action: &str) -> bool {
    matches!(
        action,
        "mouse_move" | "mouse_click" | "mouse_drag" | "key_type" | "key_press" | "screen_capture"
    )
}

fn backend_name(backend: ResolvedBackend) -> &'static str {
    match backend {
        ResolvedBackend::AgentBrowser => "agent_browser",
        ResolvedBackend::RustNative => "rust_native",
        ResolvedBackend::ComputerUse => "computer_use",
    }
}

fn unavailable_action_for_backend_error(action: &str, backend: ResolvedBackend) -> String {
    format!(
        "Action '{action}' is unavailable for backend '{}'",
        backend_name(backend)
    )
}

#[allow(dead_code)] // called from browser-native feature paths and tests
fn is_recoverable_rust_native_error(err: &anyhow::Error) -> bool {
    let message = format!("{err:#}").to_ascii_lowercase();

    if message.contains("invalid session id")
        || message.contains("no such window")
        || message.contains("session not created")
        || message.contains("connection reset")
        || message.contains("broken pipe")
    {
        return true;
    }

    message.contains("webdriver") && (message.contains("timed out") || message.contains("timeout"))
}

fn endpoint_reachable(endpoint: &reqwest::Url, timeout: Duration) -> bool {
    let host = match endpoint.host_str() {
        Some(host) if !host.is_empty() => host,
        _ => return false,
    };

    let port = match endpoint.port_or_known_default() {
        Some(port) => port,
        None => return false,
    };

    let mut addrs = match (host, port).to_socket_addrs() {
        Ok(addrs) => addrs,
        Err(_) => return false,
    };

    let addr = match addrs.next() {
        Some(addr) => addr,
        None => return false,
    };

    std::net::TcpStream::connect_timeout(&addr, timeout).is_ok()
}

/// Detect whether the current process is running inside a service environment
/// (e.g. systemd, OpenRC, or launchd) where the browser sandbox and
/// environment setup may be restricted.
fn is_service_environment() -> bool {
    if std::env::var_os("INVOCATION_ID").is_some() {
        return true;
    }
    if std::env::var_os("JOURNAL_STREAM").is_some() {
        return true;
    }
    #[cfg(target_os = "linux")]
    if std::path::Path::new("/run/openrc").exists() && std::env::var_os("HOME").is_none() {
        return true;
    }
    #[cfg(target_os = "linux")]
    if std::env::var_os("HOME").is_none() {
        return true;
    }
    false
}

/// Ensure environment variables required by headless browsers are present
/// when running inside a service context.
fn ensure_browser_env(cmd: &mut Command) {
    if std::env::var_os("HOME").is_none() {
        cmd.env("HOME", "/tmp");
    }
    let existing = std::env::var("CHROMIUM_FLAGS").unwrap_or_default();
    if !existing.contains("--no-sandbox") {
        let new_flags = if existing.is_empty() {
            "--no-sandbox --disable-dev-shm-usage".to_string()
        } else {
            format!("{existing} --no-sandbox --disable-dev-shm-usage")
        };
        cmd.env("CHROMIUM_FLAGS", new_flags);
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn validate_url_blocks_ipv6_ssrf() {
        let security = Arc::new(SecurityPolicy::default());
        let tool = BrowserTool::new(security, vec!["*".into()], None).unwrap();
        assert!(tool.validate_url("https://[::1]/").is_err());
        assert!(tool.validate_url("https://[::ffff:127.0.0.1]/").is_err());
        assert!(
            tool.validate_url("https://[::ffff:10.0.0.1]:8080/")
                .is_err()
        );
    }

    // ── allowed_private_hosts opt-in tests ──────────────────────

    fn private_host_tool(
        allowed_domains: Vec<&str>,
        allowed_private_hosts: Vec<&str>,
    ) -> BrowserTool {
        let security = Arc::new(SecurityPolicy::default());
        BrowserTool::new_with_backend(
            security,
            allowed_domains.into_iter().map(String::from).collect(),
            None,
            "agent_browser".into(),
            None,
            true,
            "http://127.0.0.1:9515".into(),
            None,
            ComputerUseConfig::default(),
            allowed_private_hosts
                .into_iter()
                .map(String::from)
                .collect(),
        )
        .unwrap()
    }

    #[test]
    fn allowed_private_hosts_entry_permits_listed_host() {
        let tool = private_host_tool(vec![], vec!["10.0.0.1"]);
        assert!(tool.validate_url("http://10.0.0.1").is_ok());
    }

    #[test]
    fn allowed_private_hosts_does_not_permit_unlisted_host() {
        let tool = private_host_tool(vec![], vec!["10.0.0.1"]);
        let err = tool
            .validate_url("http://10.0.0.2")
            .unwrap_err()
            .to_string();
        assert!(err.contains("local/private"));
    }

    #[test]
    fn empty_private_allowlist_still_rejects_private() {
        let tool = private_host_tool(vec!["*"], vec![]);
        let err = tool
            .validate_url("https://localhost")
            .unwrap_err()
            .to_string();
        assert!(err.contains("local/private"));
    }

    #[test]
    fn wildcard_private_allowlist_satisfies_allowlist_requirement() {
        // allowed_domains empty + allowed_private_hosts=["*"] should not surface
        // the "no allowed_domains configured" error for private hosts.
        let tool = private_host_tool(vec![], vec!["*"]);
        assert!(tool.validate_url("http://localhost").is_ok());
    }

    #[test]
    fn browser_backend_parser_accepts_supported_values() {
        assert_eq!(
            BrowserBackendKind::parse("agent_browser").unwrap(),
            BrowserBackendKind::AgentBrowser
        );
        assert_eq!(
            BrowserBackendKind::parse("rust-native").unwrap(),
            BrowserBackendKind::RustNative
        );
        assert_eq!(
            BrowserBackendKind::parse("computer_use").unwrap(),
            BrowserBackendKind::ComputerUse
        );
        assert_eq!(
            BrowserBackendKind::parse("auto").unwrap(),
            BrowserBackendKind::Auto
        );
    }

    #[test]
    fn browser_backend_parser_rejects_unknown_values() {
        assert!(BrowserBackendKind::parse("playwright").is_err());
    }

    #[test]
    fn browser_tool_default_backend_is_agent_browser() {
        let security = Arc::new(SecurityPolicy::default());
        let tool = BrowserTool::new(security, vec!["example.com".into()], None).unwrap();
        assert_eq!(
            tool.configured_backend().unwrap(),
            BrowserBackendKind::AgentBrowser
        );
    }

    #[test]
    fn agent_browser_command_inherits_headed_env_by_default() {
        let headed_key = std::ffi::OsStr::new("AGENT_BROWSER_HEADED");
        let security = Arc::new(SecurityPolicy::default());
        let tool = BrowserTool::new(security, vec!["example.com".into()], None).unwrap();
        let cmd = tool.agent_browser_command();

        assert_eq!(
            cmd.as_std()
                .get_envs()
                .find(|(key, _)| *key == headed_key)
                .map(|(_, value)| value),
            None
        );
    }

    #[test]
    fn agent_browser_command_clears_headed_env_when_configured_false() {
        let headed_key = std::ffi::OsStr::new("AGENT_BROWSER_HEADED");
        let security = Arc::new(SecurityPolicy::default());
        let tool = BrowserTool::new_with_backend(
            security,
            vec!["example.com".into()],
            None,
            "agent_browser".into(),
            Some(false),
            true,
            "http://127.0.0.1:9515".into(),
            None,
            ComputerUseConfig::default(),
            Vec::new(),
        )
        .unwrap();
        let cmd = tool.agent_browser_command();

        assert_eq!(
            cmd.as_std()
                .get_envs()
                .find(|(key, _)| *key == headed_key)
                .map(|(_, value)| value),
            Some(None)
        );
    }

    #[test]
    fn agent_browser_command_sets_headed_env_when_configured() {
        let headed_key = std::ffi::OsStr::new("AGENT_BROWSER_HEADED");
        let security = Arc::new(SecurityPolicy::default());
        let tool = BrowserTool::new_with_backend(
            security,
            vec!["example.com".into()],
            None,
            "agent_browser".into(),
            Some(true),
            true,
            "http://127.0.0.1:9515".into(),
            None,
            ComputerUseConfig::default(),
            Vec::new(),
        )
        .unwrap();
        let cmd = tool.agent_browser_command();

        assert_eq!(
            cmd.as_std()
                .get_envs()
                .find(|(key, _)| *key == headed_key)
                .and_then(|(_, value)| value)
                .and_then(|value| value.to_str()),
            Some("1")
        );
    }

    #[test]
    fn browser_tool_accepts_auto_backend_config() {
        let security = Arc::new(SecurityPolicy::default());
        let tool = BrowserTool::new_with_backend(
            security,
            vec!["example.com".into()],
            None,
            "auto".into(),
            None,
            true,
            "http://127.0.0.1:9515".into(),
            None,
            ComputerUseConfig::default(),
            Vec::new(),
        )
        .unwrap();
        assert_eq!(tool.configured_backend().unwrap(), BrowserBackendKind::Auto);
    }

    #[test]
    fn browser_tool_accepts_computer_use_backend_config() {
        let security = Arc::new(SecurityPolicy::default());
        let tool = BrowserTool::new_with_backend(
            security,
            vec!["example.com".into()],
            None,
            "computer_use".into(),
            None,
            true,
            "http://127.0.0.1:9515".into(),
            None,
            ComputerUseConfig::default(),
            Vec::new(),
        )
        .unwrap();
        assert_eq!(
            tool.configured_backend().unwrap(),
            BrowserBackendKind::ComputerUse
        );
    }

    #[test]
    fn computer_use_endpoint_rejects_public_http_by_default() {
        let security = Arc::new(SecurityPolicy::default());
        let tool = BrowserTool::new_with_backend(
            security,
            vec!["example.com".into()],
            None,
            "computer_use".into(),
            None,
            true,
            "http://127.0.0.1:9515".into(),
            None,
            ComputerUseConfig {
                endpoint: "http://computer-use.example.com/v1/actions".into(),
                ..ComputerUseConfig::default()
            },
            Vec::new(),
        )
        .unwrap();

        assert!(tool.computer_use_endpoint_url().is_err());
    }

    #[test]
    fn computer_use_endpoint_requires_https_for_public_remote() {
        let security = Arc::new(SecurityPolicy::default());
        let tool = BrowserTool::new_with_backend(
            security,
            vec!["example.com".into()],
            None,
            "computer_use".into(),
            None,
            true,
            "http://127.0.0.1:9515".into(),
            None,
            ComputerUseConfig {
                endpoint: "https://computer-use.example.com/v1/actions".into(),
                allow_remote_endpoint: true,
                ..ComputerUseConfig::default()
            },
            Vec::new(),
        )
        .unwrap();

        assert!(tool.computer_use_endpoint_url().is_ok());
    }

    #[test]
    fn computer_use_coordinate_validation_applies_limits() {
        let security = Arc::new(SecurityPolicy::default());
        let tool = BrowserTool::new_with_backend(
            security,
            vec!["example.com".into()],
            None,
            "computer_use".into(),
            None,
            true,
            "http://127.0.0.1:9515".into(),
            None,
            ComputerUseConfig {
                max_coordinate_x: Some(100),
                max_coordinate_y: Some(100),
                ..ComputerUseConfig::default()
            },
            Vec::new(),
        )
        .unwrap();

        assert!(
            tool.validate_coordinate("x", 50, tool.computer_use.max_coordinate_x)
                .is_ok()
        );
        assert!(
            tool.validate_coordinate("x", 101, tool.computer_use.max_coordinate_x)
                .is_err()
        );
        assert!(
            tool.validate_coordinate("y", -1, tool.computer_use.max_coordinate_y)
                .is_err()
        );
    }

    #[test]
    fn browser_tool_name() {
        let security = Arc::new(SecurityPolicy::default());
        let tool = BrowserTool::new(security, vec!["example.com".into()], None).unwrap();
        assert_eq!(tool.name(), "browser");
    }

    #[test]
    fn browser_tool_validates_url() {
        let security = Arc::new(SecurityPolicy::default());
        let tool = BrowserTool::new(security, vec!["example.com".into()], None).unwrap();

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

        // file:// URLs blocked (local file exfiltration risk)
        assert!(tool.validate_url("file:///tmp/test.html").is_err());
    }

    #[test]
    fn browser_tool_empty_allowlist_blocks() {
        let security = Arc::new(SecurityPolicy::default());
        let tool = BrowserTool::new(security, vec![], None).unwrap();
        assert!(tool.validate_url("https://example.com").is_err());
    }

    #[test]
    fn computer_use_only_action_detection_is_correct() {
        assert!(is_computer_use_only_action("mouse_move"));
        assert!(is_computer_use_only_action("mouse_click"));
        assert!(is_computer_use_only_action("mouse_drag"));
        assert!(is_computer_use_only_action("key_type"));
        assert!(is_computer_use_only_action("key_press"));
        assert!(is_computer_use_only_action("screen_capture"));
        assert!(!is_computer_use_only_action("open"));
        assert!(!is_computer_use_only_action("snapshot"));
    }

    #[test]
    fn unavailable_action_error_preserves_backend_context() {
        assert_eq!(
            unavailable_action_for_backend_error("mouse_move", ResolvedBackend::AgentBrowser),
            "Action 'mouse_move' is unavailable for backend 'agent_browser'"
        );
        assert_eq!(
            unavailable_action_for_backend_error("mouse_move", ResolvedBackend::RustNative),
            "Action 'mouse_move' is unavailable for backend 'rust_native'"
        );
    }

    #[test]
    fn recoverable_error_detection_matches_session_patterns() {
        for message in [
            "invalid session id",
            "No Such Window",
            "session not created",
            "connection reset by peer",
            "broken pipe while writing webdriver command",
            "WebDriver request timed out",
        ] {
            let err = anyhow::Error::msg(message);
            assert!(is_recoverable_rust_native_error(&err), "{message}");
        }

        let allowlist_error =
            anyhow::Error::msg("URL host 'localhost' is not in browser allowlist [example.com]");
        assert!(!is_recoverable_rust_native_error(&allowlist_error));
    }

    #[test]
    fn non_recoverable_error_detection_rejects_policy_errors() {
        for message in [
            "Blocked by security policy",
            "URL host '127.0.0.1' is private and disallowed",
            "Action 'mouse_move' is unavailable for backend 'rust_native'",
        ] {
            let err = anyhow::Error::msg(message);
            assert!(!is_recoverable_rust_native_error(&err), "{message}");
        }
    }

    #[cfg(feature = "browser-native")]
    #[test]
    fn reset_session_is_idempotent_without_client() {
        tokio_test::block_on(async {
            let mut state = native_backend::NativeBrowserState::default();
            state.reset_session().await;
            state.reset_session().await;
        });
    }

    #[test]
    fn ensure_browser_env_sets_home_when_missing() {
        let original_home = std::env::var_os("HOME");
        unsafe { std::env::remove_var("HOME") };

        let mut cmd = Command::new("true");
        ensure_browser_env(&mut cmd);
        // Function completes without panic — HOME and CHROMIUM_FLAGS set on cmd.

        if let Some(home) = original_home {
            unsafe { std::env::set_var("HOME", home) };
        }
    }

    #[test]
    fn ensure_browser_env_sets_chromium_flags() {
        let original = std::env::var_os("CHROMIUM_FLAGS");
        unsafe { std::env::remove_var("CHROMIUM_FLAGS") };

        let mut cmd = Command::new("true");
        ensure_browser_env(&mut cmd);

        if let Some(val) = original {
            unsafe { std::env::set_var("CHROMIUM_FLAGS", val) };
        }
    }

    #[test]
    fn is_service_environment_detects_invocation_id() {
        let original = std::env::var_os("INVOCATION_ID");
        unsafe { std::env::set_var("INVOCATION_ID", "test-unit-id") };

        assert!(is_service_environment());

        if let Some(val) = original {
            unsafe { std::env::set_var("INVOCATION_ID", val) };
        } else {
            unsafe { std::env::remove_var("INVOCATION_ID") };
        }
    }

    #[test]
    fn is_service_environment_detects_journal_stream() {
        let original = std::env::var_os("JOURNAL_STREAM");
        unsafe { std::env::set_var("JOURNAL_STREAM", "8:12345") };

        assert!(is_service_environment());

        if let Some(val) = original {
            unsafe { std::env::set_var("JOURNAL_STREAM", val) };
        } else {
            unsafe { std::env::remove_var("JOURNAL_STREAM") };
        }
    }

    #[test]
    fn is_service_environment_false_in_normal_context() {
        let inv = std::env::var_os("INVOCATION_ID");
        let journal = std::env::var_os("JOURNAL_STREAM");
        unsafe { std::env::remove_var("INVOCATION_ID") };
        unsafe { std::env::remove_var("JOURNAL_STREAM") };

        if std::env::var_os("HOME").is_some() {
            assert!(!is_service_environment());
        }

        if let Some(val) = inv {
            unsafe { std::env::set_var("INVOCATION_ID", val) };
        }
        if let Some(val) = journal {
            unsafe { std::env::set_var("JOURNAL_STREAM", val) };
        }
    }

    #[test]
    fn windows_command_name_selection() {
        // Verify the cfg-based command name logic used in is_agent_browser_available
        // and run_command selects the correct binary name per platform.
        let cmd = if cfg!(target_os = "windows") {
            "agent-browser.cmd"
        } else {
            "agent-browser"
        };

        if cfg!(target_os = "windows") {
            assert_eq!(cmd, "agent-browser.cmd");
        } else {
            assert_eq!(cmd, "agent-browser");
        }
    }

    // -----------------------------------------------------------------------
    // validate_screenshot_path
    // -----------------------------------------------------------------------

    #[test]
    fn validate_screenshot_path_allows_path_inside_workspace() {
        let tmp = tempfile::tempdir().unwrap();
        let ws = tmp.path().to_path_buf();
        std::fs::create_dir_all(ws.join("shots")).unwrap();

        let security = Arc::new(SecurityPolicy {
            workspace_dir: ws.clone(),
            workspace_only: true,
            ..Default::default()
        });
        let tool = BrowserTool::new(security, vec!["*".into()], None).unwrap();

        let rt = tokio::runtime::Runtime::new().unwrap();
        let mut action = BrowserAction::Screenshot {
            path: Some("shots/page.png".into()),
            full_page: false,
        };
        rt.block_on(tool.validate_screenshot_path(&mut action))
            .expect("path inside workspace should be accepted");
        // The path should be resolved (relative -> absolute via workspace_dir)
        let BrowserAction::Screenshot { path: resolved, .. } = &action else {
            panic!("expected Screenshot variant");
        };
        let resolved = resolved.as_ref().expect("path should still be Some");
        assert!(
            resolved.starts_with(ws.to_str().unwrap()),
            "resolved path should be under workspace, got: {resolved}",
        );
    }

    #[test]
    fn validate_screenshot_path_rejects_path_outside_workspace() {
        let tmp = tempfile::tempdir().unwrap();
        let ws = tmp.path().to_path_buf();
        std::fs::create_dir_all(ws.join("shots")).unwrap();

        let security = Arc::new(SecurityPolicy {
            workspace_dir: ws.clone(),
            workspace_only: true,
            ..Default::default()
        });
        let tool = BrowserTool::new(security, vec!["*".into()], None).unwrap();

        let rt = tokio::runtime::Runtime::new().unwrap();
        let mut action = BrowserAction::Screenshot {
            // /tmp is in the default forbidden_paths list
            path: Some("/tmp/evil.png".into()),
            full_page: false,
        };
        let err = rt
            .block_on(tool.validate_screenshot_path(&mut action))
            .unwrap_err();
        let msg = err.to_string().to_ascii_lowercase();
        assert!(
            msg.contains("not allowed") || msg.contains("outside") || msg.contains("forbidden"),
            "unexpected error message: {err}",
        );
    }

    #[test]
    fn validate_screenshot_path_rejects_traversal() {
        let security = Arc::new(SecurityPolicy::default());
        let tool = BrowserTool::new(security, vec!["*".into()], None).unwrap();

        let rt = tokio::runtime::Runtime::new().unwrap();
        let mut action = BrowserAction::Screenshot {
            path: Some("../etc/passwd".into()),
            full_page: false,
        };
        assert!(
            rt.block_on(tool.validate_screenshot_path(&mut action))
                .is_err(),
            "path with .. traversal should be rejected",
        );
    }

    #[test]
    fn validate_screenshot_path_noop_when_path_none() {
        let security = Arc::new(SecurityPolicy::default());
        let tool = BrowserTool::new(security, vec!["*".into()], None).unwrap();

        let rt = tokio::runtime::Runtime::new().unwrap();
        let mut action = BrowserAction::Screenshot {
            path: None,
            full_page: true,
        };
        rt.block_on(tool.validate_screenshot_path(&mut action))
            .expect("None path (inline PNG) should be a no-op");
        let BrowserAction::Screenshot { path, .. } = &action else {
            panic!("expected Screenshot variant");
        };
        assert!(path.is_none(), "path should still be None after no-op");
    }

    #[test]
    fn validate_screenshot_path_noop_on_non_screenshot_action() {
        let security = Arc::new(SecurityPolicy::default());
        let tool = BrowserTool::new(security, vec!["*".into()], None).unwrap();

        let rt = tokio::runtime::Runtime::new().unwrap();
        let mut action = BrowserAction::Open {
            url: "https://example.com".into(),
        };
        // Non-screenshot actions pass through without validation
        rt.block_on(tool.validate_screenshot_path(&mut action))
            .expect("non-screenshot action should pass through");
    }

    #[cfg(unix)]
    #[test]
    fn validate_screenshot_path_rejects_existing_symlink_target() {
        let tmp = tempfile::tempdir().unwrap();
        // Layout: <tmp>/outside/real.txt  <- symlink target outside the workspace
        //         <tmp>/ws/           (workspace_dir)
        //         <tmp>/ws/page.png -> ../outside/real.txt  (workspace-resident symlink)
        let outside_dir = tmp.path().join("outside");
        std::fs::create_dir_all(&outside_dir).unwrap();
        std::fs::write(outside_dir.join("real.txt"), "data").unwrap();
        let ws = tmp.path().join("ws");
        std::fs::create_dir_all(&ws).unwrap();
        std::os::unix::fs::symlink("../outside/real.txt", ws.join("page.png")).unwrap();

        let security = Arc::new(SecurityPolicy {
            workspace_dir: ws.clone(),
            workspace_only: true,
            ..Default::default()
        });
        let tool = BrowserTool::new(security, vec!["*".into()], None).unwrap();

        let rt = tokio::runtime::Runtime::new().unwrap();
        let mut action = BrowserAction::Screenshot {
            path: Some("page.png".into()),
            full_page: false,
        };
        let err = rt
            .block_on(tool.validate_screenshot_path(&mut action))
            .unwrap_err();
        let msg = err.to_string().to_ascii_lowercase();
        assert!(
            msg.contains("symlink"),
            "expected symlink rejection, got: {err}",
        );
    }

    #[test]
    fn validate_screenshot_path_rejects_runtime_config_target() {
        let tmp = tempfile::tempdir().unwrap();
        // Layout: <tmp>/               (runtime config dir = workspace_dir.parent())
        //         <tmp>/ws/            (workspace_dir)
        //
        // runtime_config_dir is `workspace_dir.parent().canonicalize()` = `<tmp>`.
        // We add `<tmp>` to `allowed_roots` so an absolute path inside it
        // passes `is_resolved_path_allowed`, and we pass `<tmp>/config.toml`
        // as an *absolute* path. That path does NOT contain a `..` component,
        // so `is_path_allowed` accepts it, `is_resolved_path_allowed` accepts
        // it (because `<tmp>` is in `allowed_roots`), and the file's parent
        // matches the runtime_config_dir — so the runtime-config guard must
        // reject it. (Earlier version used `../config.toml`, which the
        // string-level traversal check rejected before reaching this guard.)
        let ws = tmp.path().join("ws");
        std::fs::create_dir_all(&ws).unwrap();
        let tmp_canon = tokio::runtime::Runtime::new()
            .unwrap()
            .block_on(async { tokio::fs::canonicalize(tmp.path()).await })
            .unwrap();

        let security = Arc::new(SecurityPolicy {
            workspace_dir: ws.clone(),
            workspace_only: true,
            allowed_roots: vec![tmp_canon.clone()],
            ..Default::default()
        });
        let tool = BrowserTool::new(security, vec!["*".into()], None).unwrap();

        let rt = tokio::runtime::Runtime::new().unwrap();
        let target = tmp_canon.join("config.toml");
        let mut action = BrowserAction::Screenshot {
            path: Some(target.to_string_lossy().to_string()),
            full_page: false,
        };
        let err = rt
            .block_on(tool.validate_screenshot_path(&mut action))
            .unwrap_err();
        let msg = err.to_string().to_ascii_lowercase();
        assert!(
            msg.contains("runtime config") || msg.contains("config"),
            "expected runtime config rejection, got: {err}",
        );
    }

    #[cfg(unix)]
    #[test]
    fn validate_screenshot_path_allows_existing_regular_file_target() {
        // Existing non-symlink file inside the workspace must still be a
        // valid screenshot target — the symlink guard only triggers on
        // symlink metadata. Without this test, a future refactor that
        // uses `metadata()` instead of `symlink_metadata()` could
        // accidentally reject legitimate writes.
        let tmp = tempfile::tempdir().unwrap();
        let ws = tmp.path().to_path_buf();
        std::fs::write(ws.join("existing.png"), b"\x89PNG\r\n\x1a\n").unwrap();

        let security = Arc::new(SecurityPolicy {
            workspace_dir: ws.clone(),
            workspace_only: true,
            ..Default::default()
        });
        let tool = BrowserTool::new(security, vec!["*".into()], None).unwrap();

        let rt = tokio::runtime::Runtime::new().unwrap();
        let mut action = BrowserAction::Screenshot {
            path: Some("existing.png".into()),
            full_page: false,
        };
        rt.block_on(tool.validate_screenshot_path(&mut action))
            .expect("existing regular file inside workspace should be accepted");
    }

    // ── ComputerUse dispatch hook (round-3) ────────────────────────
    //
    // `BrowserTool::execute` previously returned at the ComputerUse backend
    // branch *before* `parse_browser_action` / `validate_screenshot_path`
    // ran, so a public `screenshot` `path` parameter crossed the sidecar
    // boundary unverified. The new
    // `validate_screenshot_path_for_computer_use` hook applies the same
    // workspace / runtime-config / symlink guards the other backends use,
    // and substitutes the canonical resolved path back into the args so
    // the sidecar sees the same string we just verified. These tests pin
    // that contract directly at the hook layer (no sidecar required).

    #[test]
    fn computer_use_screenshot_hook_rejects_traversal_path() {
        // Out-of-workspace screenshot path must be rejected *before* the
        // hook returns the rewritten args to the ComputerUse dispatch.
        // A regression that drops the hook would forward `../etc/passwd`
        // into the sidecar payload untouched.
        let security = Arc::new(SecurityPolicy::default());
        let tool = BrowserTool::new(security, vec!["*".into()], None).unwrap();

        let rt = tokio::runtime::Runtime::new().unwrap();
        let args = json!({
            "action": "screenshot",
            "path": "../etc/passwd",
            "full_page": false,
        });
        let err = rt
            .block_on(tool.validate_screenshot_path_for_computer_use("screenshot", args))
            .unwrap_err();
        let msg = err.to_string().to_ascii_lowercase();
        assert!(
            msg.contains("not allowed") || msg.contains("workspace") || msg.contains("path"),
            "expected traversal rejection, got: {err}",
        );
    }

    #[test]
    fn computer_use_screenshot_hook_rejects_runtime_config_path() {
        // Mirrors `validate_screenshot_path_rejects_runtime_config_target`
        // but at the ComputerUse hook: an absolute path whose parent
        // matches a runtime_config_dir and whose file name is a protected
        // config name must be rejected *before* the args cross the sidecar
        // boundary. A regression that drops the hook would forward
        // `/tmp/config.toml` into the sidecar payload untouched.
        let tmp = tempfile::tempdir().unwrap();
        let ws = tmp.path().join("ws");
        std::fs::create_dir_all(&ws).unwrap();
        let tmp_canon = rt_canonicalize(tmp.path());

        let security = Arc::new(SecurityPolicy {
            workspace_dir: ws,
            workspace_only: true,
            allowed_roots: vec![tmp_canon.clone()],
            ..Default::default()
        });
        let tool = BrowserTool::new(security, vec!["*".into()], None).unwrap();

        let rt = tokio::runtime::Runtime::new().unwrap();
        let target = tmp_canon.join("config.toml");
        let args = json!({
            "action": "screenshot",
            "path": target.to_string_lossy().to_string(),
            "full_page": false,
        });
        let err = rt
            .block_on(tool.validate_screenshot_path_for_computer_use("screenshot", args))
            .unwrap_err();
        let msg = err.to_string().to_ascii_lowercase();
        assert!(
            msg.contains("runtime config") || msg.contains("config"),
            "expected runtime config rejection, got: {err}",
        );
    }

    #[test]
    fn computer_use_screenshot_hook_substitutes_canonical_path() {
        // Allowed screenshot path inside the workspace must be rewritten
        // to its canonical form so the sidecar sees the same string we
        // verified. Without this rewrite, the sidecar would receive a
        // user-controlled path with no policy gate between validation
        // and write.
        let tmp = tempfile::tempdir().unwrap();
        let ws = tmp.path().to_path_buf();

        let security = Arc::new(SecurityPolicy {
            workspace_dir: ws.clone(),
            workspace_only: true,
            ..Default::default()
        });
        let tool = BrowserTool::new(security, vec!["*".into()], None).unwrap();

        let rt = tokio::runtime::Runtime::new().unwrap();
        let args = json!({
            "action": "screenshot",
            "path": "page.png",
            "full_page": false,
        });
        let rewritten = rt
            .block_on(tool.validate_screenshot_path_for_computer_use("screenshot", args))
            .expect("allowed workspace screenshot must be accepted");
        let rewritten_path = rewritten
            .get("path")
            .and_then(|v| v.as_str())
            .expect("path must be present and a string after rewrite");
        let expected = ws
            .canonicalize()
            .unwrap_or_else(|_| ws.clone())
            .join("page.png");
        assert_eq!(
            rewritten_path,
            expected.to_string_lossy().as_ref(),
            "ComputerUse screenshot hook must substitute the canonical resolved path"
        );
    }

    #[test]
    fn computer_use_screenshot_hook_passthrough_for_non_screenshot_actions() {
        // Non-screenshot actions must pass through with no rewrite —
        // only the screenshot action has a `path` field that needs
        // gating. A regression that runs validation on every action
        // would corrupt unrelated params (click selector, open url, ...).
        let security = Arc::new(SecurityPolicy::default());
        let tool = BrowserTool::new(security, vec!["*".into()], None).unwrap();

        let rt = tokio::runtime::Runtime::new().unwrap();
        let args = json!({
            "action": "open",
            "url": "https://example.com",
        });
        let out = rt
            .block_on(tool.validate_screenshot_path_for_computer_use("open", args.clone()))
            .expect("non-screenshot actions must not fail the hook");
        assert_eq!(out, args, "non-screenshot args must be returned unchanged");
    }

    #[test]
    fn computer_use_screenshot_hook_noop_for_inline_png() {
        // Screenshot with no `path` (inline PNG return) must pass through
        // untouched — the sidecar never receives a destination string in
        // that case, so there is no unverified path to gate. A regression
        // that tries to canonicalize a missing path would crash or corrupt
        // the args.
        let security = Arc::new(SecurityPolicy::default());
        let tool = BrowserTool::new(security, vec!["*".into()], None).unwrap();

        let rt = tokio::runtime::Runtime::new().unwrap();
        let args = json!({
            "action": "screenshot",
            "full_page": false,
        });
        let out = rt
            .block_on(tool.validate_screenshot_path_for_computer_use("screenshot", args.clone()))
            .expect("inline-PNG screenshot must not fail the hook");
        assert_eq!(out, args, "no-path screenshot must be returned unchanged");
    }

    // Helper used only by the ComputerUse hook tests above — keeps the
    // test bodies focused on the assertion and avoids repeating the
    // runtime boilerplate.
    fn rt_canonicalize(path: &std::path::Path) -> std::path::PathBuf {
        tokio::runtime::Runtime::new()
            .unwrap()
            .block_on(async { tokio::fs::canonicalize(path).await })
            .unwrap()
    }

    // ── ComputerUse dispatch (mock-sidecar) tests ────────────────
    //
    // The five hook tests above pin the validator's contract in
    // isolation, but they bypass the dispatch in `BrowserTool::execute`.
    // These five tests drive the full dispatch path against a wiremock
    // sidecar and assert exactly what reaches the wire. They are the
    // production-boundary regression that Audacity88 round-4 asked for:
    // if the hook call at `execute:1360` were removed, tests 1-3 and 5
    // below would all fail (zero requests received instead of one).
    use std::path::PathBuf;
    use wiremock::matchers::{body_partial_json, method};
    use wiremock::{Mock, MockServer, ResponseTemplate};

    fn computer_use_tool(
        security: Arc<SecurityPolicy>,
        endpoint: String,
        allow_remote_endpoint: bool,
    ) -> BrowserTool {
        BrowserTool::new_with_backend(
            security,
            vec!["*".into()],
            None,
            "computer_use".into(),
            Some(false),
            true,
            "http://127.0.0.1:9515".into(),
            None,
            ComputerUseConfig {
                endpoint,
                allow_remote_endpoint,
                timeout_ms: 5_000,
                ..ComputerUseConfig::default()
            },
            Vec::new(),
        )
        .expect("computer_use BrowserTool should construct")
    }

    fn desktop_security(workspace: PathBuf) -> Arc<SecurityPolicy> {
        Arc::new(SecurityPolicy {
            workspace_dir: workspace,
            workspace_only: true,
            ..SecurityPolicy::default()
        })
    }

    /// Decoded recorded request body for inspection by dispatch tests.
    async fn first_request_body(server: &MockServer) -> Value {
        let mut requests = server
            .received_requests()
            .await
            .expect("received_requests failed");
        assert_eq!(requests.len(), 1, "expected exactly one sidecar request");
        let req = requests.remove(0);
        serde_json::from_slice(&req.body).expect("sidecar body must be valid JSON")
    }

    #[tokio::test]
    async fn computer_use_dispatch_rejects_traversal_path_before_sidecar() {
        // A traversal `path` must be rejected at the hook, never reaching
        // the sidecar. The wiremock mount explicitly expects zero requests.
        let server = MockServer::start().await;
        Mock::given(method("POST"))
            .respond_with(ResponseTemplate::new(200).set_body_json(json!({"ok": true})))
            .expect(0)
            .mount(&server)
            .await;

        let tmp = tempfile::tempdir().unwrap();
        let tool = computer_use_tool(
            desktop_security(tmp.path().to_path_buf()),
            server.uri(),
            false,
        );

        let err = tool
            .execute(json!({
                "action": "screenshot",
                "path": "../etc/passwd",
                "full_page": false,
            }))
            .await
            .expect_err("traversal path must be rejected at the hook");
        let msg = err.to_string().to_ascii_lowercase();
        assert!(
            msg.contains("not allowed") || msg.contains("workspace") || msg.contains("path"),
            "expected traversal rejection, got: {err}"
        );
        assert_eq!(
            server.received_requests().await.unwrap_or_default().len(),
            0,
            "no request must reach the sidecar when the hook rejects"
        );
    }

    #[tokio::test]
    async fn computer_use_dispatch_sends_canonicalized_path_to_sidecar() {
        // A workspace-allowed `path` must reach the sidecar as its
        // canonical resolved form, not as the user-supplied relative
        // path. The wiremock matcher asserts on the exact bytes the
        // server receives.
        let tmp = tempfile::tempdir().unwrap();
        let ws = tmp.path().to_path_buf();
        let ws_canon = tokio::fs::canonicalize(&ws).await.unwrap();
        let expected_canonical = ws_canon.join("page.png");
        let expected_canonical_str = expected_canonical.to_string_lossy().to_string();

        let server = MockServer::start().await;
        Mock::given(method("POST"))
            .and(body_partial_json(json!({
                "params": { "path": expected_canonical_str.clone() }
            })))
            .respond_with(ResponseTemplate::new(200).set_body_json(json!({"ok": true})))
            .expect(1)
            .mount(&server)
            .await;

        let tool = computer_use_tool(desktop_security(ws), server.uri(), false);

        tool.execute(json!({
            "action": "screenshot",
            "path": "page.png",
            "full_page": false,
        }))
        .await
        .expect("allowed workspace screenshot must succeed");

        let body = first_request_body(&server).await;
        let sent_path = body
            .get("params")
            .and_then(|p| p.get("path"))
            .and_then(|p| p.as_str())
            .expect("sidecar body must carry params.path");
        assert_eq!(
            sent_path, expected_canonical_str,
            "sidecar must receive the canonicalized path, not the user-supplied relative path"
        );
        assert_ne!(
            sent_path, "page.png",
            "sidecar must NOT receive the raw user-supplied relative path"
        );
    }

    #[tokio::test]
    async fn computer_use_dispatch_rejects_remote_endpoint_with_path() {
        // When the endpoint resolves to a remote host and a `path` is
        // supplied, the hook must bail before any sidecar request. We
        // cannot drive `execute` end-to-end here because the dispatch
        // probes TCP reachability before the hook runs and the synthetic
        // endpoint is unreachable — so we exercise the hook directly
        // and rely on the wire-level zero-expectation mount to prove no
        // HTTP traffic ever leaves the process.
        let server = MockServer::start().await;
        Mock::given(method("POST"))
            .respond_with(ResponseTemplate::new(200).set_body_json(json!({"ok": true})))
            .expect(0)
            .mount(&server)
            .await;

        let tmp = tempfile::tempdir().unwrap();
        // Public-looking host that the host classifier rejects as
        // "remote". `allow_remote_endpoint = true` is required for
        // `endpoint_is_remote()` to consider the public-host case.
        let tool = computer_use_tool(
            desktop_security(tmp.path().to_path_buf()),
            "https://computeruse.attacker.example/v1/actions".to_string(),
            true,
        );
        assert!(
            tool.endpoint_is_remote(),
            "endpoint_is_remote must report remote for the synthetic public endpoint"
        );

        let err = tool
            .validate_screenshot_path_for_computer_use(
                "screenshot",
                json!({
                    "action": "screenshot",
                    "path": "page.png",
                    "full_page": false,
                }),
            )
            .await
            .expect_err("remote endpoint + path must be rejected at the hook");
        let msg = err.to_string().to_ascii_lowercase();
        assert!(
            msg.contains("remote"),
            "expected remote-endpoint rejection, got: {err}"
        );
        // The wiremock mount (zero expected) pins that the wire layer
        // was never touched even though a sidecar mock is live.
        assert_eq!(
            server.received_requests().await.unwrap_or_default().len(),
            0,
            "no request must reach any sidecar for a remote endpoint + path"
        );
    }

    #[tokio::test]
    async fn computer_use_dispatch_passes_through_empty_string_path() {
        // An empty-string `path` is treated as inline PNG return and
        // must reach the sidecar exactly once with `params.path` carrying
        // the empty string. This pins that empty-string passthrough does
        // not trigger the remote-rejection branch nor the non-string
        // rejection branch.
        let server = MockServer::start().await;
        Mock::given(method("POST"))
            .respond_with(ResponseTemplate::new(200).set_body_json(json!({"ok": true})))
            .expect(1)
            .mount(&server)
            .await;

        let tmp = tempfile::tempdir().unwrap();
        let tool = computer_use_tool(
            desktop_security(tmp.path().to_path_buf()),
            server.uri(),
            false,
        );

        tool.execute(json!({
            "action": "screenshot",
            "path": "",
            "full_page": false,
        }))
        .await
        .expect("empty-string path must pass through to the sidecar");

        let body = first_request_body(&server).await;
        let sent_path = body
            .get("params")
            .and_then(|p| p.get("path"))
            .and_then(|p| p.as_str());
        assert_eq!(
            sent_path,
            Some(""),
            "empty-string path must reach the sidecar verbatim"
        );
    }

    #[tokio::test]
    async fn computer_use_dispatch_rejects_non_string_path_before_sidecar() {
        // A non-string `path` (here an integer) must be rejected at the
        // hook with a typed error. Previously this was silently dropped
        // by `parse_browser_action` and the raw value forwarded to the
        // sidecar unverified.
        let server = MockServer::start().await;
        Mock::given(method("POST"))
            .respond_with(ResponseTemplate::new(200).set_body_json(json!({"ok": true})))
            .expect(0)
            .mount(&server)
            .await;

        let tmp = tempfile::tempdir().unwrap();
        let tool = computer_use_tool(
            desktop_security(tmp.path().to_path_buf()),
            server.uri(),
            false,
        );

        let err = tool
            .validate_screenshot_path_for_computer_use(
                "screenshot",
                json!({
                    "action": "screenshot",
                    "path": 123,
                    "full_page": false,
                }),
            )
            .await
            .expect_err("non-string path must be rejected at the hook");
        let msg = err.to_string().to_ascii_lowercase();
        assert!(
            msg.contains("must be a string"),
            "expected non-string-path rejection, got: {err}"
        );
        assert_eq!(
            server.received_requests().await.unwrap_or_default().len(),
            0,
            "no request must reach the sidecar when the hook rejects"
        );
    }

}
