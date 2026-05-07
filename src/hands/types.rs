use chrono::{DateTime, Utc};
use serde::{Deserialize, Serialize};

use crate::cron::Schedule;

// ── Hand ───────────────────────────────────────────────────────

/// A Hand is an autonomous agent package that runs on a schedule,
/// accumulates knowledge over time, and reports results.
///
/// Hands are defined as TOML files in `~/.zeroclaw/hands/` and each
/// maintains a rolling context of findings across runs so the agent
/// grows smarter with every execution.
#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct Hand {
    /// Unique name (also used as directory/file stem)
    pub name: String,
    /// Human-readable description of what this hand does
    pub description: String,
    /// The schedule this hand runs on (reuses cron schedule types)
    pub schedule: Schedule,
    /// System prompt / execution plan for this hand
    pub prompt: String,
    /// Domain knowledge lines to inject into context
    #[serde(default)]
    pub knowledge: Vec<String>,
    /// Tools this hand is allowed to use (None = all available)
    #[serde(default)]
    pub allowed_tools: Option<Vec<String>>,
    /// Model override for this hand (None = default model_provider)
    #[serde(default)]
    pub model: Option<String>,
    /// Whether this hand is currently active
    #[serde(default = "default_true")]
    pub active: bool,
    /// Maximum runs to keep in history
    #[serde(default = "default_max_runs")]
    pub max_history: usize,
}

fn default_true() -> bool {
    true
}

fn default_max_runs() -> usize {
    100
}

// ── Hand Run ───────────────────────────────────────────────────

/// The status of a single hand execution.
#[derive(Debug, Clone, Serialize, Deserialize, PartialEq, Eq)]
#[serde(rename_all = "snake_case", tag = "status")]
pub enum HandRunStatus {
    Running,
    Completed,
    Failed { error: String },
}

/// Record of a single hand execution.
#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct HandRun {
    /// Name of the hand that produced this run
    pub hand_name: String,
    /// Unique identifier for this run
    pub run_id: String,
    /// When the run started
    pub started_at: DateTime<Utc>,
    /// When the run finished (None if still running)
    pub finished_at: Option<DateTime<Utc>>,
    /// Outcome of the run
    pub status: HandRunStatus,
    /// Key findings/outputs extracted from this run
    #[serde(default)]
    pub findings: Vec<String>,
    /// New knowledge accumulated and stored to memory
    #[serde(default)]
    pub knowledge_added: Vec<String>,
    /// Wall-clock duration in milliseconds
    pub duration_ms: Option<u64>,
}

// ── Hand Context ───────────────────────────────────────────────

/// Rolling context that accumulates across hand runs.
///
/// Persisted as `~/.zeroclaw/hands/{name}/context.json`.
#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct HandContext {
    /// Name of the hand this context belongs to
    pub hand_name: String,
    /// Past runs, most-recent first, capped at `Hand::max_history`
    #[serde(default)]
    pub history: Vec<HandRun>,
    /// Persistent facts learned across runs
    #[serde(default)]
    pub learned_facts: Vec<String>,
    /// Timestamp of the last completed run
    pub last_run: Option<DateTime<Utc>>,
    /// Total number of successful runs
    #[serde(default)]
    pub total_runs: u64,
}

impl HandContext {
    /// Create a fresh, empty context for a hand.
    pub fn new(hand_name: &str) -> Self {
        Self {
            hand_name: hand_name.to_string(),
            history: Vec::new(),
            learned_facts: Vec::new(),
            last_run: None,
            total_runs: 0,
        }
    }

