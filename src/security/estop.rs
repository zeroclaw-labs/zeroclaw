use crate::config::EstopConfig;
use crate::security::domain_matcher::DomainMatcher;
use crate::security::otp::OtpValidator;
use anyhow::{Context, Result};
use serde::{Deserialize, Serialize};
use serde_json::Value;
use std::fs;
use std::path::{Path, PathBuf};
use std::time::{SystemTime, UNIX_EPOCH};

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum EstopLoadStatus {
    Loaded,
    Missing,
    Corrupted,
}

#[derive(Debug, Clone, PartialEq, Eq)]
pub struct EstopLoadReport {
    pub status: EstopLoadStatus,
    pub state_file: String,
    pub active_levels: Vec<String>,
    pub error: Option<String>,
}

#[derive(Debug, Clone, PartialEq, Eq)]
pub enum EstopLevel {
    KillAll,
    NetworkKill,
    DomainBlock(Vec<String>),
    ToolFreeze(Vec<String>),
}

#[derive(Debug, Clone, PartialEq, Eq)]
pub enum ResumeSelector {
    KillAll,
    Network,
    Domains(Vec<String>),
    Tools(Vec<String>),
}

#[derive(Debug, Clone, Serialize, Deserialize, Default, PartialEq, Eq)]
pub struct EstopState {
    #[serde(default)]
    pub kill_all: bool,
    #[serde(default)]
    pub network_kill: bool,
    #[serde(default)]
    pub blocked_domains: Vec<String>,
    #[serde(default)]
    pub frozen_tools: Vec<String>,
    #[serde(default)]
    pub updated_at: Option<String>,
}

impl EstopState {
    pub fn fail_closed() -> Self {
        Self {
            kill_all: true,
            network_kill: false,
            blocked_domains: Vec::new(),
            frozen_tools: Vec::new(),
            updated_at: Some(now_rfc3339()),
        }
    }

    pub fn is_engaged(&self) -> bool {
        self.kill_all
            || self.network_kill
            || !self.blocked_domains.is_empty()
            || !self.frozen_tools.is_empty()
    }

    fn normalize(&mut self) {
        self.blocked_domains = dedup_sort(&self.blocked_domains);
        self.frozen_tools = dedup_sort(&self.frozen_tools);
    }
}

#[derive(Debug, Clone)]
pub struct EstopManager {
    config: EstopConfig,
    state_path: PathBuf,
    state: EstopState,
    load_status: EstopLoadStatus,
    load_error: Option<String>,
}

impl EstopManager {
    pub fn load(config: &EstopConfig, config_dir: &Path) -> Result<Self> {
        let (manager, _) = Self::load_with_report(config, config_dir)?;
        Ok(manager)
    }

    pub fn load_with_report(
        config: &EstopConfig,
        config_dir: &Path,
    ) -> Result<(Self, EstopLoadReport)> {
        let state_path = resolve_state_file_path(config_dir, &config.state_file);
        let mut should_fail_closed = false;
        let mut load_error = None;
        let mut load_status = EstopLoadStatus::Missing;
        let mut state = if state_path.exists() {
            load_status = EstopLoadStatus::Loaded;
            match fs::read_to_string(&state_path) {
                Ok(raw) => match serde_json::from_str::<EstopState>(&raw) {
                    Ok(mut parsed) => {
                        parsed.normalize();
                        parsed
                    }
                    Err(error) => {
                        let message = error.to_string();
                        tracing::warn!(
                            path = %state_path.display(),
                            "Failed to parse estop state file; entering fail-closed mode: {error}"
                        );
                        should_fail_closed = true;
                        load_status = EstopLoadStatus::Corrupted;
                        load_error = Some(message);
                        EstopState::fail_closed()
                    }
                },
                Err(error) => {
                    let message = error.to_string();
                    tracing::warn!(
                        path = %state_path.display(),
                        "Failed to read estop state file; entering fail-closed mode: {error}"
                    );
                    should_fail_closed = true;
                    load_status = EstopLoadStatus::Corrupted;
                    load_error = Some(message);
                    EstopState::fail_closed()
                }
            }
        } else {
            EstopState::default()
        };

        state.normalize();

        let mut manager = Self {
            config: config.clone(),
            state_path,
            state,
            load_status,
            load_error,
        };

        if should_fail_closed {
            let _ = manager.persist_state();
        }

        let report = manager.load_report();

        Ok((manager, report))
    }

    pub fn state_path(&self) -> &Path {
        &self.state_path
    }

    pub fn status(&self) -> EstopState {
        self.state.clone()
    }

    pub fn load_status(&self) -> EstopLoadStatus {
        self.load_status
    }

