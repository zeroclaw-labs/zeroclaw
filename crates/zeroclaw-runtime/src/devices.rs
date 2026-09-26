//! The registry of paired devices, and the operations on paired-device
//! credentials that the core owns: listing, revoking one device or all of
//! them, and minting a new pairing code.
//!
//! The registry keeps an in-memory cache in front of its SQLite file, so every
//! reader and writer in the process must use one instance:
//! [`DeviceRegistry::shared`] returns it for a data directory. The gateway's
//! device routes and the `pairing/*` RPC methods both go through it.

use chrono::{DateTime, Utc};
use parking_lot::Mutex;
use rusqlite::Connection;
use serde::{Deserialize, Serialize};
use std::collections::HashMap;
use std::path::{Path, PathBuf};
use std::sync::Arc;

/// Metadata about a paired device.
#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct DeviceInfo {
    pub id: String,
    pub name: Option<String>,
    pub device_type: Option<String>,
    pub paired_at: DateTime<Utc>,
    pub last_seen: DateTime<Utc>,
    pub ip_address: Option<String>,
    /// macOS TCC permissions (and equivalent on other OSes) the device reports as granted.
    /// Pushed by the desktop app via POST /api/devices/me/capabilities.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub capabilities: Option<Vec<String>>,
}

/// Registry of paired devices backed by SQLite.
#[derive(Debug)]
pub struct DeviceRegistry {
    cache: Mutex<HashMap<String, DeviceInfo>>,
    db_path: PathBuf,
}

impl DeviceRegistry {
    /// The process's one registry for `data_dir`, opened on first use.
    /// Two instances over the same file would keep separate caches, so a
    /// revocation through one would leave the other still listing the
    /// device; every production caller takes this one.
    pub fn shared(data_dir: &Path) -> Arc<Self> {
        static REGISTRIES: std::sync::OnceLock<Mutex<HashMap<PathBuf, Arc<DeviceRegistry>>>> =
            std::sync::OnceLock::new();
        let registries = REGISTRIES.get_or_init(|| Mutex::new(HashMap::new()));
        let mut registries = registries.lock();
        Arc::clone(
            registries
                .entry(data_dir.to_path_buf())
                .or_insert_with(|| Arc::new(Self::new(data_dir))),
        )
    }

    pub fn new(workspace_dir: &Path) -> Self {
        let db_path = workspace_dir.join("devices.db");
        let conn = Connection::open(&db_path).expect("Failed to open device registry database");
        conn.execute_batch(
            "PRAGMA journal_mode = WAL;
             PRAGMA synchronous = NORMAL;
             PRAGMA temp_store = MEMORY;
             CREATE TABLE IF NOT EXISTS devices (
                token_hash TEXT PRIMARY KEY,
                id TEXT NOT NULL,
                name TEXT,
                device_type TEXT,
                paired_at TEXT NOT NULL,
                last_seen TEXT NOT NULL,
                ip_address TEXT,
                capabilities TEXT
            )",
        )
        .expect("Failed to create devices table");

        // Additive migration for DBs created before the capabilities column existed.
        // SQLite has no IF NOT EXISTS for columns; the duplicate-column error here is benign.
        let _ = conn.execute("ALTER TABLE devices ADD COLUMN capabilities TEXT", []);

        // Warm the in-memory cache from DB
        let mut cache = HashMap::new();
        let mut stmt = conn
            .prepare("SELECT token_hash, id, name, device_type, paired_at, last_seen, ip_address, capabilities FROM devices")
            .expect("Failed to prepare device select");
        let rows = stmt
            .query_map([], |row| {
                let token_hash: String = row.get(0)?;
                let id: String = row.get(1)?;
                let name: Option<String> = row.get(2)?;
                let device_type: Option<String> = row.get(3)?;
                let paired_at_str: String = row.get(4)?;
                let last_seen_str: String = row.get(5)?;
                let ip_address: Option<String> = row.get(6)?;
                let capabilities_json: Option<String> = row.get(7)?;
                let paired_at = DateTime::parse_from_rfc3339(&paired_at_str)
                    .map(|dt| dt.with_timezone(&Utc))
                    .unwrap_or_else(|_| Utc::now());
                let last_seen = DateTime::parse_from_rfc3339(&last_seen_str)
                    .map(|dt| dt.with_timezone(&Utc))
                    .unwrap_or_else(|_| Utc::now());
                let capabilities = capabilities_json
                    .as_deref()
                    .and_then(|s| serde_json::from_str::<Vec<String>>(s).ok());
                Ok((
                    token_hash,
                    DeviceInfo {
                        id,
                        name,
                        device_type,
                        paired_at,
                        last_seen,
                        ip_address,
                        capabilities,
                    },
                ))
            })
            .expect("Failed to query devices");
        for (hash, info) in rows.flatten() {
            cache.insert(hash, info);
        }

        Self {
            cache: Mutex::new(cache),
            db_path,
        }
    }

