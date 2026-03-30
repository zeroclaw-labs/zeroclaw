//! Channel Pairing Store — one-click pairing for all messaging channels.
//!
//! ## Flow
//!
//! 1. Unauthorized user sends a message to a channel (KakaoTalk, Telegram, etc.)
//! 2. Channel creates a **pairing token** (UUID) in SQLite → sends a link
//! 3. User clicks the link → gateway web page opens
//! 4. Already-registered user: login → auto-pair → success page
//!    New user: signup → auto-pair → success page
//! 5. User returns to chat → next message is auto-accepted
//!
//! ## Storage
//! Uses SQLite for persistence, enabling token sharing between gateway and
//! channel processes (e.g., in daemon mode where both run as separate tasks).
//!
//! ## Security
//! - Tokens are UUID v4 (2^122 entropy — brute-force infeasible)
//! - Tokens expire after 10 minutes (configurable)
//! - Tokens are single-use (consumed on successful pairing)
//! - Maximum 50 active tokens at any time (prevents flooding)

use parking_lot::Mutex;
use std::path::Path;
use std::time::{SystemTime, UNIX_EPOCH};

/// How long a pairing token remains valid (seconds).
const TOKEN_TTL_SECS: u64 = 600; // 10 minutes

/// Maximum number of active (unexpired) tokens.
const MAX_ACTIVE_TOKENS: usize = 50;

/// A pending pairing token, created when an unauthorized user sends a message.
#[derive(Debug, Clone)]
pub struct PairingToken {
    /// UUID v4 token embedded in the connect URL.
    pub token: String,
    /// Which channel this token is for ("kakao", "telegram", etc.).
    pub channel: String,
    /// The platform-specific user identifier.
    pub platform_uid: String,
    /// Unix timestamp when this token expires.
    pub expires_at: u64,
}

/// A completed pairing record, written after successful web authentication.
#[derive(Debug, Clone)]
pub struct CompletedPair {
    pub channel: String,
    pub platform_uid: String,
    pub user_id: String,
    pub paired_at: u64,
}

/// Thread-safe store for channel pairing tokens and completed pairs.
/// Backed by SQLite for cross-process sharing (gateway + channels).
#[derive(Debug)]
pub struct ChannelPairingStore {
    conn: Mutex<rusqlite::Connection>,
}

impl ChannelPairingStore {
    /// Create an in-memory store (for tests).
    pub fn new() -> Self {
        let conn = rusqlite::Connection::open_in_memory()
            .expect("Failed to open in-memory SQLite for pairing store");
        Self::init_tables(&conn);
        Self {
            conn: Mutex::new(conn),
        }
    }

