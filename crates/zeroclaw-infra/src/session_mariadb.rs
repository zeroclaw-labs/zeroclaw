//! MariaDB backend for the multi-database session-persistence series.
//!
//! `MariaDbSessionBackend` is a thin distinct-type wrapper
//! around [`crate::session_mysql_shared::MySqlBackend`]
//! parameterised on the
//! [`crate::session_mysql_shared::MariaDbTag`] marker. MariaDB
//! speaks the same wire protocol as MySQL and accepts the same
//! SQL for every operation we issue (CREATE TABLE / INSERT /
//! SELECT / MATCH … AGAINST … / SHOW VARIABLES), so the actual
//! implementation lives once in the shared module — see the
//! engine-divergence notes there for the handful of places the
//! two engines' SQL trivially differs (and where we deliberately
//! keep the SQL identical so a future MySQL↔MariaDB migration
//! is just a backend-name swap).
//!
//! Connection URL resolution (in priority order):
//! 1. `ZEROCLAW_channels__mariadb_url` — the canonical config
//!    injection point documented on the
//!    `ChannelsConfig.mariadb_url` field.
//! 2. `ZEROCLAW_TEST_MARIADB_URL` — a manual escape hatch used
//!    by the live-DB integration tests in this module.

use crate::session_backend::SessionBackend;
use crate::session_mysql_shared::EngineTag;

/// Synchronous, blocking MariaDB session backend. Wraps the same
/// `mysql::Pool` and re-exports the shared `SessionBackend`
/// implementation that lives in `session_mysql_shared`. Distinct
/// from [`crate::session_mysql::MySqlSessionBackend`] only in
/// engine tag (for log / error messages) and in the
/// connection-URL env var it reads.
pub struct MariaDbSessionBackend {
    inner: crate::session_mysql_shared::MySqlBackend<crate::session_mysql_shared::MariaDbTag>,
}

impl MariaDbSessionBackend {
    /// Construct a `MariaDbSessionBackend` against the
    /// configured MariaDB URL.
    pub fn new(workspace_dir: &std::path::Path, pool_size: u32) -> std::io::Result<Self> {
        let _ = workspace_dir;
        let url = read_mariadb_url()?.ok_or_else(|| {
            std::io::Error::new(
                std::io::ErrorKind::NotFound,
                "session_backend=mariadb requires ZEROCLAW_channels__mariadb_url \
                 (or ZEROCLAW_TEST_MARIADB_URL in tests) to be set in the \
                 environment. Populate `channels.mariadb_url` in your \
                 config or inject it via the standard dotted-path \
                 env-override — for example: \
                 ZEROCLAW_channels__mariadb_url='mysql://user:pass@host:3306/db' \
                 (MariaDB accepts the mysql:// URL scheme on the wire).",
            )
        })?;
        let inner = crate::session_mysql_shared::MySqlBackend::<
            crate::session_mysql_shared::MariaDbTag,
        >::new_with(
            &url,
            pool_size,
            crate::session_mysql_shared::MariaDbTag::NAME,
        )?;
        Ok(Self { inner })
    }
}

impl SessionBackend for MariaDbSessionBackend {
    fn load(&self, session_key: &str) -> Vec<zeroclaw_api::model_provider::ChatMessage> {
        SessionBackend::load(&self.inner, session_key)
    }

    fn load_with_timestamps(
        &self,
        session_key: &str,
    ) -> Vec<crate::session_backend::TimestampedMessage> {
        SessionBackend::load_with_timestamps(&self.inner, session_key)
    }

    fn append(
        &self,
        session_key: &str,
        message: &zeroclaw_api::model_provider::ChatMessage,
    ) -> std::io::Result<()> {
        SessionBackend::append(&self.inner, session_key, message)
    }

    fn remove_last(&self, session_key: &str) -> std::io::Result<bool> {
        SessionBackend::remove_last(&self.inner, session_key)
    }

    fn update_last(
        &self,
        session_key: &str,
        message: &zeroclaw_api::model_provider::ChatMessage,
    ) -> std::io::Result<bool> {
        SessionBackend::update_last(&self.inner, session_key, message)
    }