    /// A private registry over an explicit database file, for tests that
    /// must not share the process's instance. It opens the file as is and
    /// creates no schema.
    #[cfg(any(test, feature = "test-util"))]
    pub fn with_db_path(db_path: PathBuf) -> Self {
        Self {
            cache: Mutex::new(HashMap::new()),
            db_path,
        }
    }

    fn open_db(&self) -> Result<Connection, rusqlite::Error> {
        let conn = Connection::open(&self.db_path)?;
        conn.execute_batch(
            "PRAGMA journal_mode = WAL;
             PRAGMA synchronous = NORMAL;
             PRAGMA temp_store = MEMORY;",
        )?;
        Ok(conn)
    }

    pub fn register(&self, token_hash: String, info: DeviceInfo) -> Result<(), rusqlite::Error> {
        let capabilities_json = info
            .capabilities
            .as_ref()
            .and_then(|c| serde_json::to_string(c).ok());
        let conn = self.open_db()?;
        conn.execute(
            "INSERT OR REPLACE INTO devices (token_hash, id, name, device_type, paired_at, last_seen, ip_address, capabilities) VALUES (?1, ?2, ?3, ?4, ?5, ?6, ?7, ?8)",
            rusqlite::params![
                token_hash,
                info.id,
                info.name,
                info.device_type,
                info.paired_at.to_rfc3339(),
                info.last_seen.to_rfc3339(),
                info.ip_address,
                capabilities_json,
            ],
        )?;
        self.cache.lock().insert(token_hash, info);
        Ok(())
    }

    pub fn reconcile_from_token_hashes(
        &self,
        token_hashes: &[String],
    ) -> Result<usize, rusqlite::Error> {
        let conn = self.open_db()?;
        let mut cache = self.cache.lock();
        let now = Utc::now();
        let now_str = now.to_rfc3339();
        let mut inserted = 0usize;
        for token_hash in token_hashes {
            if cache.contains_key(token_hash) {
                continue;
            }
            let info = DeviceInfo {
                id: uuid::Uuid::new_v4().to_string(),
                name: None,
                device_type: Some("legacy".to_string()),
                paired_at: now,
                last_seen: now,
                ip_address: None,
                capabilities: None,
            };
            let affected = conn.execute(
                "INSERT OR IGNORE INTO devices (token_hash, id, name, device_type, paired_at, last_seen, ip_address, capabilities) VALUES (?1, ?2, ?3, ?4, ?5, ?6, ?7, ?8)",
                rusqlite::params![
                    token_hash,
                    info.id,
                    info.name,
                    info.device_type,
                    now_str,
                    now_str,
                    info.ip_address,
                    None::<String>,
                ],
            )?;
            if affected > 0 {
                cache.insert(token_hash.clone(), info);
                inserted += 1;
            }
        }
        Ok(inserted)
    }

