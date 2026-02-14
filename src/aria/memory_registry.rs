//! Memory registry — tiered key-value store with TTL and session scoping.
//!
//! Three tiers: scratchpad (session-scoped), ephemeral (TTL-based),
//! longterm (persistent). Soft-deleted when removed.

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
pub struct AriaMemoryEntry {
    pub id: String,
    pub tenant_id: String,
    pub key: String,
    pub value: String,
    pub tier: String,
    pub namespace: Option<String>,
    pub session_id: Option<String>,
    pub ttl_seconds: Option<i64>,
    pub expires_at: Option<String>,
    pub created_at: String,
    pub updated_at: String,
}

// ── Registry ─────────────────────────────────────────────────────

pub struct AriaMemoryRegistry {
    db: AriaDb,
    cache: Mutex<HashMap<String, AriaMemoryEntry>>,
    tenant_index: Mutex<HashMap<String, HashSet<String>>>,
    name_index: Mutex<HashMap<String, String>>,
    loaded: AtomicBool,
}

impl AriaMemoryRegistry {
    pub fn new(db: AriaDb) -> Self {
        Self {
            db,
            cache: Mutex::new(HashMap::new()),
            tenant_index: Mutex::new(HashMap::new()),
            name_index: Mutex::new(HashMap::new()),
            loaded: AtomicBool::new(false),
        }
    }

    /// Name index key uses tenant:key:tier for uniqueness.
    fn nkey(tenant_id: &str, key: &str, tier: &str) -> String {
        format!("{tenant_id}:{key}:{tier}")
    }

