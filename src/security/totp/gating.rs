// Command gating engine.
//
// This is the central decision point. For every command the agent wants
// to execute, the gate determines: is this allowed, does it need TOTP,
// or is it blocked entirely?
//
// Decision flow:
//   1. Is TOTP disabled? → Allowed
//   2. Is this an E-Stop? → Allowed (TOTP-exempt, F21)
//   3. Is the operation blocked for this user's role? → Blocked
//   4. Is the context autonomous? → Check autonomy rules
//   5. Match command against gating rules → return SecurityLevel
//   6. Sign the decision with HMAC (TOCTOU protection, F15)

use std::sync::atomic::{AtomicU32, Ordering};
use std::time::Instant;

use parking_lot::Mutex;

use super::autonomy::AutonomyEngine;
use super::config::TotpConfig;
use super::types::*;
use super::users::UserRegistry;

/// The command gate engine. Thread-safe, shared via Arc in ZeroClaw.
pub struct CommandGate {
    config: TotpConfig,
    registry: UserRegistry,
    autonomy: AutonomyEngine,
    signing_key: Vec<u8>,
    /// Global rate limiter (Finding F6): tracks total verifications.
    global_verify_count: AtomicU32,
    global_verify_window_start: Mutex<Instant>,
}

impl CommandGate {
    pub fn new(config: TotpConfig, signing_key: Vec<u8>) -> Self {
        let registry = UserRegistry::from_config(&config);
        let autonomy = AutonomyEngine::from_config(&config.autonomy);

        Self {
            config,
            registry,
            autonomy,
            signing_key,
            global_verify_count: AtomicU32::new(0),
            global_verify_window_start: Mutex::new(Instant::now()),
        }
    }

    /// Evaluate a command and return a signed decision.
    ///
    /// This is the main entry point. ZeroClaw calls this before every
    /// tool execution. The returned SignedDecision MUST be verified
    /// by the execution engine before running the command (F15).
    pub fn evaluate(
        &self,
        user_id: &str,
        command: &str,
        context: &ExecutionContext,
    ) -> SignedDecision {
        let decision = self.evaluate_inner(user_id, command, context);
        SignedDecision::new(decision, command, &self.signing_key)
    }

    /// Verify that a signed decision is still valid for the given command.
    /// Call this in the execution engine right before running the command.
    pub fn verify_decision(&self, signed: &SignedDecision, command: &str) -> bool {
        signed.verify(command, &self.signing_key)
    }

    /// Check if the global rate limit has been exceeded (F6).
    pub fn is_rate_limited(&self) -> bool {
        let mut window_start = self.global_verify_window_start.lock();
        let now = Instant::now();

        // Reset window if 60 seconds have passed
        if now.duration_since(*window_start).as_secs() >= 60 {
            *window_start = now;
            self.global_verify_count.store(0, Ordering::Relaxed);
            return false;
        }

        self.global_verify_count.load(Ordering::Relaxed)
            >= self.config.global_rate_limit_per_minute
    }

    /// Record a TOTP verification attempt (for rate limiting).
    pub fn record_verify_attempt(&self) {
        self.global_verify_count.fetch_add(1, Ordering::Relaxed);
    }

    /// Get a reference to the config.
    pub fn config(&self) -> &TotpConfig {
        &self.config
    }

    /// Get a reference to the user registry.
    pub fn registry(&self) -> &UserRegistry {
        &self.registry
    }

    /// Reload config (D9). Returns the old config for downgrade comparison.
    pub fn reload_config(&mut self, new_config: TotpConfig) -> TotpConfig {
        let old = std::mem::replace(&mut self.config, new_config);
        self.registry = UserRegistry::from_config(&self.config);
        self.autonomy = AutonomyEngine::from_config(&self.config.autonomy);
        old
    }

    // ── Internal evaluation logic ────────────────────────────

    fn evaluate_inner(
        &self,
        user_id: &str,
        command: &str,
        context: &ExecutionContext,
    ) -> GateDecision {
        // 1. TOTP disabled → everything allowed
        if !self.config.enabled {
            return GateDecision::Allowed;
        }

        // 2. E-Stop is TOTP-exempt (F21)
        if Self::is_estop(command) {
            return GateDecision::Allowed;
        }

        // 3. Check role-level blocks (before TOTP)
        if self.registry.is_operation_blocked(user_id, command) {
            return GateDecision::Blocked {
                reason: format!("Operation blocked for user {user_id}'s role"),
            };
        }

        // 4. Autonomous context → check autonomy rules
        match context {
            ExecutionContext::Cron { .. } | ExecutionContext::SelfHeal { .. } => {
                return self.autonomy.evaluate(command);
            }
            ExecutionContext::Human => {} // continue to rule matching
        }

        // 5. Match against gating rules
        self.match_rules(user_id, command)
    }

    /// Match a command against all applicable rule sets.
    /// Rules are resolved by: role-specific rules first, then inherited "base" rules.
    fn match_rules(&self, user_id: &str, command: &str) -> GateDecision {
        let user = self.registry.resolve_by_id(user_id);
        let role_name = user.as_ref().map(|u| u.role.as_str()).unwrap_or("base");

        // Check role-specific rules first
        if let Some(rules) = self.config.rules.get(role_name) {
            for rule in rules {
                if command.contains(&rule.pattern) {
                    return self.level_to_decision(&rule.level, &rule.reason);
                }
            }
        }

        // Then check "base" rules (shared by all roles)
        if let Some(rules) = self.config.rules.get("base") {
            for rule in rules {
                if command.contains(&rule.pattern) {
                    return self.level_to_decision(&rule.level, &rule.reason);
                }
            }
        }

        // No rule matched → allowed
        GateDecision::Allowed
    }

