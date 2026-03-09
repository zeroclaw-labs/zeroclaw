//! Incident response playbook definitions and execution engine.
//!
//! Playbooks define structured response procedures for security incidents.
//! Each playbook has named steps, some of which require human approval before
//! execution. Playbooks are loaded from JSON files in the configured directory.

use serde::{Deserialize, Serialize};
use std::path::Path;

/// A single step in an incident response playbook.
#[derive(Debug, Clone, Serialize, Deserialize, PartialEq)]
pub struct PlaybookStep {
    /// Machine-readable action identifier (e.g. "isolate_host", "block_ip").
    pub action: String,
    /// Human-readable description of what this step does.
    pub description: String,
    /// Whether this step requires explicit human approval before execution.
    #[serde(default)]
    pub requires_approval: bool,
    /// Timeout in seconds for this step. Default: 300 (5 minutes).
    #[serde(default = "default_timeout_secs")]
    pub timeout_secs: u64,
}

fn default_timeout_secs() -> u64 {
    300
}

/// An incident response playbook.
#[derive(Debug, Clone, Serialize, Deserialize, PartialEq)]
pub struct Playbook {
    /// Unique playbook name (e.g. "suspicious_login").
    pub name: String,
    /// Human-readable description.
    pub description: String,
    /// Ordered list of response steps.
    pub steps: Vec<PlaybookStep>,
    /// Minimum alert severity that triggers this playbook (low/medium/high/critical).
    #[serde(default = "default_severity_filter")]
    pub severity_filter: String,
    /// Step indices (0-based) that can be auto-approved when below max_auto_severity.
    #[serde(default)]
    pub auto_approve_steps: Vec<usize>,
}

fn default_severity_filter() -> String {
    "medium".into()
}

/// Result of executing a single playbook step.
#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct StepExecutionResult {
    pub step_index: usize,
    pub action: String,
    pub status: StepStatus,
    pub message: String,
}

/// Status of a playbook step.
#[derive(Debug, Clone, Serialize, Deserialize, PartialEq)]
pub enum StepStatus {
    /// Step completed successfully.
    Completed,
    /// Step is waiting for human approval.
    PendingApproval,
    /// Step was skipped (e.g. not applicable).
    Skipped,
    /// Step failed with an error.
    Failed,
}

impl std::fmt::Display for StepStatus {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        match self {
            Self::Completed => write!(f, "completed"),
            Self::PendingApproval => write!(f, "pending_approval"),
            Self::Skipped => write!(f, "skipped"),
            Self::Failed => write!(f, "failed"),
        }
    }
}

/// Load all playbook definitions from a directory of JSON files.
pub fn load_playbooks(dir: &Path) -> Vec<Playbook> {
    let mut playbooks = Vec::new();

    if !dir.exists() || !dir.is_dir() {
        return builtin_playbooks();
    }

    if let Ok(entries) = std::fs::read_dir(dir) {
        for entry in entries.flatten() {
            let path = entry.path();
            if path.extension().map_or(false, |ext| ext == "json") {
                match std::fs::read_to_string(&path) {
                    Ok(contents) => match serde_json::from_str::<Playbook>(&contents) {
                        Ok(pb) => playbooks.push(pb),
                        Err(e) => {
                            tracing::warn!("Failed to parse playbook {}: {e}", path.display());
                        }
                    },
                    Err(e) => {
                        tracing::warn!("Failed to read playbook {}: {e}", path.display());
                    }
                }
            }
        }
    }

    // Merge built-in playbooks that aren't overridden by user-defined ones
    for builtin in builtin_playbooks() {
        if !playbooks.iter().any(|p| p.name == builtin.name) {
            playbooks.push(builtin);
        }
    }

    playbooks
}

/// Severity ordering for comparison: low < medium < high < critical.
pub fn severity_level(severity: &str) -> u8 {
    match severity.to_lowercase().as_str() {
        "low" => 1,
        "medium" => 2,
        "high" => 3,
        "critical" => 4,
        // Deny-by-default: unknown severities get the highest level to prevent
        // auto-approval of unrecognized severity labels.
        _ => u8::MAX,
    }
}

/// Check whether a step can be auto-approved given config constraints.
pub fn can_auto_approve(
    playbook: &Playbook,
    step_index: usize,
    alert_severity: &str,
    max_auto_severity: &str,
) -> bool {
    // Never auto-approve if alert severity exceeds the configured max
    if severity_level(alert_severity) > severity_level(max_auto_severity) {
        return false;
    }

    // Only auto-approve steps explicitly listed in auto_approve_steps
    playbook.auto_approve_steps.contains(&step_index)
}

