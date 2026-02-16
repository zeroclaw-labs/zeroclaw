//! Feed registry — SQLite-backed store for Aria data feed definitions.
//!
//! Feeds produce card-based content on a schedule. Supports active/paused/deleted
//! status, cross-tenant listing, and refresh intervals.

use super::db::AriaDb;
use anyhow::{Context, Result};
use chrono::Utc;
use rusqlite::params;
use serde::{Deserialize, Serialize};
use std::collections::{HashMap, HashSet};
use std::sync::atomic::{AtomicBool, Ordering};
use std::sync::Mutex;
use uuid::Uuid;

pub const DEFAULT_FEED_SCHEDULE: &str = "0 8 * * *";

pub fn normalize_feed_schedule(schedule: &str) -> String {
    let trimmed = schedule.trim();
    if trimmed.is_empty() {
        DEFAULT_FEED_SCHEDULE.to_string()
    } else {
        trimmed.to_string()
    }
}

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
pub struct AriaFeedEntry {
    pub id: String,
    pub tenant_id: String,
    pub name: String,
    pub description: String,
    pub handler_code: String,
    pub handler_hash: String,
    pub schedule: String,
    pub refresh_seconds: Option<i64>,
    pub category: Option<String>,
    pub retention: Option<String>,
    pub display: Option<String>,
    pub sandbox_config: Option<String>,
    pub status: String,
    pub created_at: String,
    pub updated_at: String,
}

pub struct FeedUploadRequest<'a> {
    pub tenant_id: &'a str,
    pub name: &'a str,
    pub description: &'a str,
    pub handler_code: &'a str,
    pub schedule: &'a str,
    pub refresh_seconds: Option<i64>,
    pub category: Option<&'a str>,
    pub retention: Option<&'a str>,
    pub display: Option<&'a str>,
    pub sandbox_config: Option<&'a str>,
}

// ── Registry ─────────────────────────────────────────────────────

pub struct AriaFeedRegistry {
    db: AriaDb,
    cache: Mutex<HashMap<String, AriaFeedEntry>>,
    tenant_index: Mutex<HashMap<String, HashSet<String>>>,
    name_index: Mutex<HashMap<String, String>>,
    loaded: AtomicBool,
}

