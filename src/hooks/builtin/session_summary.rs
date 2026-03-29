use async_trait::async_trait;
use std::path::PathBuf;
use std::sync::Mutex;
use std::sync::atomic::{AtomicU64, Ordering};
use std::time::Instant;

use crate::hooks::traits::HookHandler;

/// Writes a JSON summary file when a session ends.
///
/// Tracked per-session data: session_id, channel, start_time (UTC ISO-8601),
/// message_count, and duration_secs. Summaries are written to
/// `<workspace>/sessions/session_<id>.json`.
pub struct SessionSummaryHook {
    workspace_dir: PathBuf,
    /// UTC timestamp captured when the hook is created (used as fallback start).
    created_at: chrono::DateTime<chrono::Utc>,
    /// Per-session start instants (for accurate duration).
    session_starts: Mutex<std::collections::HashMap<String, Instant>>,
    /// Per-session message counter.
    message_counts: Mutex<std::collections::HashMap<String, u64>>,
    /// Global message counter for sessions not yet tracked individually.
    global_message_count: AtomicU64,
}

impl SessionSummaryHook {
    pub fn new(workspace_dir: PathBuf) -> Self {
        Self {
            workspace_dir,
            created_at: chrono::Utc::now(),
            session_starts: Mutex::new(std::collections::HashMap::new()),
            message_counts: Mutex::new(std::collections::HashMap::new()),
            global_message_count: AtomicU64::new(0),
        }
    }

    fn sessions_dir(&self) -> PathBuf {
        self.workspace_dir.join("sessions")
    }
}

#[async_trait]
impl HookHandler for SessionSummaryHook {
    fn name(&self) -> &str {
        "session-summary"
    }

    fn priority(&self) -> i32 {
        -90
    }

    async fn on_session_start(&self, session_id: &str, _channel: &str) {
        self.session_starts
            .lock()
            .unwrap_or_else(|e| e.into_inner())
            .insert(session_id.to_string(), Instant::now());
        self.message_counts
            .lock()
            .unwrap_or_else(|e| e.into_inner())
            .insert(session_id.to_string(), 0);
    }

    async fn on_message_sent(&self, _channel: &str, _recipient: &str, _content: &str) {
        self.global_message_count.fetch_add(1, Ordering::Relaxed);
    }

    async fn on_session_end(&self, session_id: &str, channel: &str) {
        let start_instant = self
            .session_starts
            .lock()
            .unwrap_or_else(|e| e.into_inner())
            .remove(session_id);

        let message_count = self
            .message_counts
            .lock()
            .unwrap_or_else(|e| e.into_inner())
            .remove(session_id)
            .unwrap_or_else(|| self.global_message_count.load(Ordering::Relaxed));

        let duration_secs = start_instant.map(|s| s.elapsed().as_secs()).unwrap_or(0);

        let start_time = if start_instant.is_some() {
            // Approximate the wall-clock start from the elapsed duration.
            (chrono::Utc::now() - chrono::Duration::seconds(duration_secs as i64)).to_rfc3339()
        } else {
            self.created_at.to_rfc3339()
        };

        let summary = serde_json::json!({
            "session_id": session_id,
            "channel": channel,
            "start_time": start_time,
            "message_count": message_count,
            "duration_secs": duration_secs,
        });

        let dir = self.sessions_dir();
        if let Err(e) = tokio::fs::create_dir_all(&dir).await {
            tracing::warn!(
                hook = "session-summary",
                error = %e,
                "failed to create sessions directory"
            );
            return;
        }

        // Sanitise session_id for use in filename (replace non-alphanumeric except dash/underscore).
        let safe_id: String = session_id
            .chars()
            .map(|c| {
                if c.is_alphanumeric() || c == '-' || c == '_' {
                    c
                } else {
                    '_'
                }
            })
            .collect();
        let path = dir.join(format!("session_{safe_id}.json"));

        let data = serde_json::to_vec_pretty(&summary).unwrap_or_else(|_| b"{}".to_vec());
        if let Err(e) = tokio::fs::write(&path, data).await {
            tracing::warn!(
                hook = "session-summary",
                error = %e,
                path = %path.display(),
                "failed to write session summary"
            );
        } else {
            tracing::info!(
                hook = "session-summary",
                session_id,
                path = %path.display(),
                "session summary written"
            );
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use tempfile::TempDir;

    #[tokio::test]
    async fn writes_summary_on_session_end() {
        let tmp = TempDir::new().unwrap();
        let hook = SessionSummaryHook::new(tmp.path().to_path_buf());

        hook.on_session_start("sess-001", "telegram").await;

        // Simulate a couple of messages.
        hook.on_message_sent("telegram", "user1", "hello").await;
        hook.on_message_sent("telegram", "user1", "world").await;

        // Bump per-session counter manually (normally driven by the runner).
        {
            let mut counts = hook.message_counts.lock().unwrap();
            *counts.get_mut("sess-001").unwrap() = 2;
        }

        hook.on_session_end("sess-001", "telegram").await;

        let path = tmp.path().join("sessions/session_sess-001.json");
        assert!(path.exists(), "summary file should exist");

        let data: serde_json::Value =
            serde_json::from_slice(&std::fs::read(&path).unwrap()).unwrap();
        assert_eq!(data["session_id"], "sess-001");
        assert_eq!(data["channel"], "telegram");
        assert_eq!(data["message_count"], 2);
        assert!(data["duration_secs"].as_u64().is_some());
        assert!(data["start_time"].as_str().is_some());
    }

    #[tokio::test]
    async fn sanitises_session_id_in_filename() {
        let tmp = TempDir::new().unwrap();
        let hook = SessionSummaryHook::new(tmp.path().to_path_buf());

        hook.on_session_end("a/b", "cli").await;

        let path = tmp.path().join("sessions/session_a_b.json");
        assert!(path.exists(), "sanitised filename should be used");
    }

    #[tokio::test]
    async fn works_without_session_start() {
        let tmp = TempDir::new().unwrap();
        let hook = SessionSummaryHook::new(tmp.path().to_path_buf());

        // No on_session_start call — should still write a summary with fallback values.
        hook.on_session_end("orphan-sess", "slack").await;

        let path = tmp.path().join("sessions/session_orphan-sess.json");
        assert!(path.exists());

        let data: serde_json::Value =
            serde_json::from_slice(&std::fs::read(&path).unwrap()).unwrap();
        assert_eq!(data["session_id"], "orphan-sess");
        assert_eq!(data["duration_secs"], 0);
    }
}
