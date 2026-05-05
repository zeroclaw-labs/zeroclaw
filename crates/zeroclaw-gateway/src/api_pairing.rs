//! Device management and pairing API handlers.

use super::AppState;
use axum::{
    extract::State,
    http::{HeaderMap, StatusCode, header},
    response::{IntoResponse, Json},
};
use chrono::{DateTime, Utc};
use parking_lot::Mutex;
use rusqlite::Connection;
use serde::{Deserialize, Serialize};
use std::collections::HashMap;
use std::path::{Path, PathBuf};

/// Metadata about a paired device.
///
/// `hostname`, `os_name`, `os_version`, and `agent_version` are best-effort
/// identifiers captured during pairing (from explicit `X-Hostname` /
/// `X-OS-Name` / `X-OS-Version` / `X-Agent-Version` headers, with
/// User-Agent fallback for browsers). They're optional to keep the
/// pairing flow non-strict — the dashboard renders whatever is present.
#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct DeviceInfo {
    pub id: String,
    pub name: Option<String>,
    pub device_type: Option<String>,
    pub paired_at: DateTime<Utc>,
    pub last_seen: DateTime<Utc>,
    pub ip_address: Option<String>,
    #[serde(default)]
    pub hostname: Option<String>,
    #[serde(default)]
    pub os_name: Option<String>,
    #[serde(default)]
    pub os_version: Option<String>,
    #[serde(default)]
    pub agent_version: Option<String>,
}

/// Registry of paired devices backed by SQLite.
#[derive(Debug)]
pub struct DeviceRegistry {
    cache: Mutex<HashMap<String, DeviceInfo>>,
    db_path: PathBuf,
}

impl DeviceRegistry {
    pub fn new(workspace_dir: &Path) -> Self {
        let db_path = workspace_dir.join("devices.db");
        let conn = Connection::open(&db_path).expect("Failed to open device registry database");
        conn.execute_batch(
            "CREATE TABLE IF NOT EXISTS devices (
                token_hash TEXT PRIMARY KEY,
                id TEXT NOT NULL,
                name TEXT,
                device_type TEXT,
                paired_at TEXT NOT NULL,
                last_seen TEXT NOT NULL,
                ip_address TEXT
            )",
        )
        .expect("Failed to create devices table");

        // Additive migration: hostname / os_name / os_version / agent_version.
        // `IF NOT EXISTS` is not supported on `ALTER TABLE ADD COLUMN` in
        // SQLite, so we list the existing columns up front and only attempt
        // ADD on the ones that are actually missing. This is idempotent
        // across restarts and surfaces *real* errors (DB locked, permission
        // denied, disk full) instead of swallowing them under the previous
        // `let _ = conn.execute(...)` shape.
        let existing_columns: std::collections::HashSet<String> = conn
            .prepare("PRAGMA table_info(devices)")
            .and_then(|mut stmt| {
                stmt.query_map([], |row| row.get::<_, String>(1))
                    .map(|rows| rows.filter_map(Result::ok).collect())
            })
            .unwrap_or_default();
        for (name, decl) in [
            ("hostname", "hostname TEXT"),
            ("os_name", "os_name TEXT"),
            ("os_version", "os_version TEXT"),
            ("agent_version", "agent_version TEXT"),
        ] {
            if existing_columns.contains(name) {
                continue;
            }
            if let Err(e) = conn.execute(&format!("ALTER TABLE devices ADD COLUMN {decl}"), []) {
                tracing::warn!(
                    target: "zeroclaw_gateway::devices",
                    "failed to add column {name} to devices table: {e}"
                );
            }
        }

