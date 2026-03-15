//! Thread-safe sub-agent session registry.
//!
//! Provides [`SubAgentRegistry`] for tracking background sub-agent sessions
//! with status lifecycle management, concurrent access, and automatic cleanup.

use crate::tools::traits::ToolResult;
use chrono::{DateTime, Utc};
use parking_lot::RwLock;
use std::collections::HashMap;
use std::sync::Arc;
use tokio::task::JoinHandle;

/// Maximum age (in seconds) for completed/failed/killed sessions before cleanup.
const SESSION_MAX_AGE_SECS: i64 = 3600;

/// Status of a sub-agent session.
#[derive(Debug, Clone, PartialEq, Eq)]
pub enum SubAgentStatus {
    Running,
    Completed,
    Failed,
    Killed,
}

impl SubAgentStatus {
    /// pub fn as_str.
    pub fn as_str(&self) -> &'static str {
        match self {
            SubAgentStatus::Running => "running",
            SubAgentStatus::Completed => "completed",
            SubAgentStatus::Failed => "failed",
            SubAgentStatus::Killed => "killed",
        }
    }
}

impl std::fmt::Display for SubAgentStatus {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        f.write_str(self.as_str())
    }
}

/// A single sub-agent session tracked by the registry.
/// pub struct SubAgentSession.
pub struct SubAgentSession {
    pub id: String,
    pub agent_name: String,
    pub task: String,
    pub status: SubAgentStatus,
    pub started_at: DateTime<Utc>,
    pub completed_at: Option<DateTime<Utc>>,
    pub result: Option<ToolResult>,
    /// Handle to the spawned tokio task, used for cancellation via `abort()`.
    pub handle: Option<JoinHandle<()>>,
}

/// Thread-safe registry for tracking background sub-agent sessions.
#[derive(Clone)]
/// pub struct SubAgentRegistry.
pub struct SubAgentRegistry {
    sessions: Arc<RwLock<HashMap<String, SubAgentSession>>>,
}

impl SubAgentRegistry {
    /// pub fn new.
    pub fn new() -> Self {
        Self {
            sessions: Arc::new(RwLock::new(HashMap::new())),
        }
    }

    /// Insert a new session into the registry.
    pub fn insert(&self, session: SubAgentSession) {
        let mut sessions = self.sessions.write();
        sessions.insert(session.id.clone(), session);
    }

    /// Atomically check the concurrent session limit and insert if under the cap.
    /// Returns `Ok(())` if inserted, `Err((running_count, session))` if at capacity.
    pub fn try_insert(
        &self,
        session: SubAgentSession,
        max_concurrent: usize,
    ) -> Result<(), (usize, Box<SubAgentSession>)> {
        let mut sessions = self.sessions.write();
        let running = sessions
            .values()
            .filter(|s| matches!(s.status, SubAgentStatus::Running))
            .count();
        if running >= max_concurrent {
            return Err((running, Box::new(session)));
        }
        sessions.insert(session.id.clone(), session);
        Ok(())
    }

    /// Set the tokio task handle for a session (used to enable cancellation).
    /// pub fn set_handle.
    pub fn set_handle(&self, session_id: &str, handle: JoinHandle<()>) {
        let mut sessions = self.sessions.write();
        if let Some(session) = sessions.get_mut(session_id) {
            session.handle = Some(handle);
        }
    }

    /// Mark a session as completed with a result.
    /// pub fn complete.
    pub fn complete(&self, session_id: &str, result: ToolResult) {
        let mut sessions = self.sessions.write();
        if let Some(session) = sessions.get_mut(session_id) {
            session.status = SubAgentStatus::Completed;
            session.completed_at = Some(Utc::now());
            session.result = Some(result);
            session.handle = None;
        }
    }

    /// Mark a session as failed with an error result.
    /// pub fn fail.
    pub fn fail(&self, session_id: &str, error: String) {
        let mut sessions = self.sessions.write();
        if let Some(session) = sessions.get_mut(session_id) {
            session.status = SubAgentStatus::Failed;
            session.completed_at = Some(Utc::now());
            session.result = Some(ToolResult {
                success: false,
                output: String::new(),
                error: Some(error),
            });
            session.handle = None;
        }
    }

