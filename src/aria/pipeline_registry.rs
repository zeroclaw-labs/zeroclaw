//! Pipeline registry — SQLite-backed store for Aria multi-step pipeline definitions.
//!
//! Pipelines define ordered steps (agent, tool, team, condition, transform)
//! with variable passing and parallel execution support. Soft-deleted when removed.

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
pub struct AriaPipelineEntry {
    pub id: String,
    pub tenant_id: String,
    pub name: String,
    pub description: String,
    pub steps: String,
    pub variables: String,
    pub timeout_seconds: Option<i64>,
    pub max_parallel: Option<i64>,
    pub status: String,
    pub created_at: String,
    pub updated_at: String,
}

pub struct PipelineCreateRequest<'a> {
    pub tenant_id: &'a str,
    pub name: &'a str,
    pub description: &'a str,
    pub steps: &'a str,
    pub variables: &'a str,
    pub timeout_seconds: Option<i64>,
    pub max_parallel: Option<i64>,
}

// ── Registry ─────────────────────────────────────────────────────

pub struct AriaPipelineRegistry {
    db: AriaDb,
    cache: Mutex<HashMap<String, AriaPipelineEntry>>,
    tenant_index: Mutex<HashMap<String, HashSet<String>>>,
    name_index: Mutex<HashMap<String, String>>,
    loaded: AtomicBool,
}

