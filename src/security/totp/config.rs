// TOML configuration schema for the TOTP module.
//
// Designed to live under [security.totp] in ZeroClaw's config.toml.
// All fields have sensible defaults; a minimal config is just:
//   [security.totp]
//   enabled = true

use std::collections::HashMap;

use serde::{Deserialize, Serialize};

use super::types::{GatingRule, TotpStatus};

/// Top-level TOTP configuration.
#[derive(Debug, Clone, Serialize, Deserialize)]
#[serde(default)]
pub struct TotpConfig {
    pub enabled: bool,
    pub max_attempts: u32,
    pub lockout_seconds: i64,
    pub global_rate_limit_per_minute: u32,
    pub clock_drift_auto_compensate: bool,
    pub clock_drift_threshold: u32,
    pub totp_prompt_timeout_seconds: u64,

    pub emergency: EmergencyConfig,
    pub maintainer: MaintainerConfig,
    pub alerts: AlertConfig,
    pub autonomy: AutonomyConfig,

    /// User-defined roles. Key = role name.
    #[serde(default)]
    pub roles: HashMap<String, RoleConfig>,

    /// User-defined gating rule sets. Key = rule set name (e.g., "base", "admin").
    #[serde(default)]
    pub rules: HashMap<String, Vec<GatingRule>>,

    /// User registry.
    #[serde(default)]
    pub users: Vec<UserConfig>,
}

impl Default for TotpConfig {
    fn default() -> Self {
        Self {
            enabled: false,
            max_attempts: 3,
            lockout_seconds: 300,
            global_rate_limit_per_minute: 10,
            clock_drift_auto_compensate: true,
            clock_drift_threshold: 3,
            totp_prompt_timeout_seconds: 120,
            emergency: EmergencyConfig::default(),
            maintainer: MaintainerConfig::default(),
            alerts: AlertConfig::default(),
            autonomy: AutonomyConfig::default(),
            roles: HashMap::new(),
            rules: HashMap::new(),
            users: Vec::new(),
        }
    }
}

/// Emergency / recovery configuration.
#[derive(Debug, Clone, Serialize, Deserialize)]
#[serde(default)]
pub struct EmergencyConfig {
    pub recovery_codes_count: usize,
    pub recovery_code_length: usize,
    pub recovery_warn_threshold: usize,
    pub admin_reset_grace_hours: u64,
}

impl Default for EmergencyConfig {
    fn default() -> Self {
        Self {
            recovery_codes_count: 10,
            recovery_code_length: 8,
            recovery_warn_threshold: 3,
            admin_reset_grace_hours: 24,
        }
    }
}

/// Maintainer (break-glass) configuration.
#[derive(Debug, Clone, Serialize, Deserialize)]
#[serde(default)]
pub struct MaintainerConfig {
    pub enabled: bool,
    pub key_path: String,
    pub audit_level: String,
}

impl Default for MaintainerConfig {
    fn default() -> Self {
        Self {
            enabled: false,
            key_path: "/etc/zeroclaw/maintainer.key".to_string(),
            audit_level: "critical".to_string(),
        }
    }
}

/// Alert / notification configuration (Finding F19).
#[derive(Debug, Clone, Serialize, Deserialize)]
#[serde(default)]
pub struct AlertConfig {
    pub enabled: bool,
    pub channel: String,
    pub severity_filter: String,
    #[serde(default)]
    pub events: Vec<String>,
}

impl Default for AlertConfig {
    fn default() -> Self {
        Self {
            enabled: false,
            channel: String::new(),
            severity_filter: "critical".to_string(),
            events: vec![
                "break_glass".to_string(),
                "lockout_triggered".to_string(),
                "config_downgrade_attempted".to_string(),
                "recovery_codes_low".to_string(),
            ],
        }
    }
}

/// Autonomy configuration for cron/self-heal context (D23).
#[derive(Debug, Clone, Serialize, Deserialize)]
#[serde(default)]
pub struct AutonomyConfig {
    pub enabled: bool,
    pub unknown_default: String,
    pub approval_expiry_hours: u64,

    /// Additional operations the user considers safe for autonomous execution.
    #[serde(default)]
    pub extra_autonomous_ops: Vec<String>,

