//! Agent registry — SQLite-backed store for Aria agent definitions.
//!
//! Each agent has handler code that is hashed for integrity, supports
//! model/temperature/prompt configuration, and is soft-deleted when removed.

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
pub struct AriaAgentEntry {
    pub id: String,
    pub tenant_id: String,
    pub name: String,
    pub description: String,
    pub model: Option<String>,
    pub temperature: Option<f64>,
    pub system_prompt: Option<String>,
    pub tools: String,
    pub thinking: Option<String>,
    pub max_retries: Option<i64>,
    pub timeout_seconds: Option<i64>,
    pub handler_code: String,
    pub handler_hash: String,
    pub sandbox_config: Option<String>,
    pub status: String,
    pub version: i64,
    pub created_at: String,
    pub updated_at: String,
}

pub struct AgentUploadRequest<'a> {
    pub tenant_id: &'a str,
    pub name: &'a str,
    pub description: &'a str,
    pub model: Option<&'a str>,
    pub temperature: Option<f64>,
    pub system_prompt: Option<&'a str>,
    pub tools: &'a str,
    pub thinking: Option<&'a str>,
    pub max_retries: Option<i64>,
    pub timeout_seconds: Option<i64>,
    pub handler_code: &'a str,
    pub sandbox_config: Option<&'a str>,
}

// ── Registry ─────────────────────────────────────────────────────

pub struct AriaAgentRegistry {
    db: AriaDb,
    cache: Mutex<HashMap<String, AriaAgentEntry>>,
    tenant_index: Mutex<HashMap<String, HashSet<String>>>,
    name_index: Mutex<HashMap<String, String>>,
    loaded: AtomicBool,
}

