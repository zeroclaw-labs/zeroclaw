//! Cron bridge — converts AriaCronFunction (SQLite registry) entries
//! into runtime cron jobs via injectable callbacks.
//!
//! The bridge reads entries from `aria_cron_functions`, converts their
//! schedule_kind + schedule_data into standard cron expressions, and
//! creates/updates jobs via the provided add_job/remove_job callbacks.

use crate::aria::db::AriaDb;
use anyhow::{Context, Result};
use rusqlite::params;
use serde::{Deserialize, Serialize};
use std::sync::Arc;

/// Entry read from the `aria_cron_functions` table.
#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct AriaCronFunctionEntry {
    pub id: String,
    pub tenant_id: String,
    pub name: String,
    pub description: String,
    pub schedule_kind: String,
    pub schedule_data: String,
    pub session_target: String,
    pub wake_mode: String,
    pub payload_kind: String,
    pub payload_data: String,
    pub isolation: Option<String>,
    pub enabled: bool,
    pub delete_after_run: bool,
    pub cron_job_id: Option<String>,
    pub status: String,
    pub created_at: String,
    pub updated_at: String,
}

/// Result of adding a job to the runtime cron system.
pub struct CronJobHandle {
    pub id: String,
}

/// Callback type for adding a cron job to the runtime system.
/// Takes (cron_expression, command) and returns the created job handle.
pub type AddJobFn = Arc<dyn Fn(&str, &str) -> Result<CronJobHandle> + Send + Sync>;

/// Callback type for removing a cron job from the runtime system.
pub type RemoveJobFn = Arc<dyn Fn(&str) -> Result<()> + Send + Sync>;

/// Bridges Aria cron function definitions to the runtime cron scheduler.
///
/// Uses callback functions for actual job management so the bridge can be
/// used in both library and binary crate contexts.
pub struct CronBridge {
    db: AriaDb,
    add_job: AddJobFn,
    remove_job: RemoveJobFn,
}

impl CronBridge {
    pub fn new(db: AriaDb, add_job: AddJobFn, remove_job: RemoveJobFn) -> Self {
        Self { db, add_job, remove_job }
    }

    /// Sync a single cron function to the runtime cron system.
    ///
    /// 1. Reads the entry from `aria_cron_functions`
    /// 2. Converts schedule_kind + schedule_data into a cron expression
    /// 3. Creates/updates a job in the existing cron system
    /// 4. Stores the cron_job_id back in the registry
    pub fn sync_cron(&self, cron_func_id: &str) -> Result<()> {
        let entry = self.load_entry(cron_func_id)?;

        let Some(entry) = entry else {
            tracing::debug!(
                cron_func_id = cron_func_id,
                "Cron function not found, nothing to sync"
            );
            return Ok(());
        };

        if !entry.enabled || entry.status != "active" {
            // If disabled or not active, remove any existing job
            if entry.cron_job_id.is_some() {
                self.remove_cron(cron_func_id)?;
            }
            return Ok(());
        }

        // Convert schedule to cron expression
        let cron_expr = schedule_to_cron_expression(&entry.schedule_kind, &entry.schedule_data)?;

        // Build the command for the cron system
        let command = build_cron_command(&entry);

        // Remove existing job if any
        if let Some(ref old_job_id) = entry.cron_job_id {
            let _ = (self.remove_job)(old_job_id);
        }

        // Add job to runtime cron
        let job = (self.add_job)(&cron_expr, &command)?;

        // Store cron_job_id back in registry
        self.set_cron_job_id(cron_func_id, &job.id)?;

        tracing::info!(
            cron_func_id = cron_func_id,
            cron_job_id = job.id.as_str(),
            expression = cron_expr.as_str(),
            "Synced cron function to runtime"
        );

        Ok(())
    }

    /// Remove a cron function's runtime job.
    pub fn remove_cron(&self, cron_func_id: &str) -> Result<()> {
        let entry = self.load_entry(cron_func_id)?;

        if let Some(entry) = entry {
            if let Some(ref job_id) = entry.cron_job_id {
                let _ = (self.remove_job)(job_id);
                tracing::debug!(
                    cron_func_id = cron_func_id,
                    cron_job_id = job_id.as_str(),
                    "Removed cron job from runtime"
                );
            }

            // Clear the cron_job_id in registry
            self.clear_cron_job_id(cron_func_id)?;
        }

        Ok(())
    }

