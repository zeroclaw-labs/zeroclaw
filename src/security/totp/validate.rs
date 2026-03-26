// Config validation and security downgrade detection (D16, F17).
//
// Validates TOML config before applying it. Detects security downgrades
// that require admin TOTP confirmation before activation.

use super::config::TotpConfig;

/// Validation result: either Ok or a list of human-readable errors.
#[derive(Debug)]
pub struct ValidationResult {
    pub errors: Vec<String>,
    pub warnings: Vec<String>,
}

impl ValidationResult {
    pub fn is_valid(&self) -> bool {
        self.errors.is_empty()
    }
}

/// Validate a TOTP config for correctness.
pub fn validate_config(config: &TotpConfig) -> ValidationResult {
    let mut errors = Vec::new();
    let mut warnings = Vec::new();

    // Check max_attempts is reasonable
    if config.max_attempts == 0 {
        errors.push("max_attempts cannot be 0 (would lock out immediately)".to_string());
    }
    if config.max_attempts > 10 {
        warnings.push(format!(
            "max_attempts={} is very high; 3-5 is recommended",
            config.max_attempts
        ));
    }

    // Check lockout_seconds
    if config.lockout_seconds < 30 {
        warnings.push(format!(
            "lockout_seconds={} is very short; 300 (5 min) is recommended",
            config.lockout_seconds
        ));
    }

    // Check rate limit
    if config.global_rate_limit_per_minute == 0 {
        errors.push("global_rate_limit_per_minute cannot be 0".to_string());
    }

    // Check roles reference valid parents
    for (name, role) in &config.roles {
        if let Some(ref parent) = role.inherits {
            if !config.roles.contains_key(parent) {
                errors.push(format!(
                    "Role '{name}' inherits from '{parent}', but '{parent}' is not defined"
                ));
            }
        }
    }

    // Check users reference valid roles
    for user in &config.users {
        if !config.roles.is_empty() && !config.roles.contains_key(&user.role) {
            errors.push(format!(
                "User '{}' has role '{}', but that role is not defined",
                user.id, user.role
            ));
        }
        if user.id.is_empty() {
            errors.push("User with empty ID found".to_string());
        }
        if user.identity.is_empty() {
            errors.push(format!("User '{}' has no identity", user.id));
        }
    }

    // Check for duplicate user IDs
    let mut seen_ids = std::collections::HashSet::new();
    for user in &config.users {
        if !seen_ids.insert(&user.id) {
            errors.push(format!("Duplicate user ID: '{}'", user.id));
        }
    }

    // Check rules have valid levels
    for (set_name, rules) in &config.rules {
        for (i, rule) in rules.iter().enumerate() {
            if rule.pattern.is_empty() {
                errors.push(format!("Rule {i} in set '{set_name}' has empty pattern"));
            }
            if rule.reason.is_empty() {
                warnings.push(format!(
                    "Rule {i} in set '{set_name}' has no reason (recommended for UX)"
                ));
            }
        }
    }

    // Check emergency config
    if config.emergency.recovery_codes_count == 0 {
        warnings.push("recovery_codes_count=0 means no recovery codes will be generated".to_string());
    }

    // Check maintainer key path
    if config.maintainer.enabled && config.maintainer.key_path.is_empty() {
        errors.push("Maintainer enabled but key_path is empty".to_string());
    }

    ValidationResult { errors, warnings }
}

/// Detected security downgrade between old and new config.
#[derive(Debug, Clone)]
pub struct SecurityDowngrade {
    pub field: String,
    pub old_value: String,
    pub new_value: String,
    pub severity: DowngradeSeverity,
}

#[derive(Debug, Clone, PartialEq, Eq)]
pub enum DowngradeSeverity {
    Critical,
    Warning,
}