    /// Open a file-backed store for production use.
    /// Both gateway and channels should point to the same file.
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
            "CREATE TABLE IF NOT EXISTS pairing_tokens (
                token TEXT PRIMARY KEY,
                channel TEXT NOT NULL,
                platform_uid TEXT NOT NULL,
                expires_at INTEGER NOT NULL
            );
            CREATE TABLE IF NOT EXISTS completed_pairs (
                channel TEXT NOT NULL,
                platform_uid TEXT NOT NULL,
                user_id TEXT NOT NULL DEFAULT '',
                paired_at INTEGER NOT NULL,
                PRIMARY KEY (channel, platform_uid)
            );
            CREATE INDEX IF NOT EXISTS idx_pairing_tokens_expires ON pairing_tokens(expires_at);",
        )
        .expect("Failed to initialize pairing tables");

        // Cleanup stale tokens on startup
        let now = epoch_secs() as i64;
        let _ = conn.execute(
            "DELETE FROM pairing_tokens WHERE expires_at <= ?1",
            rusqlite::params![now],
        );
    }

    // ── Token management (called by channels) ────────────────────────

    /// Create a pairing token for an unauthorized user.
    /// Returns the UUID token to embed in the connect URL.
    pub fn create_token(&self, channel: &str, platform_uid: &str) -> String {
        let conn = self.conn.lock();
        let now = epoch_secs() as i64;

        // Cleanup expired
        let _ = conn.execute(
            "DELETE FROM pairing_tokens WHERE expires_at <= ?1",
            rusqlite::params![now],
        );

        // Remove existing token for same channel+uid
        let _ = conn.execute(
            "DELETE FROM pairing_tokens WHERE channel = ?1 AND platform_uid = ?2",
            rusqlite::params![channel, platform_uid],
        );

        // Enforce max active tokens
        let count: i64 = conn
            .query_row("SELECT COUNT(*) FROM pairing_tokens", [], |row| row.get(0))
            .unwrap_or(0);
        if count >= MAX_ACTIVE_TOKENS as i64 {
            let _ = conn.execute(
                "DELETE FROM pairing_tokens WHERE token = \
                 (SELECT token FROM pairing_tokens ORDER BY expires_at ASC LIMIT 1)",
                [],
            );
        }

        let token = uuid::Uuid::new_v4().to_string();
        let expires_at = now + TOKEN_TTL_SECS as i64;

        let _ = conn.execute(
            "INSERT INTO pairing_tokens (token, channel, platform_uid, expires_at) \
             VALUES (?1, ?2, ?3, ?4)",
            rusqlite::params![token, channel, platform_uid, expires_at],
        );

        tracing::info!(
            channel = channel,
            "Pairing token created (expires in {}s)",
            TOKEN_TTL_SECS
        );

        token
    }

    /// Look up a pairing token without consuming it.
    /// Used by the gateway to display the login page.
    pub fn lookup_token(&self, token: &str) -> Option<PairingToken> {
        let conn = self.conn.lock();
        let now = epoch_secs() as i64;

        // Cleanup expired
        let _ = conn.execute(
            "DELETE FROM pairing_tokens WHERE expires_at <= ?1",
            rusqlite::params![now],
        );

        conn.query_row(
            "SELECT token, channel, platform_uid, expires_at \
             FROM pairing_tokens WHERE token = ?1",
            rusqlite::params![token],
            |row| {
                Ok(PairingToken {
                    token: row.get(0)?,
                    channel: row.get(1)?,
                    platform_uid: row.get(2)?,
                    expires_at: u64::try_from(row.get::<_, i64>(3)?).unwrap_or(0),
                })
            },
        )
        .ok()
    }

    /// Consume a pairing token (single-use).
    /// Called by the gateway after successful authentication.
    pub fn consume_token(&self, token: &str) -> Option<PairingToken> {
        let entry = self.lookup_token(token)?;
        let conn = self.conn.lock();
        let _ = conn.execute(
            "DELETE FROM pairing_tokens WHERE token = ?1",
            rusqlite::params![token],
        );
        Some(entry)
    }

    // ── Completed pairs (written by gateway, read by channels) ───────

    /// Record a completed pairing. Called by the gateway after successful auth.
    pub fn mark_paired(&self, channel: &str, platform_uid: &str, user_id: &str) {
        let conn = self.conn.lock();
        let now = epoch_secs() as i64;
        let _ = conn.execute(
            "INSERT OR REPLACE INTO completed_pairs (channel, platform_uid, user_id, paired_at) \
             VALUES (?1, ?2, ?3, ?4)",
            rusqlite::params![channel, platform_uid, user_id, now],
        );
        tracing::info!(
            channel = channel,
            platform_uid = platform_uid,
            "Pairing completed and recorded"
        );
    }

    /// Check if a platform identity has been paired via the web flow.
    /// Called by channels when they see an unauthorized user.
    pub fn is_paired(&self, channel: &str, platform_uid: &str) -> bool {
        let conn = self.conn.lock();
        conn.query_row(
            "SELECT COUNT(*) FROM completed_pairs WHERE channel = ?1 AND platform_uid = ?2",
            rusqlite::params![channel, platform_uid],
            |row| row.get::<_, i64>(0),
        )
        .unwrap_or(0)
            > 0
    }

    /// Look up the MoA user_id for a paired channel identity.
    /// Returns `None` if not paired.
    /// Used by channel-to-device relay to route channel messages to the correct device.
    pub fn lookup_user_id(&self, channel: &str, platform_uid: &str) -> Option<String> {
        let conn = self.conn.lock();
        conn.query_row(
            "SELECT user_id FROM completed_pairs WHERE channel = ?1 AND platform_uid = ?2",
            rusqlite::params![channel, platform_uid],
            |row| row.get::<_, String>(0),
        )
        .ok()
        .filter(|uid| !uid.is_empty())
    }

    /// Build the one-click auto-pair URL.
    pub fn auto_pair_url(gateway_base: &str, token: &str) -> String {
        format!("{gateway_base}/pair/auto/{token}")
    }

    /// Get the number of active (non-expired) tokens.
    pub fn active_count(&self) -> usize {
        let conn = self.conn.lock();
        let now = epoch_secs() as i64;
        conn.query_row(
            "SELECT COUNT(*) FROM pairing_tokens WHERE expires_at > ?1",
            rusqlite::params![now],
            |row| row.get::<_, i64>(0),
        )
        .map(|c| usize::try_from(c).unwrap_or(0))
        .unwrap_or(0)
    }
}

