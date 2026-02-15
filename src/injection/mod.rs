//! Runtime injection system — provides 12 typed platform capabilities to agents.
//!
//! Each injection type corresponds to a specific platform service (logging,
//! memory, tasks, database, etc.) and is backed by a trait. Default
//! implementations integrate with Aria's existing systems.

use crate::aria::db::AriaDb;
use crate::aria::types::InjectionType;
use anyhow::Result;
use std::collections::HashMap;
use std::sync::Arc;

// ── Injection Traits ────────────────────────────────────────────

/// Structured logging injection.
#[async_trait::async_trait]
pub trait LoggingInjection: Send + Sync {
    fn log(&self, level: &str, message: &str);
    fn log_structured(
        &self,
        level: &str,
        message: &str,
        fields: &HashMap<String, serde_json::Value>,
    );
}

/// Key-value memory store injection (wraps Aria memory registry).
#[async_trait::async_trait]
pub trait MemoryInjection: Send + Sync {
    async fn store(&self, key: &str, value: &str, namespace: Option<&str>) -> Result<()>;
    async fn recall(&self, key: &str, namespace: Option<&str>) -> Result<Option<String>>;
    async fn forget(&self, key: &str, namespace: Option<&str>) -> Result<()>;
}

/// Task management injection (wraps Aria task registry).
#[async_trait::async_trait]
pub trait TaskInjection: Send + Sync {
    async fn create_task(&self, name: &str, params: &serde_json::Value) -> Result<String>;
    async fn get_task_status(&self, task_id: &str) -> Result<Option<String>>;
    async fn cancel_task(&self, task_id: &str) -> Result<bool>;
    async fn list_tasks(&self, status: Option<&str>) -> Result<Vec<serde_json::Value>>;
}

/// Simple key-value database injection (wraps Aria KV registry).
#[async_trait::async_trait]
pub trait DatabaseInjection: Send + Sync {
    async fn get(&self, key: &str) -> Result<Option<String>>;
    async fn set(&self, key: &str, value: &str) -> Result<()>;
    async fn delete(&self, key: &str) -> Result<bool>;
    async fn list_keys(&self, prefix: Option<&str>) -> Result<Vec<String>>;
}

/// File system access injection (scoped to workspace).
#[async_trait::async_trait]
pub trait FileSystemInjection: Send + Sync {
    async fn read_file(&self, path: &str) -> Result<String>;
    async fn write_file(&self, path: &str, content: &str) -> Result<()>;
    async fn list_dir(&self, path: &str) -> Result<Vec<String>>;
    async fn file_exists(&self, path: &str) -> Result<bool>;
}

/// Network request injection (HTTP client with restrictions).
#[async_trait::async_trait]
pub trait NetworkInjection: Send + Sync {
    async fn http_get(&self, url: &str) -> Result<String>;
    async fn http_post(&self, url: &str, body: &str) -> Result<String>;
}

/// Notification dispatch injection.
#[async_trait::async_trait]
pub trait NotificationInjection: Send + Sync {
    async fn send(&self, channel: &str, message: &str) -> Result<()>;
    async fn send_with_metadata(
        &self,
        channel: &str,
        message: &str,
        metadata: &HashMap<String, serde_json::Value>,
    ) -> Result<()>;
}

/// Scheduler injection (create/manage cron jobs).
#[async_trait::async_trait]
pub trait SchedulerInjection: Send + Sync {
    async fn schedule(&self, expression: &str, command: &str) -> Result<String>;
    async fn unschedule(&self, job_id: &str) -> Result<bool>;
    async fn list_jobs(&self) -> Result<Vec<serde_json::Value>>;
}

/// Analytics/metrics injection.
#[async_trait::async_trait]
pub trait AnalyticsInjection: Send + Sync {
    fn track_event(&self, event: &str, properties: &HashMap<String, serde_json::Value>);
    fn increment_counter(&self, name: &str, value: u64);
    fn record_gauge(&self, name: &str, value: f64);
}

/// Secrets management injection.
#[async_trait::async_trait]
pub trait SecretsInjection: Send + Sync {
    async fn get_secret(&self, key: &str) -> Result<Option<String>>;
    async fn set_secret(&self, key: &str, value: &str) -> Result<()>;
    async fn delete_secret(&self, key: &str) -> Result<bool>;
}

/// Configuration access injection (read-only).
#[async_trait::async_trait]
pub trait ConfigInjection: Send + Sync {
    fn get_config(&self, key: &str) -> Option<String>;
    fn get_all_config(&self) -> HashMap<String, String>;
}

/// Event bus injection for pub/sub.
#[async_trait::async_trait]
pub trait EventInjection: Send + Sync {
    async fn emit(&self, event: &str, data: &serde_json::Value) -> Result<()>;
    async fn subscribe(&self, event: &str) -> Result<String>;
    async fn unsubscribe(&self, subscription_id: &str) -> Result<()>;
}

