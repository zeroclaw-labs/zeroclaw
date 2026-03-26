// Autonomy engine for cron/self-heal context (D23).
//
// Additive operations are autonomous (no human needed).
// Destructive operations require a human (queued for approval).
// Unknown operations default to queue_for_approval.

use std::collections::HashSet;

use super::config::AutonomyConfig;
use super::types::GateDecision;

/// Built-in list of operations safe for autonomous execution.
const DEFAULT_AUTONOMOUS_OPS: &[&str] = &[
    "db.optimize",
    "db.insert",
    "db.update",
    "db.label",
    "db.sort",
    "db.index_create",
    "db.backup_create",
    "ai.self_learn",
    "ai.knowledge_update",
    "ai.prompt_optimize",
    "config.patch_minor",
    "bugfix.non_critical",
    "cron.schedule",
    "report.generate",
];

/// Built-in list of operations that always need a human.
const DEFAULT_NEVER_AUTONOMOUS: &[&str] = &[
    "db.delete",
    "db.drop",
    "db.truncate",
    "db.schema_alter",
    "config.security",
    "config.totp",
    "user.create",
    "user.delete",
    "user.modify",
    "bugfix.critical",
    "backup.restore",
    "backup.delete",
    "cron.delete",
    "system.shutdown",
    "system.update",
    "skill.create",
    "skill.install",
];

pub struct AutonomyEngine {
    autonomous_ops: HashSet<String>,
    never_autonomous_ops: HashSet<String>,
    unknown_default: String,
}

impl AutonomyEngine {
    pub fn from_config(config: &AutonomyConfig) -> Self {
        let mut autonomous: HashSet<String> = DEFAULT_AUTONOMOUS_OPS
            .iter()
            .map(|s| s.to_string())
            .collect();
        for op in &config.extra_autonomous_ops {
            autonomous.insert(op.clone());
        }

        let mut never: HashSet<String> = DEFAULT_NEVER_AUTONOMOUS
            .iter()
            .map(|s| s.to_string())
            .collect();
        for op in &config.extra_blocked_ops {
            never.insert(op.clone());
        }

        // If something is in both lists, never_autonomous wins
        for op in &never {
            autonomous.remove(op);
        }

        Self {
            autonomous_ops: autonomous,
            never_autonomous_ops: never,
            unknown_default: config.unknown_default.clone(),
        }
    }

    /// Evaluate a command in autonomous context (Cron or SelfHeal).
    pub fn evaluate(&self, command: &str) -> GateDecision {
        // Check if explicitly autonomous
        if self.matches_list(command, &self.autonomous_ops) {
            return GateDecision::Allowed;
        }

        // Check if explicitly blocked
        if self.matches_list(command, &self.never_autonomous_ops) {
            return GateDecision::QueuedForApproval {
                reason: format!("Operation '{command}' requires human approval"),
            };
        }

        // Unknown operation → use configured default
        match self.unknown_default.as_str() {
            "block" => GateDecision::Blocked {
                reason: format!("Unknown autonomous operation: {command}"),
            },
            _ => GateDecision::QueuedForApproval {
                reason: format!("Unknown operation '{command}' queued for approval"),
            },
        }
    }

    /// Substring match against the ops list.
    fn matches_list(&self, command: &str, list: &HashSet<String>) -> bool {
        list.iter().any(|pattern| command.contains(pattern.as_str()))
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    fn default_engine() -> AutonomyEngine {
        AutonomyEngine::from_config(&AutonomyConfig::default())
    }

    #[test]
    fn autonomous_op_is_allowed() {
        let engine = default_engine();
        assert!(matches!(engine.evaluate("db.optimize"), GateDecision::Allowed));
        assert!(matches!(engine.evaluate("db.insert"), GateDecision::Allowed));
        assert!(matches!(engine.evaluate("ai.self_learn"), GateDecision::Allowed));
        assert!(matches!(engine.evaluate("report.generate"), GateDecision::Allowed));
    }

    #[test]
    fn never_autonomous_op_is_queued() {
        let engine = default_engine();
        assert!(matches!(engine.evaluate("db.delete"), GateDecision::QueuedForApproval { .. }));
        assert!(matches!(engine.evaluate("db.drop"), GateDecision::QueuedForApproval { .. }));
        assert!(matches!(engine.evaluate("system.shutdown"), GateDecision::QueuedForApproval { .. }));
        assert!(matches!(engine.evaluate("skill.create"), GateDecision::QueuedForApproval { .. }));
    }

    #[test]
    fn unknown_op_defaults_to_queue() {
        let engine = default_engine();
        assert!(matches!(
            engine.evaluate("custom.unknown_thing"),
            GateDecision::QueuedForApproval { .. }
        ));
    }

    #[test]
    fn extra_autonomous_ops_from_config() {
        let mut config = AutonomyConfig::default();
        config.extra_autonomous_ops = vec!["custom.safe_op".to_string()];
        let engine = AutonomyEngine::from_config(&config);
        assert!(matches!(engine.evaluate("custom.safe_op"), GateDecision::Allowed));
    }

    #[test]
    fn extra_blocked_ops_override_autonomous() {
        let mut config = AutonomyConfig::default();
        // db.insert is normally autonomous, but user blocks it
        config.extra_blocked_ops = vec!["db.insert".to_string()];
        let engine = AutonomyEngine::from_config(&config);
        assert!(matches!(
            engine.evaluate("db.insert"),
            GateDecision::QueuedForApproval { .. }
        ));
    }

    #[test]
    fn human_context_not_evaluated_here() {
        // This engine only handles Cron/SelfHeal contexts.
        // Human context bypasses this entirely (handled in gating.rs).
        // This test just verifies the engine exists and can be called.
        let engine = default_engine();
        let _ = engine.evaluate("anything");
    }
}