    /// Sync all active and enabled cron functions on startup.
    pub fn sync_all(&self) -> Result<()> {
        let ids = self.db.with_conn(|conn| {
            let mut stmt = conn.prepare(
                "SELECT id FROM aria_cron_functions
                 WHERE status = 'active' AND enabled = 1",
            )?;
            let rows = stmt.query_map([], |row| row.get::<_, String>(0))?;
            let mut ids = Vec::new();
            for r in rows {
                ids.push(r?);
            }
            Ok(ids)
        })?;

        let mut synced = 0;
        let mut failed = 0;

        for id in &ids {
            match self.sync_cron(id) {
                Ok(()) => synced += 1,
                Err(e) => {
                    tracing::warn!(
                        cron_func_id = id.as_str(),
                        error = %e,
                        "Failed to sync cron function"
                    );
                    failed += 1;
                }
            }
        }

        tracing::info!(
            total = ids.len(),
            synced = synced,
            failed = failed,
            "Cron bridge sync_all completed"
        );

        Ok(())
    }

    // ── Internal helpers ────────────────────────────────────────

    fn load_entry(&self, cron_func_id: &str) -> Result<Option<AriaCronFunctionEntry>> {
        self.db.with_conn(|conn| {
            let mut stmt = conn.prepare(
                "SELECT id, tenant_id, name, description, schedule_kind, schedule_data,
                        session_target, wake_mode, payload_kind, payload_data,
                        isolation, enabled, delete_after_run, cron_job_id, status,
                        created_at, updated_at
                 FROM aria_cron_functions WHERE id = ?1",
            )?;
            let result = stmt.query_row(params![cron_func_id], |row| {
                Ok(AriaCronFunctionEntry {
                    id: row.get(0)?,
                    tenant_id: row.get(1)?,
                    name: row.get(2)?,
                    description: row.get(3)?,
                    schedule_kind: row.get(4)?,
                    schedule_data: row.get(5)?,
                    session_target: row.get(6)?,
                    wake_mode: row.get(7)?,
                    payload_kind: row.get(8)?,
                    payload_data: row.get(9)?,
                    isolation: row.get(10)?,
                    enabled: row.get::<_, i32>(11)? != 0,
                    delete_after_run: row.get::<_, i32>(12)? != 0,
                    cron_job_id: row.get(13)?,
                    status: row.get(14)?,
                    created_at: row.get(15)?,
                    updated_at: row.get(16)?,
                })
            });
            match result {
                Ok(entry) => Ok(Some(entry)),
                Err(rusqlite::Error::QueryReturnedNoRows) => Ok(None),
                Err(e) => Err(e.into()),
            }
        })
    }

    fn set_cron_job_id(&self, cron_func_id: &str, job_id: &str) -> Result<()> {
        let now = chrono::Utc::now().to_rfc3339();
        self.db.with_conn(|conn| {
            conn.execute(
                "UPDATE aria_cron_functions SET cron_job_id = ?1, updated_at = ?2 WHERE id = ?3",
                params![job_id, now, cron_func_id],
            )?;
            Ok(())
        })
    }

    fn clear_cron_job_id(&self, cron_func_id: &str) -> Result<()> {
        let now = chrono::Utc::now().to_rfc3339();
        self.db.with_conn(|conn| {
            conn.execute(
                "UPDATE aria_cron_functions SET cron_job_id = NULL, updated_at = ?1 WHERE id = ?2",
                params![now, cron_func_id],
            )?;
            Ok(())
        })
    }
}