impl AriaFeedRegistry {
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
                        schedule, refresh_seconds, category, retention, display,
                        sandbox_config, status, created_at, updated_at
                 FROM aria_feeds WHERE status != 'deleted'",
            )?;
            let rows = stmt.query_map([], |row| {
                Ok(AriaFeedEntry {
                    id: row.get(0)?,
                    tenant_id: row.get(1)?,
                    name: row.get(2)?,
                    description: row.get(3)?,
                    handler_code: row.get(4)?,
                    handler_hash: row.get(5)?,
                    schedule: row.get(6)?,
                    refresh_seconds: row.get(7)?,
                    category: row.get(8)?,
                    retention: row.get(9)?,
                    display: row.get(10)?,
                    sandbox_config: row.get(11)?,
                    status: row.get(12)?,
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

    fn index_entry(&self, entry: &AriaFeedEntry) {
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

    fn deindex_entry(&self, entry: &AriaFeedEntry) {
        if let Some(set) = self.tenant_index.lock().unwrap().get_mut(&entry.tenant_id) {
            set.remove(&entry.id);
        }
        self.name_index
            .lock()
            .unwrap()
            .remove(&format!("{}:{}", entry.tenant_id, entry.name));
    }

    // ── Public API ───────────────────────────────────────────────

    pub fn upload(&self, req: FeedUploadRequest<'_>) -> Result<AriaFeedEntry> {
        let FeedUploadRequest {
            tenant_id,
            name,
            description,
            handler_code,
            schedule,
            refresh_seconds,
            category,
            retention,
            display,
            sandbox_config,
        } = req;
        self.ensure_loaded()?;
        let now = Utc::now().to_rfc3339();
        let hash = sha256_hex(handler_code);
        let normalized_schedule = normalize_feed_schedule(schedule);

        let existing_id = {
            let ni = self.name_index.lock().unwrap();
            ni.get(&format!("{tenant_id}:{name}")).cloned()
        };

        if let Some(eid) = existing_id {
            let mut cache = self.cache.lock().unwrap();
            let entry = cache.get(&eid).cloned().context("Cache inconsistency")?;
            self.db.with_conn(|conn| {
                conn.execute(
                    "UPDATE aria_feeds SET description=?1, handler_code=?2, handler_hash=?3,
                     schedule=?4, refresh_seconds=?5, category=?6, retention=?7,
                     display=?8, sandbox_config=?9, updated_at=?10 WHERE id=?11",
                    params![
                        description,
                        handler_code,
                        hash,
                        normalized_schedule,
                        refresh_seconds,
                        category,
                        retention,
                        display,
                        sandbox_config,
                        now,
                        eid
                    ],
                )?;
                Ok(())
            })?;
            let updated = AriaFeedEntry {
                description: description.to_string(),
                handler_code: handler_code.to_string(),
                handler_hash: hash,
                schedule: normalized_schedule.clone(),
                refresh_seconds,
                category: category.map(String::from),
                retention: retention.map(String::from),
                display: display.map(String::from),
                sandbox_config: sandbox_config.map(String::from),
                updated_at: now,
                ..entry
            };
            cache.insert(eid, updated.clone());
            Ok(updated)
        } else {
            let id = Uuid::new_v4().to_string();
            let entry = AriaFeedEntry {
                id: id.clone(),
                tenant_id: tenant_id.to_string(),
                name: name.to_string(),
                description: description.to_string(),
                handler_code: handler_code.to_string(),
                handler_hash: hash,
                schedule: normalized_schedule.clone(),
                refresh_seconds,
                category: category.map(String::from),
                retention: retention.map(String::from),
                display: display.map(String::from),
                sandbox_config: sandbox_config.map(String::from),
                status: "active".to_string(),
                created_at: now.clone(),
                updated_at: now,
            };
            self.db.with_conn(|conn| {
                conn.execute(
                    "INSERT INTO aria_feeds (id, tenant_id, name, description, handler_code,
                     handler_hash, schedule, refresh_seconds, category, retention, display,
                     sandbox_config, status, created_at, updated_at)
                     VALUES (?1,?2,?3,?4,?5,?6,?7,?8,?9,?10,?11,?12,?13,?14,?15)",
                    params![
                        entry.id,
                        entry.tenant_id,
                        entry.name,
                        entry.description,
                        entry.handler_code,
                        entry.handler_hash,
                        entry.schedule,
                        entry.refresh_seconds,
                        entry.category,
                        entry.retention,
                        entry.display,
                        entry.sandbox_config,
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

    pub fn get(&self, id: &str) -> Result<Option<AriaFeedEntry>> {
        self.ensure_loaded()?;
        Ok(self.cache.lock().unwrap().get(id).cloned())
    }

    pub fn get_by_name(&self, tenant_id: &str, name: &str) -> Result<Option<AriaFeedEntry>> {
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

    pub fn list(&self, tenant_id: &str) -> Result<Vec<AriaFeedEntry>> {
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

    /// List all feeds across all tenants.
    pub fn list_all(&self) -> Result<Vec<AriaFeedEntry>> {
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

    /// Soft-delete a feed.
    pub fn delete(&self, id: &str) -> Result<bool> {
        self.ensure_loaded()?;
        let entry = self.cache.lock().unwrap().get(id).cloned();
        let Some(entry) = entry else { return Ok(false) };
        let now = Utc::now().to_rfc3339();
        self.db.with_conn(|conn| {
            conn.execute(
                "UPDATE aria_feeds SET status='deleted', updated_at=?1 WHERE id=?2",
                params![now, id],
            )?;
            Ok(())
        })?;
        self.deindex_entry(&entry);
        self.cache.lock().unwrap().remove(id);
        Ok(true)
    }

    /// Update the status of a feed (active, paused, deleted).
    pub fn update_status(&self, id: &str, status: &str) -> Result<bool> {
        self.ensure_loaded()?;
        let entry = self.cache.lock().unwrap().get(id).cloned();
        let Some(entry) = entry else { return Ok(false) };

        if status == "deleted" {
            return self.delete(id);
        }

        let now = Utc::now().to_rfc3339();
        self.db.with_conn(|conn| {
            conn.execute(
                "UPDATE aria_feeds SET status=?1, updated_at=?2 WHERE id=?3",
                params![status, now, id],
            )?;
            Ok(())
        })?;

        let updated = AriaFeedEntry {
            status: status.to_string(),
            updated_at: now,
            ..entry
        };
        self.cache.lock().unwrap().insert(id.to_string(), updated);
        Ok(true)
    }
}

// ── Tests ────────────────────────────────────────────────────────

#[cfg(test)]
mod tests {
    use super::*;
    use crate::aria::db::AriaDb;

    fn setup() -> AriaFeedRegistry {
        let db = AriaDb::open_in_memory().unwrap();
        AriaFeedRegistry::new(db)
    }

    fn upload_default(reg: &AriaFeedRegistry, tenant: &str, name: &str) -> AriaFeedEntry {
        reg.upload(FeedUploadRequest {
            tenant_id: tenant,
            name,
            description: "desc",
            handler_code: "handler()",
            schedule: "*/5 * * * *",
            refresh_seconds: Some(300),
            category: Some("news"),
            retention: None,
            display: None,
            sandbox_config: None,
        })
        .unwrap()
    }

    #[test]
    fn upload_and_get_roundtrip() {
        let reg = setup();
        let entry = upload_default(&reg, "t1", "news-feed");
        assert_eq!(entry.name, "news-feed");
        assert_eq!(entry.status, "active");
        assert_eq!(entry.schedule, "*/5 * * * *");

        let fetched = reg.get(&entry.id).unwrap().unwrap();
        assert_eq!(fetched.id, entry.id);
    }

    #[test]
    fn upload_empty_schedule_uses_daily_default() {
        let reg = setup();
        let entry = reg
            .upload(FeedUploadRequest {
                tenant_id: "t1",
                name: "daily-feed",
                description: "desc",
                handler_code: "handler()",
                schedule: "   ",
                refresh_seconds: None,
                category: None,
                retention: None,
                display: None,
                sandbox_config: None,
            })
            .unwrap();
        assert_eq!(entry.schedule, DEFAULT_FEED_SCHEDULE);
    }

    #[test]
    fn upsert_by_name_updates_existing() {
        let reg = setup();
        let v1 = upload_default(&reg, "t1", "feed");
        let v2 = reg
            .upload(FeedUploadRequest {
                tenant_id: "t1",
                name: "feed",
                description: "updated desc",
                handler_code: "handler_v2()",
                schedule: "0 * * * *",
                refresh_seconds: Some(600),
                category: None,
                retention: None,
                display: None,
                sandbox_config: None,
            })
            .unwrap();
        assert_eq!(v2.id, v1.id);
        assert_eq!(v2.description, "updated desc");
        assert_eq!(reg.count("t1").unwrap(), 1);
    }

    #[test]
    fn list_with_tenant_isolation() {
        let reg = setup();
        upload_default(&reg, "t1", "f1");
        upload_default(&reg, "t1", "f2");
        upload_default(&reg, "t2", "f3");

        assert_eq!(reg.list("t1").unwrap().len(), 2);
        assert_eq!(reg.list("t2").unwrap().len(), 1);
        assert_eq!(reg.list("t3").unwrap().len(), 0);
    }

    #[test]
    fn list_all_crosses_tenants() {
        let reg = setup();
        upload_default(&reg, "t1", "f1");
        upload_default(&reg, "t2", "f2");
        upload_default(&reg, "t3", "f3");

        assert_eq!(reg.list_all().unwrap().len(), 3);
    }

    #[test]
    fn soft_delete() {
        let reg = setup();
        let entry = upload_default(&reg, "t1", "doomed");
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
        upload_default(&reg, "t1", "x");
        assert_eq!(reg.count("t1").unwrap(), 1);
    }

    #[test]
    fn update_status_to_paused() {
        let reg = setup();
        let entry = upload_default(&reg, "t1", "pausable");
        reg.update_status(&entry.id, "paused").unwrap();

        let paused = reg.get(&entry.id).unwrap().unwrap();
        assert_eq!(paused.status, "paused");
    }

    #[test]
    fn update_status_to_deleted_removes_from_cache() {
        let reg = setup();
        let entry = upload_default(&reg, "t1", "del-via-status");
        reg.update_status(&entry.id, "deleted").unwrap();
        assert!(reg.get(&entry.id).unwrap().is_none());
        assert_eq!(reg.count("t1").unwrap(), 0);
    }

    #[test]
    fn get_by_name_works() {
        let reg = setup();
        upload_default(&reg, "t1", "named-feed");
        let found = reg.get_by_name("t1", "named-feed").unwrap().unwrap();
        assert_eq!(found.name, "named-feed");
        assert!(reg.get_by_name("t2", "named-feed").unwrap().is_none());
    }

    #[test]
    fn persistence_across_reload() {
        let db = AriaDb::open_in_memory().unwrap();
        {
            let reg = AriaFeedRegistry::new(db.clone());
            upload_default(&reg, "t1", "persist-feed");
        }
        let reg2 = AriaFeedRegistry::new(db);
        let entry = reg2.get_by_name("t1", "persist-feed").unwrap().unwrap();
        assert_eq!(entry.name, "persist-feed");
    }
}