impl Default for ChannelPairingStore {
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

/// Persist a newly-paired identity to the channel's allowed-user list in config.toml.
/// This is a generic helper usable by all channels.
pub fn persist_channel_allowlist(channel_name: &str, identity: &str) -> anyhow::Result<()> {
    use crate::config::Config;
    use directories::UserDirs;

    let home = UserDirs::new()
        .map(|u| u.home_dir().to_path_buf())
        .ok_or_else(|| anyhow::anyhow!("Could not find home directory"))?;
    let zeroclaw_dir = home.join(".zeroclaw");
    let config_path = zeroclaw_dir.join("config.toml");

    let contents = std::fs::read_to_string(&config_path)
        .map_err(|e| anyhow::anyhow!("Failed to read config: {e}"))?;
    let mut config: Config =
        toml::from_str(&contents).map_err(|e| anyhow::anyhow!("Failed to parse config: {e}"))?;
    config.config_path = config_path;
    config.workspace_dir = zeroclaw_dir.join("workspace");

    let identity = identity.trim().to_string();
    if identity.is_empty() {
        anyhow::bail!("Cannot persist empty identity");
    }

    let added = match channel_name {
        "kakao" => {
            if let Some(ref mut kakao) = config.channels_config.kakao {
                if kakao.allowed_users.contains(&identity) {
                    false
                } else {
                    kakao.allowed_users.push(identity.clone());
                    true
                }
            } else {
                false
            }
        }
        "telegram" => {
            if let Some(ref mut tg) = config.channels_config.telegram {
                if tg.allowed_users.contains(&identity) {
                    false
                } else {
                    tg.allowed_users.push(identity.clone());
                    true
                }
            } else {
                false
            }
        }
        "discord" => {
            if let Some(ref mut dc) = config.channels_config.discord {
                if dc.allowed_users.contains(&identity) {
                    false
                } else {
                    dc.allowed_users.push(identity.clone());
                    true
                }
            } else {
                false
            }
        }
        "slack" => {
            if let Some(ref mut sl) = config.channels_config.slack {
                if sl.allowed_users.contains(&identity) {
                    false
                } else {
                    sl.allowed_users.push(identity.clone());
                    true
                }
            } else {
                false
            }
        }
        "whatsapp" => {
            if let Some(ref mut wa) = config.channels_config.whatsapp {
                if wa.allowed_numbers.contains(&identity) {
                    false
                } else {
                    wa.allowed_numbers.push(identity.clone());
                    true
                }
            } else {
                false
            }
        }
        "imessage" => {
            if let Some(ref mut im) = config.channels_config.imessage {
                if im.allowed_contacts.contains(&identity) {
                    false
                } else {
                    im.allowed_contacts.push(identity.clone());
                    true
                }
            } else {
                false
            }
        }
        _ => {
            tracing::warn!("persist_channel_allowlist: unsupported channel '{channel_name}'");
            return Ok(());
        }
    };

    if added {
        let toml_str = toml::to_string_pretty(&config)
            .map_err(|e| anyhow::anyhow!("Failed to serialize config: {e}"))?;
        std::fs::write(&config.config_path, toml_str)
            .map_err(|e| anyhow::anyhow!("Failed to write config: {e}"))?;
        tracing::info!(
            channel = channel_name,
            identity = %identity,
            "Persisted paired identity to config.toml"
        );
    }

    Ok(())
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn create_and_lookup_token() {
        let store = ChannelPairingStore::new();
        let token = store.create_token("kakao", "user_123");

        assert_eq!(token.len(), 36); // UUID v4 format
        let entry = store.lookup_token(&token).unwrap();
        assert_eq!(entry.channel, "kakao");
        assert_eq!(entry.platform_uid, "user_123");
    }

    #[test]
    fn consume_token_single_use() {
        let store = ChannelPairingStore::new();
        let token = store.create_token("kakao", "user_123");

        let entry = store.consume_token(&token).unwrap();
        assert_eq!(entry.platform_uid, "user_123");

        // Token should be consumed
        assert!(store.consume_token(&token).is_none());
    }

    #[test]
    fn duplicate_uid_replaces_token() {
        let store = ChannelPairingStore::new();
        let token1 = store.create_token("kakao", "user_123");
        let token2 = store.create_token("kakao", "user_123");

        assert_ne!(token1, token2);
        assert!(store.lookup_token(&token1).is_none());
        assert!(store.lookup_token(&token2).is_some());
    }

    #[test]
    fn mark_and_check_paired() {
        let store = ChannelPairingStore::new();
        assert!(!store.is_paired("kakao", "user_123"));

        store.mark_paired("kakao", "user_123", "auth_abc");
        assert!(store.is_paired("kakao", "user_123"));

        // Different channel should not be paired
        assert!(!store.is_paired("telegram", "user_123"));
    }

    #[test]
    fn active_count_tracks() {
        let store = ChannelPairingStore::new();
        assert_eq!(store.active_count(), 0);

        store.create_token("kakao", "u1");
        store.create_token("telegram", "u2");
        assert_eq!(store.active_count(), 2);
    }

    #[test]
    fn max_tokens_evicts_oldest() {
        let store = ChannelPairingStore::new();
        for i in 0..MAX_ACTIVE_TOKENS + 5 {
            store.create_token("kakao", &format!("user_{i}"));
        }
        assert!(store.active_count() <= MAX_ACTIVE_TOKENS);
    }

    #[test]
    fn auto_pair_url_format() {
        let url = ChannelPairingStore::auto_pair_url("http://localhost:3000", "abc-123-def");
        assert_eq!(url, "http://localhost:3000/pair/auto/abc-123-def");
    }
}
