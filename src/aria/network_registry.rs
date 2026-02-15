//! Network registry — SQLite-backed store for Aria container network definitions.
//!
//! Networks define driver, isolation, IPv6, DNS config, and labels for
//! multi-container networking. Soft-deleted when removed.

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
pub struct AriaNetworkEntry {
    pub id: String,
    pub tenant_id: String,
    pub name: String,
    pub driver: String,
    pub isolation: String,
    pub ipv6: bool,
    pub dns_config: Option<String>,
    pub labels: String,
    pub options: String,
    pub created_at: String,
    pub updated_at: String,
}

pub struct NetworkCreateRequest<'a> {
    pub tenant_id: &'a str,
    pub name: &'a str,
    pub driver: &'a str,
    pub isolation: &'a str,
    pub ipv6: bool,
    pub dns_config: Option<&'a str>,
    pub labels: &'a str,
    pub options: &'a str,
}

// ── Registry ─────────────────────────────────────────────────────

pub struct AriaNetworkRegistry {
    db: AriaDb,
    cache: Mutex<HashMap<String, AriaNetworkEntry>>,
    tenant_index: Mutex<HashMap<String, HashSet<String>>>,
    name_index: Mutex<HashMap<String, String>>,
    loaded: AtomicBool,
}

impl AriaNetworkRegistry {
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
                "SELECT id, tenant_id, name, driver, isolation, ipv6,
                        dns_config, labels, options, created_at, updated_at
                 FROM aria_networks",
            )?;
            let rows = stmt.query_map([], |row| {
                Ok(AriaNetworkEntry {
                    id: row.get(0)?,
                    tenant_id: row.get(1)?,
                    name: row.get(2)?,
                    driver: row.get(3)?,
                    isolation: row.get(4)?,
                    ipv6: row.get(5)?,
                    dns_config: row.get(6)?,
                    labels: row.get(7)?,
                    options: row.get(8)?,
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

    fn index_entry(&self, entry: &AriaNetworkEntry) {
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

    fn deindex_entry(&self, entry: &AriaNetworkEntry) {
        if let Some(set) = self.tenant_index.lock().unwrap().get_mut(&entry.tenant_id) {
            set.remove(&entry.id);
        }
        self.name_index
            .lock()
            .unwrap()
            .remove(&format!("{}:{}", entry.tenant_id, entry.name));
    }

    // ── Public API ───────────────────────────────────────────────

    pub fn create(&self, req: NetworkCreateRequest<'_>) -> Result<AriaNetworkEntry> {
        let NetworkCreateRequest {
            tenant_id,
            name,
            driver,
            isolation,
            ipv6,
            dns_config,
            labels,
            options,
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
                    "UPDATE aria_networks SET driver=?1, isolation=?2, ipv6=?3,
                     dns_config=?4, labels=?5, options=?6, updated_at=?7 WHERE id=?8",
                    params![driver, isolation, ipv6, dns_config, labels, options, now, eid],
                )?;
                Ok(())
            })?;
            let updated = AriaNetworkEntry {
                driver: driver.to_string(),
                isolation: isolation.to_string(),
                ipv6,
                dns_config: dns_config.map(String::from),
                labels: labels.to_string(),
                options: options.to_string(),
                updated_at: now,
                ..entry
            };
            cache.insert(eid, updated.clone());
            Ok(updated)
        } else {
            let id = Uuid::new_v4().to_string();
            let entry = AriaNetworkEntry {
                id: id.clone(),
                tenant_id: tenant_id.to_string(),
                name: name.to_string(),
                driver: driver.to_string(),
                isolation: isolation.to_string(),
                ipv6,
                dns_config: dns_config.map(String::from),
                labels: labels.to_string(),
                options: options.to_string(),
                created_at: now.clone(),
                updated_at: now,
            };
            self.db.with_conn(|conn| {
                conn.execute(
                    "INSERT INTO aria_networks (id, tenant_id, name, driver, isolation,
                     ipv6, dns_config, labels, options, created_at, updated_at)
                     VALUES (?1,?2,?3,?4,?5,?6,?7,?8,?9,?10,?11)",
                    params![
                        entry.id,
                        entry.tenant_id,
                        entry.name,
                        entry.driver,
                        entry.isolation,
                        entry.ipv6,
                        entry.dns_config,
                        entry.labels,
                        entry.options,
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

    pub fn get(&self, id: &str) -> Result<Option<AriaNetworkEntry>> {
        self.ensure_loaded()?;
        Ok(self.cache.lock().unwrap().get(id).cloned())
    }

    pub fn get_by_name(&self, tenant_id: &str, name: &str) -> Result<Option<AriaNetworkEntry>> {
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

    pub fn list(&self, tenant_id: &str) -> Result<Vec<AriaNetworkEntry>> {
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

    /// Soft-delete a network.
    pub fn delete(&self, id: &str) -> Result<bool> {
        self.ensure_loaded()?;
        let entry = self.cache.lock().unwrap().get(id).cloned();
        let Some(entry) = entry else { return Ok(false) };
        self.db.with_conn(|conn| {
            conn.execute("DELETE FROM aria_networks WHERE id=?1", params![id])?;
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

    fn setup() -> AriaNetworkRegistry {
        let db = AriaDb::open_in_memory().unwrap();
        AriaNetworkRegistry::new(db)
    }

    fn create_net(
        reg: &AriaNetworkRegistry,
        tenant: &str,
        name: &str,
        driver: &str,
        isolation: &str,
        ipv6: bool,
        dns_config: Option<&str>,
        labels: &str,
        options: &str,
    ) -> AriaNetworkEntry {
        reg.create(NetworkCreateRequest {
            tenant_id: tenant,
            name,
            driver,
            isolation,
            ipv6,
            dns_config,
            labels,
            options,
        })
        .unwrap()
    }

    fn create_default(reg: &AriaNetworkRegistry, tenant: &str, name: &str) -> AriaNetworkEntry {
        create_net(
            reg, tenant, name, "bridge", "default", false, None, "{}", "{}",
        )
    }

    #[test]
    fn create_and_get_roundtrip() {
        let reg = setup();
        let entry = create_default(&reg, "t1", "my-network");
        assert_eq!(entry.name, "my-network");
        assert_eq!(entry.driver, "bridge");
        assert_eq!(entry.isolation, "default");
        assert!(!entry.ipv6);

        let fetched = reg.get(&entry.id).unwrap().unwrap();
        assert_eq!(fetched.id, entry.id);
    }

    #[test]
    fn upsert_by_name_updates_existing() {
        let reg = setup();
        let v1 = create_default(&reg, "t1", "net");
        let v2 = create_net(
            &reg,
            "t1",
            "net",
            "overlay",
            "isolated",
            true,
            Some(r#"{"nameservers":["8.8.8.8"]}"#),
            "{}",
            "{}",
        );
        assert_eq!(v2.id, v1.id);
        assert_eq!(v2.driver, "overlay");
        assert_eq!(v2.isolation, "isolated");
        assert!(v2.ipv6);
        assert!(v2.dns_config.is_some());
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
    fn delete_removes_network() {
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
        create_default(&reg, "t1", "y");
        assert_eq!(reg.count("t1").unwrap(), 2);
    }

    #[test]
    fn get_by_name_works() {
        let reg = setup();
        create_default(&reg, "t1", "named-net");
        let found = reg.get_by_name("t1", "named-net").unwrap().unwrap();
        assert_eq!(found.name, "named-net");
        assert!(reg.get_by_name("t1", "nope").unwrap().is_none());
        assert!(reg.get_by_name("t2", "named-net").unwrap().is_none());
    }

    #[test]
    fn ipv6_flag_persists() {
        let reg = setup();
        let entry = create_net(
            &reg, "t1", "v6-net", "bridge", "default", true, None, "{}", "{}",
        );
        assert!(entry.ipv6);
        let fetched = reg.get(&entry.id).unwrap().unwrap();
        assert!(fetched.ipv6);
    }

    #[test]
    fn persistence_across_reload() {
        let db = AriaDb::open_in_memory().unwrap();
        {
            let reg = AriaNetworkRegistry::new(db.clone());
            create_net(
                &reg,
                "t1",
                "persist-net",
                "host",
                "isolated",
                true,
                None,
                "{}",
                "{}",
            );
        }
        let reg2 = AriaNetworkRegistry::new(db);
        let entry = reg2.get_by_name("t1", "persist-net").unwrap().unwrap();
        assert_eq!(entry.name, "persist-net");
        assert_eq!(entry.driver, "host");
        assert!(entry.ipv6);
    }
}