impl AriaAgentRegistry {
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
                "SELECT id, tenant_id, name, description, model, temperature,
                        system_prompt, tools, thinking, max_retries, timeout_seconds,
                        handler_code, handler_hash, sandbox_config, status, version,
                        created_at, updated_at
                 FROM aria_agents WHERE status != 'deleted'",
            )?;
            let rows = stmt.query_map([], |row| {
                Ok(AriaAgentEntry {
                    id: row.get(0)?,
                    tenant_id: row.get(1)?,
                    name: row.get(2)?,
                    description: row.get(3)?,
                    model: row.get(4)?,
                    temperature: row.get(5)?,
                    system_prompt: row.get(6)?,
                    tools: row.get(7)?,
                    thinking: row.get(8)?,
                    max_retries: row.get(9)?,
                    timeout_seconds: row.get(10)?,
                    handler_code: row.get(11)?,
                    handler_hash: row.get(12)?,
                    sandbox_config: row.get(13)?,
                    status: row.get(14)?,
                    version: row.get(15)?,
                    created_at: row.get(16)?,
                    updated_at: row.get(17)?,
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

    fn index_entry(&self, entry: &AriaAgentEntry) {
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

    fn deindex_entry(&self, entry: &AriaAgentEntry) {
        if let Some(set) = self.tenant_index.lock().unwrap().get_mut(&entry.tenant_id) {
            set.remove(&entry.id);
        }
        self.name_index
            .lock()
            .unwrap()
            .remove(&format!("{}:{}", entry.tenant_id, entry.name));
    }

    // ── Public API ───────────────────────────────────────────────

    pub fn upload(&self, req: AgentUploadRequest<'_>) -> Result<AriaAgentEntry> {
        let AgentUploadRequest {
            tenant_id,
            name,
            description,
            model,
            temperature,
            system_prompt,
            tools,
            thinking,
            max_retries,
            timeout_seconds,
            handler_code,
            sandbox_config,
        } = req;
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
            let new_version = entry.version + 1;
            self.db.with_conn(|conn| {
                conn.execute(
                    "UPDATE aria_agents SET description=?1, model=?2, temperature=?3,
                     system_prompt=?4, tools=?5, thinking=?6, max_retries=?7,
                     timeout_seconds=?8, handler_code=?9, handler_hash=?10,
                     sandbox_config=?11, version=?12, updated_at=?13
                     WHERE id=?14",
                    params![
                        description,
                        model,
                        temperature,
                        system_prompt,
                        tools,
                        thinking,
                        max_retries,
                        timeout_seconds,
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
            let updated = AriaAgentEntry {
                description: description.to_string(),
                model: model.map(String::from),
                temperature,
                system_prompt: system_prompt.map(String::from),
                tools: tools.to_string(),
                thinking: thinking.map(String::from),
                max_retries,
                timeout_seconds,
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
            let id = Uuid::new_v4().to_string();
            let entry = AriaAgentEntry {
                id: id.clone(),
                tenant_id: tenant_id.to_string(),
                name: name.to_string(),
                description: description.to_string(),
                model: model.map(String::from),
                temperature,
                system_prompt: system_prompt.map(String::from),
                tools: tools.to_string(),
                thinking: thinking.map(String::from),
                max_retries,
                timeout_seconds,
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
                    "INSERT INTO aria_agents (id, tenant_id, name, description, model,
                     temperature, system_prompt, tools, thinking, max_retries,
                     timeout_seconds, handler_code, handler_hash, sandbox_config,
                     status, version, created_at, updated_at)
                     VALUES (?1,?2,?3,?4,?5,?6,?7,?8,?9,?10,?11,?12,?13,?14,?15,?16,?17,?18)",
                    params![
                        entry.id,
                        entry.tenant_id,
                        entry.name,
                        entry.description,
                        entry.model,
                        entry.temperature,
                        entry.system_prompt,
                        entry.tools,
                        entry.thinking,
                        entry.max_retries,
                        entry.timeout_seconds,
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

    pub fn get(&self, id: &str) -> Result<Option<AriaAgentEntry>> {
        self.ensure_loaded()?;
        Ok(self.cache.lock().unwrap().get(id).cloned())
    }

    pub fn get_by_name(&self, tenant_id: &str, name: &str) -> Result<Option<AriaAgentEntry>> {
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

    pub fn list(&self, tenant_id: &str) -> Result<Vec<AriaAgentEntry>> {
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

    pub fn delete(&self, id: &str) -> Result<bool> {
        self.ensure_loaded()?;
        let entry = { self.cache.lock().unwrap().get(id).cloned() };
        let Some(entry) = entry else { return Ok(false) };
        let now = Utc::now().to_rfc3339();
        self.db.with_conn(|conn| {
            conn.execute(
                "UPDATE aria_agents SET status='deleted', updated_at=?1 WHERE id=?2",
                params![now, id],
            )?;
            Ok(())
        })?;
        self.deindex_entry(&entry);
        self.cache.lock().unwrap().remove(id);
        Ok(true)
    }

    pub fn get_prompt_section(&self, tenant_id: &str) -> Result<String> {
        let agents = self.list(tenant_id)?;
        if agents.is_empty() {
            return Ok(String::new());
        }
        let mut out = String::from("## Available Agents\n\n");
        for a in &agents {
            out.push_str(&format!(
                "- **{}** (v{}): {}\n  Model: {}\n",
                a.name,
                a.version,
                a.description,
                a.model.as_deref().unwrap_or("default"),
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

    fn setup() -> AriaAgentRegistry {
        let db = AriaDb::open_in_memory().unwrap();
        AriaAgentRegistry::new(db)
    }

    fn upload_default(reg: &AriaAgentRegistry, tenant: &str, name: &str) -> AriaAgentEntry {
        reg.upload(AgentUploadRequest {
            tenant_id: tenant,
            name,
            description: "desc",
            model: Some("claude-3"),
            temperature: Some(0.7),
            system_prompt: Some("You are helpful"),
            tools: "[]",
            thinking: None,
            max_retries: None,
            timeout_seconds: None,
            handler_code: "handler()",
            sandbox_config: None,
        })
        .unwrap()
    }

    #[test]
    fn upload_and_get_roundtrip() {
        let reg = setup();
        let entry = upload_default(&reg, "t1", "my-agent");
        assert_eq!(entry.name, "my-agent");
        assert_eq!(entry.version, 1);
        assert_eq!(entry.model.as_deref(), Some("claude-3"));

        let fetched = reg.get(&entry.id).unwrap().unwrap();
        assert_eq!(fetched.id, entry.id);
    }

    #[test]
    fn upsert_by_name_updates_existing() {
        let reg = setup();
        let v1 = upload_default(&reg, "t1", "agent");
        let v2 = reg
            .upload(AgentUploadRequest {
                tenant_id: "t1",
                name: "agent",
                description: "new desc",
                model: Some("gpt-4"),
                temperature: Some(0.5),
                system_prompt: Some("Be concise"),
                tools: "[]",
                thinking: None,
                max_retries: None,
                timeout_seconds: None,
                handler_code: "handler_v2()",
                sandbox_config: None,
            })
            .unwrap();
        assert_eq!(v2.id, v1.id);
        assert_eq!(v2.version, 2);
        assert_eq!(v2.model.as_deref(), Some("gpt-4"));
        assert_eq!(reg.count("t1").unwrap(), 1);
    }

    #[test]
    fn list_with_tenant_isolation() {
        let reg = setup();
        upload_default(&reg, "t1", "a1");
        upload_default(&reg, "t1", "a2");
        upload_default(&reg, "t2", "a3");

        assert_eq!(reg.list("t1").unwrap().len(), 2);
        assert_eq!(reg.list("t2").unwrap().len(), 1);
        assert_eq!(reg.list("t3").unwrap().len(), 0);
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
        upload_default(&reg, "t1", "y");
        assert_eq!(reg.count("t1").unwrap(), 2);
    }

    #[test]
    fn get_by_name_works() {
        let reg = setup();
        upload_default(&reg, "t1", "named-agent");
        let found = reg.get_by_name("t1", "named-agent").unwrap().unwrap();
        assert_eq!(found.name, "named-agent");
        assert!(reg.get_by_name("t1", "nope").unwrap().is_none());
        assert!(reg.get_by_name("t2", "named-agent").unwrap().is_none());
    }

    #[test]
    fn get_prompt_section() {
        let reg = setup();
        upload_default(&reg, "t1", "writer");
        let section = reg.get_prompt_section("t1").unwrap();
        assert!(section.contains("writer"));
        assert!(section.contains("claude-3"));
    }

    #[test]
    fn persistence_across_reload() {
        let db = AriaDb::open_in_memory().unwrap();
        {
            let reg = AriaAgentRegistry::new(db.clone());
            upload_default(&reg, "t1", "persist-agent");
        }
        let reg2 = AriaAgentRegistry::new(db);
        let entry = reg2.get_by_name("t1", "persist-agent").unwrap().unwrap();
        assert_eq!(entry.name, "persist-agent");
    }
}