/// Detect security downgrades between two configs (F17).
/// Returns a list of detected downgrades that require admin TOTP confirmation.
pub fn detect_downgrades(
    old: &TotpConfig,
    new: &TotpConfig,
) -> Vec<SecurityDowngrade> {
    let mut downgrades = Vec::new();

    // TOTP disabled
    if old.enabled && !new.enabled {
        downgrades.push(SecurityDowngrade {
            field: "enabled".to_string(),
            old_value: "true".to_string(),
            new_value: "false".to_string(),
            severity: DowngradeSeverity::Critical,
        });
    }

    // Max attempts increased significantly
    if new.max_attempts > old.max_attempts * 2 {
        downgrades.push(SecurityDowngrade {
            field: "max_attempts".to_string(),
            old_value: old.max_attempts.to_string(),
            new_value: new.max_attempts.to_string(),
            severity: DowngradeSeverity::Warning,
        });
    }

    // Lockout decreased significantly
    if new.lockout_seconds < old.lockout_seconds / 2 {
        downgrades.push(SecurityDowngrade {
            field: "lockout_seconds".to_string(),
            old_value: old.lockout_seconds.to_string(),
            new_value: new.lockout_seconds.to_string(),
            severity: DowngradeSeverity::Warning,
        });
    }

    // Rule level downgrades
    for (set_name, old_rules) in &old.rules {
        if let Some(new_rules) = new.rules.get(set_name) {
            for old_rule in old_rules {
                for new_rule in new_rules {
                    if old_rule.pattern == new_rule.pattern && new_rule.level < old_rule.level {
                        downgrades.push(SecurityDowngrade {
                            field: format!("rules.{}.{}", set_name, old_rule.pattern),
                            old_value: format!("{:?}", old_rule.level),
                            new_value: format!("{:?}", new_rule.level),
                            severity: DowngradeSeverity::Critical,
                        });
                    }
                }
            }
        }
    }

    // Users removed while active
    for old_user in &old.users {
        if old_user.totp_status == super::types::TotpStatus::Active {
            if !new.users.iter().any(|u| u.id == old_user.id) {
                downgrades.push(SecurityDowngrade {
                    field: format!("users.{}", old_user.id),
                    old_value: "active".to_string(),
                    new_value: "removed".to_string(),
                    severity: DowngradeSeverity::Warning,
                });
            }
        }
    }

    downgrades
}

#[cfg(test)]
mod tests {
    use super::*;
    use super::super::config::*;
    use super::super::types::*;

    #[test]
    fn valid_config_passes() {
        let mut config = TotpConfig::default();
        config.enabled = true;
        let result = validate_config(&config);
        assert!(result.is_valid());
    }

    #[test]
    fn zero_max_attempts_is_error() {
        let mut config = TotpConfig::default();
        config.max_attempts = 0;
        let result = validate_config(&config);
        assert!(!result.is_valid());
        assert!(result.errors.iter().any(|e| e.contains("max_attempts")));
    }

    #[test]
    fn invalid_role_reference_detected() {
        let mut config = TotpConfig::default();
        config.roles.insert("child".to_string(), RoleConfig {
            inherits: Some("nonexistent_parent".to_string()),
            ..Default::default()
        });
        let result = validate_config(&config);
        assert!(!result.is_valid());
        assert!(result.errors.iter().any(|e| e.contains("nonexistent_parent")));
    }

    #[test]
    fn duplicate_user_id_detected() {
        let mut config = TotpConfig::default();
        config.users = vec![
            UserConfig {
                id: "same".to_string(),
                name: "A".to_string(),
                role: "base".to_string(),
                identity: "os_user:a".to_string(),
                totp_status: TotpStatus::Active,
            },
            UserConfig {
                id: "same".to_string(),
                name: "B".to_string(),
                role: "base".to_string(),
                identity: "os_user:b".to_string(),
                totp_status: TotpStatus::Active,
            },
        ];
        let result = validate_config(&config);
        assert!(!result.is_valid());
        assert!(result.errors.iter().any(|e| e.contains("Duplicate")));
    }

    #[test]
    fn detect_totp_disable_downgrade() {
        let mut old = TotpConfig::default();
        old.enabled = true;
        let mut new = TotpConfig::default();
        new.enabled = false;

        let downgrades = detect_downgrades(&old, &new);
        assert!(!downgrades.is_empty());
        assert!(downgrades.iter().any(|d| d.field == "enabled" && d.severity == DowngradeSeverity::Critical));
    }

    #[test]
    fn detect_rule_level_downgrade() {
        let mut old = TotpConfig::default();
        old.rules.insert("base".to_string(), vec![GatingRule {
            pattern: "sudo".to_string(),
            level: SecurityLevel::TotpAndConfirm,
            reason: "test".to_string(),
        }]);

        let mut new = old.clone();
        new.rules.get_mut("base").unwrap()[0].level = SecurityLevel::None;

        let downgrades = detect_downgrades(&old, &new);
        assert!(!downgrades.is_empty());
        assert!(downgrades.iter().any(|d| d.field.contains("sudo")));
    }

    #[test]
    fn no_downgrade_when_upgrading() {
        let mut old = TotpConfig::default();
        old.enabled = true;
        old.rules.insert("base".to_string(), vec![GatingRule {
            pattern: "sudo".to_string(),
            level: SecurityLevel::Confirm,
            reason: "test".to_string(),
        }]);

        let mut new = old.clone();
        new.rules.get_mut("base").unwrap()[0].level = SecurityLevel::TotpRequired;

        let downgrades = detect_downgrades(&old, &new);
        assert!(downgrades.is_empty());
    }
}
