//! Stale conversation turn probe — tracks responses that imply follow-up work
//! and nudges the user when the conversation stalls.

use anyhow::Result;
use regex::Regex;
use serde::{Deserialize, Serialize};
use std::path::{Path, PathBuf};
use std::sync::{LazyLock, OnceLock};
use std::time::{Duration, SystemTime};
use tokio::sync::Mutex;

/// A record of a conversation turn that implied follow-up work.
#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct PendingFollowup {
    pub channel: String,
    pub reply_target: String,
    pub history_key: String,
    pub created_at: SystemTime,
    pub ttl_secs: u64,
    pub context_summary: String,
    pub nudge_count: u32,
    pub max_nudges: u32,
}

impl PendingFollowup {
    pub fn is_expired(&self) -> bool {
        self.created_at
            .elapsed()
            .map(|elapsed| elapsed >= Duration::from_secs(self.ttl_secs))
            .unwrap_or(true)
    }
}

/// File-backed store for pending follow-ups. Thread-safe via Mutex.
pub struct PendingFollowupStore {
    path: PathBuf,
    inner: Mutex<Vec<PendingFollowup>>,
}

impl PendingFollowupStore {
    pub async fn new(workspace_dir: &Path) -> Result<Self> {
        let path = workspace_dir.join("pending_followups.json");
        let entries = if path.exists() {
            let data = tokio::fs::read_to_string(&path).await?;
            serde_json::from_str(&data).unwrap_or_default()
        } else {
            Vec::new()
        };
        Ok(Self {
            path,
            inner: Mutex::new(entries),
        })
    }

    pub async fn add(&self, followup: PendingFollowup) {
        let mut entries = self.inner.lock().await;
        // Replace existing entry for the same history_key
        entries.retain(|e| e.history_key != followup.history_key);
        entries.push(followup);
        let _ = self.persist(&entries).await;
    }

    /// Remove the entry for a given history key (conversation continued).
    pub async fn remove(&self, history_key: &str) {
        let mut entries = self.inner.lock().await;
        let before = entries.len();
        entries.retain(|e| e.history_key != history_key);
        if entries.len() != before {
            let _ = self.persist(&entries).await;
        }
    }

    /// Collect expired follow-ups that haven't exhausted their nudges.
    pub async fn collect_expired(&self) -> Vec<PendingFollowup> {
        let entries = self.inner.lock().await;
        entries
            .iter()
            .filter(|e| e.is_expired() && e.nudge_count < e.max_nudges)
            .cloned()
            .collect()
    }

    /// Increment the nudge count for a given history key.
    pub async fn increment_nudge(&self, history_key: &str) {
        let mut entries = self.inner.lock().await;
        if let Some(entry) = entries.iter_mut().find(|e| e.history_key == history_key) {
            entry.nudge_count += 1;
            // If exhausted, remove it
            if entry.nudge_count >= entry.max_nudges {
                entries.retain(|e| e.history_key != history_key);
            }
        }
        let _ = self.persist(&entries).await;
    }

    /// Remove all entries that have exhausted their nudges or are very old (>1h).
    pub async fn gc(&self) {
        let mut entries = self.inner.lock().await;
        let before = entries.len();
        entries.retain(|e| {
            e.nudge_count < e.max_nudges
                && e.created_at
                    .elapsed()
                    .map(|d| d < Duration::from_secs(3600))
                    .unwrap_or(false)
        });
        if entries.len() != before {
            let _ = self.persist(&entries).await;
        }
    }

    async fn persist(&self, entries: &[PendingFollowup]) -> Result<()> {
        let data = serde_json::to_string_pretty(entries)?;
        tokio::fs::write(&self.path, data).await?;
        Ok(())
    }
}

// ── Global accessor ─────────────────────────────────────────────────────

static GLOBAL_STORE: OnceLock<PendingFollowupStore> = OnceLock::new();

/// Initialize the global pending followup store. Safe to call multiple times;
/// only the first call takes effect.
pub async fn init_global_store(workspace_dir: &Path) -> Result<()> {
    if GLOBAL_STORE.get().is_some() {
        return Ok(());
    }
    let store = PendingFollowupStore::new(workspace_dir).await?;
    let _ = GLOBAL_STORE.set(store);
    Ok(())
}

/// Get a reference to the global store, if initialized.
pub fn global_store() -> Option<&'static PendingFollowupStore> {
    GLOBAL_STORE.get()
}

// ── Follow-up detection ─────────────────────────────────────────────────

static FOLLOWUP_REGEX: LazyLock<Regex> = LazyLock::new(|| {
    Regex::new(
        r"(?i)\b(I'll\s+(get\s+back|check\s+on|look\s+into|investigate|follow\s+up|work\s+on|continue|dig\s+into)|let\s+me\s+(check|look\s+into|investigate|get\s+back|dig\s+into)|working\s+on\s+it|I\s+will\s+(get\s+back|check|investigate|follow\s+up|continue)|stay\s+tuned|give\s+me\s+a\s+moment|I'll\s+report\s+back|I'll\s+update\s+you)\b"
    )
    .expect("followup regex must compile")
});

/// Returns true if the response text implies the agent will do follow-up work
/// that hasn't happened yet.
pub fn response_implies_followup(text: &str) -> bool {
    FOLLOWUP_REGEX.is_match(text)
}

