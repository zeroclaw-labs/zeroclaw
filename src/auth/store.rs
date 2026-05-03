//! SQLite-backed user authentication store.
//!
//! Tables:
//! - `users`: username, password_hash, salt, created_at
//! - `sessions`: token_hash, user_id, device_id, expires_at
//! - `devices`: device_id, user_id, device_name, last_seen

use anyhow::{bail, Result};
use parking_lot::Mutex;
use serde::{Deserialize, Serialize};
use sha2::{Digest, Sha256};
use std::path::Path;
use std::time::{SystemTime, UNIX_EPOCH};

/// Default session duration: 30 days (seconds).
const DEFAULT_SESSION_TTL_SECS: u64 = 30 * 24 * 3600;

/// Web remote session duration: 24 hours (seconds).
/// Shorter TTL for web chat sessions to limit token-theft exposure.
pub const WEB_SESSION_TTL_SECS: u64 = 24 * 3600;

/// Token byte length before hex encoding (32 bytes = 64 hex chars).
const TOKEN_BYTES: usize = 32;

/// Salt byte length for password hashing.
const SALT_BYTES: usize = 16;

/// Number of SHA-256 iterations for password stretching.
const HASH_ITERATIONS: u32 = 100_000;

/// A registered user.
#[derive(Debug, Clone)]
pub struct User {
    pub id: String,
    pub username: String,
    pub email: Option<String>,
    pub created_at: i64,
}

/// An active session.
#[derive(Debug, Clone)]
pub struct Session {
    pub user_id: String,
    pub device_id: Option<String>,
    pub device_name: Option<String>,
    pub expires_at: i64,
}

/// A registered device.
#[derive(Debug, Clone)]
pub struct Device {
    pub device_id: String,
    pub user_id: String,
    pub device_name: String,
    pub last_seen: i64,
}

/// A device with online/offline status information.
#[derive(Debug, Clone)]
pub struct DeviceWithStatus {
    pub device_id: String,
    pub user_id: String,
    pub device_name: String,
    pub platform: Option<String>,
    pub last_seen: i64,
    pub is_online: bool,
    pub has_pairing_code: bool,
    /// Hardware fingerprint (SHA-256 hash). Used internally for deduplication;
    /// not displayed to users in the chat UI.
    pub fingerprint: Option<String>,
}

/// SQLite-backed authentication store.
pub struct AuthStore {
    conn: Mutex<rusqlite::Connection>,
    session_ttl_secs: u64,
}

impl AuthStore {
    /// Open (or create) the auth database at the given path.
    pub fn new(db_path: &Path, session_ttl_secs: Option<u64>) -> Result<Self> {
        let conn = rusqlite::Connection::open(db_path)?;

        // WAL mode for concurrent reads + crash safety
        conn.execute_batch(
            "PRAGMA journal_mode = WAL;
             PRAGMA synchronous = NORMAL;
             PRAGMA busy_timeout = 5000;
             PRAGMA foreign_keys = ON;",
        )?;

        conn.execute_batch(
            "CREATE TABLE IF NOT EXISTS users (
                id TEXT PRIMARY KEY,
                username TEXT NOT NULL UNIQUE COLLATE NOCASE,
                password_hash TEXT NOT NULL,
                salt TEXT NOT NULL,
                created_at INTEGER NOT NULL
            );

            CREATE TABLE IF NOT EXISTS sessions (
                token_hash TEXT PRIMARY KEY,
                user_id TEXT NOT NULL REFERENCES users(id) ON DELETE CASCADE,
                device_id TEXT,
                device_name TEXT,
                created_at INTEGER NOT NULL,
                expires_at INTEGER NOT NULL
            );
            CREATE INDEX IF NOT EXISTS idx_sessions_user ON sessions(user_id);
            CREATE INDEX IF NOT EXISTS idx_sessions_expires ON sessions(expires_at);

            CREATE TABLE IF NOT EXISTS devices (
                device_id TEXT PRIMARY KEY,
                user_id TEXT NOT NULL REFERENCES users(id) ON DELETE CASCADE,
                device_name TEXT NOT NULL,
                platform TEXT,
                pairing_code_hash TEXT,
                fingerprint TEXT,
                last_seen INTEGER NOT NULL
            );
            CREATE INDEX IF NOT EXISTS idx_devices_user ON devices(user_id);",
        )?;

        // Migration: add pairing_code_hash column if missing
        let has_pairing_code: bool = conn
            .prepare("SELECT pairing_code_hash FROM devices LIMIT 0")
            .is_ok();
        if !has_pairing_code {
            let _ = conn.execute_batch("ALTER TABLE devices ADD COLUMN pairing_code_hash TEXT;");
        }

        // Migration: add fingerprint column if missing
        let has_fingerprint: bool = conn
            .prepare("SELECT fingerprint FROM devices LIMIT 0")
            .is_ok();
        if !has_fingerprint {
            let _ = conn.execute_batch("ALTER TABLE devices ADD COLUMN fingerprint TEXT;");
        }

        // Create fingerprint unique index (per user) after ensuring column exists.
        // This enforces deduplication at the database level even under concurrent access.
        let _ = conn.execute_batch(
            "CREATE UNIQUE INDEX IF NOT EXISTS idx_devices_user_fingerprint ON devices(user_id, fingerprint) WHERE fingerprint IS NOT NULL;",
        );
        // Legacy non-unique index kept for backward compatibility with existing DBs.
        let _ = conn.execute_batch(
            "CREATE INDEX IF NOT EXISTS idx_devices_fingerprint ON devices(fingerprint);",
        );

        // Migration: add email column to users table if missing
        let has_email: bool = conn.prepare("SELECT email FROM users LIMIT 0").is_ok();
        if !has_email {
            let _ = conn.execute_batch("ALTER TABLE users ADD COLUMN email TEXT;");
        }

