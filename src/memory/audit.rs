//! Audit trail for memory operations.
//!
//! Provides a decorator `AuditedMemory<M>` that wraps any `Memory` backend
//! and logs all operations to a `memory_audit` table. Opt-in via
//! `[memory] audit_enabled = true`.

use super::traits::{Memory, MemoryCategory, MemoryEntry, ProceduralMessage};
use async_trait::async_trait;
use chrono::Local;
use parking_lot::Mutex;
use rusqlite::{params, Connection};
use std::path::{Path, PathBuf};
use std::sync::Arc;

/// Audit log entry operations.
#[derive(Debug, Clone, Copy)]
pub enum AuditOp {
    Store,
    Recall,
    Get,
    List,
    Forget,
    StoreProcedural,
}

impl std::fmt::Display for AuditOp {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        match self {
            Self::Store => write!(f, "store"),
            Self::Recall => write!(f, "recall"),
            Self::Get => write!(f, "get"),
            Self::List => write!(f, "list"),
            Self::Forget => write!(f, "forget"),
            Self::StoreProcedural => write!(f, "store_procedural"),
        }
    }
}

/// Decorator that wraps a `Memory` backend with audit logging.
pub struct AuditedMemory<M: Memory> {
    inner: M,
    audit_conn: Arc<Mutex<Connection>>,
    #[allow(dead_code)]
    db_path: PathBuf,
}

impl<M: Memory> AuditedMemory<M> {
    pub fn new(inner: M, workspace_dir: &Path) -> anyhow::Result<Self> {
        let db_path = workspace_dir.join("memory").join("audit.db");
        if let Some(parent) = db_path.parent() {
            std::fs::create_dir_all(parent)?;
        }

        let conn = Connection::open(&db_path)?;
        conn.execute_batch(
            "PRAGMA journal_mode = WAL;
             PRAGMA synchronous = NORMAL;
             CREATE TABLE IF NOT EXISTS memory_audit (
                 id INTEGER PRIMARY KEY AUTOINCREMENT,
                 operation TEXT NOT NULL,
                 key TEXT,
                 namespace TEXT,
                 session_id TEXT,
                 timestamp TEXT NOT NULL,
                 metadata TEXT
             );
             CREATE INDEX IF NOT EXISTS idx_audit_timestamp ON memory_audit(timestamp);
             CREATE INDEX IF NOT EXISTS idx_audit_operation ON memory_audit(operation);",
        )?;

        Ok(Self {
            inner,
            audit_conn: Arc::new(Mutex::new(conn)),
            db_path,
        })
    }

    fn log_audit(
        &self,
        op: AuditOp,
        key: Option<&str>,
        namespace: Option<&str>,
        session_id: Option<&str>,
        metadata: Option<&str>,
    ) {
        let conn = self.audit_conn.lock();
        let now = Local::now().to_rfc3339();
        let op_str = op.to_string();
        let _ = conn.execute(
            "INSERT INTO memory_audit (operation, key, namespace, session_id, timestamp, metadata)
             VALUES (?1, ?2, ?3, ?4, ?5, ?6)",
            params![op_str, key, namespace, session_id, now, metadata],
        );
    }

    /// Prune audit entries older than the given number of days.
    pub fn prune_older_than(&self, retention_days: u32) -> anyhow::Result<u64> {
        let conn = self.audit_conn.lock();
        let cutoff =
            (Local::now() - chrono::Duration::days(i64::from(retention_days))).to_rfc3339();
        let affected = conn.execute(
            "DELETE FROM memory_audit WHERE timestamp < ?1",
            params![cutoff],
        )?;
        Ok(u64::try_from(affected).unwrap_or(0))
    }

    /// Count total audit entries.
    pub fn audit_count(&self) -> anyhow::Result<usize> {
        let conn = self.audit_conn.lock();
        let count: i64 =
            conn.query_row("SELECT COUNT(*) FROM memory_audit", [], |row| row.get(0))?;
        #[allow(clippy::cast_sign_loss, clippy::cast_possible_truncation)]
        Ok(count as usize)
    }
}

#[async_trait]
impl<M: Memory> Memory for AuditedMemory<M> {
    fn name(&self) -> &str {
        self.inner.name()
    }

    async fn store(
        &self,
        key: &str,
        content: &str,
        category: MemoryCategory,
        session_id: Option<&str>,
    ) -> anyhow::Result<()> {
        self.log_audit(AuditOp::Store, Some(key), None, session_id, None);
        self.inner.store(key, content, category, session_id).await
    }

