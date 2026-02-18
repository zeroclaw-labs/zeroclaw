use anyhow::{Context, Result};
use rusqlite::{params, Connection, OptionalExtension};
use std::path::Path;

/// Represents an agent lifecycle/task event.
#[derive(Debug, Clone)]
pub struct AgentEvent {
    pub id: String,
    pub instance_id: String,
    pub event_type: String,
    pub channel: Option<String>,
    pub summary: Option<String>,
    pub status: String,
    pub duration_ms: Option<i64>,
    pub correlation_id: Option<String>,
    pub metadata: Option<String>,
    pub created_at: String,
}

/// Represents a single model usage record.
#[derive(Debug, Clone)]
pub struct AgentUsageRecord {
    pub id: String,
    pub instance_id: String,
    pub input_tokens: Option<i64>,
    pub output_tokens: Option<i64>,
    pub total_tokens: Option<i64>,
    pub provider: Option<String>,
    pub model: Option<String>,
    pub request_id: Option<String>,
    pub created_at: String,
}

/// Aggregated usage summary for an instance within a time window.
#[derive(Debug, Clone)]
pub struct AgentUsageSummary {
    pub input_tokens: Option<i64>,
    pub output_tokens: Option<i64>,
    pub total_tokens: Option<i64>,
    pub request_count: usize,
    pub unknown_count: usize,
}

/// Represents a managed ZeroClaw instance in the CP registry.
#[derive(Debug, Clone)]
pub struct Instance {
    pub id: String,
    pub name: String,
    pub status: String,
    pub port: u16,
    pub config_path: String,
    pub workspace_dir: Option<String>,
    pub archived_at: Option<String>,
    pub migration_run_id: Option<String>,
    /// Best-effort PID cache. The pidfile on disk is authoritative.
    pub pid: Option<u32>,
}

/// SQLite-backed registry for managing ZeroClaw instances.
pub struct Registry {
    conn: Connection,
}

impl Registry {
    /// Open (or create) the registry database at the given path.
    pub fn open(db_path: &Path) -> Result<Self> {
        let conn = Connection::open(db_path)
            .with_context(|| format!("Failed to open registry DB: {}", db_path.display()))?;

        conn.execute_batch("PRAGMA journal_mode=WAL; PRAGMA foreign_keys=ON;")
            .context("Failed to set SQLite pragmas")?;

        Self::init_schema(&conn)?;
        Ok(Self { conn })
    }

    /// Open an in-memory registry (for testing).
    #[cfg(test)]
    pub fn open_in_memory() -> Result<Self> {
        let conn = Connection::open_in_memory()?;
        conn.execute_batch("PRAGMA foreign_keys=ON;")?;
        Self::init_schema(&conn)?;
        Ok(Self { conn })
    }

    fn init_schema(conn: &Connection) -> Result<()> {
        conn.execute_batch(
            "CREATE TABLE IF NOT EXISTS instances (
                id TEXT PRIMARY KEY NOT NULL,
                name TEXT NOT NULL,
                status TEXT NOT NULL DEFAULT 'stopped',
                port INTEGER NOT NULL,
                config_path TEXT NOT NULL,
                workspace_dir TEXT,
                archived_at TEXT,
                created_at TEXT NOT NULL DEFAULT (datetime('now'))
            );
            CREATE INDEX IF NOT EXISTS idx_instances_name ON instances(name);",
        )
        .context("Failed to initialize registry schema")?;

        // Migration: add migration_run_id column if missing (pre-phase5 DBs lack it).
        let has_column = conn
            .prepare("PRAGMA table_info(instances)")?
            .query_map([], |row| row.get::<_, String>(1))?
            .filter_map(|r| r.ok())
            .any(|col| col == "migration_run_id");

        if !has_column {
            conn.execute_batch("ALTER TABLE instances ADD COLUMN migration_run_id TEXT;")?;
        }

        conn.execute_batch(
            "CREATE INDEX IF NOT EXISTS idx_instances_migration_run_id ON instances(migration_run_id);",
        )?;

        // Migration: add pid column if missing (pre-phase7 DBs lack it).
        let has_pid_column = conn
            .prepare("PRAGMA table_info(instances)")?
            .query_map([], |row| row.get::<_, String>(1))?
            .filter_map(|r| r.ok())
            .any(|col| col == "pid");

        if !has_pid_column {
            conn.execute_batch("ALTER TABLE instances ADD COLUMN pid INTEGER;")?;
        }