        // ── Usage analytics table ──
        conn.execute_batch(
            "CREATE TABLE IF NOT EXISTS usage_stats (
                id INTEGER PRIMARY KEY AUTOINCREMENT,
                user_id TEXT NOT NULL REFERENCES users(id) ON DELETE CASCADE,
                category TEXT NOT NULL,
                event_count INTEGER NOT NULL DEFAULT 1,
                total_chars INTEGER NOT NULL DEFAULT 0,
                recorded_at INTEGER NOT NULL
            );
            CREATE INDEX IF NOT EXISTS idx_usage_user ON usage_stats(user_id);
            CREATE INDEX IF NOT EXISTS idx_usage_cat ON usage_stats(category);
            CREATE INDEX IF NOT EXISTS idx_usage_time ON usage_stats(recorded_at);",
        )?;

        // ── Admin credentials table ──
        conn.execute_batch(
            "CREATE TABLE IF NOT EXISTS admins (
                username TEXT PRIMARY KEY COLLATE NOCASE,
                password_hash TEXT NOT NULL,
                salt TEXT NOT NULL,
                created_at INTEGER NOT NULL
            );",
        )?;

        // ── User-bound sync master keys (PR-A2 — D1+D2 wiring) ──
        //
        // The patent requires a per-user master sync key that is
        // identical across all of a user's devices, NOT a per-device
        // random key. This table stores the 32-byte master key that
        // gets transmitted to new devices over the pairing channel
        // when they join the user's account. The key is base64-encoded
        // so it round-trips through both SQLite TEXT columns and the
        // pairing wire format without any binary-data handling on the
        // intermediate hops.
        //
        // Schema notes:
        //   - One row per user. The UNIQUE constraint on user_id keeps
        //     it that way; if a user "rotates" their key (reset device,
        //     etc.), the row is REPLACED, not duplicated.
        //   - `key_b64` is base64(STANDARD_NO_PAD) of 32 raw bytes →
        //     43 chars. We do NOT store the raw bytes because some
        //     SQLite client tools mishandle BLOB columns containing
        //     null bytes during ad-hoc inspection.
        //   - `created_at` is the first time we saw this key. On
        //     rotate-replace, this is the rotation time.
        //
        // The "delete on user deletion" cascade is the same pattern
        // sessions/devices use.
        conn.execute_batch(
            "CREATE TABLE IF NOT EXISTS user_sync_keys (
                user_id TEXT PRIMARY KEY REFERENCES users(id) ON DELETE CASCADE,
                key_b64 TEXT NOT NULL,
                created_at INTEGER NOT NULL
            );",
        )?;

        // Ensure default admin exists (password: admin — must change on first login)
        let admin_count: i64 = conn.query_row(
            "SELECT COUNT(*) FROM admins",
            [],
            |row| row.get(0),
        ).unwrap_or(0);
        if admin_count == 0 {
            let salt = generate_salt();
            let hash = hash_password("admin", &salt);
            let now = epoch_secs() as i64;
            let _ = conn.execute(
                "INSERT OR IGNORE INTO admins (username, password_hash, salt, created_at) VALUES (?1, ?2, ?3, ?4)",
                rusqlite::params!["admin", hash, salt, now],
            );
        }

        Ok(Self {
            conn: Mutex::new(conn),
            session_ttl_secs: session_ttl_secs.unwrap_or(DEFAULT_SESSION_TTL_SECS),
        })
    }

    // ── User Management ─────────────────────────────────────────────

    /// Register a new user. Returns the user ID.
    pub fn register(&self, username: &str, password: &str) -> Result<String> {
        let trimmed = username.trim();
        if trimmed.is_empty() {
            bail!("Username cannot be empty");
        }
        if trimmed.len() > 64 {
            bail!("Username too long (max 64 characters)");
        }
        if password.len() < 8 {
            bail!("Password must be at least 8 characters");
        }

        let user_id = uuid::Uuid::new_v4().to_string();
        let salt = generate_salt();
        let password_hash = hash_password(password, &salt);
        let now = epoch_secs();

        let conn = self.conn.lock();
        let result = conn.execute(
            "INSERT INTO users (id, username, password_hash, salt, created_at)
             VALUES (?1, ?2, ?3, ?4, ?5)",
            rusqlite::params![user_id, trimmed, password_hash, salt, now as i64],
        );

        match result {
            Ok(_) => Ok(user_id),
            Err(rusqlite::Error::SqliteFailure(err, _))
                if err.code == rusqlite::ErrorCode::ConstraintViolation =>
            {
                bail!("Username '{}' is already taken", trimmed)
            }
            Err(e) => Err(e.into()),
        }
    }

    /// Authenticate a user by username + password.
    /// Returns the `User` on success.
    pub fn authenticate(&self, username: &str, password: &str) -> Result<User> {
        let conn = self.conn.lock();
        let row: Result<(String, String, String, Option<String>, i64), _> = conn.query_row(
            "SELECT id, password_hash, salt, email, created_at FROM users WHERE username = ?1 COLLATE NOCASE",
            rusqlite::params![username.trim()],
            |row| Ok((row.get(0)?, row.get(1)?, row.get(2)?, row.get(3)?, row.get(4)?)),
        );

        match row {
            Ok((id, stored_hash, salt, email, created_at)) => {
                let attempt_hash = hash_password(password, &salt);
                if !constant_time_eq(stored_hash.as_bytes(), attempt_hash.as_bytes()) {
                    bail!("Invalid username or password");
                }
                Ok(User {
                    id,
                    username: username.trim().to_string(),
                    email,
                    created_at,
                })
            }
            Err(rusqlite::Error::QueryReturnedNoRows) => {
                // Perform dummy hash to prevent timing side-channel
                let _ = hash_password(password, "0000000000000000");
                bail!("Invalid username or password");
            }
            Err(e) => Err(e.into()),
        }
    }

    /// Change a user's password. Used for:
    /// - Kakao OAuth users setting their first password (enables app login)
    /// - Regular users changing their password
    pub fn set_password(&self, user_id: &str, new_password: &str) -> Result<()> {
        // SECURITY: keep this in sync with `register()` (see line ~214)
        // which enforces 8+ chars. Allowing a shorter password here was
        // a downgrade path: a user could register with a strong 8+
        // password and then `set_password` it down to 4 chars,
        // weakening their account post-registration.
        if new_password.len() < 8 {
            bail!("Password must be at least 8 characters");
        }
        let salt = generate_salt();
        let hash = hash_password(new_password, &salt);
        let conn = self.conn.lock();
        let updated = conn.execute(
            "UPDATE users SET password_hash = ?1, salt = ?2 WHERE id = ?3",
            rusqlite::params![hash, salt, user_id],
        )?;
        if updated == 0 {
            bail!("User not found");
        }
        Ok(())
    }

    /// Change a user's username. Used for Kakao users who want a custom username.
    pub fn set_username(&self, user_id: &str, new_username: &str) -> Result<()> {
        let trimmed = new_username.trim();
        if trimmed.is_empty() || trimmed.len() > 64 {
            bail!("Username must be 1-64 characters");
        }
        let conn = self.conn.lock();
        let updated = conn.execute(
            "UPDATE users SET username = ?1 WHERE id = ?2",
            rusqlite::params![trimmed, user_id],
        )?;
        if updated == 0 {
            bail!("User not found");
        }
        Ok(())
    }

    /// Look up a user by ID.
    pub fn get_user(&self, user_id: &str) -> Result<Option<User>> {
        let conn = self.conn.lock();
        let row = conn.query_row(
            "SELECT id, username, email, created_at FROM users WHERE id = ?1",
            rusqlite::params![user_id],
            |row| {
                Ok(User {
                    id: row.get(0)?,
                    username: row.get(1)?,
                    email: row.get(2)?,
                    created_at: row.get(3)?,
                })
            },
        );

        match row {
            Ok(user) => Ok(Some(user)),
            Err(rusqlite::Error::QueryReturnedNoRows) => Ok(None),
            Err(e) => Err(e.into()),
        }
    }

    // ── Session Management ──────────────────────────────────────────

    /// Create a session token for an authenticated user.
    /// Returns the plaintext token (only revealed once).
    pub fn create_session(
        &self,
        user_id: &str,
        device_id: Option<&str>,
        device_name: Option<&str>,
    ) -> Result<String> {
        self.create_session_with_ttl(user_id, device_id, device_name, self.session_ttl_secs)
    }

    /// Create a session token with a custom TTL (seconds).
    /// Use for web remote sessions that need shorter lifetimes.
    pub fn create_session_with_ttl(
        &self,
        user_id: &str,
        device_id: Option<&str>,
        device_name: Option<&str>,
        ttl_secs: u64,
    ) -> Result<String> {
        let token = generate_token();
        let token_hash = hash_token(&token);
        let now = epoch_secs();
        let expires_at = now + ttl_secs;

        let conn = self.conn.lock();
        conn.execute(
            "INSERT INTO sessions (token_hash, user_id, device_id, device_name, created_at, expires_at)
             VALUES (?1, ?2, ?3, ?4, ?5, ?6)",
            rusqlite::params![
                token_hash,
                user_id,
                device_id,
                device_name,
                now as i64,
                expires_at as i64,
            ],
        )?;

        Ok(token)
    }

    /// Validate a session token and return the associated session.
    /// Returns `None` if the token is invalid or expired.
    pub fn validate_session(&self, token: &str) -> Option<Session> {
        let token_hash = hash_token(token);
        let now = epoch_secs() as i64;

        let conn = self.conn.lock();
        conn.query_row(
            "SELECT user_id, device_id, device_name, expires_at
             FROM sessions
             WHERE token_hash = ?1 AND expires_at > ?2",
            rusqlite::params![token_hash, now],
            |row| {
                Ok(Session {
                    user_id: row.get(0)?,
                    device_id: row.get(1)?,
                    device_name: row.get(2)?,
                    expires_at: row.get(3)?,
                })
            },
        )
        .ok()
    }

    /// Revoke a specific session by token.
    pub fn revoke_session(&self, token: &str) -> Result<bool> {
        let token_hash = hash_token(token);
        let conn = self.conn.lock();
        let deleted = conn.execute(
            "DELETE FROM sessions WHERE token_hash = ?1",
            rusqlite::params![token_hash],
        )?;
        Ok(deleted > 0)
    }

    /// Revoke all sessions for a user.
    pub fn revoke_all_sessions(&self, user_id: &str) -> Result<u64> {
        let conn = self.conn.lock();
        let deleted = conn.execute(
            "DELETE FROM sessions WHERE user_id = ?1",
            rusqlite::params![user_id],
        )?;
        Ok(deleted as u64)
    }

    /// Clean up expired sessions.
    pub fn cleanup_expired_sessions(&self) -> Result<u64> {
        let now = epoch_secs() as i64;
        let conn = self.conn.lock();
        let deleted = conn.execute(
            "DELETE FROM sessions WHERE expires_at <= ?1",
            rusqlite::params![now],
        )?;
        Ok(deleted as u64)
    }

    // ── Device Management ───────────────────────────────────────────

    /// Register or update a device for a user.
    /// If `fingerprint` is provided and a device with the same fingerprint already
    /// exists for this user, the existing device is updated (reused) instead of
    /// creating a duplicate entry. Returns the actual device_id used.
    pub fn register_device(
        &self,
        user_id: &str,
        device_id: &str,
        device_name: &str,
        platform: Option<&str>,
        fingerprint: Option<&str>,
    ) -> Result<String> {
        let now = epoch_secs() as i64;
        let conn = self.conn.lock();

        // If fingerprint is provided, check for an existing device with the same fingerprint
        let actual_device_id = if let Some(fp) = fingerprint {
            if let Some(existing_id) = self.find_device_by_fingerprint_inner(&conn, user_id, fp) {
                existing_id
            } else {
                device_id.to_string()
            }
        } else {
            device_id.to_string()
        };

        conn.execute(
            "INSERT INTO devices (device_id, user_id, device_name, platform, fingerprint, last_seen)
             VALUES (?1, ?2, ?3, ?4, ?5, ?6)
             ON CONFLICT(device_id) DO UPDATE SET
                device_name = excluded.device_name,
                platform = excluded.platform,
                fingerprint = COALESCE(excluded.fingerprint, devices.fingerprint),
                last_seen = excluded.last_seen",
            rusqlite::params![actual_device_id, user_id, device_name, platform, fingerprint, now],
        )?;
        Ok(actual_device_id)
    }

    /// Find a device by fingerprint for a given user (internal, requires lock held).
    fn find_device_by_fingerprint_inner(
        &self,
        conn: &rusqlite::Connection,
        user_id: &str,
        fingerprint: &str,
    ) -> Option<String> {
        conn.query_row(
            "SELECT device_id FROM devices WHERE user_id = ?1 AND fingerprint = ?2 LIMIT 1",
            rusqlite::params![user_id, fingerprint],
            |row| row.get(0),
        )
        .ok()
    }

    /// Find a device by fingerprint for a given user.
    pub fn find_device_by_fingerprint(&self, user_id: &str, fingerprint: &str) -> Option<String> {
        let conn = self.conn.lock();
        self.find_device_by_fingerprint_inner(&conn, user_id, fingerprint)
    }

    /// List all devices for a user.
    pub fn list_devices(&self, user_id: &str) -> Result<Vec<Device>> {
        let conn = self.conn.lock();
        let mut stmt = conn.prepare_cached(
            "SELECT device_id, user_id, device_name, last_seen
             FROM devices WHERE user_id = ?1 ORDER BY last_seen DESC",
        )?;
        let devices = stmt
            .query_map(rusqlite::params![user_id], |row| {
                Ok(Device {
                    device_id: row.get(0)?,
                    user_id: row.get(1)?,
                    device_name: row.get(2)?,
                    last_seen: row.get(3)?,
                })
            })?
            .collect::<Result<Vec<_>, _>>()?;
        Ok(devices)
    }

    /// Remove a device.
    pub fn remove_device(&self, user_id: &str, device_id: &str) -> Result<bool> {
        let conn = self.conn.lock();
        let deleted = conn.execute(
            "DELETE FROM devices WHERE device_id = ?1 AND user_id = ?2",
            rusqlite::params![device_id, user_id],
        )?;
        Ok(deleted > 0)
    }

    /// Remove stale devices that have been offline for longer than `max_offline_secs`.
    /// Returns the number of devices removed.
    pub fn cleanup_stale_devices(&self, user_id: &str, max_offline_secs: u64) -> Result<usize> {
        let now = epoch_secs() as i64;
        let cutoff = now - max_offline_secs as i64;
        let conn = self.conn.lock();
        let deleted = conn.execute(
            "DELETE FROM devices WHERE user_id = ?1 AND last_seen < ?2",
            rusqlite::params![user_id, cutoff],
        )?;
        if deleted > 0 {
            tracing::info!(
                "Cleaned up {deleted} stale device(s) for user {user_id} (offline > {max_offline_secs}s)"
            );
        }
        Ok(deleted)
    }

    /// Update last_seen timestamp for a device.
    pub fn touch_device(&self, device_id: &str) -> Result<()> {
        let now = epoch_secs() as i64;
        let conn = self.conn.lock();
        conn.execute(
            "UPDATE devices SET last_seen = ?1 WHERE device_id = ?2",
            rusqlite::params![now, device_id],
        )?;
        Ok(())
    }

    /// Set (or clear) the pairing code for a device.
    pub fn set_device_pairing_code(
        &self,
        user_id: &str,
        device_id: &str,
        code: Option<&str>,
    ) -> Result<()> {
        let hash = code.map(|c| hash_password(c, device_id));
        let conn = self.conn.lock();
        let updated = conn.execute(
            "UPDATE devices SET pairing_code_hash = ?1 WHERE device_id = ?2 AND user_id = ?3",
            rusqlite::params![hash, device_id, user_id],
        )?;
        if updated == 0 {
            bail!("Device not found");
        }
        Ok(())
    }

    /// Verify a pairing code for a device.
    pub fn verify_device_pairing_code(&self, device_id: &str, code: &str) -> Result<bool> {
        let conn = self.conn.lock();
        let stored_hash: Option<String> = conn
            .query_row(
                "SELECT pairing_code_hash FROM devices WHERE device_id = ?1",
                rusqlite::params![device_id],
                |row| row.get(0),
            )
            .map_err(|_| anyhow::anyhow!("Device not found"))?;

        match stored_hash {
            None => Ok(true), // No pairing code set → open access
            Some(h) => {
                let attempt = hash_password(code, device_id);
                Ok(constant_time_eq(h.as_bytes(), attempt.as_bytes()))
            }
        }
    }

    /// Check if a device has a pairing code set.
    pub fn device_has_pairing_code(&self, device_id: &str) -> Result<bool> {
        let conn = self.conn.lock();
        let hash: Option<String> = conn
            .query_row(
                "SELECT pairing_code_hash FROM devices WHERE device_id = ?1",
                rusqlite::params![device_id],
                |row| row.get(0),
            )
            .map_err(|_| anyhow::anyhow!("Device not found"))?;
        Ok(hash.is_some())
    }

    /// List all devices for a user, including online status.
    /// A device is considered online if last_seen is within `online_threshold_secs`.
    pub fn list_devices_with_status(
        &self,
        user_id: &str,
        online_threshold_secs: u64,
    ) -> Result<Vec<DeviceWithStatus>> {
        let conn = self.conn.lock();
        let now = epoch_secs() as i64;
        let cutoff = now - online_threshold_secs as i64;

        let mut stmt = conn.prepare_cached(
            "SELECT device_id, user_id, device_name, platform, last_seen, pairing_code_hash, fingerprint
             FROM devices WHERE user_id = ?1 ORDER BY last_seen DESC",
        )?;
        let devices = stmt
            .query_map(rusqlite::params![user_id], |row| {
                let last_seen: i64 = row.get(4)?;
                let pairing_code_hash: Option<String> = row.get(5)?;
                Ok(DeviceWithStatus {
                    device_id: row.get(0)?,
                    user_id: row.get(1)?,
                    device_name: row.get(2)?,
                    platform: row.get(3)?,
                    last_seen,
                    is_online: last_seen > cutoff,
                    has_pairing_code: pairing_code_hash.is_some(),
                    fingerprint: row.get(6)?,
                })
            })?
            .collect::<Result<Vec<_>, _>>()?;
        Ok(devices)
    }

    /// Count registered users.
    pub fn user_count(&self) -> Result<u64> {
        let conn = self.conn.lock();
        let count: i64 = conn.query_row("SELECT COUNT(*) FROM users", [], |row| row.get(0))?;
        Ok(u64::try_from(count).unwrap_or(0))
    }

    // ── User-bound Sync Master Key (PR-A2 — D1+D2 wiring) ──────────
    //
    // The cross-device memory sync uses a SINGLE 32-byte master key
    // per user account, shared across all of that user's devices via
    // the pairing channel. These three methods are the storage seam:
    //
    //   1. `get_user_sync_key(user_id)`           — read on device add
    //   2. `set_user_sync_key(user_id, key)`      — first-device init
    //                                               or rotation
    //   3. `get_or_create_user_sync_key(user_id)` — convenience for the
    //                                               common path (read,
    //                                               or generate+store
    //                                               if missing)
    //
    // The transmission of the key over the pairing channel is the
    // gateway/pairing module's responsibility (separate PR); these
    // methods are the storage primitive that work owns.
    //
    // SECURITY: the key is the keying material that decrypts every
    // sync delta the user ever produces. Never log it; never echo it
    // to clients except as the explicit "I'm pairing this device"
    // response. The base64 representation here is a wire-format
    // convenience, not a secrecy claim — the key is a secret either
    // way.

    /// Read the user's master sync key, base64-decoded into a 32-byte
    /// array. Returns `None` if the user has never had a key set
    /// (which happens for accounts created before this column existed
    /// AND for newly-created accounts that haven't run a sync handshake
    /// yet). Returns `Err` if the column row exists but is malformed
    /// (wrong base64, wrong byte count) — that condition signals
    /// either tampering or a partial migration and should NOT be
    /// silently ignored.
    pub fn get_user_sync_key(&self, user_id: &str) -> Result<Option<[u8; 32]>> {
        use base64::engine::general_purpose::STANDARD_NO_PAD;
        use base64::Engine;
        let conn = self.conn.lock();
        let row: rusqlite::Result<String> = conn.query_row(
            "SELECT key_b64 FROM user_sync_keys WHERE user_id = ?1",
            rusqlite::params![user_id],
            |row| row.get(0),
        );
        match row {
            Ok(b64) => {
                let bytes = STANDARD_NO_PAD
                    .decode(b64.as_bytes())
                    .map_err(|e| anyhow::anyhow!("user_sync_keys.key_b64 not valid base64: {e}"))?;
                if bytes.len() != 32 {
                    bail!(
                        "user_sync_keys.key_b64 decoded to {} bytes, expected 32",
                        bytes.len()
                    );
                }
                let mut out = [0u8; 32];
                out.copy_from_slice(&bytes);
                Ok(Some(out))
            }
            Err(rusqlite::Error::QueryReturnedNoRows) => Ok(None),
            Err(e) => Err(e.into()),
        }
    }

    /// Write the user's master sync key. Replaces any existing row
    /// (rotation case). The key is base64-encoded for storage.
    pub fn set_user_sync_key(&self, user_id: &str, key: &[u8; 32]) -> Result<()> {
        use base64::engine::general_purpose::STANDARD_NO_PAD;
        use base64::Engine;
        let key_b64 = STANDARD_NO_PAD.encode(key);
        let now = epoch_secs() as i64;
        let conn = self.conn.lock();
        conn.execute(
            "INSERT INTO user_sync_keys (user_id, key_b64, created_at)
             VALUES (?1, ?2, ?3)
             ON CONFLICT(user_id) DO UPDATE SET key_b64 = excluded.key_b64,
                                                 created_at = excluded.created_at",
            rusqlite::params![user_id, key_b64, now],
        )?;
        Ok(())
    }

    /// Convenience: read the user's master sync key, or generate +
    /// persist a fresh CSPRNG key if none exists. The "first device
    /// joins the account" flow uses this; subsequent devices use
    /// `get_user_sync_key` (returning the same value) plus the
    /// pairing channel to receive it.
    pub fn get_or_create_user_sync_key(&self, user_id: &str) -> Result<[u8; 32]> {
        if let Some(existing) = self.get_user_sync_key(user_id)? {
            return Ok(existing);
        }
        let mut key = [0u8; 32];
        rand::fill(&mut key);
        self.set_user_sync_key(user_id, &key)?;
        Ok(key)
    }

    // ── User Email Management ────────────────────────────────────────

    /// Set or update the email address for a user.
    pub fn set_user_email(&self, user_id: &str, email: &str) -> Result<()> {
        let trimmed = email.trim();
        if trimmed.is_empty() {
            bail!("Email cannot be empty");
        }
        let conn = self.conn.lock();
        let updated = conn.execute(
            "UPDATE users SET email = ?1 WHERE id = ?2",
            rusqlite::params![trimmed, user_id],
        )?;
        if updated == 0 {
            bail!("User not found");
        }
        Ok(())
    }

    /// Get the email address for a user. Returns None if not set.
    pub fn get_user_email(&self, user_id: &str) -> Result<Option<String>> {
        let conn = self.conn.lock();
        let email: Option<String> = conn
            .query_row(
                "SELECT email FROM users WHERE id = ?1",
                rusqlite::params![user_id],
                |row| row.get(0),
            )
            .map_err(|_| anyhow::anyhow!("User not found"))?;
        Ok(email)
    }

    // ── Channel Linking ─────────────────────────────────────────────

    /// Ensure the channel_links table exists (safe to call multiple times).
    pub fn ensure_channel_links_table(&self) -> Result<()> {
        let conn = self.conn.lock();
        conn.execute_batch(
            "CREATE TABLE IF NOT EXISTS channel_links (
                channel TEXT NOT NULL,
                platform_uid TEXT NOT NULL,
                user_id TEXT NOT NULL REFERENCES users(id) ON DELETE CASCADE,
                device_id TEXT,
                autonomy_mode TEXT NOT NULL DEFAULT 'read_only',
                linked_at INTEGER NOT NULL,
                PRIMARY KEY (channel, platform_uid)
            );
            CREATE INDEX IF NOT EXISTS idx_channel_links_user ON channel_links(user_id);",
        )?;
        // Migration: add device_id column if missing (existing installs)
        let table_sql: String = conn
            .query_row(
                "SELECT sql FROM sqlite_master WHERE type='table' AND name='channel_links'",
                [],
                |row| row.get(0),
            )
            .unwrap_or_default();
        if !table_sql.contains("device_id") {
            let _ = conn.execute_batch("ALTER TABLE channel_links ADD COLUMN device_id TEXT;");
        }
        if !table_sql.contains("autonomy_mode") {
            let _ = conn.execute_batch(
                "ALTER TABLE channel_links ADD COLUMN autonomy_mode TEXT NOT NULL DEFAULT 'read_only';",
            );
        }
        Ok(())
    }

    /// Link a messaging channel identity to an authenticated MoA user and device.
    pub fn link_channel(
        &self,
        channel: &str,
        platform_uid: &str,
        user_id: &str,
        device_id: Option<&str>,
    ) -> Result<()> {
        let conn = self.conn.lock();
        let now = epoch_secs();
        conn.execute(
            "INSERT INTO channel_links (channel, platform_uid, user_id, device_id, autonomy_mode, linked_at)
             VALUES (?1, ?2, ?3, ?4, 'read_only', ?5)
             ON CONFLICT(channel, platform_uid) DO UPDATE SET
                user_id = excluded.user_id,
                device_id = excluded.device_id,
                linked_at = excluded.linked_at",
            rusqlite::params![channel, platform_uid, user_id, device_id, now as i64],
        )?;
        tracing::info!(
            channel = channel,
            platform_uid = platform_uid,
            device_id = device_id,
            "Channel identity linked"
        );
        Ok(())
    }

    /// Full channel link lookup — returns user, device_id, and autonomy mode.
    pub fn find_channel_link_full(
        &self,
        channel: &str,
        platform_uid: &str,
    ) -> Result<Option<ChannelLink>> {
        let conn = self.conn.lock();
        let row = conn.query_row(
            "SELECT cl.user_id, cl.device_id, cl.autonomy_mode,
                    u.id, u.username, u.email, u.created_at
             FROM channel_links cl
             JOIN users u ON cl.user_id = u.id
             WHERE cl.channel = ?1 AND cl.platform_uid = ?2",
            rusqlite::params![channel, platform_uid],
            |row| {
                Ok(ChannelLink {
                    user_id: row.get(0)?,
                    device_id: row.get(1)?,
                    autonomy_mode: row
                        .get::<_, String>(2)
                        .unwrap_or_else(|_| "read_only".into()),
                    user: User {
                        id: row.get(3)?,
                        username: row.get(4)?,
                        email: row.get(5)?,
                        created_at: row.get(6)?,
                    },
                })
            },
        );

        match row {
            Ok(link) => Ok(Some(link)),
            Err(rusqlite::Error::QueryReturnedNoRows) => Ok(None),
            Err(e) => Err(e.into()),
        }
    }

    /// Check if a channel identity is linked to any MoA user.
    pub fn find_channel_link(&self, channel: &str, platform_uid: &str) -> Result<Option<User>> {
        let conn = self.conn.lock();
        let row = conn.query_row(
            "SELECT u.id, u.username, u.email, u.created_at
             FROM channel_links cl
             JOIN users u ON cl.user_id = u.id
             WHERE cl.channel = ?1 AND cl.platform_uid = ?2",
            rusqlite::params![channel, platform_uid],
            |row| {
                Ok(User {
                    id: row.get(0)?,
                    username: row.get(1)?,
                    email: row.get(2)?,
                    created_at: row.get(3)?,
                })
            },
        );

        match row {
            Ok(user) => Ok(Some(user)),
            Err(rusqlite::Error::QueryReturnedNoRows) => Ok(None),
            Err(e) => Err(e.into()),
        }
    }

    /// Reverse lookup: find a channel platform_uid for a given user_id.
    ///
    /// Returns the platform_uid (e.g. Kakao ID) linked to this user on the
    /// given channel, if any.
    pub fn get_channel_uid_for_user(&self, channel: &str, user_id: &str) -> Result<Option<String>> {
        let conn = self.conn.lock();
        let row = conn.query_row(
            "SELECT platform_uid FROM channel_links WHERE channel = ?1 AND user_id = ?2",
            rusqlite::params![channel, user_id],
            |row| row.get::<_, String>(0),
        );

        match row {
            Ok(uid) => Ok(Some(uid)),
            Err(rusqlite::Error::QueryReturnedNoRows) => Ok(None),
            Err(e) => Err(e.into()),
        }
    }

    /// Update the target device for an existing channel link.
    pub fn update_channel_device(
        &self,
        channel: &str,
        platform_uid: &str,
        device_id: &str,
    ) -> Result<bool> {
        let conn = self.conn.lock();
        let updated = conn.execute(
            "UPDATE channel_links SET device_id = ?3 WHERE channel = ?1 AND platform_uid = ?2",
            rusqlite::params![channel, platform_uid, device_id],
        )?;
        Ok(updated > 0)
    }

    /// Toggle autonomy mode for a channel link (read_only / full).
    pub fn set_channel_autonomy_mode(
        &self,
        channel: &str,
        platform_uid: &str,
        mode: &str,
    ) -> Result<bool> {
        let conn = self.conn.lock();
        let updated = conn.execute(
            "UPDATE channel_links SET autonomy_mode = ?3 WHERE channel = ?1 AND platform_uid = ?2",
            rusqlite::params![channel, platform_uid, mode],
        )?;
        Ok(updated > 0)
    }

    /// List all channel links for a user.
    pub fn list_user_channels(&self, user_id: &str) -> Result<Vec<UserChannelLink>> {
        let conn = self.conn.lock();
        let mut stmt = conn.prepare(
            "SELECT channel, platform_uid, device_id, autonomy_mode, linked_at
             FROM channel_links WHERE user_id = ?1 ORDER BY linked_at DESC",
        )?;
        let rows = stmt.query_map(rusqlite::params![user_id], |row| {
            Ok(UserChannelLink {
                channel: row.get(0)?,
                platform_uid: row.get(1)?,
                device_id: row.get(2)?,
                autonomy_mode: row.get::<_, String>(3).unwrap_or_else(|_| "read_only".into()),
                linked_at: row.get(4)?,
            })
        })?;
        rows.collect::<std::result::Result<Vec<_>, _>>()
            .map_err(|e| e.into())
    }

    /// Remove channel link (unlink).
    pub fn unlink_channel(&self, channel: &str, platform_uid: &str) -> Result<bool> {
        let conn = self.conn.lock();
        let deleted = conn.execute(
            "DELETE FROM channel_links WHERE channel = ?1 AND platform_uid = ?2",
            rusqlite::params![channel, platform_uid],
        )?;
        Ok(deleted > 0)
    }

    // ── Admin Operations ────────────────────────────────────────────

    /// Change admin password.
    pub fn change_admin_password(&self, username: &str, new_password: &str) -> Result<()> {
        if new_password.len() < 4 {
            bail!("Password must be at least 4 characters");
        }
        let salt = generate_salt();
        let hash = hash_password(new_password, &salt);
        let conn = self.conn.lock();
        let updated = conn.execute(
            "UPDATE admins SET password_hash = ?1, salt = ?2 WHERE username = ?3",
            rusqlite::params![hash, salt, username.trim()],
        )?;
        if updated == 0 {
            bail!("Admin user not found");
        }
        Ok(())
    }

    /// Authenticate an admin user.
    pub fn authenticate_admin(&self, username: &str, password: &str) -> Result<bool> {
        let conn = self.conn.lock();
        let row = conn.query_row(
            "SELECT password_hash, salt FROM admins WHERE username = ?1",
            rusqlite::params![username.trim()],
            |row| Ok((row.get::<_, String>(0)?, row.get::<_, String>(1)?)),
        );
        match row {
            Ok((stored_hash, salt)) => {
                let candidate = hash_password(password, &salt);
                Ok(constant_time_eq(candidate.as_bytes(), stored_hash.as_bytes()))
            }
            Err(rusqlite::Error::QueryReturnedNoRows) => Ok(false),
            Err(e) => Err(e.into()),
        }
    }

    /// List all registered users with device count and online status.
    pub fn list_all_users(&self, online_threshold_secs: u64) -> Result<Vec<AdminUserInfo>> {
        let conn = self.conn.lock();
        let now = epoch_secs() as i64;
        let threshold = now - online_threshold_secs as i64;

        let mut stmt = conn.prepare(
            "SELECT u.id, u.username, u.email, u.created_at,
                    (SELECT COUNT(*) FROM devices d WHERE d.user_id = u.id) AS device_count,
                    (SELECT COUNT(*) FROM devices d WHERE d.user_id = u.id AND d.last_seen > ?1) AS online_count,
                    (SELECT MAX(d.last_seen) FROM devices d WHERE d.user_id = u.id) AS last_active
             FROM users u
             ORDER BY u.created_at DESC",
        )?;

        let rows = stmt.query_map(rusqlite::params![threshold], |row| {
            Ok(AdminUserInfo {
                user_id: row.get(0)?,
                username: row.get(1)?,
                email: row.get(2)?,
                created_at: row.get(3)?,
                device_count: row.get(4)?,
                online_device_count: row.get(5)?,
                last_active: row.get(6)?,
            })
        })?;

        rows.collect::<std::result::Result<Vec<_>, _>>()
            .map_err(|e| e.into())
    }

    /// Record a usage event for analytics.
    pub fn record_usage(&self, user_id: &str, category: &str, chars: i64) -> Result<()> {
        let conn = self.conn.lock();
        let now = epoch_secs() as i64;
        conn.execute(
            "INSERT INTO usage_stats (user_id, category, event_count, total_chars, recorded_at)
             VALUES (?1, ?2, 1, ?3, ?4)",
            rusqlite::params![user_id, category, chars, now],
        )?;
        Ok(())
    }

    /// Aggregate usage stats for all users grouped by category.
    pub fn usage_summary(&self, since_secs: u64) -> Result<Vec<UsageStat>> {
        let conn = self.conn.lock();
        let since = (epoch_secs() - since_secs) as i64;
        let mut stmt = conn.prepare(
            "SELECT category,
                    SUM(event_count) AS total_events,
                    SUM(total_chars) AS total_chars,
                    COUNT(DISTINCT user_id) AS unique_users
             FROM usage_stats
             WHERE recorded_at > ?1
             GROUP BY category
             ORDER BY total_events DESC",
        )?;
        let rows = stmt.query_map(rusqlite::params![since], |row| {
            Ok(UsageStat {
                category: row.get(0)?,
                total_events: row.get(1)?,
                total_chars: row.get(2)?,
                unique_users: row.get(3)?,
            })
        })?;
        rows.collect::<std::result::Result<Vec<_>, _>>()
            .map_err(|e| e.into())
    }

    /// Per-user usage stats for the admin dashboard.
    pub fn per_user_usage(&self, since_secs: u64) -> Result<Vec<UserUsageStat>> {
        let conn = self.conn.lock();
        let since = (epoch_secs() - since_secs) as i64;
        let mut stmt = conn.prepare(
            "SELECT u.username, us.category,
                    SUM(us.event_count) AS events,
                    SUM(us.total_chars) AS chars
             FROM usage_stats us
             JOIN users u ON us.user_id = u.id
             WHERE us.recorded_at > ?1
             GROUP BY u.username, us.category
             ORDER BY events DESC",
        )?;
        let rows = stmt.query_map(rusqlite::params![since], |row| {
            Ok(UserUsageStat {
                username: row.get(0)?,
                category: row.get(1)?,
                events: row.get(2)?,
                chars: row.get(3)?,
            })
        })?;
        rows.collect::<std::result::Result<Vec<_>, _>>()
            .map_err(|e| e.into())
    }

    /// Active sessions (who is logged in right now).
    pub fn active_sessions(&self) -> Result<Vec<ActiveSession>> {
        let conn = self.conn.lock();
        let now = epoch_secs() as i64;
        let mut stmt = conn.prepare(
            "SELECT u.username, s.device_id, s.device_name, s.created_at, s.expires_at
             FROM sessions s
             JOIN users u ON s.user_id = u.id
             WHERE s.expires_at > ?1
             ORDER BY s.created_at DESC",
        )?;
        let rows = stmt.query_map(rusqlite::params![now], |row| {
            Ok(ActiveSession {
                username: row.get(0)?,
                device_id: row.get(1)?,
                device_name: row.get(2)?,
                logged_in_at: row.get(3)?,
                expires_at: row.get(4)?,
            })
        })?;
        rows.collect::<std::result::Result<Vec<_>, _>>()
            .map_err(|e| e.into())
    }
}