// ── Resolved Injection Enum ─────────────────────────────────────

/// A resolved runtime injection -- a capability the platform provides to agents.
#[allow(dead_code)]
pub enum ResolvedInjection {
    Logging(Arc<dyn LoggingInjection>),
    Memory(Arc<dyn MemoryInjection>),
    Tasks(Arc<dyn TaskInjection>),
    Database(Arc<dyn DatabaseInjection>),
    FileSystem(Arc<dyn FileSystemInjection>),
    Network(Arc<dyn NetworkInjection>),
    Notifications(Arc<dyn NotificationInjection>),
    Scheduler(Arc<dyn SchedulerInjection>),
    Analytics(Arc<dyn AnalyticsInjection>),
    Secrets(Arc<dyn SecretsInjection>),
    Config(Arc<dyn ConfigInjection>),
    Events(Arc<dyn EventInjection>),
}

// ── Runtime Injector ────────────────────────────────────────────

/// The `RuntimeInjector` resolves and provides injections to agents.
pub struct RuntimeInjector {
    #[allow(dead_code)]
    db: AriaDb,
    injections: HashMap<InjectionType, ResolvedInjection>,
}

impl RuntimeInjector {
    pub fn new(db: AriaDb) -> Self {
        Self {
            db,
            injections: HashMap::new(),
        }
    }

    /// Register an injection for the given type. Replaces existing if any.
    pub fn register(&mut self, injection_type: InjectionType, injection: ResolvedInjection) {
        self.injections.insert(injection_type, injection);
    }

    /// Get a reference to a resolved injection by type.
    pub fn get(&self, injection_type: &InjectionType) -> Option<&ResolvedInjection> {
        self.injections.get(injection_type)
    }

    /// Check if an injection type is registered.
    pub fn has(&self, injection_type: &InjectionType) -> bool {
        self.injections.contains_key(injection_type)
    }

    /// List all registered injection types.
    pub fn available_types(&self) -> Vec<&InjectionType> {
        self.injections.keys().collect()
    }
}

// ── Default Implementations ─────────────────────────────────────

/// Default logging injection backed by the `tracing` crate.
pub struct DefaultLogging;

#[async_trait::async_trait]
impl LoggingInjection for DefaultLogging {
    fn log(&self, level: &str, message: &str) {
        match level {
            "error" => tracing::error!(injection = "logging", "{message}"),
            "warn" => tracing::warn!(injection = "logging", "{message}"),
            "info" => tracing::info!(injection = "logging", "{message}"),
            "debug" => tracing::debug!(injection = "logging", "{message}"),
            "trace" => tracing::trace!(injection = "logging", "{message}"),
            _ => tracing::info!(injection = "logging", level = level, "{message}"),
        }
    }

    fn log_structured(
        &self,
        level: &str,
        message: &str,
        fields: &HashMap<String, serde_json::Value>,
    ) {
        let fields_str = serde_json::to_string(fields).unwrap_or_default();
        match level {
            "error" => tracing::error!(
                injection = "logging",
                fields = fields_str.as_str(),
                "{message}"
            ),
            "warn" => tracing::warn!(
                injection = "logging",
                fields = fields_str.as_str(),
                "{message}"
            ),
            "info" => tracing::info!(
                injection = "logging",
                fields = fields_str.as_str(),
                "{message}"
            ),
            "debug" => tracing::debug!(
                injection = "logging",
                fields = fields_str.as_str(),
                "{message}"
            ),
            _ => tracing::info!(
                injection = "logging",
                fields = fields_str.as_str(),
                level = level,
                "{message}"
            ),
        }
    }
}

/// Default memory injection backed by `AriaDb` memory registry.
pub struct DefaultMemory {
    db: AriaDb,
    tenant_id: String,
}

impl DefaultMemory {
    pub fn new(db: AriaDb, tenant_id: String) -> Self {
        Self { db, tenant_id }
    }
}

#[async_trait::async_trait]
impl MemoryInjection for DefaultMemory {
    async fn store(&self, key: &str, value: &str, namespace: Option<&str>) -> Result<()> {
        let now = chrono::Utc::now().to_rfc3339();
        let id = uuid::Uuid::new_v4().to_string();
        let ns = namespace.unwrap_or("default");
        self.db.with_conn(|conn| {
            conn.execute(
                "INSERT INTO aria_memory (id, tenant_id, key, value, tier, namespace, created_at, updated_at)
                 VALUES (?1, ?2, ?3, ?4, 'longterm', ?5, ?6, ?6)
                 ON CONFLICT(tenant_id, key, tier) DO UPDATE SET value = ?4, updated_at = ?6",
                rusqlite::params![id, self.tenant_id, key, value, ns, now],
            )?;
            Ok(())
        })
    }

