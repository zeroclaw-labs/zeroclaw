use crate::providers::ChatMessage;
use crate::{
    config::AgentSessionBackend, config::AgentSessionConfig, config::AgentSessionStrategy,
};
use anyhow::{Context, Result};
use async_trait::async_trait;
use parking_lot::Mutex;
use rusqlite::{params, Connection};
use std::collections::HashMap;
use std::path::{Path, PathBuf};
use std::sync::atomic::{AtomicI64, Ordering};
use std::sync::Arc;
use std::sync::{LazyLock, Mutex as StdMutex};
use std::time::{Duration, SystemTime, UNIX_EPOCH};
use tokio::sync::RwLock;
use tokio::time;

static SHARED_SESSION_MANAGERS: LazyLock<StdMutex<HashMap<String, Arc<dyn SessionManager>>>> =
    LazyLock::new(|| StdMutex::new(HashMap::new()));

pub fn resolve_session_id(
    session_config: &AgentSessionConfig,
    sender_id: &str,
    channel_name: Option<&str>,
) -> String {
    fn escape_part(raw: &str) -> String {
        raw.replace(':', "%3A")
    }

    match session_config.strategy {
        AgentSessionStrategy::Main => "main".to_string(),
        AgentSessionStrategy::PerChannel => escape_part(channel_name.unwrap_or("main")),
        AgentSessionStrategy::PerSender => match channel_name {
            Some(channel) => format!("{}:{sender_id}", escape_part(channel)),
            None => sender_id.to_string(),
        },
    }
}

pub fn create_session_manager(
    session_config: &AgentSessionConfig,
    workspace_dir: &Path,
) -> Result<Option<Arc<dyn SessionManager>>> {
    let ttl = Duration::from_secs(session_config.ttl_seconds);
    let max_messages = session_config.max_messages;
    match session_config.backend {
        AgentSessionBackend::None => Ok(None),
        AgentSessionBackend::Memory => Ok(Some(MemorySessionManager::new(ttl, max_messages))),
        AgentSessionBackend::Sqlite => {
            let path = SqliteSessionManager::default_db_path(workspace_dir);
            Ok(Some(SqliteSessionManager::new(path, ttl, max_messages)?))
        }
    }
}

pub fn shared_session_manager(
    session_config: &AgentSessionConfig,
    workspace_dir: &Path,
) -> Result<Option<Arc<dyn SessionManager>>> {
    let key = format!("{}:{session_config:?}", workspace_dir.display());

    {
        let map = SHARED_SESSION_MANAGERS
            .lock()
            .unwrap_or_else(|e| e.into_inner());
        if let Some(mgr) = map.get(&key) {
            return Ok(Some(mgr.clone()));
        }
    }

    let mgr_opt = create_session_manager(session_config, workspace_dir)?;
    if let Some(mgr) = mgr_opt.as_ref() {
        let mut map = SHARED_SESSION_MANAGERS
            .lock()
            .unwrap_or_else(|e| e.into_inner());
        map.insert(key, mgr.clone());
    }
    Ok(mgr_opt)
}

#[derive(Clone)]
pub struct Session {
    id: String,
    manager: Arc<dyn SessionManager>,
}

impl Session {
    pub fn id(&self) -> &str {
        &self.id
    }

    pub async fn get_history(&self) -> Result<Vec<ChatMessage>> {
        self.manager.get_history(&self.id).await
    }

    pub async fn update_history(&self, history: Vec<ChatMessage>) -> Result<()> {
        self.manager.set_history(&self.id, history).await
    }
}

#[async_trait]
pub trait SessionManager: Send + Sync {
    fn clone_arc(&self) -> Arc<dyn SessionManager>;
    async fn ensure_exists(&self, _session_id: &str) -> Result<()> {
        Ok(())
    }
    async fn get_history(&self, session_id: &str) -> Result<Vec<ChatMessage>>;
    async fn set_history(&self, session_id: &str, history: Vec<ChatMessage>) -> Result<()>;
    async fn delete(&self, session_id: &str) -> Result<()>;
    async fn cleanup_expired(&self) -> Result<usize>;

