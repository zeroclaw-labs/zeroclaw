//! Cron function registry — SQLite-backed store for scheduled function definitions.
//!
//! Unlike other registries, cron functions use HARD deletes. Supports
//! `schedule_kind` (at|every|cron), session targeting, wake modes, and
//! optional delete-after-run semantics.

use super::db::AriaDb;
use anyhow::{Context, Result};
use chrono::Utc;
use rusqlite::params;
use serde::{Deserialize, Serialize};
use std::collections::{HashMap, HashSet};
use std::sync::atomic::{AtomicBool, Ordering};
use std::sync::Mutex;
use uuid::Uuid;

// ── Entry ────────────────────────────────────────────────────────

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

// ── Registry ─────────────────────────────────────────────────────

pub struct AriaCronFunctionRegistry {
    db: AriaDb,
    cache: Mutex<HashMap<String, AriaCronFunctionEntry>>,
    tenant_index: Mutex<HashMap<String, HashSet<String>>>,
    name_index: Mutex<HashMap<String, String>>,
    loaded: AtomicBool,
}

impl AriaCronFunctionRegistry {
    pub fn new(db: AriaDb) -> Self {
        Self {
            db,
            cache: Mutex::new(HashMap::new()),
            tenant_index: Mutex::new(HashMap::new()),
            name_index: Mutex::new(HashMap::new()),
            loaded: AtomicBool::new(false),
        }
    }

