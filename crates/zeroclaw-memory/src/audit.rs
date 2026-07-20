//! Audit trail for memory operations.

use super::traits::{
    ExportFilter, Memory, MemoryCategory, MemoryEntry, MemoryStats, ProceduralMessage, StoreOptions,
};
use async_trait::async_trait;
use chrono::Local;
use parking_lot::Mutex;
use rusqlite::{Connection, params};
use std::path::Path;
use std::sync::Arc;

#[cfg(unix)]
fn ensure_owner_only_dir(path: &Path) -> anyhow::Result<()> {
    use std::os::unix::fs::PermissionsExt;

    std::fs::create_dir_all(path)?;
    std::fs::set_permissions(path, std::fs::Permissions::from_mode(0o700))?;
    Ok(())
}

#[cfg(not(unix))]
fn ensure_owner_only_dir(path: &Path) -> anyhow::Result<()> {
    std::fs::create_dir_all(path)?;
    Ok(())
}

#[cfg(unix)]
fn ensure_owner_only_file(path: &Path) -> anyhow::Result<()> {
    use std::os::unix::fs::{OpenOptionsExt, PermissionsExt};

    if !path.exists() {
        match std::fs::OpenOptions::new()
            .read(true)
            .write(true)
            .create_new(true)
            .mode(0o600)
            .open(path)
        {
            Ok(_) => {}
            Err(e) if e.kind() == std::io::ErrorKind::AlreadyExists => {}
            Err(e) => return Err(e.into()),
        }
    }
    std::fs::set_permissions(path, std::fs::Permissions::from_mode(0o600))?;
    Ok(())
}

#[cfg(not(unix))]
fn ensure_owner_only_file(path: &Path) -> anyhow::Result<()> {
    if !path.exists() {
        match std::fs::OpenOptions::new()
            .read(true)
            .write(true)
            .create_new(true)
            .open(path)
        {
            Ok(_) => {}
            Err(e) if e.kind() == std::io::ErrorKind::AlreadyExists => {}
            Err(e) => return Err(e.into()),
        }
    }
    Ok(())
}

fn sqlite_sidecar_path(db_path: &Path, suffix: &str) -> std::path::PathBuf {
    let mut path = db_path.as_os_str().to_os_string();
    path.push(suffix);
    path.into()
}

#[cfg(unix)]
fn harden_existing_sqlite_sidecars(db_path: &Path) -> anyhow::Result<()> {
    for suffix in ["-wal", "-shm"] {
        let sidecar = sqlite_sidecar_path(db_path, suffix);
        if sidecar.exists() {
            ensure_owner_only_file(&sidecar)?;
        }
    }
    Ok(())
}

#[cfg(not(unix))]
fn harden_existing_sqlite_sidecars(_db_path: &Path) -> anyhow::Result<()> {
    Ok(())
}

/// Audit log entry operations.
#[derive(Debug, Clone, Copy)]
pub enum AuditOp {
    Store,
    Recall,
    Get,
    List,
    Forget,
    Purge,
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
            Self::Purge => write!(f, "purge"),
            Self::StoreProcedural => write!(f, "store_procedural"),
        }
    }
}

/// Decorator that wraps a `Memory` backend with audit logging.
pub struct AuditedMemory<M: Memory> {
    inner: M,
    audit_conn: Arc<Mutex<Connection>>,
}

impl<M: Memory> ::zeroclaw_api::attribution::Attributable for AuditedMemory<M> {
    fn role(&self) -> ::zeroclaw_api::attribution::Role {
        self.inner.role()
    }
    fn alias(&self) -> &str {
        self.inner.alias()
    }
}