    /// Additional operations the user wants to always block autonomously.
    #[serde(default)]
    pub extra_blocked_ops: Vec<String>,
}

impl Default for AutonomyConfig {
    fn default() -> Self {
        Self {
            enabled: true,
            unknown_default: "queue_for_approval".to_string(),
            approval_expiry_hours: 72,
            extra_autonomous_ops: Vec::new(),
            extra_blocked_ops: Vec::new(),
        }
    }
}

/// Role definition. User-defined, any name.
#[derive(Debug, Clone, Serialize, Deserialize)]
#[serde(default)]
pub struct RoleConfig {
    /// Parent role to inherit rules from.
    pub inherits: Option<String>,
    /// Operations blocked at role level (before TOTP check).
    #[serde(default)]
    pub blocked_operations: Vec<String>,
}

impl Default for RoleConfig {
    fn default() -> Self {
        Self {
            inherits: None,
            blocked_operations: Vec::new(),
        }
    }
}

/// User entry in the config file.
#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct UserConfig {
    pub id: String,
    pub name: String,
    pub role: String,
    /// Identity string: "type:identifier" (e.g., "telegram:123456789").
    pub identity: String,
    #[serde(default)]
    pub totp_status: TotpStatus,
}

#[cfg(test)]
mod tests {
    use super::*;
    use super::super::types::SecurityLevel;

    #[test]
    fn default_config_is_disabled() {
        let config = TotpConfig::default();
        assert!(!config.enabled);
        assert_eq!(config.max_attempts, 3);
        assert_eq!(config.lockout_seconds, 300);
    }

    #[test]
    fn toml_roundtrip() {
        let mut config = TotpConfig::default();
        config.enabled = true;
        config.max_attempts = 5;

        let toml_str = toml::to_string_pretty(&config).unwrap();
        let parsed: TotpConfig = toml::from_str(&toml_str).unwrap();

        assert!(parsed.enabled);
        assert_eq!(parsed.max_attempts, 5);
    }

    #[test]
    fn partial_config_uses_defaults() {
        let toml_str = r#"
            enabled = true
        "#;
        let config: TotpConfig = toml::from_str(toml_str).unwrap();
        assert!(config.enabled);
        assert_eq!(config.max_attempts, 3); // default
        assert_eq!(config.lockout_seconds, 300); // default
        assert_eq!(config.emergency.recovery_codes_count, 10); // default
    }

    #[test]
    fn security_level_serde_snake_case() {
        let rule = GatingRule {
            pattern: "test".to_string(),
            level: SecurityLevel::TotpAndConfirm,
            reason: "test".to_string(),
        };
        let json = serde_json::to_string(&rule).unwrap();
        assert!(json.contains("\"totp_and_confirm\""));

        let parsed: GatingRule = serde_json::from_str(&json).unwrap();
        assert_eq!(parsed.level, SecurityLevel::TotpAndConfirm);
    }

    #[test]
    fn user_config_with_role() {
        let toml_str = r#"
            enabled = true

            [roles.admin]
            inherits = "attorney"

            [roles.attorney]
            blocked_operations = []

            [roles.paralegal]
            inherits = "attorney"
            blocked_operations = ["akte.loeschen", "gericht.einreichen"]

            [[rules.base]]
            pattern = "akte.loeschen"
            level = "totp_and_confirm"
            reason = "Case file deletion"

            [[users]]
            id = "mueller"
            name = "RA Dr. Mueller"
            role = "admin"
            identity = "telegram:123456789"
            totp_status = "active"

            [[users]]
            id = "bauer"
            name = "Fr. Bauer"
            role = "paralegal"
            identity = "telegram:555666777"
            totp_status = "pending"
        "#;

        let config: TotpConfig = toml::from_str(toml_str).unwrap();
        assert_eq!(config.users.len(), 2);
        assert_eq!(config.users[0].role, "admin");
        assert_eq!(config.users[1].role, "paralegal");
        assert_eq!(config.roles.len(), 3);
        assert_eq!(
            config.roles["paralegal"].blocked_operations,
            vec!["akte.loeschen", "gericht.einreichen"]
        );
        assert_eq!(config.rules["base"].len(), 1);
        assert_eq!(config.rules["base"][0].level, SecurityLevel::TotpAndConfirm);
    }
}
