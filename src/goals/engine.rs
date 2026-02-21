use anyhow::Result;
use serde::{Deserialize, Serialize};
use std::fmt::Write as _;
use std::path::{Path, PathBuf};

/// Maximum retry attempts per step before marking the goal as blocked.
const MAX_STEP_ATTEMPTS: u32 = 3;

// ── Data Structures ─────────────────────────────────────────────

/// Root state persisted to `{workspace}/state/goals.json`.
/// Format matches the `goal-tracker` skill's file layout.
#[derive(Debug, Clone, Serialize, Deserialize, Default)]
pub struct GoalState {
    #[serde(default)]
    pub goals: Vec<Goal>,
}

#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct Goal {
    pub id: String,
    pub description: String,
    #[serde(default)]
    pub status: GoalStatus,
    #[serde(default)]
    pub priority: GoalPriority,
    #[serde(default)]
    pub created_at: String,
    #[serde(default)]
    pub updated_at: String,
    #[serde(default)]
    pub steps: Vec<Step>,
    /// Accumulated context from previous step results.
    #[serde(default)]
    pub context: String,
    /// Last error encountered during step execution.
    #[serde(default)]
    pub last_error: Option<String>,
}

#[derive(Debug, Clone, Serialize, Deserialize, PartialEq, Eq, Default)]
#[serde(rename_all = "snake_case")]
pub enum GoalStatus {
    #[default]
    Pending,
    InProgress,
    Completed,
    Blocked,
    Cancelled,
}

#[derive(Debug, Clone, Copy, Serialize, Deserialize, PartialEq, Eq, Default)]
#[serde(rename_all = "snake_case")]
pub enum GoalPriority {
    Low = 0,
    #[default]
    Medium = 1,
    High = 2,
    Critical = 3,
}

impl PartialOrd for GoalPriority {
    fn partial_cmp(&self, other: &Self) -> Option<std::cmp::Ordering> {
        Some(self.cmp(other))
    }
}

impl Ord for GoalPriority {
    fn cmp(&self, other: &Self) -> std::cmp::Ordering {
        (*self as u8).cmp(&(*other as u8))
    }
}

#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct Step {
    pub id: String,
    pub description: String,
    #[serde(default)]
    pub status: StepStatus,
    #[serde(default)]
    pub result: Option<String>,
    #[serde(default)]
    pub attempts: u32,
}

#[derive(Debug, Clone, Serialize, Deserialize, PartialEq, Eq, Default)]
#[serde(rename_all = "snake_case")]
pub enum StepStatus {
    #[default]
    Pending,
    InProgress,
    Completed,
    Failed,
}

// ── GoalEngine ──────────────────────────────────────────────────

pub struct GoalEngine {
    state_path: PathBuf,
}

impl GoalEngine {
    pub fn new(workspace_dir: &Path) -> Self {
        Self {
            state_path: workspace_dir.join("state").join("goals.json"),
        }
    }

    /// Load goal state from disk. Returns empty state if file doesn't exist.
    pub async fn load_state(&self) -> Result<GoalState> {
        if !self.state_path.exists() {
            return Ok(GoalState::default());
        }
        let bytes = tokio::fs::read(&self.state_path).await?;
        if bytes.is_empty() {
            return Ok(GoalState::default());
        }
        let state: GoalState = serde_json::from_slice(&bytes)?;
        Ok(state)
    }

    /// Atomic save: write to .tmp then rename.
    pub async fn save_state(&self, state: &GoalState) -> Result<()> {
        if let Some(parent) = self.state_path.parent() {
            tokio::fs::create_dir_all(parent).await?;
        }
        let tmp = self.state_path.with_extension("json.tmp");
        let data = serde_json::to_vec_pretty(state)?;
        tokio::fs::write(&tmp, data).await?;
        tokio::fs::rename(&tmp, &self.state_path).await?;
        Ok(())
    }

    /// Select the next actionable (goal_index, step_index) pair.
    ///
    /// Strategy: highest-priority in-progress goal, first pending step
    /// that hasn't exceeded `MAX_STEP_ATTEMPTS`.
    pub fn select_next_actionable(state: &GoalState) -> Option<(usize, usize)> {
        let mut best: Option<(usize, usize, GoalPriority)> = None;

        for (gi, goal) in state.goals.iter().enumerate() {
            if goal.status != GoalStatus::InProgress {
                continue;
            }
            if let Some(si) = goal
                .steps
                .iter()
                .position(|s| s.status == StepStatus::Pending && s.attempts < MAX_STEP_ATTEMPTS)
            {
                match best {
                    Some((_, _, ref bp)) if goal.priority <= *bp => {}
                    _ => best = Some((gi, si, goal.priority)),
                }
            }
        }

        best.map(|(gi, si, _)| (gi, si))
    }

