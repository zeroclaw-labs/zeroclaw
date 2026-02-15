//! Task registry — SQLite-backed store for Aria background task tracking.
//!
//! Tasks track execution lifecycle: pending -> running -> completed/failed/cancelled.
//! Soft-deleted when removed.

use super::db::AriaDb;
use anyhow::{Context, Result};
use chrono::Utc;
use rusqlite::params;
use serde::{Deserialize, Serialize};
use std::collections::{HashMap, HashSet};
use std::sync::atomic::{AtomicBool, Ordering};
use std::sync::Mutex;
use uuid::Uuid;

// ── Helpers ──────────────────────────────────────────────────────

fn sha256_hex(input: &str) -> String {
    use std::collections::hash_map::DefaultHasher;
    use std::hash::{Hash, Hasher};
    let mut h = DefaultHasher::new();
    input.hash(&mut h);
    format!("{:016x}", h.finish())
}

// ── Entry ────────────────────────────────────────────────────────

#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct AriaTaskEntry {
    pub id: String,
    pub tenant_id: String,
    pub name: String,
    pub description: String,
    pub handler_code: String,
    pub handler_hash: String,
    pub params: String,
    pub status: String,
    pub result: Option<String>,
    pub error: Option<String>,
    pub agent_id: Option<String>,
    pub started_at: Option<String>,
    pub completed_at: Option<String>,
    pub created_at: String,
    pub updated_at: String,
}

// ── Registry ─────────────────────────────────────────────────────

pub struct AriaTaskRegistry {
    db: AriaDb,
    cache: Mutex<HashMap<String, AriaTaskEntry>>,
    tenant_index: Mutex<HashMap<String, HashSet<String>>>,
    name_index: Mutex<HashMap<String, String>>,
    loaded: AtomicBool,
}

