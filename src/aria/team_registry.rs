//! Team registry — SQLite-backed store for Aria multi-agent team definitions.
//!
//! Teams define collaboration patterns between agents (coordinator, round-robin,
//! parallel, sequential, etc.). Soft-deleted when removed.

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
pub struct AriaTeamEntry {
    pub id: String,
    pub tenant_id: String,
    pub name: String,
    pub description: String,
    pub mode: String,
    pub members: String,
    pub shared_context: Option<String>,
    pub timeout_seconds: Option<i64>,
    pub max_rounds: Option<i64>,
    pub status: String,
    pub created_at: String,
    pub updated_at: String,
}

// ── Registry ─────────────────────────────────────────────────────

pub struct AriaTeamRegistry {
    db: AriaDb,
    cache: Mutex<HashMap<String, AriaTeamEntry>>,
    tenant_index: Mutex<HashMap<String, HashSet<String>>>,
    name_index: Mutex<HashMap<String, String>>,
    loaded: AtomicBool,
}

impl AriaTeamRegistry {
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
                "SELECT id, tenant_id, name, description, mode, members,
                        shared_context, timeout_seconds, max_rounds, status,
                        created_at, updated_at
                 FROM aria_teams WHERE status != 'deleted'",
            )?;
            let rows = stmt.query_map([], |row| {
                Ok(AriaTeamEntry {
                    id: row.get(0)?,
                    tenant_id: row.get(1)?,
                    name: row.get(2)?,
                    description: row.get(3)?,
                    mode: row.get(4)?,
                    members: row.get(5)?,
                    shared_context: row.get(6)?,
                    timeout_seconds: row.get(7)?,
                    max_rounds: row.get(8)?,
                    status: row.get(9)?,
                    created_at: row.get(10)?,
                    updated_at: row.get(11)?,
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
            ti.entry(e.tenant_id.clone()).or_default().insert(e.id.clone());
            ni.insert(format!("{}:{}", e.tenant_id, e.name), e.id.clone());
            cache.insert(e.id.clone(), e);
        }
        self.loaded.store(true, Ordering::Release);
        Ok(())
    }

    fn index_entry(&self, entry: &AriaTeamEntry) {
        self.tenant_index.lock().unwrap()
            .entry(entry.tenant_id.clone()).or_default().insert(entry.id.clone());
        self.name_index.lock().unwrap()
            .insert(format!("{}:{}", entry.tenant_id, entry.name), entry.id.clone());
    }

    fn deindex_entry(&self, entry: &AriaTeamEntry) {
        if let Some(set) = self.tenant_index.lock().unwrap().get_mut(&entry.tenant_id) {
            set.remove(&entry.id);
        }
        self.name_index.lock().unwrap()
            .remove(&format!("{}:{}", entry.tenant_id, entry.name));
    }

    // ── Public API ───────────────────────────────────────────────

    pub fn create(
        &self,
        tenant_id: &str,
        name: &str,
        description: &str,
        mode: &str,
        members: &str,
        shared_context: Option<&str>,
        timeout_seconds: Option<i64>,
        max_rounds: Option<i64>,
    ) -> Result<AriaTeamEntry> {
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
                    "UPDATE aria_teams SET description=?1, mode=?2, members=?3,
                     shared_context=?4, timeout_seconds=?5, max_rounds=?6,
                     updated_at=?7 WHERE id=?8",
                    params![
                        description, mode, members, shared_context,
                        timeout_seconds, max_rounds, now, eid
                    ],
                )?;
                Ok(())
            })?;
            let updated = AriaTeamEntry {
                description: description.to_string(),
                mode: mode.to_string(),
                members: members.to_string(),
                shared_context: shared_context.map(String::from),
                timeout_seconds,
                max_rounds,
                updated_at: now,
                ..entry
            };
            cache.insert(eid, updated.clone());
            Ok(updated)
        } else {
            let id = Uuid::new_v4().to_string();
            let entry = AriaTeamEntry {
                id: id.clone(),
                tenant_id: tenant_id.to_string(),
                name: name.to_string(),
                description: description.to_string(),
                mode: mode.to_string(),
                members: members.to_string(),
                shared_context: shared_context.map(String::from),
                timeout_seconds,
                max_rounds,
                status: "active".to_string(),
                created_at: now.clone(),
                updated_at: now,
            };
            self.db.with_conn(|conn| {
                conn.execute(
                    "INSERT INTO aria_teams (id, tenant_id, name, description, mode,
                     members, shared_context, timeout_seconds, max_rounds, status,
                     created_at, updated_at)
                     VALUES (?1,?2,?3,?4,?5,?6,?7,?8,?9,?10,?11,?12)",
                    params![
                        entry.id, entry.tenant_id, entry.name, entry.description,
                        entry.mode, entry.members, entry.shared_context,
                        entry.timeout_seconds, entry.max_rounds, entry.status,
                        entry.created_at, entry.updated_at
                    ],
                )?;
                Ok(())
            })?;
            self.index_entry(&entry);
            self.cache.lock().unwrap().insert(id, entry.clone());
            Ok(entry)
        }
    }

    pub fn get(&self, id: &str) -> Result<Option<AriaTeamEntry>> {
        self.ensure_loaded()?;
        Ok(self.cache.lock().unwrap().get(id).cloned())
    }

    pub fn get_by_name(&self, tenant_id: &str, name: &str) -> Result<Option<AriaTeamEntry>> {
        self.ensure_loaded()?;
        let id = self.name_index.lock().unwrap()
            .get(&format!("{tenant_id}:{name}")).cloned();
        match id {
            Some(id) => self.get(&id),
            None => Ok(None),
        }
    }

    pub fn list(&self, tenant_id: &str) -> Result<Vec<AriaTeamEntry>> {
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

    /// List all teams across all tenants.
    pub fn list_all(&self) -> Result<Vec<AriaTeamEntry>> {
        self.ensure_loaded()?;
        let cache = self.cache.lock().unwrap();
        Ok(cache.values().cloned().collect())
    }

    pub fn count(&self, tenant_id: &str) -> Result<usize> {
        self.ensure_loaded()?;
        Ok(self.tenant_index.lock().unwrap().get(tenant_id).map_or(0, |s| s.len()))
    }

    /// Soft-delete a team.
    pub fn delete(&self, id: &str) -> Result<bool> {
        self.ensure_loaded()?;
        let entry = self.cache.lock().unwrap().get(id).cloned();
        let Some(entry) = entry else { return Ok(false) };
        let now = Utc::now().to_rfc3339();
        self.db.with_conn(|conn| {
            conn.execute(
                "UPDATE aria_teams SET status='deleted', updated_at=?1 WHERE id=?2",
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

    fn setup() -> AriaTeamRegistry {
        let db = AriaDb::open_in_memory().unwrap();
        AriaTeamRegistry::new(db)
    }

    fn create_default(reg: &AriaTeamRegistry, tenant: &str, name: &str) -> AriaTeamEntry {
        reg.create(
            tenant, name, "desc", "coordinator",
            r#"[{"agent_id":"a1","role":"leader"}]"#,
            None, Some(300), Some(10),
        ).unwrap()
    }

    #[test]
    fn create_and_get_roundtrip() {
        let reg = setup();
        let entry = create_default(&reg, "t1", "my-team");
        assert_eq!(entry.name, "my-team");
        assert_eq!(entry.mode, "coordinator");
        assert_eq!(entry.status, "active");

        let fetched = reg.get(&entry.id).unwrap().unwrap();
        assert_eq!(fetched.id, entry.id);
    }

    #[test]
    fn upsert_by_name_updates_existing() {
        let reg = setup();
        let v1 = create_default(&reg, "t1", "team");
        let v2 = reg.create(
            "t1", "team", "updated", "parallel", "[]", None, Some(600), None,
        ).unwrap();
        assert_eq!(v2.id, v1.id);
        assert_eq!(v2.mode, "parallel");
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
        create_default(&reg, "t1", "named-team");
        let found = reg.get_by_name("t1", "named-team").unwrap().unwrap();
        assert_eq!(found.name, "named-team");
        assert!(reg.get_by_name("t1", "nope").unwrap().is_none());
        assert!(reg.get_by_name("t2", "named-team").unwrap().is_none());
    }

    #[test]
    fn persistence_across_reload() {
        let db = AriaDb::open_in_memory().unwrap();
        {
            let reg = AriaTeamRegistry::new(db.clone());
            create_default(&reg, "t1", "persist-team");
        }
        let reg2 = AriaTeamRegistry::new(db);
        let entry = reg2.get_by_name("t1", "persist-team").unwrap().unwrap();
        assert_eq!(entry.name, "persist-team");
    }
}