/// Evaluate a playbook step. Returns the result with approval gating.
///
/// Steps that require approval and cannot be auto-approved will return
/// `StepStatus::PendingApproval` without executing.
pub fn evaluate_step(
    playbook: &Playbook,
    step_index: usize,
    alert_severity: &str,
    max_auto_severity: &str,
    require_approval: bool,
) -> StepExecutionResult {
    let step = match playbook.steps.get(step_index) {
        Some(s) => s,
        None => {
            return StepExecutionResult {
                step_index,
                action: "unknown".into(),
                status: StepStatus::Failed,
                message: format!("Step index {step_index} out of range"),
            };
        }
    };

    // Enforce approval gates: steps that require approval must either be
    // auto-approved or wait for human approval. Never mark an unexecuted
    // approval-gated step as Completed.
    if step.requires_approval {
        if !require_approval
            || !can_auto_approve(playbook, step_index, alert_severity, max_auto_severity)
        {
            return StepExecutionResult {
                step_index,
                action: step.action.clone(),
                status: StepStatus::PendingApproval,
                message: format!(
                    "Step '{}' requires human approval (severity: {alert_severity})",
                    step.description
                ),
            };
        }
    }

    // Step is approved (either doesn't require approval, or was auto-approved)
    // Actual execution would be delegated to the appropriate tool/system
    StepExecutionResult {
        step_index,
        action: step.action.clone(),
        status: StepStatus::Completed,
        message: format!("Executed: {}", step.description),
    }
}