    async fn recall(&self, key: &str, namespace: Option<&str>) -> Result<Option<String>> {
        let _ns = namespace.unwrap_or("default");
        self.db.with_conn(|conn| {
            let result = conn.query_row(
                "SELECT value FROM aria_memory WHERE tenant_id = ?1 AND key = ?2 AND tier = 'longterm'",
                rusqlite::params![self.tenant_id, key],
                |row| row.get::<_, String>(0),
            );
            match result {
                Ok(val) => Ok(Some(val)),
                Err(rusqlite::Error::QueryReturnedNoRows) => Ok(None),
                Err(e) => Err(e.into()),
            }
        })
    }

    async fn forget(&self, key: &str, namespace: Option<&str>) -> Result<()> {
        let _ns = namespace.unwrap_or("default");
        self.db.with_conn(|conn| {
            conn.execute(
                "DELETE FROM aria_memory WHERE tenant_id = ?1 AND key = ?2 AND tier = 'longterm'",
                rusqlite::params![self.tenant_id, key],
            )?;
            Ok(())
        })
    }
}

/// Default task injection backed by `AriaDb` task registry.
pub struct DefaultTasks {
    db: AriaDb,
    tenant_id: String,
}

impl DefaultTasks {
    pub fn new(db: AriaDb, tenant_id: String) -> Self {
        Self { db, tenant_id }
    }
}

#[async_trait::async_trait]
impl TaskInjection for DefaultTasks {
    async fn create_task(&self, name: &str, params: &serde_json::Value) -> Result<String> {
        let id = uuid::Uuid::new_v4().to_string();
        let now = chrono::Utc::now().to_rfc3339();
        let params_str = serde_json::to_string(params)?;
        self.db.with_conn(|conn| {
            conn.execute(
                "INSERT INTO aria_tasks (id, tenant_id, name, params, status, created_at, updated_at)
                 VALUES (?1, ?2, ?3, ?4, 'pending', ?5, ?5)",
                rusqlite::params![id, self.tenant_id, name, params_str, now],
            )?;
            Ok(())
        })?;
        Ok(id)
    }

    async fn get_task_status(&self, task_id: &str) -> Result<Option<String>> {
        self.db.with_conn(|conn| {
            let result = conn.query_row(
                "SELECT status FROM aria_tasks WHERE id = ?1 AND tenant_id = ?2",
                rusqlite::params![task_id, self.tenant_id],
                |row| row.get::<_, String>(0),
            );
            match result {
                Ok(status) => Ok(Some(status)),
                Err(rusqlite::Error::QueryReturnedNoRows) => Ok(None),
                Err(e) => Err(e.into()),
            }
        })
    }

    async fn cancel_task(&self, task_id: &str) -> Result<bool> {
        let now = chrono::Utc::now().to_rfc3339();
        self.db.with_conn(|conn| {
            let changed = conn.execute(
                "UPDATE aria_tasks SET status = 'cancelled', updated_at = ?1
                 WHERE id = ?2 AND tenant_id = ?3 AND status IN ('pending', 'running')",
                rusqlite::params![now, task_id, self.tenant_id],
            )?;
            Ok(changed > 0)
        })
    }

    async fn list_tasks(&self, status: Option<&str>) -> Result<Vec<serde_json::Value>> {
        let status_owned = status.map(String::from);
        self.db.with_conn(|conn| {
            let mut tasks = Vec::new();

            fn map_row(row: &rusqlite::Row<'_>) -> rusqlite::Result<serde_json::Value> {
                Ok(serde_json::json!({
                    "id": row.get::<_, String>(0)?,
                    "name": row.get::<_, String>(1)?,
                    "status": row.get::<_, String>(2)?,
                    "created_at": row.get::<_, String>(3)?,
                }))
            }

            if let Some(ref s) = status_owned {
                let mut stmt = conn.prepare(
                    "SELECT id, name, status, created_at FROM aria_tasks
                     WHERE tenant_id = ?1 AND status = ?2 ORDER BY created_at DESC",
                )?;
                let rows = stmt.query_map(rusqlite::params![self.tenant_id, s], map_row)?;
                for r in rows {
                    tasks.push(r?);
                }
            } else {
                let mut stmt = conn.prepare(
                    "SELECT id, name, status, created_at FROM aria_tasks
                     WHERE tenant_id = ?1 ORDER BY created_at DESC",
                )?;
                let rows = stmt.query_map(rusqlite::params![self.tenant_id], map_row)?;
                for r in rows {
                    tasks.push(r?);
                }
            }

            Ok(tasks)
        })
    }
}

/// Default database injection backed by `AriaDb` KV registry.
pub struct DefaultDatabase {
    db: AriaDb,
    tenant_id: String,
}

impl DefaultDatabase {
    pub fn new(db: AriaDb, tenant_id: String) -> Self {
        Self { db, tenant_id }
    }
}