    /// Kill a running session by aborting its tokio task.
    /// Returns `true` if the session was found and killed, `false` otherwise.
    /// pub fn kill.
    pub fn kill(&self, session_id: &str) -> bool {
        let mut sessions = self.sessions.write();
        if let Some(session) = sessions.get_mut(session_id) {
            if session.status != SubAgentStatus::Running {
                return false;
            }
            if let Some(handle) = session.handle.take() {
                handle.abort();
            }
            session.status = SubAgentStatus::Killed;
            session.completed_at = Some(Utc::now());
            session.result = Some(ToolResult {
                success: false,
                output: String::new(),
                error: Some("Session killed by user".to_string()),
            });
            true
        } else {
            false
        }
    }

    /// Get the status and optional result for a session.
    /// pub fn get_status.
    pub fn get_status(&self, session_id: &str) -> Option<SubAgentStatusSnapshot> {
        let sessions = self.sessions.read();
        sessions.get(session_id).map(|s| SubAgentStatusSnapshot {
            status: s.status.clone(),
            agent_name: s.agent_name.clone(),
            task: s.task.clone(),
            started_at: s.started_at,
            completed_at: s.completed_at,
            result: s.result.clone(),
        })
    }

    /// List sessions, optionally filtered by status.
    /// Also performs lazy cleanup of old completed sessions.
    /// pub fn list.
    pub fn list(&self, status_filter: Option<&str>) -> Vec<SubAgentSessionInfo> {
        self.cleanup_old_sessions();

        let sessions = self.sessions.read();
        sessions
            .values()
            .filter(|s| match status_filter {
                Some("running") => s.status == SubAgentStatus::Running,
                Some("completed") => s.status == SubAgentStatus::Completed,
                Some("failed") => s.status == SubAgentStatus::Failed,
                Some("killed") => s.status == SubAgentStatus::Killed,
                _ => true,
            })
            .map(|s| {
                let duration_ms = s.completed_at.map(|end| {
                    u64::try_from((end - s.started_at).num_milliseconds()).unwrap_or_default()
                });
                SubAgentSessionInfo {
                    session_id: s.id.clone(),
                    agent: s.agent_name.clone(),
                    task: truncate_task(&s.task, 100),
                    status: s.status.as_str().to_string(),
                    started_at: s.started_at.to_rfc3339(),
                    completed_at: s.completed_at.map(|t| t.to_rfc3339()),
                    duration_ms,
                }
            })
            .collect()
    }

    /// Remove completed/failed/killed sessions older than the max age.
    fn cleanup_old_sessions(&self) {
        let now = Utc::now();
        let mut sessions = self.sessions.write();
        sessions.retain(|_, s| {
            if s.status == SubAgentStatus::Running {
                return true;
            }
            match s.completed_at {
                Some(completed) => (now - completed).num_seconds() < SESSION_MAX_AGE_SECS,
                None => true,
            }
        });
    }

    /// Check if a session exists.
    /// pub fn exists.
    pub fn exists(&self, session_id: &str) -> bool {
        self.sessions.read().contains_key(session_id)
    }

    /// Get the number of currently running sessions.
    /// pub fn running_count.
    pub fn running_count(&self) -> usize {
        self.sessions
            .read()
            .values()
            .filter(|s| s.status == SubAgentStatus::Running)
            .count()
    }
}

impl Default for SubAgentRegistry {
    fn default() -> Self {
        Self::new()
    }
}

/// Snapshot of a session's status returned by `get_status`.
#[derive(Debug, Clone)]
/// pub struct SubAgentStatusSnapshot.
pub struct SubAgentStatusSnapshot {
    pub status: SubAgentStatus,
    pub agent_name: String,
    pub task: String,
    pub started_at: DateTime<Utc>,
    pub completed_at: Option<DateTime<Utc>>,
    pub result: Option<ToolResult>,
}