    fn list_sessions(&self) -> Vec<String> {
        SessionBackend::list_sessions(&self.inner)
    }

    fn list_sessions_with_metadata(&self) -> Vec<crate::session_backend::SessionMetadata> {
        SessionBackend::list_sessions_with_metadata(&self.inner)
    }

    fn cleanup_stale(&self, ttl_hours: u32) -> std::io::Result<usize> {
        SessionBackend::cleanup_stale(&self.inner, ttl_hours)
    }

    fn clear_messages(&self, session_key: &str) -> std::io::Result<usize> {
        SessionBackend::clear_messages(&self.inner, session_key)
    }

    fn delete_session(&self, session_key: &str) -> std::io::Result<bool> {
        SessionBackend::delete_session(&self.inner, session_key)
    }

    fn clear_agent_attribution(&self, agent_alias: &str) -> std::io::Result<usize> {
        SessionBackend::clear_agent_attribution(&self.inner, agent_alias)
    }

    fn rename_agent_attribution(&self, from: &str, to: &str) -> std::io::Result<usize> {
        SessionBackend::rename_agent_attribution(&self.inner, from, to)
    }

    fn count_agent_attribution(&self, agent_alias: &str) -> std::io::Result<usize> {
        SessionBackend::count_agent_attribution(&self.inner, agent_alias)
    }

    fn session_exists(&self, session_key: &str) -> bool {
        SessionBackend::session_exists(&self.inner, session_key)
    }

    fn set_session_name(&self, session_key: &str, name: &str) -> std::io::Result<()> {
        SessionBackend::set_session_name(&self.inner, session_key, name)
    }

    fn get_session_name(&self, session_key: &str) -> std::io::Result<Option<String>> {
        SessionBackend::get_session_name(&self.inner, session_key)
    }

    fn set_session_agent_alias(&self, session_key: &str, agent_alias: &str) -> std::io::Result<()> {
        SessionBackend::set_session_agent_alias(&self.inner, session_key, agent_alias)
    }

    fn get_session_agent_alias(&self, session_key: &str) -> std::io::Result<Option<String>> {
        SessionBackend::get_session_agent_alias(&self.inner, session_key)
    }

    fn set_session_context(
        &self,
        session_key: &str,
        context: crate::session_backend::SessionContext<'_>,
    ) -> std::io::Result<()> {
        SessionBackend::set_session_context(&self.inner, session_key, context)
    }

    fn get_session_metadata(
        &self,
        session_key: &str,
    ) -> Option<crate::session_backend::SessionMetadata> {
        SessionBackend::get_session_metadata(&self.inner, session_key)
    }

    fn set_session_state(
        &self,
        session_key: &str,
        state: &str,
        turn_id: Option<&str>,
    ) -> std::io::Result<()> {
        SessionBackend::set_session_state(&self.inner, session_key, state, turn_id)
    }

    fn get_session_state(
        &self,
        session_key: &str,
    ) -> std::io::Result<Option<crate::session_backend::SessionState>> {
        SessionBackend::get_session_state(&self.inner, session_key)
    }

    fn list_running_sessions(&self) -> Vec<crate::session_backend::SessionMetadata> {
        SessionBackend::list_running_sessions(&self.inner)
    }

    fn list_stuck_sessions(
        &self,
        threshold_secs: u64,
    ) -> Vec<crate::session_backend::SessionMetadata> {
        SessionBackend::list_stuck_sessions(&self.inner, threshold_secs)
    }

    fn search(
        &self,
        query: &crate::session_backend::SessionQuery,
    ) -> Vec<crate::session_backend::SessionMetadata> {
        SessionBackend::search(&self.inner, query)
    }

    fn compact(&self, session_key: &str) -> std::io::Result<()> {
        SessionBackend::compact(&self.inner, session_key)
    }
}