#[async_trait::async_trait]
impl DatabaseInjection for DefaultDatabase {
    async fn get(&self, key: &str) -> Result<Option<String>> {
        self.db.with_conn(|conn| {
            let result = conn.query_row(
                "SELECT value FROM aria_kv WHERE tenant_id = ?1 AND key = ?2",
                rusqlite::params![self.tenant_id, key],
                |row| row.get::<_, String>(0),
            );
            match result {
                Ok(val) => Ok(Some(val)),
                Err(rusqlite::Error::QueryReturnedNoRows) => Ok(None),
                Err(e) => Err(e.into()),
            }
        })
    }

    async fn set(&self, key: &str, value: &str) -> Result<()> {
        let now = chrono::Utc::now().to_rfc3339();
        let id = uuid::Uuid::new_v4().to_string();
        self.db.with_conn(|conn| {
            conn.execute(
                "INSERT INTO aria_kv (id, tenant_id, key, value, created_at, updated_at)
                 VALUES (?1, ?2, ?3, ?4, ?5, ?5)
                 ON CONFLICT(tenant_id, key) DO UPDATE SET value = ?4, updated_at = ?5",
                rusqlite::params![id, self.tenant_id, key, value, now],
            )?;
            Ok(())
        })
    }

    async fn delete(&self, key: &str) -> Result<bool> {
        self.db.with_conn(|conn| {
            let changed = conn.execute(
                "DELETE FROM aria_kv WHERE tenant_id = ?1 AND key = ?2",
                rusqlite::params![self.tenant_id, key],
            )?;
            Ok(changed > 0)
        })
    }

    async fn list_keys(&self, prefix: Option<&str>) -> Result<Vec<String>> {
        self.db.with_conn(|conn| {
            let mut keys = Vec::new();
            if let Some(prefix) = prefix {
                let pattern = format!("{prefix}%");
                let mut stmt = conn.prepare(
                    "SELECT key FROM aria_kv WHERE tenant_id = ?1 AND key LIKE ?2 ORDER BY key",
                )?;
                let rows = stmt.query_map(rusqlite::params![self.tenant_id, pattern], |row| {
                    row.get::<_, String>(0)
                })?;
                for r in rows {
                    keys.push(r?);
                }
            } else {
                let mut stmt =
                    conn.prepare("SELECT key FROM aria_kv WHERE tenant_id = ?1 ORDER BY key")?;
                let rows = stmt.query_map(rusqlite::params![self.tenant_id], |row| {
                    row.get::<_, String>(0)
                })?;
                for r in rows {
                    keys.push(r?);
                }
            }
            Ok(keys)
        })
    }
}

/// Default file system injection — returns errors until workspace-scoped FS is wired.
pub struct DefaultFileSystem;

#[async_trait::async_trait]
impl FileSystemInjection for DefaultFileSystem {
    async fn read_file(&self, path: &str) -> Result<String> {
        anyhow::bail!("FileSystem injection not yet implemented (path: {path})")
    }

    async fn write_file(&self, path: &str, _content: &str) -> Result<()> {
        anyhow::bail!("FileSystem injection not yet implemented (path: {path})")
    }

    async fn list_dir(&self, path: &str) -> Result<Vec<String>> {
        anyhow::bail!("FileSystem injection not yet implemented (path: {path})")
    }

    async fn file_exists(&self, path: &str) -> Result<bool> {
        anyhow::bail!("FileSystem injection not yet implemented (path: {path})")
    }
}

/// Default network injection — returns errors until restricted HTTP client is wired.
pub struct DefaultNetwork;

#[async_trait::async_trait]
impl NetworkInjection for DefaultNetwork {
    async fn http_get(&self, url: &str) -> Result<String> {
        anyhow::bail!("Network injection not yet implemented (url: {url})")
    }

    async fn http_post(&self, url: &str, _body: &str) -> Result<String> {
        anyhow::bail!("Network injection not yet implemented (url: {url})")
    }
}

/// Default notification injection — logs notifications via tracing.
pub struct DefaultNotifications;

#[async_trait::async_trait]
impl NotificationInjection for DefaultNotifications {
    async fn send(&self, channel: &str, message: &str) -> Result<()> {
        tracing::info!(
            injection = "notifications",
            channel = channel,
            "Notification: {message}"
        );
        Ok(())
    }

    async fn send_with_metadata(
        &self,
        channel: &str,
        message: &str,
        _metadata: &HashMap<String, serde_json::Value>,
    ) -> Result<()> {
        self.send(channel, message).await
    }
}

/// Default scheduler injection — returns errors until `CronBridge` integration is wired.
pub struct DefaultScheduler;

#[async_trait::async_trait]
impl SchedulerInjection for DefaultScheduler {
    async fn schedule(&self, expression: &str, _command: &str) -> Result<String> {
        anyhow::bail!("Scheduler injection not yet implemented (expr: {expression})")
    }

    async fn unschedule(&self, job_id: &str) -> Result<bool> {
        anyhow::bail!("Scheduler injection not yet implemented (job_id: {job_id})")
    }

    async fn list_jobs(&self) -> Result<Vec<serde_json::Value>> {
        Ok(Vec::new())
    }
}