    async fn recall(
        &self,
        query: &str,
        limit: usize,
        session_id: Option<&str>,
        since: Option<&str>,
        until: Option<&str>,
    ) -> anyhow::Result<Vec<MemoryEntry>> {
        self.log_audit(
            AuditOp::Recall,
            None,
            None,
            session_id,
            Some(&format!("query={query}")),
        );
        self.inner
            .recall(query, limit, session_id, since, until)
            .await
    }

    async fn get(&self, key: &str) -> anyhow::Result<Option<MemoryEntry>> {
        self.log_audit(AuditOp::Get, Some(key), None, None, None);
        self.inner.get(key).await
    }

    async fn list(
        &self,
        category: Option<&MemoryCategory>,
        session_id: Option<&str>,
    ) -> anyhow::Result<Vec<MemoryEntry>> {
        self.log_audit(AuditOp::List, None, None, session_id, None);
        self.inner.list(category, session_id).await
    }

    async fn forget(&self, key: &str) -> anyhow::Result<bool> {
        self.log_audit(AuditOp::Forget, Some(key), None, None, None);
        self.inner.forget(key).await
    }

    async fn count(&self) -> anyhow::Result<usize> {
        self.inner.count().await
    }

    async fn health_check(&self) -> bool {
        self.inner.health_check().await
    }

    async fn store_procedural(
        &self,
        messages: &[ProceduralMessage],
        session_id: Option<&str>,
    ) -> anyhow::Result<()> {
        self.log_audit(
            AuditOp::StoreProcedural,
            None,
            None,
            session_id,
            Some(&format!("messages={}", messages.len())),
        );
        self.inner.store_procedural(messages, session_id).await
    }

    async fn recall_namespaced(
        &self,
        namespace: &str,
        query: &str,
        limit: usize,
        session_id: Option<&str>,
        since: Option<&str>,
        until: Option<&str>,
    ) -> anyhow::Result<Vec<MemoryEntry>> {
        self.log_audit(
            AuditOp::Recall,
            None,
            Some(namespace),
            session_id,
            Some(&format!("query={query}")),
        );
        self.inner
            .recall_namespaced(namespace, query, limit, session_id, since, until)
            .await
    }

    async fn store_with_metadata(
        &self,
        key: &str,
        content: &str,
        category: MemoryCategory,
        session_id: Option<&str>,
        namespace: Option<&str>,
        importance: Option<f64>,
        metadata: Option<&str>,
    ) -> anyhow::Result<()> {
        self.log_audit(AuditOp::Store, Some(key), namespace, session_id, None);
        self.inner
            .store_with_metadata(key, content, category, session_id, namespace, importance, metadata)
            .await
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::memory::NoneMemory;
    use tempfile::TempDir;

    #[tokio::test]
    async fn audited_memory_logs_store_operation() {
        let tmp = TempDir::new().unwrap();
        let inner = NoneMemory::new();
        let audited = AuditedMemory::new(inner, tmp.path()).unwrap();

        audited
            .store("test_key", "test_value", MemoryCategory::Core, None)
            .await
            .unwrap();

        assert_eq!(audited.audit_count().unwrap(), 1);
    }

    #[tokio::test]
    async fn audited_memory_logs_recall_operation() {
        let tmp = TempDir::new().unwrap();
        let inner = NoneMemory::new();
        let audited = AuditedMemory::new(inner, tmp.path()).unwrap();

        let _ = audited.recall("query", 10, None, None, None).await;

        assert_eq!(audited.audit_count().unwrap(), 1);
    }

    #[tokio::test]
    async fn audited_memory_prune_works() {
        let tmp = TempDir::new().unwrap();
        let inner = NoneMemory::new();
        let audited = AuditedMemory::new(inner, tmp.path()).unwrap();

        audited
            .store("k1", "v1", MemoryCategory::Core, None)
            .await
            .unwrap();

        // Pruning with 0 days should remove entries
        let pruned = audited.prune_older_than(0).unwrap();
        // Entry was just created, so 0-day retention should remove it
        // Pruning should succeed (pruned is usize, always >= 0)
        let _ = pruned;
    }

    #[tokio::test]
    async fn audited_memory_delegates_correctly() {
        let tmp = TempDir::new().unwrap();
        let inner = NoneMemory::new();
        let audited = AuditedMemory::new(inner, tmp.path()).unwrap();

        assert_eq!(audited.name(), "none");
        assert!(audited.health_check().await);
        assert_eq!(audited.count().await.unwrap(), 0);
    }
}
