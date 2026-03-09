use schemars::JsonSchema;
use serde::{Deserialize, Serialize};
use std::fmt;
use std::path::PathBuf;

// ── Priority ────────────────────────────────────────────────────

/// SOP priority level, used for execution mode resolution and scheduling.
#[derive(Debug, Clone, Copy, Default, PartialEq, Eq, Hash, Serialize, Deserialize)]
#[serde(rename_all = "lowercase")]
pub enum SopPriority {
    Low,
    #[default]
    Normal,
    High,
    Critical,
}

impl fmt::Display for SopPriority {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        match self {
            Self::Low => write!(f, "low"),
            Self::Normal => write!(f, "normal"),
            Self::High => write!(f, "high"),
            Self::Critical => write!(f, "critical"),
        }
    }
}

// ── Execution Mode ──────────────────────────────────────────────

/// How much autonomy the agent has when executing an SOP.
#[derive(Debug, Clone, Copy, Default, PartialEq, Eq, Hash, Serialize, Deserialize, JsonSchema)]
#[serde(rename_all = "snake_case")]
pub enum SopExecutionMode {
    /// Execute all steps without human approval.
    Auto,
    /// Request approval before starting, then execute all steps.
    #[default]
    Supervised,
    /// Request approval before each step.
    StepByStep,
    /// Critical/High → Auto, Normal/Low → Supervised.
    PriorityBased,
}

impl fmt::Display for SopExecutionMode {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        match self {
            Self::Auto => write!(f, "auto"),
            Self::Supervised => write!(f, "supervised"),
            Self::StepByStep => write!(f, "step_by_step"),
            Self::PriorityBased => write!(f, "priority_based"),
        }
    }
}

// ── Trigger ─────────────────────────────────────────────────────

/// What event can activate an SOP.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
#[serde(tag = "type", rename_all = "lowercase")]
pub enum SopTrigger {
    Mqtt {
        topic: String,
        #[serde(default)]
        condition: Option<String>,
    },
    Webhook {
        path: String,
    },
    Cron {
        expression: String,
    },
    Peripheral {
        board: String,
        signal: String,
        #[serde(default)]
        condition: Option<String>,
    },
    Manual,
}

impl fmt::Display for SopTrigger {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        match self {
            Self::Mqtt { topic, .. } => write!(f, "mqtt:{topic}"),
            Self::Webhook { path } => write!(f, "webhook:{path}"),
            Self::Cron { expression } => write!(f, "cron:{expression}"),
            Self::Peripheral { board, signal, .. } => write!(f, "peripheral:{board}/{signal}"),
            Self::Manual => write!(f, "manual"),
        }
    }
}

// ── Step ────────────────────────────────────────────────────────

/// A single step in an SOP procedure, parsed from SOP.md.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct SopStep {
    pub number: u32,
    pub title: String,
    pub body: String,
    #[serde(default)]
    pub suggested_tools: Vec<String>,
    #[serde(default)]
    pub requires_confirmation: bool,
}

// ── SOP ─────────────────────────────────────────────────────────

/// A complete Standard Operating Procedure definition.
#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct Sop {
    pub name: String,
    pub description: String,
    pub version: String,
    pub priority: SopPriority,
    pub execution_mode: SopExecutionMode,
    pub triggers: Vec<SopTrigger>,
    pub steps: Vec<SopStep>,
    #[serde(default = "default_cooldown_secs")]
    pub cooldown_secs: u64,
    #[serde(default = "default_max_concurrent")]
    pub max_concurrent: u32,
    #[serde(skip)]
    pub location: Option<PathBuf>,
}

fn default_cooldown_secs() -> u64 {
    0
}

fn default_max_concurrent() -> u32 {
    1
}

// ── TOML manifest (internal parse target) ───────────────────────

/// Top-level SOP.toml structure.
#[derive(Debug, Clone, Deserialize)]
pub(crate) struct SopManifest {
    pub sop: SopMeta,
    #[serde(default)]
    pub triggers: Vec<SopTrigger>,
}

/// The `[sop]` table in SOP.toml.
#[derive(Debug, Clone, Deserialize)]
pub(crate) struct SopMeta {
    pub name: String,
    pub description: String,
    #[serde(default = "default_sop_version")]
    pub version: String,
    #[serde(default)]
    pub priority: SopPriority,
    #[serde(default)]
    pub execution_mode: Option<SopExecutionMode>,
    #[serde(default = "default_cooldown_secs")]
    pub cooldown_secs: u64,
    #[serde(default = "default_max_concurrent")]
    pub max_concurrent: u32,
}