/// Default analytics injection — logs metrics via tracing.
pub struct DefaultAnalytics;

#[async_trait::async_trait]
impl AnalyticsInjection for DefaultAnalytics {
    fn track_event(&self, event: &str, properties: &HashMap<String, serde_json::Value>) {
        let props_str = serde_json::to_string(properties).unwrap_or_default();
        tracing::info!(
            injection = "analytics",
            event = event,
            properties = props_str.as_str(),
            "Analytics event tracked"
        );
    }

    fn increment_counter(&self, name: &str, value: u64) {
        tracing::debug!(
            injection = "analytics",
            counter = name,
            value = value,
            "Counter incremented"
        );
    }

    fn record_gauge(&self, name: &str, value: f64) {
        tracing::debug!(
            injection = "analytics",
            gauge = name,
            value = value,
            "Gauge recorded"
        );
    }
}

/// Default secrets injection — read-only (returns None); write/delete require vault integration.
pub struct DefaultSecrets;

#[async_trait::async_trait]
impl SecretsInjection for DefaultSecrets {
    async fn get_secret(&self, key: &str) -> Result<Option<String>> {
        tracing::debug!(
            injection = "secrets",
            key = key,
            "Secret requested (read-only default)"
        );
        Ok(None)
    }

    async fn set_secret(&self, key: &str, _value: &str) -> Result<()> {
        anyhow::bail!("Secrets injection not yet fully implemented (key: {key})")
    }

    async fn delete_secret(&self, key: &str) -> Result<bool> {
        anyhow::bail!("Secrets injection not yet fully implemented (key: {key})")
    }
}

/// Default config injection — provides read-only config values.
pub struct DefaultConfig {
    values: HashMap<String, String>,
}

impl DefaultConfig {
    pub fn new(values: HashMap<String, String>) -> Self {
        Self { values }
    }
}

#[async_trait::async_trait]
impl ConfigInjection for DefaultConfig {
    fn get_config(&self, key: &str) -> Option<String> {
        self.values.get(key).cloned()
    }

    fn get_all_config(&self) -> HashMap<String, String> {
        self.values.clone()
    }
}

/// Default event injection — logs events via tracing; does not persist subscriptions.
pub struct DefaultEvents;

#[async_trait::async_trait]
impl EventInjection for DefaultEvents {
    async fn emit(&self, event: &str, data: &serde_json::Value) -> Result<()> {
        tracing::info!(
            injection = "events",
            event = event,
            data = %data,
            "Event emitted"
        );
        Ok(())
    }

    async fn subscribe(&self, event: &str) -> Result<String> {
        let sub_id = uuid::Uuid::new_v4().to_string();
        tracing::debug!(
            injection = "events",
            event = event,
            subscription_id = sub_id.as_str(),
            "Subscribed to event (default, non-persisted)"
        );
        Ok(sub_id)
    }

    async fn unsubscribe(&self, subscription_id: &str) -> Result<()> {
        tracing::debug!(
            injection = "events",
            subscription_id = subscription_id,
            "Unsubscribed from event (default, non-persisted)"
        );
        Ok(())
    }
}

// ── Tests ───────────────────────────────────────────────────────

#[cfg(test)]
mod tests {
    use super::*;
    use crate::aria::db::AriaDb;
    use crate::aria::types::InjectionType;

    fn setup() -> (AriaDb, RuntimeInjector) {
        let db = AriaDb::open_in_memory().unwrap();
        let injector = RuntimeInjector::new(db.clone());
        (db, injector)
    }

    // ── RuntimeInjector tests ──────────────────────────────────

    #[test]
    fn injector_starts_empty() {
        let (_db, injector) = setup();
        assert!(!injector.has(&InjectionType::Logging));
        assert!(!injector.has(&InjectionType::Memory));
        assert!(injector.available_types().is_empty());
    }

    #[test]
    fn register_and_get_injection() {
        let (_db, mut injector) = setup();

        injector.register(
            InjectionType::Logging,
            ResolvedInjection::Logging(Arc::new(DefaultLogging)),
        );

        assert!(injector.has(&InjectionType::Logging));
        assert!(!injector.has(&InjectionType::Memory));
        assert!(injector.get(&InjectionType::Logging).is_some());
        assert!(injector.get(&InjectionType::Memory).is_none());
    }

    #[test]
    fn register_multiple_types() {
        let (db, mut injector) = setup();

        injector.register(
            InjectionType::Logging,
            ResolvedInjection::Logging(Arc::new(DefaultLogging)),
        );
        injector.register(
            InjectionType::Memory,
            ResolvedInjection::Memory(Arc::new(DefaultMemory::new(db.clone(), "test".to_string()))),
        );
        injector.register(
            InjectionType::Analytics,
            ResolvedInjection::Analytics(Arc::new(DefaultAnalytics)),
        );

        assert_eq!(injector.available_types().len(), 3);
        assert!(injector.has(&InjectionType::Logging));
        assert!(injector.has(&InjectionType::Memory));
        assert!(injector.has(&InjectionType::Analytics));
    }

