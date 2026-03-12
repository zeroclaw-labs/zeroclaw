use anyhow::{Context, Result};
use parking_lot::Mutex;
use rusqlite::Connection;
use serde::{Deserialize, Serialize};
use std::path::Path;
use std::sync::Arc;

#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct Bot {
    pub id: String,
    pub name: String,
    pub host: String,
    pub port: u16,
    pub status: String,
    pub version: String,
    pub last_heartbeat: String,
    pub channels: String,
    pub provider: String,
    pub memory_backend: String,
    pub uptime_secs: i64,
    pub registered_at: String,
}

#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct Command {
    pub id: String,
    pub bot_id: String,
    pub kind: String,
    pub payload: String,
    pub status: String,
    pub created_by: String,
    pub created_at: String,
    pub acked_at: Option<String>,
    pub result: Option<String>,
    pub requires_approval: bool,
}

#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct Approval {
    pub id: String,
    pub command_id: String,
    pub status: String,
    pub reviewer: String,
    pub reviewed_at: Option<String>,
    pub reason: Option<String>,
}

#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct AuditEntry {
    pub id: i64,
    pub actor: String,
    pub action: String,
    pub target: String,
    pub detail: String,
    pub timestamp: String,
}

#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct Event {
    pub id: i64,
    pub bot_id: String,
    pub kind: String,
    pub payload: String,
    pub timestamp: String,
}

pub struct ControlStore {
    db: Arc<Mutex<Connection>>,
}

impl ControlStore {
    pub fn open(workspace: &Path) -> Result<Self> {
        let db_path = workspace.join("control.db");
        let conn = Connection::open(&db_path)
            .with_context(|| format!("open control store at {}", db_path.display()))?;
        conn.execute_batch("PRAGMA journal_mode=WAL; PRAGMA foreign_keys=ON;")?;
        let store = Self {
            db: Arc::new(Mutex::new(conn)),
        };
        store.migrate()?;
        Ok(store)
    }