    /// Record a completed run, updating counters and trimming history.
    pub fn record_run(&mut self, run: HandRun, max_history: usize) {
        if run.status == (HandRunStatus::Completed) {
            self.total_runs += 1;
            self.last_run = run.finished_at;
        }

        // Merge new knowledge
        for fact in &run.knowledge_added {
            if !self.learned_facts.contains(fact) {
                self.learned_facts.push(fact.clone());
            }
        }

        // Insert at the front (most-recent first)
        self.history.insert(0, run);

        // Cap history length
        self.history.truncate(max_history);
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::cron::Schedule;

    fn sample_hand() -> Hand {
        Hand {
            name: "market-scanner".into(),
            description: "Scans market trends and reports findings".into(),
            schedule: Schedule::Cron {
                expr: "0 9 * * 1-5".into(),
                tz: Some("America/New_York".into()),
            },
            prompt: "Scan market trends and report key findings.".into(),
            knowledge: vec!["Focus on tech sector.".into()],
            allowed_tools: Some(vec!["web_search".into(), "memory".into()]),
            model: Some("claude-opus-4-6".into()),
            active: true,
            max_history: 50,
        }
    }

    fn sample_run(name: &str, status: HandRunStatus) -> HandRun {
        let now = Utc::now();
        HandRun {
            hand_name: name.into(),
            run_id: uuid::Uuid::new_v4().to_string(),
            started_at: now,
            finished_at: Some(now),
            status,
            findings: vec!["finding-1".into()],
            knowledge_added: vec!["learned-fact-A".into()],
            duration_ms: Some(1234),
        }
    }

    // ── Deserialization ────────────────────────────────────────

    #[test]
    fn hand_deserializes_from_toml() {
        let toml_str = r#"
name = "market-scanner"
description = "Scans market trends"
prompt = "Scan trends."

[schedule]
kind = "cron"
expr = "0 9 * * 1-5"
tz = "America/New_York"
"#;
        let hand: Hand = toml::from_str(toml_str).unwrap();
        assert_eq!(hand.name, "market-scanner");
        assert!(hand.active, "active should default to true");
        assert_eq!(hand.max_history, 100, "max_history should default to 100");
        assert!(hand.knowledge.is_empty());
        assert!(hand.allowed_tools.is_none());
        assert!(hand.model.is_none());
    }

    #[test]
    fn hand_deserializes_full_toml() {
        let toml_str = r#"
name = "news-digest"
description = "Daily news digest"
prompt = "Summarize the day's news."
knowledge = ["focus on AI", "include funding rounds"]
allowed_tools = ["web_search"]
model = "claude-opus-4-6"
active = false
max_history = 25

[schedule]
kind = "every"
every_ms = 3600000
"#;
        let hand: Hand = toml::from_str(toml_str).unwrap();
        assert_eq!(hand.name, "news-digest");
        assert!(!hand.active);
        assert_eq!(hand.max_history, 25);
        assert_eq!(hand.knowledge.len(), 2);
        assert_eq!(hand.allowed_tools.as_ref().unwrap().len(), 1);
        assert_eq!(hand.model.as_deref(), Some("claude-opus-4-6"));
        assert!(matches!(
            hand.schedule,
            Schedule::Every {
                every_ms: 3_600_000
            }
        ));
    }

    #[test]
    fn hand_roundtrip_json() {
        let hand = sample_hand();
        let json = serde_json::to_string(&hand).unwrap();
        let parsed: Hand = serde_json::from_str(&json).unwrap();
        assert_eq!(parsed.name, hand.name);
        assert_eq!(parsed.max_history, hand.max_history);
    }

    // ── HandRunStatus ──────────────────────────────────────────

    #[test]
    fn hand_run_status_serde_roundtrip() {
        let statuses = vec![
            HandRunStatus::Running,
            HandRunStatus::Completed,
            HandRunStatus::Failed {
                error: "timeout".into(),
            },
        ];
        for status in statuses {
            let json = serde_json::to_string(&status).unwrap();
            let parsed: HandRunStatus = serde_json::from_str(&json).unwrap();
            assert_eq!(parsed, status);
        }
    }

    // ── HandContext ────────────────────────────────────────────

    #[test]
    fn context_new_is_empty() {
        let ctx = HandContext::new("test-hand");
        assert_eq!(ctx.hand_name, "test-hand");
        assert!(ctx.history.is_empty());
        assert!(ctx.learned_facts.is_empty());
        assert!(ctx.last_run.is_none());
        assert_eq!(ctx.total_runs, 0);
    }

    #[test]
    fn context_record_run_increments_counters() {
        let mut ctx = HandContext::new("scanner");
        let run = sample_run("scanner", HandRunStatus::Completed);
        ctx.record_run(run, 100);

        assert_eq!(ctx.total_runs, 1);
        assert!(ctx.last_run.is_some());
        assert_eq!(ctx.history.len(), 1);
        assert_eq!(ctx.learned_facts, vec!["learned-fact-A"]);
    }

    #[test]
    fn context_record_failed_run_does_not_increment_total() {
        let mut ctx = HandContext::new("scanner");
        let run = sample_run(
            "scanner",
            HandRunStatus::Failed {
                error: "boom".into(),
            },
        );
        ctx.record_run(run, 100);

        assert_eq!(ctx.total_runs, 0);
        assert!(ctx.last_run.is_none());
        assert_eq!(ctx.history.len(), 1);
    }

    #[test]
    fn context_caps_history_at_max() {
        let mut ctx = HandContext::new("scanner");
        for _ in 0..10 {
            let run = sample_run("scanner", HandRunStatus::Completed);
            ctx.record_run(run, 3);
        }
        assert_eq!(ctx.history.len(), 3);
        assert_eq!(ctx.total_runs, 10);
    }

    #[test]
    fn context_deduplicates_learned_facts() {
        let mut ctx = HandContext::new("scanner");
        let run1 = sample_run("scanner", HandRunStatus::Completed);
        let run2 = sample_run("scanner", HandRunStatus::Completed);
        ctx.record_run(run1, 100);
        ctx.record_run(run2, 100);

        // Both runs add "learned-fact-A" but it should appear only once
        assert_eq!(ctx.learned_facts.len(), 1);
    }

    #[test]
    fn context_json_roundtrip() {
        let mut ctx = HandContext::new("scanner");
        let run = sample_run("scanner", HandRunStatus::Completed);
        ctx.record_run(run, 100);

        let json = serde_json::to_string_pretty(&ctx).unwrap();
        let parsed: HandContext = serde_json::from_str(&json).unwrap();

        assert_eq!(parsed.hand_name, "scanner");
        assert_eq!(parsed.total_runs, 1);
        assert_eq!(parsed.history.len(), 1);
        assert_eq!(parsed.learned_facts, vec!["learned-fact-A"]);
    }

    #[test]
    fn most_recent_run_is_first_in_history() {
        let mut ctx = HandContext::new("scanner");
        for i in 0..3 {
            let mut run = sample_run("scanner", HandRunStatus::Completed);
            run.findings = vec![format!("finding-{i}")];
            ctx.record_run(run, 100);
        }
        assert_eq!(ctx.history[0].findings[0], "finding-2");
        assert_eq!(ctx.history[2].findings[0], "finding-0");
    }
}
