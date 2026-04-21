//! Sticky case-session registry — pin a user's active "case" (사건/안건)
//! so subsequent forwarded messages and questions stay scoped to one
//! memory namespace.
//!
//! ## Flow
//!
//! 1. User issues `/case start 김OO_2024가합123` in their 1:1 chat with MoA.
//! 2. MoA records `(channel, platform_uid) → "김OO_2024가합123"` in this
//!    store.
//! 3. While the case is active, the gateway derives a `session_id` for
//!    every inbound utterance from the active case label, and passes it to
//!    [`crate::memory::Memory::store`] / `recall` calls so memory is
//!    scoped per-case.
//! 4. `/case end` clears the active case; subsequent messages fall back to
//!    the channel's default session naming.
//!
//! ## Persistence
//!
//! Active-case state is backed by a small SQLite database alongside the
//! channel pairing store. The file lives under `<workspace_dir>/case_sessions.db`
//! in production and opens in-memory for tests (via [`CaseSessionStore::new`]).
//! The schema mirrors [`super::pairing::ChannelPairingStore`] so both
//! subsystems share the same operational contract (file-based, WAL,
//! idempotent schema init, safe restart).
//!
//! The persisted state is intentionally small — one row per
//! `(channel, platform_uid)` — and serves only to survive gateway
//! restarts so an active case keeps its memory scope across reboots.
//! Historical memory entries are already indexed by `session_id` in the
//! main memory store, so there is no data duplication here.

use parking_lot::Mutex;
use std::path::Path;
use std::time::{SystemTime, UNIX_EPOCH};

/// Maximum length of a user-supplied case label (characters).
///
/// Keeps the stored session_id bounded and the chat reply readable.
const MAX_CASE_LABEL_CHARS: usize = 80;

/// An active case session for a `(channel, platform_uid)` pair.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct ActiveCase {
    /// Sanitized identifier used as the memory `session_id` suffix.
    /// Letters, digits, underscore, hyphen only.
    pub case_id: String,
    /// Original user-supplied label, kept for display in `/case current`.
    pub label: String,
    /// When the user started this case (Unix epoch seconds).
    pub started_at: u64,
}

/// SQLite-backed registry of active cases per `(channel, platform_uid)`
/// pair. Backed by an in-memory database in [`new`](Self::new) so tests
/// stay hermetic; production wiring uses [`open`](Self::open) to point
/// at a file under the workspace directory.
#[derive(Debug)]
pub struct CaseSessionStore {
    conn: Mutex<rusqlite::Connection>,
}

impl CaseSessionStore {
    /// Create an in-memory store (for tests).
    pub fn new() -> Self {
        let conn = rusqlite::Connection::open_in_memory()
            .expect("failed to open in-memory SQLite for case session store");
        Self::init_tables(&conn);
        Self {
            conn: Mutex::new(conn),
        }
    }

    /// Open a file-backed store for production use.
    /// Both gateway startup and test fixtures may call this when a real
    /// file needs to survive the process.
    pub fn open(db_path: &Path) -> anyhow::Result<Self> {
        let conn = rusqlite::Connection::open(db_path)?;
        conn.execute_batch("PRAGMA journal_mode = WAL; PRAGMA busy_timeout = 5000;")?;
        Self::init_tables(&conn);
        Ok(Self {
            conn: Mutex::new(conn),
        })
    }