fn default_sop_version() -> String {
    "0.1.0".to_string()
}

// ── Event ────────────────────────────────────────────────────────

/// The source type of an incoming event that may trigger an SOP.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Hash, Serialize, Deserialize)]
#[serde(rename_all = "lowercase")]
pub enum SopTriggerSource {
    Mqtt,
    Webhook,
    Cron,
    Peripheral,
    Manual,
}

impl fmt::Display for SopTriggerSource {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        match self {
            Self::Mqtt => write!(f, "mqtt"),
            Self::Webhook => write!(f, "webhook"),
            Self::Cron => write!(f, "cron"),
            Self::Peripheral => write!(f, "peripheral"),
            Self::Manual => write!(f, "manual"),
        }
    }
}

/// An incoming event that may trigger one or more SOPs.
#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct SopEvent {
    pub source: SopTriggerSource,
    /// Topic, path, or signal identifier (depends on source type).
    #[serde(default)]
    pub topic: Option<String>,
    /// Raw payload (JSON string, sensor reading, etc.).
    #[serde(default)]
    pub payload: Option<String>,
    /// When the event occurred (ISO-8601).
    pub timestamp: String,
}

// ── Run state ────────────────────────────────────────────────────

/// Status of an SOP execution run.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum SopRunStatus {
    Pending,
    Running,
    WaitingApproval,
    Completed,
    Failed,
    Cancelled,
}

impl fmt::Display for SopRunStatus {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        match self {
            Self::Pending => write!(f, "pending"),
            Self::Running => write!(f, "running"),
            Self::WaitingApproval => write!(f, "waiting_approval"),
            Self::Completed => write!(f, "completed"),
            Self::Failed => write!(f, "failed"),
            Self::Cancelled => write!(f, "cancelled"),
        }
    }
}

/// Result status of a single step execution.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum SopStepStatus {
    Completed,
    Failed,
    Skipped,
}

impl fmt::Display for SopStepStatus {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        match self {
            Self::Completed => write!(f, "completed"),
            Self::Failed => write!(f, "failed"),
            Self::Skipped => write!(f, "skipped"),
        }
    }
}

/// Result of executing a single SOP step.
#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct SopStepResult {
    pub step_number: u32,
    pub status: SopStepStatus,
    pub output: String,
    pub started_at: String,
    pub completed_at: Option<String>,
}

/// A full SOP execution run (from trigger to completion).
#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct SopRun {
    pub run_id: String,
    pub sop_name: String,
    pub trigger_event: SopEvent,
    pub status: SopRunStatus,
    pub current_step: u32,
    pub total_steps: u32,
    pub started_at: String,
    pub completed_at: Option<String>,
    pub step_results: Vec<SopStepResult>,
    /// ISO-8601 timestamp when the run entered WaitingApproval (for timeout tracking).
    #[serde(default)]
    pub waiting_since: Option<String>,
}

/// What the engine instructs the caller to do next after a state transition.
#[derive(Debug, Clone)]
pub enum SopRunAction {
    /// Inject this step into the agent for execution.
    ExecuteStep {
        run_id: String,
        step: SopStep,
        context: String,
    },
    /// Pause and wait for operator approval before executing this step.
    WaitApproval {
        run_id: String,
        step: SopStep,
        context: String,
    },
    /// The SOP run completed successfully.
    Completed { run_id: String, sop_name: String },
    /// The SOP run failed.
    Failed {
        run_id: String,
        sop_name: String,
        reason: String,
    },
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn priority_display() {
        assert_eq!(SopPriority::Critical.to_string(), "critical");
        assert_eq!(SopPriority::Low.to_string(), "low");
    }

    #[test]
    fn execution_mode_display() {
        assert_eq!(SopExecutionMode::Auto.to_string(), "auto");
        assert_eq!(
            SopExecutionMode::PriorityBased.to_string(),
            "priority_based"
        );
    }

    #[test]
    fn trigger_display() {
        let mqtt = SopTrigger::Mqtt {
            topic: "sensors/temp".into(),
            condition: Some("$.value > 85".into()),
        };
        assert_eq!(mqtt.to_string(), "mqtt:sensors/temp");

        let manual = SopTrigger::Manual;
        assert_eq!(manual.to_string(), "manual");
    }

    #[test]
    fn priority_serde_roundtrip() {
        let json = serde_json::to_string(&SopPriority::Critical).unwrap();
        assert_eq!(json, "\"critical\"");
        let parsed: SopPriority = serde_json::from_str(&json).unwrap();
        assert_eq!(parsed, SopPriority::Critical);
    }