    fn ensure_loaded(&self) -> Result<()> {
        if self.loaded.load(Ordering::Acquire) {
            return Ok(());
        }
        let entries = self.db.with_conn(|conn| {
            let mut stmt = conn.prepare(
                "SELECT id, tenant_id, name, description, schedule_kind, schedule_data,
                        session_target, wake_mode, payload_kind, payload_data,
                        isolation, enabled, delete_after_run, cron_job_id, status,
                        created_at, updated_at
                 FROM aria_cron_functions",
            )?;
            let rows = stmt.query_map([], |row| {
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
                    enabled: row.get(11)?,
                    delete_after_run: row.get(12)?,
                    cron_job_id: row.get(13)?,
                    status: row.get(14)?,
                    created_at: row.get(15)?,
                    updated_at: row.get(16)?,
                })
            })?;
            let mut out = Vec::new();
            for r in rows {
                out.push(r?);
            }
            Ok(out)
        })?;

        let mut cache = self.cache.lock().unwrap();
        let mut ti = self.tenant_index.lock().unwrap();
        let mut ni = self.name_index.lock().unwrap();
        cache.clear();
        ti.clear();
        ni.clear();

        for e in entries {
            ti.entry(e.tenant_id.clone())
                .or_default()
                .insert(e.id.clone());
            ni.insert(format!("{}:{}", e.tenant_id, e.name), e.id.clone());
            cache.insert(e.id.clone(), e);
        }
        self.loaded.store(true, Ordering::Release);
        Ok(())
    }

    fn index_entry(&self, entry: &AriaCronFunctionEntry) {
        self.tenant_index
            .lock()
            .unwrap()
            .entry(entry.tenant_id.clone())
            .or_default()
            .insert(entry.id.clone());
        self.name_index.lock().unwrap().insert(
            format!("{}:{}", entry.tenant_id, entry.name),
            entry.id.clone(),
        );
    }

    fn deindex_entry(&self, entry: &AriaCronFunctionEntry) {
        if let Some(set) = self.tenant_index.lock().unwrap().get_mut(&entry.tenant_id) {
            set.remove(&entry.id);
        }
        self.name_index
            .lock()
            .unwrap()
            .remove(&format!("{}:{}", entry.tenant_id, entry.name));
    }

    // ── Public API ───────────────────────────────────────────────

    #[allow(clippy::too_many_arguments)]
    pub fn create(
        &self,
        tenant_id: &str,
        name: &str,
        description: &str,
        schedule_kind: &str,
        schedule_data: &str,
        session_target: &str,
        wake_mode: &str,
        payload_kind: &str,
        payload_data: &str,
        isolation: Option<&str>,
        enabled: bool,
        delete_after_run: bool,
    ) -> Result<AriaCronFunctionEntry> {
        self.ensure_loaded()?;
        let now = Utc::now().to_rfc3339();

        let existing_id = {
            let ni = self.name_index.lock().unwrap();
            ni.get(&format!("{tenant_id}:{name}")).cloned()
        };

        if let Some(eid) = existing_id {
            let mut cache = self.cache.lock().unwrap();
            let entry = cache.get(&eid).cloned().context("Cache inconsistency")?;
            self.db.with_conn(|conn| {
                conn.execute(
                    "UPDATE aria_cron_functions SET description=?1, schedule_kind=?2,
                     schedule_data=?3, session_target=?4, wake_mode=?5, payload_kind=?6,
                     payload_data=?7, isolation=?8, enabled=?9, delete_after_run=?10,
                     status='active', cron_job_id=NULL, updated_at=?11 WHERE id=?12",
                    params![
                        description,
                        schedule_kind,
                        schedule_data,
                        session_target,
                        wake_mode,
                        payload_kind,
                        payload_data,
                        isolation,
                        enabled,
                        delete_after_run,
                        now,
                        eid
                    ],
                )?;
                Ok(())
            })?;
            let updated = AriaCronFunctionEntry {
                description: description.to_string(),
                schedule_kind: schedule_kind.to_string(),
                schedule_data: schedule_data.to_string(),
                session_target: session_target.to_string(),
                wake_mode: wake_mode.to_string(),
                payload_kind: payload_kind.to_string(),
                payload_data: payload_data.to_string(),
                isolation: isolation.map(String::from),
                enabled,
                delete_after_run,
                status: "active".to_string(),
                cron_job_id: None,
                updated_at: now,
                ..entry
            };
            cache.insert(eid, updated.clone());
            Ok(updated)
        } else {
            let id = Uuid::new_v4().to_string();
            let entry = AriaCronFunctionEntry {
                id: id.clone(),
                tenant_id: tenant_id.to_string(),
                name: name.to_string(),
                description: description.to_string(),
                schedule_kind: schedule_kind.to_string(),
                schedule_data: schedule_data.to_string(),
                session_target: session_target.to_string(),
                wake_mode: wake_mode.to_string(),
                payload_kind: payload_kind.to_string(),
                payload_data: payload_data.to_string(),
                isolation: isolation.map(String::from),
                enabled,
                delete_after_run,
                cron_job_id: None,
                status: "active".to_string(),
                created_at: now.clone(),
                updated_at: now,
            };
            self.db.with_conn(|conn| {
                conn.execute(
                    "INSERT INTO aria_cron_functions (id, tenant_id, name, description,
                     schedule_kind, schedule_data, session_target, wake_mode, payload_kind,
                     payload_data, isolation, enabled, delete_after_run, cron_job_id,
                     status, created_at, updated_at)
                     VALUES (?1,?2,?3,?4,?5,?6,?7,?8,?9,?10,?11,?12,?13,?14,?15,?16,?17)",
                    params![
                        entry.id,
                        entry.tenant_id,
                        entry.name,
                        entry.description,
                        entry.schedule_kind,
                        entry.schedule_data,
                        entry.session_target,
                        entry.wake_mode,
                        entry.payload_kind,
                        entry.payload_data,
                        entry.isolation,
                        entry.enabled,
                        entry.delete_after_run,
                        entry.cron_job_id,
                        entry.status,
                        entry.created_at,
                        entry.updated_at
                    ],
                )?;
                Ok(())
            })?;
            self.index_entry(&entry);
            self.cache.lock().unwrap().insert(id, entry.clone());
            Ok(entry)
        }
    }

    pub fn get(&self, id: &str) -> Result<Option<AriaCronFunctionEntry>> {
        self.ensure_loaded()?;
        Ok(self.cache.lock().unwrap().get(id).cloned())
    }

    pub fn get_by_name(
        &self,
        tenant_id: &str,
        name: &str,
    ) -> Result<Option<AriaCronFunctionEntry>> {
        self.ensure_loaded()?;
        let id = self
            .name_index
            .lock()
            .unwrap()
            .get(&format!("{tenant_id}:{name}"))
            .cloned();
        match id {
            Some(id) => self.get(&id),
            None => Ok(None),
        }
    }

    pub fn list(&self, tenant_id: &str) -> Result<Vec<AriaCronFunctionEntry>> {
        self.ensure_loaded()?;
        let ti = self.tenant_index.lock().unwrap();
        let ids = match ti.get(tenant_id) {
            Some(set) => set.clone(),
            None => return Ok(Vec::new()),
        };
        drop(ti);
        let cache = self.cache.lock().unwrap();
        Ok(ids.iter().filter_map(|id| cache.get(id).cloned()).collect())
    }

    pub fn count(&self, tenant_id: &str) -> Result<usize> {
        self.ensure_loaded()?;
        Ok(self
            .tenant_index
            .lock()
            .unwrap()
            .get(tenant_id)
            .map_or(0, std::collections::HashSet::len))
    }

    /// HARD delete — permanently removes the cron function from DB and cache.
    pub fn delete(&self, id: &str) -> Result<bool> {
        self.ensure_loaded()?;
        let entry = self.cache.lock().unwrap().get(id).cloned();
        let Some(entry) = entry else { return Ok(false) };
        self.db.with_conn(|conn| {
            conn.execute("DELETE FROM aria_cron_functions WHERE id=?1", params![id])?;
            Ok(())
        })?;
        self.deindex_entry(&entry);
        self.cache.lock().unwrap().remove(id);
        Ok(true)
    }

    /// Associate a platform cron job ID with this function.
    pub fn set_cron_job_id(&self, id: &str, cron_job_id: &str) -> Result<bool> {
        self.ensure_loaded()?;
        let entry = self.cache.lock().unwrap().get(id).cloned();
        let Some(entry) = entry else { return Ok(false) };
        let now = Utc::now().to_rfc3339();
        self.db.with_conn(|conn| {
            conn.execute(
                "UPDATE aria_cron_functions SET cron_job_id=?1, updated_at=?2 WHERE id=?3",
                params![cron_job_id, now, id],
            )?;
            Ok(())
        })?;
        let updated = AriaCronFunctionEntry {
            cron_job_id: Some(cron_job_id.to_string()),
            updated_at: now,
            ..entry
        };
        self.cache.lock().unwrap().insert(id.to_string(), updated);
        Ok(true)
    }

    /// Look up a cron function by its platform `cron_job_id`.
    pub fn get_by_cron_job_id(&self, cron_job_id: &str) -> Result<Option<AriaCronFunctionEntry>> {
        self.ensure_loaded()?;
        let cache = self.cache.lock().unwrap();
        Ok(cache
            .values()
            .find(|e| e.cron_job_id.as_deref() == Some(cron_job_id))
            .cloned())
    }
}

