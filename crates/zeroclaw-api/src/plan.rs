//! Shared execution-plan types for the TodoWrite tracker.

use serde::{Deserialize, Serialize};

/// Execution status of a single plan entry. Snake-case on the wire to
/// match ACP (`in_progress`).
#[derive(Debug, Clone, Copy, Serialize, Deserialize, PartialEq, Eq, Default)]
#[serde(rename_all = "snake_case")]
pub enum PlanStatus {
    #[default]
    Pending,
    InProgress,
    Completed,
}

/// Relative importance of a plan entry. Defaults to `Medium` when a
/// caller omits it; ACP requires `priority` on the outward projection,
/// so normalization to a concrete value happens at parse time here.
#[derive(Debug, Clone, Copy, Serialize, Deserialize, PartialEq, Eq, Default)]
#[serde(rename_all = "snake_case")]
pub enum PlanPriority {
    High,
    #[default]
    Medium,
    Low,
}

/// A single plan entry. ACP-shaped (`content`/`priority`/`status`) plus
/// the optional ZeroClaw `active_form` extension.
#[derive(Debug, Clone, Serialize, Deserialize, PartialEq, Eq)]
pub struct PlanEntry {
    pub content: String,
    #[serde(default)]
    pub status: PlanStatus,
    #[serde(default)]
    pub priority: PlanPriority,
    #[serde(
        default,
        skip_serializing_if = "Option::is_none",
        rename = "activeForm"
    )]
    pub active_form: Option<String>,
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn plan_entry_round_trips_acp_shape() {
        let entry = PlanEntry {
            content: "Analyze codebase".to_string(),
            status: PlanStatus::InProgress,
            priority: PlanPriority::High,
            active_form: Some("Analyzing codebase".to_string()),
        };
        let v = serde_json::to_value(&entry).unwrap();
        assert_eq!(v["content"], "Analyze codebase");
        assert_eq!(v["status"], "in_progress");
        assert_eq!(v["priority"], "high");
        assert_eq!(v["activeForm"], "Analyzing codebase");

        let back: PlanEntry = serde_json::from_value(v).unwrap();
        assert_eq!(back, entry);
    }

    #[test]
    fn active_form_is_skipped_when_absent() {
        let entry = PlanEntry {
            content: "Do thing".to_string(),
            status: PlanStatus::Pending,
            priority: PlanPriority::Medium,
            active_form: None,
        };
        let v = serde_json::to_value(&entry).unwrap();
        assert!(
            v.get("activeForm").is_none(),
            "activeForm must be omitted when None"
        );
        assert_eq!(v["status"], "pending");
        assert_eq!(v["priority"], "medium");
    }

    #[test]
    fn priority_defaults_to_medium_when_missing() {
        let v = serde_json::json!({ "content": "x", "status": "pending" });
        let entry: PlanEntry = serde_json::from_value(v).unwrap();
        assert_eq!(entry.priority, PlanPriority::Medium);
    }

    #[test]
    fn status_defaults_to_pending_when_missing() {
        let v = serde_json::json!({ "content": "x" });
        let entry: PlanEntry = serde_json::from_value(v).unwrap();
        assert_eq!(entry.status, PlanStatus::Pending);
    }
}