/// Admin dashboard user info.
#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct AdminUserInfo {
    pub user_id: String,
    pub username: String,
    pub email: Option<String>,
    pub created_at: i64,
    pub device_count: i64,
    pub online_device_count: i64,
    pub last_active: Option<i64>,
}

/// Aggregated usage stat by category.
#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct UsageStat {
    pub category: String,
    pub total_events: i64,
    pub total_chars: i64,
    pub unique_users: i64,
}

/// Per-user usage stat.
#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct UserUsageStat {
    pub username: String,
    pub category: String,
    pub events: i64,
    pub chars: i64,
}

/// Active session info.
#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct ActiveSession {
    pub username: String,
    pub device_id: Option<String>,
    pub device_name: Option<String>,
    pub logged_in_at: i64,
    pub expires_at: i64,
}

/// Channel link info for user-facing display.
#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct UserChannelLink {
    pub channel: String,
    pub platform_uid: String,
    pub device_id: Option<String>,
    pub autonomy_mode: String,
    pub linked_at: i64,
}

/// Full channel link record — user identity + target device + autonomy mode.
#[derive(Debug, Clone)]
pub struct ChannelLink {
    pub user_id: String,
    pub device_id: Option<String>,
    pub autonomy_mode: String,
    pub user: User,
}