// ── Tests ────────────────────────────────────────────────────────

#[cfg(test)]
mod tests {
    use super::*;
    use crate::aria::db::AriaDb;

    fn setup() -> AriaCronFunctionRegistry {
        let db = AriaDb::open_in_memory().unwrap();
        AriaCronFunctionRegistry::new(db)
    }

    fn create_default(
        reg: &AriaCronFunctionRegistry,
        tenant: &str,
        name: &str,
    ) -> AriaCronFunctionEntry {
        reg.create(
            tenant,
            name,
            "desc",
            "every",
            r#"{"every_ms":60000}"#,
            "main",
            "now",
            "systemEvent",
            r#"{"text":"tick"}"#,
            None,
            true,
            false,
        )
        .unwrap()
    }

    #[test]
    fn create_and_get_roundtrip() {
        let reg = setup();
        let entry = create_default(&reg, "t1", "ticker");
        assert_eq!(entry.name, "ticker");
        assert_eq!(entry.schedule_kind, "every");
        assert!(entry.enabled);

        let fetched = reg.get(&entry.id).unwrap().unwrap();
        assert_eq!(fetched.id, entry.id);
    }

    #[test]
    fn upsert_by_name_updates_existing() {
        let reg = setup();
        let v1 = create_default(&reg, "t1", "cron");
        let v2 = reg
            .create(
                "t1",
                "cron",
                "updated",
                "cron",
                r#"{"expr":"0 * * * *"}"#,
                "isolated",
                "now",
                "agentTurn",
                r#"{"message":"go"}"#,
                None,
                false,
                true,
            )
            .unwrap();
        assert_eq!(v2.id, v1.id);
        assert_eq!(v2.schedule_kind, "cron");
        assert!(!v2.enabled);
        assert!(v2.delete_after_run);
        assert_eq!(reg.count("t1").unwrap(), 1);
    }