/// Resolve the MariaDB connection URL from the canonical
/// config-override env var, falling back to the test-only
/// `ZEROCLAW_TEST_MARIADB_URL`.
fn read_mariadb_url() -> std::io::Result<Option<String>> {
    if let Ok(value) = std::env::var("ZEROCLAW_channels__mariadb_url") {
        if value.trim().is_empty() {
            return Err(std::io::Error::new(
                std::io::ErrorKind::InvalidInput,
                "ZEROCLAW_channels__mariadb_url is set but empty; \
                 provide a mysql://user:pass@host:port/db URL.",
            ));
        }
        return Ok(Some(value));
    }
    if let Ok(value) = std::env::var("ZEROCLAW_TEST_MARIADB_URL") {
        if value.trim().is_empty() {
            return Ok(None);
        }
        return Ok(Some(value));
    }
    Ok(None)
}

// ── Live-DB integration tests (PR 2) ─────────────────────────────
//
// Mirrors `session_mysql::live_db_tests` for MariaDB. These
// tests exercise the production code path against a real
// MariaDB server and are gated by `ZEROCLAW_TEST_MARIADB_URL`:
// default `cargo test` skips them (via `#[ignore]`); operators
// who want to run them set the env var and use
// `cargo test -p zeroclaw-infra --features backend-mariadb -- \
//      --include-ignored mariadb_live`.
//
// Each test generates a unique session-key prefix
// (`mariadb_live_<pid>_<nanos>_`) so concurrent CI jobs (or
// reruns against the same operator database) cannot collide,
// and cleans up its own session rows before returning. The
// test bodies are intentionally identical in shape to the
// MySQL live tests — MariaDB accepts the same SQL for every
// operation we issue here, so the live asserts are
// effectively a parallel-run regression on the shared
// implementation.
#[cfg(all(test, feature = "backend-mariadb"))]
mod live_db_tests {
    use super::*;
    use crate::session_backend::SessionBackend;
    use std::sync::atomic::{AtomicU64, Ordering};
    use std::time::{SystemTime, UNIX_EPOCH};
    use zeroclaw_api::model_provider::ChatMessage;

    static UNIQ: AtomicU64 = AtomicU64::new(0);

    fn unique_key(prefix: &str) -> String {
        let nanos = SystemTime::now()
            .duration_since(UNIX_EPOCH)
            .map(|d| d.as_nanos())
            .unwrap_or(0);
        let n = UNIQ.fetch_add(1, Ordering::Relaxed);
        format!(
            "mariadb_live_{}_{}_{nanos}_{n}_{prefix}",
            std::process::id(),
            prefix,
        )
    }

    /// Returns `Some(backend)` when `ZEROCLAW_TEST_MARIADB_URL`
    /// (or `ZEROCLAW_channels__mariadb_url`) is set and a real
    /// MariaDB is reachable; `None` when the env var is unset
    /// (so the test skips cleanly).
    fn maybe_backend() -> Option<MariaDbSessionBackend> {
        read_mariadb_url().ok().flatten()?;
        let pool_size = 2;
        // Tests use a per-invocation TempDir so the
        // `workspace_dir` argument — which the MariaDB backend
        // currently ignores — is hermetic.
        let tmp = tempfile::TempDir::new().expect("TempDir::new");
        match MariaDbSessionBackend::new(tmp.path(), pool_size) {
            Ok(b) => Some(b),
            Err(e) => {
                eprintln!(
                    "skipping mariadb live test: connection failed: {e}. \
                     start a local MariaDB (e.g. `docker run -p 3306:3306 \
                     -e MARIADB_ROOT_PASSWORD=root mariadb:11`) and set \
                     ZEROCLAW_TEST_MARIADB_URL."
                );
                None
            }
        }
    }

    #[test]
    #[ignore = "requires ZEROCLAW_TEST_MARIADB_URL pointing at a real MariaDB server"]
    fn mariadb_live_round_trip() {
        let Some(backend) = maybe_backend() else {
            return;
        };
        let key = unique_key("round_trip");
        backend
            .append(&key, &ChatMessage::user("hello mariadb"))
            .expect("append user");
        backend
            .append(&key, &ChatMessage::assistant("hi there"))
            .expect("append assistant");
        let msgs = backend.load(&key);
        assert_eq!(msgs.len(), 2);
        assert_eq!(msgs[0].role, "user");
        assert_eq!(msgs[1].role, "assistant");
        // Clean up
        let _ = backend.delete_session(&key);
    }