    async fn get_or_create(&self, session_id: &str) -> Result<Session> {
        self.ensure_exists(session_id).await?;
        Ok(Session {
            id: session_id.to_string(),
            manager: self.clone_arc(),
        })
    }
}

fn unix_seconds_now() -> i64 {
    SystemTime::now()
        .duration_since(UNIX_EPOCH)
        .unwrap_or(Duration::from_secs(0))
        .as_secs() as i64
}

fn trim_non_system(history: &mut Vec<ChatMessage>, max_messages: usize) {
    history.retain(|m| m.role != "system");
    if max_messages == 0 || history.len() <= max_messages {
        return;
    }
    let drop_count = history.len() - max_messages;
    history.drain(0..drop_count);
}

#[derive(Debug)]
struct MemorySessionState {
    history: RwLock<Vec<ChatMessage>>,
    updated_at_unix: AtomicI64,
}

struct MemorySessionManagerInner {
    sessions: RwLock<HashMap<String, Arc<MemorySessionState>>>,
    ttl: Duration,
    max_messages: usize,
}

#[derive(Clone)]
pub struct MemorySessionManager {
    inner: Arc<MemorySessionManagerInner>,
}

impl MemorySessionManager {
    pub fn new(ttl: Duration, max_messages: usize) -> Arc<Self> {
        let mgr = Arc::new(Self {
            inner: Arc::new(MemorySessionManagerInner {
                sessions: RwLock::new(HashMap::new()),
                ttl,
                max_messages,
            }),
        });
        mgr.spawn_cleanup_task();
        mgr
    }

    fn spawn_cleanup_task(self: &Arc<Self>) {
        let mgr = Arc::clone(self);
        let interval = cleanup_interval(mgr.inner.ttl);
        tokio::spawn(async move {
            let mut ticker = time::interval(interval);
            loop {
                ticker.tick().await;
                let _ = mgr.cleanup_expired().await;
            }
        });
    }
}

#[async_trait]
impl SessionManager for MemorySessionManager {
    fn clone_arc(&self) -> Arc<dyn SessionManager> {
        Arc::new(self.clone())
    }

    async fn ensure_exists(&self, session_id: &str) -> Result<()> {
        let mut sessions = self.inner.sessions.write().await;
        if sessions.contains_key(session_id) {
            return Ok(());
        }
        let now = unix_seconds_now();
        sessions.insert(
            session_id.to_string(),
            Arc::new(MemorySessionState {
                history: RwLock::new(Vec::new()),
                updated_at_unix: AtomicI64::new(now),
            }),
        );
        Ok(())
    }

    async fn get_history(&self, session_id: &str) -> Result<Vec<ChatMessage>> {
        let state = {
            let sessions = self.inner.sessions.read().await;
            sessions.get(session_id).cloned()
        };
        let Some(state) = state else {
            return Ok(Vec::new());
        };
        let history = state.history.read().await;
        let mut history = history.clone();
        trim_non_system(&mut history, self.inner.max_messages);
        Ok(history)
    }

    async fn set_history(&self, session_id: &str, mut history: Vec<ChatMessage>) -> Result<()> {
        trim_non_system(&mut history, self.inner.max_messages);
        let now = unix_seconds_now();
        let state = {
            let mut sessions = self.inner.sessions.write().await;
            sessions
                .entry(session_id.to_string())
                .or_insert_with(|| {
                    Arc::new(MemorySessionState {
                        history: RwLock::new(Vec::new()),
                        updated_at_unix: AtomicI64::new(now),
                    })
                })
                .clone()
        };
        state.updated_at_unix.store(now, Ordering::Relaxed);
        let mut stored = state.history.write().await;
        *stored = history;
        Ok(())
    }

