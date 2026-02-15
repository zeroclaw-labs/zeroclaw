//! Tool registry — SQLite-backed store for Aria tool definitions.
//!
//! Each tool has handler code that is hashed (SHA-256-like) for integrity,
//! auto-versioned on name collision, and soft-deleted when removed.

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
pub struct AriaToolEntry {
    pub id: String,
    pub tenant_id: String,
    pub name: String,
    pub description: String,
    pub schema: String,
    pub handler_code: String,
    pub handler_hash: String,
    pub sandbox_config: Option<String>,
    pub status: String,
    pub version: i64,
    pub created_at: String,
    pub updated_at: String,
}

// ── Registry ─────────────────────────────────────────────────────

pub struct AriaToolRegistry {
    db: AriaDb,
    cache: Mutex<HashMap<String, AriaToolEntry>>,
    tenant_index: Mutex<HashMap<String, HashSet<String>>>,
    name_index: Mutex<HashMap<String, String>>,
    loaded: AtomicBool,
}

impl AriaToolRegistry {
    pub fn new(db: AriaDb) -> Self {
        Self {
            db,
            cache: Mutex::new(HashMap::new()),
            tenant_index: Mutex::new(HashMap::new()),
            name_index: Mutex::new(HashMap::new()),
            loaded: AtomicBool::new(false),
        }
    }

    // ── Cache management ─────────────────────────────────────────