        // Warm the in-memory cache from DB
        let mut cache = HashMap::new();
        let mut stmt = conn
            .prepare(
                "SELECT token_hash, id, name, device_type, paired_at, last_seen, ip_address, \
                 hostname, os_name, os_version, agent_version FROM devices",
            )
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
                let hostname: Option<String> = row.get(7)?;
                let os_name: Option<String> = row.get(8)?;
                let os_version: Option<String> = row.get(9)?;
                let agent_version: Option<String> = row.get(10)?;
                let paired_at = DateTime::parse_from_rfc3339(&paired_at_str)
                    .map(|dt| dt.with_timezone(&Utc))
                    .unwrap_or_else(|_| Utc::now());
                let last_seen = DateTime::parse_from_rfc3339(&last_seen_str)
                    .map(|dt| dt.with_timezone(&Utc))
                    .unwrap_or_else(|_| Utc::now());
                Ok((
                    token_hash,
                    DeviceInfo {
                        id,
                        name,
                        device_type,
                        paired_at,
                        last_seen,
                        ip_address,
                        hostname,
                        os_name,
                        os_version,
                        agent_version,
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

    fn open_db(&self) -> Connection {
        Connection::open(&self.db_path).expect("Failed to open device registry database")
    }

    pub fn register(&self, token_hash: String, info: DeviceInfo) {
        let conn = self.open_db();
        conn.execute(
            "INSERT OR REPLACE INTO devices (\
                token_hash, id, name, device_type, paired_at, last_seen, ip_address, \
                hostname, os_name, os_version, agent_version\
             ) VALUES (?1, ?2, ?3, ?4, ?5, ?6, ?7, ?8, ?9, ?10, ?11)",
            rusqlite::params![
                token_hash,
                info.id,
                info.name,
                info.device_type,
                info.paired_at.to_rfc3339(),
                info.last_seen.to_rfc3339(),
                info.ip_address,
                info.hostname,
                info.os_name,
                info.os_version,
                info.agent_version,
            ],
        )
        .expect("Failed to insert device");
        self.cache.lock().insert(token_hash, info);
    }

    pub fn list(&self) -> Vec<DeviceInfo> {
        let conn = self.open_db();
        let mut stmt = conn
            .prepare(
                "SELECT token_hash, id, name, device_type, paired_at, last_seen, ip_address, \
                 hostname, os_name, os_version, agent_version FROM devices",
            )
            .expect("Failed to prepare device select");
        let rows = stmt
            .query_map([], |row| {
                let id: String = row.get(1)?;
                let name: Option<String> = row.get(2)?;
                let device_type: Option<String> = row.get(3)?;
                let paired_at_str: String = row.get(4)?;
                let last_seen_str: String = row.get(5)?;
                let ip_address: Option<String> = row.get(6)?;
                let hostname: Option<String> = row.get(7)?;
                let os_name: Option<String> = row.get(8)?;
                let os_version: Option<String> = row.get(9)?;
                let agent_version: Option<String> = row.get(10)?;
                let paired_at = DateTime::parse_from_rfc3339(&paired_at_str)
                    .map(|dt| dt.with_timezone(&Utc))
                    .unwrap_or_else(|_| Utc::now());
                let last_seen = DateTime::parse_from_rfc3339(&last_seen_str)
                    .map(|dt| dt.with_timezone(&Utc))
                    .unwrap_or_else(|_| Utc::now());
                Ok(DeviceInfo {
                    id,
                    name,
                    device_type,
                    paired_at,
                    last_seen,
                    ip_address,
                    hostname,
                    os_name,
                    os_version,
                    agent_version,
                })
            })
            .expect("Failed to query devices");
        rows.filter_map(|r| r.ok()).collect()
    }

    pub fn revoke(&self, device_id: &str) -> bool {
        let conn = self.open_db();
        let deleted = conn
            .execute(
                "DELETE FROM devices WHERE id = ?1",
                rusqlite::params![device_id],
            )
            .unwrap_or(0);
        if deleted > 0 {
            let mut cache = self.cache.lock();
            let key = cache
                .iter()
                .find(|(_, v)| v.id == device_id)
                .map(|(k, _)| k.clone());
            if let Some(key) = key {
                cache.remove(&key);
            }
            true
        } else {
            false
        }
    }

    /// Update the human-readable label for a device. Returns `true` if a row
    /// matched the given device id, `false` otherwise.
    pub fn rename(&self, device_id: &str, new_name: Option<String>) -> bool {
        let conn = self.open_db();
        let updated = conn
            .execute(
                "UPDATE devices SET name = ?1 WHERE id = ?2",
                rusqlite::params![new_name, device_id],
            )
            .unwrap_or(0);
        if updated > 0 {
            for device in self.cache.lock().values_mut() {
                if device.id == device_id {
                    device.name = new_name.clone();
                    break;
                }
            }
            true
        } else {
            false
        }
    }

    pub fn update_last_seen(&self, token_hash: &str) {
        let now = Utc::now();
        let conn = self.open_db();
        conn.execute(
            "UPDATE devices SET last_seen = ?1 WHERE token_hash = ?2",
            rusqlite::params![now.to_rfc3339(), token_hash],
        )
        .ok();
        if let Some(device) = self.cache.lock().get_mut(token_hash) {
            device.last_seen = now;
        }
    }

    pub fn device_count(&self) -> usize {
        self.cache.lock().len()
    }
}

/// Store for pending pairing requests.
#[derive(Debug)]
pub struct PairingStore {
    pending: Mutex<Vec<PendingPairing>>,
    #[allow(dead_code)] // WIP: will be used to cap pending pairing requests
    max_pending: usize,
}

#[derive(Debug, Clone, Serialize)]
struct PendingPairing {
    code: String,
    created_at: DateTime<Utc>,
    expires_at: DateTime<Utc>,
    client_ip: Option<String>,
    attempts: u32,
}

impl PairingStore {
    pub fn new(max_pending: usize) -> Self {
        Self {
            pending: Mutex::new(Vec::new()),
            max_pending,
        }
    }

    pub fn pending_count(&self) -> usize {
        let mut pending = self.pending.lock();
        pending.retain(|p| p.expires_at > Utc::now());
        pending.len()
    }
}

fn extract_bearer(headers: &HeaderMap) -> Option<&str> {
    headers
        .get(header::AUTHORIZATION)
        .and_then(|v| v.to_str().ok())
        .and_then(|auth| auth.strip_prefix("Bearer "))
}

fn require_auth(state: &AppState, headers: &HeaderMap) -> Result<(), (StatusCode, &'static str)> {
    if state.pairing.require_pairing() {
        let token = extract_bearer(headers).unwrap_or("");
        if !state.pairing.is_authenticated(token) {
            return Err((StatusCode::UNAUTHORIZED, "Unauthorized"));
        }
    }
    Ok(())
}

/// POST /api/pairing/initiate — initiate a new pairing session
pub async fn initiate_pairing(
    State(state): State<AppState>,
    headers: HeaderMap,
) -> impl IntoResponse {
    if let Err(e) = require_auth(&state, &headers) {
        return e.into_response();
    }

    match state.pairing.generate_new_pairing_code() {
        Some(code) => Json(serde_json::json!({
            "pairing_code": code,
            "message": "New pairing code generated"
        }))
        .into_response(),
        None => (
            StatusCode::SERVICE_UNAVAILABLE,
            "Pairing is disabled or not available",
        )
            .into_response(),
    }
}

/// POST /api/pair — submit pairing code (for new device pairing)
pub async fn submit_pairing_enhanced(
    State(state): State<AppState>,
    headers: HeaderMap,
    Json(body): Json<serde_json::Value>,
) -> impl IntoResponse {
    let code = body["code"].as_str().unwrap_or("");
    let device_name = body["device_name"].as_str().map(String::from);
    let device_type = body["device_type"].as_str().map(String::from);

    let client_id = headers
        .get("X-Forwarded-For")
        .and_then(|v| v.to_str().ok())
        .unwrap_or("unknown")
        .to_string();

    match state.pairing.try_pair(code, &client_id).await {
        Ok(Some(token)) => {
            // Register the new device
            let token_hash = {
                use sha2::{Digest, Sha256};
                let hash = Sha256::digest(token.as_bytes());
                hex::encode(hash)
            };
            if let Some(ref registry) = state.device_registry {
                registry.register(
                    token_hash,
                    DeviceInfo {
                        id: uuid::Uuid::new_v4().to_string(),
                        name: device_name,
                        device_type,
                        paired_at: Utc::now(),
                        last_seen: Utc::now(),
                        ip_address: Some(client_id),
                        hostname: body["hostname"].as_str().map(String::from),
                        os_name: body["os_name"].as_str().map(String::from),
                        os_version: body["os_version"].as_str().map(String::from),
                        agent_version: body["agent_version"].as_str().map(String::from),
                    },
                );
            }
            Json(serde_json::json!({
                "token": token,
                "message": "Pairing successful"
            }))
            .into_response()
        }
        Ok(None) => (StatusCode::BAD_REQUEST, "Invalid or expired pairing code").into_response(),
        Err(lockout_secs) => (
            StatusCode::TOO_MANY_REQUESTS,
            format!("Too many attempts. Locked out for {lockout_secs}s"),
        )
            .into_response(),
    }
}

/// GET /api/devices — list paired devices
pub async fn list_devices(State(state): State<AppState>, headers: HeaderMap) -> impl IntoResponse {
    if let Err(e) = require_auth(&state, &headers) {
        return e.into_response();
    }

    let devices = state
        .device_registry
        .as_ref()
        .map(|r| r.list())
        .unwrap_or_default();

    let count = devices.len();
    let nodes_cfg = state.config.lock().nodes.clone();
    Json(serde_json::json!({
        "devices": devices,
        "count": count,
        "policy": {
            "stale_after_secs": nodes_cfg.stale_after_secs,
            "offline_after_secs": nodes_cfg.offline_after_secs,
        }
    }))
    .into_response()
}

/// DELETE /api/devices/{id} — revoke a paired device
pub async fn revoke_device(
    State(state): State<AppState>,
    headers: HeaderMap,
    axum::extract::Path(device_id): axum::extract::Path<String>,
) -> impl IntoResponse {
    if let Err(e) = require_auth(&state, &headers) {
        return e.into_response();
    }

    let revoked = state
        .device_registry
        .as_ref()
        .map(|r| r.revoke(&device_id))
        .unwrap_or(false);

    if revoked {
        Json(serde_json::json!({
            "message": "Device revoked",
            "device_id": device_id
        }))
        .into_response()
    } else {
        (StatusCode::NOT_FOUND, "Device not found").into_response()
    }
}

/// `PATCH /api/devices/{id}` — update the device's friendly label.
///
/// Body: `{"name": "Argenis's Mac"}` (or `{"name": null}` to clear).
/// Returns 404 if the id is unknown.
#[derive(Deserialize)]
pub struct DevicePatchBody {
    /// Replacement label. Pass `null` to clear back to the inferred default.
    pub name: Option<String>,
}

pub async fn patch_device(
    State(state): State<AppState>,
    headers: HeaderMap,
    axum::extract::Path(device_id): axum::extract::Path<String>,
    Json(body): Json<DevicePatchBody>,
) -> impl IntoResponse {
    if let Err(e) = require_auth(&state, &headers) {
        return e.into_response();
    }

    let updated = state
        .device_registry
        .as_ref()
        .map(|r| r.rename(&device_id, body.name.clone()))
        .unwrap_or(false);

    if updated {
        Json(serde_json::json!({
            "device_id": device_id,
            "name": body.name,
        }))
        .into_response()
    } else {
        (StatusCode::NOT_FOUND, "Device not found").into_response()
    }
}

/// POST /api/devices/{id}/token/rotate — rotate a device's token
pub async fn rotate_token(
    State(state): State<AppState>,
    headers: HeaderMap,
    axum::extract::Path(device_id): axum::extract::Path<String>,
) -> impl IntoResponse {
    if let Err(e) = require_auth(&state, &headers) {
        return e.into_response();
    }

    // Generate a new pairing code for re-pairing
    match state.pairing.generate_new_pairing_code() {
        Some(code) => Json(serde_json::json!({
            "device_id": device_id,
            "pairing_code": code,
            "message": "Use this code to re-pair the device"
        }))
        .into_response(),
        None => (
            StatusCode::SERVICE_UNAVAILABLE,
            "Cannot generate new pairing code",
        )
            .into_response(),
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use tempfile::TempDir;

    fn fresh_registry() -> (TempDir, DeviceRegistry) {
        let tmp = TempDir::new().unwrap();
        let registry = DeviceRegistry::new(tmp.path());
        (tmp, registry)
    }

    fn sample_device(id: &str, name: Option<&str>) -> DeviceInfo {
        DeviceInfo {
            id: id.into(),
            name: name.map(String::from),
            device_type: Some("macos".into()),
            paired_at: Utc::now(),
            last_seen: Utc::now(),
            ip_address: Some("127.0.0.1".into()),
            hostname: Some("argenis-Mac.local".into()),
            os_name: Some("macOS".into()),
            os_version: Some("10.15.7".into()),
            agent_version: Some("0.7.4".into()),
        }
    }

    #[test]
    fn rename_persists_new_name() {
        let (_tmp, registry) = fresh_registry();
        registry.register("hash-1".into(), sample_device("dev-1", Some("Old Name")));

        assert!(registry.rename("dev-1", Some("New Name".into())));
        let listed = registry.list();
        let updated = listed.iter().find(|d| d.id == "dev-1").unwrap();
        assert_eq!(updated.name.as_deref(), Some("New Name"));
    }

    #[test]
    fn rename_returns_false_for_unknown_id() {
        let (_tmp, registry) = fresh_registry();
        // No device registered yet; rename should be a no-op.
        assert!(!registry.rename("does-not-exist", Some("X".into())));
    }

    #[test]
    fn rename_to_none_clears_label() {
        let (_tmp, registry) = fresh_registry();
        registry.register("hash-1".into(), sample_device("dev-1", Some("Set")));

        assert!(registry.rename("dev-1", None));
        let listed = registry.list();
        let cleared = listed.iter().find(|d| d.id == "dev-1").unwrap();
        assert!(cleared.name.is_none());
    }

    /// Pre-create a `devices.db` with the v1 schema (no hostname / os_name /
    /// os_version / agent_version columns) and a sample row, then open a
    /// fresh `DeviceRegistry` against the same dir. The additive migration
    /// must add the columns and preserve the pre-existing row.
    #[test]
    fn migration_adds_columns_to_pre_existing_db() {
        let tmp = TempDir::new().unwrap();
        let db_path = tmp.path().join("devices.db");

        // Simulate the v1 schema (the shape master had before this PR).
        {
            let conn = Connection::open(&db_path).unwrap();
            conn.execute_batch(
                "CREATE TABLE devices (
                    token_hash TEXT PRIMARY KEY,
                    id TEXT NOT NULL,
                    name TEXT,
                    device_type TEXT,
                    paired_at TEXT NOT NULL,
                    last_seen TEXT NOT NULL,
                    ip_address TEXT
                )",
            )
            .unwrap();
            conn.execute(
                "INSERT INTO devices (token_hash, id, name, device_type, paired_at, last_seen, ip_address) VALUES (?1, ?2, ?3, ?4, ?5, ?6, ?7)",
                rusqlite::params![
                    "legacy-hash",
                    "legacy-dev",
                    "Pre-migration Mac",
                    "macos",
                    Utc::now().to_rfc3339(),
                    Utc::now().to_rfc3339(),
                    "127.0.0.1",
                ],
            )
            .unwrap();
        }

        // Opening the registry runs the migration.
        let registry = DeviceRegistry::new(tmp.path());

        // Pre-existing row is still there with its old fields intact and the
        // new fields populated as `None`.
        let listed = registry.list();
        let legacy = listed.iter().find(|d| d.id == "legacy-dev").unwrap();
        assert_eq!(legacy.name.as_deref(), Some("Pre-migration Mac"));
        assert_eq!(legacy.device_type.as_deref(), Some("macos"));
        assert!(legacy.hostname.is_none());
        assert!(legacy.os_name.is_none());
        assert!(legacy.os_version.is_none());
        assert!(legacy.agent_version.is_none());

        // And new registers can populate the new fields end-to-end.
        registry.register("new-hash".into(), sample_device("new-dev", Some("Fresh")));
        let listed = registry.list();
        let fresh = listed.iter().find(|d| d.id == "new-dev").unwrap();
        assert_eq!(fresh.hostname.as_deref(), Some("argenis-Mac.local"));
        assert_eq!(fresh.os_version.as_deref(), Some("10.15.7"));
    }

    /// Re-opening an already-migrated db must be a no-op (no errors
    /// surfaced, columns unchanged). Catches the case where the idempotent
    /// existence check regresses.
    #[test]
    fn migration_is_idempotent_on_already_migrated_db() {
        let tmp = TempDir::new().unwrap();
        let _first = DeviceRegistry::new(tmp.path());
        // Second open should not panic and should preserve schema.
        let second = DeviceRegistry::new(tmp.path());
        // Round-trip a row to confirm the table is still usable.
        second.register("h".into(), sample_device("d", None));
        assert_eq!(second.list().len(), 1);
    }
}