impl<M: Memory> AuditedMemory<M> {
    pub fn new(inner: M, workspace_dir: &Path) -> anyhow::Result<Self> {
        let db_path = workspace_dir.join("memory").join("audit.db");
        if let Some(parent) = db_path.parent() {
            ensure_owner_only_dir(parent)?;
        }
        ensure_owner_only_file(&db_path)?;

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
        ensure_owner_only_file(&db_path)?;
        harden_existing_sqlite_sidecars(&db_path)?;

        Ok(Self {
            inner,
            audit_conn: Arc::new(Mutex::new(conn)),
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
        let now = Local::now().to_rfc3339();
        let op_str = op.to_string();
        let started = std::time::Instant::now();
        let recorded = {
            let conn = self.audit_conn.lock();
            conn.execute(
                "INSERT INTO memory_audit (operation, key, namespace, session_id, timestamp, metadata)
                 VALUES (?1, ?2, ?3, ?4, ?5, ?6)",
                params![op_str, key, namespace, session_id, now, metadata],
            )
            .is_ok()
        };
        let elapsed_ms = u64::try_from(started.elapsed().as_millis()).unwrap_or(u64::MAX);
        // Mirror the audit row onto the log stream as a `memory_audit`
        // event. The observer bridge projects this action onto
        // `ObserverEvent::MemoryAudit` for metrics backends; bounded
        // attributes only (no keys, no content). Outcome/duration describe
        // the audit-row insert itself (the trail's own health): the wrapped
        // memory operation has not run yet at this point, and a failing
        // audit.db must surface as success=false instead of being silently
        // swallowed.
        let outcome = if recorded {
            ::zeroclaw_log::EventOutcome::Success
        } else {
            ::zeroclaw_log::EventOutcome::Failure
        };
        ::zeroclaw_log::record!(
            INFO,
            ::zeroclaw_log::Event::new(module_path!(), ::zeroclaw_log::Action::MemoryAudit)
                .with_category(::zeroclaw_log::EventCategory::Memory)
                .with_outcome(outcome)
                .with_duration(elapsed_ms)
                .with_attrs(::serde_json::json!({
                    "memory_action": op_str,
                    "backend": self.inner.name(),
                    "success": recorded
                })),
            "memory.audit"
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

    #[cfg(test)]
    pub(crate) fn inner(&self) -> &M {
        &self.inner
    }
}

#[async_trait]
impl<M: Memory> Memory for AuditedMemory<M> {
    fn name(&self) -> &str {
        self.inner.name()
    }

    fn refresh_embedder(
        &self,
        model_provider: &str,
        api_key: Option<&str>,
        model: &str,
        dimensions: usize,
    ) {
        // Transparent decorator: forward the embedder refresh to the wrapped
        // backend like every other method
        self.inner
            .refresh_embedder(model_provider, api_key, model, dimensions);
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

    async fn get_for_agent(
        &self,
        key: &str,
        agent_id: &str,
    ) -> anyhow::Result<Option<MemoryEntry>> {
        self.log_audit(
            AuditOp::Get,
            Some(key),
            None,
            None,
            Some(&format!("agent_id={agent_id}")),
        );
        self.inner.get_for_agent(key, agent_id).await
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

    async fn forget_for_agent(&self, key: &str, agent_id: &str) -> anyhow::Result<bool> {
        self.log_audit(
            AuditOp::Forget,
            Some(key),
            None,
            None,
            Some(&format!("agent_id={agent_id}")),
        );
        self.inner.forget_for_agent(key, agent_id).await
    }

    async fn purge_namespace(&self, namespace: &str) -> anyhow::Result<usize> {
        self.log_audit(AuditOp::Purge, None, Some(namespace), None, None);
        self.inner.purge_namespace(namespace).await
    }

    async fn purge_session(&self, session_id: &str) -> anyhow::Result<usize> {
        self.log_audit(AuditOp::Purge, None, None, Some(session_id), None);
        self.inner.purge_session(session_id).await
    }

    async fn purge_session_for_agent(
        &self,
        session_id: &str,
        agent_id: &str,
    ) -> anyhow::Result<usize> {
        self.log_audit(
            AuditOp::Purge,
            None,
            None,
            Some(session_id),
            Some(&format!("agent_id={agent_id}")),
        );
        self.inner
            .purge_session_for_agent(session_id, agent_id)
            .await
    }

    async fn purge_agent(&self, agent_alias: &str) -> anyhow::Result<usize> {
        self.log_audit(
            AuditOp::Purge,
            None,
            None,
            None,
            Some(&format!("agent_alias={agent_alias}")),
        );
        self.inner.purge_agent(agent_alias).await
    }

    async fn export_agent(&self, agent_alias: &str) -> anyhow::Result<Vec<MemoryEntry>> {
        self.inner.export_agent(agent_alias).await
    }

    async fn rename_agent(&self, from: &str, to: &str) -> anyhow::Result<usize> {
        self.inner.rename_agent(from, to).await
    }

    async fn count_agent(&self, agent_alias: &str) -> anyhow::Result<usize> {
        self.inner.count_agent(agent_alias).await
    }

    async fn count(&self) -> anyhow::Result<usize> {
        self.inner.count().await
    }

    async fn health_check(&self) -> bool {
        self.inner.health_check().await
    }

    async fn supersede(&self, superseded_ids: &[String], new_id: &str) -> anyhow::Result<()> {
        self.inner.supersede(superseded_ids, new_id).await
    }

    async fn count_in_scope(
        &self,
        namespace: Option<&str>,
        category: Option<&MemoryCategory>,
    ) -> anyhow::Result<u64> {
        self.inner.count_in_scope(namespace, category).await
    }

    async fn stats(&self) -> anyhow::Result<MemoryStats> {
        self.inner.stats().await
    }

    async fn reindex(&self) -> anyhow::Result<usize> {
        self.inner.reindex().await
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
    ) -> anyhow::Result<()> {
        self.log_audit(AuditOp::Store, Some(key), namespace, session_id, None);
        self.inner
            .store_with_metadata(key, content, category, session_id, namespace, importance)
            .await
    }

    async fn store_with_options(
        &self,
        key: &str,
        content: &str,
        category: MemoryCategory,
        session_id: Option<&str>,
        options: StoreOptions,
    ) -> anyhow::Result<()> {
        self.log_audit(
            AuditOp::Store,
            Some(key),
            options.namespace.as_deref(),
            session_id,
            None,
        );
        self.inner
            .store_with_options(key, content, category, session_id, options)
            .await
    }

    async fn store_with_agent(
        &self,
        key: &str,
        content: &str,
        category: MemoryCategory,
        session_id: Option<&str>,
        namespace: Option<&str>,
        importance: Option<f64>,
        agent_id: Option<&str>,
    ) -> anyhow::Result<()> {
        self.log_audit(AuditOp::Store, Some(key), namespace, session_id, None);
        self.inner
            .store_with_agent(
                key, content, category, session_id, namespace, importance, agent_id,
            )
            .await
    }

    async fn recall_for_agents(
        &self,
        allowed_agent_ids: &[&str],
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
            .recall_for_agents(allowed_agent_ids, query, limit, session_id, since, until)
            .await
    }

    async fn export(&self, filter: &ExportFilter) -> anyhow::Result<Vec<MemoryEntry>> {
        self.inner.export(filter).await
    }

    async fn ensure_agent_uuid(&self, alias: &str) -> anyhow::Result<String> {
        self.inner.ensure_agent_uuid(alias).await
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::none::NoneMemory;
    use tempfile::TempDir;

    #[test]
    fn refresh_embedder_forwards_to_inner_backend() {
        let tmp = TempDir::new().unwrap();
        let inner = crate::sqlite::SqliteMemory::new("test", tmp.path()).unwrap();
        let audited = AuditedMemory::new(inner, tmp.path()).unwrap();
        assert_eq!(audited.inner().embedder_dimensions(), 0);

        Memory::refresh_embedder(
            &audited,
            "openai",
            Some("sk-test"),
            "text-embedding-3-small",
            1536,
        );

        assert_eq!(
            audited.inner().embedder_dimensions(),
            1536,
            "AuditedMemory must forward refresh_embedder to the wrapped backend"
        );
    }

    #[tokio::test]
    async fn audited_memory_logs_store_operation() {
        let tmp = TempDir::new().unwrap();
        let inner = NoneMemory::new("none");
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
        let inner = NoneMemory::new("none");
        let audited = AuditedMemory::new(inner, tmp.path()).unwrap();

        let _ = audited.recall("query", 10, None, None, None).await;

        assert_eq!(audited.audit_count().unwrap(), 1);
    }

    #[tokio::test]
    async fn audited_memory_prune_removes_only_rows_past_retention() {
        let tmp = TempDir::new().unwrap();
        let inner = NoneMemory::new("none");
        let audited = AuditedMemory::new(inner, tmp.path()).unwrap();

        audited
            .store("k1", "v1", MemoryCategory::Core, None)
            .await
            .unwrap();

        // Plant a stale audit row well past the retention window.
        {
            let conn = Connection::open(tmp.path().join("memory").join("audit.db")).unwrap();
            let old = (Local::now() - chrono::Duration::days(40)).to_rfc3339();
            conn.execute(
                "INSERT INTO memory_audit (operation, timestamp) VALUES ('store', ?1)",
                params![old],
            )
            .unwrap();
        }

        assert_eq!(audited.audit_count().unwrap(), 2);
        let pruned = audited.prune_older_than(30).unwrap();
        assert_eq!(pruned, 1);
        assert_eq!(audited.audit_count().unwrap(), 1);
    }

    #[cfg(unix)]
    #[test]
    fn audited_memory_hardens_existing_audit_storage_permissions() {
        use std::os::unix::fs::PermissionsExt;

        fn mode(path: &Path) -> u32 {
            std::fs::metadata(path).unwrap().permissions().mode() & 0o777
        }

        let tmp = TempDir::new().unwrap();
        let memory_dir = tmp.path().join("memory");
        std::fs::create_dir_all(&memory_dir).unwrap();
        std::fs::set_permissions(&memory_dir, std::fs::Permissions::from_mode(0o777)).unwrap();

        let db_path = memory_dir.join("audit.db");
        let seeding_conn = Connection::open(&db_path).unwrap();
        seeding_conn
            .execute_batch(
                "PRAGMA journal_mode = WAL;
                 CREATE TABLE IF NOT EXISTS memory_audit (
                     id INTEGER PRIMARY KEY AUTOINCREMENT,
                     operation TEXT NOT NULL,
                     key TEXT,
                     namespace TEXT,
                     session_id TEXT,
                     timestamp TEXT NOT NULL,
                     metadata TEXT
                 );
                 INSERT INTO memory_audit (operation, timestamp) VALUES ('store', '2026-01-01T00:00:00Z');",
            )
            .unwrap();
        std::fs::set_permissions(&db_path, std::fs::Permissions::from_mode(0o666)).unwrap();

        for suffix in ["-wal", "-shm"] {
            let sidecar = sqlite_sidecar_path(&db_path, suffix);
            if sidecar.exists() {
                std::fs::set_permissions(&sidecar, std::fs::Permissions::from_mode(0o666)).unwrap();
            }
        }

        let _audited = AuditedMemory::new(NoneMemory::new("none"), tmp.path()).unwrap();

        assert_eq!(mode(&memory_dir), 0o700);
        assert_eq!(mode(&db_path), 0o600);
        for suffix in ["-wal", "-shm"] {
            let sidecar = sqlite_sidecar_path(&db_path, suffix);
            if sidecar.exists() {
                assert_eq!(mode(&sidecar), 0o600);
            }
        }
    }

    #[tokio::test]
    async fn audited_memory_delegates_correctly() {
        let tmp = TempDir::new().unwrap();
        let inner = NoneMemory::new("none");
        let audited = AuditedMemory::new(inner, tmp.path()).unwrap();

        assert_eq!(audited.name(), "none");
        assert!(audited.health_check().await);
        assert_eq!(audited.count().await.unwrap(), 0);
    }

    #[tokio::test]
    async fn audited_memory_logs_store_with_options_and_forwards() {
        let tmp = TempDir::new().unwrap();
        let inner = crate::sqlite::SqliteMemory::new("test", tmp.path()).unwrap();
        let audited = AuditedMemory::new(inner, tmp.path()).unwrap();

        audited
            .store_with_options(
                "opt_key",
                "opt_value",
                MemoryCategory::Core,
                None,
                StoreOptions::default().with_namespace("ns1"),
            )
            .await
            .unwrap();

        assert_eq!(audited.audit_count().unwrap(), 1);
        let entry = audited.inner().get("opt_key").await.unwrap().unwrap();
        assert_eq!(
            entry.namespace, "ns1",
            "store_with_options must forward the full options to the wrapped backend"
        );
    }

    #[tokio::test]
    async fn audited_memory_purge_session_logs_and_forwards() {
        let tmp = TempDir::new().unwrap();
        let inner = crate::sqlite::SqliteMemory::new("test", tmp.path()).unwrap();
        let audited = AuditedMemory::new(inner, tmp.path()).unwrap();

        audited
            .store("p1", "v1", MemoryCategory::Conversation, Some("s1"))
            .await
            .unwrap();
        let purged = audited.purge_session("s1").await.unwrap();
        assert_eq!(
            purged, 1,
            "purge_session must forward to the wrapped backend"
        );

        let conn = Connection::open(tmp.path().join("memory").join("audit.db")).unwrap();
        let purge_rows: i64 = conn
            .query_row(
                "SELECT COUNT(*) FROM memory_audit WHERE operation = 'purge'",
                [],
                |row| row.get(0),
            )
            .unwrap();
        assert_eq!(purge_rows, 1);
    }

    /// `supersede` and `stats` have inert trait defaults (no-op / empty);
    /// the decorator must forward both so wrapping a backend does not
    /// silently disable conflict supersede or zero out telemetry.
    #[tokio::test]
    async fn audited_memory_forwards_supersede_and_stats() {
        let tmp = TempDir::new().unwrap();
        let inner = crate::sqlite::SqliteMemory::new("test", tmp.path()).unwrap();
        let audited = AuditedMemory::new(inner, tmp.path()).unwrap();

        audited
            .store(
                "a",
                "the office is in Denver",
                MemoryCategory::Core,
                Some("s"),
            )
            .await
            .unwrap();
        audited
            .store(
                "b",
                "the office is in Boulder",
                MemoryCategory::Core,
                Some("s"),
            )
            .await
            .unwrap();

        let entries = audited.inner().list(None, None).await.unwrap();
        let old_id = entries.iter().find(|e| e.key == "a").unwrap().id.clone();
        let new_id = entries.iter().find(|e| e.key == "b").unwrap().id.clone();

        audited.supersede(&[old_id], &new_id).await.unwrap();

        let stats = audited.stats().await.unwrap();
        assert_eq!(stats.total_rows, 2);
        assert_eq!(
            stats.superseded_rows, 1,
            "supersede must reach the wrapped backend, not the trait default no-op"
        );
        assert_eq!(audited.reindex().await.unwrap(), 0);
    }

    #[allow(clippy::await_holding_lock)]
    #[tokio::test]
    async fn audited_ops_emit_memory_audit_events_via_bridge() {
        use std::any::Any;
        use std::sync::{Arc as StdArc, Mutex as StdMutex};
        use zeroclaw_api::observability_traits::{Observer, ObserverEvent, ObserverMetric};

        let _writer_guard = zeroclaw_log::__private_test_writer_lock();
        let _hook_guard = zeroclaw_log::__private_test_hook_lock();
        zeroclaw_log::try_install_capture_subscriber();

        #[derive(Default)]
        struct CapturingObserver {
            events: StdMutex<Vec<ObserverEvent>>,
        }

        impl Observer for CapturingObserver {
            fn record_event(&self, event: &ObserverEvent) {
                self.events.lock().unwrap().push(event.clone());
            }
            fn record_metric(&self, _metric: &ObserverMetric) {}
            fn name(&self) -> &str {
                "capturing"
            }
            fn as_any(&self) -> &dyn Any {
                self
            }
        }

        let tmp = TempDir::new().unwrap();
        let audited = AuditedMemory::new(NoneMemory::new("none"), tmp.path()).unwrap();

        let observer = StdArc::new(CapturingObserver::default());
        ::zeroclaw_log::set_observer_bridge(observer.clone());
        audited
            .store("bridge_key", "v", MemoryCategory::Core, None)
            .await
            .unwrap();
        ::zeroclaw_log::clear_observer_bridge();

        let events = observer.events.lock().unwrap();
        assert!(
            events.iter().any(|e| matches!(
                e,
                ObserverEvent::MemoryAudit { action, backend, success, .. }
                    if action == "store" && backend == "none" && *success
            )),
            "expected a MemoryAudit store event via the observer bridge"
        );
    }
}