// ── Cryptographic Helpers ───────────────────────────────────────────

/// Generate a random salt (hex-encoded).
fn generate_salt() -> String {
    let bytes: [u8; SALT_BYTES] = rand::random();
    hex::encode(bytes)
}

/// Generate a random session token (hex-encoded).
fn generate_token() -> String {
    let bytes: [u8; TOKEN_BYTES] = rand::random();
    hex::encode(bytes)
}

/// Hash a password with salt using iterated SHA-256.
fn hash_password(password: &str, salt: &str) -> String {
    let mut hash = Sha256::new();
    hash.update(salt.as_bytes());
    hash.update(password.as_bytes());
    let mut result = hash.finalize();

    // Iterated hashing for key stretching
    for _ in 1..HASH_ITERATIONS {
        let mut h = Sha256::new();
        h.update(result);
        h.update(salt.as_bytes());
        result = h.finalize();
    }

    hex::encode(result)
}

/// Hash a session token (SHA-256, single pass — tokens are already high-entropy).
fn hash_token(token: &str) -> String {
    let mut h = Sha256::new();
    h.update(token.as_bytes());
    hex::encode(h.finalize())
}

/// Constant-time byte comparison to prevent timing attacks.
///
/// The previous implementation returned early on a length mismatch,
/// leaking the stored hash length via response time (a timing oracle
/// on password-hash length, which constrains the attacker's search
/// space). This version always iterates `max(a.len, b.len)` bytes,
/// XORing each pair (treating out-of-bounds positions as 0) plus
/// folding in the length difference, then collapses the accumulator
/// to a single bool. Equality is byte-for-byte identical to the
/// previous implementation when the inputs are the same length;
/// the only behavior change is that mismatched-length inputs now
/// take the same time as matched-length ones.
///
/// Mirrors the correct implementation already shipped in
/// `src/security/pairing.rs::constant_time_eq` (lines 439-456).
fn constant_time_eq(a: &[u8], b: &[u8]) -> bool {
    let max_len = a.len().max(b.len());
    // Iterate the longer of the two, treating out-of-bounds positions
    // as 0. This keeps total work constant per max_len regardless of
    // which input is longer.
    let mut byte_diff: u8 = 0;
    for i in 0..max_len {
        let ax = *a.get(i).unwrap_or(&0);
        let by = *b.get(i).unwrap_or(&0);
        byte_diff |= ax ^ by;
    }
    // Fold the length difference into the verdict via a single
    // boolean OR. `len_diff != 0` is true iff the lengths differ;
    // we OR that into the byte-level diff so unequal-length inputs
    // can never compare equal even when the longer suffix is all zero.
    let len_diff_nonzero = a.len() ^ b.len() != 0;
    (byte_diff != 0) | len_diff_nonzero == false
}