    #[test]
    fn register_replaces_existing() {
        let (_db, mut injector) = setup();

        injector.register(
            InjectionType::Logging,
            ResolvedInjection::Logging(Arc::new(DefaultLogging)),
        );
        // Replace
        injector.register(
            InjectionType::Logging,
            ResolvedInjection::Logging(Arc::new(DefaultLogging)),
        );

        assert_eq!(injector.available_types().len(), 1);
    }

    #[test]
    fn available_types_reflects_registrations() {
        let (db, mut injector) = setup();

        injector.register(
            InjectionType::Database,
            ResolvedInjection::Database(Arc::new(DefaultDatabase::new(
                db.clone(),
                "test".to_string(),
            ))),
        );
        injector.register(
            InjectionType::Config,
            ResolvedInjection::Config(Arc::new(DefaultConfig::new(HashMap::new()))),
        );

        let types = injector.available_types();
        assert_eq!(types.len(), 2);
    }

    // ── DefaultLogging tests ───────────────────────────────────

    #[test]
    fn default_logging_does_not_panic() {
        let logger = DefaultLogging;
        logger.log("info", "test message");
        logger.log("error", "error message");
        logger.log("debug", "debug message");
        logger.log("warn", "warning");
        logger.log("trace", "trace");
        logger.log("custom", "custom level");

        let mut fields = HashMap::new();
        fields.insert("key".to_string(), serde_json::json!("value"));
        logger.log_structured("info", "structured msg", &fields);
    }

    // ── DefaultMemory tests ────────────────────────────────────

    #[tokio::test]
    async fn memory_store_and_recall() {
        let db = AriaDb::open_in_memory().unwrap();
        let memory = DefaultMemory::new(db, "test-tenant".to_string());

        memory.store("key1", "value1", None).await.unwrap();
        let result = memory.recall("key1", None).await.unwrap();
        assert_eq!(result.as_deref(), Some("value1"));
    }

    #[tokio::test]
    async fn memory_recall_missing_returns_none() {
        let db = AriaDb::open_in_memory().unwrap();
        let memory = DefaultMemory::new(db, "test-tenant".to_string());

        let result = memory.recall("nonexistent", None).await.unwrap();
        assert!(result.is_none());
    }

    #[tokio::test]
    async fn memory_store_overwrites() {
        let db = AriaDb::open_in_memory().unwrap();
        let memory = DefaultMemory::new(db, "test-tenant".to_string());

        memory.store("key1", "value1", None).await.unwrap();
        memory.store("key1", "value2", None).await.unwrap();
        let result = memory.recall("key1", None).await.unwrap();
        assert_eq!(result.as_deref(), Some("value2"));
    }

    #[tokio::test]
    async fn memory_forget_removes() {
        let db = AriaDb::open_in_memory().unwrap();
        let memory = DefaultMemory::new(db, "test-tenant".to_string());

        memory.store("key1", "value1", None).await.unwrap();
        memory.forget("key1", None).await.unwrap();
        let result = memory.recall("key1", None).await.unwrap();
        assert!(result.is_none());
    }

    // ── DefaultTasks tests ─────────────────────────────────────

    #[tokio::test]
    async fn tasks_create_and_get_status() {
        let db = AriaDb::open_in_memory().unwrap();
        let tasks = DefaultTasks::new(db, "test-tenant".to_string());

        let task_id = tasks
            .create_task("test-task", &serde_json::json!({"param": "value"}))
            .await
            .unwrap();
        assert!(!task_id.is_empty());

        let status = tasks.get_task_status(&task_id).await.unwrap();
        assert_eq!(status.as_deref(), Some("pending"));
    }

    #[tokio::test]
    async fn tasks_cancel() {
        let db = AriaDb::open_in_memory().unwrap();
        let tasks = DefaultTasks::new(db, "test-tenant".to_string());

        let task_id = tasks
            .create_task("cancel-me", &serde_json::json!({}))
            .await
            .unwrap();
        let cancelled = tasks.cancel_task(&task_id).await.unwrap();
        assert!(cancelled);

        let status = tasks.get_task_status(&task_id).await.unwrap();
        assert_eq!(status.as_deref(), Some("cancelled"));
    }

    #[tokio::test]
    async fn tasks_cancel_nonexistent_returns_false() {
        let db = AriaDb::open_in_memory().unwrap();
        let tasks = DefaultTasks::new(db, "test-tenant".to_string());

        let cancelled = tasks.cancel_task("no-such-task").await.unwrap();
        assert!(!cancelled);
    }

    #[tokio::test]
    async fn tasks_list_all() {
        let db = AriaDb::open_in_memory().unwrap();
        let tasks = DefaultTasks::new(db, "test-tenant".to_string());

        tasks
            .create_task("task-a", &serde_json::json!({}))
            .await
            .unwrap();
        tasks
            .create_task("task-b", &serde_json::json!({}))
            .await
            .unwrap();

        let all = tasks.list_tasks(None).await.unwrap();
        assert_eq!(all.len(), 2);
    }