/// Truncate text with ellipsis for context summaries.
pub fn truncate_for_summary(text: &str, max_chars: usize) -> String {
    if text.chars().count() <= max_chars {
        text.to_string()
    } else {
        let truncated: String = text.chars().take(max_chars.saturating_sub(3)).collect();
        format!("{truncated}...")
    }
}

// ── Tests ────────────────────────────────────────────────────────────────

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn detects_ill_get_back() {
        assert!(response_implies_followup(
            "I'll get back to you with the results."
        ));
    }

    #[test]
    fn detects_let_me_check() {
        assert!(response_implies_followup(
            "Let me check on that and update you."
        ));
    }

    #[test]
    fn detects_working_on_it() {
        assert!(response_implies_followup("Working on it now."));
    }

    #[test]
    fn detects_ill_investigate() {
        assert!(response_implies_followup(
            "I'll investigate this issue further."
        ));
    }

    #[test]
    fn detects_stay_tuned() {
        assert!(response_implies_followup("Stay tuned for the update."));
    }

    #[test]
    fn does_not_match_normal_response() {
        assert!(!response_implies_followup(
            "Here are the results you asked for."
        ));
    }

    #[test]
    fn does_not_match_completed_action() {
        assert!(!response_implies_followup(
            "I've completed the task. The file has been updated."
        ));
    }

    #[test]
    fn does_not_match_empty() {
        assert!(!response_implies_followup(""));
    }

    #[test]
    fn truncate_short_text() {
        assert_eq!(truncate_for_summary("hello", 10), "hello");
    }

    #[test]
    fn truncate_long_text() {
        let result = truncate_for_summary("hello world this is a long string", 15);
        assert!(result.ends_with("..."));
        assert!(result.chars().count() <= 15);
    }

    #[tokio::test]
    async fn store_add_and_collect() {
        let dir = std::env::temp_dir().join("zeroclaw_test_followup_store");
        let _ = tokio::fs::remove_dir_all(&dir).await;
        tokio::fs::create_dir_all(&dir).await.unwrap();

        let store = PendingFollowupStore::new(&dir).await.unwrap();
        store
            .add(PendingFollowup {
                channel: "signal".into(),
                reply_target: "+1234".into(),
                history_key: "signal_+1234".into(),
                created_at: SystemTime::now() - Duration::from_secs(600),
                ttl_secs: 300,
                context_summary: "test".into(),
                nudge_count: 0,
                max_nudges: 2,
            })
            .await;

        let expired = store.collect_expired().await;
        assert_eq!(expired.len(), 1);
        assert_eq!(expired[0].history_key, "signal_+1234");

        let _ = tokio::fs::remove_dir_all(&dir).await;
    }

    #[tokio::test]
    async fn store_remove_clears_entry() {
        let dir = std::env::temp_dir().join("zeroclaw_test_followup_remove");
        let _ = tokio::fs::remove_dir_all(&dir).await;
        tokio::fs::create_dir_all(&dir).await.unwrap();

        let store = PendingFollowupStore::new(&dir).await.unwrap();
        store
            .add(PendingFollowup {
                channel: "signal".into(),
                reply_target: "+1234".into(),
                history_key: "signal_+1234".into(),
                created_at: SystemTime::now() - Duration::from_secs(600),
                ttl_secs: 300,
                context_summary: "test".into(),
                nudge_count: 0,
                max_nudges: 2,
            })
            .await;

        store.remove("signal_+1234").await;
        assert!(store.collect_expired().await.is_empty());

        let _ = tokio::fs::remove_dir_all(&dir).await;
    }

    #[tokio::test]
    async fn store_nudge_increments_and_removes_at_max() {
        let dir = std::env::temp_dir().join("zeroclaw_test_followup_nudge");
        let _ = tokio::fs::remove_dir_all(&dir).await;
        tokio::fs::create_dir_all(&dir).await.unwrap();

        let store = PendingFollowupStore::new(&dir).await.unwrap();
        store
            .add(PendingFollowup {
                channel: "signal".into(),
                reply_target: "+1234".into(),
                history_key: "signal_+1234".into(),
                created_at: SystemTime::now() - Duration::from_secs(600),
                ttl_secs: 300,
                context_summary: "test".into(),
                nudge_count: 1,
                max_nudges: 2,
            })
            .await;

        // This nudge takes it to 2/2 => removed
        store.increment_nudge("signal_+1234").await;
        assert!(store.collect_expired().await.is_empty());

        let _ = tokio::fs::remove_dir_all(&dir).await;
    }

    #[test]
    fn followup_is_expired() {
        let f = PendingFollowup {
            channel: "signal".into(),
            reply_target: "+1234".into(),
            history_key: "signal_+1234".into(),
            created_at: SystemTime::now() - Duration::from_secs(600),
            ttl_secs: 300,
            context_summary: "test".into(),
            nudge_count: 0,
            max_nudges: 2,
        };
        assert!(f.is_expired());
    }

    #[test]
    fn followup_not_expired() {
        let f = PendingFollowup {
            channel: "signal".into(),
            reply_target: "+1234".into(),
            history_key: "signal_+1234".into(),
            created_at: SystemTime::now(),
            ttl_secs: 300,
            context_summary: "test".into(),
            nudge_count: 0,
            max_nudges: 2,
        };
        assert!(!f.is_expired());
    }
}