/// Built-in playbook definitions for common incident types.
pub fn builtin_playbooks() -> Vec<Playbook> {
    vec![
        Playbook {
            name: "suspicious_login".into(),
            description: "Respond to suspicious login activity detected by SIEM".into(),
            steps: vec![
                PlaybookStep {
                    action: "gather_login_context".into(),
                    description: "Collect login metadata: IP, geo, device fingerprint, time".into(),
                    requires_approval: false,
                    timeout_secs: 60,
                },
                PlaybookStep {
                    action: "check_threat_intel".into(),
                    description: "Query threat intelligence for source IP reputation".into(),
                    requires_approval: false,
                    timeout_secs: 30,
                },
                PlaybookStep {
                    action: "notify_user".into(),
                    description: "Send verification notification to account owner".into(),
                    requires_approval: true,
                    timeout_secs: 300,
                },
                PlaybookStep {
                    action: "force_password_reset".into(),
                    description: "Force password reset if login confirmed unauthorized".into(),
                    requires_approval: true,
                    timeout_secs: 120,
                },
            ],
            severity_filter: "medium".into(),
            auto_approve_steps: vec![0, 1],
        },
        Playbook {
            name: "malware_detected".into(),
            description: "Respond to malware detection on endpoint".into(),
            steps: vec![
                PlaybookStep {
                    action: "isolate_endpoint".into(),
                    description: "Network-isolate the affected endpoint".into(),
                    requires_approval: true,
                    timeout_secs: 60,
                },
                PlaybookStep {
                    action: "collect_forensics".into(),
                    description: "Capture memory dump and disk image for analysis".into(),
                    requires_approval: false,
                    timeout_secs: 600,
                },
                PlaybookStep {
                    action: "scan_lateral_movement".into(),
                    description: "Check for lateral movement indicators on adjacent hosts".into(),
                    requires_approval: false,
                    timeout_secs: 300,
                },
                PlaybookStep {
                    action: "remediate_endpoint".into(),
                    description: "Remove malware and restore endpoint to clean state".into(),
                    requires_approval: true,
                    timeout_secs: 600,
                },
            ],
            severity_filter: "high".into(),
            auto_approve_steps: vec![1, 2],
        },
        Playbook {
            name: "data_exfiltration_attempt".into(),
            description: "Respond to suspected data exfiltration".into(),
            steps: vec![
                PlaybookStep {
                    action: "block_egress".into(),
                    description: "Block suspicious outbound connections".into(),
                    requires_approval: true,
                    timeout_secs: 30,
                },
                PlaybookStep {
                    action: "identify_data_scope".into(),
                    description: "Determine what data may have been accessed or transferred".into(),
                    requires_approval: false,
                    timeout_secs: 300,
                },
                PlaybookStep {
                    action: "preserve_evidence".into(),
                    description: "Preserve network logs and access records".into(),
                    requires_approval: false,
                    timeout_secs: 120,
                },
                PlaybookStep {
                    action: "escalate_to_legal".into(),
                    description: "Notify legal and compliance teams".into(),
                    requires_approval: true,
                    timeout_secs: 60,
                },
            ],
            severity_filter: "critical".into(),
            auto_approve_steps: vec![1, 2],
        },
        Playbook {
            name: "brute_force".into(),
            description: "Respond to brute force authentication attempts".into(),
            steps: vec![
                PlaybookStep {
                    action: "block_source_ip".into(),
                    description: "Block the attacking source IP at firewall".into(),
                    requires_approval: true,
                    timeout_secs: 30,
                },
                PlaybookStep {
                    action: "check_compromised_accounts".into(),
                    description: "Check if any accounts were successfully compromised".into(),
                    requires_approval: false,
                    timeout_secs: 120,
                },
                PlaybookStep {
                    action: "enable_rate_limiting".into(),
                    description: "Enable enhanced rate limiting on auth endpoints".into(),
                    requires_approval: true,
                    timeout_secs: 60,
                },
            ],
            severity_filter: "medium".into(),
            auto_approve_steps: vec![1],
        },
    ]
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn builtin_playbooks_are_valid() {
        let playbooks = builtin_playbooks();
        assert_eq!(playbooks.len(), 4);

        let names: Vec<&str> = playbooks.iter().map(|p| p.name.as_str()).collect();
        assert!(names.contains(&"suspicious_login"));
        assert!(names.contains(&"malware_detected"));
        assert!(names.contains(&"data_exfiltration_attempt"));
        assert!(names.contains(&"brute_force"));

        for pb in &playbooks {
            assert!(!pb.steps.is_empty(), "Playbook {} has no steps", pb.name);
            assert!(!pb.description.is_empty());
        }
    }

    #[test]
    fn severity_level_ordering() {
        assert!(severity_level("low") < severity_level("medium"));
        assert!(severity_level("medium") < severity_level("high"));
        assert!(severity_level("high") < severity_level("critical"));
        assert_eq!(severity_level("unknown"), u8::MAX);
    }

    #[test]
    fn auto_approve_respects_severity_cap() {
        let pb = &builtin_playbooks()[0]; // suspicious_login

        // Step 0 is in auto_approve_steps
        assert!(can_auto_approve(pb, 0, "low", "low"));
        assert!(can_auto_approve(pb, 0, "low", "medium"));

        // Alert severity exceeds max -> cannot auto-approve
        assert!(!can_auto_approve(pb, 0, "high", "low"));
        assert!(!can_auto_approve(pb, 0, "critical", "medium"));

        // Step 2 is NOT in auto_approve_steps
        assert!(!can_auto_approve(pb, 2, "low", "critical"));
    }

    #[test]
    fn evaluate_step_requires_approval() {
        let pb = &builtin_playbooks()[0]; // suspicious_login

        // Step 2 (notify_user) requires approval, high severity, max=low -> pending
        let result = evaluate_step(pb, 2, "high", "low", true);
        assert_eq!(result.status, StepStatus::PendingApproval);
        assert_eq!(result.action, "notify_user");

        // Step 0 (gather_login_context) does NOT require approval -> completed
        let result = evaluate_step(pb, 0, "high", "low", true);
        assert_eq!(result.status, StepStatus::Completed);
    }

    #[test]
    fn evaluate_step_out_of_range() {
        let pb = &builtin_playbooks()[0];
        let result = evaluate_step(pb, 99, "low", "low", true);
        assert_eq!(result.status, StepStatus::Failed);
    }

    #[test]
    fn playbook_json_roundtrip() {
        let pb = &builtin_playbooks()[0];
        let json = serde_json::to_string(pb).unwrap();
        let parsed: Playbook = serde_json::from_str(&json).unwrap();
        assert_eq!(parsed, *pb);
    }

    #[test]
    fn load_playbooks_from_nonexistent_dir_returns_builtins() {
        let playbooks = load_playbooks(Path::new("/nonexistent/dir"));
        assert_eq!(playbooks.len(), 4);
    }

    #[test]
    fn load_playbooks_merges_custom_and_builtin() {
        let dir = tempfile::tempdir().unwrap();
        let custom = Playbook {
            name: "custom_playbook".into(),
            description: "A custom playbook".into(),
            steps: vec![PlaybookStep {
                action: "custom_action".into(),
                description: "Do something custom".into(),
                requires_approval: true,
                timeout_secs: 60,
            }],
            severity_filter: "low".into(),
            auto_approve_steps: vec![],
        };
        let json = serde_json::to_string(&custom).unwrap();
        std::fs::write(dir.path().join("custom.json"), json).unwrap();

        let playbooks = load_playbooks(dir.path());
        // 4 builtins + 1 custom
        assert_eq!(playbooks.len(), 5);
        assert!(playbooks.iter().any(|p| p.name == "custom_playbook"));
    }

    #[test]
    fn load_playbooks_custom_overrides_builtin() {
        let dir = tempfile::tempdir().unwrap();
        let override_pb = Playbook {
            name: "suspicious_login".into(),
            description: "Custom override".into(),
            steps: vec![PlaybookStep {
                action: "custom_step".into(),
                description: "Overridden step".into(),
                requires_approval: false,
                timeout_secs: 30,
            }],
            severity_filter: "low".into(),
            auto_approve_steps: vec![0],
        };
        let json = serde_json::to_string(&override_pb).unwrap();
        std::fs::write(dir.path().join("suspicious_login.json"), json).unwrap();

        let playbooks = load_playbooks(dir.path());
        // 3 remaining builtins + 1 overridden = 4
        assert_eq!(playbooks.len(), 4);
        let sl = playbooks
            .iter()
            .find(|p| p.name == "suspicious_login")
            .unwrap();
        assert_eq!(sl.description, "Custom override");
    }
}
