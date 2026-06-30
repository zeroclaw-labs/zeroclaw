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
    /// Construct a registry and warm its in-memory cache from the SQLite
    /// database at `<workspace_dir>/devices.db`.
    ///
    /// # Panics
    ///
    /// Panics if the device registry database cannot be opened or initialised.
    /// This is intentional startup-path behaviour: a gateway that cannot reach
    /// its device registry at boot must not come up at all, since later
    /// per-request errors would only surface after devices begin trying to
    /// pair. Callers that prefer graceful degradation should wrap construction
    /// in `catch_unwind` or call out of `main` before any HTTP server starts.
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

    /// Construct a registry directly from a database path with an empty
    /// in-memory cache, bypassing the workspace-relative join and the
    /// initial schema/cache warm-up.
    ///
    /// Intended only for tests that need to inject an unusable path
    /// (e.g. a non-existent directory or read-only location) to force
    /// `register` / `revoke` / `list` to surface a `rusqlite::Error`
    /// without polluting the workspace.
    #[cfg(test)]
    pub(crate) fn with_db_path(db_path: PathBuf) -> Self {
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

    /// Backfill placeholder rows for paired tokens that have no device entry.
    ///
    /// Bearer tokens paired through the legacy `/pair` route (`handle_pair`)
    /// historically never called [`register`](Self::register), so their hashes
    /// live in `gateway.paired_tokens` — the canonical credential set the auth
    /// gate checks — with no matching device row. Such tokens fully
    /// authenticate yet are invisible in `GET /api/devices` and cannot be
    /// revoked from the management UI, which is a security-management gap.
    ///
    /// This reconciles the registry (metadata, keyed by `token_hash`) against
    /// that canonical set on startup: every hash without a row gets a neutral
    /// `"legacy"` placeholder so it surfaces and can be revoked like any other
    /// device. The source of truth for *which* tokens are valid remains
    /// `PairingGuard`/`gateway.paired_tokens` — this never invents a token,
    /// only surfaces ones that already authenticate. `INSERT OR IGNORE` keeps a
    /// real row from being clobbered. Returns the number of rows inserted.
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

    /// Delete a device by id and return its SHA-256 token hash so the caller
    /// can revoke the matching bearer token.
    ///
    /// `Ok(None)` means the device did not exist; real SQLite errors are
    /// propagated so handlers can distinguish "nothing to do" from "DB is
    /// broken" — confusing the two during incident response is dangerous.
    /// Uses `DELETE … RETURNING` (SQLite ≥ 3.35) so the read and delete are
    /// atomic under concurrent revoke calls.
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

    /// Replace the capability list for the device identified by `token_hash`.
    /// Returns true if a row was updated. A database error during the write
    /// is reported as "no row updated" rather than propagated — the row may
    /// legitimately not exist (token was revoked between bearer issuance and
    /// capability push), and conflating that with "DB is broken" misleads the
    /// operator during incident response.
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

/// Store for pending pairing requests.
#[derive(Debug, Default)]
pub struct PairingStore {
    pending: Mutex<Vec<PendingPairing>>,
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
    pub fn new() -> Self {
        Self::default()
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
            // `try_pair` is not just validation: by the time we land
            // here, the pairing code is consumed and the token's
            // SHA-256 hash is already in `PairingGuard::paired_tokens`.
            // Every step below must succeed atomically — if any of
            // them fails, we MUST roll back via
            // `revoke_token_hash` and return 500 WITHOUT the token
            // in the body, otherwise the in-process credential state
            // remains accepted while the operator sees a 500 (and a
            // usable token if the legacy /pair path leaks it).
            let token_hash = {
                use sha2::{Digest, Sha256};
                let hash = Sha256::digest(token.as_bytes());
                hex::encode(hash)
            };

            if let Some(ref registry) = state.device_registry {
                if let Err(e) = registry.register(
                    token_hash.clone(),
                    DeviceInfo {
                        id: uuid::Uuid::new_v4().to_string(),
                        name: device_name,
                        device_type,
                        paired_at: Utc::now(),
                        last_seen: Utc::now(),
                        ip_address: Some(client_id),
                        capabilities: None,
                    },
                ) {
                    ::zeroclaw_log::record!(
                        ERROR,
                        ::zeroclaw_log::Event::new(module_path!(), ::zeroclaw_log::Action::Fail)
                            .with_outcome(::zeroclaw_log::EventOutcome::Failure)
                            .with_attrs(::serde_json::json!({"error": format!("{e}")})),
                        "device registry insert failed after successful pairing; rolling back in-process token"
                    );
                    // Compensating action: drop the just-accepted
                    // hash so the failed pairing leaves no
                    // authenticate-able state. The pairing code is
                    // already consumed (one-shot), so the operator
                    // must call `initiate_pairing` to issue a new
                    // code. The orphaned registry row, if any, sits
                    // until the operator removes it via the
                    // management UI; the next `revoke_all` /
                    // `reconcile` cycle cleans it up.
                    state.pairing.revoke_token_hash(&token_hash);
                    return (
                        StatusCode::INTERNAL_SERVER_ERROR,
                        Json(serde_json::json!({
                            "paired": false,
                            "persisted": false,
                            "error": format!("Device registry error: {e}"),
                            "message": "Pairing failed; the in-process token was not retained.",
                        })),
                    )
                        .into_response();
                }
            }
            if let Err(e) =
                super::persist_pairing_tokens(state.config.clone(), &state.pairing).await
            {
                ::zeroclaw_log::record!(
                    ERROR,
                    ::zeroclaw_log::Event::new(module_path!(), ::zeroclaw_log::Action::Fail)
                        .with_outcome(::zeroclaw_log::EventOutcome::Failure)
                        .with_attrs(::serde_json::json!({"error": format!("{e}")})),
                    "pairing token persistence failed; rolling back in-process token"
                );
                // Same compensating action as above: persistence
                // failed, so a restart would resurrect the in-memory
                // token. Drop it now and do NOT return the
                // plaintext token in the body — the previous
                // behavior leaked a usable bearer on a 200, which
                // is the very gap this PR closes.
                state.pairing.revoke_token_hash(&token_hash);
                return (
                    StatusCode::INTERNAL_SERVER_ERROR,
                    Json(serde_json::json!({
                        "paired": false,
                        "persisted": false,
                        "error": format!("Token persistence error: {e}"),
                        "message": "Pairing failed; the in-process token was not retained.",
                    })),
                )
                    .into_response();
            }
            Json(serde_json::json!({
                "paired": true,
                "persisted": true,
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

    let devices = match state.device_registry.as_ref() {
        Some(r) => match r.list() {
            Ok(devices) => devices,
            Err(e) => {
                ::zeroclaw_log::record!(
                    ERROR,
                    ::zeroclaw_log::Event::new(module_path!(), ::zeroclaw_log::Action::Fail)
                        .with_outcome(::zeroclaw_log::EventOutcome::Failure)
                        .with_attrs(::serde_json::json!({"error": format!("{e}")})),
                    "device registry list failed"
                );
                return (
                    StatusCode::INTERNAL_SERVER_ERROR,
                    format!("Device registry error: {e}"),
                )
                    .into_response();
            }
        },
        None => Vec::new(),
    };

    let count = devices.len();
    Json(serde_json::json!({
        "devices": devices,
        "count": count
    }))
    .into_response()
}

/// DELETE /api/devices/{id} — revoke a paired device and its bearer token.
pub async fn revoke_device(
    State(state): State<AppState>,
    headers: HeaderMap,
    axum::extract::Path(device_id): axum::extract::Path<String>,
) -> impl IntoResponse {
    if let Err(e) = require_auth(&state, &headers) {
        return e.into_response();
    }

    let Some(registry) = state.device_registry.as_ref() else {
        return (
            StatusCode::SERVICE_UNAVAILABLE,
            "Device registry is disabled",
        )
            .into_response();
    };

    let token_hash = match registry.revoke(&device_id) {
        Ok(Some(hash)) => hash,
        Ok(None) => return (StatusCode::NOT_FOUND, "Device not found").into_response(),
        Err(e) => {
            return (
                StatusCode::INTERNAL_SERVER_ERROR,
                format!("Device registry error: {e}"),
            )
                .into_response();
        }
    };

    state.pairing.revoke_token_hash(&token_hash);

    // If persistence fails after the in-memory revoke + row delete, the
    // device row is already gone and the token is already invalid in this
    // process; a daemon restart will resurrect the token from the unchanged
    // on-disk config. Surface that to the caller so they know to re-pair
    // and audit, rather than treating the operation as silently complete.
    if let Err(e) = super::persist_pairing_tokens(state.config.clone(), &state.pairing).await {
        return (
            StatusCode::INTERNAL_SERVER_ERROR,
            format!("Token revoked in memory but config persist failed: {e}"),
        )
            .into_response();
    }

    Json(serde_json::json!({
        "message": "Device revoked and bearer token invalidated",
        "device_id": device_id,
    }))
    .into_response()
}

/// POST /api/devices/me/capabilities — the calling device replaces its capability list.
///
/// The "me" path means there's no separate device id in the URL — the bearer token in
/// Authorization identifies which row gets updated. Body: `{ "capabilities": ["..."] }`.
pub async fn update_my_capabilities(
    State(state): State<AppState>,
    headers: HeaderMap,
    Json(body): Json<serde_json::Value>,
) -> impl IntoResponse {
    if let Err(e) = require_auth(&state, &headers) {
        return e.into_response();
    }

    let token = match extract_bearer(&headers) {
        Some(t) => t,
        None => return (StatusCode::UNAUTHORIZED, "Missing bearer token").into_response(),
    };
    let token_hash = {
        use sha2::{Digest, Sha256};
        let hash = Sha256::digest(token.as_bytes());
        hex::encode(hash)
    };

    let capabilities: Vec<String> = body
        .get("capabilities")
        .and_then(|v| v.as_array())
        .map(|arr| {
            arr.iter()
                .filter_map(|v| v.as_str().map(String::from))
                .collect()
        })
        .unwrap_or_default();

    let registry = match state.device_registry.as_ref() {
        Some(r) => r,
        None => {
            return (
                StatusCode::SERVICE_UNAVAILABLE,
                "Device registry is disabled",
            )
                .into_response();
        }
    };

    if registry.update_capabilities(&token_hash, capabilities.clone()) {
        Json(serde_json::json!({
            "message": "Capabilities updated",
            "capabilities": capabilities,
        }))
        .into_response()
    } else {
        (StatusCode::NOT_FOUND, "Device not found for this token").into_response()
    }
}

/// POST /api/devices/{id}/token/rotate — revoke the device's current bearer
/// token and issue a fresh pairing code for re-pairing.
///
/// The device row is removed because the schema keys on `token_hash`; once
/// the token is revoked the row's primary key is dead anyway. Re-pairing
/// inserts a fresh row with the new token's hash.
///
/// The rotation's load-bearing effect is invalidating the leaked token, not
/// issuing a new code. If another flow holds the pairing-code slot the
/// revoke still happens; the response reports that no new code was issued
/// and the operator can use the pending code or call again once it clears.
///
/// If the caller is using the same bearer token as the device being rotated
/// (self-revocation), the response is delivered over the now-invalid token;
/// subsequent requests from that client will fail until they re-pair. That
/// is the intended path for "rotate my own token after I think it leaked."
pub async fn rotate_token(
    State(state): State<AppState>,
    headers: HeaderMap,
    axum::extract::Path(device_id): axum::extract::Path<String>,
) -> impl IntoResponse {
    if let Err(e) = require_auth(&state, &headers) {
        return e.into_response();
    }

    let Some(registry) = state.device_registry.as_ref() else {
        return (
            StatusCode::SERVICE_UNAVAILABLE,
            "Device registry is disabled",
        )
            .into_response();
    };

    let token_hash = match registry.revoke(&device_id) {
        Ok(Some(hash)) => hash,
        Ok(None) => return (StatusCode::NOT_FOUND, "Device not found").into_response(),
        Err(e) => {
            return (
                StatusCode::INTERNAL_SERVER_ERROR,
                format!("Device registry error: {e}"),
            )
                .into_response();
        }
    };

    state.pairing.revoke_token_hash(&token_hash);

    // Same persist-fail caveat as `revoke_device`: device row + in-memory
    // token are already gone; surfacing the persist error tells the caller
    // a restart could resurrect the token.
    if let Err(e) = super::persist_pairing_tokens(state.config.clone(), &state.pairing).await {
        return (
            StatusCode::INTERNAL_SERVER_ERROR,
            format!("Token revoked in memory but config persist failed: {e}"),
        )
            .into_response();
    }

    // Issue the new pairing code atomically against the slot. If another
    // flow holds the slot, the revoke still stands — return 200 with
    // `pairing_code: null` and a message that tells the operator what
    // happened so they do not assume rotation failed.
    match state.pairing.generate_pairing_code_if_vacant() {
        Ok(code) => Json(serde_json::json!({
            "device_id": device_id,
            "pairing_code": code,
            "message": "Old token revoked. Use this code to re-pair the device.",
        }))
        .into_response(),
        Err(zeroclaw_config::pairing::GeneratePairingCodeError::Pending) => {
            Json(serde_json::json!({
                "device_id": device_id,
                "pairing_code": null,
                "message": "Old token revoked. A pairing code is already pending; use it or call again after it clears.",
            }))
            .into_response()
        }
        Err(zeroclaw_config::pairing::GeneratePairingCodeError::PairingDisabled) => {
            Json(serde_json::json!({
                "device_id": device_id,
                "pairing_code": null,
                "message": "Old token revoked. Pairing is disabled; cannot issue a new code.",
            }))
            .into_response()
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::api::test_state;
    use axum::Json;
    use http_body_util::BodyExt;
    use std::sync::Arc;
    use zeroclaw_config::pairing::PairingGuard;
    use zeroclaw_config::schema::Config;

    /// Build an `AppState` whose device-registry points at a non-existent
    /// path so every SQLite write fails. Pairing enabled so a freshly
    /// issued code is actually consumable.
    fn unwriteable_registry_state() -> AppState {
        let mut state = test_state(Config::default());
        state.pairing = Arc::new(PairingGuard::new(true, &[]));
        // `/this/path/does/not/exist/devices.db` cannot be opened because
        // the parent directory does not exist. `open_db` returns
        // `DatabasePathMissing`/`CannotOpen`, surfacing as
        // `rusqlite::Error` from `register`. This is the regression
        // setup we need.
        state.device_registry = Some(Arc::new(DeviceRegistry::with_db_path(PathBuf::from(
            "/this/path/does/not/exist/devices.db",
        ))));
        state
    }

    async fn response_json(response: axum::response::Response) -> (StatusCode, serde_json::Value) {
        let status = response.status();
        let bytes = response.into_body().collect().await.unwrap().to_bytes();
        let json: serde_json::Value = serde_json::from_slice(&bytes).unwrap();
        (status, json)
    }

    /// If `registry.register(...)` fails after `try_pair` already
    /// accepted the code, the handler must roll back the in-process
    /// token (no accepted credential left behind) and must NOT return
    /// the plaintext bearer in the 500 body.
    #[tokio::test]
    async fn submit_pairing_enhanced_rolls_back_in_process_token_when_registry_register_fails() {
        let state = unwriteable_registry_state();

        // Issue a pairing code so the next `try_pair` succeeds.
        let code = state
            .pairing
            .generate_new_pairing_code()
            .expect("pairing code must be issuable when require_pairing=true");

        let (status, body) = response_json(
            submit_pairing_enhanced(
                State(state.clone()),
                HeaderMap::new(),
                Json(serde_json::json!({"code": code, "device_name": "test"})),
            )
            .await
            .into_response(),
        )
        .await;

        assert_eq!(
            status,
            StatusCode::INTERNAL_SERVER_ERROR,
            "registry.register failure path must surface as 500"
        );
        assert_eq!(body["paired"], serde_json::Value::Bool(false));
        assert_eq!(body["persisted"], serde_json::Value::Bool(false));
        assert!(
            body.get("token").is_none(),
            "5xx body MUST NOT contain the plaintext bearer token; got: {body}"
        );
        assert!(
            state.pairing.tokens().is_empty(),
            "PairingGuard::paired_tokens must be empty after a failed registry.register \
             (compensating `revoke_token_hash`); instead have {:?}",
            state.pairing.tokens()
        );
    }

    /// If token persistence to `config.toml` fails after `try_pair`
    /// already accepted the code, the handler must roll back the
    /// in-process token (so a restart does not resurrect it) and must
    /// NOT return the plaintext bearer in the body — the previous
    /// version leaked a usable bearer on a 200, which is exactly the
    /// gap this whole PR closes.
    #[tokio::test]
    async fn submit_pairing_enhanced_rolls_back_in_process_token_when_persist_fails() {
        let mut state = test_state(Config::default());
        state.pairing = Arc::new(PairingGuard::new(true, &[]));
        // No device_registry at all → registry branch is skipped,
        // so the persistence branch is the only failing step.
        // Point the config path at a non-existent file inside a
        // directory whose parent doesn't exist so `save_dirty`
        // cannot write.
        {
            let mut cfg = state.config.write();
            cfg.config_path = PathBuf::from("/no/such/dir/config.toml");
        }

        let code = state
            .pairing
            .generate_new_pairing_code()
            .expect("pairing code must be issuable when require_pairing=true");

        let (status, body) = response_json(
            submit_pairing_enhanced(
                State(state.clone()),
                HeaderMap::new(),
                Json(serde_json::json!({"code": code})),
            )
            .await
            .into_response(),
        )
        .await;

        assert_eq!(
            status,
            StatusCode::INTERNAL_SERVER_ERROR,
            "persistence failure path must surface as 500 (legacy leaked 200 + token)"
        );
        assert_eq!(body["paired"], serde_json::Value::Bool(false));
        assert!(
            body.get("token").is_none(),
            "5xx body MUST NOT contain the plaintext bearer token; got: {body}"
        );
        assert!(
            state.pairing.tokens().is_empty(),
            "PairingGuard::paired_tokens must be empty after a failed persist; have {:?}",
            state.pairing.tokens()
        );
    }
}
