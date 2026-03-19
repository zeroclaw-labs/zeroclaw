use crate::config::HeartbeatConfig;
use crate::observability::{Observer, ObserverEvent};
use anyhow::Result;
use serde::{Deserialize, Serialize};
use std::fmt;
use std::path::Path;
use std::sync::Arc;
use tokio::time::{self, Duration};
use tracing::{info, warn};

// ── Structured task types ────────────────────────────────────────

/// Priority level for a heartbeat task.
#[derive(Debug, Clone, Copy, PartialEq, Eq, PartialOrd, Ord, Serialize, Deserialize)]
#[serde(rename_all = "lowercase")]
pub enum TaskPriority {
    Low,
    Medium,
    High,
}

impl fmt::Display for TaskPriority {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        match self {
            Self::Low => write!(f, "low"),
            Self::Medium => write!(f, "medium"),
            Self::High => write!(f, "high"),
        }
    }
}

/// Status of a heartbeat task.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "lowercase")]
pub enum TaskStatus {
    Active,
    Paused,
    Completed,
}

impl fmt::Display for TaskStatus {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        match self {
            Self::Active => write!(f, "active"),
            Self::Paused => write!(f, "paused"),
            Self::Completed => write!(f, "completed"),
        }
    }
}

/// A structured heartbeat task with priority and status metadata.
#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct HeartbeatTask {
    pub text: String,
    pub priority: TaskPriority,
    pub status: TaskStatus,
}

impl HeartbeatTask {
    pub fn is_runnable(&self) -> bool {
        self.status == TaskStatus::Active
    }
}

impl fmt::Display for HeartbeatTask {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        write!(f, "[{}] {}", self.priority, self.text)
    }
}

// ── Engine ───────────────────────────────────────────────────────

/// Heartbeat engine — reads HEARTBEAT.md and executes tasks periodically
pub struct HeartbeatEngine {
    config: HeartbeatConfig,
    workspace_dir: std::path::PathBuf,
    observer: Arc<dyn Observer>,
}

impl HeartbeatEngine {
    pub fn new(
        config: HeartbeatConfig,
        workspace_dir: std::path::PathBuf,
        observer: Arc<dyn Observer>,
    ) -> Self {
        Self {
            config,
            workspace_dir,
            observer,
        }
    }

    /// Start the heartbeat loop (runs until cancelled)
    pub async fn run(&self) -> Result<()> {
        if !self.config.enabled {
            info!("Heartbeat disabled");
            return Ok(());
        }

        let interval_mins = self.config.interval_minutes.max(5);
        info!("💓 Heartbeat started: every {} minutes", interval_mins);

        let mut interval = time::interval(Duration::from_secs(u64::from(interval_mins) * 60));

        loop {
            interval.tick().await;
            self.observer.record_event(&ObserverEvent::HeartbeatTick);

            match self.tick().await {
                Ok(tasks) => {
                    if tasks > 0 {
                        info!("💓 Heartbeat: processed {} tasks", tasks);
                    }
                }
                Err(e) => {
                    warn!("💓 Heartbeat error: {}", e);
                    self.observer.record_event(&ObserverEvent::Error {
                        component: "heartbeat".into(),
                        message: e.to_string(),
                    });
                }
            }
        }
    }

    /// Single heartbeat tick — read HEARTBEAT.md and return task count
    async fn tick(&self) -> Result<usize> {
        Ok(self.collect_tasks().await?.len())
    }

    /// Read HEARTBEAT.md and return all parsed structured tasks.
    pub async fn collect_tasks(&self) -> Result<Vec<HeartbeatTask>> {
        let heartbeat_path = self.workspace_dir.join("HEARTBEAT.md");
        if !heartbeat_path.exists() {
            return Ok(Vec::new());
        }
        let content = tokio::fs::read_to_string(&heartbeat_path).await?;
        Ok(Self::parse_tasks(&content))
    }

    /// Collect only runnable (active) tasks, sorted by priority (high first).
    pub async fn collect_runnable_tasks(&self) -> Result<Vec<HeartbeatTask>> {
        let mut tasks: Vec<HeartbeatTask> = self
            .collect_tasks()
            .await?
            .into_iter()
            .filter(HeartbeatTask::is_runnable)
            .collect();
        // Sort by priority descending (High > Medium > Low)
        tasks.sort_by(|a, b| b.priority.cmp(&a.priority));
        Ok(tasks)
    }