    async fn delete(&self, session_id: &str) -> Result<()> {
        let mut sessions = self.inner.sessions.write().await;
        sessions.remove(session_id);
        Ok(())
    }

    async fn cleanup_expired(&self) -> Result<usize> {
        if self.inner.ttl.is_zero() {
            return Ok(0);
        }
        let cutoff = unix_seconds_now() - self.inner.ttl.as_secs() as i64;
        let mut sessions = self.inner.sessions.write().await;
        let before = sessions.len();
        sessions.retain(|_, s| s.updated_at_unix.load(Ordering::Relaxed) >= cutoff);
        Ok(before.saturating_sub(sessions.len()))
    }
}

#[derive(Clone)]
pub struct SqliteSessionManager {
    conn: Arc<Mutex<Connection>>,
    ttl: Duration,
    max_messages: usize,
}

impl SqliteSessionManager {
    pub fn new(db_path: PathBuf, ttl: Duration, max_messages: usize) -> Result<Arc<Self>> {
        if let Some(parent) = db_path.parent() {
            std::fs::create_dir_all(parent)?;
        }
        let conn = Connection::open(&db_path)?;
        conn.execute_batch(
            "PRAGMA journal_mode = WAL;
             PRAGMA synchronous  = NORMAL;",
        )?;
        conn.execute_batch(
            "CREATE TABLE IF NOT EXISTS agent_sessions (
                session_id   TEXT PRIMARY KEY,
                history_json TEXT NOT NULL,
                updated_at   INTEGER NOT NULL
             );
             CREATE INDEX IF NOT EXISTS idx_agent_sessions_updated_at
             ON agent_sessions(updated_at);",
        )?;

        let mgr = Arc::new(Self {
            conn: Arc::new(Mutex::new(conn)),
            ttl,
            max_messages,
        });
        mgr.spawn_cleanup_task();
        Ok(mgr)
    }

    pub fn default_db_path(workspace_dir: &Path) -> PathBuf {
        workspace_dir.join("memory").join("sessions.db")
    }

    fn spawn_cleanup_task(self: &Arc<Self>) {
        let mgr = Arc::clone(self);
        let interval = cleanup_interval(mgr.ttl);
        tokio::spawn(async move {
            let mut ticker = time::interval(interval);
            loop {
                ticker.tick().await;
                let _ = mgr.cleanup_expired().await;
            }
        });
    }

    #[cfg(test)]
    pub async fn force_expire_session(&self, session_id: &str, age: Duration) -> Result<()> {
        let conn = self.conn.clone();
        let session_id = session_id.to_string();
        let age_secs = age.as_secs() as i64;

        tokio::task::spawn_blocking(move || {
            let conn = conn.lock();
            let new_time = unix_seconds_now() - age_secs;
            conn.execute(
                "UPDATE agent_sessions SET updated_at = ?2 WHERE session_id = ?1",
                params![session_id, new_time],
            )?;
            Ok(())
        })
        .await
        .context("SQLite blocking task panicked")?
    }
}

#[async_trait]
impl SessionManager for SqliteSessionManager {
    fn clone_arc(&self) -> Arc<dyn SessionManager> {
        Arc::new(self.clone())
    }

    async fn ensure_exists(&self, session_id: &str) -> Result<()> {
        let now = unix_seconds_now();
        let conn = self.conn.clone();
        let session_id = session_id.to_string();

        tokio::task::spawn_blocking(move || {
            let conn = conn.lock();
            conn.execute(
                "INSERT OR IGNORE INTO agent_sessions(session_id, history_json, updated_at)
                 VALUES(?1, '[]', ?2)",
                params![session_id, now],
            )?;
            Ok(())
        })
        .await
        .context("SQLite blocking task panicked")?
    }

