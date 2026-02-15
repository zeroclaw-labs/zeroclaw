//! Container registry — SQLite-backed store for Aria container lifecycle management.
//!
//! Tracks container state (pending/running/stopped/exited/error), runtime
//! properties (IP, PID), and network associations. Includes a `network_index`
//! for O(1) container-to-network lookups. Soft-deleted when removed.

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
pub struct AriaContainerEntry {
    pub id: String,
    pub tenant_id: String,
    pub name: String,
    pub image: String,
    pub config: String,
    pub state: String,
    pub container_ip: Option<String>,
    pub container_pid: Option<i64>,
    pub network_id: Option<String>,
    pub labels: String,
    pub created_at: String,
    pub updated_at: String,
}

// ── Registry ─────────────────────────────────────────────────────

pub struct AriaContainerRegistry {
    db: AriaDb,
    cache: Mutex<HashMap<String, AriaContainerEntry>>,
    tenant_index: Mutex<HashMap<String, HashSet<String>>>,
    name_index: Mutex<HashMap<String, String>>,
    /// Maps `network_id` -> set of container IDs for O(1) network lookups.
    network_index: Mutex<HashMap<String, HashSet<String>>>,
    loaded: AtomicBool,
}

impl AriaContainerRegistry {
    pub fn new(db: AriaDb) -> Self {
        Self {
            db,
            cache: Mutex::new(HashMap::new()),
            tenant_index: Mutex::new(HashMap::new()),
            name_index: Mutex::new(HashMap::new()),
            network_index: Mutex::new(HashMap::new()),
            loaded: AtomicBool::new(false),
        }
    }