impl AriaPipelineRegistry {
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
                "SELECT id, tenant_id, name, description, steps, variables,
                        timeout_seconds, max_parallel, status, created_at, updated_at
                 FROM aria_pipelines WHERE status != 'deleted'",
            )?;
            let rows = stmt.query_map([], |row| {
                Ok(AriaPipelineEntry {
                    id: row.get(0)?,
                    tenant_id: row.get(1)?,
                    name: row.get(2)?,
                    description: row.get(3)?,
                    steps: row.get(4)?,
                    variables: row.get(5)?,
                    timeout_seconds: row.get(6)?,
                    max_parallel: row.get(7)?,
                    status: row.get(8)?,
                    created_at: row.get(9)?,
                    updated_at: row.get(10)?,
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

    fn index_entry(&self, entry: &AriaPipelineEntry) {
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

    fn deindex_entry(&self, entry: &AriaPipelineEntry) {
        if let Some(set) = self.tenant_index.lock().unwrap().get_mut(&entry.tenant_id) {
            set.remove(&entry.id);
        }
        self.name_index
            .lock()
            .unwrap()
            .remove(&format!("{}:{}", entry.tenant_id, entry.name));
    }

    // ── Public API ───────────────────────────────────────────────

    pub fn create(&self, req: PipelineCreateRequest<'_>) -> Result<AriaPipelineEntry> {
        let PipelineCreateRequest {
            tenant_id,
            name,
            description,
            steps,
            variables,
            timeout_seconds,
            max_parallel,
        } = req;
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
                    "UPDATE aria_pipelines SET description=?1, steps=?2, variables=?3,
                     timeout_seconds=?4, max_parallel=?5, updated_at=?6 WHERE id=?7",
                    params![
                        description,
                        steps,
                        variables,
                        timeout_seconds,
                        max_parallel,
                        now,
                        eid
                    ],
                )?;
                Ok(())
            })?;
            let updated = AriaPipelineEntry {
                description: description.to_string(),
                steps: steps.to_string(),
                variables: variables.to_string(),
                timeout_seconds,
                max_parallel,
                updated_at: now,
                ..entry
            };
            cache.insert(eid, updated.clone());
            Ok(updated)
        } else {
            let id = Uuid::new_v4().to_string();
            let entry = AriaPipelineEntry {
                id: id.clone(),
                tenant_id: tenant_id.to_string(),
                name: name.to_string(),
                description: description.to_string(),
                steps: steps.to_string(),
                variables: variables.to_string(),
                timeout_seconds,
                max_parallel,
                status: "active".to_string(),
                created_at: now.clone(),
                updated_at: now,
            };
            self.db.with_conn(|conn| {
                conn.execute(
                    "INSERT INTO aria_pipelines (id, tenant_id, name, description, steps,
                     variables, timeout_seconds, max_parallel, status, created_at, updated_at)
                     VALUES (?1,?2,?3,?4,?5,?6,?7,?8,?9,?10,?11)",
                    params![
                        entry.id,
                        entry.tenant_id,
                        entry.name,
                        entry.description,
                        entry.steps,
                        entry.variables,
                        entry.timeout_seconds,
                        entry.max_parallel,
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

    pub fn get(&self, id: &str) -> Result<Option<AriaPipelineEntry>> {
        self.ensure_loaded()?;
        Ok(self.cache.lock().unwrap().get(id).cloned())
    }

    pub fn get_by_name(&self, tenant_id: &str, name: &str) -> Result<Option<AriaPipelineEntry>> {
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

    pub fn list(&self, tenant_id: &str) -> Result<Vec<AriaPipelineEntry>> {
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

    /// List all pipelines across all tenants.
    pub fn list_all(&self) -> Result<Vec<AriaPipelineEntry>> {
        self.ensure_loaded()?;
        let cache = self.cache.lock().unwrap();
        Ok(cache.values().cloned().collect())
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

    /// Soft-delete a pipeline.
    pub fn delete(&self, id: &str) -> Result<bool> {
        self.ensure_loaded()?;
        let entry = self.cache.lock().unwrap().get(id).cloned();
        let Some(entry) = entry else { return Ok(false) };
        let now = Utc::now().to_rfc3339();
        self.db.with_conn(|conn| {
            conn.execute(
                "UPDATE aria_pipelines SET status='deleted', updated_at=?1 WHERE id=?2",
                params![now, id],
            )?;
            Ok(())
        })?;
        self.deindex_entry(&entry);
        self.cache.lock().unwrap().remove(id);
        Ok(true)
    }
}

// ── Tests ────────────────────────────────────────────────────────

#[cfg(test)]
mod tests {
    use super::*;
    use crate::aria::db::AriaDb;

    fn setup() -> AriaPipelineRegistry {
        let db = AriaDb::open_in_memory().unwrap();
        AriaPipelineRegistry::new(db)
    }

    fn create_pipe(
        reg: &AriaPipelineRegistry,
        tenant: &str,
        name: &str,
        description: &str,
        steps: &str,
        variables: &str,
        timeout_seconds: Option<i64>,
        max_parallel: Option<i64>,
    ) -> AriaPipelineEntry {
        reg.create(PipelineCreateRequest {
            tenant_id: tenant,
            name,
            description,
            steps,
            variables,
            timeout_seconds,
            max_parallel,
        })
        .unwrap()
    }

    fn create_default(reg: &AriaPipelineRegistry, tenant: &str, name: &str) -> AriaPipelineEntry {
        create_pipe(
            reg,
            tenant,
            name,
            "desc",
            r#"[{"id":"s1","name":"step1","step_type":"agent"}]"#,
            r#"{"input":"hello"}"#,
            Some(600),
            Some(4),
        )
    }

    #[test]
    fn create_and_get_roundtrip() {
        let reg = setup();
        let entry = create_default(&reg, "t1", "my-pipeline");
        assert_eq!(entry.name, "my-pipeline");
        assert_eq!(entry.status, "active");
        assert_eq!(entry.max_parallel, Some(4));

        let fetched = reg.get(&entry.id).unwrap().unwrap();
        assert_eq!(fetched.id, entry.id);
    }

    #[test]
    fn upsert_by_name_updates_existing() {
        let reg = setup();
        let v1 = create_default(&reg, "t1", "pipe");
        let v2 = create_pipe(
            &reg,
            "t1",
            "pipe",
            "updated",
            "[]",
            "{}",
            Some(300),
            Some(2),
        );
        assert_eq!(v2.id, v1.id);
        assert_eq!(v2.description, "updated");
        assert_eq!(v2.max_parallel, Some(2));
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
    fn list_all_crosses_tenants() {
        let reg = setup();
        create_default(&reg, "t1", "a");
        create_default(&reg, "t2", "b");
        create_default(&reg, "t3", "c");
        assert_eq!(reg.list_all().unwrap().len(), 3);
    }

    #[test]
    fn soft_delete() {
        let reg = setup();
        let entry = create_default(&reg, "t1", "doomed");
        assert!(reg.delete(&entry.id).unwrap());
        assert_eq!(reg.count("t1").unwrap(), 0);
        assert!(reg.get(&entry.id).unwrap().is_none());
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
    fn get_by_name_works() {
        let reg = setup();
        create_default(&reg, "t1", "named-pipe");
        let found = reg.get_by_name("t1", "named-pipe").unwrap().unwrap();
        assert_eq!(found.name, "named-pipe");
        assert!(reg.get_by_name("t1", "nope").unwrap().is_none());
        assert!(reg.get_by_name("t2", "named-pipe").unwrap().is_none());
    }

    #[test]
    fn persistence_across_reload() {
        let db = AriaDb::open_in_memory().unwrap();
        {
            let reg = AriaPipelineRegistry::new(db.clone());
            create_default(&reg, "t1", "persist-pipe");
        }
        let reg2 = AriaPipelineRegistry::new(db);
        let entry = reg2.get_by_name("t1", "persist-pipe").unwrap().unwrap();
        assert_eq!(entry.name, "persist-pipe");
    }
}
