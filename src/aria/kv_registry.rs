//! KV registry — simple persistent key-value store with prefix queries.
//!
//! Unlike the memory registry (which is tiered/TTL-aware), the KV registry
//! is a flat key-value store with no expiration. Soft-deleted when removed.

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
pub struct AriaKvEntry {
    pub id: String,
    pub tenant_id: String,
    pub key: String,
    pub value: String,
    pub created_at: String,
    pub updated_at: String,
}

// ── Registry ─────────────────────────────────────────────────────

pub struct AriaKvRegistry {
    db: AriaDb,
    cache: Mutex<HashMap<String, AriaKvEntry>>,
    tenant_index: Mutex<HashMap<String, HashSet<String>>>,
    name_index: Mutex<HashMap<String, String>>,
    loaded: AtomicBool,
}

impl AriaKvRegistry {
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
                "SELECT id, tenant_id, key, value, created_at, updated_at FROM aria_kv",
            )?;
            let rows = stmt.query_map([], |row| {
                Ok(AriaKvEntry {
                    id: row.get(0)?,
                    tenant_id: row.get(1)?,
                    key: row.get(2)?,
                    value: row.get(3)?,
                    created_at: row.get(4)?,
                    updated_at: row.get(5)?,
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
            ni.insert(format!("{}:{}", e.tenant_id, e.key), e.id.clone());
            cache.insert(e.id.clone(), e);
        }
        self.loaded.store(true, Ordering::Release);
        Ok(())
    }

    fn index_entry(&self, entry: &AriaKvEntry) {
        self.tenant_index.lock().unwrap()
            .entry(entry.tenant_id.clone()).or_default().insert(entry.id.clone());
        self.name_index.lock().unwrap()
            .insert(format!("{}:{}", entry.tenant_id, entry.key), entry.id.clone());
    }

    fn deindex_entry(&self, entry: &AriaKvEntry) {
        if let Some(set) = self.tenant_index.lock().unwrap().get_mut(&entry.tenant_id) {
            set.remove(&entry.id);
        }
        self.name_index.lock().unwrap()
            .remove(&format!("{}:{}", entry.tenant_id, entry.key));
    }

    // ── Public API ───────────────────────────────────────────────

    /// Create or update a KV entry. Upserts on tenant+key.
    pub fn create(
        &self,
        tenant_id: &str,
        key: &str,
        value: &str,
    ) -> Result<AriaKvEntry> {
        self.ensure_loaded()?;
        let now = Utc::now().to_rfc3339();

        let existing_id = {
            let ni = self.name_index.lock().unwrap();
            ni.get(&format!("{tenant_id}:{key}")).cloned()
        };

        if let Some(eid) = existing_id {
            let mut cache = self.cache.lock().unwrap();
            let entry = cache.get(&eid).cloned().context("Cache inconsistency")?;
            self.db.with_conn(|conn| {
                conn.execute(
                    "UPDATE aria_kv SET value=?1, updated_at=?2 WHERE id=?3",
                    params![value, now, eid],
                )?;
                Ok(())
            })?;
            let updated = AriaKvEntry {
                value: value.to_string(),
                updated_at: now,
                ..entry
            };
            cache.insert(eid, updated.clone());
            Ok(updated)
        } else {
            let id = Uuid::new_v4().to_string();
            let entry = AriaKvEntry {
                id: id.clone(),
                tenant_id: tenant_id.to_string(),
                key: key.to_string(),
                value: value.to_string(),
                created_at: now.clone(),
                updated_at: now,
            };
            self.db.with_conn(|conn| {
                conn.execute(
                    "INSERT INTO aria_kv (id, tenant_id, key, value, created_at, updated_at)
                     VALUES (?1,?2,?3,?4,?5,?6)",
                    params![
                        entry.id, entry.tenant_id, entry.key, entry.value,
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

    pub fn get(&self, id: &str) -> Result<Option<AriaKvEntry>> {
        self.ensure_loaded()?;
        Ok(self.cache.lock().unwrap().get(id).cloned())
    }

    pub fn get_by_name(&self, tenant_id: &str, key: &str) -> Result<Option<AriaKvEntry>> {
        self.ensure_loaded()?;
        let id = self.name_index.lock().unwrap()
            .get(&format!("{tenant_id}:{key}")).cloned();
        match id {
            Some(id) => self.get(&id),
            None => Ok(None),
        }
    }

    pub fn list(&self, tenant_id: &str) -> Result<Vec<AriaKvEntry>> {
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
        Ok(self.tenant_index.lock().unwrap().get(tenant_id).map_or(0, |s| s.len()))
    }

    /// Delete a KV entry.
    pub fn delete(&self, id: &str) -> Result<bool> {
        self.ensure_loaded()?;
        let entry = self.cache.lock().unwrap().get(id).cloned();
        let Some(entry) = entry else { return Ok(false) };
        self.db.with_conn(|conn| {
            conn.execute("DELETE FROM aria_kv WHERE id=?1", params![id])?;
            Ok(())
        })?;
        self.deindex_entry(&entry);
        self.cache.lock().unwrap().remove(id);
        Ok(true)
    }

    /// Query entries whose keys start with the given prefix, scoped to a tenant.
    pub fn query(&self, tenant_id: &str, prefix: &str) -> Result<Vec<AriaKvEntry>> {
        self.ensure_loaded()?;
        let ti = self.tenant_index.lock().unwrap();
        let ids = match ti.get(tenant_id) {
            Some(set) => set.clone(),
            None => return Ok(Vec::new()),
        };
        drop(ti);
        let cache = self.cache.lock().unwrap();
        Ok(ids
            .iter()
            .filter_map(|id| cache.get(id))
            .filter(|e| e.key.starts_with(prefix))
            .cloned()
            .collect())
    }
}

// ── Tests ────────────────────────────────────────────────────────

#[cfg(test)]
mod tests {
    use super::*;
    use crate::aria::db::AriaDb;

    fn setup() -> AriaKvRegistry {
        let db = AriaDb::open_in_memory().unwrap();
        AriaKvRegistry::new(db)
    }

    #[test]
    fn create_and_get_roundtrip() {
        let reg = setup();
        let entry = reg.create("t1", "config.theme", "dark").unwrap();
        assert_eq!(entry.key, "config.theme");
        assert_eq!(entry.value, "dark");

        let fetched = reg.get(&entry.id).unwrap().unwrap();
        assert_eq!(fetched.value, "dark");
    }

    #[test]
    fn upsert_by_key_updates_value() {
        let reg = setup();
        let v1 = reg.create("t1", "counter", "1").unwrap();
        let v2 = reg.create("t1", "counter", "2").unwrap();
        assert_eq!(v2.id, v1.id);
        assert_eq!(v2.value, "2");
        assert_eq!(reg.count("t1").unwrap(), 1);
    }

    #[test]
    fn list_with_tenant_isolation() {
        let reg = setup();
        reg.create("t1", "a", "1").unwrap();
        reg.create("t1", "b", "2").unwrap();
        reg.create("t2", "c", "3").unwrap();

        assert_eq!(reg.list("t1").unwrap().len(), 2);
        assert_eq!(reg.list("t2").unwrap().len(), 1);
        assert_eq!(reg.list("t3").unwrap().len(), 0);
    }

    #[test]
    fn delete_removes_entry() {
        let reg = setup();
        let entry = reg.create("t1", "temp", "val").unwrap();
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
        reg.create("t1", "x", "1").unwrap();
        assert_eq!(reg.count("t1").unwrap(), 1);
        reg.create("t1", "y", "2").unwrap();
        assert_eq!(reg.count("t1").unwrap(), 2);
    }

    #[test]
    fn query_prefix_matching() {
        let reg = setup();
        reg.create("t1", "config.theme", "dark").unwrap();
        reg.create("t1", "config.lang", "en").unwrap();
        reg.create("t1", "data.items", "[]").unwrap();
        reg.create("t2", "config.theme", "light").unwrap();

        let config_entries = reg.query("t1", "config.").unwrap();
        assert_eq!(config_entries.len(), 2);

        let data_entries = reg.query("t1", "data.").unwrap();
        assert_eq!(data_entries.len(), 1);

        let none = reg.query("t1", "missing.").unwrap();
        assert!(none.is_empty());

        // Tenant isolation
        let t2_config = reg.query("t2", "config.").unwrap();
        assert_eq!(t2_config.len(), 1);
    }

    #[test]
    fn get_by_name_works() {
        let reg = setup();
        reg.create("t1", "my-key", "my-value").unwrap();
        let found = reg.get_by_name("t1", "my-key").unwrap().unwrap();
        assert_eq!(found.value, "my-value");
        assert!(reg.get_by_name("t1", "nope").unwrap().is_none());
        assert!(reg.get_by_name("t2", "my-key").unwrap().is_none());
    }

    #[test]
    fn persistence_across_reload() {
        let db = AriaDb::open_in_memory().unwrap();
        {
            let reg = AriaKvRegistry::new(db.clone());
            reg.create("t1", "persist-key", "persist-val").unwrap();
        }
        let reg2 = AriaKvRegistry::new(db);
        let entry = reg2.get_by_name("t1", "persist-key").unwrap().unwrap();
        assert_eq!(entry.value, "persist-val");
    }
}
