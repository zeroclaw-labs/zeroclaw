use crate::config::EstopConfig;
use crate::security::domain_matcher::DomainMatcher;
use crate::security::otp::OtpValidator;
use anyhow::{Context, Result};
use serde::{Deserialize, Serialize};
use std::fs;
use std::path::{Path, PathBuf};
use std::time::{SystemTime, UNIX_EPOCH};

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
}

impl EstopManager {
    pub fn load(config: &EstopConfig, config_dir: &Path) -> Result<Self> {
        let state_path = resolve_state_file_path(config_dir, &config.state_file);
        let mut should_fail_closed = false;
        let mut state = if state_path.exists() {
            match fs::read_to_string(&state_path) {
                Ok(raw) => match serde_json::from_str::<EstopState>(&raw) {
                    Ok(mut parsed) => {
                        parsed.normalize();
                        parsed
                    }
                    Err(error) => {
                        tracing::warn!(
                            path = %state_path.display(),
                            "Failed to parse estop state file; entering fail-closed mode: {error}"
                        );
                        should_fail_closed = true;
                        EstopState::fail_closed()
                    }
                },
                Err(error) => {
                    tracing::warn!(
                        path = %state_path.display(),
                        "Failed to read estop state file; entering fail-closed mode: {error}"
                    );
                    should_fail_closed = true;
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
        };

        if should_fail_closed {
            let _ = manager.persist_state();
        }

        Ok(manager)
    }

    pub fn state_path(&self) -> &Path {
        &self.state_path
    }

    pub fn status(&self) -> EstopState {
        self.state.clone()
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
}