    pub fn load_report(&self) -> EstopLoadReport {
        EstopLoadReport {
            status: self.load_status,
            state_file: self.state_path.display().to_string(),
            active_levels: active_levels(&self.state),
            error: self.load_error.clone(),
        }
    }

    pub fn engage(&mut self, level: EstopLevel) -> Result<()> {
        match level {
            EstopLevel::KillAll => {
                self.state.kill_all = true;
            }
            EstopLevel::NetworkKill => {
                self.state.network_kill = true;
            }
            EstopLevel::DomainBlock(domains) => {
                for domain in domains {
                    let normalized = domain.trim().to_ascii_lowercase();
                    DomainMatcher::validate_pattern(&normalized)?;
                    self.state.blocked_domains.push(normalized);
                }
            }
            EstopLevel::ToolFreeze(tools) => {
                for tool in tools {
                    let normalized = normalize_tool_name(&tool)?;
                    self.state.frozen_tools.push(normalized);
                }
            }
        }

        self.state.updated_at = Some(now_rfc3339());
        self.state.normalize();
        self.persist_state()
    }

    pub fn resume(
        &mut self,
        selector: ResumeSelector,
        otp_code: Option<&str>,
        otp_validator: Option<&OtpValidator>,
    ) -> Result<()> {
        self.ensure_resume_is_authorized(otp_code, otp_validator)?;

        match selector {
            ResumeSelector::KillAll => {
                self.state.kill_all = false;
            }
            ResumeSelector::Network => {
                self.state.network_kill = false;
            }
            ResumeSelector::Domains(domains) => {
                let normalized = domains
                    .iter()
                    .map(|domain| domain.trim().to_ascii_lowercase())
                    .collect::<Vec<_>>();
                self.state
                    .blocked_domains
                    .retain(|existing| !normalized.iter().any(|target| target == existing));
            }
            ResumeSelector::Tools(tools) => {
                let normalized = tools
                    .iter()
                    .map(|tool| normalize_tool_name(tool))
                    .collect::<Result<Vec<_>>>()?;
                self.state
                    .frozen_tools
                    .retain(|existing| !normalized.iter().any(|target| target == existing));
            }
        }

        self.state.updated_at = Some(now_rfc3339());
        self.state.normalize();
        self.persist_state()
    }

    /// Enforce estop state against a tool call.
    pub fn check_tool(&self, tool_name: &str, args: &Value) -> Result<()> {
        if self.state.kill_all {
            anyhow::bail!("Emergency stop active: kill-all blocks all tools");
        }

        let normalized_tool = normalize_tool_name(tool_name)?;
        if self
            .state
            .frozen_tools
            .iter()
            .any(|frozen| frozen == &normalized_tool)
        {
            anyhow::bail!("Emergency stop active: tool-freeze blocks tool '{normalized_tool}'");
        }

        if self.state.network_kill && is_network_tool_call(&normalized_tool, args) {
            anyhow::bail!(
                "Emergency stop active: network-kill blocks network tool '{normalized_tool}'"
            );
        }

        Ok(())
    }

    /// Enforce domain-block estop state for browser navigations.
    pub fn check_domain(&self, domain: &str) -> Result<()> {
        if self.state.kill_all {
            anyhow::bail!("Emergency stop active: kill-all blocks all tools");
        }

        if self.state.blocked_domains.is_empty() {
            return Ok(());
        }

        let matcher = DomainMatcher::new(&self.state.blocked_domains, &[] as &[String])?;
        if matcher.is_gated(domain) {
            anyhow::bail!("Emergency stop active: domain-block blocks browser domain '{domain}'");
        }

        Ok(())
    }

    fn ensure_resume_is_authorized(
        &self,
        otp_code: Option<&str>,
        otp_validator: Option<&OtpValidator>,
    ) -> Result<()> {
        if !self.config.require_otp_to_resume {
            return Ok(());
        }

        let code = otp_code
            .map(str::trim)
            .filter(|value| !value.is_empty())
            .context("OTP code is required to resume estop state")?;
        let validator = otp_validator
            .context("OTP validator is required to resume estop state with OTP enabled")?;
        let valid = validator.validate(code)?;
        if !valid {
            anyhow::bail!("Invalid OTP code; estop resume denied");
        }
        Ok(())
    }

    fn persist_state(&mut self) -> Result<()> {
        if let Some(parent) = self.state_path.parent() {
            fs::create_dir_all(parent).with_context(|| {
                format!("Failed to create estop state dir {}", parent.display())
            })?;
        }

        let body =
            serde_json::to_string_pretty(&self.state).context("Failed to serialize estop state")?;

        let temp_path = self
            .state_path
            .with_extension(format!("tmp-{}", uuid::Uuid::new_v4()));
        fs::write(&temp_path, body).with_context(|| {
            format!(
                "Failed to write temporary estop state file {}",
                temp_path.display()
            )
        })?;

        #[cfg(unix)]
        {
            use std::os::unix::fs::PermissionsExt;
            let _ = fs::set_permissions(&temp_path, fs::Permissions::from_mode(0o600));
        }

        fs::rename(&temp_path, &self.state_path).with_context(|| {
            format!(
                "Failed to atomically replace estop state file {}",
                self.state_path.display()
            )
        })?;

        Ok(())
    }
}

