//! KakaoTalk share-back token store.
//!
//! When MoA replies to a user in their 1:1 KakaoTalk chat, the reply
//! carries a `📤 단톡방으로 보내기` quick-reply button. The button URL
//! points at the gateway's `/kakao/share/{token}` page, which loads
//! the Kakao JavaScript SDK and calls `Kakao.Share.sendDefault` so the
//! user can pick a target chat (the third-party 단톡방) and forward the
//! AI's reply with the `[🤖 AI 답변]` prefix in one tap.
//!
//! This module owns the short-lived token store. The store is SQLite-
//! backed, modeled exactly on [`super::pairing::ChannelPairingStore`]
//! so both have the same operational contract (file-based, WAL,
//! cleanup on startup, per-token TTL). Tokens are UUID v4 (122 bits
//! entropy), TTL 10 minutes, single-use, with a per-user rate limit.
//!
//! ## Threat model
//!
//! - Token is unguessable and short-lived. Anyone who shoulder-surfs
//!   the URL during the 10-minute window can preview the AI reply
//!   before consume; deployments must terminate TLS at the proxy
//!   (already a deployment guideline).
//! - `message_text` is stored in plaintext SQLite under
//!   `~/.zeroclaw/workspace/`, the same trust boundary as memory.
//!   Logs MUST NOT include `message_text` or full token (see
//!   tracing calls below — only the 8-char prefix is logged).
//! - Per-user rate limit (default 60/min) prevents disk flooding by
//!   a chatty channel.
//! - Single-use consume + TTL together mean the URL stops working
//!   after one share or 10 minutes, whichever is first.

use parking_lot::Mutex;
use std::collections::HashMap;
use std::path::Path;
use std::time::{SystemTime, UNIX_EPOCH};

/// How long a share token remains valid (seconds).
pub const SHARE_TOKEN_TTL_SECS: u64 = 600; // 10 minutes

/// Maximum number of active (unexpired) tokens. Higher than the pairing
/// store because every AI reply mints one.
pub const MAX_ACTIVE_SHARE_TOKENS: usize = 200;

/// Default per-user rate limit when [`KakaoShareStore::create_token`] is
/// called via the rate-limited entry point. 60 minted tokens per rolling
/// minute is generous for a chat UI but guards against runaway loops.
pub const DEFAULT_RATE_LIMIT_PER_MINUTE: usize = 60;

/// Standard prefix attached to every shared-back message body. Constant,
/// not configurable — KISS, and the prefix is the entire point of the
/// observer-mode UX (other 단톡방 participants must see "this is from AI").
pub const SHARE_PREFIX: &str = "[🤖 AI 답변]\n";

/// A pending share token created when MoA renders a reply.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct ShareToken {
    /// UUID v4 token embedded in the share URL.
    pub token: String,
    /// Kakao platform user id this token was minted for.
    pub user_id: String,
    /// Reply body to forward (without the `SHARE_PREFIX` — the share
    /// page prepends the prefix at render time so the prefix lives in
    /// exactly one place).
    pub message_text: String,
    /// Unix timestamp when this token expires.
    pub expires_at: u64,
}

/// Build the public URL for a share token.
pub fn share_url(gateway_base: &str, token: &str) -> String {
    format!("{gateway_base}/kakao/share/{token}")
}

/// Build the rendered text that the share page will hand to
/// `Kakao.Share.sendDefault`. Keeps the prefix in one place.
pub fn render_share_text(message_text: &str) -> String {
    format!("{SHARE_PREFIX}{message_text}")
}

/// Thread-safe SQLite-backed store for Kakao share tokens.
#[derive(Debug)]
pub struct KakaoShareStore {
    conn: Mutex<rusqlite::Connection>,
    /// In-memory rate-limit ledger: user_id → minted-at epochs (oldest
    /// first). Sliding 60-second window. Kept out of SQLite because it
    /// changes on every mint and the data is intentionally ephemeral.
    rate_ledger: Mutex<HashMap<String, Vec<u64>>>,
}

impl KakaoShareStore {
    /// In-memory store for tests.
    pub fn new() -> Self {
        let conn = rusqlite::Connection::open_in_memory()
            .expect("failed to open in-memory SQLite for kakao share store");
        Self::init_tables(&conn);
        Self {
            conn: Mutex::new(conn),
            rate_ledger: Mutex::new(HashMap::new()),
        }
    }

    /// File-backed store for production use.
    pub fn open(db_path: &Path) -> anyhow::Result<Self> {
        let conn = rusqlite::Connection::open(db_path)?;
        conn.execute_batch("PRAGMA journal_mode = WAL; PRAGMA busy_timeout = 5000;")?;
        Self::init_tables(&conn);
        Ok(Self {
            conn: Mutex::new(conn),
            rate_ledger: Mutex::new(HashMap::new()),
        })
    }