    #[test]
    #[ignore = "requires ZEROCLAW_TEST_MARIADB_URL pointing at a real MariaDB server"]
    fn mariadb_live_metadata_round_trip() {
        let Some(backend) = maybe_backend() else {
            return;
        };
        let key = unique_key("metadata");
        backend
            .append(&key, &ChatMessage::user("a"))
            .expect("append");
        backend
            .append(&key, &ChatMessage::user("b"))
            .expect("append");
        backend
            .append(&key, &ChatMessage::user("c"))
            .expect("append");
        backend.set_session_name(&key, "MariaDB live test").unwrap();
        backend
            .set_session_agent_alias(&key, "live-test-agent")
            .unwrap();
        backend
            .set_session_context(
                &key,
                crate::session_backend::SessionContext {
                    channel_id: Some("discord.live"),
                    room_id: Some("room-1"),
                    sender_id: Some("user-1"),
                },
            )
            .unwrap();

        let meta = backend.get_session_metadata(&key).expect("metadata");
        assert_eq!(meta.key, key);
        assert_eq!(meta.message_count, 3);
        assert_eq!(meta.name.as_deref(), Some("MariaDB live test"));
        assert_eq!(meta.agent_alias.as_deref(), Some("live-test-agent"));
        assert_eq!(meta.channel_id.as_deref(), Some("discord.live"));
        assert_eq!(meta.room_id.as_deref(), Some("room-1"));
        assert_eq!(meta.sender_id.as_deref(), Some("user-1"));

        // Clean up
        let _ = backend.delete_session(&key);
    }

    #[test]
    #[ignore = "requires ZEROCLAW_TEST_MARIADB_URL pointing at a real MariaDB server"]
    fn mariadb_live_fulltext_search() {
        let Some(backend) = maybe_backend() else {
            return;
        };
        let k_match = unique_key("fts_match");
        let k_skip = unique_key("fts_skip");
        backend
            .append(&k_match, &ChatMessage::user("How do I parse JSON in Rust?"))
            .expect("append match");
        backend
            .append(&k_skip, &ChatMessage::user("What is the weather?"))
            .expect("append skip");

        let results = backend.search(&crate::session_backend::SessionQuery {
            keyword: Some("Rust".into()),
            limit: Some(10),
        });
        let keys: Vec<&str> = results.iter().map(|m| m.key.as_str()).collect();
        assert!(
            keys.contains(&k_match.as_str()),
            "FULLTEXT search must return the matching session; got keys: {keys:?}"
        );
        assert!(
            !keys.contains(&k_skip.as_str()),
            "FULLTEXT search must not return the non-matching session; got keys: {keys:?}"
        );

        // Clean up
        let _ = backend.delete_session(&k_match);
        let _ = backend.delete_session(&k_skip);
    }

    #[test]
    #[ignore = "requires ZEROCLAW_TEST_MARIADB_URL pointing at a real MariaDB server"]
    fn mariadb_live_factory_round_trip() {
        // Verifies that when the test URL env var is set, the
        // factory's mariadb arm constructs a real backend that
        // satisfies the full SessionBackend trait via the trait
        // object returned by `make_session_backend`. The other
        // live tests exercise `MariaDbSessionBackend` directly;
        // this one goes through the dispatch factory — the
        // path operators actually hit at startup.
        let Some(_) = read_mariadb_url().ok().flatten() else {
            return;
        };
        let tmp = tempfile::TempDir::new().expect("TempDir::new");
        let backend = match crate::make_session_backend(tmp.path(), "mariadb") {
            Ok(b) => b,
            Err(e) => {
                eprintln!("skipping factory test: {e}");
                return;
            }
        };
        let key = unique_key("factory");
        backend
            .append(&key, &ChatMessage::user("via factory"))
            .expect("append");
        let msgs = backend.load(&key);
        assert_eq!(msgs.len(), 1);
        let _ = backend.delete_session(&key);
    }
}