    /// Parse tasks from HEARTBEAT.md with structured metadata support.
    ///
    /// Supports both legacy flat format and new structured format:
    ///
    /// Legacy:
    ///   `- Check email`  →  medium priority, active status
    ///
    /// Structured:
    ///   `- [high] Check email`           →  high priority, active
    ///   `- [low|paused] Review old PRs`  →  low priority, paused
    ///   `- [completed] Old task`         →  medium priority, completed
    fn parse_tasks(content: &str) -> Vec<HeartbeatTask> {
        content
            .lines()
            .filter_map(|line| {
                let trimmed = line.trim();
                let text = trimmed.strip_prefix("- ")?;
                if text.is_empty() {
                    return None;
                }
                Some(Self::parse_task_line(text))
            })
            .collect()
    }

    /// Parse a single task line into a structured `HeartbeatTask`.
    ///
    /// Format: `[priority|status] task text` or just `task text`.
    fn parse_task_line(text: &str) -> HeartbeatTask {
        if let Some(rest) = text.strip_prefix('[') {
            if let Some((meta, task_text)) = rest.split_once(']') {
                let task_text = task_text.trim();
                if !task_text.is_empty() {
                    let (priority, status) = Self::parse_meta(meta);
                    return HeartbeatTask {
                        text: task_text.to_string(),
                        priority,
                        status,
                    };
                }
            }
        }
        // No metadata — default to medium/active
        HeartbeatTask {
            text: text.to_string(),
            priority: TaskPriority::Medium,
            status: TaskStatus::Active,
        }
    }

    /// Parse metadata tags like `high`, `low|paused`, `completed`.
    fn parse_meta(meta: &str) -> (TaskPriority, TaskStatus) {
        let mut priority = TaskPriority::Medium;
        let mut status = TaskStatus::Active;

        for part in meta.split('|') {
            match part.trim().to_ascii_lowercase().as_str() {
                "high" => priority = TaskPriority::High,
                "medium" | "med" => priority = TaskPriority::Medium,
                "low" => priority = TaskPriority::Low,
                "active" => status = TaskStatus::Active,
                "paused" | "pause" => status = TaskStatus::Paused,
                "completed" | "complete" | "done" => status = TaskStatus::Completed,
                _ => {}
            }
        }

        (priority, status)
    }

    /// Build the Phase 1 LLM decision prompt for two-phase heartbeat.
    pub fn build_decision_prompt(tasks: &[HeartbeatTask]) -> String {
        let mut prompt = String::from(
            "You are a heartbeat scheduler. Review the following periodic tasks and decide \
             whether any should be executed right now.\n\n\
             Consider:\n\
             - Task priority (high tasks are more urgent)\n\
             - Whether the task is time-sensitive or can wait\n\
             - Whether running the task now would provide value\n\n\
             Tasks:\n",
        );

        for (i, task) in tasks.iter().enumerate() {
            use std::fmt::Write;
            let _ = writeln!(prompt, "{}. [{}] {}", i + 1, task.priority, task.text);
        }

        prompt.push_str(
            "\nRespond with ONLY one of:\n\
             - `run: 1,2,3` (comma-separated task numbers to execute)\n\
             - `skip` (nothing needs to run right now)\n\n\
             Be conservative — skip if tasks are routine and not time-sensitive.",
        );

        prompt
    }

    /// Parse the Phase 1 LLM decision response.
    ///
    /// Returns indices of tasks to run, or empty vec if skipped.
    pub fn parse_decision_response(response: &str, task_count: usize) -> Vec<usize> {
        let trimmed = response.trim().to_ascii_lowercase();

        if trimmed == "skip" || trimmed.starts_with("skip") {
            return Vec::new();
        }

        // Look for "run: 1,2,3" pattern
        let numbers_part = if let Some(after_run) = trimmed.strip_prefix("run:") {
            after_run.trim()
        } else if let Some(after_run) = trimmed.strip_prefix("run ") {
            after_run.trim()
        } else {
            // Try to parse as bare numbers
            trimmed.as_str()
        };

        numbers_part
            .split(',')
            .filter_map(|s| {
                let n: usize = s.trim().parse().ok()?;
                if n >= 1 && n <= task_count {
                    Some(n - 1) // Convert to 0-indexed
                } else {
                    None
                }
            })
            .collect()
    }

