//! Staged dream mutations — the "pending" review surface.
//!
//! When `audit_mode` is enabled (default), proposed memory mutations from a
//! dream cycle are written to `dream_pending.json` in the workspace directory
//! instead of being applied directly. The user can review and promote them
//! via `zeroclaw dream promote`.

use anyhow::{Context, Result};
use chrono::{DateTime, Utc};
use serde::{Deserialize, Serialize};
use std::path::Path;
use zeroclaw_api::memory_traits::{Memory, MemoryCategory};

const PENDING_FILENAME: &str = "dream_pending.json";

/// Per-item result from a promote attempt, used to communicate partial failures.
#[derive(Debug, Clone)]
pub struct PromoteResult {
    /// Number of insights successfully written to memory.
    pub stored: usize,
    /// Number of prune keys successfully removed.
    pub pruned: usize,
    /// Insights that failed to store, kept for retry. Stays in the pending file.
    pub failed_insights: Vec<StagedInsight>,
    /// Prune keys whose `forget()` returned an error, kept for retry.
    pub failed_prunes: Vec<String>,
    /// Whether `dream_pending.json` still exists after promotion. `true` when
    /// there were failures (file rewritten with just the failed items); `false`
    /// when everything succeeded (file removed).
    pub pending_retained: bool,
}

/// A staged set of proposed memory mutations from a dream cycle.
#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct DreamPending {
    /// Proposed Core memory insights to be created.
    pub insights: Vec<StagedInsight>,
    /// Memory keys proposed for deletion (stale/outdated).
    pub proposed_prunes: Vec<String>,
    /// When this pending set was generated.
    pub timestamp: DateTime<Utc>,
    /// Human-readable summary of what the dream cycle found.
    pub summary: Option<String>,
}

/// A single proposed Core memory insight.
#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct StagedInsight {
    /// The insight text to be stored as a Core memory.
    pub content: String,
    /// Computed importance score.
    pub importance: f64,
}

impl DreamPending {
    /// Persist pending mutations to `dream_pending.json`.
    pub fn save(&self, workspace_dir: &Path) -> Result<()> {
        let path = workspace_dir.join(PENDING_FILENAME);
        let json =
            serde_json::to_string_pretty(self).context("dream pending: failed to serialize")?;
        std::fs::write(&path, json)
            .with_context(|| format!("dream pending: failed to write {}", path.display()))?;
        Ok(())
    }

    /// Load pending mutations, if any exist.
    pub fn load(workspace_dir: &Path) -> Result<Option<DreamPending>> {
        let path = workspace_dir.join(PENDING_FILENAME);
        if !path.exists() {
            return Ok(None);
        }

        let data = std::fs::read_to_string(&path)
            .with_context(|| format!("dream pending: failed to read {}", path.display()))?;
        let pending: DreamPending = serde_json::from_str(&data)
            .with_context(|| format!("dream pending: failed to parse {}", path.display()))?;

        Ok(Some(pending))
    }

    /// Remove the pending file after promotion or rejection.
    pub fn clear(workspace_dir: &Path) -> Result<()> {
        let path = workspace_dir.join(PENDING_FILENAME);
        if path.exists() {
            std::fs::remove_file(&path)
                .with_context(|| format!("dream pending: failed to remove {}", path.display()))?;
        }
        Ok(())
    }
}