    async fn get_history(&self, session_id: &str) -> Result<Vec<ChatMessage>> {
        let conn = self.conn.clone();
        let session_id = session_id.to_string();
        let max_messages = self.max_messages;

        tokio::task::spawn_blocking(move || {
            let conn = conn.lock();
            let mut stmt =
                conn.prepare("SELECT history_json FROM agent_sessions WHERE session_id = ?1")?;
            let mut rows = stmt.query(params![session_id])?;
            if let Some(row) = rows.next()? {
                let json: String = row.get(0)?;
                let mut history: Vec<ChatMessage> =
                    serde_json::from_str(&json).with_context(|| {
                        format!("Failed to parse session history for session_id={session_id}")
                    })?;
                trim_non_system(&mut history, max_messages);
                return Ok(history);
            }
            Ok(Vec::new())
        })
        .await
        .context("SQLite blocking task panicked")?
    }

    async fn set_history(&self, session_id: &str, mut history: Vec<ChatMessage>) -> Result<()> {
        trim_non_system(&mut history, self.max_messages);
        let json = serde_json::to_string(&history)?;
        let now = unix_seconds_now();
        let conn = self.conn.clone();
        let session_id = session_id.to_string();

        tokio::task::spawn_blocking(move || {
            let conn = conn.lock();
            conn.execute(
                "INSERT INTO agent_sessions(session_id, history_json, updated_at)
                 VALUES(?1, ?2, ?3)
                 ON CONFLICT(session_id) DO UPDATE SET history_json=excluded.history_json, updated_at=excluded.updated_at",
                params![session_id, json, now],
            )?;
            Ok(())
        })
        .await
        .context("SQLite blocking task panicked")?
    }

    async fn delete(&self, session_id: &str) -> Result<()> {
        let conn = self.conn.clone();
        let session_id = session_id.to_string();

        tokio::task::spawn_blocking(move || {
            let conn = conn.lock();
            conn.execute(
                "DELETE FROM agent_sessions WHERE session_id = ?1",
                params![session_id],
            )?;
            Ok(())
        })
        .await
        .context("SQLite blocking task panicked")?
    }

    async fn cleanup_expired(&self) -> Result<usize> {
        if self.ttl.is_zero() {
            return Ok(0);
        }
        let conn = self.conn.clone();
        let ttl_secs = self.ttl.as_secs() as i64;

        tokio::task::spawn_blocking(move || {
            let cutoff = unix_seconds_now() - ttl_secs;
            let conn = conn.lock();
            let removed = conn.execute(
                "DELETE FROM agent_sessions WHERE updated_at < ?1",
                params![cutoff],
            )?;
            Ok(removed)
        })
        .await
        .context("SQLite blocking task panicked")?
    }
}