    /// Build a focused prompt for the agent to execute one step.
    pub fn build_step_prompt(goal: &Goal, step: &Step) -> String {
        let mut prompt = String::new();

        let _ = writeln!(
            prompt,
            "[Goal Loop] Executing step for goal: {}\n",
            goal.description
        );

        // Completed steps summary
        let completed: Vec<&Step> = goal
            .steps
            .iter()
            .filter(|s| s.status == StepStatus::Completed)
            .collect();
        if !completed.is_empty() {
            prompt.push_str("Completed steps:\n");
            for s in &completed {
                let _ = writeln!(
                    prompt,
                    "- [done] {}: {}",
                    s.description,
                    s.result.as_deref().unwrap_or("(no result)")
                );
            }
            prompt.push('\n');
        }

        // Accumulated context
        if !goal.context.is_empty() {
            let _ = write!(prompt, "Context so far:\n{}\n\n", goal.context);
        }

        // Current step
        let _ = write!(
            prompt,
            "Current step: {}\n\
             Please execute this step. Provide a clear summary of what you did and the outcome.\n",
            step.description
        );

        // Retry warning
        if step.attempts > 0 {
            let _ = write!(
                prompt,
                "\nWARNING: This step has failed {} time(s) before. \
                 Last error: {}\n\
                 Try a different approach.\n",
                step.attempts,
                goal.last_error.as_deref().unwrap_or("unknown")
            );
        }

        prompt
    }

    /// Simple heuristic: output containing error indicators → failure.
    pub fn interpret_result(output: &str) -> bool {
        let lower = output.to_ascii_lowercase();
        let failure_indicators = [
            "failed to",
            "error:",
            "unable to",
            "cannot ",
            "could not",
            "fatal:",
            "panic:",
        ];
        !failure_indicators.iter().any(|ind| lower.contains(ind))
    }