pub fn resolve_state_file_path(config_dir: &Path, state_file: &str) -> PathBuf {
    let expanded = shellexpand::tilde(state_file).into_owned();
    let path = PathBuf::from(expanded);
    if path.is_absolute() {
        path
    } else {
        config_dir.join(path)
    }
}

fn normalize_tool_name(raw: &str) -> Result<String> {
    let value = raw.trim().to_ascii_lowercase();
    if value.is_empty() {
        anyhow::bail!("Tool name must not be empty");
    }
    if !value
        .chars()
        .all(|ch| ch.is_ascii_alphanumeric() || ch == '_' || ch == '-')
    {
        anyhow::bail!("Tool name '{raw}' contains invalid characters");
    }
    Ok(value)
}

fn dedup_sort(values: &[String]) -> Vec<String> {
    let mut deduped = values
        .iter()
        .map(|value| value.trim())
        .filter(|value| !value.is_empty())
        .map(ToString::to_string)
        .collect::<Vec<_>>();
    deduped.sort_unstable();
    deduped.dedup();
    deduped
}

fn now_rfc3339() -> String {
    let secs = SystemTime::now()
        .duration_since(UNIX_EPOCH)
        .map(|duration| duration.as_secs())
        .unwrap_or(0);
    chrono::DateTime::<chrono::Utc>::from_timestamp(secs as i64, 0)
        .unwrap_or(chrono::DateTime::<chrono::Utc>::UNIX_EPOCH)
        .to_rfc3339()
}

fn is_network_tool_call(tool_name: &str, args: &Value) -> bool {
    match tool_name {
        "browser" | "browser_open" | "http_request" | "web_search" | "composio" | "pushover" => {
            true
        }
        "shell" => args
            .get("command")
            .and_then(Value::as_str)
            .is_some_and(shell_command_uses_network),
        _ => false,
    }
}

fn active_levels(state: &EstopState) -> Vec<String> {
    let mut levels = Vec::new();
    if state.kill_all {
        levels.push("kill_all".to_string());
    }
    if state.network_kill {
        levels.push("network_kill".to_string());
    }
    if !state.blocked_domains.is_empty() {
        levels.push("domain_block".to_string());
    }
    if !state.frozen_tools.is_empty() {
        levels.push("tool_freeze".to_string());
    }
    levels
}