/// Current Unix epoch in seconds.
fn epoch_secs() -> u64 {
    SystemTime::now()
        .duration_since(UNIX_EPOCH)
        .unwrap_or_default()
        .as_secs()
}

// ── Tests ───────────────────────────────────────────────────────────

#[cfg(test)]
mod tests {
    use super::*;
    use tempfile::TempDir;

    fn test_store() -> (TempDir, AuthStore) {
        let tmp = TempDir::new().unwrap();
        let db_path = tmp.path().join("auth.db");
        let store = AuthStore::new(&db_path, Some(3600)).unwrap();
        (tmp, store)
    }

    #[test]
    fn register_and_authenticate() {
        let (_tmp, store) = test_store();

        let user_id = store.register("test_user", "securepassword123").unwrap();
        assert!(!user_id.is_empty());

        let user = store
            .authenticate("test_user", "securepassword123")
            .unwrap();
        assert_eq!(user.id, user_id);
        assert_eq!(user.username, "test_user");
    }

    #[test]
    fn register_duplicate_username_fails() {
        let (_tmp, store) = test_store();

        store.register("test_user", "password123!").unwrap();
        let result = store.register("test_user", "otherpassword1");
        assert!(result.is_err());
        assert!(result.unwrap_err().to_string().contains("already taken"));
    }