    #[test]
    fn execution_mode_serde_roundtrip() {
        let json = serde_json::to_string(&SopExecutionMode::PriorityBased).unwrap();
        assert_eq!(json, "\"priority_based\"");
        let parsed: SopExecutionMode = serde_json::from_str(&json).unwrap();
        assert_eq!(parsed, SopExecutionMode::PriorityBased);
    }

    #[test]
    fn trigger_toml_roundtrip() {
        let toml_str = r#"
type = "mqtt"
topic = "facility/pump/pressure"
condition = "$.value > 85"
"#;
        let trigger: SopTrigger = toml::from_str(toml_str).unwrap();
        assert!(
            matches!(trigger, SopTrigger::Mqtt { ref topic, .. } if topic == "facility/pump/pressure")
        );
    }

    #[test]
    fn trigger_manual_toml() {
        let toml_str = r#"type = "manual""#;
        let trigger: SopTrigger = toml::from_str(toml_str).unwrap();
        assert_eq!(trigger, SopTrigger::Manual);
    }

    #[test]
    fn run_status_display() {
        assert_eq!(
            SopRunStatus::WaitingApproval.to_string(),
            "waiting_approval"
        );
    }

    #[test]
    fn step_defaults() {
        let step: SopStep =
            serde_json::from_str(r#"{"number": 1, "title": "Check", "body": "Verify readings"}"#)
                .unwrap();
        assert!(step.suggested_tools.is_empty());
        assert!(!step.requires_confirmation);
    }

    #[test]
    fn manifest_parse() {
        let toml_str = r#"
[sop]
name = "test-sop"
description = "A test SOP"

[[triggers]]
type = "manual"

[[triggers]]
type = "webhook"
path = "/sop/test"
"#;
        let manifest: SopManifest = toml::from_str(toml_str).unwrap();
        assert_eq!(manifest.sop.name, "test-sop");
        assert_eq!(manifest.triggers.len(), 2);
        assert_eq!(manifest.sop.priority, SopPriority::Normal);
        assert_eq!(manifest.sop.execution_mode, None);
    }

    #[test]
    fn trigger_source_display() {
        assert_eq!(SopTriggerSource::Mqtt.to_string(), "mqtt");
        assert_eq!(SopTriggerSource::Manual.to_string(), "manual");
    }

    #[test]
    fn step_status_display() {
        assert_eq!(SopStepStatus::Completed.to_string(), "completed");
        assert_eq!(SopStepStatus::Failed.to_string(), "failed");
        assert_eq!(SopStepStatus::Skipped.to_string(), "skipped");
    }

    #[test]
    fn sop_event_serde_roundtrip() {
        let event = SopEvent {
            source: SopTriggerSource::Mqtt,
            topic: Some("sensors/pressure".into()),
            payload: Some(r#"{"value": 87.3}"#.into()),
            timestamp: "2026-02-19T12:00:00Z".into(),
        };
        let json = serde_json::to_string(&event).unwrap();
        let parsed: SopEvent = serde_json::from_str(&json).unwrap();
        assert_eq!(parsed.source, SopTriggerSource::Mqtt);
        assert_eq!(parsed.topic.as_deref(), Some("sensors/pressure"));
    }

    #[test]
    fn sop_run_serde_roundtrip() {
        let run = SopRun {
            run_id: "run-001".into(),
            sop_name: "test-sop".into(),
            trigger_event: SopEvent {
                source: SopTriggerSource::Manual,
                topic: None,
                payload: None,
                timestamp: "2026-02-19T12:00:00Z".into(),
            },
            status: SopRunStatus::Running,
            current_step: 2,
            total_steps: 5,
            started_at: "2026-02-19T12:00:00Z".into(),
            completed_at: None,
            step_results: vec![SopStepResult {
                step_number: 1,
                status: SopStepStatus::Completed,
                output: "Step 1 done".into(),
                started_at: "2026-02-19T12:00:00Z".into(),
                completed_at: Some("2026-02-19T12:00:05Z".into()),
            }],
            waiting_since: None,
        };
        let json = serde_json::to_string(&run).unwrap();
        let parsed: SopRun = serde_json::from_str(&json).unwrap();
        assert_eq!(parsed.run_id, "run-001");
        assert_eq!(parsed.status, SopRunStatus::Running);
        assert_eq!(parsed.step_results.len(), 1);
        assert_eq!(parsed.step_results[0].status, SopStepStatus::Completed);
    }
}