    fn level_to_decision(&self, level: &SecurityLevel, reason: &str) -> GateDecision {
        match level {
            SecurityLevel::None => GateDecision::Allowed,
            SecurityLevel::Confirm => GateDecision::ConfirmRequired {
                reason: reason.to_string(),
            },
            SecurityLevel::TotpRequired => GateDecision::TotpRequired {
                reason: reason.to_string(),
            },
            SecurityLevel::TotpAndConfirm => GateDecision::TotpAndConfirmRequired {
                reason: reason.to_string(),
            },
        }
    }

    /// Check if a command is an emergency stop.
    fn is_estop(command: &str) -> bool {
        let normalized = command.trim().to_lowercase();
        normalized == "e_stop"
            || normalized == "estop"
            || normalized == "emergency_stop"
            || normalized.starts_with("e_stop ")
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use super::super::config::*;
    use super::super::types::TotpStatus;

    fn test_config() -> TotpConfig {
        let mut config = TotpConfig::default();
        config.enabled = true;

        // Roles
        config.roles.insert("admin".to_string(), RoleConfig {
            inherits: Some("base".to_string()),
            blocked_operations: vec![],
        });
        config.roles.insert("base".to_string(), RoleConfig {
            inherits: None,
            blocked_operations: vec![],
        });
        config.roles.insert("readonly".to_string(), RoleConfig {
            inherits: None,
            blocked_operations: vec!["shell".to_string(), "file_write".to_string()],
        });

        // Rules
        config.rules.insert("base".to_string(), vec![
            GatingRule {
                pattern: "rm -rf".to_string(),
                level: SecurityLevel::TotpAndConfirm,
                reason: "Destructive file deletion".to_string(),
            },
            GatingRule {
                pattern: "sudo".to_string(),
                level: SecurityLevel::TotpRequired,
                reason: "Elevated privileges".to_string(),
            },
            GatingRule {
                pattern: "shell".to_string(),
                level: SecurityLevel::TotpRequired,
                reason: "Shell execution".to_string(),
            },
        ]);

        // Users
        config.users = vec![
            UserConfig {
                id: "admin1".to_string(),
                name: "Admin".to_string(),
                role: "admin".to_string(),
                identity: "os_user:admin".to_string(),
                totp_status: TotpStatus::Active,
            },
            UserConfig {
                id: "viewer".to_string(),
                name: "Viewer".to_string(),
                role: "readonly".to_string(),
                identity: "web:viewer@example.com".to_string(),
                totp_status: TotpStatus::Active,
            },
        ];

        config
    }

    #[test]
    fn safe_command_is_allowed() {
        let gate = CommandGate::new(test_config(), b"test-key".to_vec());
        let sd = gate.evaluate("admin1", "ls -la", &ExecutionContext::Human);
        assert!(sd.decision.is_allowed());
    }

    #[test]
    fn dangerous_command_requires_totp() {
        let gate = CommandGate::new(test_config(), b"test-key".to_vec());
        let sd = gate.evaluate("admin1", "sudo apt update", &ExecutionContext::Human);
        assert!(matches!(sd.decision, GateDecision::TotpRequired { .. }));
    }

    #[test]
    fn destructive_command_requires_totp_and_confirm() {
        let gate = CommandGate::new(test_config(), b"test-key".to_vec());
        let sd = gate.evaluate("admin1", "rm -rf /tmp/data", &ExecutionContext::Human);
        assert!(matches!(
            sd.decision,
            GateDecision::TotpAndConfirmRequired { .. }
        ));
    }

    #[test]
    fn disabled_config_allows_everything() {
        let mut config = test_config();
        config.enabled = false;
        let gate = CommandGate::new(config, b"test-key".to_vec());
        let sd = gate.evaluate("admin1", "rm -rf /", &ExecutionContext::Human);
        assert!(sd.decision.is_allowed());
    }

    #[test]
    fn role_block_overrides_rules() {
        let gate = CommandGate::new(test_config(), b"test-key".to_vec());
        let sd = gate.evaluate("viewer", "shell echo hello", &ExecutionContext::Human);
        assert!(matches!(sd.decision, GateDecision::Blocked { .. }));
    }

    #[test]
    fn estop_is_totp_exempt() {
        let gate = CommandGate::new(test_config(), b"test-key".to_vec());
        let sd = gate.evaluate("admin1", "e_stop", &ExecutionContext::Human);
        assert!(sd.decision.is_allowed());
    }

    #[test]
    fn signed_decision_verifies() {
        let gate = CommandGate::new(test_config(), b"test-key".to_vec());
        let sd = gate.evaluate("admin1", "sudo reboot", &ExecutionContext::Human);
        assert!(gate.verify_decision(&sd, "sudo reboot"));
        assert!(!gate.verify_decision(&sd, "sudo rm -rf /"));
    }

    #[test]
    fn cron_context_uses_autonomy() {
        let gate = CommandGate::new(test_config(), b"test-key".to_vec());
        let context = ExecutionContext::Cron {
            job_name: "backup".to_string(),
        };

        // db.optimize is autonomous → allowed
        let sd = gate.evaluate("admin1", "db.optimize", &context);
        assert!(sd.decision.is_allowed());

        // db.delete is never autonomous → queued
        let sd = gate.evaluate("admin1", "db.delete", &context);
        assert!(matches!(
            sd.decision,
            GateDecision::QueuedForApproval { .. }
        ));
    }

    #[test]
    fn global_rate_limit() {
        let mut config = test_config();
        config.global_rate_limit_per_minute = 3;
        let gate = CommandGate::new(config, b"test-key".to_vec());

        assert!(!gate.is_rate_limited());
        gate.record_verify_attempt();
        gate.record_verify_attempt();
        gate.record_verify_attempt();
        assert!(gate.is_rate_limited());
    }
}