    fn ensure_loaded(&self) -> Result<()> {
        if self.loaded.load(Ordering::Acquire) {
            return Ok(());
        }
        let entries = self.db.with_conn(|conn| {
            let mut stmt = conn.prepare(
                "SELECT id, tenant_id, name, image, config, state,
                        container_ip, container_pid, network_id, labels,
                        created_at, updated_at
                 FROM aria_containers",
            )?;
            let rows = stmt.query_map([], |row| {
                Ok(AriaContainerEntry {
                    id: row.get(0)?,
                    tenant_id: row.get(1)?,
                    name: row.get(2)?,
                    image: row.get(3)?,
                    config: row.get(4)?,
                    state: row.get(5)?,
                    container_ip: row.get(6)?,
                    container_pid: row.get(7)?,
                    network_id: row.get(8)?,
                    labels: row.get(9)?,
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
        let mut nwi = self.network_index.lock().unwrap();
        cache.clear();
        ti.clear();
        ni.clear();
        nwi.clear();

        for e in entries {
            ti.entry(e.tenant_id.clone())
                .or_default()
                .insert(e.id.clone());
            ni.insert(format!("{}:{}", e.tenant_id, e.name), e.id.clone());
            if let Some(ref nid) = e.network_id {
                nwi.entry(nid.clone()).or_default().insert(e.id.clone());
            }
            cache.insert(e.id.clone(), e);
        }
        self.loaded.store(true, Ordering::Release);
        Ok(())
    }

    fn index_entry(&self, entry: &AriaContainerEntry) {
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
        if let Some(ref nid) = entry.network_id {
            self.network_index
                .lock()
                .unwrap()
                .entry(nid.clone())
                .or_default()
                .insert(entry.id.clone());
        }
    }

    fn deindex_entry(&self, entry: &AriaContainerEntry) {
        if let Some(set) = self.tenant_index.lock().unwrap().get_mut(&entry.tenant_id) {
            set.remove(&entry.id);
        }
        self.name_index
            .lock()
            .unwrap()
            .remove(&format!("{}:{}", entry.tenant_id, entry.name));
        if let Some(ref nid) = entry.network_id {
            if let Some(set) = self.network_index.lock().unwrap().get_mut(nid) {
                set.remove(&entry.id);
            }
        }
    }

    // ── Public API ───────────────────────────────────────────────

    pub fn create(
        &self,
        tenant_id: &str,
        name: &str,
        image: &str,
        config: &str,
        network_id: Option<&str>,
        labels: &str,
    ) -> Result<AriaContainerEntry> {
        self.ensure_loaded()?;
        let now = Utc::now().to_rfc3339();

        let existing_id = {
            let ni = self.name_index.lock().unwrap();
            ni.get(&format!("{tenant_id}:{name}")).cloned()
        };

        if let Some(eid) = existing_id {
            // Remove old network index before updating
            let old_entry = {
                let cache = self.cache.lock().unwrap();
                cache.get(&eid).cloned().context("Cache inconsistency")?
            };
            if let Some(ref old_nid) = old_entry.network_id {
                if let Some(set) = self.network_index.lock().unwrap().get_mut(old_nid) {
                    set.remove(&eid);
                }
            }

            self.db.with_conn(|conn| {
                conn.execute(
                    "UPDATE aria_containers SET image=?1, config=?2, network_id=?3,
                     labels=?4, state='pending', container_ip=NULL, container_pid=NULL,
                     updated_at=?5 WHERE id=?6",
                    params![image, config, network_id, labels, now, eid],
                )?;
                Ok(())
            })?;
            let updated = AriaContainerEntry {
                image: image.to_string(),
                config: config.to_string(),
                state: "pending".to_string(),
                container_ip: None,
                container_pid: None,
                network_id: network_id.map(String::from),
                labels: labels.to_string(),
                updated_at: now,
                ..old_entry
            };
            // Re-index with new network_id
            if let Some(ref nid) = updated.network_id {
                self.network_index
                    .lock()
                    .unwrap()
                    .entry(nid.clone())
                    .or_default()
                    .insert(eid.clone());
            }
            self.cache.lock().unwrap().insert(eid, updated.clone());
            Ok(updated)
        } else {
            let id = Uuid::new_v4().to_string();
            let entry = AriaContainerEntry {
                id: id.clone(),
                tenant_id: tenant_id.to_string(),
                name: name.to_string(),
                image: image.to_string(),
                config: config.to_string(),
                state: "pending".to_string(),
                container_ip: None,
                container_pid: None,
                network_id: network_id.map(String::from),
                labels: labels.to_string(),
                created_at: now.clone(),
                updated_at: now,
            };
            self.db.with_conn(|conn| {
                conn.execute(
                    "INSERT INTO aria_containers (id, tenant_id, name, image, config, state,
                     container_ip, container_pid, network_id, labels, created_at, updated_at)
                     VALUES (?1,?2,?3,?4,?5,?6,?7,?8,?9,?10,?11,?12)",
                    params![
                        entry.id,
                        entry.tenant_id,
                        entry.name,
                        entry.image,
                        entry.config,
                        entry.state,
                        entry.container_ip,
                        entry.container_pid,
                        entry.network_id,
                        entry.labels,
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

    pub fn get(&self, id: &str) -> Result<Option<AriaContainerEntry>> {
        self.ensure_loaded()?;
        Ok(self.cache.lock().unwrap().get(id).cloned())
    }

    pub fn get_by_name(&self, tenant_id: &str, name: &str) -> Result<Option<AriaContainerEntry>> {
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

    pub fn list(&self, tenant_id: &str) -> Result<Vec<AriaContainerEntry>> {
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

    /// Soft-delete a container.
    pub fn delete(&self, id: &str) -> Result<bool> {
        self.ensure_loaded()?;
        let entry = self.cache.lock().unwrap().get(id).cloned();
        let Some(entry) = entry else { return Ok(false) };
        self.db.with_conn(|conn| {
            conn.execute("DELETE FROM aria_containers WHERE id=?1", params![id])?;
            Ok(())
        })?;
        self.deindex_entry(&entry);
        self.cache.lock().unwrap().remove(id);
        Ok(true)
    }

    /// Update runtime state (state, IP, PID) for a running container.
    pub fn update_runtime_state(
        &self,
        id: &str,
        state: &str,
        container_ip: Option<&str>,
        container_pid: Option<i64>,
    ) -> Result<bool> {
        self.ensure_loaded()?;
        let entry = self.cache.lock().unwrap().get(id).cloned();
        let Some(entry) = entry else { return Ok(false) };
        let now = Utc::now().to_rfc3339();
        self.db.with_conn(|conn| {
            conn.execute(
                "UPDATE aria_containers SET state=?1, container_ip=?2,
                 container_pid=?3, updated_at=?4 WHERE id=?5",
                params![state, container_ip, container_pid, now, id],
            )?;
            Ok(())
        })?;
        let updated = AriaContainerEntry {
            state: state.to_string(),
            container_ip: container_ip.map(String::from),
            container_pid,
            updated_at: now,
            ..entry
        };
        self.cache.lock().unwrap().insert(id.to_string(), updated);
        Ok(true)
    }

    /// List all containers associated with a given network.
    pub fn list_by_network(&self, network_id: &str) -> Result<Vec<AriaContainerEntry>> {
        self.ensure_loaded()?;
        let nwi = self.network_index.lock().unwrap();
        let ids = match nwi.get(network_id) {
            Some(set) => set.clone(),
            None => return Ok(Vec::new()),
        };
        drop(nwi);
        let cache = self.cache.lock().unwrap();
        Ok(ids.iter().filter_map(|id| cache.get(id).cloned()).collect())
    }
}

// ── Tests ────────────────────────────────────────────────────────

#[cfg(test)]
mod tests {
    use super::*;
    use crate::aria::db::AriaDb;

    fn setup() -> AriaContainerRegistry {
        let db = AriaDb::open_in_memory().unwrap();
        AriaContainerRegistry::new(db)
    }

    fn create_default(reg: &AriaContainerRegistry, tenant: &str, name: &str) -> AriaContainerEntry {
        reg.create(tenant, name, "node:20-slim", "{}", None, "{}")
            .unwrap()
    }

    #[test]
    fn create_and_get_roundtrip() {
        let reg = setup();
        let entry = create_default(&reg, "t1", "my-container");
        assert_eq!(entry.name, "my-container");
        assert_eq!(entry.image, "node:20-slim");
        assert_eq!(entry.state, "pending");

        let fetched = reg.get(&entry.id).unwrap().unwrap();
        assert_eq!(fetched.id, entry.id);
    }

    #[test]
    fn upsert_by_name_updates_existing() {
        let reg = setup();
        let v1 = create_default(&reg, "t1", "ctr");
        let v2 = reg
            .create("t1", "ctr", "python:3.12", "{}", None, "{}")
            .unwrap();
        assert_eq!(v2.id, v1.id);
        assert_eq!(v2.image, "python:3.12");
        assert_eq!(v2.state, "pending");
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
    fn delete_removes_container() {
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
    fn update_runtime_state() {
        let reg = setup();
        let entry = create_default(&reg, "t1", "runner");

        reg.update_runtime_state(&entry.id, "running", Some("172.17.0.2"), Some(12345))
            .unwrap();

        let running = reg.get(&entry.id).unwrap().unwrap();
        assert_eq!(running.state, "running");
        assert_eq!(running.container_ip.as_deref(), Some("172.17.0.2"));
        assert_eq!(running.container_pid, Some(12345));

        // Transition to stopped
        reg.update_runtime_state(&entry.id, "stopped", None, None)
            .unwrap();
        let stopped = reg.get(&entry.id).unwrap().unwrap();
        assert_eq!(stopped.state, "stopped");
        assert!(stopped.container_ip.is_none());
    }

    #[test]
    fn list_by_network() {
        let reg = setup();
        reg.create("t1", "ctr-a", "node:20", "{}", Some("net-1"), "{}")
            .unwrap();
        reg.create("t1", "ctr-b", "node:20", "{}", Some("net-1"), "{}")
            .unwrap();
        reg.create("t1", "ctr-c", "node:20", "{}", Some("net-2"), "{}")
            .unwrap();
        create_default(&reg, "t1", "ctr-d"); // no network

        let net1 = reg.list_by_network("net-1").unwrap();
        assert_eq!(net1.len(), 2);

        let net2 = reg.list_by_network("net-2").unwrap();
        assert_eq!(net2.len(), 1);

        let net3 = reg.list_by_network("net-3").unwrap();
        assert!(net3.is_empty());
    }

    #[test]
    fn network_index_updates_on_upsert() {
        let reg = setup();
        reg.create("t1", "ctr", "node:20", "{}", Some("net-old"), "{}")
            .unwrap();
        assert_eq!(reg.list_by_network("net-old").unwrap().len(), 1);

        // Upsert changes network
        reg.create("t1", "ctr", "node:20", "{}", Some("net-new"), "{}")
            .unwrap();
        assert_eq!(reg.list_by_network("net-old").unwrap().len(), 0);
        assert_eq!(reg.list_by_network("net-new").unwrap().len(), 1);
    }

    #[test]
    fn get_by_name_works() {
        let reg = setup();
        create_default(&reg, "t1", "named-ctr");
        let found = reg.get_by_name("t1", "named-ctr").unwrap().unwrap();
        assert_eq!(found.name, "named-ctr");
        assert!(reg.get_by_name("t1", "nope").unwrap().is_none());
        assert!(reg.get_by_name("t2", "named-ctr").unwrap().is_none());
    }

    #[test]
    fn persistence_across_reload() {
        let db = AriaDb::open_in_memory().unwrap();
        {
            let reg = AriaContainerRegistry::new(db.clone());
            reg.create("t1", "persist-ctr", "img", "{}", Some("net"), "{}")
                .unwrap();
        }
        let reg2 = AriaContainerRegistry::new(db);
        let entry = reg2.get_by_name("t1", "persist-ctr").unwrap().unwrap();
        assert_eq!(entry.name, "persist-ctr");
        assert_eq!(reg2.list_by_network("net").unwrap().len(), 1);
    }
}