    fn init_tables(conn: &rusqlite::Connection) {
        conn.execute_batch(
            "CREATE TABLE IF NOT EXISTS kakao_share_tokens (
                token TEXT PRIMARY KEY,
                user_id TEXT NOT NULL,
                message_text TEXT NOT NULL,
                expires_at INTEGER NOT NULL
            );
            CREATE INDEX IF NOT EXISTS idx_kakao_share_tokens_expires
                ON kakao_share_tokens(expires_at);
            CREATE INDEX IF NOT EXISTS idx_kakao_share_tokens_user
                ON kakao_share_tokens(user_id);",
        )
        .expect("failed to initialize kakao_share_tokens table");

        // Cleanup stale tokens on startup.
        let now = epoch_secs() as i64;
        let _ = conn.execute(
            "DELETE FROM kakao_share_tokens WHERE expires_at <= ?1",
            rusqlite::params![now],
        );
    }

    /// Mint a share token without rate-limit enforcement. Returns the
    /// token UUID. Used in tests and by callers that have already
    /// performed their own rate-limiting.
    pub fn create_token(&self, user_id: &str, message_text: &str) -> String {
        self.create_token_at(user_id, message_text, epoch_secs())
    }

    /// Mint a share token subject to a per-user rate limit. Returns
    /// `Some(token)` on success, `None` when the user has exceeded
    /// `max_per_minute` mints in the trailing 60s window.
    pub fn create_token_with_rate_limit(
        &self,
        user_id: &str,
        message_text: &str,
        max_per_minute: usize,
    ) -> Option<String> {
        let now = epoch_secs();
        if !self.allow_mint(user_id, now, max_per_minute) {
            tracing::warn!(
                user_id = user_id,
                "Kakao share: rate limit exceeded ({max_per_minute}/min)"
            );
            return None;
        }
        Some(self.create_token_at(user_id, message_text, now))
    }

    fn create_token_at(&self, user_id: &str, message_text: &str, now: u64) -> String {
        let conn = self.conn.lock();
        let now_i64 = now as i64;

        // Cleanup expired
        let _ = conn.execute(
            "DELETE FROM kakao_share_tokens WHERE expires_at <= ?1",
            rusqlite::params![now_i64],
        );

        // Enforce global active cap by evicting the oldest if needed.
        let count: i64 = conn
            .query_row("SELECT COUNT(*) FROM kakao_share_tokens", [], |r| r.get(0))
            .unwrap_or(0);
        if count >= MAX_ACTIVE_SHARE_TOKENS as i64 {
            let _ = conn.execute(
                "DELETE FROM kakao_share_tokens WHERE token = \
                 (SELECT token FROM kakao_share_tokens ORDER BY expires_at ASC LIMIT 1)",
                [],
            );
        }

        let token = uuid::Uuid::new_v4().to_string();
        let expires_at = now_i64 + SHARE_TOKEN_TTL_SECS as i64;

        let _ = conn.execute(
            "INSERT INTO kakao_share_tokens (token, user_id, message_text, expires_at) \
             VALUES (?1, ?2, ?3, ?4)",
            rusqlite::params![token, user_id, message_text, expires_at],
        );

        // Never log full token / message body — see threat model.
        let token_prefix = &token[..token.len().min(8)];
        tracing::info!(
            user_id = user_id,
            token_prefix = token_prefix,
            ttl_secs = SHARE_TOKEN_TTL_SECS,
            "Kakao share token created"
        );

        token
    }

    /// Look up a token without consuming it. Used by the GET share page
    /// to render the message body for the JS SDK.
    pub fn lookup_token(&self, token: &str) -> Option<ShareToken> {
        let conn = self.conn.lock();
        let now = epoch_secs() as i64;
        let _ = conn.execute(
            "DELETE FROM kakao_share_tokens WHERE expires_at <= ?1",
            rusqlite::params![now],
        );
        conn.query_row(
            "SELECT token, user_id, message_text, expires_at \
             FROM kakao_share_tokens WHERE token = ?1",
            rusqlite::params![token],
            |row| {
                Ok(ShareToken {
                    token: row.get(0)?,
                    user_id: row.get(1)?,
                    message_text: row.get(2)?,
                    expires_at: u64::try_from(row.get::<_, i64>(3)?).unwrap_or(0),
                })
            },
        )
        .ok()
    }

    /// Single-use consume. Returns the token's payload if it was
    /// present, then deletes it. Idempotent on already-consumed tokens
    /// (returns `None`).
    pub fn consume_token(&self, token: &str) -> Option<ShareToken> {
        let entry = self.lookup_token(token)?;
        let conn = self.conn.lock();
        let _ = conn.execute(
            "DELETE FROM kakao_share_tokens WHERE token = ?1",
            rusqlite::params![token],
        );
        Some(entry)
    }

    /// Number of unexpired tokens currently held.
    pub fn active_count(&self) -> usize {
        let conn = self.conn.lock();
        let now = epoch_secs() as i64;
        conn.query_row(
            "SELECT COUNT(*) FROM kakao_share_tokens WHERE expires_at > ?1",
            rusqlite::params![now],
            |row| row.get::<_, i64>(0),
        )
        .map(|c| usize::try_from(c).unwrap_or(0))
        .unwrap_or(0)
    }

    /// Returns true and records the mint when the user is under the
    /// limit; returns false otherwise. Sliding 60s window.
    fn allow_mint(&self, user_id: &str, now: u64, max_per_minute: usize) -> bool {
        let mut ledger = self.rate_ledger.lock();
        let bucket = ledger.entry(user_id.to_string()).or_default();
        let cutoff = now.saturating_sub(60);
        bucket.retain(|&t| t > cutoff);
        if bucket.len() >= max_per_minute {
            return false;
        }
        bucket.push(now);
        true
    }
}

