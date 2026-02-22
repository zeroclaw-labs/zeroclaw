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

#[derive(Debug, Clone, Serialize, PartialEq, Eq, Default)]
#[serde(rename_all = "snake_case")]
pub enum GoalStatus {
    #[default]
    Pending,
    InProgress,
    Completed,
    Blocked,
    Cancelled,
}

impl<'de> Deserialize<'de> for GoalStatus {
    fn deserialize<D: serde::Deserializer<'de>>(d: D) -> Result<Self, D::Error> {
        let s = String::deserialize(d)?;
        Ok(match s.as_str() {
            "in_progress" => Self::InProgress,
            "completed" => Self::Completed,
            "blocked" => Self::Blocked,
            "cancelled" => Self::Cancelled,
            _ => Self::Pending,
        })
    }
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

#[derive(Debug, Clone, Serialize, PartialEq, Eq, Default)]
#[serde(rename_all = "snake_case")]
pub enum StepStatus {
    #[default]
    Pending,
    InProgress,
    Completed,
    Failed,
    Blocked,
}

impl<'de> Deserialize<'de> for StepStatus {
    fn deserialize<D: serde::Deserializer<'de>>(d: D) -> Result<Self, D::Error> {
        let s = String::deserialize(d)?;
        Ok(match s.as_str() {
            "in_progress" => Self::InProgress,
            "completed" => Self::Completed,
            "failed" => Self::Failed,
            "blocked" => Self::Blocked,
            _ => Self::Pending,
        })
    }
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

    /// Find in-progress goals that have no actionable steps remaining.
    ///
    /// A goal is "stalled" when it is `InProgress` but every step is either
    /// completed, blocked, or has exhausted its retry attempts. These goals
    /// need a reflection session to decide: add new steps, mark completed,
    /// mark blocked, or escalate to the user.
    pub fn find_stalled_goals(state: &GoalState) -> Vec<usize> {
        state
            .goals
            .iter()
            .enumerate()
            .filter(|(_, g)| g.status == GoalStatus::InProgress)
            .filter(|(_, g)| {
                !g.steps.is_empty()
                    && !g.steps.iter().any(|s| {
                        s.status == StepStatus::Pending && s.attempts < MAX_STEP_ATTEMPTS
                    })
            })
            .map(|(i, _)| i)
            .collect()
    }

    /// Build a reflection prompt for a stalled goal.
    ///
    /// The agent is asked to review the goal's overall progress and decide
    /// what to do next: add new steps, mark the goal completed, or escalate.
    pub fn build_reflection_prompt(goal: &Goal) -> String {
        let mut prompt = String::new();

        let _ = writeln!(
            prompt,
            "[Goal Reflection] Goal: {}\n",
            goal.description
        );

        prompt.push_str("All steps have been attempted. Here is the current state:\n\n");

        for s in &goal.steps {
            let status_tag = match s.status {
                StepStatus::Completed => "done",
                StepStatus::Failed | StepStatus::Blocked => "blocked",
                _ if s.attempts >= MAX_STEP_ATTEMPTS => "exhausted",
                _ => "pending",
            };
            let result = s.result.as_deref().unwrap_or("(no result)");
            let _ = writeln!(prompt, "- [{status_tag}] {}: {result}", s.description);
        }

        if !goal.context.is_empty() {
            let _ = write!(prompt, "\nAccumulated context:\n{}\n", goal.context);
        }

        if let Some(ref err) = goal.last_error {
            let _ = write!(prompt, "\nLast error: {err}\n");
        }

        prompt.push_str(
            "\nReflect on this goal and take ONE of the following actions:\n\
             1. If the goal is effectively achieved, update state/goals.json to mark it `completed`.\n\
             2. If some steps failed but you can try a different approach, add NEW steps to \
                state/goals.json with fresh descriptions (don't reuse failed step IDs).\n\
             3. If the goal is truly blocked and needs human input, mark it `blocked` in \
                state/goals.json and explain what you need from the user.\n\
             4. Use memory_store to record what you learned from the failures.\n\n\
             Be decisive. Do not leave the goal in its current state.",
        );

        prompt
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

    #[test]
    fn find_stalled_goals_detects_exhausted_steps() {
        let state = GoalState {
            goals: vec![Goal {
                id: "g1".into(),
                description: "Stalled goal".into(),
                status: GoalStatus::InProgress,
                priority: GoalPriority::High,
                created_at: String::new(),
                updated_at: String::new(),
                steps: vec![
                    Step {
                        id: "s1".into(),
                        description: "Done step".into(),
                        status: StepStatus::Completed,
                        result: Some("ok".into()),
                        attempts: 1,
                    },
                    Step {
                        id: "s2".into(),
                        description: "Exhausted step".into(),
                        status: StepStatus::Pending,
                        result: None,
                        attempts: 3, // >= MAX_STEP_ATTEMPTS
                    },
                ],
                context: String::new(),
                last_error: Some("step failed 3 times".into()),
            }],
        };

        let stalled = GoalEngine::find_stalled_goals(&state);
        assert_eq!(stalled, vec![0]);
    }

    #[test]
    fn find_stalled_goals_ignores_actionable_goals() {
        let state = sample_goal_state(); // has pending steps with attempts=0
        let stalled = GoalEngine::find_stalled_goals(&state);
        assert!(stalled.is_empty());
    }

    #[test]
    fn find_stalled_goals_ignores_completed_goals() {
        let state = GoalState {
            goals: vec![Goal {
                id: "g1".into(),
                description: "Done".into(),
                status: GoalStatus::Completed,
                priority: GoalPriority::Medium,
                created_at: String::new(),
                updated_at: String::new(),
                steps: vec![Step {
                    id: "s1".into(),
                    description: "Only step".into(),
                    status: StepStatus::Completed,
                    result: Some("ok".into()),
                    attempts: 1,
                }],
                context: String::new(),
                last_error: None,
            }],
        };

        let stalled = GoalEngine::find_stalled_goals(&state);
        assert!(stalled.is_empty());
    }

    #[test]
    fn build_reflection_prompt_includes_step_summary() {
        let goal = Goal {
            id: "g1".into(),
            description: "Test reflection".into(),
            status: GoalStatus::InProgress,
            priority: GoalPriority::High,
            created_at: String::new(),
            updated_at: String::new(),
            steps: vec![
                Step {
                    id: "s1".into(),
                    description: "Completed step".into(),
                    status: StepStatus::Completed,
                    result: Some("worked".into()),
                    attempts: 1,
                },
                Step {
                    id: "s2".into(),
                    description: "Failed step".into(),
                    status: StepStatus::Pending,
                    result: None,
                    attempts: 3,
                },
            ],
            context: "some context".into(),
            last_error: Some("policy_denied".into()),
        };

        let prompt = GoalEngine::build_reflection_prompt(&goal);
        assert!(prompt.contains("[Goal Reflection]"));
        assert!(prompt.contains("Test reflection"));
        assert!(prompt.contains("[done] Completed step"));
        assert!(prompt.contains("[exhausted] Failed step"));
        assert!(prompt.contains("some context"));
        assert!(prompt.contains("policy_denied"));
        assert!(prompt.contains("memory_store"));
    }

    // ── Self-healing deserialization tests ───────────────────────

    #[test]
    fn goal_status_deserializes_all_valid_variants() {
        let cases = vec![
            ("\"pending\"", GoalStatus::Pending),
            ("\"in_progress\"", GoalStatus::InProgress),
            ("\"completed\"", GoalStatus::Completed),
            ("\"blocked\"", GoalStatus::Blocked),
            ("\"cancelled\"", GoalStatus::Cancelled),
        ];
        for (json_str, expected) in cases {
            let parsed: GoalStatus =
                serde_json::from_str(json_str).unwrap_or_else(|e| panic!("{json_str}: {e}"));
            assert_eq!(parsed, expected, "GoalStatus mismatch for {json_str}");
        }
    }

    #[test]
    fn goal_status_self_healing_unknown_variants() {
        for variant in &["\"unknown\"", "\"invalid\"", "\"PENDING\"", "\"IN_PROGRESS\"", "\"\""] {
            let parsed: GoalStatus =
                serde_json::from_str(variant).unwrap_or_else(|e| panic!("{variant}: {e}"));
            assert_eq!(parsed, GoalStatus::Pending);
        }
    }

    #[test]
    fn step_status_deserializes_all_valid_variants() {
        let cases = vec![
            ("\"pending\"", StepStatus::Pending),
            ("\"in_progress\"", StepStatus::InProgress),
            ("\"completed\"", StepStatus::Completed),
            ("\"failed\"", StepStatus::Failed),
            ("\"blocked\"", StepStatus::Blocked),
        ];
        for (json_str, expected) in cases {
            let parsed: StepStatus =
                serde_json::from_str(json_str).unwrap_or_else(|e| panic!("{json_str}: {e}"));
            assert_eq!(parsed, expected, "StepStatus mismatch for {json_str}");
        }
    }

    #[test]
    fn step_status_self_healing_unknown_variants() {
        for variant in &["\"unknown\"", "\"done\"", "\"FAILED\"", "\"\""] {
            let parsed: StepStatus =
                serde_json::from_str(variant).unwrap_or_else(|e| panic!("{variant}: {e}"));
            assert_eq!(parsed, StepStatus::Pending);
        }
    }

    #[test]
    fn goal_status_self_healing_in_full_goal_json() {
        let json = r#"{"id":"g1","description":"test","status":"totally_bogus","steps":[]}"#;
        let goal: Goal = serde_json::from_str(json).unwrap();
        assert_eq!(goal.status, GoalStatus::Pending);
    }

    // ── find_stalled_goals edge cases ───────────────────────────

    #[test]
    fn find_stalled_goals_empty_steps_not_stalled() {
        let state = GoalState {
            goals: vec![Goal {
                id: "g1".into(),
                description: "No steps".into(),
                status: GoalStatus::InProgress,
                priority: GoalPriority::High,
                created_at: String::new(),
                updated_at: String::new(),
                steps: vec![],
                context: String::new(),
                last_error: None,
            }],
        };
        assert!(GoalEngine::find_stalled_goals(&state).is_empty());
    }

    #[test]
    fn find_stalled_goals_multiple_stalled() {
        let stalled_goal = |id: &str| Goal {
            id: id.into(),
            description: format!("Stalled {id}"),
            status: GoalStatus::InProgress,
            priority: GoalPriority::Medium,
            created_at: String::new(),
            updated_at: String::new(),
            steps: vec![Step {
                id: "s1".into(),
                description: "Exhausted".into(),
                status: StepStatus::Pending,
                result: None,
                attempts: MAX_STEP_ATTEMPTS,
            }],
            context: String::new(),
            last_error: None,
        };
        let state = GoalState {
            goals: vec![stalled_goal("g1"), stalled_goal("g2"), stalled_goal("g3")],
        };
        assert_eq!(GoalEngine::find_stalled_goals(&state), vec![0, 1, 2]);
    }

    #[test]
    fn find_stalled_goals_all_steps_completed_is_stalled() {
        let state = GoalState {
            goals: vec![Goal {
                id: "g1".into(),
                description: "All done but still in-progress".into(),
                status: GoalStatus::InProgress,
                priority: GoalPriority::High,
                created_at: String::new(),
                updated_at: String::new(),
                steps: vec![
                    Step {
                        id: "s1".into(),
                        description: "Done".into(),
                        status: StepStatus::Completed,
                        result: Some("ok".into()),
                        attempts: 1,
                    },
                    Step {
                        id: "s2".into(),
                        description: "Also done".into(),
                        status: StepStatus::Completed,
                        result: Some("ok".into()),
                        attempts: 1,
                    },
                ],
                context: String::new(),
                last_error: None,
            }],
        };
        assert_eq!(GoalEngine::find_stalled_goals(&state), vec![0]);
    }

    #[test]
    fn find_stalled_goals_mix_completed_and_blocked_steps() {
        let state = GoalState {
            goals: vec![Goal {
                id: "g1".into(),
                description: "Mixed".into(),
                status: GoalStatus::InProgress,
                priority: GoalPriority::High,
                created_at: String::new(),
                updated_at: String::new(),
                steps: vec![
                    Step {
                        id: "s1".into(),
                        description: "Done".into(),
                        status: StepStatus::Completed,
                        result: Some("ok".into()),
                        attempts: 1,
                    },
                    Step {
                        id: "s2".into(),
                        description: "Blocked".into(),
                        status: StepStatus::Blocked,
                        result: None,
                        attempts: 0,
                    },
                ],
                context: String::new(),
                last_error: None,
            }],
        };
        assert_eq!(GoalEngine::find_stalled_goals(&state), vec![0]);
    }

    // ── build_reflection_prompt edge cases ───────────────────────

    #[test]
    fn build_reflection_prompt_empty_context_omits_section() {
        let goal = Goal {
            id: "g1".into(),
            description: "Empty context".into(),
            status: GoalStatus::InProgress,
            priority: GoalPriority::High,
            created_at: String::new(),
            updated_at: String::new(),
            steps: vec![Step {
                id: "s1".into(),
                description: "Step".into(),
                status: StepStatus::Completed,
                result: Some("ok".into()),
                attempts: 1,
            }],
            context: String::new(),
            last_error: None,
        };
        let prompt = GoalEngine::build_reflection_prompt(&goal);
        assert!(!prompt.contains("Accumulated context"));
    }

    #[test]
    fn build_reflection_prompt_no_last_error_omits_section() {
        let goal = Goal {
            id: "g1".into(),
            description: "No error".into(),
            status: GoalStatus::InProgress,
            priority: GoalPriority::High,
            created_at: String::new(),
            updated_at: String::new(),
            steps: vec![Step {
                id: "s1".into(),
                description: "Step".into(),
                status: StepStatus::Completed,
                result: Some("ok".into()),
                attempts: 1,
            }],
            context: "some ctx".into(),
            last_error: None,
        };
        let prompt = GoalEngine::build_reflection_prompt(&goal);
        assert!(!prompt.contains("Last error"));
    }

    #[test]
    fn build_reflection_prompt_all_done_tags() {
        let goal = Goal {
            id: "g1".into(),
            description: "All done".into(),
            status: GoalStatus::InProgress,
            priority: GoalPriority::High,
            created_at: String::new(),
            updated_at: String::new(),
            steps: vec![
                Step {
                    id: "s1".into(),
                    description: "First".into(),
                    status: StepStatus::Completed,
                    result: Some("ok".into()),
                    attempts: 1,
                },
                Step {
                    id: "s2".into(),
                    description: "Second".into(),
                    status: StepStatus::Completed,
                    result: Some("ok".into()),
                    attempts: 1,
                },
            ],
            context: String::new(),
            last_error: None,
        };
        let prompt = GoalEngine::build_reflection_prompt(&goal);
        assert!(prompt.contains("[done] First"));
        assert!(prompt.contains("[done] Second"));
        assert!(!prompt.contains("[exhausted]"));
        assert!(!prompt.contains("[blocked]"));
    }

    // ── GoalPriority comparison and serde ────────────────────────

    #[test]
    fn priority_all_comparisons() {
        assert!(GoalPriority::Critical > GoalPriority::High);
        assert!(GoalPriority::High > GoalPriority::Medium);
        assert!(GoalPriority::Medium > GoalPriority::Low);
        assert!(GoalPriority::Low < GoalPriority::Critical);
    }

    #[test]
    fn priority_serde_roundtrip_all_variants() {
        for priority in &[
            GoalPriority::Low,
            GoalPriority::Medium,
            GoalPriority::High,
            GoalPriority::Critical,
        ] {
            let json = serde_json::to_string(priority).unwrap();
            let parsed: GoalPriority = serde_json::from_str(&json).unwrap();
            assert_eq!(*priority, parsed);
        }
    }
}