/// Serializable session info for list output.
#[derive(Debug, Clone, serde::Serialize)]
/// pub struct SubAgentSessionInfo.
pub struct SubAgentSessionInfo {
    pub session_id: String,
    pub agent: String,
    pub task: String,
    pub status: String,
    pub started_at: String,
    pub completed_at: Option<String>,
    pub duration_ms: Option<u64>,
}

fn truncate_task(task: &str, max_len: usize) -> String {
    if task.chars().count() <= max_len {
        task.to_string()
    } else {
        let byte_idx = task
            .char_indices()
            .nth(max_len)
            .map(|(i, _)| i)
            .unwrap_or(task.len());
        format!("{}...", &task[..byte_idx])
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    fn make_session(id: &str, agent: &str, task: &str) -> SubAgentSession {
        SubAgentSession {
            id: id.to_string(),
            agent_name: agent.to_string(),
            task: task.to_string(),
            status: SubAgentStatus::Running,
            started_at: Utc::now(),
            completed_at: None,
            result: None,
            handle: None,
        }
    }

    #[test]
    fn registry_insert_and_list() {
        let registry = SubAgentRegistry::new();
        registry.insert(make_session("s1", "researcher", "find info"));
        registry.insert(make_session("s2", "coder", "write code"));

        let all = registry.list(Some("all"));
        assert_eq!(all.len(), 2);
    }

    #[test]
    fn registry_complete_session() {
        let registry = SubAgentRegistry::new();
        registry.insert(make_session("s1", "researcher", "find info"));

        registry.complete(
            "s1",
            ToolResult {
                success: true,
                output: "done".to_string(),
                error: None,
            },
        );

        let snap = registry.get_status("s1").unwrap();
        assert_eq!(snap.status, SubAgentStatus::Completed);
        assert!(snap.completed_at.is_some());
        assert!(snap.result.unwrap().success);
    }

    #[test]
    fn registry_fail_session() {
        let registry = SubAgentRegistry::new();
        registry.insert(make_session("s1", "researcher", "find info"));

        registry.fail("s1", "provider error".to_string());

        let snap = registry.get_status("s1").unwrap();
        assert_eq!(snap.status, SubAgentStatus::Failed);
        assert!(!snap.result.unwrap().success);
    }

    #[test]
    fn registry_kill_running_session() {
        let registry = SubAgentRegistry::new();
        registry.insert(make_session("s1", "researcher", "find info"));

        assert!(registry.kill("s1"));

        let snap = registry.get_status("s1").unwrap();
        assert_eq!(snap.status, SubAgentStatus::Killed);
        assert!(snap
            .result
            .unwrap()
            .error
            .as_deref()
            .unwrap()
            .contains("killed"));
    }

    #[test]
    fn registry_kill_non_running_returns_false() {
        let registry = SubAgentRegistry::new();
        registry.insert(make_session("s1", "researcher", "find info"));
        registry.complete(
            "s1",
            ToolResult {
                success: true,
                output: "done".to_string(),
                error: None,
            },
        );

        assert!(!registry.kill("s1"));
    }

    #[test]
    fn registry_kill_unknown_returns_false() {
        let registry = SubAgentRegistry::new();
        assert!(!registry.kill("nonexistent"));
    }

    #[test]
    fn registry_list_filters_by_status() {
        let registry = SubAgentRegistry::new();
        registry.insert(make_session("s1", "researcher", "task1"));
        registry.insert(make_session("s2", "coder", "task2"));

        registry.complete(
            "s1",
            ToolResult {
                success: true,
                output: "done".to_string(),
                error: None,
            },
        );

        let running = registry.list(Some("running"));
        assert_eq!(running.len(), 1);
        assert_eq!(running[0].session_id, "s2");

        let completed = registry.list(Some("completed"));
        assert_eq!(completed.len(), 1);
        assert_eq!(completed[0].session_id, "s1");
    }

    #[test]
    fn registry_get_status_unknown() {
        let registry = SubAgentRegistry::new();
        assert!(registry.get_status("nonexistent").is_none());
    }

    #[test]
    fn registry_exists() {
        let registry = SubAgentRegistry::new();
        registry.insert(make_session("s1", "researcher", "task"));
        assert!(registry.exists("s1"));
        assert!(!registry.exists("nonexistent"));
    }

    #[test]
    fn registry_running_count() {
        let registry = SubAgentRegistry::new();
        assert_eq!(registry.running_count(), 0);

        registry.insert(make_session("s1", "a", "t1"));
        registry.insert(make_session("s2", "b", "t2"));
        assert_eq!(registry.running_count(), 2);

        registry.complete(
            "s1",
            ToolResult {
                success: true,
                output: "done".to_string(),
                error: None,
            },
        );
        assert_eq!(registry.running_count(), 1);
    }

    #[test]
    fn registry_cleanup_old_sessions() {
        let registry = SubAgentRegistry::new();

        // Insert a session and mark it completed with an old timestamp
        let mut session = make_session("old", "agent", "task");
        session.status = SubAgentStatus::Completed;
        session.completed_at =
            Some(Utc::now() - chrono::Duration::seconds(SESSION_MAX_AGE_SECS + 1));
        session.result = Some(ToolResult {
            success: true,
            output: "old result".to_string(),
            error: None,
        });
        registry.insert(session);

        // Insert a recent completed session
        registry.insert(make_session("recent", "agent", "task"));
        registry.complete(
            "recent",
            ToolResult {
                success: true,
                output: "recent result".to_string(),
                error: None,
            },
        );

        // List triggers cleanup
        let all = registry.list(Some("all"));
        // Old session should be cleaned up, recent should remain
        assert_eq!(all.len(), 1);
        assert_eq!(all[0].session_id, "recent");
    }

    #[test]
    fn truncate_task_short() {
        assert_eq!(truncate_task("short", 100), "short");
    }

    #[test]
    fn truncate_task_long() {
        let long = "a".repeat(150);
        let truncated = truncate_task(&long, 100);
        assert!(truncated.ends_with("..."));
        assert_eq!(truncated.chars().count(), 103); // 100 chars + "..."
    }

    #[test]
    fn truncate_task_multibyte_safe() {
        // Each emoji is 4 bytes. 10 emojis = 40 bytes but 10 chars.
        let emojis = "🦀".repeat(10);
        let truncated = truncate_task(&emojis, 5);
        assert!(truncated.ends_with("..."));
        assert_eq!(truncated.chars().count(), 8); // 5 emojis + "..."
    }

    #[test]
    fn status_display() {
        assert_eq!(SubAgentStatus::Running.as_str(), "running");
        assert_eq!(SubAgentStatus::Completed.as_str(), "completed");
        assert_eq!(SubAgentStatus::Failed.as_str(), "failed");
        assert_eq!(SubAgentStatus::Killed.as_str(), "killed");
        assert_eq!(format!("{}", SubAgentStatus::Running), "running");
    }

    #[test]
    fn registry_default() {
        let registry = SubAgentRegistry::default();
        assert_eq!(registry.list(None).len(), 0);
    }

    #[test]
    fn concurrent_insert_and_list() {
        use std::sync::Arc;
        use std::thread;

        let registry = Arc::new(SubAgentRegistry::new());
        let mut handles = Vec::new();

        for i in 0..10 {
            let reg = registry.clone();
            handles.push(thread::spawn(move || {
                reg.insert(make_session(
                    &format!("s{i}"),
                    "agent",
                    &format!("task {i}"),
                ));
            }));
        }

        for h in handles {
            h.join().unwrap();
        }

        assert_eq!(registry.list(Some("all")).len(), 10);
    }

    #[test]
    fn session_info_serialization() {
        let info = SubAgentSessionInfo {
            session_id: "test-id".to_string(),
            agent: "researcher".to_string(),
            task: "find info".to_string(),
            status: "running".to_string(),
            started_at: "2024-01-01T00:00:00Z".to_string(),
            completed_at: None,
            duration_ms: None,
        };
        let json = serde_json::to_string(&info).unwrap();
        assert!(json.contains("test-id"));
        assert!(json.contains("researcher"));
    }
}