    #[test]
    fn list_with_tenant_isolation() {
        let reg = setup();
        create_default(&reg, "t1", "a");
        create_default(&reg, "t1", "b");
        create_default(&reg, "t2", "c");

        assert_eq!(reg.list("t1").unwrap().len(), 2);
        assert_eq!(reg.list("t2").unwrap().len(), 1);
        assert_eq!(reg.list("t3").unwrap().len(), 0);
    }

    #[test]
    fn hard_delete_permanently_removes() {
        let reg = setup();
        let entry = create_default(&reg, "t1", "doomed");
        assert!(reg.delete(&entry.id).unwrap());
        assert_eq!(reg.count("t1").unwrap(), 0);
        assert!(reg.get(&entry.id).unwrap().is_none());

        // Verify it's actually gone from DB (not just soft-deleted)
        let exists = reg
            .db
            .with_conn(|conn| {
                let count: i64 = conn.query_row(
                    "SELECT COUNT(*) FROM aria_cron_functions WHERE id=?1",
                    params![entry.id],
                    |row| row.get(0),
                )?;
                Ok(count > 0)
            })
            .unwrap();
        assert!(!exists);
    }

    #[test]
    fn delete_nonexistent_returns_false() {
        let reg = setup();
        assert!(!reg.delete("ghost").unwrap());
    }

    #[test]
    fn count_accuracy() {
        let reg = setup();
        assert_eq!(reg.count("t1").unwrap(), 0);
        create_default(&reg, "t1", "x");
        assert_eq!(reg.count("t1").unwrap(), 1);
    }

    #[test]
    fn set_and_get_by_cron_job_id() {
        let reg = setup();
        let entry = create_default(&reg, "t1", "scheduled");
        assert!(entry.cron_job_id.is_none());

        reg.set_cron_job_id(&entry.id, "platform-job-123").unwrap();

        let updated = reg.get(&entry.id).unwrap().unwrap();
        assert_eq!(updated.cron_job_id.as_deref(), Some("platform-job-123"));

        let found = reg.get_by_cron_job_id("platform-job-123").unwrap().unwrap();
        assert_eq!(found.id, entry.id);

        assert!(reg.get_by_cron_job_id("no-such-job").unwrap().is_none());
    }

    #[test]
    fn get_by_name_works() {
        let reg = setup();
        create_default(&reg, "t1", "named-cron");
        let found = reg.get_by_name("t1", "named-cron").unwrap().unwrap();
        assert_eq!(found.name, "named-cron");
        assert!(reg.get_by_name("t2", "named-cron").unwrap().is_none());
    }

    #[test]
    fn persistence_across_reload() {
        let db = AriaDb::open_in_memory().unwrap();
        {
            let reg = AriaCronFunctionRegistry::new(db.clone());
            create_default(&reg, "t1", "persist-cron");
        }
        let reg2 = AriaCronFunctionRegistry::new(db);
        let entry = reg2.get_by_name("t1", "persist-cron").unwrap().unwrap();
        assert_eq!(entry.name, "persist-cron");
    }
}