    #[test]
    fn register_case_insensitive_duplicate_fails() {
        let (_tmp, store) = test_store();

        store.register("TestUser", "password123!").unwrap();
        let result = store.register("testuser", "otherpassword1");
        assert!(result.is_err());
    }

    #[test]
    fn authenticate_wrong_password_fails() {
        let (_tmp, store) = test_store();

        store.register("test_user", "correct_password").unwrap();
        let result = store.authenticate("test_user", "wrong_password");
        assert!(result.is_err());
        assert!(result.unwrap_err().to_string().contains("Invalid"));
    }

    #[test]
    fn authenticate_nonexistent_user_fails() {
        let (_tmp, store) = test_store();

        let result = store.authenticate("ghost_user", "anypassword1");
        assert!(result.is_err());
        assert!(result.unwrap_err().to_string().contains("Invalid"));
    }

    #[test]
    fn register_empty_username_fails() {
        let (_tmp, store) = test_store();

        let result = store.register("", "password123!");
        assert!(result.is_err());
        assert!(result.unwrap_err().to_string().contains("empty"));
    }

    #[test]
    fn register_short_password_fails() {
        let (_tmp, store) = test_store();

        let result = store.register("test_user", "short");
        assert!(result.is_err());
        assert!(result.unwrap_err().to_string().contains("8 characters"));
    }