fn shell_command_uses_network(command: &str) -> bool {
    let normalized = command.to_ascii_lowercase();
    [
        "http://",
        "https://",
        "curl ",
        "wget ",
        "ssh ",
        "scp ",
        "sftp ",
        "ping ",
        "nc ",
        "ncat ",
        "netcat ",
        "telnet ",
        "ftp ",
        "dig ",
        "nslookup ",
    ]
    .iter()
    .any(|token| normalized.contains(token))
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::config::OtpConfig;
    use crate::security::otp::OtpValidator;
    use crate::security::SecretStore;
    use tempfile::tempdir;

    fn estop_config(path: &Path) -> EstopConfig {
        EstopConfig {
            enabled: true,
            state_file: path.display().to_string(),
            require_otp_to_resume: false,
            auto_triggers: crate::config::EstopAutoTriggersConfig::default(),
        }
    }

    #[test]
    fn estop_levels_compose_and_resume() {
        let dir = tempdir().unwrap();
        let state_path = dir.path().join("estop-state.json");
        let cfg = estop_config(&state_path);
        let mut manager = EstopManager::load(&cfg, dir.path()).unwrap();

        manager
            .engage(EstopLevel::DomainBlock(vec!["*.chase.com".into()]))
            .unwrap();
        manager
            .engage(EstopLevel::ToolFreeze(vec!["shell".into()]))
            .unwrap();
        manager.engage(EstopLevel::NetworkKill).unwrap();
        assert!(manager.status().network_kill);
        assert_eq!(manager.status().blocked_domains, vec!["*.chase.com"]);
        assert_eq!(manager.status().frozen_tools, vec!["shell"]);

        manager
            .resume(
                ResumeSelector::Domains(vec!["*.chase.com".into()]),
                None,
                None,
            )
            .unwrap();
        assert!(manager.status().blocked_domains.is_empty());
        assert!(manager.status().network_kill);

        manager
            .resume(ResumeSelector::Tools(vec!["shell".into()]), None, None)
            .unwrap();
        assert!(manager.status().frozen_tools.is_empty());
    }

    #[test]
    fn estop_state_survives_reload() {
        let dir = tempdir().unwrap();
        let state_path = dir.path().join("estop-state.json");
        let cfg = estop_config(&state_path);

        {
            let mut manager = EstopManager::load(&cfg, dir.path()).unwrap();
            manager.engage(EstopLevel::KillAll).unwrap();
            manager
                .engage(EstopLevel::DomainBlock(vec!["*.paypal.com".into()]))
                .unwrap();
        }

        let reloaded = EstopManager::load(&cfg, dir.path()).unwrap();
        let state = reloaded.status();
        assert!(state.kill_all);
        assert_eq!(state.blocked_domains, vec!["*.paypal.com"]);
    }

    #[test]
    fn corrupted_state_defaults_to_fail_closed_kill_all() {
        let dir = tempdir().unwrap();
        let state_path = dir.path().join("estop-state.json");
        fs::write(&state_path, "{not-valid-json").unwrap();
        let cfg = estop_config(&state_path);
        let manager = EstopManager::load(&cfg, dir.path()).unwrap();
        assert!(manager.status().kill_all);
    }

    #[test]
    fn resume_requires_valid_otp_when_enabled() {
        let dir = tempdir().unwrap();
        let state_path = dir.path().join("estop-state.json");
        let mut cfg = estop_config(&state_path);
        cfg.require_otp_to_resume = true;

        let mut manager = EstopManager::load(&cfg, dir.path()).unwrap();
        manager.engage(EstopLevel::KillAll).unwrap();

        let err = manager
            .resume(ResumeSelector::KillAll, None, None)
            .expect_err("resume should require OTP");
        assert!(err.to_string().contains("OTP code is required"));
    }

    #[test]
    fn resume_accepts_valid_otp_code() {
        let dir = tempdir().unwrap();
        let state_path = dir.path().join("estop-state.json");
        let mut cfg = estop_config(&state_path);
        cfg.require_otp_to_resume = true;

        let otp_cfg = OtpConfig {
            enabled: true,
            ..OtpConfig::default()
        };
        let store = SecretStore::new(dir.path(), true);
        let (validator, _) = OtpValidator::from_config(&otp_cfg, dir.path(), &store).unwrap();
        let now = SystemTime::now()
            .duration_since(UNIX_EPOCH)
            .map(|duration| duration.as_secs())
            .unwrap_or(0);
        let code = validator.code_for_timestamp(now);

        let mut manager = EstopManager::load(&cfg, dir.path()).unwrap();
        manager.engage(EstopLevel::KillAll).unwrap();
        manager
            .resume(ResumeSelector::KillAll, Some(&code), Some(&validator))
            .unwrap();
        assert!(!manager.status().kill_all);
    }

    #[test]
    fn check_tool_enforces_freeze_network_and_kill_all() {
        let dir = tempdir().unwrap();
        let state_path = dir.path().join("estop-state.json");
        let cfg = estop_config(&state_path);
        let mut manager = EstopManager::load(&cfg, dir.path()).unwrap();

        manager
            .engage(EstopLevel::ToolFreeze(vec!["shell".into()]))
            .unwrap();
        let err = manager
            .check_tool("shell", &serde_json::json!({"command": "pwd"}))
            .unwrap_err();
        assert!(err.to_string().contains("tool-freeze"));

        manager
            .resume(ResumeSelector::Tools(vec!["shell".into()]), None, None)
            .unwrap();
        manager.engage(EstopLevel::NetworkKill).unwrap();
        let err = manager
            .check_tool(
                "shell",
                &serde_json::json!({"command": "curl https://example.com"}),
            )
            .unwrap_err();
        assert!(err.to_string().contains("network-kill"));

        manager.engage(EstopLevel::KillAll).unwrap();
        let err = manager
            .check_tool("file_read", &serde_json::json!({"path": "Cargo.toml"}))
            .unwrap_err();
        assert!(err.to_string().contains("kill-all"));
    }

    #[test]
    fn check_domain_blocks_matching_domain_patterns() {
        let dir = tempdir().unwrap();
        let state_path = dir.path().join("estop-state.json");
        let cfg = estop_config(&state_path);
        let mut manager = EstopManager::load(&cfg, dir.path()).unwrap();
        manager
            .engage(EstopLevel::DomainBlock(vec!["*.chase.com".into()]))
            .unwrap();

        let err = manager.check_domain("secure.chase.com").unwrap_err();
        assert!(err.to_string().contains("domain-block"));
        assert!(manager.check_domain("example.com").is_ok());
    }
}