    fn ensure_loaded(&self) -> Result<()> {
        if self.loaded.load(Ordering::Acquire) {
            return Ok(());
        }
        let entries = self.db.with_conn(|conn| {
            let mut stmt = conn.prepare(
                "SELECT id, tenant_id, key, value, tier, namespace, session_id,
                        ttl_seconds, expires_at, created_at, updated_at
                 FROM aria_memory",
            )?;
            let rows = stmt.query_map([], |row| {
                Ok(AriaMemoryEntry {
                    id: row.get(0)?,
                    tenant_id: row.get(1)?,
                    key: row.get(2)?,
                    value: row.get(3)?,
                    tier: row.get(4)?,
                    namespace: row.get(5)?,
                    session_id: row.get(6)?,
                    ttl_seconds: row.get(7)?,
                    expires_at: row.get(8)?,
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
            ti.entry(e.tenant_id.clone()).or_default().insert(e.id.clone());
            ni.insert(Self::nkey(&e.tenant_id, &e.key, &e.tier), e.id.clone());
            cache.insert(e.id.clone(), e);
        }
        self.loaded.store(true, Ordering::Release);
        Ok(())
    }

    fn index_entry(&self, entry: &AriaMemoryEntry) {
        self.tenant_index.lock().unwrap()
            .entry(entry.tenant_id.clone()).or_default().insert(entry.id.clone());
        self.name_index.lock().unwrap()
            .insert(Self::nkey(&entry.tenant_id, &entry.key, &entry.tier), entry.id.clone());
    }

    fn deindex_entry(&self, entry: &AriaMemoryEntry) {
        if let Some(set) = self.tenant_index.lock().unwrap().get_mut(&entry.tenant_id) {
            set.remove(&entry.id);
        }
        self.name_index.lock().unwrap()
            .remove(&Self::nkey(&entry.tenant_id, &entry.key, &entry.tier));
    }

    // ── Public API ───────────────────────────────────────────────

    /// Create or update a memory entry. Upserts on tenant+key+tier.
    pub fn create(
        &self,
        tenant_id: &str,
        key: &str,
        value: &str,
        tier: &str,
        namespace: Option<&str>,
        session_id: Option<&str>,
        ttl_seconds: Option<i64>,
    ) -> Result<AriaMemoryEntry> {
        self.ensure_loaded()?;
        let now = Utc::now().to_rfc3339();
        let expires_at = ttl_seconds.map(|ttl| {
            (Utc::now() + chrono::Duration::seconds(ttl)).to_rfc3339()
        });

        let existing_id = {
            let ni = self.name_index.lock().unwrap();
            ni.get(&Self::nkey(tenant_id, key, tier)).cloned()
        };

        if let Some(eid) = existing_id {
            let mut cache = self.cache.lock().unwrap();
            let entry = cache.get(&eid).cloned().context("Cache inconsistency")?;
            self.db.with_conn(|conn| {
                conn.execute(
                    "UPDATE aria_memory SET value=?1, namespace=?2, session_id=?3,
                     ttl_seconds=?4, expires_at=?5, updated_at=?6 WHERE id=?7",
                    params![value, namespace, session_id, ttl_seconds, expires_at, now, eid],
                )?;
                Ok(())
            })?;
            let updated = AriaMemoryEntry {
                value: value.to_string(),
                namespace: namespace.map(String::from),
                session_id: session_id.map(String::from),
                ttl_seconds,
                expires_at,
                updated_at: now,
                ..entry
            };
            cache.insert(eid, updated.clone());
            Ok(updated)
        } else {
            let id = Uuid::new_v4().to_string();
            let entry = AriaMemoryEntry {
                id: id.clone(),
                tenant_id: tenant_id.to_string(),
                key: key.to_string(),
                value: value.to_string(),
                tier: tier.to_string(),
                namespace: namespace.map(String::from),
                session_id: session_id.map(String::from),
                ttl_seconds,
                expires_at,
                created_at: now.clone(),
                updated_at: now,
            };
            self.db.with_conn(|conn| {
                conn.execute(
                    "INSERT INTO aria_memory (id, tenant_id, key, value, tier, namespace,
                     session_id, ttl_seconds, expires_at, created_at, updated_at)
                     VALUES (?1,?2,?3,?4,?5,?6,?7,?8,?9,?10,?11)",
                    params![
                        entry.id, entry.tenant_id, entry.key, entry.value, entry.tier,
                        entry.namespace, entry.session_id, entry.ttl_seconds,
                        entry.expires_at, entry.created_at, entry.updated_at
                    ],
                )?;
                Ok(())
            })?;
            self.index_entry(&entry);
            self.cache.lock().unwrap().insert(id, entry.clone());
            Ok(entry)
        }
    }

    pub fn get(&self, id: &str) -> Result<Option<AriaMemoryEntry>> {
        self.ensure_loaded()?;
        Ok(self.cache.lock().unwrap().get(id).cloned())
    }

    pub fn get_by_name(&self, tenant_id: &str, key: &str, tier: &str) -> Result<Option<AriaMemoryEntry>> {
        self.ensure_loaded()?;
        let id = self.name_index.lock().unwrap()
            .get(&Self::nkey(tenant_id, key, tier)).cloned();
        match id {
            Some(id) => self.get(&id),
            None => Ok(None),
        }
    }

    pub fn list(&self, tenant_id: &str) -> Result<Vec<AriaMemoryEntry>> {
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

    /// Soft-delete (remove) a memory entry.
    pub fn delete(&self, id: &str) -> Result<bool> {
        self.ensure_loaded()?;
        let entry = self.cache.lock().unwrap().get(id).cloned();
        let Some(entry) = entry else { return Ok(false) };
        self.db.with_conn(|conn| {
            conn.execute("DELETE FROM aria_memory WHERE id=?1", params![id])?;
            Ok(())
        })?;
        self.deindex_entry(&entry);
        self.cache.lock().unwrap().remove(id);
        Ok(true)
    }

    /// Sweep expired ephemeral entries (where expires_at < now).
    pub fn sweep_expired(&self) -> Result<usize> {
        self.ensure_loaded()?;
        let now = Utc::now().to_rfc3339();
        let expired_ids: Vec<String> = {
            let cache = self.cache.lock().unwrap();
            cache.values()
                .filter(|e| {
                    if let Some(ref exp) = e.expires_at {
                        exp.as_str() < now.as_str()
                    } else {
                        false
                    }
                })
                .map(|e| e.id.clone())
                .collect()
        };
        let count = expired_ids.len();
        for id in &expired_ids {
            self.delete(id)?;
        }
        Ok(count)
    }

    /// Clear all scratchpad entries for a given session.
    pub fn clear_session(&self, session_id: &str) -> Result<usize> {
        self.ensure_loaded()?;
        let session_ids: Vec<String> = {
            let cache = self.cache.lock().unwrap();
            cache.values()
                .filter(|e| {
                    e.tier == "scratchpad"
                        && e.session_id.as_deref() == Some(session_id)
                })
                .map(|e| e.id.clone())
                .collect()
        };
        let count = session_ids.len();
        for id in &session_ids {
            self.delete(id)?;
        }
        Ok(count)
    }
}

// ── Tests ────────────────────────────────────────────────────────

#[cfg(test)]
mod tests {
    use super::*;
    use crate::aria::db::AriaDb;

    fn setup() -> AriaMemoryRegistry {
        let db = AriaDb::open_in_memory().unwrap();
        AriaMemoryRegistry::new(db)
    }

    #[test]
    fn create_and_get_roundtrip() {
        let reg = setup();
        let entry = reg
            .create("t1", "user_pref", "dark_mode", "longterm", None, None, None)
            .unwrap();
        assert_eq!(entry.key, "user_pref");
        assert_eq!(entry.tier, "longterm");

        let fetched = reg.get(&entry.id).unwrap().unwrap();
        assert_eq!(fetched.value, "dark_mode");
    }

    #[test]
    fn upsert_by_key_and_tier() {
        let reg = setup();
        let v1 = reg
            .create("t1", "counter", "1", "longterm", None, None, None)
            .unwrap();
        let v2 = reg
            .create("t1", "counter", "2", "longterm", None, None, None)
            .unwrap();
        assert_eq!(v2.id, v1.id);
        assert_eq!(v2.value, "2");
        assert_eq!(reg.count("t1").unwrap(), 1);
    }

    #[test]
    fn different_tiers_are_separate() {
        let reg = setup();
        reg.create("t1", "key", "scratch_val", "scratchpad", None, Some("s1"), None)
            .unwrap();
        reg.create("t1", "key", "long_val", "longterm", None, None, None)
            .unwrap();
        assert_eq!(reg.count("t1").unwrap(), 2);

        let scratch = reg.get_by_name("t1", "key", "scratchpad").unwrap().unwrap();
        assert_eq!(scratch.value, "scratch_val");
        let long = reg.get_by_name("t1", "key", "longterm").unwrap().unwrap();
        assert_eq!(long.value, "long_val");
    }

    #[test]
    fn list_with_tenant_isolation() {
        let reg = setup();
        reg.create("t1", "a", "1", "longterm", None, None, None).unwrap();
        reg.create("t1", "b", "2", "longterm", None, None, None).unwrap();
        reg.create("t2", "c", "3", "longterm", None, None, None).unwrap();

        assert_eq!(reg.list("t1").unwrap().len(), 2);
        assert_eq!(reg.list("t2").unwrap().len(), 1);
        assert_eq!(reg.list("t3").unwrap().len(), 0);
    }

    #[test]
    fn delete_removes_entry() {
        let reg = setup();
        let entry = reg
            .create("t1", "temp", "val", "longterm", None, None, None)
            .unwrap();
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
        reg.create("t1", "a", "1", "longterm", None, None, None).unwrap();
        assert_eq!(reg.count("t1").unwrap(), 1);
    }

    #[test]
    fn sweep_expired_removes_old_entries() {
        let reg = setup();
        // Create an entry with 0 TTL (already expired)
        let entry = reg
            .create("t1", "ephemeral_key", "val", "ephemeral", None, None, Some(0))
            .unwrap();
        // Manually set expires_at in the past
        let past = "2000-01-01T00:00:00+00:00";
        reg.db.with_conn(|conn| {
            conn.execute(
                "UPDATE aria_memory SET expires_at=?1 WHERE id=?2",
                params![past, entry.id],
            )?;
            Ok(())
        }).unwrap();
        // Update cache
        {
            let mut cache = reg.cache.lock().unwrap();
            if let Some(e) = cache.get_mut(&entry.id) {
                e.expires_at = Some(past.to_string());
            }
        }

        let swept = reg.sweep_expired().unwrap();
        assert_eq!(swept, 1);
        assert_eq!(reg.count("t1").unwrap(), 0);
    }

    #[test]
    fn clear_session_removes_scratchpad_entries() {
        let reg = setup();
        reg.create("t1", "scratch1", "v1", "scratchpad", None, Some("sess-abc"), None)
            .unwrap();
        reg.create("t1", "scratch2", "v2", "scratchpad", None, Some("sess-abc"), None)
            .unwrap();
        reg.create("t1", "scratch3", "v3", "scratchpad", None, Some("sess-other"), None)
            .unwrap();
        reg.create("t1", "persist", "v4", "longterm", None, None, None)
            .unwrap();

        let cleared = reg.clear_session("sess-abc").unwrap();
        assert_eq!(cleared, 2);
        assert_eq!(reg.count("t1").unwrap(), 2); // scratch3 + persist remain
    }

    #[test]
    fn clear_session_does_not_touch_other_tiers() {
        let reg = setup();
        reg.create("t1", "k", "v", "longterm", None, Some("sess-abc"), None)
            .unwrap();
        let cleared = reg.clear_session("sess-abc").unwrap();
        assert_eq!(cleared, 0);
        assert_eq!(reg.count("t1").unwrap(), 1);
    }

    #[test]
    fn persistence_across_reload() {
        let db = AriaDb::open_in_memory().unwrap();
        {
            let reg = AriaMemoryRegistry::new(db.clone());
            reg.create("t1", "keep", "value", "longterm", None, None, None).unwrap();
        }
        let reg2 = AriaMemoryRegistry::new(db);
        let entry = reg2.get_by_name("t1", "keep", "longterm").unwrap().unwrap();
        assert_eq!(entry.value, "value");
    }
}