    #[test]
    fn session_create_and_validate() {
        let (_tmp, store) = test_store();

        let user_id = store.register("test_user", "securepassword123").unwrap();
        let token = store.create_session(&user_id, None, None).unwrap();
        assert!(!token.is_empty());

        let session = store.validate_session(&token);
        assert!(session.is_some());
        assert_eq!(session.unwrap().user_id, user_id);
    }

    #[test]
    fn session_invalid_token_returns_none() {
        let (_tmp, store) = test_store();

        let session = store.validate_session("invalid_token_value");
        assert!(session.is_none());
    }

    #[test]
    fn session_revoke() {
        let (_tmp, store) = test_store();

        let user_id = store.register("test_user", "securepassword123").unwrap();
        let token = store.create_session(&user_id, None, None).unwrap();

        assert!(store.validate_session(&token).is_some());
        assert!(store.revoke_session(&token).unwrap());
        assert!(store.validate_session(&token).is_none());
    }

    #[test]
    fn session_revoke_all_for_user() {
        let (_tmp, store) = test_store();

        let user_id = store.register("test_user", "securepassword123").unwrap();
        let t1 = store.create_session(&user_id, None, None).unwrap();
        let t2 = store.create_session(&user_id, None, None).unwrap();

        assert!(store.validate_session(&t1).is_some());
        assert!(store.validate_session(&t2).is_some());

        let count = store.revoke_all_sessions(&user_id).unwrap();
        assert_eq!(count, 2);

        assert!(store.validate_session(&t1).is_none());
        assert!(store.validate_session(&t2).is_none());
    }

    #[test]
    fn session_with_device_info() {
        let (_tmp, store) = test_store();

        let user_id = store.register("test_user", "securepassword123").unwrap();
        let token = store
            .create_session(&user_id, Some("device_abc"), Some("My Phone"))
            .unwrap();

        let session = store.validate_session(&token).unwrap();
        assert_eq!(session.device_id.as_deref(), Some("device_abc"));
        assert_eq!(session.device_name.as_deref(), Some("My Phone"));
    }

    #[test]
    fn device_register_and_list() {
        let (_tmp, store) = test_store();

        let user_id = store.register("test_user", "securepassword123").unwrap();

        store
            .register_device(&user_id, "dev_1", "Phone", Some("android"), None)
            .unwrap();
        store
            .register_device(&user_id, "dev_2", "Laptop", Some("linux"), None)
            .unwrap();

        let devices = store.list_devices(&user_id).unwrap();
        assert_eq!(devices.len(), 2);
    }