    #[tokio::test]
    async fn tasks_list_filtered_by_status() {
        let db = AriaDb::open_in_memory().unwrap();
        let tasks = DefaultTasks::new(db, "test-tenant".to_string());

        let task_id = tasks
            .create_task("task-a", &serde_json::json!({}))
            .await
            .unwrap();
        tasks
            .create_task("task-b", &serde_json::json!({}))
            .await
            .unwrap();
        tasks.cancel_task(&task_id).await.unwrap();

        let pending = tasks.list_tasks(Some("pending")).await.unwrap();
        assert_eq!(pending.len(), 1);

        let cancelled = tasks.list_tasks(Some("cancelled")).await.unwrap();
        assert_eq!(cancelled.len(), 1);
    }

    // ── DefaultDatabase tests ──────────────────────────────────

    #[tokio::test]
    async fn database_set_and_get() {
        let db = AriaDb::open_in_memory().unwrap();
        let kv = DefaultDatabase::new(db, "test-tenant".to_string());

        kv.set("key1", "value1").await.unwrap();
        let result = kv.get("key1").await.unwrap();
        assert_eq!(result.as_deref(), Some("value1"));
    }

    #[tokio::test]
    async fn database_get_missing_returns_none() {
        let db = AriaDb::open_in_memory().unwrap();
        let kv = DefaultDatabase::new(db, "test-tenant".to_string());

        let result = kv.get("nonexistent").await.unwrap();
        assert!(result.is_none());
    }

    #[tokio::test]
    async fn database_set_overwrites() {
        let db = AriaDb::open_in_memory().unwrap();
        let kv = DefaultDatabase::new(db, "test-tenant".to_string());

        kv.set("key1", "value1").await.unwrap();
        kv.set("key1", "value2").await.unwrap();
        let result = kv.get("key1").await.unwrap();
        assert_eq!(result.as_deref(), Some("value2"));
    }

    #[tokio::test]
    async fn database_delete() {
        let db = AriaDb::open_in_memory().unwrap();
        let kv = DefaultDatabase::new(db, "test-tenant".to_string());

        kv.set("key1", "value1").await.unwrap();
        let deleted = kv.delete("key1").await.unwrap();
        assert!(deleted);

        let result = kv.get("key1").await.unwrap();
        assert!(result.is_none());
    }

    #[tokio::test]
    async fn database_delete_nonexistent_returns_false() {
        let db = AriaDb::open_in_memory().unwrap();
        let kv = DefaultDatabase::new(db, "test-tenant".to_string());

        let deleted = kv.delete("nonexistent").await.unwrap();
        assert!(!deleted);
    }

    #[tokio::test]
    async fn database_list_keys() {
        let db = AriaDb::open_in_memory().unwrap();
        let kv = DefaultDatabase::new(db, "test-tenant".to_string());

        kv.set("app:key1", "v1").await.unwrap();
        kv.set("app:key2", "v2").await.unwrap();
        kv.set("other:key3", "v3").await.unwrap();

        let all = kv.list_keys(None).await.unwrap();
        assert_eq!(all.len(), 3);

        let app_keys = kv.list_keys(Some("app:")).await.unwrap();
        assert_eq!(app_keys.len(), 2);

        let other_keys = kv.list_keys(Some("other:")).await.unwrap();
        assert_eq!(other_keys.len(), 1);
    }

    // ── DefaultConfig tests ────────────────────────────────────

    #[test]
    fn config_get_and_get_all() {
        let mut values = HashMap::new();
        values.insert("key1".to_string(), "value1".to_string());
        values.insert("key2".to_string(), "value2".to_string());

        let config = DefaultConfig::new(values);

        assert_eq!(config.get_config("key1"), Some("value1".to_string()));
        assert_eq!(config.get_config("key2"), Some("value2".to_string()));
        assert_eq!(config.get_config("key3"), None);

        let all = config.get_all_config();
        assert_eq!(all.len(), 2);
    }

    // ── DefaultAnalytics tests ─────────────────────────────────

    #[test]
    fn analytics_does_not_panic() {
        let analytics = DefaultAnalytics;
        analytics.track_event("test_event", &HashMap::new());
        analytics.increment_counter("test_counter", 1);
        analytics.record_gauge("test_gauge", 42.0);
    }

    // ── DefaultNotifications tests ─────────────────────────────

    #[tokio::test]
    async fn notifications_send_succeeds() {
        let notif = DefaultNotifications;
        notif.send("test-channel", "hello").await.unwrap();
    }

    #[tokio::test]
    async fn notifications_send_with_metadata_succeeds() {
        let notif = DefaultNotifications;
        let meta = HashMap::from([("key".to_string(), serde_json::json!("value"))]);
        notif
            .send_with_metadata("test-channel", "hello", &meta)
            .await
            .unwrap();
    }