    fn init_tables(conn: &rusqlite::Connection) {
        conn.execute_batch(
            "CREATE TABLE IF NOT EXISTS active_cases (
                channel TEXT NOT NULL,
                platform_uid TEXT NOT NULL,
                case_id TEXT NOT NULL,
                label TEXT NOT NULL,
                started_at INTEGER NOT NULL,
                PRIMARY KEY (channel, platform_uid)
            );
            CREATE INDEX IF NOT EXISTS idx_active_cases_platform_uid
                ON active_cases(platform_uid);",
        )
        .expect("failed to initialize active_cases table");
    }

    /// Start a new active case. Replaces any prior case for the same user.
    /// Returns the canonical `ActiveCase` that was stored, or `Err` when
    /// the label is empty or sanitizes to nothing.
    pub fn start(
        &self,
        channel: &str,
        platform_uid: &str,
        label: &str,
    ) -> anyhow::Result<ActiveCase> {
        let label = label.trim();
        if label.is_empty() {
            anyhow::bail!("case label cannot be empty");
        }

        let truncated_label = truncate_chars(label, MAX_CASE_LABEL_CHARS);
        let case_id = sanitize_case_id(&truncated_label);
        if case_id.is_empty() {
            anyhow::bail!("case label produced no valid identifier characters");
        }

        let active = ActiveCase {
            case_id,
            label: truncated_label,
            started_at: epoch_secs(),
        };

        let conn = self.conn.lock();
        conn.execute(
            "INSERT INTO active_cases (channel, platform_uid, case_id, label, started_at) \
             VALUES (?1, ?2, ?3, ?4, ?5) \
             ON CONFLICT(channel, platform_uid) DO UPDATE SET \
                case_id = excluded.case_id, \
                label = excluded.label, \
                started_at = excluded.started_at",
            rusqlite::params![
                channel,
                platform_uid,
                active.case_id,
                active.label,
                active.started_at as i64,
            ],
        )?;

        Ok(active)
    }

    /// Look up the currently active case, if any.
    pub fn current(&self, channel: &str, platform_uid: &str) -> Option<ActiveCase> {
        let conn = self.conn.lock();
        conn.query_row(
            "SELECT case_id, label, started_at FROM active_cases \
             WHERE channel = ?1 AND platform_uid = ?2",
            rusqlite::params![channel, platform_uid],
            |row| {
                Ok(ActiveCase {
                    case_id: row.get(0)?,
                    label: row.get(1)?,
                    started_at: u64::try_from(row.get::<_, i64>(2)?).unwrap_or(0),
                })
            },
        )
        .ok()
    }

    /// Clear the active case. Returns the case that was cleared, if any.
    pub fn end(&self, channel: &str, platform_uid: &str) -> Option<ActiveCase> {
        let prior = self.current(channel, platform_uid)?;
        let conn = self.conn.lock();
        let _ = conn.execute(
            "DELETE FROM active_cases WHERE channel = ?1 AND platform_uid = ?2",
            rusqlite::params![channel, platform_uid],
        );
        Some(prior)
    }

    /// List active cases for a single user across all their channels.
    /// Used by `/case list` to remind the user where they have active cases.
    pub fn list_for_user(&self, platform_uid: &str) -> Vec<(String, ActiveCase)> {
        let conn = self.conn.lock();
        let mut stmt = match conn.prepare(
            "SELECT channel, case_id, label, started_at FROM active_cases \
             WHERE platform_uid = ?1 ORDER BY started_at ASC",
        ) {
            Ok(s) => s,
            Err(_) => return Vec::new(),
        };
        let rows = stmt
            .query_map(rusqlite::params![platform_uid], |row| {
                Ok((
                    row.get::<_, String>(0)?,
                    ActiveCase {
                        case_id: row.get(1)?,
                        label: row.get(2)?,
                        started_at: u64::try_from(row.get::<_, i64>(3)?).unwrap_or(0),
                    },
                ))
            })
            .ok();
        match rows {
            Some(iter) => iter.filter_map(Result::ok).collect(),
            None => Vec::new(),
        }
    }

    /// Count of active cases across all users/channels. Exposed for
    /// diagnostics and tests — the gateway does not expose this over HTTP.
    pub fn active_count(&self) -> usize {
        let conn = self.conn.lock();
        conn.query_row("SELECT COUNT(*) FROM active_cases", [], |row| {
            row.get::<_, i64>(0)
        })
        .map(|c| usize::try_from(c).unwrap_or(0))
        .unwrap_or(0)
    }
}

impl Default for CaseSessionStore {
    fn default() -> Self {
        Self::new()
    }
}

/// Build the `session_id` to thread through the memory layer.
/// Returns a `<channel>_case_<case_id>` identifier when a case is active;
/// callers fall back to their default session id otherwise
/// (e.g. `gateway_message_session_id` for non-case messages).
pub fn case_session_id(channel: &str, case: &ActiveCase) -> String {
    format!("{channel}_case_{}", case.case_id)
}

/// Sanitize a free-form case label into a memory-friendly identifier.
/// Letters, digits, hyphen, and underscore are kept; everything else is
/// collapsed to underscores; consecutive underscores collapsed to one.
fn sanitize_case_id(label: &str) -> String {
    let mut out = String::with_capacity(label.len());
    let mut last_was_sep = true;
    for ch in label.chars() {
        if ch.is_alphanumeric() || ch == '_' || ch == '-' {
            out.push(ch);
            last_was_sep = false;
        } else if !last_was_sep {
            out.push('_');
            last_was_sep = true;
        }
    }
    while out.ends_with('_') {
        out.pop();
    }
    while out.starts_with('_') {
        out.remove(0);
    }
    out
}

fn truncate_chars(s: &str, max_chars: usize) -> String {
    s.chars().take(max_chars).collect()
}

fn epoch_secs() -> u64 {
    SystemTime::now()
        .duration_since(UNIX_EPOCH)
        .unwrap_or_default()
        .as_secs()
}

#[cfg(test)]
mod tests {
    use super::*;
    use tempfile::tempdir;

    #[test]
    fn start_then_current_returns_case() {
        let store = CaseSessionStore::new();
        let active = store
            .start("kakao", "u_1", "김OO_2024가합123")
            .expect("start should succeed");
        assert_eq!(active.label, "김OO_2024가합123");
        assert!(!active.case_id.is_empty());
        assert_eq!(store.current("kakao", "u_1"), Some(active));
    }

    #[test]
    fn empty_label_rejected() {
        let store = CaseSessionStore::new();
        assert!(store.start("kakao", "u_1", "   ").is_err());
        assert!(store.current("kakao", "u_1").is_none());
    }

    #[test]
    fn label_with_only_separators_rejected() {
        let store = CaseSessionStore::new();
        // Punctuation-only collapses to empty case_id and must error out.
        assert!(store.start("kakao", "u_1", "!!! ??? ...").is_err());
    }