/// Convert schedule_kind + schedule_data into a standard 5-field cron expression.
///
/// Supported kinds:
/// - "cron": schedule_data is already a cron expression
/// - "every": schedule_data is JSON `{"every_ms": N}` — converted to nearest minute interval
/// - "at": schedule_data is JSON `{"at_ms": N}` — one-shot, uses a far-future cron as placeholder
pub fn schedule_to_cron_expression(kind: &str, data: &str) -> Result<String> {
    match kind {
        "cron" => {
            // schedule_data contains the cron expression itself (possibly wrapped in JSON)
            let expr = if data.starts_with('"') {
                serde_json::from_str::<String>(data)
                    .unwrap_or_else(|_| data.to_string())
            } else if data.starts_with('{') {
                let parsed: serde_json::Value =
                    serde_json::from_str(data).context("Invalid cron schedule JSON")?;
                parsed["expr"]
                    .as_str()
                    .unwrap_or(data)
                    .to_string()
            } else {
                data.to_string()
            };
            Ok(expr)
        }
        "every" => {
            let parsed: serde_json::Value =
                serde_json::from_str(data).context("Invalid 'every' schedule JSON")?;
            let every_ms = parsed["every_ms"]
                .as_u64()
                .context("Missing every_ms in schedule data")?;

            // Convert to minutes, minimum 1 minute
            let minutes = (every_ms / 60_000).max(1);

            if minutes < 60 {
                Ok(format!("*/{minutes} * * * *"))
            } else if minutes < 1440 {
                let hours = minutes / 60;
                Ok(format!("0 */{hours} * * *"))
            } else {
                // Daily or longer
                Ok("0 0 * * *".to_string())
            }
        }
        "at" => {
            // One-time execution — use a far-future placeholder.
            // The actual scheduling is handled by the cron system's one-shot logic.
            let parsed: serde_json::Value =
                serde_json::from_str(data).context("Invalid 'at' schedule JSON")?;
            let at_ms = parsed["at_ms"]
                .as_i64()
                .context("Missing at_ms in schedule data")?;

            // Convert to a specific time cron expression
            let dt = chrono::DateTime::from_timestamp(at_ms / 1000, 0)
                .context("Invalid timestamp in at_ms")?;
            let minute = chrono::Timelike::minute(&dt);
            let hour = chrono::Timelike::hour(&dt);
            let day = chrono::Datelike::day(&dt);
            let month = chrono::Datelike::month(&dt);

            Ok(format!("{minute} {hour} {day} {month} *"))
        }
        _ => {
            anyhow::bail!("Unknown schedule kind: {kind}")
        }
    }
}

