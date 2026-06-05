use anyhow::{Context, Result};
use rusqlite::{Connection, params};
use serde::{Deserialize, Serialize};
use std::path::{Path, PathBuf};
use uuid::Uuid;

#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum LifecycleState {
    Resident,
    Dormant,
}

impl LifecycleState {
    fn as_str(&self) -> &'static str {
        match self {
            Self::Resident => "resident",
            Self::Dormant => "dormant",
        }
    }

    fn from_str(s: &str) -> Self {
        match s {
            "resident" => Self::Resident,
            _ => Self::Dormant,
        }
    }
}

#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct PoolMember {
    pub agentid: String,
    pub name: String,
    pub system_prompt: Option<String>,
    pub provider: String,
    pub model: String,
    pub allowed_tools: Vec<String>,
    pub lifecycle_state: LifecycleState,
    pub created_at: String,
    pub last_active: String,
}

pub struct PoolStore {
    db_path: PathBuf,
}

impl PoolStore {
    pub fn open(workspace_dir: &Path) -> Result<Self> {
        let state_dir = workspace_dir.join("state");
        std::fs::create_dir_all(&state_dir)?;
        let db_path = state_dir.join("state.db");

        let conn = Connection::open(&db_path)
            .with_context(|| format!("Failed to open state.db: {}", db_path.display()))?;
        conn.execute_batch(
            "PRAGMA journal_mode = WAL;
             PRAGMA synchronous = NORMAL;
             PRAGMA busy_timeout = 5000;",
        )?;
        conn.execute_batch(
            "CREATE TABLE IF NOT EXISTS pool_members (
                 agentid         TEXT PRIMARY KEY,
                 name            TEXT NOT NULL,
                 system_prompt   TEXT,
                 provider        TEXT NOT NULL,
                 model           TEXT NOT NULL,
                 allowed_tools   TEXT NOT NULL DEFAULT '[]',
                 lifecycle_state TEXT NOT NULL DEFAULT 'dormant',
                 created_at      TEXT NOT NULL,
                 last_active     TEXT NOT NULL
             );
             CREATE INDEX IF NOT EXISTS idx_pool_members_name ON pool_members(name);
             CREATE INDEX IF NOT EXISTS idx_pool_members_state ON pool_members(lifecycle_state);",
        )
        .context("Failed to create pool_members table")?;
        Ok(Self { db_path })
    }

    fn connect(&self) -> Result<Connection> {
        let conn = Connection::open(&self.db_path)
            .with_context(|| format!("Failed to open state.db: {}", self.db_path.display()))?;
        conn.execute_batch("PRAGMA busy_timeout = 5000;")?;
        Ok(conn)
    }

    pub fn spawn_member(
        &self,
        name: &str,
        system_prompt: Option<&str>,
        provider: &str,
        model: &str,
        allowed_tools: &[String],
    ) -> Result<PoolMember> {
        let agentid = Uuid::new_v4().to_string();
        let now = chrono::Utc::now().to_rfc3339();
        let tools_json = serde_json::to_string(allowed_tools)?;

        let conn = self.connect()?;
        conn.execute(
            "INSERT INTO pool_members (agentid, name, system_prompt, provider, model, allowed_tools, lifecycle_state, created_at, last_active)
             VALUES (?1, ?2, ?3, ?4, ?5, ?6, ?7, ?8, ?9)",
            params![agentid, name, system_prompt, provider, model, tools_json, "resident", now, now],
        )?;

        Ok(PoolMember {
            agentid,
            name: name.to_string(),
            system_prompt: system_prompt.map(String::from),
            provider: provider.to_string(),
            model: model.to_string(),
            allowed_tools: allowed_tools.to_vec(),
            lifecycle_state: LifecycleState::Resident,
            created_at: now.clone(),
            last_active: now,
        })
    }

    pub fn get_member(&self, agentid: &str) -> Result<Option<PoolMember>> {
        let conn = self.connect()?;
        let mut stmt = conn.prepare(
            "SELECT agentid, name, system_prompt, provider, model, allowed_tools, lifecycle_state, created_at, last_active
             FROM pool_members WHERE agentid = ?1",
        )?;

        let mut rows = stmt.query_map(params![agentid], |row| {
            let tools_json: String = row.get(5)?;
            Ok(PoolMember {
                agentid: row.get(0)?,
                name: row.get(1)?,
                system_prompt: row.get(2)?,
                provider: row.get(3)?,
                model: row.get(4)?,
                allowed_tools: serde_json::from_str(&tools_json).unwrap_or_default(),
                lifecycle_state: LifecycleState::from_str(&row.get::<_, String>(6)?),
                created_at: row.get(7)?,
                last_active: row.get(8)?,
            })
        })?;

        Ok(rows.next().transpose()?)
    }

    pub fn get_member_by_name(&self, name: &str) -> Result<Option<PoolMember>> {
        let conn = self.connect()?;
        let mut stmt = conn.prepare(
            "SELECT agentid, name, system_prompt, provider, model, allowed_tools, lifecycle_state, created_at, last_active
             FROM pool_members WHERE name = ?1 ORDER BY last_active DESC LIMIT 1",
        )?;

        let mut rows = stmt.query_map(params![name], |row| {
            let tools_json: String = row.get(5)?;
            Ok(PoolMember {
                agentid: row.get(0)?,
                name: row.get(1)?,
                system_prompt: row.get(2)?,
                provider: row.get(3)?,
                model: row.get(4)?,
                allowed_tools: serde_json::from_str(&tools_json).unwrap_or_default(),
                lifecycle_state: LifecycleState::from_str(&row.get::<_, String>(6)?),
                created_at: row.get(7)?,
                last_active: row.get(8)?,
            })
        })?;

        Ok(rows.next().transpose()?)
    }

    pub fn list_members(&self) -> Result<Vec<PoolMember>> {
        let conn = self.connect()?;
        let mut stmt = conn.prepare(
            "SELECT agentid, name, system_prompt, provider, model, allowed_tools, lifecycle_state, created_at, last_active
             FROM pool_members ORDER BY last_active DESC",
        )?;

        let rows = stmt.query_map([], |row| {
            let tools_json: String = row.get(5)?;
            Ok(PoolMember {
                agentid: row.get(0)?,
                name: row.get(1)?,
                system_prompt: row.get(2)?,
                provider: row.get(3)?,
                model: row.get(4)?,
                allowed_tools: serde_json::from_str(&tools_json).unwrap_or_default(),
                lifecycle_state: LifecycleState::from_str(&row.get::<_, String>(6)?),
                created_at: row.get(7)?,
                last_active: row.get(8)?,
            })
        })?;

        Ok(rows.filter_map(|r| r.ok()).collect())
    }

    pub fn set_lifecycle_state(&self, agentid: &str, state: LifecycleState) -> Result<bool> {
        let conn = self.connect()?;
        let updated = conn.execute(
            "UPDATE pool_members SET lifecycle_state = ?1, last_active = ?2 WHERE agentid = ?3",
            params![state.as_str(), chrono::Utc::now().to_rfc3339(), agentid],
        )?;
        Ok(updated > 0)
    }

    pub fn touch_last_active(&self, agentid: &str) -> Result<()> {
        let conn = self.connect()?;
        conn.execute(
            "UPDATE pool_members SET last_active = ?1 WHERE agentid = ?2",
            params![chrono::Utc::now().to_rfc3339(), agentid],
        )?;
        Ok(())
    }

    pub fn destroy_member(&self, agentid: &str) -> Result<bool> {
        let conn = self.connect()?;
        let deleted = conn.execute(
            "DELETE FROM pool_members WHERE agentid = ?1",
            params![agentid],
        )?;
        Ok(deleted > 0)
    }

    pub fn coldest_resident(&self) -> Result<Option<PoolMember>> {
        let conn = self.connect()?;
        let mut stmt = conn.prepare(
            "SELECT agentid, name, system_prompt, provider, model, allowed_tools, lifecycle_state, created_at, last_active
             FROM pool_members WHERE lifecycle_state = 'resident'
             ORDER BY last_active ASC LIMIT 1",
        )?;

        let mut rows = stmt.query_map([], |row| {
            let tools_json: String = row.get(5)?;
            Ok(PoolMember {
                agentid: row.get(0)?,
                name: row.get(1)?,
                system_prompt: row.get(2)?,
                provider: row.get(3)?,
                model: row.get(4)?,
                allowed_tools: serde_json::from_str(&tools_json).unwrap_or_default(),
                lifecycle_state: LifecycleState::from_str(&row.get::<_, String>(6)?),
                created_at: row.get(7)?,
                last_active: row.get(8)?,
            })
        })?;

        Ok(rows.next().transpose()?)
    }

    pub fn resident_count(&self) -> Result<usize> {
        let conn = self.connect()?;
        let count: i64 = conn.query_row(
            "SELECT COUNT(*) FROM pool_members WHERE lifecycle_state = 'resident'",
            [],
            |r| r.get(0),
        )?;
        Ok(count as usize)
    }

    pub fn member_count(&self) -> Result<usize> {
        let conn = self.connect()?;
        let count: i64 = conn.query_row(
            "SELECT COUNT(*) FROM pool_members",
            [],
            |r| r.get(0),
        )?;
        Ok(count as usize)
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use tempfile::TempDir;

    fn test_store() -> (PoolStore, TempDir) {
        let tmp = TempDir::new().unwrap();
        let store = PoolStore::open(tmp.path()).unwrap();
        (store, tmp)
    }

    #[test]
    fn spawn_creates_member_with_unique_agentid() {
        let (store, _tmp) = test_store();
        let m1 = store.spawn_member("researcher", None, "zai", "glm-5.1", &[]).unwrap();
        let m2 = store.spawn_member("coder", None, "zai", "glm-5.1", &[]).unwrap();
        assert_ne!(m1.agentid, m2.agentid);
        assert_eq!(m1.lifecycle_state, LifecycleState::Resident);
        assert_eq!(store.member_count().unwrap(), 2);
    }

    #[test]
    fn get_member_by_agentid_and_name() {
        let (store, _tmp) = test_store();
        let m = store.spawn_member("researcher", Some("You are a researcher"), "zai", "glm-5.1", &[]).unwrap();

        let by_id = store.get_member(&m.agentid).unwrap().unwrap();
        assert_eq!(by_id.name, "researcher");
        assert_eq!(by_id.system_prompt, Some("You are a researcher".to_string()));

        let by_name = store.get_member_by_name("researcher").unwrap().unwrap();
        assert_eq!(by_name.agentid, m.agentid);
    }

    #[test]
    fn lifecycle_spin_down_and_spin_up_preserves_agentid() {
        let (store, _tmp) = test_store();
        let m = store.spawn_member("researcher", None, "zai", "glm-5.1", &[]).unwrap();
        let original_agentid = m.agentid.clone();

        store.set_lifecycle_state(&m.agentid, LifecycleState::Dormant).unwrap();
        let dormant = store.get_member(&original_agentid).unwrap().unwrap();
        assert_eq!(dormant.lifecycle_state, LifecycleState::Dormant);
        assert_eq!(dormant.agentid, original_agentid);

        store.set_lifecycle_state(&m.agentid, LifecycleState::Resident).unwrap();
        let restored = store.get_member(&original_agentid).unwrap().unwrap();
        assert_eq!(restored.lifecycle_state, LifecycleState::Resident);
        assert_eq!(restored.agentid, original_agentid);
    }

    #[test]
    fn destroy_member_removes_row() {
        let (store, _tmp) = test_store();
        let m = store.spawn_member("disposable", None, "zai", "glm-5.1", &[]).unwrap();
        assert!(store.destroy_member(&m.agentid).unwrap());
        assert!(store.get_member(&m.agentid).unwrap().is_none());
        assert_eq!(store.member_count().unwrap(), 0);
    }

    #[test]
    fn coldest_resident_returns_least_recently_active() {
        let (store, _tmp) = test_store();
        let old = store.spawn_member("old", None, "zai", "glm-5.1", &[]).unwrap();
        std::thread::sleep(std::time::Duration::from_millis(10));
        let _new = store.spawn_member("new", None, "zai", "glm-5.1", &[]).unwrap();

        let coldest = store.coldest_resident().unwrap().unwrap();
        assert_eq!(coldest.agentid, old.agentid);
    }

    #[test]
    fn list_members_returns_all() {
        let (store, _tmp) = test_store();
        store.spawn_member("a", None, "zai", "glm-5.1", &[]).unwrap();
        store.spawn_member("b", None, "zai", "glm-5.1", &[]).unwrap();
        store.spawn_member("c", None, "zai", "glm-5.1", &[]).unwrap();
        assert_eq!(store.list_members().unwrap().len(), 3);
    }

    #[test]
    fn allowed_tools_round_trips() {
        let (store, _tmp) = test_store();
        let tools = vec!["shell".to_string(), "file_read".to_string()];
        let m = store.spawn_member("tooled", None, "zai", "glm-5.1", &tools).unwrap();
        let loaded = store.get_member(&m.agentid).unwrap().unwrap();
        assert_eq!(loaded.allowed_tools, tools);
    }

    #[test]
    fn resident_count_tracks_lifecycle() {
        let (store, _tmp) = test_store();
        let m1 = store.spawn_member("a", None, "zai", "glm-5.1", &[]).unwrap();
        let _m2 = store.spawn_member("b", None, "zai", "glm-5.1", &[]).unwrap();
        assert_eq!(store.resident_count().unwrap(), 2);

        store.set_lifecycle_state(&m1.agentid, LifecycleState::Dormant).unwrap();
        assert_eq!(store.resident_count().unwrap(), 1);
    }

    #[test]
    fn rehydration_round_trip_context_survives_down_up_cycle() {
        use daemonclaw_infra::session_backend::SessionBackend;
        use daemonclaw_infra::session_sqlite::SqliteSessionBackend;
        use daemonclaw_providers::ChatMessage;

        let tmp = TempDir::new().unwrap();
        let store = PoolStore::open(tmp.path()).unwrap();
        let backend = SqliteSessionBackend::new(tmp.path()).unwrap();

        let member = store
            .spawn_member("researcher", Some("You are a researcher"), "zai", "glm-5.1", &[])
            .unwrap();
        assert_eq!(member.lifecycle_state, LifecycleState::Resident);

        // Simulate conversation: write history keyed by agentid
        let msg1 = ChatMessage::user("Summarize the paper".to_string());
        let msg2 = ChatMessage::assistant("The paper discusses...".to_string());
        backend
            .append_with_actor(&member.agentid, &msg1, Some(&member.agentid), Some("pool_member"))
            .unwrap();
        backend
            .append_with_actor(&member.agentid, &msg2, Some(&member.agentid), Some("pool_member"))
            .unwrap();

        // Spin down — durable context in sessions.db, pool flag flipped
        store
            .set_lifecycle_state(&member.agentid, LifecycleState::Dormant)
            .unwrap();
        let dormant = store.get_member(&member.agentid).unwrap().unwrap();
        assert_eq!(dormant.lifecycle_state, LifecycleState::Dormant);

        // Spin up — reload from sessions.db
        store
            .set_lifecycle_state(&member.agentid, LifecycleState::Resident)
            .unwrap();
        let restored = store.get_member(&member.agentid).unwrap().unwrap();
        assert_eq!(restored.lifecycle_state, LifecycleState::Resident);
        assert_eq!(restored.agentid, member.agentid);

        // THE CRITERION: context survives the full down→up cycle
        let loaded_history = backend.load(&member.agentid);
        assert_eq!(loaded_history.len(), 2, "history must survive spin-down→up");
        assert_eq!(loaded_history[0].role, "user");
        assert_eq!(loaded_history[0].content, "Summarize the paper");
        assert_eq!(loaded_history[1].role, "assistant");
        assert_eq!(loaded_history[1].content, "The paper discusses...");
    }

    #[test]
    fn eviction_under_pressure_spins_down_coldest_preserves_context() {
        use daemonclaw_infra::session_backend::SessionBackend;
        use daemonclaw_infra::session_sqlite::SqliteSessionBackend;
        use daemonclaw_providers::ChatMessage;

        let tmp = TempDir::new().unwrap();
        let store = PoolStore::open(tmp.path()).unwrap();
        let backend = SqliteSessionBackend::new(tmp.path()).unwrap();

        // Spawn two residents; old is colder
        let old = store
            .spawn_member("old-agent", None, "zai", "glm-5.1", &[])
            .unwrap();
        std::thread::sleep(std::time::Duration::from_millis(10));
        let _new = store
            .spawn_member("new-agent", None, "zai", "glm-5.1", &[])
            .unwrap();

        // Give old some conversation context
        backend
            .append_with_actor(
                &old.agentid,
                &ChatMessage::user("important context".to_string()),
                Some(&old.agentid),
                Some("pool_member"),
            )
            .unwrap();

        // Simulate eviction: coldest resident → dormant
        let coldest = store.coldest_resident().unwrap().unwrap();
        assert_eq!(coldest.agentid, old.agentid);
        store
            .set_lifecycle_state(&coldest.agentid, LifecycleState::Dormant)
            .unwrap();

        // Verify: evicted, not deleted
        let evicted = store.get_member(&old.agentid).unwrap().unwrap();
        assert_eq!(evicted.lifecycle_state, LifecycleState::Dormant);
        assert_eq!(store.resident_count().unwrap(), 1);

        // Context preserved and re-loadable
        let history = backend.load(&old.agentid);
        assert_eq!(history.len(), 1);
        assert_eq!(history[0].content, "important context");
    }

    #[test]
    fn pool_off_produces_no_row() {
        let (store, _tmp) = test_store();
        // Pool OFF: no spawn_member call → no rows
        assert_eq!(store.member_count().unwrap(), 0);
        assert_eq!(store.list_members().unwrap().len(), 0);
    }

    #[test]
    fn pool_on_delegation_creates_persistent_member() {
        let (store, _tmp) = test_store();
        let member = store
            .spawn_member("delegate-target", Some("Do research"), "zai", "glm-5.1", &["shell".to_string()])
            .unwrap();

        // Row exists after spawn
        let loaded = store.get_member(&member.agentid).unwrap();
        assert!(loaded.is_some(), "pool member must persist after spawn");
        let loaded = loaded.unwrap();
        assert_eq!(loaded.name, "delegate-target");
        assert_eq!(loaded.lifecycle_state, LifecycleState::Resident);
        assert_eq!(loaded.allowed_tools, vec!["shell"]);
    }

    #[test]
    fn member_retargetable_by_agentid_across_delegations() {
        let (store, _tmp) = test_store();
        let member = store
            .spawn_member("researcher", None, "zai", "glm-5.1", &[])
            .unwrap();
        let agentid = member.agentid.clone();

        // First "delegation": touch
        store.touch_last_active(&agentid).unwrap();

        // Second "delegation": same agentid resolves to same member
        let found = store.get_member(&agentid).unwrap().unwrap();
        assert_eq!(found.name, "researcher");
        assert_eq!(found.agentid, agentid);
    }

    #[test]
    fn actor_attribution_on_sessions() {
        use daemonclaw_infra::session_backend::SessionBackend;
        use daemonclaw_infra::session_sqlite::SqliteSessionBackend;
        use daemonclaw_providers::ChatMessage;

        let tmp = TempDir::new().unwrap();
        let backend = SqliteSessionBackend::new(tmp.path()).unwrap();

        let agentid_a = "agent-aaa";
        let agentid_b = "agent-bbb";

        // Actor A writes to its session
        backend
            .append_with_actor(
                agentid_a,
                &ChatMessage::assistant("Result from A".to_string()),
                Some(agentid_a),
                Some("pool_member"),
            )
            .unwrap();

        // Actor B writes to its session
        backend
            .append_with_actor(
                agentid_b,
                &ChatMessage::assistant("Result from B".to_string()),
                Some(agentid_b),
                Some("pool_member"),
            )
            .unwrap();

        // Main agent queries each member's session
        let history_a = backend.load(agentid_a);
        assert_eq!(history_a.len(), 1);
        assert_eq!(history_a[0].content, "Result from A");

        let history_b = backend.load(agentid_b);
        assert_eq!(history_b.len(), 1);
        assert_eq!(history_b[0].content, "Result from B");
    }

    #[test]
    fn destruction_purges_pool_member_row() {
        use daemonclaw_infra::session_backend::SessionBackend;
        use daemonclaw_infra::session_sqlite::SqliteSessionBackend;
        use daemonclaw_providers::ChatMessage;

        let tmp = TempDir::new().unwrap();
        let store = PoolStore::open(tmp.path()).unwrap();
        let backend = SqliteSessionBackend::new(tmp.path()).unwrap();

        let member = store
            .spawn_member("doomed", None, "zai", "glm-5.1", &[])
            .unwrap();
        backend
            .append_with_actor(
                &member.agentid,
                &ChatMessage::user("hello".to_string()),
                Some(&member.agentid),
                Some("pool_member"),
            )
            .unwrap();

        // Destroy member
        assert!(store.destroy_member(&member.agentid).unwrap());
        assert!(store.get_member(&member.agentid).unwrap().is_none());

        // Sessions also cleaned up by session backend
        let _ = backend.delete_session(&member.agentid);
        let history = backend.load(&member.agentid);
        assert!(history.is_empty(), "sessions must be purged after destruction");
    }
}