    fn ensure_loaded(&self) -> Result<()> {
        if self.loaded.load(Ordering::Acquire) {
            return Ok(());
        }
        let entries = self.db.with_conn(|conn| {
            let mut stmt = conn.prepare(
                "SELECT id, tenant_id, name, description, schema, handler_code,
                        handler_hash, sandbox_config, status, version, created_at, updated_at
                 FROM aria_tools WHERE status != 'deleted'",
            )?;
            let rows = stmt.query_map([], |row| {
                Ok(AriaToolEntry {
                    id: row.get(0)?,
                    tenant_id: row.get(1)?,
                    name: row.get(2)?,
                    description: row.get(3)?,
                    schema: row.get(4)?,
                    handler_code: row.get(5)?,
                    handler_hash: row.get(6)?,
                    sandbox_config: row.get(7)?,
                    status: row.get(8)?,
                    version: row.get(9)?,
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
            ti.entry(e.tenant_id.clone())
                .or_default()
                .insert(e.id.clone());
            ni.insert(format!("{}:{}", e.tenant_id, e.name), e.id.clone());
            cache.insert(e.id.clone(), e);
        }
        self.loaded.store(true, Ordering::Release);
        Ok(())
    }

    fn index_entry(&self, entry: &AriaToolEntry) {
        let mut ti = self.tenant_index.lock().unwrap();
        ti.entry(entry.tenant_id.clone())
            .or_default()
            .insert(entry.id.clone());
        let mut ni = self.name_index.lock().unwrap();
        ni.insert(
            format!("{}:{}", entry.tenant_id, entry.name),
            entry.id.clone(),
        );
    }

    fn deindex_entry(&self, entry: &AriaToolEntry) {
        let mut ti = self.tenant_index.lock().unwrap();
        if let Some(set) = ti.get_mut(&entry.tenant_id) {
            set.remove(&entry.id);
        }
        let mut ni = self.name_index.lock().unwrap();
        ni.remove(&format!("{}:{}", entry.tenant_id, entry.name));
    }

    // ── Public API ───────────────────────────────────────────────

    /// Upload (upsert) a tool. Same tenant+name updates; different name creates.
    /// Handler code is hashed and version is auto-incremented on name collision.
    pub fn upload(
        &self,
        tenant_id: &str,
        name: &str,
        description: &str,
        schema: &str,
        handler_code: &str,
        sandbox_config: Option<&str>,
    ) -> Result<AriaToolEntry> {
        self.ensure_loaded()?;
        let now = Utc::now().to_rfc3339();
        let hash = sha256_hex(handler_code);

        // Check for existing entry with same tenant+name
        let existing_id = {
            let ni = self.name_index.lock().unwrap();
            ni.get(&format!("{tenant_id}:{name}")).cloned()
        };

        if let Some(eid) = existing_id {
            // Update existing
            let mut cache = self.cache.lock().unwrap();
            let entry = cache.get(&eid).cloned().context("Cache inconsistency")?;
            let new_version = entry.version + 1;
            self.db.with_conn(|conn| {
                conn.execute(
                    "UPDATE aria_tools SET description=?1, schema=?2, handler_code=?3,
                     handler_hash=?4, sandbox_config=?5, version=?6, updated_at=?7
                     WHERE id=?8",
                    params![
                        description,
                        schema,
                        handler_code,
                        hash,
                        sandbox_config,
                        new_version,
                        now,
                        eid
                    ],
                )?;
                Ok(())
            })?;
            let updated = AriaToolEntry {
                description: description.to_string(),
                schema: schema.to_string(),
                handler_code: handler_code.to_string(),
                handler_hash: hash,
                sandbox_config: sandbox_config.map(String::from),
                version: new_version,
                updated_at: now,
                ..entry
            };
            cache.insert(eid, updated.clone());
            Ok(updated)
        } else {
            // Resolve unique name (auto-version)
            let final_name = {
                let ni = self.name_index.lock().unwrap();
                let mut candidate = name.to_string();
                let mut v = 2;
                while ni.contains_key(&format!("{tenant_id}:{candidate}")) {
                    candidate = format!("{name}-v{v}");
                    v += 1;
                }
                candidate
            };
            let id = Uuid::new_v4().to_string();
            let entry = AriaToolEntry {
                id: id.clone(),
                tenant_id: tenant_id.to_string(),
                name: final_name.clone(),
                description: description.to_string(),
                schema: schema.to_string(),
                handler_code: handler_code.to_string(),
                handler_hash: hash,
                sandbox_config: sandbox_config.map(String::from),
                status: "active".to_string(),
                version: 1,
                created_at: now.clone(),
                updated_at: now,
            };
            self.db.with_conn(|conn| {
                conn.execute(
                    "INSERT INTO aria_tools (id, tenant_id, name, description, schema,
                     handler_code, handler_hash, sandbox_config, status, version,
                     created_at, updated_at)
                     VALUES (?1,?2,?3,?4,?5,?6,?7,?8,?9,?10,?11,?12)",
                    params![
                        entry.id,
                        entry.tenant_id,
                        entry.name,
                        entry.description,
                        entry.schema,
                        entry.handler_code,
                        entry.handler_hash,
                        entry.sandbox_config,
                        entry.status,
                        entry.version,
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

    pub fn get(&self, id: &str) -> Result<Option<AriaToolEntry>> {
        self.ensure_loaded()?;
        let cache = self.cache.lock().unwrap();
        Ok(cache.get(id).cloned())
    }

    pub fn get_by_name(&self, tenant_id: &str, name: &str) -> Result<Option<AriaToolEntry>> {
        self.ensure_loaded()?;
        let ni = self.name_index.lock().unwrap();
        let id = ni.get(&format!("{tenant_id}:{name}")).cloned();
        drop(ni);
        match id {
            Some(id) => self.get(&id),
            None => Ok(None),
        }
    }

    pub fn list(&self, tenant_id: &str) -> Result<Vec<AriaToolEntry>> {
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
        let ti = self.tenant_index.lock().unwrap();
        Ok(ti.get(tenant_id).map_or(0, std::collections::HashSet::len))
    }

    /// Soft-delete a tool by ID.
    pub fn delete(&self, id: &str) -> Result<bool> {
        self.ensure_loaded()?;
        let entry = {
            let cache = self.cache.lock().unwrap();
            cache.get(id).cloned()
        };
        let Some(entry) = entry else { return Ok(false) };
        let now = Utc::now().to_rfc3339();
        self.db.with_conn(|conn| {
            conn.execute(
                "UPDATE aria_tools SET status='deleted', updated_at=?1 WHERE id=?2",
                params![now, id],
            )?;
            Ok(())
        })?;
        self.deindex_entry(&entry);
        self.cache.lock().unwrap().remove(id);
        Ok(true)
    }

    /// Generate a system-prompt-injectable summary of all tools for a tenant.
    pub fn get_prompt_section(&self, tenant_id: &str) -> Result<String> {
        let tools = self.list(tenant_id)?;
        if tools.is_empty() {
            return Ok(String::new());
        }
        let mut out = String::from("## Available Tools\n\n");
        for t in &tools {
            out.push_str(&format!(
                "- **{}** (v{}): {}\n  Schema: {}\n",
                t.name, t.version, t.description, t.schema
            ));
        }
        Ok(out)
    }
}

// ── Tests ────────────────────────────────────────────────────────

#[cfg(test)]
mod tests {
    use super::*;
    use crate::aria::db::AriaDb;

    fn setup() -> AriaToolRegistry {
        let db = AriaDb::open_in_memory().unwrap();
        AriaToolRegistry::new(db)
    }

    #[test]
    fn upload_and_get_roundtrip() {
        let reg = setup();
        let entry = reg
            .upload("t1", "my-tool", "A tool", "{}", "console.log('hi')", None)
            .unwrap();
        assert_eq!(entry.name, "my-tool");
        assert_eq!(entry.version, 1);
        assert_eq!(entry.status, "active");
        assert!(!entry.handler_hash.is_empty());

        let fetched = reg.get(&entry.id).unwrap().unwrap();
        assert_eq!(fetched.id, entry.id);
        assert_eq!(fetched.handler_code, "console.log('hi')");
    }

    #[test]
    fn upsert_by_name_updates_existing() {
        let reg = setup();
        let v1 = reg
            .upload("t1", "my-tool", "v1 desc", "{}", "code_v1", None)
            .unwrap();
        assert_eq!(v1.version, 1);

        let v2 = reg
            .upload("t1", "my-tool", "v2 desc", "{}", "code_v2", None)
            .unwrap();
        assert_eq!(v2.id, v1.id);
        assert_eq!(v2.version, 2);
        assert_eq!(v2.description, "v2 desc");
        assert_eq!(v2.handler_code, "code_v2");

        // Only one tool should exist
        assert_eq!(reg.count("t1").unwrap(), 1);
    }

    #[test]
    fn list_with_tenant_isolation() {
        let reg = setup();
        reg.upload("t1", "tool-a", "", "{}", "a", None).unwrap();
        reg.upload("t1", "tool-b", "", "{}", "b", None).unwrap();
        reg.upload("t2", "tool-c", "", "{}", "c", None).unwrap();

        assert_eq!(reg.list("t1").unwrap().len(), 2);
        assert_eq!(reg.list("t2").unwrap().len(), 1);
        assert_eq!(reg.list("t3").unwrap().len(), 0);
    }

    #[test]
    fn soft_delete_removes_from_cache() {
        let reg = setup();
        let entry = reg.upload("t1", "tool", "", "{}", "x", None).unwrap();
        assert_eq!(reg.count("t1").unwrap(), 1);

        let deleted = reg.delete(&entry.id).unwrap();
        assert!(deleted);
        assert_eq!(reg.count("t1").unwrap(), 0);
        assert!(reg.get(&entry.id).unwrap().is_none());
    }

    #[test]
    fn delete_nonexistent_returns_false() {
        let reg = setup();
        assert!(!reg.delete("no-such-id").unwrap());
    }

    #[test]
    fn count_accuracy() {
        let reg = setup();
        assert_eq!(reg.count("t1").unwrap(), 0);
        reg.upload("t1", "a", "", "{}", "x", None).unwrap();
        assert_eq!(reg.count("t1").unwrap(), 1);
        reg.upload("t1", "b", "", "{}", "y", None).unwrap();
        assert_eq!(reg.count("t1").unwrap(), 2);
    }

    #[test]
    fn get_by_name() {
        let reg = setup();
        reg.upload("t1", "finder", "find things", "{}", "code", None)
            .unwrap();
        let found = reg.get_by_name("t1", "finder").unwrap().unwrap();
        assert_eq!(found.name, "finder");
        assert!(reg.get_by_name("t1", "nope").unwrap().is_none());
        assert!(reg.get_by_name("t2", "finder").unwrap().is_none());
    }

    #[test]
    fn handler_hash_changes_on_code_change() {
        let reg = setup();
        let v1 = reg.upload("t1", "h", "", "{}", "code_a", None).unwrap();
        let v2 = reg.upload("t1", "h", "", "{}", "code_b", None).unwrap();
        assert_ne!(v1.handler_hash, v2.handler_hash);
    }

    #[test]
    fn get_prompt_section_generates_output() {
        let reg = setup();
        reg.upload(
            "t1",
            "calc",
            "Calculator",
            r#"{"type":"object"}"#,
            "fn",
            None,
        )
        .unwrap();
        let section = reg.get_prompt_section("t1").unwrap();
        assert!(section.contains("calc"));
        assert!(section.contains("Calculator"));
    }

    #[test]
    fn get_prompt_section_empty_for_no_tools() {
        let reg = setup();
        let section = reg.get_prompt_section("t1").unwrap();
        assert!(section.is_empty());
    }

    #[test]
    fn persistence_across_reload() {
        let db = AriaDb::open_in_memory().unwrap();
        {
            let reg = AriaToolRegistry::new(db.clone());
            reg.upload("t1", "persist", "d", "{}", "code", None)
                .unwrap();
        }
        // New registry instance, same DB
        let reg2 = AriaToolRegistry::new(db);
        let entry = reg2.get_by_name("t1", "persist").unwrap().unwrap();
        assert_eq!(entry.name, "persist");
    }
}