    pub fn max_step_attempts() -> u32 {
        MAX_STEP_ATTEMPTS
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use tempfile::TempDir;

    fn sample_goal_state() -> GoalState {
        GoalState {
            goals: vec![
                Goal {
                    id: "g1".into(),
                    description: "Build automation platform".into(),
                    status: GoalStatus::InProgress,
                    priority: GoalPriority::High,
                    created_at: "2026-01-01T00:00:00Z".into(),
                    updated_at: "2026-01-01T00:00:00Z".into(),
                    steps: vec![
                        Step {
                            id: "s1".into(),
                            description: "Research tools".into(),
                            status: StepStatus::Completed,
                            result: Some("Found 3 tools".into()),
                            attempts: 1,
                        },
                        Step {
                            id: "s2".into(),
                            description: "Setup environment".into(),
                            status: StepStatus::Pending,
                            result: None,
                            attempts: 0,
                        },
                        Step {
                            id: "s3".into(),
                            description: "Write code".into(),
                            status: StepStatus::Pending,
                            result: None,
                            attempts: 0,
                        },
                    ],
                    context: "Using Python + Selenium".into(),
                    last_error: None,
                },
                Goal {
                    id: "g2".into(),
                    description: "Learn Rust".into(),
                    status: GoalStatus::InProgress,
                    priority: GoalPriority::Medium,
                    created_at: "2026-01-02T00:00:00Z".into(),
                    updated_at: "2026-01-02T00:00:00Z".into(),
                    steps: vec![Step {
                        id: "s1".into(),
                        description: "Read the book".into(),
                        status: StepStatus::Pending,
                        result: None,
                        attempts: 0,
                    }],
                    context: String::new(),
                    last_error: None,
                },
            ],
        }
    }

    #[test]
    fn goal_loop_config_serde_roundtrip() {
        let toml_str = r#"
enabled = true
interval_minutes = 15
step_timeout_secs = 180
max_steps_per_cycle = 5
channel = "lark"
target = "oc_test"
"#;
        let config: crate::config::GoalLoopConfig = toml::from_str(toml_str).unwrap();
        assert!(config.enabled);
        assert_eq!(config.interval_minutes, 15);
        assert_eq!(config.step_timeout_secs, 180);
        assert_eq!(config.max_steps_per_cycle, 5);
        assert_eq!(config.channel.as_deref(), Some("lark"));
        assert_eq!(config.target.as_deref(), Some("oc_test"));
    }

    #[test]
    fn goal_loop_config_defaults() {
        let config = crate::config::GoalLoopConfig::default();
        assert!(!config.enabled);
        assert_eq!(config.interval_minutes, 10);
        assert_eq!(config.step_timeout_secs, 120);
        assert_eq!(config.max_steps_per_cycle, 3);
        assert!(config.channel.is_none());
        assert!(config.target.is_none());
    }

    #[test]
    fn goal_state_serde_roundtrip() {
        let state = sample_goal_state();
        let json = serde_json::to_string_pretty(&state).unwrap();
        let parsed: GoalState = serde_json::from_str(&json).unwrap();
        assert_eq!(parsed.goals.len(), 2);
        assert_eq!(parsed.goals[0].steps.len(), 3);
        assert_eq!(parsed.goals[0].steps[0].status, StepStatus::Completed);
    }

    #[test]
    fn select_next_actionable_picks_highest_priority() {
        let state = sample_goal_state();
        let result = GoalEngine::select_next_actionable(&state);
        // g1 (High) step s2 should be selected over g2 (Medium)
        assert_eq!(result, Some((0, 1)));
    }

    #[test]
    fn select_next_actionable_skips_exhausted_steps() {
        let mut state = sample_goal_state();
        // Exhaust s2 attempts
        state.goals[0].steps[1].attempts = MAX_STEP_ATTEMPTS;
        let result = GoalEngine::select_next_actionable(&state);
        // Should skip s2, pick s3
        assert_eq!(result, Some((0, 2)));
    }

    #[test]
    fn select_next_actionable_skips_non_in_progress_goals() {
        let mut state = sample_goal_state();
        state.goals[0].status = GoalStatus::Completed;
        let result = GoalEngine::select_next_actionable(&state);
        // g1 completed, should pick g2 s1
        assert_eq!(result, Some((1, 0)));
    }

    #[test]
    fn select_next_actionable_returns_none_when_nothing_actionable() {
        let state = GoalState::default();
        assert!(GoalEngine::select_next_actionable(&state).is_none());
    }

    #[test]
    fn build_step_prompt_includes_goal_and_step() {
        let state = sample_goal_state();
        let prompt = GoalEngine::build_step_prompt(&state.goals[0], &state.goals[0].steps[1]);
        assert!(prompt.contains("Build automation platform"));
        assert!(prompt.contains("Setup environment"));
        assert!(prompt.contains("Research tools"));
        assert!(prompt.contains("Using Python + Selenium"));
        assert!(!prompt.contains("WARNING")); // no retries yet
    }

    #[test]
    fn build_step_prompt_includes_retry_warning() {
        let mut state = sample_goal_state();
        state.goals[0].steps[1].attempts = 2;
        state.goals[0].last_error = Some("connection refused".into());
        let prompt = GoalEngine::build_step_prompt(&state.goals[0], &state.goals[0].steps[1]);
        assert!(prompt.contains("WARNING"));
        assert!(prompt.contains("2 time(s)"));
        assert!(prompt.contains("connection refused"));
    }

    #[test]
    fn interpret_result_success() {
        assert!(GoalEngine::interpret_result(
            "Successfully set up the environment"
        ));
        assert!(GoalEngine::interpret_result("Done. All tasks completed."));
    }

    #[test]
    fn interpret_result_failure() {
        assert!(!GoalEngine::interpret_result("Failed to install package"));
        assert!(!GoalEngine::interpret_result(
            "Error: connection timeout occurred"
        ));
        assert!(!GoalEngine::interpret_result("Unable to find the resource"));
        assert!(!GoalEngine::interpret_result("cannot open file"));
        assert!(!GoalEngine::interpret_result("Fatal: repository not found"));
    }

    #[tokio::test]
    async fn load_save_state_roundtrip() {
        let tmp = TempDir::new().unwrap();
        let engine = GoalEngine::new(tmp.path());

        // Initially empty
        let empty = engine.load_state().await.unwrap();
        assert!(empty.goals.is_empty());

        // Save and reload
        let state = sample_goal_state();
        engine.save_state(&state).await.unwrap();
        let loaded = engine.load_state().await.unwrap();
        assert_eq!(loaded.goals.len(), 2);
        assert_eq!(loaded.goals[0].id, "g1");
        assert_eq!(loaded.goals[1].priority, GoalPriority::Medium);
    }

    #[test]
    fn priority_ordering() {
        assert!(GoalPriority::Critical > GoalPriority::High);
        assert!(GoalPriority::High > GoalPriority::Medium);
        assert!(GoalPriority::Medium > GoalPriority::Low);
    }

    #[test]
    fn goal_status_default_is_pending() {
        assert_eq!(GoalStatus::default(), GoalStatus::Pending);
    }

    #[test]
    fn step_status_default_is_pending() {
        assert_eq!(StepStatus::default(), StepStatus::Pending);
    }
}