/// Apply staged dream mutations from `dream_pending.json` to the memory backend.
///
/// Insights are stored as Core memories. Proposed prunes are retired
/// **non-destructively by default** — marked `superseded` (hidden from
/// retrieval, kept in the store) unless `hard_prune` is set, in which case they
/// are permanently deleted.
///
/// On success: removes `dream_pending.json`.
/// On partial failure: rewrites `dream_pending.json` with just the failed items
/// so the user can retry. This preserves unapplied staged work — promotion is
/// idempotent against transient backend errors.
pub async fn promote_pending(
    workspace_dir: &Path,
    memory: &dyn Memory,
    hard_prune: bool,
) -> Result<Option<PromoteResult>> {
    let Some(pending) = DreamPending::load(workspace_dir)? else {
        return Ok(None);
    };

    let mut stored = 0usize;
    let mut failed_insights: Vec<StagedInsight> = Vec::new();
    for insight in &pending.insights {
        let key = format!("dream_insight_{}", uuid::Uuid::new_v4());
        match memory
            .store_with_metadata(
                &key,
                &insight.content,
                MemoryCategory::Core,
                None,
                Some("dream"),
                Some(insight.importance),
            )
            .await
        {
            Ok(()) => stored += 1,
            Err(_) => failed_insights.push(insight.clone()),
        }
    }

    let mut pruned = 0usize;
    let mut failed_prunes: Vec<String> = Vec::new();
    for key in &pending.proposed_prunes {
        // Non-destructive by default: supersede (hide from retrieval, keep in
        // store). `Ok(false)` means there was nothing to retire (already gone /
        // already superseded / append-only backend) — treat as resolved and
        // drop it from pending rather than retry forever.
        let outcome = if hard_prune {
            memory.forget(key).await
        } else {
            memory
                .mark_superseded(key, super::engine::DREAM_SUPERSEDE_MARKER)
                .await
        };
        match outcome {
            Ok(true) => pruned += 1,
            Ok(false) => {}
            Err(_) => failed_prunes.push(key.clone()),
        }
    }

    let has_failures = !failed_insights.is_empty() || !failed_prunes.is_empty();
    if has_failures {
        let retry_pending = DreamPending {
            insights: failed_insights.clone(),
            proposed_prunes: failed_prunes.clone(),
            timestamp: pending.timestamp,
            summary: pending.summary,
        };
        retry_pending.save(workspace_dir)?;
    } else {
        DreamPending::clear(workspace_dir)?;
    }

    Ok(Some(PromoteResult {
        stored,
        pruned,
        failed_insights,
        failed_prunes,
        pending_retained: has_failures,
    }))
}

#[cfg(test)]
mod tests {
    use super::*;
    use async_trait::async_trait;
    use std::sync::Mutex;
    use zeroclaw_api::attribution::{Attributable, MemoryKind, Role};
    use zeroclaw_api::memory_traits::MemoryEntry;

    /// Memory that succeeds for the first N store calls then errors. Used to
    /// simulate transient backend failures mid-promote.
    struct FailingMemory {
        store_calls: Mutex<usize>,
        store_succeeds_first: usize,
    }

    impl FailingMemory {
        fn new(store_succeeds_first: usize) -> Self {
            Self {
                store_calls: Mutex::new(0),
                store_succeeds_first,
            }
        }
    }

    impl Attributable for FailingMemory {
        fn role(&self) -> Role {
            Role::Memory(MemoryKind::InMemory)
        }
        fn alias(&self) -> &str {
            "failing-mock"
        }
    }

    #[async_trait]
    impl Memory for FailingMemory {
        fn name(&self) -> &str {
            "failing-mock"
        }
        async fn store(
            &self,
            _key: &str,
            _content: &str,
            _category: MemoryCategory,
            _session_id: Option<&str>,
        ) -> anyhow::Result<()> {
            let mut n = self.store_calls.lock().unwrap();
            *n += 1;
            if *n > self.store_succeeds_first {
                anyhow::bail!("simulated backend failure on store #{n}");
            }
            Ok(())
        }
        async fn store_with_metadata(
            &self,
            key: &str,
            content: &str,
            category: MemoryCategory,
            session_id: Option<&str>,
            _namespace: Option<&str>,
            _importance: Option<f64>,
        ) -> anyhow::Result<()> {
            self.store(key, content, category, session_id).await
        }
        async fn recall(
            &self,
            _q: &str,
            _l: usize,
            _s: Option<&str>,
            _since: Option<&str>,
            _until: Option<&str>,
        ) -> anyhow::Result<Vec<MemoryEntry>> {
            Ok(Vec::new())
        }
        async fn get(&self, _k: &str) -> anyhow::Result<Option<MemoryEntry>> {
            Ok(None)
        }
        async fn list(
            &self,
            _c: Option<&MemoryCategory>,
            _s: Option<&str>,
        ) -> anyhow::Result<Vec<MemoryEntry>> {
            Ok(Vec::new())
        }
        async fn forget(&self, _k: &str) -> anyhow::Result<bool> {
            Ok(true)
        }
        async fn store_with_agent(
            &self,
            key: &str,
            content: &str,
            category: MemoryCategory,
            session_id: Option<&str>,
            _namespace: Option<&str>,
            _importance: Option<f64>,
            _agent_alias: Option<&str>,
        ) -> anyhow::Result<()> {
            self.store(key, content, category, session_id).await
        }
        async fn forget_for_agent(&self, _key: &str, _agent_alias: &str) -> anyhow::Result<bool> {
            Ok(true)
        }
        async fn recall_for_agents(
            &self,
            _agent_aliases: &[&str],
            _query: &str,
            _limit: usize,
            _session_id: Option<&str>,
            _since: Option<&str>,
            _until: Option<&str>,
        ) -> anyhow::Result<Vec<MemoryEntry>> {
            Ok(Vec::new())
        }
        async fn count(&self) -> anyhow::Result<usize> {
            Ok(0)
        }
        async fn health_check(&self) -> bool {
            true
        }
    }