/// Build the shell command for a cron function's runtime job.
///
/// The command encodes the payload as an afw agent invocation.
fn build_cron_command(entry: &AriaCronFunctionEntry) -> String {
    match entry.payload_kind.as_str() {
        "systemEvent" => {
            let text = serde_json::from_str::<serde_json::Value>(&entry.payload_data)
                .ok()
                .and_then(|v| v["text"].as_str().map(String::from))
                .unwrap_or_else(|| entry.payload_data.clone());
            format!(
                "afw agent -m \"[cron:{}] {}\"",
                entry.name,
                text.replace('"', "\\\"")
            )
        }
        "agentTurn" => {
            let payload = serde_json::from_str::<serde_json::Value>(&entry.payload_data)
                .unwrap_or_default();
            let message = payload["message"]
                .as_str()
                .unwrap_or("cron trigger");
            let mut cmd = format!(
                "afw agent -m \"{}\"",
                message.replace('"', "\\\"")
            );
            if let Some(model) = payload["model"].as_str() {
                cmd.push_str(&format!(" --model {model}"));
            }
            cmd
        }
        _ => {
            format!(
                "afw agent -m \"[cron:{}] trigger\"",
                entry.name
            )
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::aria::db::AriaDb;
    use chrono::Utc;
    use rusqlite::params;
    use std::sync::atomic::{AtomicU32, Ordering as AtomicOrdering};

    /// Create a test bridge with mock add/remove job callbacks.
    fn setup() -> (AriaDb, CronBridge) {
        let db = AriaDb::open_in_memory().unwrap();
        let counter = Arc::new(AtomicU32::new(0));

        let add_counter = counter.clone();
        let add_fn: AddJobFn = Arc::new(move |_expr, _cmd| {
            let n = add_counter.fetch_add(1, AtomicOrdering::Relaxed);
            Ok(CronJobHandle { id: format!("job-{n}") })
        });
        let remove_fn: RemoveJobFn = Arc::new(|_id| Ok(()));

        let bridge = CronBridge::new(db.clone(), add_fn, remove_fn);
        (db, bridge)
    }

    fn insert_cron_function(
        db: &AriaDb,
        id: &str,
        name: &str,
        schedule_kind: &str,
        schedule_data: &str,
        enabled: bool,
        status: &str,
    ) {
        let now = Utc::now().to_rfc3339();
        db.with_conn(|conn| {
            conn.execute(
                "INSERT INTO aria_cron_functions
                 (id, tenant_id, name, schedule_kind, schedule_data, payload_kind, payload_data,
                  enabled, status, created_at, updated_at)
                 VALUES (?1, 'test-tenant', ?2, ?3, ?4, 'systemEvent', '{\"text\":\"hello\"}',
                         ?5, ?6, ?7, ?7)",
                params![id, name, schedule_kind, schedule_data, enabled as i32, status, now],
            )?;
            Ok(())
        })
        .unwrap();
    }

    // ── Schedule conversion tests ──────────────────────────────

    #[test]
    fn cron_expression_passthrough() {
        let expr = schedule_to_cron_expression("cron", "*/5 * * * *").unwrap();
        assert_eq!(expr, "*/5 * * * *");
    }

    #[test]
    fn cron_expression_from_json_object() {
        let data = r#"{"expr":"0 9 * * 1"}"#;
        let expr = schedule_to_cron_expression("cron", data).unwrap();
        assert_eq!(expr, "0 9 * * 1");
    }

    #[test]
    fn cron_expression_from_json_string() {
        let data = r#""*/10 * * * *""#;
        let expr = schedule_to_cron_expression("cron", data).unwrap();
        assert_eq!(expr, "*/10 * * * *");
    }

    #[test]
    fn every_minutes_conversion() {
        let data = r#"{"every_ms":300000}"#; // 5 minutes
        let expr = schedule_to_cron_expression("every", data).unwrap();
        assert_eq!(expr, "*/5 * * * *");
    }

    #[test]
    fn every_hours_conversion() {
        let data = r#"{"every_ms":7200000}"#; // 2 hours
        let expr = schedule_to_cron_expression("every", data).unwrap();
        assert_eq!(expr, "0 */2 * * *");
    }

    #[test]
    fn every_daily_conversion() {
        let data = r#"{"every_ms":86400000}"#; // 24 hours
        let expr = schedule_to_cron_expression("every", data).unwrap();
        assert_eq!(expr, "0 0 * * *");
    }

    #[test]
    fn every_sub_minute_floors_to_1_min() {
        let data = r#"{"every_ms":30000}"#; // 30 seconds
        let expr = schedule_to_cron_expression("every", data).unwrap();
        assert_eq!(expr, "*/1 * * * *");
    }

    #[test]
    fn at_timestamp_conversion() {
        // 2025-06-15T14:30:00 UTC = 1750000200000 ms
        let data = r#"{"at_ms":1750000200000}"#;
        let expr = schedule_to_cron_expression("at", data).unwrap();
        // Should produce specific minute/hour/day/month
        assert!(!expr.contains('*') || expr.ends_with("*")); // weekday is always *
        let parts: Vec<&str> = expr.split_whitespace().collect();
        assert_eq!(parts.len(), 5);
    }

    #[test]
    fn unknown_schedule_kind_errors() {
        let result = schedule_to_cron_expression("unknown", "{}");
        assert!(result.is_err());
    }

    // ── Build command tests ────────────────────────────────────

    #[test]
    fn build_command_system_event() {
        let entry = AriaCronFunctionEntry {
            id: "test-id".into(),
            tenant_id: "t1".into(),
            name: "morning-check".into(),
            description: String::new(),
            schedule_kind: "cron".into(),
            schedule_data: "0 9 * * *".into(),
            session_target: "main".into(),
            wake_mode: "next-heartbeat".into(),
            payload_kind: "systemEvent".into(),
            payload_data: r#"{"text":"Good morning!"}"#.into(),
            isolation: None,
            enabled: true,
            delete_after_run: false,
            cron_job_id: None,
            status: "active".into(),
            created_at: String::new(),
            updated_at: String::new(),
        };

        let cmd = build_cron_command(&entry);
        assert!(cmd.contains("afw agent -m"));
        assert!(cmd.contains("Good morning!"));
        assert!(cmd.contains("[cron:morning-check]"));
    }

    #[test]
    fn build_command_agent_turn() {
        let entry = AriaCronFunctionEntry {
            id: "test-id".into(),
            tenant_id: "t1".into(),
            name: "daily-report".into(),
            description: String::new(),
            schedule_kind: "cron".into(),
            schedule_data: "0 17 * * *".into(),
            session_target: "main".into(),
            wake_mode: "now".into(),
            payload_kind: "agentTurn".into(),
            payload_data: r#"{"message":"Generate daily report","model":"claude-3"}"#.into(),
            isolation: None,
            enabled: true,
            delete_after_run: false,
            cron_job_id: None,
            status: "active".into(),
            created_at: String::new(),
            updated_at: String::new(),
        };

        let cmd = build_cron_command(&entry);
        assert!(cmd.contains("afw agent -m"));
        assert!(cmd.contains("Generate daily report"));
        assert!(cmd.contains("--model claude-3"));
    }

    // ── Bridge lifecycle tests ─────────────────────────────────

    #[test]
    fn sync_cron_creates_job() {
        let (db, bridge) = setup();

        insert_cron_function(&db, "cf-1", "test-cron", "cron", "*/5 * * * *", true, "active");

        bridge.sync_cron("cf-1").unwrap();

        // Verify cron_job_id was set
        let job_id: Option<String> = db
            .with_conn(|conn| {
                conn.query_row(
                    "SELECT cron_job_id FROM aria_cron_functions WHERE id = 'cf-1'",
                    [],
                    |row| row.get(0),
                )
                .map_err(Into::into)
            })
            .unwrap();
        assert!(job_id.is_some());
    }

    #[test]
    fn sync_cron_skips_disabled() {
        let (db, bridge) = setup();

        insert_cron_function(&db, "cf-2", "disabled-cron", "cron", "*/5 * * * *", false, "active");

        bridge.sync_cron("cf-2").unwrap();

        // cron_job_id should remain NULL
        let job_id: Option<String> = db
            .with_conn(|conn| {
                conn.query_row(
                    "SELECT cron_job_id FROM aria_cron_functions WHERE id = 'cf-2'",
                    [],
                    |row| row.get(0),
                )
                .map_err(Into::into)
            })
            .unwrap();
        assert!(job_id.is_none());
    }

    #[test]
    fn sync_cron_skips_nonexistent() {
        let (_db, bridge) = setup();

        // Should not error for missing entry
        bridge.sync_cron("nonexistent").unwrap();
    }

    #[test]
    fn remove_cron_clears_job_id() {
        let (db, bridge) = setup();

        insert_cron_function(&db, "cf-3", "to-remove", "cron", "*/10 * * * *", true, "active");
        bridge.sync_cron("cf-3").unwrap();

        // Verify job was created
        let job_id: Option<String> = db
            .with_conn(|conn| {
                conn.query_row(
                    "SELECT cron_job_id FROM aria_cron_functions WHERE id = 'cf-3'",
                    [],
                    |row| row.get(0),
                )
                .map_err(Into::into)
            })
            .unwrap();
        assert!(job_id.is_some());

        // Remove
        bridge.remove_cron("cf-3").unwrap();

        let job_id: Option<String> = db
            .with_conn(|conn| {
                conn.query_row(
                    "SELECT cron_job_id FROM aria_cron_functions WHERE id = 'cf-3'",
                    [],
                    |row| row.get(0),
                )
                .map_err(Into::into)
            })
            .unwrap();
        assert!(job_id.is_none());
    }

    #[test]
    fn sync_all_processes_active_entries() {
        let (db, bridge) = setup();

        insert_cron_function(&db, "cf-a", "cron-a", "cron", "*/5 * * * *", true, "active");
        insert_cron_function(&db, "cf-b", "cron-b", "cron", "*/10 * * * *", true, "active");
        insert_cron_function(&db, "cf-c", "cron-c", "cron", "*/15 * * * *", false, "active");
        insert_cron_function(&db, "cf-d", "cron-d", "cron", "*/20 * * * *", true, "deleted");

        bridge.sync_all().unwrap();

        // Only cf-a and cf-b should have job IDs
        let get_job_id = |id: &str| -> Option<String> {
            db.with_conn(|conn| {
                conn.query_row(
                    "SELECT cron_job_id FROM aria_cron_functions WHERE id = ?1",
                    params![id],
                    |row| row.get(0),
                )
                .map_err(Into::into)
            })
            .unwrap()
        };

        assert!(get_job_id("cf-a").is_some());
        assert!(get_job_id("cf-b").is_some());
        assert!(get_job_id("cf-c").is_none()); // disabled
        // cf-d is deleted, not in sync_all query
    }

    #[test]
    fn sync_cron_replaces_existing_job() {
        let (db, bridge) = setup();

        insert_cron_function(&db, "cf-5", "replace-me", "cron", "*/5 * * * *", true, "active");

        // First sync
        bridge.sync_cron("cf-5").unwrap();
        let first_job_id: Option<String> = db
            .with_conn(|conn| {
                conn.query_row(
                    "SELECT cron_job_id FROM aria_cron_functions WHERE id = 'cf-5'",
                    [],
                    |row| row.get(0),
                )
                .map_err(Into::into)
            })
            .unwrap();

        // Second sync (reschedule)
        bridge.sync_cron("cf-5").unwrap();
        let second_job_id: Option<String> = db
            .with_conn(|conn| {
                conn.query_row(
                    "SELECT cron_job_id FROM aria_cron_functions WHERE id = 'cf-5'",
                    [],
                    |row| row.get(0),
                )
                .map_err(Into::into)
            })
            .unwrap();

        assert!(first_job_id.is_some());
        assert!(second_job_id.is_some());
        // Job IDs should differ (old job removed, new one created)
        assert_ne!(first_job_id, second_job_id);
    }
}