impl Default for KakaoShareStore {
    fn default() -> Self {
        Self::new()
    }
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

    #[test]
    fn create_and_lookup_token() {
        let store = KakaoShareStore::new();
        let token = store.create_token("u_1", "안녕하세요");
        assert_eq!(token.len(), 36); // UUID v4 string length
        let entry = store.lookup_token(&token).unwrap();
        assert_eq!(entry.user_id, "u_1");
        assert_eq!(entry.message_text, "안녕하세요");
        // Token is NOT consumed by lookup.
        assert!(store.lookup_token(&token).is_some());
    }

    #[test]
    fn consume_token_is_single_use() {
        let store = KakaoShareStore::new();
        let token = store.create_token("u_1", "msg");
        let first = store.consume_token(&token).unwrap();
        assert_eq!(first.message_text, "msg");
        assert!(store.consume_token(&token).is_none());
        assert!(store.lookup_token(&token).is_none());
    }

    #[test]
    fn unknown_token_returns_none() {
        let store = KakaoShareStore::new();
        assert!(store
            .lookup_token("00000000-0000-0000-0000-000000000000")
            .is_none());
        assert!(store
            .consume_token("00000000-0000-0000-0000-000000000000")
            .is_none());
    }

    #[test]
    fn share_url_format() {
        assert_eq!(
            share_url("https://gw.example.com", "abc-123"),
            "https://gw.example.com/kakao/share/abc-123"
        );
    }

    #[test]
    fn render_share_text_prepends_constant_prefix() {
        let rendered = render_share_text("본문입니다");
        assert!(rendered.starts_with(SHARE_PREFIX));
        assert!(rendered.ends_with("본문입니다"));
        // Whole prefix should be: "[🤖 AI 답변]\n"
        assert_eq!(SHARE_PREFIX, "[🤖 AI 답변]\n");
    }

    #[test]
    fn rate_limit_blocks_after_threshold() {
        let store = KakaoShareStore::new();
        for _ in 0..3 {
            assert!(store
                .create_token_with_rate_limit("u_1", "msg", 3)
                .is_some());
        }
        // 4th in same minute is blocked.
        assert!(store
            .create_token_with_rate_limit("u_1", "msg", 3)
            .is_none());
    }

    #[test]
    fn rate_limit_isolates_users() {
        let store = KakaoShareStore::new();
        for _ in 0..2 {
            assert!(store.create_token_with_rate_limit("u_1", "m", 2).is_some());
        }
        assert!(store.create_token_with_rate_limit("u_1", "m", 2).is_none());
        // u_2 still has its own budget.
        assert!(store.create_token_with_rate_limit("u_2", "m", 2).is_some());
    }

    #[test]
    fn max_active_evicts_oldest() {
        let store = KakaoShareStore::new();
        // Create well over the cap; oldest should be evicted so total
        // stays at MAX_ACTIVE_SHARE_TOKENS.
        for i in 0..(MAX_ACTIVE_SHARE_TOKENS + 5) {
            store.create_token(&format!("u_{i}"), "msg");
        }
        assert!(store.active_count() <= MAX_ACTIVE_SHARE_TOKENS);
    }

    #[test]
    fn expired_tokens_are_dropped_on_lookup() {
        let store = KakaoShareStore::new();
        // Manually inject a token that already expired. We bypass
        // create_token_at to fully control expires_at.
        let token = "11111111-2222-3333-4444-555555555555";
        let conn = store.conn.lock();
        let _ = conn.execute(
            "INSERT INTO kakao_share_tokens (token, user_id, message_text, expires_at) \
             VALUES (?1, ?2, ?3, ?4)",
            rusqlite::params![token, "u_1", "msg", 1_i64], // Expired ages ago
        );
        drop(conn);
        // Lookup should sweep and return None.
        assert!(store.lookup_token(token).is_none());
        assert_eq!(store.active_count(), 0);
    }

    #[test]
    fn active_count_tracks_mints() {
        let store = KakaoShareStore::new();
        assert_eq!(store.active_count(), 0);
        store.create_token("u_1", "a");
        store.create_token("u_2", "b");
        assert_eq!(store.active_count(), 2);
    }
}