    /// Create a default HEARTBEAT.md if it doesn't exist
    pub async fn ensure_heartbeat_file(workspace_dir: &Path) -> Result<()> {
        let path = workspace_dir.join("HEARTBEAT.md");
        if !path.exists() {
            let default = "# Periodic Tasks\n\n\
                           # Add tasks below (one per line, starting with `- `)\n\
                           # The agent will check this file on each heartbeat tick.\n\
                           #\n\
                           # Format: - [priority|status] Task description\n\
                           #   priority: high, medium (default), low\n\
                           #   status:   active (default), paused, completed\n\
                           #\n\
                           # Examples:\n\
                           # - [high] Check my email for important messages\n\
                           # - Review my calendar for upcoming events\n\
                           # - [low|paused] Check the weather forecast\n";
            tokio::fs::write(&path, default).await?;
        }
        Ok(())
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn parse_tasks_basic() {
        let content = "# Tasks\n\n- Check email\n- Review calendar\nNot a task\n- Third task";
        let tasks = HeartbeatEngine::parse_tasks(content);
        assert_eq!(tasks.len(), 3);
        assert_eq!(tasks[0].text, "Check email");
        assert_eq!(tasks[0].priority, TaskPriority::Medium);
        assert_eq!(tasks[0].status, TaskStatus::Active);
    }

    #[test]
    fn parse_tasks_empty_content() {
        assert!(HeartbeatEngine::parse_tasks("").is_empty());
    }

    #[test]
    fn parse_tasks_only_comments() {
        let tasks = HeartbeatEngine::parse_tasks("# No tasks here\n\nJust comments\n# Another");
        assert!(tasks.is_empty());
    }

    #[test]
    fn parse_tasks_with_leading_whitespace() {
        let content = "  - Indented task\n\t- Tab indented";
        let tasks = HeartbeatEngine::parse_tasks(content);
        assert_eq!(tasks.len(), 2);
        assert_eq!(tasks[0].text, "Indented task");
        assert_eq!(tasks[1].text, "Tab indented");
    }

    #[test]
    fn parse_tasks_dash_without_space_ignored() {
        let content = "- Real task\n-\n- Another";
        let tasks = HeartbeatEngine::parse_tasks(content);
        assert_eq!(tasks.len(), 2);
        assert_eq!(tasks[0].text, "Real task");
        assert_eq!(tasks[1].text, "Another");
    }

    #[test]
    fn parse_tasks_trailing_space_bullet_trimmed_to_dash() {
        let content = "- ";
        let tasks = HeartbeatEngine::parse_tasks(content);
        assert_eq!(tasks.len(), 0);
    }

    #[test]
    fn parse_tasks_bullet_with_content_after_spaces() {
        let content = "- hello  ";
        let tasks = HeartbeatEngine::parse_tasks(content);
        assert_eq!(tasks.len(), 1);
        assert_eq!(tasks[0].text, "hello");
    }

    #[test]
    fn parse_tasks_unicode() {
        let content = "- Check email 📧\n- Review calendar 📅\n- 日本語タスク";
        let tasks = HeartbeatEngine::parse_tasks(content);
        assert_eq!(tasks.len(), 3);
        assert!(tasks[0].text.contains('📧'));
        assert!(tasks[2].text.contains("日本語"));
    }

    #[test]
    fn parse_tasks_mixed_markdown() {
        let content = "# Periodic Tasks\n\n## Quick\n- Task A\n\n## Long\n- Task B\n\n* Not a dash bullet\n1. Not numbered";
        let tasks = HeartbeatEngine::parse_tasks(content);
        assert_eq!(tasks.len(), 2);
        assert_eq!(tasks[0].text, "Task A");
        assert_eq!(tasks[1].text, "Task B");
    }

    #[test]
    fn parse_tasks_single_task() {
        let tasks = HeartbeatEngine::parse_tasks("- Only one");
        assert_eq!(tasks.len(), 1);
        assert_eq!(tasks[0].text, "Only one");
    }

    #[test]
    fn parse_tasks_many_tasks() {
        let content: String = (0..100).fold(String::new(), |mut s, i| {
            use std::fmt::Write;
            let _ = writeln!(s, "- Task {i}");
            s
        });
        let tasks = HeartbeatEngine::parse_tasks(&content);
        assert_eq!(tasks.len(), 100);
        assert_eq!(tasks[99].text, "Task 99");
    }

    // ── Structured task parsing tests ────────────────────────────

    #[test]
    fn parse_task_with_high_priority() {
        let content = "- [high] Urgent email check";
        let tasks = HeartbeatEngine::parse_tasks(content);
        assert_eq!(tasks.len(), 1);
        assert_eq!(tasks[0].text, "Urgent email check");
        assert_eq!(tasks[0].priority, TaskPriority::High);
        assert_eq!(tasks[0].status, TaskStatus::Active);
    }

    #[test]
    fn parse_task_with_low_paused() {
        let content = "- [low|paused] Review old PRs";
        let tasks = HeartbeatEngine::parse_tasks(content);
        assert_eq!(tasks.len(), 1);
        assert_eq!(tasks[0].text, "Review old PRs");
        assert_eq!(tasks[0].priority, TaskPriority::Low);
        assert_eq!(tasks[0].status, TaskStatus::Paused);
    }

    #[test]
    fn parse_task_completed() {
        let content = "- [completed] Old task";
        let tasks = HeartbeatEngine::parse_tasks(content);
        assert_eq!(tasks.len(), 1);
        assert_eq!(tasks[0].priority, TaskPriority::Medium);
        assert_eq!(tasks[0].status, TaskStatus::Completed);
    }

    #[test]
    fn parse_task_without_metadata_defaults() {
        let content = "- Plain task";
        let tasks = HeartbeatEngine::parse_tasks(content);
        assert_eq!(tasks.len(), 1);
        assert_eq!(tasks[0].text, "Plain task");
        assert_eq!(tasks[0].priority, TaskPriority::Medium);
        assert_eq!(tasks[0].status, TaskStatus::Active);
    }

    #[test]
    fn parse_mixed_structured_and_legacy() {
        let content = "- [high] Urgent\n- Normal task\n- [low|paused] Later";
        let tasks = HeartbeatEngine::parse_tasks(content);
        assert_eq!(tasks.len(), 3);
        assert_eq!(tasks[0].priority, TaskPriority::High);
        assert_eq!(tasks[1].priority, TaskPriority::Medium);
        assert_eq!(tasks[2].priority, TaskPriority::Low);
        assert_eq!(tasks[2].status, TaskStatus::Paused);
    }

    #[test]
    fn runnable_filters_paused_and_completed() {
        let content = "- [high] Active\n- [low|paused] Paused\n- [completed] Done";
        let tasks = HeartbeatEngine::parse_tasks(content);
        let runnable: Vec<_> = tasks
            .into_iter()
            .filter(HeartbeatTask::is_runnable)
            .collect();
        assert_eq!(runnable.len(), 1);
        assert_eq!(runnable[0].text, "Active");
    }

    // ── Two-phase decision tests ────────────────────────────────

    #[test]
    fn decision_prompt_includes_all_tasks() {
        let tasks = vec![
            HeartbeatTask {
                text: "Check email".into(),
                priority: TaskPriority::High,
                status: TaskStatus::Active,
            },
            HeartbeatTask {
                text: "Review calendar".into(),
                priority: TaskPriority::Medium,
                status: TaskStatus::Active,
            },
        ];
        let prompt = HeartbeatEngine::build_decision_prompt(&tasks);
        assert!(prompt.contains("1. [high] Check email"));
        assert!(prompt.contains("2. [medium] Review calendar"));
        assert!(prompt.contains("skip"));
        assert!(prompt.contains("run:"));
    }

    #[test]
    fn parse_decision_skip() {
        let indices = HeartbeatEngine::parse_decision_response("skip", 3);
        assert!(indices.is_empty());
    }

    #[test]
    fn parse_decision_skip_with_reason() {
        let indices =
            HeartbeatEngine::parse_decision_response("skip — nothing urgent right now", 3);
        assert!(indices.is_empty());
    }

    #[test]
    fn parse_decision_run_single() {
        let indices = HeartbeatEngine::parse_decision_response("run: 1", 3);
        assert_eq!(indices, vec![0]);
    }

    #[test]
    fn parse_decision_run_multiple() {
        let indices = HeartbeatEngine::parse_decision_response("run: 1, 3", 3);
        assert_eq!(indices, vec![0, 2]);
    }

    #[test]
    fn parse_decision_run_out_of_range_ignored() {
        let indices = HeartbeatEngine::parse_decision_response("run: 1, 5, 2", 3);
        assert_eq!(indices, vec![0, 1]);
    }

    #[test]
    fn parse_decision_run_zero_ignored() {
        let indices = HeartbeatEngine::parse_decision_response("run: 0, 1", 3);
        assert_eq!(indices, vec![0]);
    }

    // ── Task display ────────────────────────────────────────────

    #[test]
    fn task_display_format() {
        let task = HeartbeatTask {
            text: "Check email".into(),
            priority: TaskPriority::High,
            status: TaskStatus::Active,
        };
        assert_eq!(format!("{task}"), "[high] Check email");
    }

    #[test]
    fn priority_ordering() {
        assert!(TaskPriority::High > TaskPriority::Medium);
        assert!(TaskPriority::Medium > TaskPriority::Low);
    }

    // ── Async tests ─────────────────────────────────────────────

    #[tokio::test]
    async fn ensure_heartbeat_file_creates_file() {
        let dir = std::env::temp_dir().join("jhedaiclaw_test_heartbeat");
        let _ = tokio::fs::remove_dir_all(&dir).await;
        tokio::fs::create_dir_all(&dir).await.unwrap();

        HeartbeatEngine::ensure_heartbeat_file(&dir).await.unwrap();

        let path = dir.join("HEARTBEAT.md");
        assert!(path.exists());
        let content = tokio::fs::read_to_string(&path).await.unwrap();
        assert!(content.contains("Periodic Tasks"));
        assert!(content.contains("[high]"));

        let _ = tokio::fs::remove_dir_all(&dir).await;
    }

    #[tokio::test]
    async fn ensure_heartbeat_file_does_not_overwrite() {
        let dir = std::env::temp_dir().join("jhedaiclaw_test_heartbeat_no_overwrite");
        let _ = tokio::fs::remove_dir_all(&dir).await;
        tokio::fs::create_dir_all(&dir).await.unwrap();

        let path = dir.join("HEARTBEAT.md");
        tokio::fs::write(&path, "- My custom task").await.unwrap();

        HeartbeatEngine::ensure_heartbeat_file(&dir).await.unwrap();

        let content = tokio::fs::read_to_string(&path).await.unwrap();
        assert_eq!(content, "- My custom task");

        let _ = tokio::fs::remove_dir_all(&dir).await;
    }

    #[tokio::test]
    async fn tick_returns_zero_when_no_file() {
        let dir = std::env::temp_dir().join("jhedaiclaw_test_tick_no_file");
        let _ = tokio::fs::remove_dir_all(&dir).await;
        tokio::fs::create_dir_all(&dir).await.unwrap();

        let observer: Arc<dyn Observer> = Arc::new(crate::observability::NoopObserver);
        let engine = HeartbeatEngine::new(
            HeartbeatConfig {
                enabled: true,
                interval_minutes: 30,
                ..HeartbeatConfig::default()
            },
            dir.clone(),
            observer,
        );
        let count = engine.tick().await.unwrap();
        assert_eq!(count, 0);

        let _ = tokio::fs::remove_dir_all(&dir).await;
    }

    #[tokio::test]
    async fn tick_counts_tasks_from_file() {
        let dir = std::env::temp_dir().join("jhedaiclaw_test_tick_count");
        let _ = tokio::fs::remove_dir_all(&dir).await;
        tokio::fs::create_dir_all(&dir).await.unwrap();

        tokio::fs::write(dir.join("HEARTBEAT.md"), "- A\n- B\n- C")
            .await
            .unwrap();

        let observer: Arc<dyn Observer> = Arc::new(crate::observability::NoopObserver);
        let engine = HeartbeatEngine::new(
            HeartbeatConfig {
                enabled: true,
                interval_minutes: 30,
                ..HeartbeatConfig::default()
            },
            dir.clone(),
            observer,
        );
        let count = engine.tick().await.unwrap();
        assert_eq!(count, 3);

        let _ = tokio::fs::remove_dir_all(&dir).await;
    }

    #[tokio::test]
    async fn run_returns_immediately_when_disabled() {
        let observer: Arc<dyn Observer> = Arc::new(crate::observability::NoopObserver);
        let engine = HeartbeatEngine::new(
            HeartbeatConfig {
                enabled: false,
                interval_minutes: 30,
                ..HeartbeatConfig::default()
            },
            std::env::temp_dir(),
            observer,
        );
        // Should return Ok immediately, not loop forever
        let result = engine.run().await;
        assert!(result.is_ok());
    }

    #[tokio::test]
    async fn collect_runnable_tasks_sorts_by_priority() {
        let dir = std::env::temp_dir().join("jhedaiclaw_test_runnable_sort");
        let _ = tokio::fs::remove_dir_all(&dir).await;
        tokio::fs::create_dir_all(&dir).await.unwrap();

        tokio::fs::write(
            dir.join("HEARTBEAT.md"),
            "- [low] Low task\n- [high] High task\n- Medium task\n- [low|paused] Skip me",
        )
        .await
        .unwrap();

        let observer: Arc<dyn Observer> = Arc::new(crate::observability::NoopObserver);
        let engine = HeartbeatEngine::new(
            HeartbeatConfig {
                enabled: true,
                interval_minutes: 30,
                ..HeartbeatConfig::default()
            },
            dir.clone(),
            observer,
        );

        let tasks = engine.collect_runnable_tasks().await.unwrap();
        assert_eq!(tasks.len(), 3); // paused one excluded
        assert_eq!(tasks[0].priority, TaskPriority::High);
        assert_eq!(tasks[1].priority, TaskPriority::Medium);
        assert_eq!(tasks[2].priority, TaskPriority::Low);

        let _ = tokio::fs::remove_dir_all(&dir).await;
    }
}