    pub fn list(&self) -> Result<Vec<DeviceInfo>, rusqlite::Error> {
        let conn = self.open_db()?;
        let mut stmt = conn.prepare(
            "SELECT token_hash, id, name, device_type, paired_at, last_seen, ip_address, capabilities FROM devices",
        )?;
        let rows = stmt.query_map([], |row| {
            let id: String = row.get(1)?;
            let name: Option<String> = row.get(2)?;
            let device_type: Option<String> = row.get(3)?;
            let paired_at_str: String = row.get(4)?;
            let last_seen_str: String = row.get(5)?;
            let ip_address: Option<String> = row.get(6)?;
            let capabilities_json: Option<String> = row.get(7)?;
            let paired_at = DateTime::parse_from_rfc3339(&paired_at_str)
                .map(|dt| dt.with_timezone(&Utc))
                .unwrap_or_else(|_| Utc::now());
            let last_seen = DateTime::parse_from_rfc3339(&last_seen_str)
                .map(|dt| dt.with_timezone(&Utc))
                .unwrap_or_else(|_| Utc::now());
            let capabilities = capabilities_json
                .as_deref()
                .and_then(|s| serde_json::from_str::<Vec<String>>(s).ok());
            Ok(DeviceInfo {
                id,
                name,
                device_type,
                paired_at,
                last_seen,
                ip_address,
                capabilities,
            })
        })?;
        rows.collect()
    }

    pub fn revoke(&self, device_id: &str) -> Result<Option<String>, rusqlite::Error> {
        let conn = self.open_db()?;
        let deleted: Option<String> = conn
            .query_row(
                "DELETE FROM devices WHERE id = ?1 RETURNING token_hash",
                rusqlite::params![device_id],
                |row| row.get::<_, String>(0),
            )
            .map(Some)
            .or_else(|e| match e {
                rusqlite::Error::QueryReturnedNoRows => Ok(None),
                other => Err(other),
            })?;
        if let Some(hash) = deleted.as_ref() {
            self.cache.lock().remove(hash);
        }
        Ok(deleted)
    }

    /// Delete every device row and clear the in-memory cache. Returns the
    /// number of rows removed. Pairs with `PairingGuard::revoke_all_tokens`
    /// for the "rotate after compromise — nuke everything" path so the device
    /// registry does not silently coexist with the now-revoked token set.
    pub fn clear(&self) -> Result<usize, rusqlite::Error> {
        let conn = self.open_db()?;
        let removed = conn.execute("DELETE FROM devices", [])?;
        self.cache.lock().clear();
        Ok(removed)
    }

    pub fn update_last_seen(&self, token_hash: &str) {
        let now = Utc::now();
        // Last-seen is a best-effort touch — a write failure here is
        // observable (the row's last_seen stays stale) but does not affect
        // pairing or revocation, so swallow the error rather than poisoning
        // the caller.
        if let Ok(conn) = self.open_db() {
            let _ = conn.execute(
                "UPDATE devices SET last_seen = ?1 WHERE token_hash = ?2",
                rusqlite::params![now.to_rfc3339(), token_hash],
            );
        }
        if let Some(device) = self.cache.lock().get_mut(token_hash) {
            device.last_seen = now;
        }
    }

    pub fn update_capabilities(&self, token_hash: &str, capabilities: Vec<String>) -> bool {
        let json = serde_json::to_string(&capabilities).unwrap_or_else(|_| "[]".into());
        let conn = match self.open_db() {
            Ok(c) => c,
            Err(_) => return false,
        };
        let updated = conn
            .execute(
                "UPDATE devices SET capabilities = ?1, last_seen = ?2 WHERE token_hash = ?3",
                rusqlite::params![json, Utc::now().to_rfc3339(), token_hash],
            )
            .unwrap_or(0);
        if updated > 0
            && let Some(device) = self.cache.lock().get_mut(token_hash)
        {
            device.capabilities = Some(capabilities);
            device.last_seen = Utc::now();
        }
        updated > 0
    }

    pub fn device_count(&self) -> usize {
        self.cache.lock().len()
    }
}

// ── Paired-device operations the core owns ────────────────────────────────

use parking_lot::RwLock;
use serde_json::Value;
use zeroclaw_config::pairing::PairingGuard;
use zeroclaw_config::schema::Config;