        // Phase 7.5: unique active-name index (prevents duplicate active names)
        let dupes: Vec<(String, i64)> = conn
            .prepare("SELECT name, COUNT(*) as cnt FROM instances WHERE archived_at IS NULL GROUP BY name HAVING cnt > 1")?
            .query_map([], |row| Ok((row.get(0)?, row.get(1)?)))?
            .filter_map(|r| r.ok())
            .collect();

        if !dupes.is_empty() {
            let names: Vec<&str> = dupes.iter().map(|(n, _)| n.as_str()).collect();
            anyhow::bail!(
                "Cannot create unique active-name index: duplicate active instance names found: {:?}. \
                 Resolve manually by archiving or renaming duplicates, then restart. \
                 SQL to inspect: SELECT id, name, status FROM instances WHERE name IN ({}) AND archived_at IS NULL",
                names,
                names.iter().map(|n| format!("'{n}'")).collect::<Vec<_>>().join(", ")
            );
        }

        conn.execute_batch(
            "CREATE UNIQUE INDEX IF NOT EXISTS idx_instances_active_name
             ON instances(name) WHERE archived_at IS NULL;"
        )?;

        // Phase 7.5: agent_events table
        conn.execute_batch(
            "CREATE TABLE IF NOT EXISTS agent_events (
                id TEXT PRIMARY KEY NOT NULL,
                instance_id TEXT NOT NULL,
                event_type TEXT NOT NULL,
                channel TEXT,
                summary TEXT,
                status TEXT NOT NULL DEFAULT 'completed',
                duration_ms INTEGER,
                correlation_id TEXT,
                metadata TEXT,
                created_at TEXT NOT NULL DEFAULT (datetime('now')),
                FOREIGN KEY (instance_id) REFERENCES instances(id)
            );
            CREATE INDEX IF NOT EXISTS idx_agent_events_instance_created
                ON agent_events(instance_id, created_at DESC);"
        )?;

        // Phase 7.5: agent_usage table
        conn.execute_batch(
            "CREATE TABLE IF NOT EXISTS agent_usage (
                id TEXT PRIMARY KEY NOT NULL,
                instance_id TEXT NOT NULL,
                input_tokens INTEGER,
                output_tokens INTEGER,
                total_tokens INTEGER,
                provider TEXT,
                model TEXT,
                request_id TEXT,
                created_at TEXT NOT NULL DEFAULT (datetime('now')),
                FOREIGN KEY (instance_id) REFERENCES instances(id)
            );
            CREATE INDEX IF NOT EXISTS idx_agent_usage_instance_created
                ON agent_usage(instance_id, created_at DESC);"
        )?;

        Ok(())
    }

    /// Create a new instance in the registry.
    pub fn create_instance(
        &self,
        id: &str,
        name: &str,
        port: u16,
        config_path: &str,
        workspace_dir: Option<&str>,
        migration_run_id: Option<&str>,
    ) -> Result<()> {
        self.conn.execute(
            "INSERT INTO instances (id, name, status, port, config_path, workspace_dir, migration_run_id)
             VALUES (?1, ?2, 'stopped', ?3, ?4, ?5, ?6)",
            params![id, name, port as i64, config_path, workspace_dir, migration_run_id],
        ).with_context(|| format!("Failed to create instance '{name}'"))?;
        Ok(())
    }

    /// Get an instance by ID.
    pub fn get_instance(&self, id: &str) -> Result<Option<Instance>> {
        self.conn
            .query_row(
                "SELECT id, name, status, port, config_path, workspace_dir, archived_at, migration_run_id, pid
                 FROM instances WHERE id = ?1",
                params![id],
                |row| {
                    Ok(Instance {
                        id: row.get(0)?,
                        name: row.get(1)?,
                        status: row.get(2)?,
                        port: row.get::<_, i64>(3)? as u16,
                        config_path: row.get(4)?,
                        workspace_dir: row.get(5)?,
                        archived_at: row.get(6)?,
                        migration_run_id: row.get(7)?,
                        pid: row.get::<_, Option<i64>>(8)?.map(|p| p as u32),
                    })
                },
            )
            .optional()
            .context("Failed to query instance by ID")
    }

    /// Get a non-archived instance by name.
    pub fn get_instance_by_name(&self, name: &str) -> Result<Option<Instance>> {
        self.conn
            .query_row(
                "SELECT id, name, status, port, config_path, workspace_dir, archived_at, migration_run_id, pid
                 FROM instances WHERE name = ?1 AND archived_at IS NULL",
                params![name],
                |row| {
                    Ok(Instance {
                        id: row.get(0)?,
                        name: row.get(1)?,
                        status: row.get(2)?,
                        port: row.get::<_, i64>(3)? as u16,
                        config_path: row.get(4)?,
                        workspace_dir: row.get(5)?,
                        archived_at: row.get(6)?,
                        migration_run_id: row.get(7)?,
                        pid: row.get::<_, Option<i64>>(8)?.map(|p| p as u32),
                    })
                },
            )
            .optional()
            .context("Failed to query instance by name")
    }

    /// Find an archived instance by name.
    pub fn find_archived_instance_by_name(&self, name: &str) -> Result<Option<Instance>> {
        self.conn
            .query_row(
                "SELECT id, name, status, port, config_path, workspace_dir, archived_at, migration_run_id, pid
                 FROM instances WHERE name = ?1 AND archived_at IS NOT NULL",
                params![name],
                |row| {
                    Ok(Instance {
                        id: row.get(0)?,
                        name: row.get(1)?,
                        status: row.get(2)?,
                        port: row.get::<_, i64>(3)? as u16,
                        config_path: row.get(4)?,
                        workspace_dir: row.get(5)?,
                        archived_at: row.get(6)?,
                        migration_run_id: row.get(7)?,
                        pid: row.get::<_, Option<i64>>(8)?.map(|p| p as u32),
                    })
                },
            )
            .optional()
            .context("Failed to query archived instance by name")
    }

    /// Delete an instance only if its migration_run_id matches.
    /// Returns true if a row was deleted, false if no match.
    pub fn delete_instance_if_migration(&self, id: &str, run_id: &str) -> Result<bool> {
        let rows = self
            .conn
            .execute(
                "DELETE FROM instances WHERE id = ?1 AND migration_run_id = ?2",
                params![id, run_id],
            )
            .context("Failed to delete migration instance")?;
        Ok(rows > 0)
    }

    /// Allocate the next available port in [start, end], skipping ports already
    /// in the DB and any in the excludes list. Linear scan, deterministic.
    /// Returns None if no port is available.
    pub fn allocate_port_with_excludes(
        &self,
        start: u16,
        end: u16,
        excludes: &[u16],
    ) -> Result<Option<u16>> {
        let mut stmt = self
            .conn
            .prepare("SELECT port FROM instances WHERE archived_at IS NULL")?;
        let used: std::collections::HashSet<u16> = stmt
            .query_map([], |row| Ok(row.get::<_, i64>(0)? as u16))?
            .filter_map(|r| r.ok())
            .collect();

        for port in start..=end {
            if !used.contains(&port) && !excludes.contains(&port) {
                return Ok(Some(port));
            }
        }
        Ok(None)
    }

    /// Update the status of an instance by ID.
    pub fn update_status(&self, id: &str, status: &str) -> Result<()> {
        let rows = self
            .conn
            .execute(
                "UPDATE instances SET status = ?1 WHERE id = ?2",
                params![status, id],
            )
            .context("Failed to update instance status")?;
        if rows == 0 {
            anyhow::bail!("No instance with id '{id}'");
        }
        Ok(())
    }

    /// Update the cached PID for an instance (best-effort cache; pidfile is authoritative).
    pub fn update_pid(&self, id: &str, pid: Option<u32>) -> Result<()> {
        let rows = self
            .conn
            .execute(
                "UPDATE instances SET pid = ?1 WHERE id = ?2",
                params![pid.map(|p| p as i64), id],
            )
            .context("Failed to update instance PID")?;
        if rows == 0 {
            anyhow::bail!("No instance with id '{id}'");
        }
        Ok(())
    }

    /// Borrow the underlying connection (for rollback operations).
    pub fn conn(&self) -> &Connection {
        &self.conn
    }

    // ── Agent events (Phase 7.5) ──────────────────────────────────

    /// Insert an agent event record.
    pub fn insert_agent_event(&self, event: &AgentEvent) -> Result<()> {
        self.conn.execute(
            "INSERT INTO agent_events (id, instance_id, event_type, channel, summary, status, duration_ms, correlation_id, metadata, created_at)
             VALUES (?1, ?2, ?3, ?4, ?5, ?6, ?7, ?8, ?9, ?10)",
            params![
                event.id,
                event.instance_id,
                event.event_type,
                event.channel,
                event.summary,
                event.status,
                event.duration_ms,
                event.correlation_id,
                event.metadata,
                event.created_at,
            ],
        ).context("Failed to insert agent event")?;
        Ok(())
    }

    /// List agent events for an instance with pagination and filtering.
    /// Returns (events, total_count).
    pub fn list_agent_events(
        &self,
        instance_id: &str,
        limit: usize,
        offset: usize,
        status_filter: Option<&str>,
        after: Option<&str>,
        before: Option<&str>,
    ) -> Result<(Vec<AgentEvent>, usize)> {
        let mut where_clauses = vec!["instance_id = ?1".to_string()];
        let mut param_idx = 2u32;
        let mut bind_values: Vec<Box<dyn rusqlite::types::ToSql>> = vec![Box::new(instance_id.to_string())];

        if let Some(status) = status_filter {
            where_clauses.push(format!("status = ?{param_idx}"));
            bind_values.push(Box::new(status.to_string()));
            param_idx += 1;
        }
        if let Some(after_ts) = after {
            where_clauses.push(format!("created_at > ?{param_idx}"));
            bind_values.push(Box::new(after_ts.to_string()));
            param_idx += 1;
        }
        if let Some(before_ts) = before {
            where_clauses.push(format!("created_at < ?{param_idx}"));
            bind_values.push(Box::new(before_ts.to_string()));
            param_idx += 1;
        }

        let where_sql = where_clauses.join(" AND ");

        // Count total
        let count_sql = format!("SELECT COUNT(*) FROM agent_events WHERE {where_sql}");
        let params_ref: Vec<&dyn rusqlite::types::ToSql> = bind_values.iter().map(|b| b.as_ref()).collect();
        let total: usize = self.conn.query_row(&count_sql, params_ref.as_slice(), |row| {
            row.get::<_, i64>(0).map(|v| v as usize)
        })?;

        // Query with pagination
        let query_sql = format!(
            "SELECT id, instance_id, event_type, channel, summary, status, duration_ms, correlation_id, metadata, created_at \
             FROM agent_events WHERE {where_sql} \
             ORDER BY created_at DESC, id DESC \
             LIMIT ?{param_idx} OFFSET ?{}",
            param_idx + 1
        );
        let mut all_params: Vec<Box<dyn rusqlite::types::ToSql>> = bind_values;
        all_params.push(Box::new(limit as i64));
        all_params.push(Box::new(offset as i64));

        let params_ref: Vec<&dyn rusqlite::types::ToSql> = all_params.iter().map(|b| b.as_ref()).collect();
        let mut stmt = self.conn.prepare(&query_sql)?;
        let rows = stmt.query_map(params_ref.as_slice(), |row| {
            Ok(AgentEvent {
                id: row.get(0)?,
                instance_id: row.get(1)?,
                event_type: row.get(2)?,
                channel: row.get(3)?,
                summary: row.get(4)?,
                status: row.get(5)?,
                duration_ms: row.get(6)?,
                correlation_id: row.get(7)?,
                metadata: row.get(8)?,
                created_at: row.get(9)?,
            })
        })?;

        let mut events = Vec::new();
        for row in rows {
            events.push(row?);
        }
        Ok((events, total))
    }

    // ── Agent usage (Phase 7.5) ─────────────────────────────────

    /// Insert an agent usage record.
    pub fn insert_agent_usage(&self, usage: &AgentUsageRecord) -> Result<()> {
        self.conn.execute(
            "INSERT INTO agent_usage (id, instance_id, input_tokens, output_tokens, total_tokens, provider, model, request_id, created_at)
             VALUES (?1, ?2, ?3, ?4, ?5, ?6, ?7, ?8, ?9)",
            params![
                usage.id,
                usage.instance_id,
                usage.input_tokens,
                usage.output_tokens,
                usage.total_tokens,
                usage.provider,
                usage.model,
                usage.request_id,
                usage.created_at,
            ],
        ).context("Failed to insert agent usage")?;
        Ok(())
    }

    /// Get aggregated usage for an instance within a time window.
    pub fn get_agent_usage(
        &self,
        instance_id: &str,
        window_start: Option<&str>,
        window_end: Option<&str>,
    ) -> Result<AgentUsageSummary> {
        let mut where_clauses = vec!["instance_id = ?1".to_string()];
        let mut bind_values: Vec<Box<dyn rusqlite::types::ToSql>> = vec![Box::new(instance_id.to_string())];
        let mut param_idx = 2u32;

        if let Some(start) = window_start {
            where_clauses.push(format!("created_at >= ?{param_idx}"));
            bind_values.push(Box::new(start.to_string()));
            param_idx += 1;
        }
        if let Some(end) = window_end {
            where_clauses.push(format!("created_at <= ?{param_idx}"));
            bind_values.push(Box::new(end.to_string()));
        }

        let where_sql = where_clauses.join(" AND ");
        let sql = format!(
            "SELECT \
                SUM(input_tokens), \
                SUM(output_tokens), \
                SUM(total_tokens), \
                COUNT(*), \
                SUM(CASE WHEN total_tokens IS NULL THEN 1 ELSE 0 END) \
             FROM agent_usage WHERE {where_sql}"
        );

        let params_ref: Vec<&dyn rusqlite::types::ToSql> = bind_values.iter().map(|b| b.as_ref()).collect();
        self.conn.query_row(&sql, params_ref.as_slice(), |row| {
            Ok(AgentUsageSummary {
                input_tokens: row.get(0)?,
                output_tokens: row.get(1)?,
                total_tokens: row.get(2)?,
                request_count: row.get::<_, Option<i64>>(3)?.unwrap_or(0) as usize,
                unknown_count: row.get::<_, Option<i64>>(4)?.unwrap_or(0) as usize,
            })
        }).context("Failed to query agent usage")
    }

    /// List all non-archived instances.
    pub fn list_instances(&self) -> Result<Vec<Instance>> {
        let mut stmt = self.conn.prepare(
            "SELECT id, name, status, port, config_path, workspace_dir, archived_at, migration_run_id, pid
             FROM instances WHERE archived_at IS NULL ORDER BY name",
        )?;
        let rows = stmt.query_map([], |row| {
            Ok(Instance {
                id: row.get(0)?,
                name: row.get(1)?,
                status: row.get(2)?,
                port: row.get::<_, i64>(3)? as u16,
                config_path: row.get(4)?,
                workspace_dir: row.get(5)?,
                archived_at: row.get(6)?,
                migration_run_id: row.get(7)?,
                pid: row.get::<_, Option<i64>>(8)?.map(|p| p as u32),
            })
        })?;
        let mut instances = Vec::new();
        for row in rows {
            instances.push(row?);
        }
        Ok(instances)
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn create_and_get_instance() {
        let reg = Registry::open_in_memory().unwrap();
        reg.create_instance("id-1", "test-agent", 18801, "/tmp/config.toml", Some("/tmp/ws"), None)
            .unwrap();

        let inst = reg.get_instance("id-1").unwrap().unwrap();
        assert_eq!(inst.name, "test-agent");
        assert_eq!(inst.port, 18801);
        assert_eq!(inst.status, "stopped");
        assert!(inst.migration_run_id.is_none());
    }

    #[test]
    fn get_instance_by_name_excludes_archived() {
        let reg = Registry::open_in_memory().unwrap();
        reg.create_instance("id-1", "agent", 18801, "/tmp/c.toml", None, None)
            .unwrap();
        // Archive it
        reg.conn
            .execute(
                "UPDATE instances SET archived_at = datetime('now') WHERE id = 'id-1'",
                [],
            )
            .unwrap();

        assert!(reg.get_instance_by_name("agent").unwrap().is_none());
        assert!(reg.find_archived_instance_by_name("agent").unwrap().is_some());
    }

    #[test]
    fn delete_instance_if_migration_scoped() {
        let reg = Registry::open_in_memory().unwrap();
        reg.create_instance("id-1", "a1", 18801, "/c.toml", None, Some("run-abc"))
            .unwrap();

        // Wrong run_id: should not delete
        assert!(!reg.delete_instance_if_migration("id-1", "run-xyz").unwrap());
        assert!(reg.get_instance("id-1").unwrap().is_some());

        // Correct run_id: should delete
        assert!(reg.delete_instance_if_migration("id-1", "run-abc").unwrap());
        assert!(reg.get_instance("id-1").unwrap().is_none());
    }

    #[test]
    fn allocate_port_skips_used_and_excluded() {
        let reg = Registry::open_in_memory().unwrap();
        reg.create_instance("id-1", "a1", 18801, "/c.toml", None, None)
            .unwrap();

        let port = reg
            .allocate_port_with_excludes(18801, 18810, &[18802])
            .unwrap()
            .unwrap();
        assert_eq!(port, 18803);
    }

    #[test]
    fn allocate_port_returns_none_when_exhausted() {
        let reg = Registry::open_in_memory().unwrap();
        reg.create_instance("id-1", "a1", 18801, "/c.toml", None, None)
            .unwrap();
        reg.create_instance("id-2", "a2", 18802, "/c.toml", None, None)
            .unwrap();

        let port = reg
            .allocate_port_with_excludes(18801, 18802, &[])
            .unwrap();
        assert!(port.is_none());
    }

    #[test]
    fn schema_migration_adds_migration_run_id_column() {
        // Simulate a pre-phase5 DB: create table WITHOUT migration_run_id,
        // then open via Registry which should ALTER TABLE to add it.
        let conn = Connection::open_in_memory().unwrap();
        conn.execute_batch("PRAGMA foreign_keys=ON;").unwrap();
        conn.execute_batch(
            "CREATE TABLE instances (
                id TEXT PRIMARY KEY NOT NULL,
                name TEXT NOT NULL,
                status TEXT NOT NULL DEFAULT 'stopped',
                port INTEGER NOT NULL,
                config_path TEXT NOT NULL,
                workspace_dir TEXT,
                archived_at TEXT,
                created_at TEXT NOT NULL DEFAULT (datetime('now'))
            );",
        )
        .unwrap();

        // Insert a row without migration_run_id (old schema)
        conn.execute(
            "INSERT INTO instances (id, name, status, port, config_path)
             VALUES ('old-1', 'legacy-agent', 'running', 18801, '/old/config.toml')",
            [],
        )
        .unwrap();

        // Now run init_schema which should add the column
        Registry::init_schema(&conn).unwrap();

        // Verify: column exists and old row is readable with NULL migration_run_id
        let reg = Registry { conn };
        let inst = reg.get_instance("old-1").unwrap().unwrap();
        assert_eq!(inst.name, "legacy-agent");
        assert_eq!(inst.status, "running");
        assert!(inst.migration_run_id.is_none());

        // Verify: new instances with migration_run_id work
        reg.create_instance("new-1", "new-agent", 18802, "/new/config.toml", None, Some("run-123"))
            .unwrap();
        let new_inst = reg.get_instance("new-1").unwrap().unwrap();
        assert_eq!(new_inst.migration_run_id.as_deref(), Some("run-123"));
    }

    #[test]
    fn update_status_changes_instance_status() {
        let reg = Registry::open_in_memory().unwrap();
        reg.create_instance("id-1", "agent", 18801, "/c.toml", None, None)
            .unwrap();

        assert_eq!(reg.get_instance("id-1").unwrap().unwrap().status, "stopped");

        reg.update_status("id-1", "running").unwrap();
        assert_eq!(reg.get_instance("id-1").unwrap().unwrap().status, "running");

        reg.update_status("id-1", "stopped").unwrap();
        assert_eq!(reg.get_instance("id-1").unwrap().unwrap().status, "stopped");
    }

    #[test]
    fn update_status_errors_on_missing_instance() {
        let reg = Registry::open_in_memory().unwrap();
        let err = reg.update_status("nonexistent", "running").unwrap_err();
        assert!(err.to_string().contains("No instance"));
    }

    #[test]
    fn list_instances_excludes_archived() {
        let reg = Registry::open_in_memory().unwrap();
        reg.create_instance("id-1", "active", 18801, "/c.toml", None, None)
            .unwrap();
        reg.create_instance("id-2", "archived", 18802, "/c.toml", None, None)
            .unwrap();
        reg.conn
            .execute(
                "UPDATE instances SET archived_at = datetime('now') WHERE id = 'id-2'",
                [],
            )
            .unwrap();

        let list = reg.list_instances().unwrap();
        assert_eq!(list.len(), 1);
        assert_eq!(list[0].name, "active");
    }

    #[test]
    fn update_pid_roundtrip() {
        let reg = Registry::open_in_memory().unwrap();
        reg.create_instance("id-1", "agent", 18801, "/c.toml", None, None)
            .unwrap();

        // Initially None
        assert!(reg.get_instance("id-1").unwrap().unwrap().pid.is_none());

        // Set PID
        reg.update_pid("id-1", Some(12345)).unwrap();
        assert_eq!(reg.get_instance("id-1").unwrap().unwrap().pid, Some(12345));

        // Clear PID
        reg.update_pid("id-1", None).unwrap();
        assert!(reg.get_instance("id-1").unwrap().unwrap().pid.is_none());
    }

    #[test]
    fn update_pid_errors_on_missing_instance() {
        let reg = Registry::open_in_memory().unwrap();
        let err = reg.update_pid("nonexistent", Some(123)).unwrap_err();
        assert!(err.to_string().contains("No instance"));
    }
}