impl AriaTaskRegistry {
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
                "SELECT id, tenant_id, name, description, handler_code, handler_hash,
                        params, status, result, error, agent_id, started_at,
                        completed_at, created_at, updated_at
                 FROM aria_tasks",
            )?;
            let rows = stmt.query_map([], |row| {
                Ok(AriaTaskEntry {
                    id: row.get(0)?,
                    tenant_id: row.get(1)?,
                    name: row.get(2)?,
                    description: row.get(3)?,
                    handler_code: row.get(4)?,
                    handler_hash: row.get(5)?,
                    params: row.get(6)?,
                    status: row.get(7)?,
                    result: row.get(8)?,
                    error: row.get(9)?,
                    agent_id: row.get(10)?,
                    started_at: row.get(11)?,
                    completed_at: row.get(12)?,
                    created_at: row.get(13)?,
                    updated_at: row.get(14)?,
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

    fn index_entry(&self, entry: &AriaTaskEntry) {
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

    fn deindex_entry(&self, entry: &AriaTaskEntry) {
        if let Some(set) = self.tenant_index.lock().unwrap().get_mut(&entry.tenant_id) {
            set.remove(&entry.id);
        }
        self.name_index
            .lock()
            .unwrap()
            .remove(&format!("{}:{}", entry.tenant_id, entry.name));
    }

    // ── Public API ───────────────────────────────────────────────

    /// Create a new task (or update if same tenant+name).
    pub fn create(
        &self,
        tenant_id: &str,
        name: &str,
        description: &str,
        handler_code: &str,
        params: &str,
        agent_id: Option<&str>,
    ) -> Result<AriaTaskEntry> {
        self.ensure_loaded()?;
        let now = Utc::now().to_rfc3339();
        let hash = sha256_hex(handler_code);

        let existing_id = {
            let ni = self.name_index.lock().unwrap();
            ni.get(&format!("{tenant_id}:{name}")).cloned()
        };

        if let Some(eid) = existing_id {
            let mut cache = self.cache.lock().unwrap();
            let entry = cache.get(&eid).cloned().context("Cache inconsistency")?;
            self.db.with_conn(|conn| {
                conn.execute(
                    "UPDATE aria_tasks SET description=?1, handler_code=?2, handler_hash=?3,
                     params=?4, agent_id=?5, status='pending', result=NULL, error=NULL,
                     started_at=NULL, completed_at=NULL, updated_at=?6
                     WHERE id=?7",
                    params![description, handler_code, hash, params, agent_id, now, eid],
                )?;
                Ok(())
            })?;
            let updated = AriaTaskEntry {
                description: description.to_string(),
                handler_code: handler_code.to_string(),
                handler_hash: hash,
                params: params.to_string(),
                agent_id: agent_id.map(String::from),
                status: "pending".to_string(),
                result: None,
                error: None,
                started_at: None,
                completed_at: None,
                updated_at: now,
                ..entry
            };
            cache.insert(eid, updated.clone());
            Ok(updated)
        } else {
            let id = Uuid::new_v4().to_string();
            let entry = AriaTaskEntry {
                id: id.clone(),
                tenant_id: tenant_id.to_string(),
                name: name.to_string(),
                description: description.to_string(),
                handler_code: handler_code.to_string(),
                handler_hash: hash,
                params: params.to_string(),
                status: "pending".to_string(),
                result: None,
                error: None,
                agent_id: agent_id.map(String::from),
                started_at: None,
                completed_at: None,
                created_at: now.clone(),
                updated_at: now,
            };
            self.db.with_conn(|conn| {
                conn.execute(
                    "INSERT INTO aria_tasks (id, tenant_id, name, description, handler_code,
                     handler_hash, params, status, result, error, agent_id, started_at,
                     completed_at, created_at, updated_at)
                     VALUES (?1,?2,?3,?4,?5,?6,?7,?8,?9,?10,?11,?12,?13,?14,?15)",
                    params![
                        entry.id,
                        entry.tenant_id,
                        entry.name,
                        entry.description,
                        entry.handler_code,
                        entry.handler_hash,
                        entry.params,
                        entry.status,
                        entry.result,
                        entry.error,
                        entry.agent_id,
                        entry.started_at,
                        entry.completed_at,
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

    pub fn get(&self, id: &str) -> Result<Option<AriaTaskEntry>> {
        self.ensure_loaded()?;
        Ok(self.cache.lock().unwrap().get(id).cloned())
    }

    pub fn get_by_name(&self, tenant_id: &str, name: &str) -> Result<Option<AriaTaskEntry>> {
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

    pub fn list(&self, tenant_id: &str) -> Result<Vec<AriaTaskEntry>> {
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

    /// Soft-delete a task.
    pub fn delete(&self, id: &str) -> Result<bool> {
        self.ensure_loaded()?;
        let entry = self.cache.lock().unwrap().get(id).cloned();
        let Some(entry) = entry else { return Ok(false) };
        self.db.with_conn(|conn| {
            conn.execute("DELETE FROM aria_tasks WHERE id=?1", params![id])?;
            Ok(())
        })?;
        self.deindex_entry(&entry);
        self.cache.lock().unwrap().remove(id);
        Ok(true)
    }

    /// Update task status with optional result/error and timestamps.
    pub fn update_status(
        &self,
        id: &str,
        status: &str,
        result: Option<&str>,
        error: Option<&str>,
    ) -> Result<bool> {
        self.ensure_loaded()?;
        let entry = self.cache.lock().unwrap().get(id).cloned();
        let Some(entry) = entry else { return Ok(false) };
        let now = Utc::now().to_rfc3339();

        let started_at = if status == "running" && entry.started_at.is_none() {
            Some(now.clone())
        } else {
            entry.started_at.clone()
        };
        let completed_at = if status == "completed" || status == "failed" || status == "cancelled" {
            Some(now.clone())
        } else {
            entry.completed_at.clone()
        };

        self.db.with_conn(|conn| {
            conn.execute(
                "UPDATE aria_tasks SET status=?1, result=?2, error=?3,
                 started_at=?4, completed_at=?5, updated_at=?6 WHERE id=?7",
                params![status, result, error, started_at, completed_at, now, id],
            )?;
            Ok(())
        })?;

        let updated = AriaTaskEntry {
            status: status.to_string(),
            result: result.map(String::from),
            error: error.map(String::from),
            started_at,
            completed_at,
            updated_at: now,
            ..entry
        };
        self.cache.lock().unwrap().insert(id.to_string(), updated);
        Ok(true)
    }

    /// Cancel a task (sets status to "cancelled").
    pub fn cancel(&self, id: &str) -> Result<bool> {
        self.update_status(id, "cancelled", None, None)
    }
}

// ── Tests ────────────────────────────────────────────────────────

#[cfg(test)]
mod tests {
    use super::*;
    use crate::aria::db::AriaDb;

    fn setup() -> AriaTaskRegistry {
        let db = AriaDb::open_in_memory().unwrap();
        AriaTaskRegistry::new(db)
    }

    #[test]
    fn create_and_get_roundtrip() {
        let reg = setup();
        let entry = reg
            .create("t1", "my-task", "desc", "run()", "{}", None)
            .unwrap();
        assert_eq!(entry.name, "my-task");
        assert_eq!(entry.status, "pending");
        let fetched = reg.get(&entry.id).unwrap().unwrap();
        assert_eq!(fetched.handler_code, "run()");
    }

    #[test]
    fn upsert_by_name_resets_task() {
        let reg = setup();
        let v1 = reg.create("t1", "task", "d1", "code1", "{}", None).unwrap();
        reg.update_status(&v1.id, "completed", Some("done"), None)
            .unwrap();

        let v2 = reg.create("t1", "task", "d2", "code2", "{}", None).unwrap();
        assert_eq!(v2.id, v1.id);
        assert_eq!(v2.status, "pending");
        assert!(v2.result.is_none());
        assert_eq!(reg.count("t1").unwrap(), 1);
    }

    #[test]
    fn list_with_tenant_isolation() {
        let reg = setup();
        reg.create("t1", "a", "", "x", "{}", None).unwrap();
        reg.create("t1", "b", "", "y", "{}", None).unwrap();
        reg.create("t2", "c", "", "z", "{}", None).unwrap();

        assert_eq!(reg.list("t1").unwrap().len(), 2);
        assert_eq!(reg.list("t2").unwrap().len(), 1);
        assert_eq!(reg.list("t3").unwrap().len(), 0);
    }

    #[test]
    fn delete_removes_task() {
        let reg = setup();
        let entry = reg.create("t1", "gone", "", "x", "{}", None).unwrap();
        assert!(reg.delete(&entry.id).unwrap());
        assert_eq!(reg.count("t1").unwrap(), 0);
        assert!(reg.get(&entry.id).unwrap().is_none());
    }

    #[test]
    fn delete_nonexistent_returns_false() {
        let reg = setup();
        assert!(!reg.delete("nope").unwrap());
    }

    #[test]
    fn count_accuracy() {
        let reg = setup();
        assert_eq!(reg.count("t1").unwrap(), 0);
        reg.create("t1", "a", "", "x", "{}", None).unwrap();
        assert_eq!(reg.count("t1").unwrap(), 1);
    }

    #[test]
    fn status_transitions() {
        let reg = setup();
        let entry = reg
            .create("t1", "lifecycle", "d", "code", "{}", None)
            .unwrap();
        assert_eq!(entry.status, "pending");

        reg.update_status(&entry.id, "running", None, None).unwrap();
        let running = reg.get(&entry.id).unwrap().unwrap();
        assert_eq!(running.status, "running");
        assert!(running.started_at.is_some());
        assert!(running.completed_at.is_none());

        reg.update_status(&entry.id, "completed", Some("result_data"), None)
            .unwrap();
        let completed = reg.get(&entry.id).unwrap().unwrap();
        assert_eq!(completed.status, "completed");
        assert_eq!(completed.result.as_deref(), Some("result_data"));
        assert!(completed.completed_at.is_some());
    }

    #[test]
    fn status_transition_to_failed() {
        let reg = setup();
        let entry = reg
            .create("t1", "fail-task", "", "code", "{}", None)
            .unwrap();
        reg.update_status(&entry.id, "running", None, None).unwrap();
        reg.update_status(&entry.id, "failed", None, Some("timeout"))
            .unwrap();

        let failed = reg.get(&entry.id).unwrap().unwrap();
        assert_eq!(failed.status, "failed");
        assert_eq!(failed.error.as_deref(), Some("timeout"));
        assert!(failed.completed_at.is_some());
    }

    #[test]
    fn cancel_task() {
        let reg = setup();
        let entry = reg
            .create("t1", "cancel-me", "", "code", "{}", None)
            .unwrap();
        reg.update_status(&entry.id, "running", None, None).unwrap();
        reg.cancel(&entry.id).unwrap();

        let cancelled = reg.get(&entry.id).unwrap().unwrap();
        assert_eq!(cancelled.status, "cancelled");
        assert!(cancelled.completed_at.is_some());
    }

    #[test]
    fn get_by_name_works() {
        let reg = setup();
        reg.create("t1", "named-task", "d", "code", "{}", None)
            .unwrap();
        let found = reg.get_by_name("t1", "named-task").unwrap().unwrap();
        assert_eq!(found.name, "named-task");
        assert!(reg.get_by_name("t1", "nope").unwrap().is_none());
    }

    #[test]
    fn persistence_across_reload() {
        let db = AriaDb::open_in_memory().unwrap();
        {
            let reg = AriaTaskRegistry::new(db.clone());
            reg.create("t1", "persist-task", "d", "code", "{}", None)
                .unwrap();
        }
        let reg2 = AriaTaskRegistry::new(db);
        let entry = reg2.get_by_name("t1", "persist-task").unwrap().unwrap();
        assert_eq!(entry.name, "persist-task");
    }
}