    #[test]
    fn device_update_on_conflict() {
        let (_tmp, store) = test_store();

        let user_id = store.register("test_user", "securepassword123").unwrap();

        store
            .register_device(&user_id, "dev_1", "Old Name", None, None)
            .unwrap();
        store
            .register_device(&user_id, "dev_1", "New Name", Some("ios"), None)
            .unwrap();

        let devices = store.list_devices(&user_id).unwrap();
        assert_eq!(devices.len(), 1);
        assert_eq!(devices[0].device_name, "New Name");
    }

    #[test]
    fn device_fingerprint_dedup() {
        let (_tmp, store) = test_store();

        let user_id = store.register("test_user", "securepassword123").unwrap();

        // Register device with fingerprint
        let id1 = store
            .register_device(
                &user_id,
                "dev_1",
                "Phone",
                Some("android"),
                Some("fp_abc123"),
            )
            .unwrap();
        assert_eq!(id1, "dev_1");

        // Register with a different device_id but same fingerprint → should reuse dev_1
        let id2 = store
            .register_device(
                &user_id,
                "dev_new",
                "Phone Reinstalled",
                Some("android"),
                Some("fp_abc123"),
            )
            .unwrap();
        assert_eq!(id2, "dev_1");

        // Should still be only 1 device
        let devices = store.list_devices(&user_id).unwrap();
        assert_eq!(devices.len(), 1);
        assert_eq!(devices[0].device_name, "Phone Reinstalled");
    }

    #[test]
    fn device_remove() {
        let (_tmp, store) = test_store();

        let user_id = store.register("test_user", "securepassword123").unwrap();
        store
            .register_device(&user_id, "dev_1", "Phone", None, None)
            .unwrap();

        assert!(store.remove_device(&user_id, "dev_1").unwrap());
        assert!(!store.remove_device(&user_id, "dev_1").unwrap());

        let devices = store.list_devices(&user_id).unwrap();
        assert!(devices.is_empty());
    }

    #[test]
    fn user_count_tracks_registrations() {
        let (_tmp, store) = test_store();

        assert_eq!(store.user_count().unwrap(), 0);
        store.register("user_a", "password123!").unwrap();
        assert_eq!(store.user_count().unwrap(), 1);
        store.register("user_b", "password456!").unwrap();
        assert_eq!(store.user_count().unwrap(), 2);
    }

    #[test]
    fn get_user_by_id() {
        let (_tmp, store) = test_store();

        let user_id = store.register("test_user", "securepassword123").unwrap();
        let user = store.get_user(&user_id).unwrap();
        assert!(user.is_some());
        assert_eq!(user.unwrap().username, "test_user");

        let none = store.get_user("nonexistent_id").unwrap();
        assert!(none.is_none());
    }

    #[test]
    fn password_hash_is_deterministic_with_same_salt() {
        let h1 = hash_password("test_password", "fixed_salt_value");
        let h2 = hash_password("test_password", "fixed_salt_value");
        assert_eq!(h1, h2);
    }

    #[test]
    fn password_hash_differs_with_different_salt() {
        let h1 = hash_password("test_password", "salt_a");
        let h2 = hash_password("test_password", "salt_b");
        assert_ne!(h1, h2);
    }

    #[test]
    fn constant_time_eq_works() {
        assert!(constant_time_eq(b"hello", b"hello"));
        assert!(!constant_time_eq(b"hello", b"world"));
        assert!(!constant_time_eq(b"short", b"longer"));
    }

    #[test]
    fn constant_time_eq_handles_length_mismatch_safely() {
        // Regression: previous implementation returned early on length
        // mismatch, leaking length via response time. New implementation
        // must still return false for unequal lengths.
        assert!(!constant_time_eq(b"", b"a"));
        assert!(!constant_time_eq(b"a", b""));
        assert!(!constant_time_eq(b"abc", b"abcd"));
        assert!(!constant_time_eq(b"abcd", b"abc"));
        // And — crucially — an all-zero-suffix longer side must NOT
        // compare equal to its prefix.
        assert!(!constant_time_eq(b"abc\0\0", b"abc"));
        assert!(!constant_time_eq(b"abc", b"abc\0\0"));
        // Same-length zero bytes should still match.
        assert!(constant_time_eq(b"\0\0\0", b"\0\0\0"));
    }

    #[test]
    fn set_password_rejects_short_password() {
        // Regression for D4: set_password used to allow 4+ chars while
        // register required 8+. That was a downgrade path. Both paths
        // now require >= 8.
        let (_tmp, store) = test_store();
        let user_id = store.register("test_user", "initialpw1").unwrap();
        let too_short = store.set_password(&user_id, "1234567");
        assert!(too_short.is_err(), "set_password must reject < 8 chars");
        assert!(too_short.unwrap_err().to_string().contains("at least 8"));
        let exactly_eight = store.set_password(&user_id, "12345678");
        assert!(exactly_eight.is_ok(), "set_password must accept exactly 8 chars");
    }

    // ── PR-A2: user-bound sync master key tests (D1+D2 storage) ──

    #[test]
    fn user_sync_key_round_trip() {
        let (_tmp, store) = test_store();
        let user_id = store.register("alice", "initialpw1").unwrap();

        // Initially no key.
        assert!(store.get_user_sync_key(&user_id).unwrap().is_none());

        // Set a known key, read it back identical.
        let original = [42u8; 32];
        store.set_user_sync_key(&user_id, &original).unwrap();
        let read_back = store.get_user_sync_key(&user_id).unwrap();
        assert_eq!(read_back, Some(original));
    }

    #[test]
    fn user_sync_key_replaces_on_rotation() {
        let (_tmp, store) = test_store();
        let user_id = store.register("alice", "initialpw1").unwrap();

        let v1 = [1u8; 32];
        let v2 = [2u8; 32];
        store.set_user_sync_key(&user_id, &v1).unwrap();
        store.set_user_sync_key(&user_id, &v2).unwrap();
        assert_eq!(store.get_user_sync_key(&user_id).unwrap(), Some(v2));
    }

    #[test]
    fn get_or_create_returns_same_key_on_repeat() {
        let (_tmp, store) = test_store();
        let user_id = store.register("alice", "initialpw1").unwrap();

        let k1 = store.get_or_create_user_sync_key(&user_id).unwrap();
        let k2 = store.get_or_create_user_sync_key(&user_id).unwrap();
        assert_eq!(
            k1, k2,
            "get_or_create must return the persisted key on second call, \
             not a fresh CSPRNG one — that would break cross-device sync"
        );
    }

    #[test]
    fn get_or_create_generates_nonzero_key_on_first_call() {
        let (_tmp, store) = test_store();
        let user_id = store.register("alice", "initialpw1").unwrap();
        let k = store.get_or_create_user_sync_key(&user_id).unwrap();
        // Probability of CSPRNG giving 32 zero bytes is ~ 2^-256.
        assert_ne!(k, [0u8; 32], "fresh key must not be all zeros");
    }

    #[test]
    fn user_sync_key_is_per_user_isolated() {
        let (_tmp, store) = test_store();
        let alice = store.register("alice", "alicepass1").unwrap();
        let bob = store.register("bob", "bobpassww1").unwrap();

        let alice_k = store.get_or_create_user_sync_key(&alice).unwrap();
        let bob_k = store.get_or_create_user_sync_key(&bob).unwrap();

        assert_ne!(
            alice_k, bob_k,
            "two CSPRNG-generated keys must differ (otherwise the RNG is broken \
             OR the get_or_create is sharing rows across users)"
        );
        assert_eq!(store.get_user_sync_key(&alice).unwrap(), Some(alice_k));
        assert_eq!(store.get_user_sync_key(&bob).unwrap(), Some(bob_k));
    }
}