/// A paired-device operation that did not complete: the status the dashboard
/// route answers with, and the message both surfaces carry.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct DeviceFailure {
    pub http_status: u16,
    pub message: String,
}

impl DeviceFailure {
    fn new(http_status: u16, message: impl Into<String>) -> Self {
        Self {
            http_status,
            message: message.into(),
        }
    }
}

/// The registry the core's pairing operations use: the process's one
/// instance for `config`'s data directory, when pairing is required. Without
/// pairing there are no paired devices to manage.
#[must_use]
pub fn registry_for(config: &Config, pairing: &PairingGuard) -> Option<Arc<DeviceRegistry>> {
    pairing
        .require_pairing()
        .then(|| DeviceRegistry::shared(&config.data_dir))
}

/// Persist the in-memory paired-token set to `config.toml` and the live
/// config, under the config write lock, so a revocation survives a restart.
pub async fn persist_pairing_tokens(
    config: Arc<RwLock<Config>>,
    pairing: &PairingGuard,
    config_write_lock: Arc<tokio::sync::Mutex<()>>,
) -> anyhow::Result<()> {
    use anyhow::Context;
    // Self-contained: no caller pre-reads config for modify, so this
    // acquires the witness itself rather than taking it as a param. Held
    // across the whole read-modify-save-swap below.
    let _guard = Arc::clone(&config_write_lock).lock_owned().await;
    debug_assert!(
        config_write_lock.try_lock().is_err(),
        "persist_pairing_tokens must hold config_write_lock across its read-modify-save-swap"
    );
    let paired_tokens = pairing.tokens();
    let mut updated_cfg = { config.read().clone() };
    updated_cfg.gateway.paired_tokens = paired_tokens;
    updated_cfg.mark_dirty("gateway.paired_tokens");
    updated_cfg
        .save_dirty()
        .await
        .context("Failed to persist paired tokens to config.toml")?;
    // Keep shared runtime config in sync with persisted tokens.
    *config.write() = updated_cfg;
    Ok(())
}

/// The paired-device list, `GET /api/devices` and `pairing/list`.
pub fn list_devices_body(registry: Option<&DeviceRegistry>) -> Result<Value, DeviceFailure> {
    let devices = match registry {
        Some(registry) => registry.list().map_err(|e| {
            ::zeroclaw_log::record!(
                ERROR,
                ::zeroclaw_log::Event::new(module_path!(), ::zeroclaw_log::Action::Fail)
                    .with_outcome(::zeroclaw_log::EventOutcome::Failure)
                    .with_attrs(::serde_json::json!({"error": format!("{e}")})),
                "device registry list failed"
            );
            DeviceFailure::new(500, format!("Device registry error: {e}"))
        })?,
        None => Vec::new(),
    };
    let count = devices.len();
    Ok(serde_json::json!({ "devices": devices, "count": count }))
}

/// Revoke one paired device and its bearer token, then persist the token set.
/// `DELETE /api/devices/{id}` and `pairing/revoke`.
pub async fn revoke_device(
    registry: Option<&DeviceRegistry>,
    pairing: &PairingGuard,
    config: Arc<RwLock<Config>>,
    config_write_lock: Arc<tokio::sync::Mutex<()>>,
    device_id: &str,
) -> Result<Value, DeviceFailure> {
    let Some(registry) = registry else {
        return Err(DeviceFailure::new(503, "Device registry is disabled"));
    };
    let token_hash = match registry.revoke(device_id) {
        Ok(Some(hash)) => hash,
        Ok(None) => return Err(DeviceFailure::new(404, "Device not found")),
        Err(e) => {
            return Err(DeviceFailure::new(
                500,
                format!("Device registry error: {e}"),
            ));
        }
    };
    pairing.revoke_token_hash(&token_hash);
    if let Err(e) = persist_pairing_tokens(config, pairing, config_write_lock).await {
        return Err(DeviceFailure::new(
            500,
            format!("Token revoked in memory but config persist failed: {e}"),
        ));
    }
    Ok(serde_json::json!({
        "message": "Device revoked and bearer token invalidated",
        "device_id": device_id,
    }))
}

