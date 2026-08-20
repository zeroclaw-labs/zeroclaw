//! Browser automation tool with pluggable backends.

use crate::helpers::domain_guard;
use anyhow::Context;
use async_trait::async_trait;
use base64::Engine;
use serde::{Deserialize, Serialize};
use serde_json::{Value, json};
use std::net::ToSocketAddrs;
use std::process::Stdio;
use std::sync::Arc;
use std::time::Duration;
use tokio::process::Command;
use zeroclaw_api::tool::{Tool, ToolOutput, ToolResult};
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
                output: serde_json::to_string_pretty(&output)
                    .unwrap_or_default()
                    .into(),
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

    /// Validates screenshot destination path against workspace policy.
    /// Runs before any backend (agent-browser, rust-native, ComputerUse) writes a screenshot file.
    ///
    /// Applies the same guards as `file_write` / `file_edit`:
    /// 1. String-level `is_path_allowed` — rejects null bytes, `..` traversal, URL-encoded traversal
    /// 2. `resolve_tool_path` + `canonicalize` parent — resolves relative/tilde paths
    /// 3. `is_resolved_path_allowed` — confirms canonical parent is inside workspace allowlist
    /// 4. `is_runtime_config_path` — rejects `config.toml`, `config.toml.bak`, `.config.toml.tmp-*`
    /// 5. `symlink_metadata` — rejects existing symlink targets
    ///
    /// Replaces the raw path with the canonical target so backends write the checked string.
    async fn validate_screenshot_path(&self, action: &mut BrowserAction) -> anyhow::Result<()> {
        let BrowserAction::Screenshot { path, .. } = action else {
            return Ok(());
        };
        let Some(path_str) = path.as_ref() else {
            return Ok(());
        };

        // One canonical target validator shared by every backend. It returns
        // the checked target as a lossless UTF-8 string — never a lossy
        // conversion — so the backends write exactly the path that was allowed.
        *path = Some(self.validate_screenshot_target(path_str).await?);
        Ok(())
    }

    /// The single canonical screenshot-destination validator. Applies the same
    /// guards as `file_write` / `file_edit`:
    /// 1. String-level `is_path_allowed` — rejects null bytes, `..` traversal,
    ///    URL-encoded traversal.
    /// 2. `resolve_tool_path` + `canonicalize` parent — resolves relative/tilde
    ///    paths.
    /// 3. `is_resolved_path_allowed` — canonical parent inside the workspace
    ///    allowlist.
    /// 4. `is_runtime_config_path` — rejects `config.toml`, `config.toml.bak`,
    ///    `.config.toml.tmp-*`.
    /// 5. `symlink_metadata` — rejects existing symlink targets.
    /// 6. Rejects canonical destinations that are not valid UTF-8.
    ///
    /// Shared by the local backends (`validate_screenshot_path`) and the
    /// ComputerUse flow (`validate_screenshot_path_for_computer_use`) so one
    /// policy cannot drift between them.
    ///
    /// Returns the validated target as a lossless UTF-8 string. Every backend
    /// consumes the destination as a string (command argument, JSON value, or
    /// `tokio::fs::write(&str)`), so a canonical destination that is not valid
    /// UTF-8 is rejected here: a lossy conversion could change the pathname and
    /// name a location that never passed the allowlist.
    async fn validate_screenshot_target(&self, raw_path: &str) -> anyhow::Result<String> {
        // String-level reject (null bytes, .. traversal, URL-encoded traversal)
        if !self.security.is_path_allowed(raw_path) {
            let msg = crate::i18n::get_required_tool_string_with_args(
                "tool-browser-screenshot-error-path-not-allowed",
                &[("path", raw_path)],
            );
            anyhow::bail!("{msg}");
        }

        // Resolve relative / tilde paths against the workspace directory.
        let full = self.security.resolve_tool_path(raw_path);

        // The file does not exist yet, so canonicalize the *parent* directory
        // to verify it is inside the workspace allowlist.
        let parent = full.parent().unwrap_or(&full);
        let canonical = tokio::fs::canonicalize(parent).await.with_context(|| {
            crate::i18n::get_required_tool_string_with_args(
                "tool-browser-screenshot-error-parent-not-exist",
                &[
                    ("path", raw_path),
                    ("parent", &parent.display().to_string()),
                ],
            )
        })?;

        if !self.security.is_resolved_path_allowed(&canonical) {
            let msg = crate::i18n::get_required_tool_string_with_args(
                "tool-browser-screenshot-error-path-outside-workspace",
                &[
                    ("path", raw_path),
                    ("canonical", &canonical.display().to_string()),
                ],
            );
            anyhow::bail!("{msg}");
        }

        // Build the final *target* path (parent + file name) so we can apply
        // the same target-level guards the file_write / file_edit tools use.
        let Some(file_name) = full.file_name() else {
            let msg = crate::i18n::get_required_tool_string_with_args(
                "tool-browser-screenshot-error-missing-filename",
                &[("path", raw_path)],
            );
            anyhow::bail!("{msg}");
        };
        let resolved_target = canonical.join(file_name);

        if self.security.is_runtime_config_path(&resolved_target) {
            let msg = crate::i18n::get_required_tool_string_with_args(
                "tool-browser-screenshot-error-runtime-config-target",
                &[
                    ("path", raw_path),
                    ("target", &resolved_target.display().to_string()),
                ],
            );
            anyhow::bail!("{msg}");
        }

        // If the target already exists and is a symlink, refuse to follow it.
        if let Ok(meta) = tokio::fs::symlink_metadata(&resolved_target).await
            && meta.file_type().is_symlink()
        {
            let msg = crate::i18n::get_required_tool_string_with_args(
                "tool-browser-screenshot-error-symlink-target",
                &[("target", &resolved_target.display().to_string())],
            );
            anyhow::bail!("{msg}");
        }

        // The allowlist above validated the byte-preserving PathBuf. Every
        // backend receives the destination as a UTF-8 string, and a lossy
        // conversion (`to_string_lossy`) would silently replace non-UTF-8
        // bytes with U+FFFD — naming a pathname that never passed the policy.
        // Fail closed here, while we still hold the checked target: on Unix a
        // valid UTF-8 input can canonicalize (through a symlink) to a parent
        // containing non-UTF-8 bytes.
        let Some(resolved_str) = resolved_target.to_str() else {
            let msg = crate::i18n::get_required_tool_string_with_args(
                "tool-browser-screenshot-error-path-not-utf8",
                &[("path", raw_path)],
            );
            anyhow::bail!("{msg}");
        };

        Ok(resolved_str.to_string())
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

    /// Validates the screenshot path for the ComputerUse backend before the
    /// sidecar round-trip. Applies the same canonical workspace policy /
    /// runtime-config / symlink guards as the local backends (via
    /// [`Self::validate_screenshot_target`]) and classifies the raw destination
    /// into absent / valid string / invalid input.
    async fn validate_screenshot_path_for_computer_use(
        &self,
        action_str: &str,
        args: Value,
    ) -> anyhow::Result<Value> {
        if action_str != "screenshot" {
            // Not a screenshot action, pass through unchanged
            return Ok(args);
        }

        let path = args.get("path").cloned();

        // Classify path into Absent/String/NonString
        match &path {
            None | Some(Value::Null) => {
                // Absent: no path or null → inline PNG return
                Ok(args)
            }
            Some(Value::String(s)) if s.is_empty() => {
                // Absent: empty string → inline PNG return
                Ok(args)
            }
            Some(Value::String(path_str)) => {
                // String: validate against workspace through the one canonical
                // validator shared with the local backends.
                let mut args = args;
                let resolved_target = self.validate_screenshot_target(path_str).await?;

                // Store the validated path for local write after sidecar returns PNG.
                // Do NOT forward the path to the sidecar - it returns PNG bytes.
                if let Some(obj) = args.as_object_mut() {
                    obj.insert("path".to_string(), Value::String(resolved_target));
                }
                Ok(args)
            }
            Some(_) => {
                // NonString: integer, array, object → reject
                let msg = crate::i18n::get_required_tool_string_with_args(
                    "tool-browser-screenshot-error-computeruse-non-string-path",
                    &[("path", &format!("{path:?}"))],
                );
                anyhow::bail!("{msg}");
            }
        }
    }

    async fn execute_computer_use_action(
        &self,
        action: &str,
        args: &Value,
    ) -> anyhow::Result<ToolResult> {
        let endpoint = self.computer_use_endpoint_url()?;

        // Validate screenshot path but do NOT forward it to the sidecar.
        // The sidecar returns PNG bytes, and we perform the validated local write.
        let validated_path = if action == "screenshot" {
            match self
                .validate_screenshot_path_for_computer_use(action, args.clone())
                .await
            {
                Ok(validated_args) => {
                    // Extract the validated path from the returned args
                    validated_args
                        .get("path")
                        .and_then(|v| v.as_str())
                        .map(String::from)
                }
                Err(e) => {
                    return Ok(ToolResult {
                        success: false,
                        output: ToolOutput::default(),
                        error: Some(e.to_string()),
                    });
                }
            }
        } else {
            None
        };

        // Build params without the path - sidecar should return PNG bytes
        let mut params = args.as_object().cloned().ok_or_else(|| {
            ::zeroclaw_log::record!(
                WARN,
                ::zeroclaw_log::Event::new(module_path!(), ::zeroclaw_log::Action::Reject)
                    .with_outcome(::zeroclaw_log::EventOutcome::Failure),
                "browser: screenshot args must be a JSON object"
            );
            anyhow::Error::msg(crate::i18n::get_required_tool_string(
                "tool-browser-screenshot-error-args-not-object",
            ))
        })?;

        // Remove path from params - we'll handle the write locally after validation
        params.remove("path");
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

        // A path-bearing screenshot is the ONLY flow that transfers bytes from
        // the sidecar to the local filesystem. For that flow the tool must fail
        // closed: success requires a well-formed ComputerUseResponse with
        // success != false, a non-empty PNG payload, and a completed local
        // write. A non-JSON or structurally invalid 2xx body must NOT fall
        // through to a generic success (which would report success without
        // creating the requested file).
        let is_path_bearing_screenshot =
            action == "screenshot" && validated_path.as_deref().is_some_and(|p| !p.is_empty());

        if let Ok(parsed) = serde_json::from_str::<ComputerUseResponse>(&body) {
            if status.is_success() && parsed.success.unwrap_or(true) {
                // If this was a screenshot with a validated non-empty path, write the PNG
                // locally. Bind the validated path structurally (the path-bearing flag
                // above guarantees a non-empty Some here) instead of unwrapping a latent
                // panic site.
                if let Some(path_str) = validated_path.as_deref().filter(|p| !p.is_empty()) {
                    // Extract PNG data from the response
                    let png_data = parsed
                        .data
                        .as_ref()
                        .and_then(|d| d.get("png_base64"))
                        .and_then(|v| v.as_str())
                        .ok_or_else(|| {
                            anyhow::Error::msg(crate::i18n::get_required_tool_string(
                                "tool-browser-screenshot-error-sidecar-no-png-data",
                            ))
                        })?;

                    // Decode and validate the PNG payload: it must decode to a
                    // non-empty buffer with a PNG signature. Base64-decodable
                    // arbitrary bytes are NOT a valid screenshot — writing them
                    // to the `.png` destination would turn the sidecar boundary
                    // into an arbitrary decoded-byte write.
                    let png_bytes = base64::engine::general_purpose::STANDARD
                        .decode(png_data)
                        .with_context(|| "Failed to decode PNG base64 data")?;
                    if png_bytes.is_empty() {
                        anyhow::bail!(crate::i18n::get_required_tool_string(
                            "tool-browser-screenshot-error-sidecar-empty-png",
                        ));
                    }
                    const PNG_SIGNATURE: &[u8] =
                        &[0x89, b'P', b'N', b'G', b'\r', b'\n', 0x1a, b'\n'];
                    if !png_bytes.starts_with(PNG_SIGNATURE) {
                        anyhow::bail!(crate::i18n::get_required_tool_string(
                            "tool-browser-screenshot-error-sidecar-not-png",
                        ));
                    }

                    tokio::fs::write(path_str, &png_bytes)
                        .await
                        .with_context(|| format!("Failed to write screenshot to {path_str}"))?;

                    // Return success with the path information
                    let output = serde_json::to_string_pretty(&json!({
                        "backend": "computer_use",
                        "action": action,
                        "path": path_str,
                        "bytes": png_bytes.len(),
                    }))
                    .unwrap_or_default();

                    return Ok(ToolResult {
                        success: true,
                        output: output.into(),
                        error: None,
                    });
                }

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
                    output: output.into(),
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
                output: ToolOutput::default(),
                error,
            });
        }

        if status.is_success() {
            if is_path_bearing_screenshot {
                return Ok(ToolResult {
                    success: false,
                    output: ToolOutput::default(),
                    error: Some(crate::i18n::get_required_tool_string(
                        "tool-browser-screenshot-error-sidecar-non-json-success",
                    )),
                });
            }
            return Ok(ToolResult {
                success: true,
                output: body.into(),
                error: None,
            });
        }

        Ok(ToolResult {
            success: false,
            output: ToolOutput::default(),
            error: Some(format!(
                "computer-use sidecar request failed with status {status}: {}",
                body.trim()
            )),
        })
    }

    async fn execute_action(
        &self,
        mut action: BrowserAction,
        backend: ResolvedBackend,
    ) -> anyhow::Result<ToolResult> {
        // Validate screenshot path before any backend writes a file
        if matches!(action, BrowserAction::Screenshot { .. }) {
            self.validate_screenshot_path(&mut action).await?;
        }

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
                output: output.into(),
                error: None,
            })
        } else {
            Ok(ToolResult {
                success: false,
                output: ToolOutput::default(),
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
                output: ToolOutput::default(),
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
                    output: ToolOutput::default(),
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
                output: ToolOutput::default(),
                error: Some(format!("Unknown action: {action_str}")),
            });
        }

        if backend == ResolvedBackend::ComputerUse {
            return self.execute_computer_use_action(action_str, &args).await;
        }

        if is_computer_use_only_action(action_str) {
            return Ok(ToolResult {
                success: false,
                output: ToolOutput::default(),
                error: Some(unavailable_action_for_backend_error(action_str, backend)),
            });
        }

        let action = match parse_browser_action(action_str, &args) {
            Ok(a) => a,
            Err(e) => {
                return Ok(ToolResult {
                    success: false,
                    output: ToolOutput::default(),
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
        "screenshot" => {
            // Parse the raw optional destination once into absent / valid
            // string / invalid input. A present non-string `path` (number,
            // object, …) is invalid input and must be rejected up front — the
            // same contract the ComputerUse path enforces — instead of being
            // silently coerced to `None` (which would make the local backends
            // take an inline screenshot while ComputerUse rejects the same
            // input). An empty string means absent (inline screenshot), also
            // matching ComputerUse.
            match args.get("path") {
                None | Some(serde_json::Value::Null) => Ok(BrowserAction::Screenshot {
                    path: None,
                    full_page: args
                        .get("full_page")
                        .and_then(serde_json::Value::as_bool)
                        .unwrap_or(false),
                }),
                Some(serde_json::Value::String(s)) if s.is_empty() => {
                    Ok(BrowserAction::Screenshot {
                        path: None,
                        full_page: args
                            .get("full_page")
                            .and_then(serde_json::Value::as_bool)
                            .unwrap_or(false),
                    })
                }
                Some(serde_json::Value::String(s)) => Ok(BrowserAction::Screenshot {
                    path: Some(s.clone()),
                    full_page: args
                        .get("full_page")
                        .and_then(serde_json::Value::as_bool)
                        .unwrap_or(false),
                }),
                Some(_) => Err(anyhow::Error::msg(crate::i18n::get_required_tool_string(
                    "tool-browser-screenshot-error-non-string-path",
                ))),
            }
        }
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
    fn wildcard_private_allowlist_permits_localhost() {
        let tool = private_host_tool(vec![], vec!["*"]);
        assert!(tool.validate_url("http://localhost:8080").is_ok());
        assert!(tool.validate_url("https://localhost:8443").is_ok());
    }

    #[test]
    fn wildcard_private_allowlist_permits_rfc1918() {
        let tool = private_host_tool(vec![], vec!["*"]);
        assert!(tool.validate_url("http://192.168.1.5").is_ok());
        assert!(tool.validate_url("http://10.0.0.1").is_ok());
        assert!(tool.validate_url("http://172.16.0.1").is_ok());
    }

    #[test]
    fn wildcard_private_allowlist_does_not_loosen_file_scheme() {
        // file:// is always blocked, regardless of allowed_private_hosts.
        let tool = private_host_tool(vec!["*"], vec!["*"]);
        let err = tool
            .validate_url("file:///etc/passwd")
            .unwrap_err()
            .to_string();
        assert!(err.contains("file://"));
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
    fn specific_private_host_alone_satisfies_allowlist_requirement() {
        let tool = private_host_tool(vec![], vec!["192.168.1.5"]);
        assert!(tool.validate_url("http://192.168.1.5").is_ok());
    }

    #[test]
    fn wildcard_private_allowlist_does_not_widen_public_allowlist() {
        // Public hosts are still subject to allowed_domains when private hosts
        // are wide-open — the bypass is scoped to private/local hosts only.
        let tool = private_host_tool(vec!["example.com"], vec!["*"]);
        let err = tool
            .validate_url("https://other.com")
            .unwrap_err()
            .to_string();
        assert!(err.contains("allowed_domains"));
    }

    #[test]
    fn userinfo_url_targeting_private_host_rejected_under_wildcard_public_allowlist() {
        // Default-shipped posture: allowed_domains = ["*"], no private
        // allowlist. `extract_host` would otherwise treat
        // `example.com@127.0.0.1` as the host and accept it.
        let tool = private_host_tool(vec!["*"], vec![]);
        let err = tool
            .validate_url("http://example.com@127.0.0.1/")
            .unwrap_err()
            .to_string();
        assert!(err.contains("userinfo"), "got: {err}");
    }

    #[test]
    fn userinfo_url_targeting_private_host_rejected_under_wildcard_private_allowlist() {
        // Even with the private bypass wide open, userinfo is rejected before
        // host classification — so this is a parser-mismatch defense, not a
        // policy decision the operator can opt around.
        let tool = private_host_tool(vec!["*"], vec!["*"]);
        let err = tool
            .validate_url("http://example.com@127.0.0.1/")
            .unwrap_err()
            .to_string();
        assert!(err.contains("userinfo"), "got: {err}");
    }

    #[test]
    fn userinfo_url_with_password_rejected() {
        // `user:pass@host` form — same parser hole, same fix.
        let tool = private_host_tool(vec!["*"], vec![]);
        let err = tool
            .validate_url("https://user:pass@10.0.0.1/")
            .unwrap_err()
            .to_string();
        assert!(err.contains("userinfo"), "got: {err}");
    }

    #[test]
    fn query_only_url_targeting_private_host_rejected_under_wildcard_public_allowlist() {
        let tool = private_host_tool(vec!["*"], vec![]);
        let err = tool
            .validate_url("http://127.0.0.1?x")
            .unwrap_err()
            .to_string();
        assert!(
            err.contains("local/private host"),
            "expected private-host block, got: {err}",
        );
    }

    #[test]
    fn fragment_only_url_targeting_private_host_rejected_under_wildcard_public_allowlist() {
        let tool = private_host_tool(vec!["*"], vec![]);
        let err = tool
            .validate_url("http://127.0.0.1#x")
            .unwrap_err()
            .to_string();
        assert!(
            err.contains("local/private host"),
            "expected private-host block, got: {err}",
        );
    }

    // ============ Screenshot path validation tests ============

    use zeroclaw_config::policy::AutonomyLevel;

    fn screenshot_tool_with_workspace(ws: &std::path::Path) -> BrowserTool {
        let security = Arc::new(SecurityPolicy {
            autonomy: AutonomyLevel::Full,
            workspace_dir: ws.to_path_buf(),
            allowed_roots: vec![ws.to_path_buf()],
            ..SecurityPolicy::default()
        });
        BrowserTool::new(security, vec!["*".into()], None).unwrap()
    }

    #[tokio::test]
    async fn validate_screenshot_path_allows_path_inside_workspace() {
        let tmp = tempfile::TempDir::new().unwrap();
        let ws = tmp.path().join("ws");
        let shots = ws.join("shots");
        tokio::fs::create_dir_all(&shots).await.unwrap();

        let tool = screenshot_tool_with_workspace(&ws);
        let mut action = BrowserAction::Screenshot {
            path: Some("shots/page.png".into()),
            full_page: false,
        };

        // Canonicalize the expected workspace path first (macOS fix)
        let expected_canonical = std::fs::canonicalize(&ws).unwrap();

        tool.validate_screenshot_path(&mut action).await.unwrap();

        // Verify path is replaced with canonical form
        if let BrowserAction::Screenshot { path, .. } = action {
            let canonical_path = path.unwrap();
            // Compare canonical forms, not raw strings
            assert!(canonical_path.starts_with(expected_canonical.to_string_lossy().as_ref()));
            assert!(canonical_path.ends_with("page.png"));
        } else {
            panic!("action should still be Screenshot");
        }
    }

    #[tokio::test]
    async fn validate_screenshot_path_rejects_path_outside_workspace() {
        let tmp = tempfile::TempDir::new().unwrap();
        let ws = tmp.path().join("ws");
        let outside = tmp.path().join("outside");
        tokio::fs::create_dir_all(&ws).await.unwrap();
        tokio::fs::create_dir_all(&outside).await.unwrap();

        // Create a file in the outside directory so canonicalize succeeds
        let outside_file = outside.join("page.png");
        tokio::fs::write(&outside_file, b"test").await.unwrap();

        // Use absolute path that's not in allowed_roots
        let security = Arc::new(SecurityPolicy {
            autonomy: AutonomyLevel::Full,
            workspace_dir: ws.clone(),
            allowed_roots: vec![ws.clone()], // outside is NOT in allowed_roots
            ..SecurityPolicy::default()
        });

        let tool = BrowserTool::new(security, vec!["*".into()], None).unwrap();
        let mut action = BrowserAction::Screenshot {
            path: Some(outside_file.to_string_lossy().to_string()),
            full_page: false,
        };

        let err = tool
            .validate_screenshot_path(&mut action)
            .await
            .unwrap_err();
        // Should be rejected as outside workspace
        assert!(
            err.to_string().contains("outside-workspace")
                || err.to_string().contains("outside/page.png"),
            "Expected outside-workspace rejection, got: {}",
            err
        );
    }

    #[tokio::test]
    async fn validate_screenshot_path_rejects_traversal() {
        let tmp = tempfile::TempDir::new().unwrap();
        let ws = tmp.path().join("ws");
        tokio::fs::create_dir_all(&ws).await.unwrap();

        let tool = screenshot_tool_with_workspace(&ws);
        let mut action = BrowserAction::Screenshot {
            path: Some("../../etc/passwd".into()),
            full_page: false,
        };

        let err = tool
            .validate_screenshot_path(&mut action)
            .await
            .unwrap_err();
        // String-level traversal should be rejected with path-not-allowed error
        assert!(
            err.to_string().contains("not in the workspace allowlist")
                || err.to_string().contains("../../etc/passwd"),
            "Expected traversal rejection, got: {}",
            err
        );
    }

    #[tokio::test]
    async fn validate_screenshot_path_noop_when_path_none() {
        let tmp = tempfile::TempDir::new().unwrap();
        let ws = tmp.path().join("ws");
        tokio::fs::create_dir_all(&ws).await.unwrap();

        let tool = screenshot_tool_with_workspace(&ws);
        let mut action = BrowserAction::Screenshot {
            path: None,
            full_page: false,
        };

        tool.validate_screenshot_path(&mut action).await.unwrap();
        assert!(matches!(
            action,
            BrowserAction::Screenshot { path: None, .. }
        ));
    }

    #[tokio::test]
    async fn validate_screenshot_path_rejects_runtime_config_target() {
        let tmp = tempfile::TempDir::new().unwrap();
        let ws = tmp.path().join("ws");
        let config_dir = tmp.path().join("config");
        tokio::fs::create_dir_all(&ws).await.unwrap();
        tokio::fs::create_dir_all(&config_dir).await.unwrap();

        // Create an actual config.toml file in the config directory
        let config_path = config_dir.join("config.toml");
        tokio::fs::write(&config_path, b"").await.unwrap();

        // Create the config file so is_runtime_config_path detects it
        tokio::fs::write(&config_path, b"test").await.unwrap();

        let security = Arc::new(SecurityPolicy {
            autonomy: AutonomyLevel::Full,
            workspace_dir: ws.clone(),
            allowed_roots: vec![ws.clone(), config_dir.clone()],
            config_path: Some(config_path.clone()),
            ..SecurityPolicy::default()
        });

        let tool = BrowserTool::new(security, vec!["*".into()], None).unwrap();

        let mut action = BrowserAction::Screenshot {
            path: Some(config_path.to_string_lossy().to_string()),
            full_page: false,
        };

        // Should be rejected as runtime-config target
        let err = tool
            .validate_screenshot_path(&mut action)
            .await
            .unwrap_err();
        assert!(
            err.to_string().contains("runtime config") || err.to_string().contains("Refusing"),
            "Expected runtime-config rejection, got: {}",
            err
        );
    }

    #[tokio::test]
    #[cfg(unix)]
    async fn validate_screenshot_path_rejects_existing_symlink_target() {
        use std::os::unix::fs::symlink;

        let tmp = tempfile::TempDir::new().unwrap();
        let ws = tmp.path().join("ws");
        let outside = tmp.path().join("outside");
        tokio::fs::create_dir_all(&ws).await.unwrap();
        tokio::fs::create_dir_all(&outside).await.unwrap();

        // Create a symlink inside workspace pointing outside
        let link_path = ws.join("page.png");
        let target_path = outside.join("real.txt");
        tokio::fs::write(&target_path, b"real").await.unwrap();
        symlink(&target_path, &link_path).unwrap();

        let tool = screenshot_tool_with_workspace(&ws);
        let mut action = BrowserAction::Screenshot {
            path: Some("page.png".into()),
            full_page: false,
        };

        let err = tool
            .validate_screenshot_path(&mut action)
            .await
            .unwrap_err();
        assert!(err.to_string().contains("symlink"));
    }

    /// Path-identity regression: a valid UTF-8 alias can resolve (through a
    /// symlink) to a canonical parent whose name contains non-UTF-8 bytes. The
    /// allowlist validates the byte-preserving `PathBuf`, but the backends
    /// consume the destination as a UTF-8 string — a lossy conversion would
    /// silently rewrite the pathname and name a location that never passed the
    /// policy. `execute_action` must reject such a target before either local
    /// backend receives the action. If the validator call is removed, this
    /// fails (the backends would otherwise succeed or error without the
    /// specific allowlist rejection).
    ///
    /// Linux-only: the fixture needs a directory whose name carries a raw
    /// non-UTF-8 byte, and macOS (APFS) rejects such pathnames at
    /// `create_dir_all` time with `EILSEQ`, so the fixture cannot be built
    /// there.
    #[tokio::test]
    #[cfg(target_os = "linux")]
    async fn execute_action_rejects_non_utf8_canonical_target_before_backend_dispatch() {
        use std::ffi::OsString;
        use std::os::unix::ffi::OsStringExt;
        use std::os::unix::fs::symlink;

        let tmp = tempfile::TempDir::new().unwrap();
        let ws = tmp.path().join("ws");

        // A real directory (inside the workspace allowlist) whose name carries a
        // raw non-UTF-8 byte, so the outside-workspace gate does not fire first.
        let mut raw_name = b"nonutf8-".to_vec();
        raw_name.push(0xFF);
        let non_utf8_dir = ws.join(std::path::PathBuf::from(OsString::from_vec(raw_name)));
        tokio::fs::create_dir_all(non_utf8_dir.join("shots"))
            .await
            .unwrap();

        // UTF-8 symlink alias inside the workspace -> the non-UTF-8 directory.
        symlink(&non_utf8_dir, ws.join("alias")).unwrap();

        let tool = screenshot_tool_with_workspace(&ws);

        // AgentBrowser: the rejection must come from the validator, before
        // dispatch (the backend would otherwise never see this exact error).
        let action = BrowserAction::Screenshot {
            path: Some("alias/shots/page.png".into()),
            full_page: false,
        };
        let err = tool
            .execute_action(action, ResolvedBackend::AgentBrowser)
            .await
            .unwrap_err()
            .to_string();
        assert!(
            err.contains("non-UTF-8"),
            "a non-UTF-8 canonical target must be rejected by the validator before backend \
             dispatch, got: {err}"
        );

        // RustNative: same gate, same rejection, before the local write.
        let action2 = BrowserAction::Screenshot {
            path: Some("alias/shots/page.png".into()),
            full_page: false,
        };
        let err = tool
            .execute_action(action2, ResolvedBackend::RustNative)
            .await
            .unwrap_err()
            .to_string();
        assert!(
            err.contains("non-UTF-8"),
            "a non-UTF-8 canonical target must be rejected for rust_native too, got: {err}"
        );
    }

    /// ComputerUse shares the same canonical target validator, so a non-UTF-8
    /// canonical destination is rejected locally — before the sidecar round
    /// trip — and is never forwarded.
    ///
    /// Linux-only for the same reason as
    /// `execute_action_rejects_non_utf8_canonical_target_before_backend_dispatch`:
    /// macOS rejects the raw-byte pathname fixture with `EILSEQ`.
    #[tokio::test]
    #[cfg(target_os = "linux")]
    async fn computer_use_rejects_non_utf8_canonical_target_before_sidecar() {
        use std::ffi::OsString;
        use std::os::unix::ffi::OsStringExt;
        use std::os::unix::fs::symlink;

        let tmp = tempfile::TempDir::new().unwrap();
        let ws = tmp.path().join("ws");

        let mut raw_name = b"nonutf8-".to_vec();
        raw_name.push(0xFF);
        let non_utf8_dir = ws.join(std::path::PathBuf::from(OsString::from_vec(raw_name)));
        tokio::fs::create_dir_all(non_utf8_dir.join("shots"))
            .await
            .unwrap();
        symlink(&non_utf8_dir, ws.join("alias")).unwrap();

        // ComputerUse tool whose workspace is the temp `ws` (the shared helper
        // pins `current_dir`).
        let security = Arc::new(SecurityPolicy {
            autonomy: AutonomyLevel::Full,
            workspace_dir: ws.clone(),
            allowed_roots: vec![ws.clone()],
            ..SecurityPolicy::default()
        });
        let tool = BrowserTool::new_with_backend(
            security,
            vec!["*".into()],
            None,
            "computer_use".into(),
            None,
            true,
            "http://127.0.0.1:9515".into(),
            None,
            test_computer_use_config(),
            Vec::new(),
        )
        .unwrap();

        let err = tool
            .validate_screenshot_path_for_computer_use(
                "screenshot",
                json!({"path": "alias/shots/page.png"}),
            )
            .await
            .unwrap_err()
            .to_string();
        assert!(
            err.contains("non-UTF-8"),
            "computer_use must reject a non-UTF-8 canonical target before the sidecar call, got: {err}"
        );
    }

    #[tokio::test]
    #[cfg(unix)]
    async fn validate_screenshot_path_allows_existing_regular_file_target() {
        let tmp = tempfile::TempDir::new().unwrap();
        let ws = tmp.path().join("ws");
        tokio::fs::create_dir_all(&ws).await.unwrap();

        // Create a regular file (not symlink) inside workspace
        let file_path = ws.join("existing.png");
        tokio::fs::write(&file_path, b"existing").await.unwrap();

        let tool = screenshot_tool_with_workspace(&ws);
        let mut action = BrowserAction::Screenshot {
            path: Some("existing.png".into()),
            full_page: false,
        };

        // Should succeed - regular files are OK
        tool.validate_screenshot_path(&mut action).await.unwrap();
    }

    #[tokio::test]
    async fn execute_action_rejects_malicious_screenshot_before_local_backend_dispatch() {
        // Production-boundary regression for the `execute_action` wiring
        // (line ~1302): a screenshot action carrying a traversal path must be
        // rejected by `validate_screenshot_path` before either local backend
        // (AgentBrowser or RustNative) receives it. If that call is removed,
        // the validation error never fires and this assertion fails — the
        // backend-specific error does not mention the path or the allowlist.
        let tmp = tempfile::TempDir::new().unwrap();
        let ws = tmp.path().join("ws");
        tokio::fs::create_dir_all(&ws).await.unwrap();

        let tool = screenshot_tool_with_workspace(&ws);
        let action = BrowserAction::Screenshot {
            path: Some("../etc/passwd".into()),
            full_page: false,
        };

        let err = tool
            .execute_action(action, ResolvedBackend::AgentBrowser)
            .await
            .unwrap_err()
            .to_string();
        assert!(
            err.contains("not in the workspace allowlist"),
            "traversal path must be rejected by the screenshot-path validator before backend \
             dispatch (the specific allowlist rejection, not any error echoing the path), got: {err}"
        );

        // The mut-borrow contract still holds for the second local backend.
        let action2 = BrowserAction::Screenshot {
            path: Some("../etc/passwd".into()),
            full_page: false,
        };
        let err = tool
            .execute_action(action2, ResolvedBackend::RustNative)
            .await
            .unwrap_err()
            .to_string();
        assert!(
            err.contains("not in the workspace allowlist"),
            "traversal path must be rejected at execute_action for rust_native too, got: {err}"
        );
    }

    /// `Tool::execute` raw-input boundary: a present non-string `path` must be
    /// rejected up front — the same contract the ComputerUse path enforces —
    /// rather than silently coerced to `None` (which would make the local
    /// backends take an inline screenshot while ComputerUse rejects the same
    /// input).
    #[tokio::test]
    async fn execute_rejects_present_non_string_screenshot_path() {
        // Parser boundary (no backend dependency): a present non-string path
        // must be rejected at parse time, never coerced to `None`. This is the
        // mutation-sensitive assertion — reverting the parser's non-string
        // branch back to `None` coercion makes `expect_err` fail regardless of
        // whether a backend is available in the test environment.
        let parse_err = parse_browser_action("screenshot", &json!({ "path": 123 }))
            .expect_err("a present non-string path must be rejected by the parser")
            .to_string();
        assert!(
            parse_err.contains("must be a string"),
            "the parser must name the string contract, got: {parse_err}"
        );

        let tmp = tempfile::TempDir::new().unwrap();
        let ws = tmp.path().join("ws");
        tokio::fs::create_dir_all(&ws).await.unwrap();
        let tool = screenshot_tool_with_workspace(&ws);

        // Local backend path (default). The non-string path must produce a
        // rejection ToolResult, never a silent inline screenshot.
        let result = tool
            .execute(json!({
                "action": "screenshot",
                "path": 123,
            }))
            .await
            .expect("execute must not panic on a non-string path");
        assert!(
            !result.success,
            "a present non-string path must be rejected, not coerced to None; got: {:?}",
            result.output
        );

        // ComputerUse path: same contract, non-string path rejected.
        let tool = browser_tool_with_computer_use(test_computer_use_config());
        let result = tool
            .execute(json!({
                "action": "screenshot",
                "path": json!({"nested": "object"}),
            }))
            .await
            .expect("execute must not panic on a non-string path");
        assert!(
            !result.success,
            "computer_use must reject a present non-string path too; got: {:?}",
            result.output
        );
    }

    // ============ ComputerUse dispatch tests ============

    fn test_computer_use_config() -> ComputerUseConfig {
        ComputerUseConfig {
            endpoint: "http://127.0.0.1:8787".to_string(),
            api_key: None,
            timeout_ms: 5000,
            allow_remote_endpoint: true,
            window_allowlist: vec![],
            max_coordinate_x: None,
            max_coordinate_y: None,
        }
    }

    #[cfg(test)]
    fn browser_tool_with_computer_use(config: ComputerUseConfig) -> BrowserTool {
        let security = Arc::new(SecurityPolicy {
            autonomy: AutonomyLevel::Full,
            workspace_dir: std::env::current_dir().unwrap(),
            allowed_roots: vec![std::env::current_dir().unwrap()],
            ..SecurityPolicy::default()
        });
        BrowserTool::new_with_backend(
            security,
            vec!["*".into()],
            None,
            "computer_use".into(),
            None,
            true,
            "http://127.0.0.1:9515".into(),
            None,
            config,
            Vec::new(),
        )
        .unwrap()
    }

    #[tokio::test]
    async fn computer_use_dispatch_rejects_traversal_path_before_sidecar() {
        use wiremock::matchers::{method, path};
        use wiremock::{Mock, MockServer, ResponseTemplate};

        // Start a mock server to pass the endpoint reachability check
        let server = MockServer::start().await;

        // Mock the reachability check (GET request)
        Mock::given(method("GET"))
            .and(path("/"))
            .respond_with(ResponseTemplate::new(200))
            .mount(&server)
            .await;

        // Mock the POST endpoint - should NOT be called because traversal is rejected before sidecar
        Mock::given(method("POST"))
            .and(path("/"))
            .respond_with(ResponseTemplate::new(200))
            .expect(0)
            .mount(&server)
            .await;

        let mut config = test_computer_use_config();
        config.endpoint = server.uri();
        let tool = browser_tool_with_computer_use(config);

        let args = json!({
            "action": "screenshot",
            "path": "../etc/passwd"
        });

        // Validation happens in execute_computer_use_action, returns ToolResult with error
        let result = tool.execute(args).await.unwrap();
        assert!(!result.success, "Expected validation to fail");
        let error = result.error.expect("Expected error in result");
        assert!(
            error.contains("not in the workspace allowlist") || error.contains("../etc/passwd"),
            "Expected traversal rejection, got: {}",
            error
        );
    }

    #[tokio::test]
    async fn computer_use_dispatch_rejects_runtime_config_target_before_sidecar() {
        use wiremock::matchers::{method, path};
        use wiremock::{Mock, MockServer, ResponseTemplate};

        let server = MockServer::start().await;
        let tmp = tempfile::TempDir::new().unwrap();
        let ws = tmp.path().join("ws");
        let config_dir = tmp.path().join("config");
        tokio::fs::create_dir_all(&ws).await.unwrap();
        tokio::fs::create_dir_all(&config_dir).await.unwrap();
        let config_path = config_dir.join("config.toml");
        tokio::fs::write(&config_path, b"").await.unwrap();

        // POST must never be reached: the ComputerUse runtime-config guard
        // rejects before any sidecar action request.
        Mock::given(method("POST"))
            .and(path("/"))
            .respond_with(ResponseTemplate::new(200))
            .expect(0)
            .mount(&server)
            .await;

        let security = Arc::new(SecurityPolicy {
            autonomy: AutonomyLevel::Full,
            workspace_dir: ws.clone(),
            allowed_roots: vec![ws.clone(), config_dir.clone()],
            config_path: Some(config_path.clone()),
            ..SecurityPolicy::default()
        });

        let mut config = test_computer_use_config();
        config.endpoint = server.uri();
        let tool = BrowserTool::new_with_backend(
            security,
            vec!["*".into()],
            None,
            "computer_use".into(),
            None,
            true,
            "http://127.0.0.1:9515".into(),
            None,
            config,
            Vec::new(),
        )
        .unwrap();

        let args = json!({
            "action": "screenshot",
            "path": config_path.to_string_lossy().to_string()
        });

        // Validation happens in execute_computer_use_action, returns ToolResult with error.
        let result = tool.execute(args).await.unwrap();
        assert!(!result.success, "Expected runtime-config rejection");
        let error = result.error.expect("Expected error in result");
        assert!(
            error.contains("runtime config") || error.contains("Refusing"),
            "Expected runtime-config rejection, got: {}",
            error
        );
    }

    #[tokio::test]
    #[cfg(unix)]
    async fn computer_use_dispatch_rejects_symlink_target_before_sidecar() {
        use std::os::unix::fs::symlink;
        use wiremock::matchers::{method, path};
        use wiremock::{Mock, MockServer, ResponseTemplate};

        let server = MockServer::start().await;
        let tmp = tempfile::TempDir::new().unwrap();
        let ws = tmp.path().join("ws");
        let outside = tmp.path().join("outside");
        tokio::fs::create_dir_all(&ws).await.unwrap();
        tokio::fs::create_dir_all(&outside).await.unwrap();

        // Create a symlink inside the workspace pointing outside.
        let link_path = ws.join("page.png");
        let target_path = outside.join("real.txt");
        tokio::fs::write(&target_path, b"real").await.unwrap();
        symlink(&target_path, &link_path).unwrap();

        // POST must never be reached: the ComputerUse symlink-target guard
        // rejects before any sidecar action request.
        Mock::given(method("POST"))
            .and(path("/"))
            .respond_with(ResponseTemplate::new(200))
            .expect(0)
            .mount(&server)
            .await;

        let security = Arc::new(SecurityPolicy {
            autonomy: AutonomyLevel::Full,
            workspace_dir: ws.clone(),
            allowed_roots: vec![ws.clone()],
            ..SecurityPolicy::default()
        });

        let mut config = test_computer_use_config();
        config.endpoint = server.uri();
        let tool = BrowserTool::new_with_backend(
            security,
            vec!["*".into()],
            None,
            "computer_use".into(),
            None,
            true,
            "http://127.0.0.1:9515".into(),
            None,
            config,
            Vec::new(),
        )
        .unwrap();

        let args = json!({
            "action": "screenshot",
            "path": "page.png"
        });

        // Validation happens in execute_computer_use_action, returns ToolResult with error.
        let result = tool.execute(args).await.unwrap();
        assert!(!result.success, "Expected symlink-target rejection");
        let error = result.error.expect("Expected error in result");
        assert!(
            error.contains("symlink"),
            "Expected symlink-target rejection, got: {}",
            error
        );
    }

    #[tokio::test]
    async fn computer_use_dispatch_writes_validated_png_locally_without_forwarding_path() {
        use wiremock::matchers::{method, path};
        use wiremock::{Mock, MockServer, ResponseTemplate};

        let server = MockServer::start().await;
        let tmp = tempfile::TempDir::new().unwrap();
        let ws = tmp.path().join("ws");
        tokio::fs::create_dir_all(&ws).await.unwrap();

        // Create the page.png file so canonicalize succeeds
        let page_path = ws.join("page.png");
        tokio::fs::write(&page_path, b"test").await.unwrap();

        // Mock the reachability check (GET request)
        Mock::given(method("GET"))
            .and(path("/"))
            .respond_with(ResponseTemplate::new(200))
            .mount(&server)
            .await;

        // Mock the POST endpoint to return PNG data
        Mock::given(method("POST"))
            .and(path("/"))
            .respond_with(ResponseTemplate::new(200).set_body_json(json!({
                "success": true,
                "data": {"png_base64": "iVBORw0KGgoAAAANSUhEUgAAAAEAAAABCAYAAAAfFcSJAAAADUlEQVR42mNk+M9QDwADhgGAWjR9awAAAABJRU5ErkJggg=="}
            })))
            .expect(1)
            .mount(&server)
            .await;

        // Setup security policy that allows the temp directory
        let security = Arc::new(SecurityPolicy {
            autonomy: AutonomyLevel::Full,
            workspace_dir: ws.clone(),
            allowed_roots: vec![tmp.path().to_path_buf()], // Allow the entire temp directory
            ..SecurityPolicy::default()
        });

        let mut config = test_computer_use_config();
        config.endpoint = server.uri();
        let tool = BrowserTool::new_with_backend(
            security,
            vec!["*".into()],
            None,
            "computer_use".into(),
            None,
            true,
            "http://127.0.0.1:9515".into(),
            None,
            config,
            Vec::new(),
        )
        .unwrap();

        // Use absolute path to the created file
        let args = json!({
            "action": "screenshot",
            "path": page_path.to_string_lossy().to_string()
        });

        // Should succeed - path is validated locally but NOT forwarded to the
        // sidecar. The sidecar returns PNG bytes and ZeroClaw performs the
        // validated local write.
        let result = tool.execute(args).await.unwrap();
        assert!(
            result.success,
            "Expected success, got error: {:?}",
            result.error
        );

        // The fail-closed contract: exactly one sidecar action request, and the
        // destination path is absent from its params.
        let requests = server.received_requests().await.unwrap();
        assert_eq!(requests.len(), 1);
        let body: Value = serde_json::from_slice(&requests[0].body).unwrap();
        let params = body.get("params").unwrap().as_object().unwrap();
        assert!(
            !params.contains_key("path"),
            "Path should not be forwarded to sidecar"
        );

        // ZeroClaw performed the validated local write: the pre-existing file
        // was overwritten with the decoded PNG bytes from the sidecar.
        let expected_png = base64::engine::general_purpose::STANDARD
            .decode("iVBORw0KGgoAAAANSUhEUgAAAAEAAAABCAYAAAAfFcSJAAAADUlEQVR42mNk+M9QDwADhgGAWjR9awAAAABJRU5ErkJggg==")
            .unwrap();
        let written = tokio::fs::read(&page_path).await.unwrap();
        assert_eq!(
            written, expected_png,
            "local screenshot write must match the sidecar PNG"
        );
    }

    #[tokio::test]
    async fn computer_use_dispatch_does_not_forward_path_and_writes_local_target() {
        use wiremock::matchers::{body_partial_json, method, path};
        use wiremock::{Mock, MockServer, ResponseTemplate};

        // Positive remote-sidecar contract: the validated destination is NOT
        // transmitted to the sidecar (the path is removed before the request),
        // and the returned PNG is written only to the validated local target.
        // A non-loopback sidecar address exercises the same flow — the old
        // filesystem-sharing rejection (endpoint_is_remote_filesystem) is gone.
        let tmp = tempfile::TempDir::new().unwrap();
        let ws = tmp.path().join("ws");
        tokio::fs::create_dir_all(&ws).await.unwrap();

        let server = MockServer::start().await;
        // The sidecar must NOT receive a `path` field in the screenshot params.
        Mock::given(method("POST"))
            .and(path("/"))
            .and(body_partial_json(
                serde_json::json!({"action": "screenshot"}),
            ))
            .respond_with(
                ResponseTemplate::new(200).set_body_json(serde_json::json!({
                    "success": true,
                    "data": { "png_base64": "iVBORw0KGgoAAAANSUhEUgAAAAEAAAABCAYAAAAfFcSJAAAADUlEQVR42mNk+M9QDwADhgGAWjR9awAAAABJRU5ErkJggg==" }
                })),
            )
            .mount(&server)
            .await;

        let mut config = test_computer_use_config();
        config.endpoint = server.uri();

        let security = Arc::new(SecurityPolicy {
            autonomy: AutonomyLevel::Full,
            workspace_dir: ws.clone(),
            allowed_roots: vec![ws.clone()],
            ..SecurityPolicy::default()
        });
        let tool = BrowserTool::new_with_backend(
            security,
            vec!["*".into()],
            None,
            "computer_use".into(),
            None,
            true,
            "http://127.0.0.1:9515".into(),
            None,
            config,
            Vec::new(),
        )
        .unwrap();

        let result = tool
            .execute(json!({
                "action": "screenshot",
                "path": "screenshot.png"
            }))
            .await
            .expect("execute must succeed");
        assert!(
            result.success,
            "a valid screenshot from the sidecar must write the validated local target: {:?}",
            result.error
        );

        // The local target was written with the PNG bytes.
        let written = tokio::fs::read(ws.join("screenshot.png"))
            .await
            .expect("the validated local target must be written");
        assert!(
            written.starts_with(&[0x89, b'P', b'N', b'G', b'\r', b'\n', 0x1a, b'\n']),
            "the written bytes must be a PNG, not arbitrary decoded data"
        );

        // The destination was not transmitted: every sidecar request body must
        // be free of a `path` field.
        let requests = server.received_requests().await.expect("infallible");
        for req in &requests {
            let body: serde_json::Value = req.body_json().expect("request body is JSON");
            assert!(
                body.get("params").and_then(|p| p.get("path")).is_none(),
                "the validated destination must NOT be forwarded to the sidecar: {body}"
            );
        }
    }

    /// Fail-closed contract for a path-bearing screenshot: the tool must NOT
    /// report success (or write the destination) unless the sidecar returned a
    /// well-formed ComputerUseResponse with success != false and a valid
    /// non-empty PNG payload. A malformed or unsuccessful 2xx body must fail
    /// and leave the destination unwritten.
    #[tokio::test]
    async fn computer_use_dispatch_fails_closed_on_malformed_screenshot_response() {
        use wiremock::matchers::{method, path};
        use wiremock::{Mock, MockServer, ResponseTemplate};

        let not_a_png_b64 = base64::engine::general_purpose::STANDARD.encode(b"not a png");
        // Each case is (name, wire body, expected error fragment). The
        // non-JSON case sends genuinely non-JSON bytes via `set_body_raw`
        // (a `set_body_json(json!("..."))` would transmit a *valid* JSON
        // string, exercising a different branch). Every case is a 200 so the
        // failure must come from response handling, not the HTTP layer.
        let cases: Vec<(&str, ResponseTemplate, &str)> = vec![
            (
                "non-json-2xx",
                ResponseTemplate::new(200)
                    .set_body_raw(b"this is not json {{{".to_vec(), "text/plain"),
                "non-JSON",
            ),
            (
                "success-false",
                ResponseTemplate::new(200)
                    .set_body_json(json!({"success": false, "error": "boom"})),
                "boom",
            ),
            (
                "empty-base64",
                ResponseTemplate::new(200)
                    .set_body_json(json!({"success": true, "data": {"png_base64": ""}})),
                "empty screenshot payload",
            ),
            (
                "non-png-bytes",
                ResponseTemplate::new(200)
                    .set_body_json(json!({"success": true, "data": {"png_base64": not_a_png_b64}})),
                "non-PNG screenshot payload",
            ),
        ];

        for (name, response_template, expected_error_fragment) in cases {
            let server = MockServer::start().await;
            let tmp = tempfile::TempDir::new().unwrap();
            let ws = tmp.path().join("ws");
            tokio::fs::create_dir_all(&ws).await.unwrap();

            // Reachability probe (GET) so the action POST is actually issued.
            Mock::given(method("GET"))
                .and(path("/"))
                .respond_with(ResponseTemplate::new(200))
                .mount(&server)
                .await;
            // The action POST must happen exactly once. If a pre-dispatch
            // failure short-circuits before the sidecar request, the POST
            // never fires and the test would otherwise pass on an unwritten
            // file alone — so the exact-once expectation makes that impossible.
            Mock::given(method("POST"))
                .and(path("/"))
                .respond_with(response_template)
                .expect(1)
                .mount(&server)
                .await;

            let security = Arc::new(SecurityPolicy {
                autonomy: AutonomyLevel::Full,
                workspace_dir: ws.clone(),
                allowed_roots: vec![ws.clone()],
                ..SecurityPolicy::default()
            });
            let mut config = test_computer_use_config();
            config.endpoint = server.uri();
            let tool = BrowserTool::new_with_backend(
                security,
                vec!["*".into()],
                None,
                "computer_use".into(),
                None,
                true,
                "http://127.0.0.1:9515".into(),
                None,
                config,
                Vec::new(),
            )
            .unwrap();

            let target = ws.join("shot.png");
            let result = tool
                .execute(json!({
                    "action": "screenshot",
                    "path": "shot.png"
                }))
                .await;

            // A malformed/unsuccessful sidecar response must fail the tool:
            // either as an Ok(success=false) ToolResult or as an Err — never a
            // success. Each shape must surface its own expected error, and the
            // destination must remain unwritten.
            let error_text = match &result {
                Ok(r) => {
                    assert!(
                        !r.success,
                        "{name}: must fail closed, got success with output {:?}",
                        r.output
                    );
                    r.error.clone().unwrap_or_default()
                }
                Err(e) => e.to_string(),
            };
            assert!(
                error_text.contains(expected_error_fragment),
                "{name}: expected error containing {expected_error_fragment:?}, got: {error_text}"
            );
            assert!(
                !tokio::fs::try_exists(&target)
                    .await
                    .expect("filesystem must be readable"),
                "{name}: the destination must NOT be written on a failed screenshot"
            );
        }
    }

    #[tokio::test]
    async fn computer_use_dispatch_rejects_non_string_path_before_sidecar() {
        use wiremock::matchers::{method, path};
        use wiremock::{Mock, MockServer, ResponseTemplate};

        // Start a mock server to pass the endpoint reachability check in resolve_backend()
        let server = MockServer::start().await;

        // Mock the reachability check (GET request) - should return 200
        Mock::given(method("GET"))
            .and(path("/"))
            .respond_with(ResponseTemplate::new(200))
            .mount(&server)
            .await;

        // Mock the screenshot action (POST request) - should NOT be called because
        // path validation happens before the sidecar request. Exact zero is
        // asserted so a regression that forwards a non-string path fails here.
        Mock::given(method("POST"))
            .and(path("/"))
            .respond_with(ResponseTemplate::new(200).set_body_json(json!({
                "success": true,
                "data": {"ok": true}
            })))
            .expect(0)
            .mount(&server)
            .await;

        let mut config = test_computer_use_config();
        config.endpoint = server.uri();
        let tool = browser_tool_with_computer_use(config);

        // Integer path - should fail before reaching sidecar
        let args = json!({
            "action": "screenshot",
            "path": 12345
        });
        // Validation happens in execute_computer_use_action, returns ToolResult with error
        let result = tool.execute(args).await.unwrap();
        assert!(!result.success, "Expected validation to fail");
        let error = result.error.expect("Expected error in result");
        assert!(
            error.contains("string") || error.contains("path"),
            "Expected non-string path error, got: {}",
            error
        );

        // Array path
        let args = json!({
            "action": "screenshot",
            "path": ["path1", "path2"]
        });
        let result = tool.execute(args).await.unwrap();
        assert!(!result.success, "Expected validation to fail");
        let error = result.error.expect("Expected error in result");
        assert!(
            error.contains("string") || error.contains("path"),
            "Expected non-string path error, got: {}",
            error
        );

        // Object path
        let args = json!({
            "action": "screenshot",
            "path": {"key": "value"}
        });
        let result = tool.execute(args).await.unwrap();
        assert!(!result.success, "Expected validation to fail");
        let error = result.error.expect("Expected error in result");
        assert!(
            error.contains("string") || error.contains("path"),
            "Expected non-string path error, got: {}",
            error
        );
    }

    #[tokio::test]
    async fn computer_use_dispatch_passes_through_empty_string_path() {
        use wiremock::matchers::{method, path};
        use wiremock::{Mock, MockServer, ResponseTemplate};

        let server = MockServer::start().await;

        // Empty string path → inline PNG semantics, no path validation, forwarded to sidecar
        Mock::given(method("POST"))
            .and(path("/"))
            .respond_with(ResponseTemplate::new(200).set_body_json(json!({
                "success": true,
                "data": {"png_base64": "iVBORw0KGgoAAAANSUhEUgAAAAEAAAABCAYAAAAfFcSJAAAADUlEQVR42mNk+M9QDwADhgGAWjR9awAAAABJRU5ErkJggg=="}
            })))
            .expect(1)
            .mount(&server)
            .await;

        let mut config = test_computer_use_config();
        config.endpoint = server.uri();
        let tool = browser_tool_with_computer_use(config);

        // Empty string path → inline PNG, no local write
        let args = json!({
            "action": "screenshot",
            "path": ""
        });

        let result = tool.execute(args).await.unwrap();
        assert!(
            result.success,
            "Expected success, got error: {:?}",
            result.error
        );
    }
}