    // ── DefaultEvents tests ────────────────────────────────────

    #[tokio::test]
    async fn events_emit_succeeds() {
        let events = DefaultEvents;
        events
            .emit("test_event", &serde_json::json!({"key": "value"}))
            .await
            .unwrap();
    }

    #[tokio::test]
    async fn events_subscribe_returns_id() {
        let events = DefaultEvents;
        let sub_id = events.subscribe("test_event").await.unwrap();
        assert!(!sub_id.is_empty());
    }

    #[tokio::test]
    async fn events_unsubscribe_succeeds() {
        let events = DefaultEvents;
        let sub_id = events.subscribe("test_event").await.unwrap();
        events.unsubscribe(&sub_id).await.unwrap();
    }

    // ── DefaultSecrets tests ───────────────────────────────────

    #[tokio::test]
    async fn secrets_get_returns_none() {
        let secrets = DefaultSecrets;
        let result = secrets.get_secret("api_key").await.unwrap();
        assert!(result.is_none());
    }

    #[tokio::test]
    async fn secrets_set_not_implemented() {
        let secrets = DefaultSecrets;
        let result = secrets.set_secret("key", "value").await;
        assert!(result.is_err());
    }

    // ── Unimplemented injection tests ──────────────────────────

    #[tokio::test]
    async fn filesystem_not_implemented() {
        let fs = DefaultFileSystem;
        assert!(fs.read_file("test.txt").await.is_err());
        assert!(fs.write_file("test.txt", "content").await.is_err());
        assert!(fs.list_dir("/tmp").await.is_err());
        assert!(fs.file_exists("test.txt").await.is_err());
    }

    #[tokio::test]
    async fn network_not_implemented() {
        let net = DefaultNetwork;
        assert!(net.http_get("https://example.com").await.is_err());
        assert!(net.http_post("https://example.com", "{}").await.is_err());
    }

    #[tokio::test]
    async fn scheduler_not_implemented() {
        let sched = DefaultScheduler;
        assert!(sched.schedule("*/5 * * * *", "echo hi").await.is_err());
        assert!(sched.unschedule("job-id").await.is_err());
        let jobs = sched.list_jobs().await.unwrap();
        assert!(jobs.is_empty());
    }

    // ── Full integration: register all 12 types ────────────────

    #[test]
    fn register_all_twelve_injection_types() {
        let db = AriaDb::open_in_memory().unwrap();
        let mut injector = RuntimeInjector::new(db.clone());

        injector.register(
            InjectionType::Logging,
            ResolvedInjection::Logging(Arc::new(DefaultLogging)),
        );
        injector.register(
            InjectionType::Memory,
            ResolvedInjection::Memory(Arc::new(DefaultMemory::new(db.clone(), "t1".to_string()))),
        );
        injector.register(
            InjectionType::Tasks,
            ResolvedInjection::Tasks(Arc::new(DefaultTasks::new(db.clone(), "t1".to_string()))),
        );
        injector.register(
            InjectionType::Database,
            ResolvedInjection::Database(Arc::new(DefaultDatabase::new(
                db.clone(),
                "t1".to_string(),
            ))),
        );
        injector.register(
            InjectionType::FileSystem,
            ResolvedInjection::FileSystem(Arc::new(DefaultFileSystem)),
        );
        injector.register(
            InjectionType::Network,
            ResolvedInjection::Network(Arc::new(DefaultNetwork)),
        );
        injector.register(
            InjectionType::Notifications,
            ResolvedInjection::Notifications(Arc::new(DefaultNotifications)),
        );
        injector.register(
            InjectionType::Scheduler,
            ResolvedInjection::Scheduler(Arc::new(DefaultScheduler)),
        );
        injector.register(
            InjectionType::Analytics,
            ResolvedInjection::Analytics(Arc::new(DefaultAnalytics)),
        );
        injector.register(
            InjectionType::Secrets,
            ResolvedInjection::Secrets(Arc::new(DefaultSecrets)),
        );
        injector.register(
            InjectionType::Config,
            ResolvedInjection::Config(Arc::new(DefaultConfig::new(HashMap::new()))),
        );
        injector.register(
            InjectionType::Events,
            ResolvedInjection::Events(Arc::new(DefaultEvents)),
        );

        assert_eq!(injector.available_types().len(), 12);

        // Verify all types are accessible
        for injection_type in &[
            InjectionType::Logging,
            InjectionType::Memory,
            InjectionType::Tasks,
            InjectionType::Database,
            InjectionType::FileSystem,
            InjectionType::Network,
            InjectionType::Notifications,
            InjectionType::Scheduler,
            InjectionType::Analytics,
            InjectionType::Secrets,
            InjectionType::Config,
            InjectionType::Events,
        ] {
            assert!(injector.has(injection_type), "Missing: {injection_type:?}");
            assert!(injector.get(injection_type).is_some());
        }
    }
}