/// Mint a new one-time pairing code, first revoking every paired token
/// (`rotate = "all"`) or one device's (`rotate = <device id>`) when asked.
/// Returns the status and body of `POST /admin/paircode/new`, which
/// `pairing/new-code` and `pairing/revoke-all` also serve.
pub async fn new_pairing_code(
    registry: Option<&DeviceRegistry>,
    pairing: &PairingGuard,
    config: Arc<RwLock<Config>>,
    config_write_lock: Arc<tokio::sync::Mutex<()>>,
    rotate: Option<&str>,
) -> (u16, Value) {
    let failure = |status: u16, pairing_required: bool, message: String| {
        (
            status,
            serde_json::json!({
                "success": false,
                "pairing_required": pairing_required,
                "pairing_code": null,
                "message": message,
            }),
        )
    };
    if !pairing.require_pairing() {
        return failure(400, false, "Pairing is disabled for this gateway".into());
    }
    let rotate = rotate.map(str::trim).filter(|s| !s.is_empty());

    let revocation_message = match rotate {
        Some("all") => {
            let revoked = pairing.revoke_all_tokens();
            if let Some(registry) = registry
                && let Err(e) = registry.clear()
            {
                return failure(
                    500,
                    true,
                    format!("Tokens revoked in memory but device registry clear failed: {e}"),
                );
            }
            if let Err(e) =
                persist_pairing_tokens(Arc::clone(&config), pairing, Arc::clone(&config_write_lock))
                    .await
            {
                return failure(
                    500,
                    true,
                    format!("Tokens revoked in memory but config persist failed: {e}"),
                );
            }
            ::zeroclaw_log::record!(
                INFO,
                ::zeroclaw_log::Event::new(module_path!(), ::zeroclaw_log::Action::Note)
                    .with_attrs(::serde_json::json!({"revoked": revoked})),
                "all paired tokens revoked"
            );
            Some(format!(
                "Revoked all {revoked} paired token(s) and cleared the device registry."
            ))
        }
        Some(device_id) => {
            let Some(registry) = registry else {
                return failure(
                    503,
                    true,
                    "Device registry is disabled; cannot rotate a single device.".into(),
                );
            };
            let token_hash = match registry.revoke(device_id) {
                Ok(Some(hash)) => hash,
                Ok(None) => {
                    return failure(
                        404,
                        true,
                        format!("Device '{device_id}' not found; nothing revoked."),
                    );
                }
                Err(e) => return failure(500, true, format!("Device registry error: {e}")),
            };
            pairing.revoke_token_hash(&token_hash);
            if let Err(e) =
                persist_pairing_tokens(Arc::clone(&config), pairing, Arc::clone(&config_write_lock))
                    .await
            {
                return failure(
                    500,
                    true,
                    format!("Token revoked in memory but config persist failed: {e}"),
                );
            }
            ::zeroclaw_log::record!(
                INFO,
                ::zeroclaw_log::Event::new(module_path!(), ::zeroclaw_log::Action::Note),
                "single device token revoked"
            );
            Some(format!(
                "Revoked the bearer token for device '{device_id}'."
            ))
        }
        None => None,
    };

    let code_policy = config.read().gateway.pairing_code;
    let Some(code) = pairing.generate_new_pairing_code(code_policy) else {
        return failure(500, true, "Pairing code generation failed".into());
    };
    if rotate.is_none() {
        ::zeroclaw_log::record!(
            INFO,
            ::zeroclaw_log::Event::new(module_path!(), ::zeroclaw_log::Action::Note),
            "new pairing code generated"
        );
    }
    let message = match revocation_message {
        Some(revoked) => format!("{revoked} Use this one-time code to re-pair."),
        None => "New pairing code generated — use this one-time code to pair".to_string(),
    };
    (
        200,
        serde_json::json!({
            "success": true,
            "pairing_required": true,
            "pairing_code": code,
            "message": message,
        }),
    )
}