    #[test]
    fn pending_roundtrip() {
        let temp = tempfile::tempdir().unwrap();
        let pending = DreamPending {
            insights: vec![StagedInsight {
                content: "User prefers Rust.".into(),
                importance: 0.8,
            }],
            proposed_prunes: vec!["old_key_1".into()],
            timestamp: Utc::now(),
            summary: Some("Quiet day.".into()),
        };

        pending.save(temp.path()).unwrap();
        let loaded = DreamPending::load(temp.path()).unwrap();
        assert!(loaded.is_some());
        let loaded = loaded.unwrap();
        assert_eq!(loaded.insights.len(), 1);
        assert_eq!(loaded.proposed_prunes.len(), 1);
    }

    #[test]
    fn no_pending_returns_none() {
        let temp = tempfile::tempdir().unwrap();
        let loaded = DreamPending::load(temp.path()).unwrap();
        assert!(loaded.is_none());
    }

    #[test]
    fn clear_removes_file() {
        let temp = tempfile::tempdir().unwrap();
        let pending = DreamPending {
            insights: vec![],
            proposed_prunes: vec![],
            timestamp: Utc::now(),
            summary: None,
        };
        pending.save(temp.path()).unwrap();
        assert!(temp.path().join("dream_pending.json").exists());

        DreamPending::clear(temp.path()).unwrap();
        assert!(!temp.path().join("dream_pending.json").exists());
    }

    /// When promote() encounters a partial backend failure, the pending file
    /// must be rewritten with the unapplied items rather than cleared. This
    /// preserves staged work for the user to retry.
    #[tokio::test]
    async fn partial_promote_failure_preserves_failed_items() {
        let temp = tempfile::tempdir().unwrap();
        let workspace = temp.path().to_path_buf();

        // Stage three insights.
        let pending = DreamPending {
            insights: vec![
                StagedInsight {
                    content: "Insight A".into(),
                    importance: 0.5,
                },
                StagedInsight {
                    content: "Insight B".into(),
                    importance: 0.5,
                },
                StagedInsight {
                    content: "Insight C".into(),
                    importance: 0.5,
                },
            ],
            proposed_prunes: vec![],
            timestamp: Utc::now(),
            summary: None,
        };
        pending.save(&workspace).unwrap();

        // Memory that succeeds for the first store, fails on every subsequent.
        let memory = FailingMemory::new(1);

        let result = promote_pending(&workspace, &memory, false)
            .await
            .unwrap()
            .unwrap();

        assert_eq!(result.stored, 1, "only the first store should succeed");
        assert_eq!(
            result.failed_insights.len(),
            2,
            "two stores must be reported as failed"
        );
        assert!(
            result.pending_retained,
            "pending file should be retained on partial failure"
        );

        // Critical: pending file must still exist with the failed items so the
        // user can retry.
        let after = DreamPending::load(&workspace).unwrap();
        assert!(
            after.is_some(),
            "dream_pending.json must survive a partial-failure promote"
        );
        let after = after.unwrap();
        assert_eq!(after.insights.len(), 2);
        assert!(after.insights.iter().any(|i| i.content == "Insight B"));
        assert!(after.insights.iter().any(|i| i.content == "Insight C"));
    }

    #[tokio::test]
    async fn full_promote_success_clears_pending() {
        let temp = tempfile::tempdir().unwrap();
        let workspace = temp.path().to_path_buf();

        let pending = DreamPending {
            insights: vec![StagedInsight {
                content: "ok".into(),
                importance: 0.5,
            }],
            proposed_prunes: vec![],
            timestamp: Utc::now(),
            summary: None,
        };
        pending.save(&workspace).unwrap();

        let memory = FailingMemory::new(10); // never fails

        let result = promote_pending(&workspace, &memory, false)
            .await
            .unwrap()
            .unwrap();
        assert_eq!(result.stored, 1);
        assert!(!result.pending_retained);
        assert!(DreamPending::load(&workspace).unwrap().is_none());
    }
}