    #[test]
    fn end_clears_case_and_returns_prior() {
        let store = CaseSessionStore::new();
        let started = store.start("kakao", "u_1", "사건1").unwrap();
        let ended = store.end("kakao", "u_1");
        assert_eq!(ended, Some(started));
        assert!(store.current("kakao", "u_1").is_none());
    }

    #[test]
    fn end_when_no_active_case_returns_none() {
        let store = CaseSessionStore::new();
        assert!(store.end("kakao", "u_1").is_none());
    }

    #[test]
    fn start_replaces_existing_case() {
        let store = CaseSessionStore::new();
        store.start("kakao", "u_1", "사건A").unwrap();
        let new_active = store.start("kakao", "u_1", "사건B").unwrap();
        assert_eq!(store.current("kakao", "u_1"), Some(new_active.clone()));
        assert_eq!(new_active.label, "사건B");
    }

    #[test]
    fn cases_isolated_across_users_and_channels() {
        let store = CaseSessionStore::new();
        store.start("kakao", "u_1", "사건A").unwrap();
        store.start("kakao", "u_2", "사건B").unwrap();
        store.start("telegram", "u_1", "사건C").unwrap();
        assert_eq!(store.current("kakao", "u_1").unwrap().label, "사건A");
        assert_eq!(store.current("kakao", "u_2").unwrap().label, "사건B");
        assert_eq!(store.current("telegram", "u_1").unwrap().label, "사건C");
    }

    #[test]
    fn list_for_user_returns_all_channels() {
        let store = CaseSessionStore::new();
        store.start("kakao", "u_1", "사건A").unwrap();
        store.start("telegram", "u_1", "사건B").unwrap();
        store.start("kakao", "u_2", "사건C").unwrap();
        let mut cases = store.list_for_user("u_1");
        cases.sort_by(|a, b| a.0.cmp(&b.0));
        assert_eq!(cases.len(), 2);
        assert_eq!(cases[0].0, "kakao");
        assert_eq!(cases[1].0, "telegram");
    }

    #[test]
    fn label_truncated_to_max_chars() {
        let store = CaseSessionStore::new();
        let very_long = "가".repeat(MAX_CASE_LABEL_CHARS + 50);
        let active = store.start("kakao", "u_1", &very_long).unwrap();
        assert_eq!(active.label.chars().count(), MAX_CASE_LABEL_CHARS);
    }

    #[test]
    fn sanitize_case_id_keeps_unicode_letters_and_collapses_separators() {
        // Korean letters are alphanumeric in Unicode → kept.
        // Spaces and punctuation collapse to a single underscore.
        assert_eq!(sanitize_case_id("김OO 2024가합123"), "김OO_2024가합123");
        assert_eq!(sanitize_case_id("a!!b???c"), "a_b_c");
        assert_eq!(sanitize_case_id("___test___"), "test");
        assert_eq!(sanitize_case_id("  spaces  "), "spaces");
    }

    #[test]
    fn case_session_id_format() {
        let case = ActiveCase {
            case_id: "kim_2024".to_string(),
            label: "kim_2024".to_string(),
            started_at: 0,
        };
        assert_eq!(case_session_id("kakao", &case), "kakao_case_kim_2024");
        assert_eq!(case_session_id("telegram", &case), "telegram_case_kim_2024");
    }

    #[test]
    fn started_at_is_set_to_current_epoch() {
        let store = CaseSessionStore::new();
        let before = epoch_secs();
        let active = store.start("kakao", "u_1", "사건").unwrap();
        let after = epoch_secs();
        assert!(active.started_at >= before && active.started_at <= after);
    }

    #[test]
    fn active_count_tracks_mints_and_ends() {
        let store = CaseSessionStore::new();
        assert_eq!(store.active_count(), 0);
        store.start("kakao", "u_1", "A").unwrap();
        store.start("telegram", "u_2", "B").unwrap();
        assert_eq!(store.active_count(), 2);
        store.end("kakao", "u_1");
        assert_eq!(store.active_count(), 1);
    }

    #[test]
    fn file_backed_store_persists_across_reopen() {
        let dir = tempdir().expect("tempdir");
        let path = dir.path().join("case_sessions.db");

        // Write through the first handle.
        {
            let store = CaseSessionStore::open(&path).expect("open case_sessions.db");
            store.start("kakao", "u_1", "영속_사건").unwrap();
            assert_eq!(store.current("kakao", "u_1").unwrap().label, "영속_사건");
        }

        // Reopen and verify the row survived process restart.
        {
            let store = CaseSessionStore::open(&path).expect("reopen case_sessions.db");
            let active = store
                .current("kakao", "u_1")
                .expect("case must persist across reopen");
            assert_eq!(active.label, "영속_사건");
            // Ending still works after reopen.
            assert!(store.end("kakao", "u_1").is_some());
        }

        // Third open sees the deletion.
        let store = CaseSessionStore::open(&path).expect("reopen after end");
        assert!(store.current("kakao", "u_1").is_none());
    }
}