fn cleanup_interval(ttl: Duration) -> Duration {
    if ttl.is_zero() {
        return Duration::from_secs(60);
    }
    let half = ttl / 2;
    if half < Duration::from_secs(30) {
        Duration::from_secs(30)
    } else if half > Duration::from_secs(300) {
        Duration::from_secs(300)
    } else {
        half
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn resolve_session_id_respects_strategy() {
        let mut cfg = AgentSessionConfig::default();
        cfg.strategy = AgentSessionStrategy::Main;
        assert_eq!(resolve_session_id(&cfg, "u1", Some("whatsapp")), "main");

        cfg.strategy = AgentSessionStrategy::PerChannel;
        assert_eq!(resolve_session_id(&cfg, "u1", Some("whatsapp")), "whatsapp");
        assert_eq!(resolve_session_id(&cfg, "u1", None), "main");

        cfg.strategy = AgentSessionStrategy::PerSender;
        assert_eq!(
            resolve_session_id(&cfg, "u1", Some("whatsapp")),
            "whatsapp:u1"
        );
        assert_eq!(resolve_session_id(&cfg, "u1", None), "u1");

        assert_eq!(
            resolve_session_id(&cfg, "u1", Some("matrix:@alice")),
            "matrix%3A@alice:u1"
        );
    }

    #[tokio::test]
    async fn memory_session_accumulates_history() -> Result<()> {
        let mgr = MemorySessionManager::new(Duration::from_secs(3600), 50);
        let session = mgr.get_or_create("s1").await?;

        assert!(session.get_history().await?.is_empty());

        session
            .update_history(vec![ChatMessage::user("hi"), ChatMessage::assistant("ok")])
            .await?;
        assert_eq!(session.get_history().await?.len(), 2);

        let mut h = session.get_history().await?;
        h.push(ChatMessage::user("again"));
        h.push(ChatMessage::assistant("ok2"));
        session.update_history(h).await?;
        assert_eq!(session.get_history().await?.len(), 4);
        Ok(())
    }

    #[tokio::test]
    async fn memory_sessions_do_not_mix_histories() -> Result<()> {
        let mgr = MemorySessionManager::new(Duration::from_secs(3600), 50);
        let a = mgr.get_or_create("a").await?;
        let b = mgr.get_or_create("b").await?;

        a.update_history(vec![ChatMessage::user("u1"), ChatMessage::assistant("a1")])
            .await?;
        b.update_history(vec![ChatMessage::user("u2"), ChatMessage::assistant("b1")])
            .await?;

        let ha = a.get_history().await?;
        let hb = b.get_history().await?;
        assert_eq!(ha[0].content, "u1");
        assert_eq!(hb[0].content, "u2");
        Ok(())
    }

    #[tokio::test]
    async fn max_messages_trims_oldest_non_system() -> Result<()> {
        let mgr = MemorySessionManager::new(Duration::from_secs(3600), 2);
        let session = mgr.get_or_create("s1").await?;
        session
            .update_history(vec![
                ChatMessage::system("s"),
                ChatMessage::user("1"),
                ChatMessage::assistant("2"),
                ChatMessage::user("3"),
                ChatMessage::assistant("4"),
            ])
            .await?;
        let h = session.get_history().await?;
        assert_eq!(h.len(), 2);
        assert_eq!(h[0].content, "3");
        assert_eq!(h[1].content, "4");
        Ok(())
    }

    #[tokio::test]
    async fn sqlite_session_persists_across_instances() -> Result<()> {
        let dir = tempfile::tempdir()?;
        let db_path = dir.path().join("sessions.db");

        {
            let mgr = SqliteSessionManager::new(db_path.clone(), Duration::from_secs(3600), 50)?;
            let session = mgr.get_or_create("s1").await?;
            session
                .update_history(vec![ChatMessage::user("hi"), ChatMessage::assistant("ok")])
                .await?;
        }

        let mgr2 = SqliteSessionManager::new(db_path, Duration::from_secs(3600), 50)?;
        let session2 = mgr2.get_or_create("s1").await?;
        let history = session2.get_history().await?;
        assert_eq!(history.len(), 2);
        assert_eq!(history[0].role, "user");
        assert_eq!(history[1].role, "assistant");
        Ok(())
    }

    #[tokio::test]
    async fn sqlite_session_cleanup_expires() -> Result<()> {
        let dir = tempfile::tempdir()?;
        let db_path = dir.path().join("sessions.db");
        // TTL 1 second
        let mgr = SqliteSessionManager::new(db_path, Duration::from_secs(1), 50)?;
        let session = mgr.get_or_create("s1").await?;
        session
            .update_history(vec![ChatMessage::user("hi"), ChatMessage::assistant("ok")])
            .await?;

        // Force expire by setting age to 2 seconds
        mgr.force_expire_session("s1", Duration::from_secs(2))
            .await?;

        let removed = mgr.cleanup_expired().await?;
        if removed == 0 {
            let history = mgr.get_history("s1").await?;
            assert!(
                history.is_empty(),
                "expired session should already be gone when explicit cleanup removes 0 rows"
            );
        } else {
            assert!(removed >= 1);
        }
        Ok(())
    }
}