    fn migrate(&self) -> Result<()> {
        let db = self.db.lock();
        db.execute_batch(
            "CREATE TABLE IF NOT EXISTS bots (
                id TEXT PRIMARY KEY,
                name TEXT NOT NULL,
                host TEXT NOT NULL DEFAULT '',
                port INTEGER NOT NULL DEFAULT 3000,
                status TEXT NOT NULL DEFAULT 'unknown',
                version TEXT NOT NULL DEFAULT '',
                last_heartbeat TEXT NOT NULL DEFAULT '',
                channels TEXT NOT NULL DEFAULT '[]',
                provider TEXT NOT NULL DEFAULT '',
                memory_backend TEXT NOT NULL DEFAULT '',
                uptime_secs INTEGER NOT NULL DEFAULT 0,
                registered_at TEXT NOT NULL DEFAULT (datetime('now'))
            );
            CREATE TABLE IF NOT EXISTS events (
                id INTEGER PRIMARY KEY AUTOINCREMENT,
                bot_id TEXT NOT NULL,
                kind TEXT NOT NULL,
                payload TEXT NOT NULL DEFAULT '{}',
                timestamp TEXT NOT NULL DEFAULT (datetime('now'))
            );
            CREATE INDEX IF NOT EXISTS idx_events_bot ON events(bot_id);
            CREATE INDEX IF NOT EXISTS idx_events_ts ON events(timestamp);
            CREATE TABLE IF NOT EXISTS commands (
                id TEXT PRIMARY KEY,
                bot_id TEXT NOT NULL,
                kind TEXT NOT NULL,
                payload TEXT NOT NULL DEFAULT '{}',
                status TEXT NOT NULL DEFAULT 'pending',
                created_by TEXT NOT NULL DEFAULT 'admin',
                created_at TEXT NOT NULL DEFAULT (datetime('now')),
                acked_at TEXT,
                result TEXT,
                requires_approval INTEGER NOT NULL DEFAULT 0
            );
            CREATE INDEX IF NOT EXISTS idx_commands_bot ON commands(bot_id);
            CREATE INDEX IF NOT EXISTS idx_commands_status ON commands(status);
            CREATE TABLE IF NOT EXISTS approvals (
                id TEXT PRIMARY KEY,
                command_id TEXT NOT NULL UNIQUE,
                status TEXT NOT NULL DEFAULT 'pending',
                reviewer TEXT NOT NULL DEFAULT '',
                reviewed_at TEXT,
                reason TEXT,
                FOREIGN KEY (command_id) REFERENCES commands(id)
            );
            CREATE TABLE IF NOT EXISTS audit_log (
                id INTEGER PRIMARY KEY AUTOINCREMENT,
                actor TEXT NOT NULL,
                action TEXT NOT NULL,
                target TEXT NOT NULL DEFAULT '',
                detail TEXT NOT NULL DEFAULT '',
                timestamp TEXT NOT NULL DEFAULT (datetime('now'))
            );
            CREATE INDEX IF NOT EXISTS idx_audit_ts ON audit_log(timestamp);",
        )?;
        Ok(())
    }

    pub fn upsert_bot(&self, bot: &Bot) -> Result<()> {
        let db = self.db.lock();
        db.execute(
            "INSERT INTO bots (id, name, host, port, status, version, last_heartbeat, channels, provider, memory_backend, uptime_secs, registered_at)
             VALUES (?1, ?2, ?3, ?4, ?5, ?6, ?7, ?8, ?9, ?10, ?11, ?12)
             ON CONFLICT(id) DO UPDATE SET
               name=excluded.name, host=excluded.host, port=excluded.port,
               status=excluded.status, version=excluded.version,
               last_heartbeat=excluded.last_heartbeat, channels=excluded.channels,
               provider=excluded.provider, memory_backend=excluded.memory_backend,
               uptime_secs=excluded.uptime_secs",
            rusqlite::params![
                bot.id, bot.name, bot.host, bot.port, bot.status, bot.version,
                bot.last_heartbeat, bot.channels, bot.provider, bot.memory_backend,
                bot.uptime_secs, bot.registered_at,
            ],
        )?;
        Ok(())
    }

    pub fn list_bots(&self) -> Result<Vec<Bot>> {
        let db = self.db.lock();
        let mut stmt = db.prepare(
            "SELECT id, name, host, port, status, version, last_heartbeat, channels, provider, memory_backend, uptime_secs, registered_at FROM bots ORDER BY name"
        )?;
        let rows = stmt.query_map([], |row| {
            Ok(Bot {
                id: row.get(0)?,
                name: row.get(1)?,
                host: row.get(2)?,
                port: row.get(3)?,
                status: row.get(4)?,
                version: row.get(5)?,
                last_heartbeat: row.get(6)?,
                channels: row.get(7)?,
                provider: row.get(8)?,
                memory_backend: row.get(9)?,
                uptime_secs: row.get(10)?,
                registered_at: row.get(11)?,
            })
        })?;
        let mut bots = Vec::new();
        for row in rows {
            bots.push(row?);
        }
        Ok(bots)
    }

    pub fn get_bot(&self, id: &str) -> Result<Option<Bot>> {
        let db = self.db.lock();
        let mut stmt = db.prepare(
            "SELECT id, name, host, port, status, version, last_heartbeat, channels, provider, memory_backend, uptime_secs, registered_at FROM bots WHERE id = ?1"
        )?;
        let mut rows = stmt.query_map(rusqlite::params![id], |row| {
            Ok(Bot {
                id: row.get(0)?,
                name: row.get(1)?,
                host: row.get(2)?,
                port: row.get(3)?,
                status: row.get(4)?,
                version: row.get(5)?,
                last_heartbeat: row.get(6)?,
                channels: row.get(7)?,
                provider: row.get(8)?,
                memory_backend: row.get(9)?,
                uptime_secs: row.get(10)?,
                registered_at: row.get(11)?,
            })
        })?;
        match rows.next() {
            Some(Ok(bot)) => Ok(Some(bot)),
            _ => Ok(None),
        }
    }

    pub fn delete_bot(&self, id: &str) -> Result<bool> {
        let db = self.db.lock();
        let changed = db.execute("DELETE FROM bots WHERE id = ?1", rusqlite::params![id])?;
        Ok(changed > 0)
    }

    pub fn insert_event(&self, bot_id: &str, kind: &str, payload: &str) -> Result<i64> {
        let db = self.db.lock();
        db.execute(
            "INSERT INTO events (bot_id, kind, payload) VALUES (?1, ?2, ?3)",
            rusqlite::params![bot_id, kind, payload],
        )?;
        Ok(db.last_insert_rowid())
    }

    pub fn list_events(&self, bot_id: Option<&str>, limit: i64) -> Result<Vec<Event>> {
        let db = self.db.lock();
        let (sql, params): (&str, Vec<Box<dyn rusqlite::types::ToSql>>) = match bot_id {
            Some(bid) => (
                "SELECT id, bot_id, kind, payload, timestamp FROM events WHERE bot_id = ?1 ORDER BY id DESC LIMIT ?2",
                vec![Box::new(bid.to_string()), Box::new(limit)],
            ),
            None => (
                "SELECT id, bot_id, kind, payload, timestamp FROM events ORDER BY id DESC LIMIT ?1",
                vec![Box::new(limit)],
            ),
        };
        let mut stmt = db.prepare(sql)?;
        let rows = stmt.query_map(rusqlite::params_from_iter(params.iter()), |row| {
            Ok(Event {
                id: row.get(0)?,
                bot_id: row.get(1)?,
                kind: row.get(2)?,
                payload: row.get(3)?,
                timestamp: row.get(4)?,
            })
        })?;
        let mut events = Vec::new();
        for row in rows {
            events.push(row?);
        }
        Ok(events)
    }

    pub fn insert_command(&self, cmd: &Command) -> Result<()> {
        let db = self.db.lock();
        db.execute(
            "INSERT INTO commands (id, bot_id, kind, payload, status, created_by, requires_approval) VALUES (?1, ?2, ?3, ?4, ?5, ?6, ?7)",
            rusqlite::params![cmd.id, cmd.bot_id, cmd.kind, cmd.payload, cmd.status, cmd.created_by, i32::from(cmd.requires_approval)],
        )?;
        Ok(())
    }

    pub fn list_commands(
        &self,
        bot_id: Option<&str>,
        status: Option<&str>,
        limit: i64,
    ) -> Result<Vec<Command>> {
        let db = self.db.lock();
        let mut sql = String::from("SELECT id, bot_id, kind, payload, status, created_by, created_at, acked_at, result, requires_approval FROM commands");
        let mut conditions = Vec::new();
        let mut params: Vec<Box<dyn rusqlite::types::ToSql>> = Vec::new();
        let mut idx = 1;
        if let Some(bid) = bot_id {
            conditions.push(format!("bot_id = ?{idx}"));
            params.push(Box::new(bid.to_string()));
            idx += 1;
        }
        if let Some(st) = status {
            conditions.push(format!("status = ?{idx}"));
            params.push(Box::new(st.to_string()));
            idx += 1;
        }
        if !conditions.is_empty() {
            sql.push_str(" WHERE ");
            sql.push_str(&conditions.join(" AND "));
        }
        use std::fmt::Write;
        let _ = write!(sql, " ORDER BY created_at DESC LIMIT ?{idx}");
        params.push(Box::new(limit));

        let mut stmt = db.prepare(&sql)?;
        let rows = stmt.query_map(rusqlite::params_from_iter(params.iter()), |row| {
            Ok(Command {
                id: row.get(0)?,
                bot_id: row.get(1)?,
                kind: row.get(2)?,
                payload: row.get(3)?,
                status: row.get(4)?,
                created_by: row.get(5)?,
                created_at: row.get(6)?,
                acked_at: row.get(7)?,
                result: row.get(8)?,
                requires_approval: row.get::<_, i32>(9)? != 0,
            })
        })?;
        let mut cmds = Vec::new();
        for row in rows {
            cmds.push(row?);
        }
        Ok(cmds)
    }

    pub fn update_command_status(
        &self,
        id: &str,
        status: &str,
        result: Option<&str>,
    ) -> Result<bool> {
        let db = self.db.lock();
        let acked = if status == "acked" || status == "failed" {
            Some(chrono::Utc::now().format("%Y-%m-%d %H:%M:%S").to_string())
        } else {
            None
        };
        let changed = db.execute(
            "UPDATE commands SET status = ?1, acked_at = COALESCE(?2, acked_at), result = COALESCE(?3, result) WHERE id = ?4",
            rusqlite::params![status, acked, result, id],
        )?;
        Ok(changed > 0)
    }

    pub fn get_pending_commands(&self, bot_id: &str) -> Result<Vec<Command>> {
        self.list_commands(Some(bot_id), Some("approved"), 100)
    }

    pub fn insert_approval(&self, approval: &Approval) -> Result<()> {
        let db = self.db.lock();
        db.execute(
            "INSERT INTO approvals (id, command_id, status, reviewer) VALUES (?1, ?2, ?3, ?4)",
            rusqlite::params![
                approval.id,
                approval.command_id,
                approval.status,
                approval.reviewer
            ],
        )?;
        Ok(())
    }

    pub fn update_approval(
        &self,
        command_id: &str,
        status: &str,
        reviewer: &str,
        reason: Option<&str>,
    ) -> Result<bool> {
        let db = self.db.lock();
        let now = chrono::Utc::now().format("%Y-%m-%d %H:%M:%S").to_string();
        let changed = db.execute(
            "UPDATE approvals SET status = ?1, reviewer = ?2, reviewed_at = ?3, reason = ?4 WHERE command_id = ?5",
            rusqlite::params![status, reviewer, now, reason, command_id],
        )?;
        if changed > 0 && status == "approved" {
            db.execute(
                "UPDATE commands SET status = 'approved' WHERE id = ?1 AND status = 'pending_approval'",
                rusqlite::params![command_id],
            )?;
        } else if changed > 0 && status == "rejected" {
            db.execute(
                "UPDATE commands SET status = 'rejected' WHERE id = ?1",
                rusqlite::params![command_id],
            )?;
        }
        Ok(changed > 0)
    }

    pub fn list_approvals(&self, status: Option<&str>, limit: i64) -> Result<Vec<Approval>> {
        let db = self.db.lock();
        let (sql, params): (&str, Vec<Box<dyn rusqlite::types::ToSql>>) = match status {
            Some(st) => (
                "SELECT a.id, a.command_id, a.status, a.reviewer, a.reviewed_at, a.reason FROM approvals a ORDER BY a.rowid DESC LIMIT ?2",
                vec![Box::new(st.to_string()), Box::new(limit)],
            ),
            None => (
                "SELECT id, command_id, status, reviewer, reviewed_at, reason FROM approvals ORDER BY rowid DESC LIMIT ?1",
                vec![Box::new(limit)],
            ),
        };
        let sql_final = if status.is_some() {
            "SELECT id, command_id, status, reviewer, reviewed_at, reason FROM approvals WHERE status = ?1 ORDER BY rowid DESC LIMIT ?2"
        } else {
            sql
        };
        let mut stmt = db.prepare(sql_final)?;
        let rows = stmt.query_map(rusqlite::params_from_iter(params.iter()), |row| {
            Ok(Approval {
                id: row.get(0)?,
                command_id: row.get(1)?,
                status: row.get(2)?,
                reviewer: row.get(3)?,
                reviewed_at: row.get(4)?,
                reason: row.get(5)?,
            })
        })?;
        let mut approvals = Vec::new();
        for row in rows {
            approvals.push(row?);
        }
        Ok(approvals)
    }

    pub fn audit(&self, actor: &str, action: &str, target: &str, detail: &str) -> Result<()> {
        let db = self.db.lock();
        db.execute(
            "INSERT INTO audit_log (actor, action, target, detail) VALUES (?1, ?2, ?3, ?4)",
            rusqlite::params![actor, action, target, detail],
        )?;
        Ok(())
    }

    pub fn mark_stale_bots(&self, stale_secs: i64) -> Result<Vec<String>> {
        let db = self.db.lock();
        let cutoff = (chrono::Utc::now() - chrono::Duration::seconds(stale_secs))
            .format("%Y-%m-%d %H:%M:%S")
            .to_string();
        let mut stmt =
            db.prepare("SELECT id FROM bots WHERE status = 'online' AND last_heartbeat < ?1")?;
        let ids: Vec<String> = stmt
            .query_map(rusqlite::params![cutoff], |row| row.get(0))?
            .filter_map(|r| r.ok())
            .collect();
        if !ids.is_empty() {
            db.execute(
                "UPDATE bots SET status = 'offline' WHERE status = 'online' AND last_heartbeat < ?1",
                rusqlite::params![cutoff],
            )?;
        }
        Ok(ids)
    }

    pub fn list_audit(&self, limit: i64) -> Result<Vec<AuditEntry>> {
        let db = self.db.lock();
        let mut stmt = db.prepare(
            "SELECT id, actor, action, target, detail, timestamp FROM audit_log ORDER BY id DESC LIMIT ?1"
        )?;
        let rows = stmt.query_map(rusqlite::params![limit], |row| {
            Ok(AuditEntry {
                id: row.get(0)?,
                actor: row.get(1)?,
                action: row.get(2)?,
                target: row.get(3)?,
                detail: row.get(4)?,
                timestamp: row.get(5)?,
            })
        })?;
        let mut entries = Vec::new();
        for row in rows {
            entries.push(row?);
        }
        Ok(entries)
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use tempfile::TempDir;

    fn test_store() -> (ControlStore, TempDir) {
        let tmp = TempDir::new().unwrap();
        let store = ControlStore::open(tmp.path()).unwrap();
        (store, tmp)
    }

    fn sample_bot(id: &str) -> Bot {
        Bot {
            id: id.to_string(),
            name: format!("bot_{id}"),
            host: "127.0.0.1".to_string(),
            port: 3000,
            status: "online".to_string(),
            version: "0.2.0".to_string(),
            last_heartbeat: chrono::Utc::now().format("%Y-%m-%d %H:%M:%S").to_string(),
            channels: "[]".to_string(),
            provider: "openrouter".to_string(),
            memory_backend: "sqlite".to_string(),
            uptime_secs: 100,
            registered_at: chrono::Utc::now().format("%Y-%m-%d %H:%M:%S").to_string(),
        }
    }

    fn sample_command(id: &str, bot_id: &str) -> Command {
        Command {
            id: id.to_string(),
            bot_id: bot_id.to_string(),
            kind: "restart".to_string(),
            payload: "{}".to_string(),
            status: "pending".to_string(),
            created_by: "admin".to_string(),
            created_at: String::new(),
            acked_at: None,
            result: None,
            requires_approval: false,
        }
    }

    fn sample_approval(id: &str, command_id: &str) -> Approval {
        Approval {
            id: id.to_string(),
            command_id: command_id.to_string(),
            status: "pending".to_string(),
            reviewer: "admin".to_string(),
            reviewed_at: None,
            reason: None,
        }
    }

    #[test]
    fn open_creates_tables() {
        let (store, _tmp) = test_store();
        let bots = store.list_bots().unwrap();
        assert!(bots.is_empty());
    }

    #[test]
    fn upsert_and_get_bot_roundtrip() {
        let (store, _tmp) = test_store();
        let bot = sample_bot("b1");
        store.upsert_bot(&bot).unwrap();

        let fetched = store.get_bot("b1").unwrap().unwrap();
        assert_eq!(fetched.id, "b1");
        assert_eq!(fetched.name, "bot_b1");
        assert_eq!(fetched.port, 3000);
    }

    #[test]
    fn upsert_bot_updates_existing() {
        let (store, _tmp) = test_store();
        let mut bot = sample_bot("b1");
        store.upsert_bot(&bot).unwrap();

        bot.status = "offline".to_string();
        bot.uptime_secs = 999;
        store.upsert_bot(&bot).unwrap();

        let fetched = store.get_bot("b1").unwrap().unwrap();
        assert_eq!(fetched.status, "offline");
        assert_eq!(fetched.uptime_secs, 999);

        assert_eq!(store.list_bots().unwrap().len(), 1);
    }

    #[test]
    fn list_bots_ordered_by_name() {
        let (store, _tmp) = test_store();
        let mut b2 = sample_bot("b2");
        b2.name = "zulu".to_string();
        let mut b1 = sample_bot("b1");
        b1.name = "alpha".to_string();
        store.upsert_bot(&b2).unwrap();
        store.upsert_bot(&b1).unwrap();

        let bots = store.list_bots().unwrap();
        assert_eq!(bots[0].name, "alpha");
        assert_eq!(bots[1].name, "zulu");
    }

    #[test]
    fn get_bot_missing_returns_none() {
        let (store, _tmp) = test_store();
        assert!(store.get_bot("nonexistent").unwrap().is_none());
    }

    #[test]
    fn delete_bot_removes_entry() {
        let (store, _tmp) = test_store();
        store.upsert_bot(&sample_bot("b1")).unwrap();
        assert!(store.delete_bot("b1").unwrap());
        assert!(store.get_bot("b1").unwrap().is_none());
    }

    #[test]
    fn delete_bot_nonexistent_returns_false() {
        let (store, _tmp) = test_store();
        assert!(!store.delete_bot("nope").unwrap());
    }

    #[test]
    fn insert_and_list_events() {
        let (store, _tmp) = test_store();
        let id1 = store.insert_event("b1", "started", "{}").unwrap();
        let id2 = store
            .insert_event("b1", "error", r#"{"msg":"oops"}"#)
            .unwrap();
        assert!(id2 > id1);

        let events = store.list_events(Some("b1"), 10).unwrap();
        assert_eq!(events.len(), 2);
        assert_eq!(events[0].kind, "error");
    }

    #[test]
    fn list_events_filters_by_bot() {
        let (store, _tmp) = test_store();
        store.insert_event("b1", "start", "{}").unwrap();
        store.insert_event("b2", "start", "{}").unwrap();

        let b1_events = store.list_events(Some("b1"), 10).unwrap();
        assert_eq!(b1_events.len(), 1);

        let all_events = store.list_events(None, 10).unwrap();
        assert_eq!(all_events.len(), 2);
    }

    #[test]
    fn list_events_respects_limit() {
        let (store, _tmp) = test_store();
        for i in 0..5 {
            store.insert_event("b1", &format!("ev{i}"), "{}").unwrap();
        }
        let events = store.list_events(None, 2).unwrap();
        assert_eq!(events.len(), 2);
    }

    #[test]
    fn insert_and_list_commands() {
        let (store, _tmp) = test_store();
        let cmd = sample_command("c1", "b1");
        store.insert_command(&cmd).unwrap();

        let cmds = store.list_commands(Some("b1"), None, 10).unwrap();
        assert_eq!(cmds.len(), 1);
        assert_eq!(cmds[0].kind, "restart");
    }

    #[test]
    fn list_commands_filters_by_status() {
        let (store, _tmp) = test_store();
        store.insert_command(&sample_command("c1", "b1")).unwrap();
        store
            .insert_command(&Command {
                status: "acked".to_string(),
                ..sample_command("c2", "b1")
            })
            .unwrap();

        let pending = store.list_commands(None, Some("pending"), 10).unwrap();
        assert_eq!(pending.len(), 1);
        assert_eq!(pending[0].id, "c1");

        let acked = store.list_commands(None, Some("acked"), 10).unwrap();
        assert_eq!(acked.len(), 1);
        assert_eq!(acked[0].id, "c2");
    }

    #[test]
    fn list_commands_combined_filters() {
        let (store, _tmp) = test_store();
        store.insert_command(&sample_command("c1", "b1")).unwrap();
        store.insert_command(&sample_command("c2", "b2")).unwrap();

        let result = store
            .list_commands(Some("b1"), Some("pending"), 10)
            .unwrap();
        assert_eq!(result.len(), 1);
        assert_eq!(result[0].id, "c1");
    }

    #[test]
    fn update_command_status_sets_acked_at() {
        let (store, _tmp) = test_store();
        store.insert_command(&sample_command("c1", "b1")).unwrap();

        assert!(store
            .update_command_status("c1", "acked", Some("ok"))
            .unwrap());
        let cmds = store.list_commands(None, None, 10).unwrap();
        assert_eq!(cmds[0].status, "acked");
        assert!(cmds[0].acked_at.is_some());
        assert_eq!(cmds[0].result.as_deref(), Some("ok"));
    }

    #[test]
    fn update_command_status_nonexistent_returns_false() {
        let (store, _tmp) = test_store();
        assert!(!store.update_command_status("nope", "acked", None).unwrap());
    }

    #[test]
    fn get_pending_commands_returns_approved_only() {
        let (store, _tmp) = test_store();
        store.insert_command(&sample_command("c1", "b1")).unwrap();
        store
            .insert_command(&Command {
                status: "approved".to_string(),
                ..sample_command("c2", "b1")
            })
            .unwrap();

        let pending = store.get_pending_commands("b1").unwrap();
        assert_eq!(pending.len(), 1);
        assert_eq!(pending[0].id, "c2");
    }

    #[test]
    fn insert_and_list_approvals() {
        let (store, _tmp) = test_store();
        store.insert_command(&sample_command("c1", "b1")).unwrap();
        store.insert_approval(&sample_approval("a1", "c1")).unwrap();

        let approvals = store.list_approvals(None, 10).unwrap();
        assert_eq!(approvals.len(), 1);
        assert_eq!(approvals[0].command_id, "c1");
    }

    #[test]
    fn list_approvals_filters_by_status() {
        let (store, _tmp) = test_store();
        store.insert_command(&sample_command("c1", "b1")).unwrap();
        store.insert_command(&sample_command("c2", "b1")).unwrap();
        store.insert_approval(&sample_approval("a1", "c1")).unwrap();
        store
            .insert_approval(&Approval {
                status: "approved".to_string(),
                ..sample_approval("a2", "c2")
            })
            .unwrap();

        let pending = store.list_approvals(Some("pending"), 10).unwrap();
        assert_eq!(pending.len(), 1);
        assert_eq!(pending[0].id, "a1");
    }

    #[test]
    fn update_approval_approved_cascades_to_command() {
        let (store, _tmp) = test_store();
        store
            .insert_command(&Command {
                status: "pending_approval".to_string(),
                ..sample_command("c1", "b1")
            })
            .unwrap();
        store.insert_approval(&sample_approval("a1", "c1")).unwrap();

        assert!(store
            .update_approval("c1", "approved", "reviewer1", Some("lgtm"))
            .unwrap());

        let cmds = store.list_commands(None, Some("approved"), 10).unwrap();
        assert_eq!(cmds.len(), 1);
        assert_eq!(cmds[0].id, "c1");
    }

    #[test]
    fn update_approval_rejected_cascades_to_command() {
        let (store, _tmp) = test_store();
        store
            .insert_command(&Command {
                status: "pending_approval".to_string(),
                ..sample_command("c1", "b1")
            })
            .unwrap();
        store.insert_approval(&sample_approval("a1", "c1")).unwrap();

        assert!(store
            .update_approval("c1", "rejected", "reviewer1", Some("nope"))
            .unwrap());

        let cmds = store.list_commands(None, Some("rejected"), 10).unwrap();
        assert_eq!(cmds.len(), 1);
    }

    #[test]
    fn audit_and_list_audit() {
        let (store, _tmp) = test_store();
        store
            .audit("admin", "delete_bot", "b1", "removed stale bot")
            .unwrap();
        store
            .audit("system", "heartbeat_timeout", "b2", "")
            .unwrap();

        let entries = store.list_audit(10).unwrap();
        assert_eq!(entries.len(), 2);
        assert_eq!(entries[0].action, "heartbeat_timeout");
        assert_eq!(entries[1].action, "delete_bot");
    }

    #[test]
    fn list_audit_respects_limit() {
        let (store, _tmp) = test_store();
        for i in 0..5 {
            store.audit("a", &format!("act{i}"), "t", "").unwrap();
        }
        assert_eq!(store.list_audit(2).unwrap().len(), 2);
    }

    #[test]
    fn mark_stale_bots_transitions_to_offline() {
        let (store, _tmp) = test_store();
        let old_ts = (chrono::Utc::now() - chrono::Duration::seconds(120))
            .format("%Y-%m-%d %H:%M:%S")
            .to_string();
        let mut bot = sample_bot("stale1");
        bot.last_heartbeat = old_ts;
        store.upsert_bot(&bot).unwrap();

        let fresh = sample_bot("fresh1");
        store.upsert_bot(&fresh).unwrap();

        let stale_ids = store.mark_stale_bots(60).unwrap();
        assert_eq!(stale_ids, vec!["stale1"]);

        let stale = store.get_bot("stale1").unwrap().unwrap();
        assert_eq!(stale.status, "offline");

        let still_online = store.get_bot("fresh1").unwrap().unwrap();
        assert_eq!(still_online.status, "online");
    }

    #[test]
    fn mark_stale_bots_ignores_already_offline() {
        let (store, _tmp) = test_store();
        let old_ts = (chrono::Utc::now() - chrono::Duration::seconds(120))
            .format("%Y-%m-%d %H:%M:%S")
            .to_string();
        let mut bot = sample_bot("b1");
        bot.status = "offline".to_string();
        bot.last_heartbeat = old_ts;
        store.upsert_bot(&bot).unwrap();

        let stale_ids = store.mark_stale_bots(60).unwrap();
        assert!(stale_ids.is_empty());
    }
}
