use serde::{Deserialize, Serialize};
use std::path::{Path, PathBuf};
use std::sync::atomic::{AtomicBool, Ordering};
use std::sync::Arc;
use tauri::{Emitter, Manager};
use tauri_plugin_shell::ShellExt;
use rusqlite::Connection;

mod device_link;
mod embedding_status;
mod local_chat_ws;

/// Write a session token to disk and restrict permissions to owner-only (0o600) on Unix.
fn persist_session_token(path: &Path, token: &str) -> std::io::Result<()> {
    std::fs::write(path, token)?;
    #[cfg(unix)]
    {
        use std::os::unix::fs::PermissionsExt;
        std::fs::set_permissions(path, std::fs::Permissions::from_mode(0o600))?;
    }
    Ok(())
}

// ── State ────────────────────────────────────────────────────────

/// Shared application state for Tauri commands.
///
/// Architecture: MoA = ZeroClaw installer/launcher.
/// The Tauri app bundles the ZeroClaw binary and launches it as a sidecar
/// process on startup. All AI operations (chat, voice, coding, tools) run
/// on the local ZeroClaw gateway. The relay server is used only for
/// cross-device memory sync and operator key fallback.
struct AppState {
    /// Local ZeroClaw gateway URL (default: http://127.0.0.1:3000)
    server_url: std::sync::Mutex<String>,
    /// Railway relay server URL (memory sync + operator key fallback)
    relay_url: std::sync::Mutex<String>,
    token: std::sync::Mutex<Option<String>>,
    /// Whether sync WebSocket is currently connected.
    sync_connected: AtomicBool,
    /// Flag to signal sync task to stop.
    sync_stop: AtomicBool,
    /// App data directory (platform-specific).
    data_dir: PathBuf,
    /// Whether the local ZeroClaw gateway is running.
    gateway_running: Arc<AtomicBool>,
}

/// Default gateway host and port for the local ZeroClaw instance.
const DEFAULT_GATEWAY_HOST: &str = "127.0.0.1";
const DEFAULT_GATEWAY_PORT: u16 = 3000;

// ── Types ────────────────────────────────────────────────────────

#[derive(Serialize, Deserialize, Clone)]
struct ChatResponse {
    response: String,
    model: String,
}

#[derive(Serialize, Deserialize, Clone)]
struct PairResponse {
    paired: bool,
    token: String,
}

#[derive(Serialize, Deserialize, Clone)]
struct HealthResponse {
    status: String,
}

#[derive(Serialize, Deserialize, Clone)]
struct SyncStatus {
    connected: bool,
    device_id: String,
    last_sync: Option<u64>,
}

// ── Tauri Commands ───────────────────────────────────────────────

#[tauri::command]
fn greet(name: &str) -> String {
    format!("Hello, {}! Welcome to MoA.", name)
}

/// Send a chat message to the MoA gateway and return the response.
///
/// Routing with automatic fallback:
/// 1. Try local gateway first (via /api/chat)
/// 2. If local fails (network error or missing API key) → fallback to relay server
/// 3. If both fail → return combined error
#[tauri::command]
async fn chat(
    message: String,
    state: tauri::State<'_, AppState>,
) -> Result<ChatResponse, String> {
    let server_url = state.server_url.lock().map_err(|e| e.to_string())?.clone();
    let relay_url = state.relay_url.lock().map_err(|e| e.to_string())?.clone();
    let token = state
        .token
        .lock()
        .map_err(|e| e.to_string())?
        .clone()
        .ok_or_else(|| "Not authenticated. Please login first.".to_string())?;

    let client = reqwest::Client::new();
    let body = serde_json::json!({ "message": message });

    // ── Try local gateway first ──
    let local_result = client
        .post(format!("{server_url}/api/chat"))
        .header("Content-Type", "application/json")
        .header("Authorization", format!("Bearer {token}"))
        .json(&body)
        .send()
        .await;

    match local_result {
        Ok(res) if res.status() == 401 => {
            *state.token.lock().map_err(|e| e.to_string())? = None;
            return Err("Authentication expired. Please re-login.".to_string());
        }
        Ok(res) if res.status().is_success() => {
            return res.json::<ChatResponse>()
                .await
                .map_err(|e| format!("Invalid response: {e}"));
        }
        Ok(res) if res.status().as_u16() == 400 || res.status().as_u16() == 500 => {
            // API key missing/invalid or provider auth error — check if fallback is suggested
            let status = res.status();
            let text = res.text().await.unwrap_or_default();
            let should_fallback = text.contains("fallback_to_relay")
                || text.contains("missing_api_key")
                || text.contains("provider_auth_error")
                || text.contains("Unauthorized")
                || text.contains("API key");

            if should_fallback {
                eprintln!("[MoA] Local gateway API key issue ({status}), falling back to relay...");
            } else {
                return Err(format!("Chat failed: {text}"));
            }
        }
        Ok(res) => {
            // Other non-success status — still try relay as fallback
            let status = res.status();
            let text = res.text().await.unwrap_or_default();
            eprintln!("[MoA] Local gateway error ({status}): {text}, trying relay...");
        }
        Err(e) => {
            eprintln!("[MoA] Local gateway unreachable ({e}), trying relay...");
        }
    }

    // ── Fallback to relay server ──
    let relay_result = client
        .post(format!("{relay_url}/api/chat"))
        .header("Content-Type", "application/json")
        .header("Authorization", format!("Bearer {token}"))
        .json(&body)
        .send()
        .await
        .map_err(|e| format!(
            "Cannot connect to MoA. Local gateway and relay server both unreachable: {e}"
        ))?;

    if relay_result.status() == 401 {
        return Err("Chat authentication failed on relay. Please re-login.".to_string());
    }

    if !relay_result.status().is_success() {
        let status = relay_result.status();
        let text = relay_result.text().await.unwrap_or_default();
        return Err(format!("Chat failed on relay ({status}): {text}"));
    }

    relay_result.json::<ChatResponse>()
        .await
        .map_err(|e| format!("Invalid relay response: {e}"))
}

/// Pair with a MoA gateway server using credentials and/or pairing code.
///
/// Supports three modes:
/// - Credentials only (username + password) — auto-connect
/// - Pairing code only — legacy Bluetooth-style flow
/// - Both credentials and code
#[tauri::command]
async fn pair(
    username: Option<String>,
    password: Option<String>,
    code: Option<String>,
    server_url: Option<String>,
    state: tauri::State<'_, AppState>,
) -> Result<PairResponse, String> {
    if let Some(url) = server_url {
        *state.server_url.lock().map_err(|e| e.to_string())? = url;
    }

    let url = state.server_url.lock().map_err(|e| e.to_string())?.clone();

    let client = reqwest::Client::new();
    let mut req = client
        .post(format!("{url}/pair"))
        .header("Content-Type", "application/json");

    if let Some(ref code) = code {
        req = req.header("X-Pairing-Code", code);
    }

    // Build body with credentials
    let mut body = serde_json::Map::new();
    if let Some(ref u) = username {
        body.insert("username".into(), serde_json::Value::String(u.clone()));
    }
    if let Some(ref p) = password {
        body.insert("password".into(), serde_json::Value::String(p.clone()));
    }

    if !body.is_empty() {
        req = req.json(&serde_json::Value::Object(body));
    }

    let res = req
        .send()
        .await
        .map_err(|e| format!("Network error: {e}"))?;

    if !res.status().is_success() {
        let status = res.status();
        let text = res.text().await.unwrap_or_default();
        return Err(format!("Pairing failed ({status}): {text}"));
    }

    let data: PairResponse = res
        .json()
        .await
        .map_err(|e| format!("Invalid response: {e}"))?;

    if data.paired {
        *state.token.lock().map_err(|e| e.to_string())? = Some(data.token.clone());
        // Persist token to data dir
        let token_path = state.data_dir.join("session_token");
        let _ = persist_session_token(&token_path, &data.token);
    }

    Ok(data)
}

/// Login via /api/auth/login — the new multi-user auth flow.
#[tauri::command]
async fn auth_login(
    username: String,
    password: String,
    device_id: Option<String>,
    device_name: Option<String>,
    state: tauri::State<'_, AppState>,
) -> Result<serde_json::Value, String> {
    let url = state.server_url.lock().map_err(|e| e.to_string())?.clone();

    let client = reqwest::Client::new();
    let res = client
        .post(format!("{url}/api/auth/login"))
        .header("Content-Type", "application/json")
        .json(&serde_json::json!({
            "username": username,
            "password": password,
            "device_id": device_id,
            "device_name": device_name,
        }))
        .send()
        .await
        .map_err(|e| format!("Network error: {e}"))?;

    if !res.status().is_success() {
        let text = res.text().await.unwrap_or_default();
        return Err(format!("Login failed: {text}"));
    }

    let data: serde_json::Value = res
        .json()
        .await
        .map_err(|e| format!("Invalid response: {e}"))?;

    if let Some(token) = data.get("token").and_then(|t| t.as_str()) {
        *state.token.lock().map_err(|e| e.to_string())? = Some(token.to_string());
        let token_path = state.data_dir.join("session_token");
        let _ = persist_session_token(&token_path, token);
    }

    Ok(data)
}

/// Register via /api/auth/register.
#[tauri::command]
async fn auth_register(
    username: String,
    password: String,
    state: tauri::State<'_, AppState>,
) -> Result<serde_json::Value, String> {
    let url = state.server_url.lock().map_err(|e| e.to_string())?.clone();

    let client = reqwest::Client::new();
    let res = client
        .post(format!("{url}/api/auth/register"))
        .header("Content-Type", "application/json")
        .json(&serde_json::json!({
            "username": username,
            "password": password,
        }))
        .send()
        .await
        .map_err(|e| format!("Network error: {e}"))?;

    if !res.status().is_success() {
        let text = res.text().await.unwrap_or_default();
        return Err(format!("Registration failed: {text}"));
    }

    res.json::<serde_json::Value>()
        .await
        .map_err(|e| format!("Invalid response: {e}"))
}

/// Check gateway health.
#[tauri::command]
async fn health_check(state: tauri::State<'_, AppState>) -> Result<HealthResponse, String> {
    let url = state.server_url.lock().map_err(|e| e.to_string())?.clone();

    let client = reqwest::Client::builder()
        .timeout(std::time::Duration::from_secs(5))
        .build()
        .map_err(|e| e.to_string())?;

    let res = client
        .get(format!("{url}/health"))
        .send()
        .await
        .map_err(|e| format!("Health check failed: {e}"))?;

    if !res.status().is_success() {
        return Err(format!("Health check failed ({})", res.status()));
    }

    res.json::<HealthResponse>()
        .await
        .map_err(|e| format!("Invalid response: {e}"))
}

/// Get the current server URL.
#[tauri::command]
fn get_server_url(state: tauri::State<'_, AppState>) -> Result<String, String> {
    Ok(state.server_url.lock().map_err(|e| e.to_string())?.clone())
}

/// Set the local gateway URL.
#[tauri::command]
fn set_server_url(url: String, state: tauri::State<'_, AppState>) -> Result<(), String> {
    *state.server_url.lock().map_err(|e| e.to_string())? = url;
    Ok(())
}

/// Get the relay server URL.
#[tauri::command]
fn get_relay_url(state: tauri::State<'_, AppState>) -> Result<String, String> {
    Ok(state.relay_url.lock().map_err(|e| e.to_string())?.clone())
}

/// Set the relay server URL.
#[tauri::command]
fn set_relay_url(url: String, state: tauri::State<'_, AppState>) -> Result<(), String> {
    *state.relay_url.lock().map_err(|e| e.to_string())? = url;
    Ok(())
}

/// Save an API key to the local ZeroClaw agent config.
#[tauri::command]
async fn save_api_key(
    provider: String,
    api_key: String,
    state: tauri::State<'_, AppState>,
) -> Result<(), String> {
    let server_url = state.server_url.lock().map_err(|e| e.to_string())?.clone();
    let token = state.token.lock().map_err(|e| e.to_string())?.clone();

    let client = reqwest::Client::builder()
        .timeout(std::time::Duration::from_secs(5))
        .build()
        .map_err(|e| e.to_string())?;

    let mut req = client
        .put(format!("{server_url}/api/config/api-key"))
        .header("Content-Type", "application/json")
        .json(&serde_json::json!({ "provider": provider, "api_key": api_key }));

    if let Some(ref t) = token {
        req = req.header("Authorization", format!("Bearer {t}"));
    }

    let res = req.send().await.map_err(|e| format!("Failed to save API key: {e}"))?;

    if res.status().is_success() {
        Ok(())
    } else {
        let text = res.text().await.unwrap_or_default();
        Err(format!("Failed to save API key: {text}"))
    }
}

/// Check if we have an active token.
#[tauri::command]
fn is_authenticated(state: tauri::State<'_, AppState>) -> Result<bool, String> {
    Ok(state
        .token
        .lock()
        .map_err(|e| e.to_string())?
        .is_some())
}

/// Clear the current auth token.
#[tauri::command]
fn disconnect(state: tauri::State<'_, AppState>) -> Result<(), String> {
    *state.token.lock().map_err(|e| e.to_string())? = None;
    state.sync_stop.store(true, Ordering::SeqCst);
    state.sync_connected.store(false, Ordering::SeqCst);
    // Remove persisted token
    let token_path = state.data_dir.join("session_token");
    let _ = std::fs::remove_file(token_path);
    Ok(())
}

/// Get platform info for the frontend.
#[tauri::command]
fn get_platform_info() -> serde_json::Value {
    serde_json::json!({
        "os": std::env::consts::OS,
        "arch": std::env::consts::ARCH,
        "is_mobile": cfg!(target_os = "android") || cfg!(target_os = "ios"),
    })
}

/// Generate a deterministic device fingerprint based on machine-specific info.
/// This fingerprint survives app reinstalls, enabling the server to recognize
/// the same physical device even when localStorage/app data is wiped.
///
/// Components: hostname + OS + arch + machine-id (platform-specific hardware identifier).
/// The machine-id provides true per-device uniqueness even when hostname/OS/arch overlap.
/// Cached device fingerprint — computed once and reused.
/// On Windows, `read_machine_id()` spawns `reg query` which causes a brief
/// UI freeze every time it runs. Caching eliminates repeated process spawns.
static DEVICE_FINGERPRINT_CACHE: std::sync::OnceLock<String> = std::sync::OnceLock::new();

#[tauri::command]
fn get_device_fingerprint() -> String {
    DEVICE_FINGERPRINT_CACHE
        .get_or_init(|| {
            use sha2::{Digest, Sha256};

            let hostname = hostname::get()
                .map(|h| h.to_string_lossy().to_string())
                .unwrap_or_default();
            let os = std::env::consts::OS;
            let arch = std::env::consts::ARCH;
            let machine_id = read_machine_id();

            let raw = format!("zeroclaw-fp:{}:{}:{}:{}", hostname, os, arch, machine_id);
            let hash = Sha256::digest(raw.as_bytes());
            format!("{:x}", hash)
        })
        .clone()
}

/// Read a platform-specific persistent machine identifier.
/// - Linux: /etc/machine-id (systemd) or /var/lib/dbus/machine-id
/// - macOS: IOPlatformSerialNumber via system_profiler
/// - Windows: MachineGuid from registry
/// Falls back to empty string if unavailable (fingerprint still works, just less unique).
fn read_machine_id() -> String {
    #[cfg(target_os = "linux")]
    {
        std::fs::read_to_string("/etc/machine-id")
            .or_else(|_| std::fs::read_to_string("/var/lib/dbus/machine-id"))
            .map(|s| s.trim().to_string())
            .unwrap_or_default()
    }
    #[cfg(target_os = "macos")]
    {
        std::process::Command::new("ioreg")
            .args(["-rd1", "-c", "IOPlatformExpertDevice"])
            .output()
            .ok()
            .and_then(|out| {
                let stdout = String::from_utf8_lossy(&out.stdout);
                stdout
                    .lines()
                    .find(|l| l.contains("IOPlatformSerialNumber"))
                    .and_then(|l| l.split('"').nth(3))
                    .map(|s| s.to_string())
            })
            .unwrap_or_default()
    }
    #[cfg(target_os = "windows")]
    {
        std::process::Command::new("reg")
            .args([
                "query",
                r"HKLM\SOFTWARE\Microsoft\Cryptography",
                "/v",
                "MachineGuid",
            ])
            .output()
            .ok()
            .and_then(|out| {
                let stdout = String::from_utf8_lossy(&out.stdout);
                stdout
                    .lines()
                    .find(|l| l.contains("MachineGuid"))
                    .and_then(|l| l.split_whitespace().last())
                    .map(|s| s.to_string())
            })
            .unwrap_or_default()
    }
    #[cfg(not(any(target_os = "linux", target_os = "macos", target_os = "windows")))]
    {
        String::new()
    }
}

// ── Sync Commands ────────────────────────────────────────────────

/// Get sync connection status.
#[tauri::command]
fn get_sync_status(state: tauri::State<'_, AppState>) -> SyncStatus {
    SyncStatus {
        connected: state.sync_connected.load(Ordering::SeqCst),
        device_id: get_device_id(&state.data_dir),
        last_sync: None,
    }
}

/// Trigger a full sync (Layer 3) via relay server.
#[tauri::command]
async fn trigger_full_sync(state: tauri::State<'_, AppState>) -> Result<String, String> {
    // Full sync uses the relay server, not the local gateway
    let server_url = state.relay_url.lock().map_err(|e| e.to_string())?.clone();
    let token = state
        .token
        .lock()
        .map_err(|e| e.to_string())?
        .clone()
        .ok_or_else(|| "Not authenticated".to_string())?;
    let device_id = get_device_id(&state.data_dir);

    // Upload a sync request via HTTP relay as a trigger
    let client = reqwest::Client::new();
    let payload = serde_json::json!({
        "type": "full_sync_request",
        "from_device_id": device_id,
        "manifest": {
            "memory_chunk_ids": [],
            "conversation_ids": [],
            "setting_keys": [],
            "generated_at": std::time::SystemTime::now()
                .duration_since(std::time::UNIX_EPOCH)
                .unwrap_or_default()
                .as_secs()
        }
    });

    let res = client
        .post(format!("{server_url}/api/sync/relay"))
        .header("Authorization", format!("Bearer {token}"))
        .json(&serde_json::json!({
            "encrypted_payload": payload.to_string(),
            "nonce": "full_sync_trigger"
        }))
        .send()
        .await
        .map_err(|e| format!("Full sync trigger failed: {e}"))?;

    if res.status().is_success() {
        Ok("Full sync triggered".to_string())
    } else {
        let text = res.text().await.unwrap_or_default();
        Err(format!("Full sync failed: {text}"))
    }
}

// ── Lifecycle Commands ───────────────────────────────────────────

/// Called when the app goes to background (mobile).
/// Saves state and reduces activity.
#[tauri::command]
fn on_app_pause(state: tauri::State<'_, AppState>) -> Result<(), String> {
    // Persist current token to disk for recovery
    if let Ok(guard) = state.token.lock() {
        if let Some(token) = guard.as_ref() {
            let token_path = state.data_dir.join("session_token");
            let _ = persist_session_token(&token_path, token);
        }
    }

    // Persist server URL
    if let Ok(url) = state.server_url.lock() {
        let url_path = state.data_dir.join("server_url");
        let _ = std::fs::write(url_path, url.as_str());
    }

    Ok(())
}

/// Called when the app returns to foreground (mobile).
/// Restores state and reconnects.
#[tauri::command]
async fn on_app_resume(state: tauri::State<'_, AppState>) -> Result<serde_json::Value, String> {
    let mut restored_token = false;
    let mut restored_url = false;

    // Restore token if lost from memory
    if state.token.lock().map_err(|e| e.to_string())?.is_none() {
        let token_path = state.data_dir.join("session_token");
        if let Ok(token) = std::fs::read_to_string(&token_path) {
            let token = token.trim().to_string();
            if !token.is_empty() {
                *state.token.lock().map_err(|e| e.to_string())? = Some(token);
                restored_token = true;
            }
        }
    }

    // Restore server URL
    let url_path = state.data_dir.join("server_url");
    if let Ok(url) = std::fs::read_to_string(&url_path) {
        let url = url.trim().to_string();
        if !url.is_empty() {
            *state.server_url.lock().map_err(|e| e.to_string())? = url;
            restored_url = true;
        }
    }

    // Try health check to verify connection.
    // Clone the URL out of the lock so the MutexGuard is dropped before await.
    let health_url = state
        .server_url
        .lock()
        .map_err(|e| e.to_string())?
        .clone();
    let is_online = {
        let client = reqwest::Client::builder()
            .timeout(std::time::Duration::from_secs(3))
            .build()
            .unwrap_or_default();
        client
            .get(format!("{}/health", health_url))
            .send()
            .await
            .is_ok()
    };

    state.sync_stop.store(false, Ordering::SeqCst);

    Ok(serde_json::json!({
        "restored_token": restored_token,
        "restored_url": restored_url,
        "is_online": is_online,
        "has_token": state.token.lock().map_err(|e| e.to_string())?.is_some(),
    }))
}

// ── Channel Pairing ─────────────────────────────────────────────

/// Generate a channel pairing code for linking a messaging channel
/// (KakaoTalk, WhatsApp, Telegram, etc.) to this MoA account.
///
/// The user sends this code in the channel chat to complete pairing.
/// After pairing, channel messages are automatically routed to this device.
#[tauri::command]
async fn generate_channel_pairing_code(
    channel: String,
    state: tauri::State<'_, AppState>,
) -> Result<serde_json::Value, String> {
    let relay_url = state
        .relay_url
        .lock()
        .map_err(|e| e.to_string())?
        .clone();
    let token = state
        .token
        .lock()
        .map_err(|e| e.to_string())?
        .clone()
        .ok_or("Not authenticated. Please login first.")?;

    let client = reqwest::Client::new();
    let res = client
        .post(format!("{}/api/channel-pairing/create", relay_url))
        .header("Authorization", format!("Bearer {}", token))
        .json(&serde_json::json!({
            "channel": channel,
        }))
        .send()
        .await
        .map_err(|e| format!("Network error: {e}"))?;

    if !res.status().is_success() {
        let status = res.status();
        let error = res.text().await.unwrap_or_default();
        return Err(format!("Server error ({}): {}", status, error));
    }

    let data: serde_json::Value = res
        .json()
        .await
        .map_err(|e| format!("Parse error: {e}"))?;

    Ok(data)
}

/// Get the list of supported channels and their pairing status.
#[tauri::command]
async fn get_channel_pairing_status(
    state: tauri::State<'_, AppState>,
) -> Result<serde_json::Value, String> {
    let relay_url = state
        .relay_url
        .lock()
        .map_err(|e| e.to_string())?
        .clone();
    let token = state
        .token
        .lock()
        .map_err(|e| e.to_string())?
        .clone()
        .ok_or("Not authenticated.")?;

    let client = reqwest::Client::new();
    let res = client
        .get(format!("{}/api/channel-pairing/status", relay_url))
        .header("Authorization", format!("Bearer {}", token))
        .send()
        .await
        .map_err(|e| format!("Network error: {e}"))?;

    if !res.status().is_success() {
        return Ok(serde_json::json!({ "channels": [] }));
    }

    let data: serde_json::Value = res
        .json()
        .await
        .map_err(|e| format!("Parse error: {e}"))?;

    Ok(data)
}

// ── Helpers ──────────────────────────────────────────────────────

/// Get or create a persistent device ID.
fn get_device_id(data_dir: &std::path::Path) -> String {
    let id_path = data_dir.join(".device_id");
    if let Ok(id) = std::fs::read_to_string(&id_path) {
        let id = id.trim().to_string();
        if !id.is_empty() {
            return id;
        }
    }
    let id = uuid::Uuid::new_v4().to_string();
    let _ = std::fs::create_dir_all(data_dir);
    let _ = std::fs::write(&id_path, &id);
    id
}

// ── ZeroClaw Gateway Lifecycle ────────────────────────────────────

/// Check if a ZeroClaw gateway is already running at the given URL.
async fn is_gateway_reachable(url: &str) -> bool {
    let client = reqwest::Client::builder()
        .timeout(std::time::Duration::from_secs(2))
        .build()
        .unwrap_or_default();
    client.get(format!("{url}/health")).send().await.is_ok()
}

/// Maximum number of automatic restart attempts for the sidecar process.
const MAX_SIDECAR_RETRIES: u32 = 3;

/// Maximum time to wait for the gateway health endpoint (30 seconds).
const GATEWAY_READY_TIMEOUT_MS: u64 = 30_000;

/// Interval for the watchdog health check (15 seconds).
const WATCHDOG_INTERVAL_SECS: u64 = 30;

/// Emit a gateway status event to the frontend so it can display progress.
fn emit_gateway_status(app_handle: &tauri::AppHandle, status: &str, message: &str) {
    let _ = app_handle.emit(
        "gateway-status",
        serde_json::json!({ "status": status, "message": message }),
    );
}

/// Launch the bundled runtime as a sidecar process.
///
/// MoA treats its backend runtime as an internal implementation detail:
/// 1. Check if a gateway is already running (development/manual start)
/// 2. If not, launch the bundled sidecar binary with `gateway` command
/// 3. Wait until the gateway health endpoint responds (30s timeout)
/// 4. On failure, auto-retry up to 3 times before marking as failed
/// 5. Mark gateway_running = true on success
///
/// Proxy HTTP requests to the local gateway via the Rust backend.
///
/// Tauri v2 webview (WebView2 on Windows) may block direct `fetch()` from
/// the custom protocol origin (`https://tauri.localhost`) to `http://127.0.0.1`.
/// This command bypasses that restriction by making the request through `reqwest`.
#[tauri::command]
async fn gateway_fetch(
    url: String,
    method: String,
    headers: std::collections::HashMap<String, String>,
    body: Option<String>,
) -> Result<serde_json::Value, String> {
    let client = reqwest::Client::builder()
        .timeout(std::time::Duration::from_secs(300))
        .build()
        .map_err(|e| e.to_string())?;

    let mut req = match method.to_uppercase().as_str() {
        "GET" => client.get(&url),
        "POST" => client.post(&url),
        "PUT" => client.put(&url),
        "DELETE" => client.delete(&url),
        _ => return Err(format!("Unsupported method: {method}")),
    };

    for (k, v) in &headers {
        req = req.header(k.as_str(), v.as_str());
    }
    if let Some(b) = body {
        req = req.body(b);
    }

    let res = req.send().await.map_err(|e| {
        format!("__NETWORK_ERROR__:{e}")
    })?;

    let status = res.status().as_u16();
    let response_body = res.text().await.unwrap_or_default();

    Ok(serde_json::json!({
        "status": status,
        "body": response_body,
    }))
}

/// Emits `gateway-status` events to the frontend for user feedback:
/// - `{ status: "starting", message: "..." }` — launch in progress
/// - `{ status: "ready", message: "..." }` — gateway is responding
/// - `{ status: "failed", message: "..." }` — all attempts exhausted
fn spawn_zeroclaw_gateway(app: &tauri::App) {
    let gateway_url = format!("http://{DEFAULT_GATEWAY_HOST}:{DEFAULT_GATEWAY_PORT}");
    let state = app.state::<AppState>();
    let gateway_running = state.gateway_running.clone();
    let app_handle = app.handle().clone();

    tauri::async_runtime::spawn(async move {
        // Step 0: Check if a previous zeroclaw is still running.
        // If so, reuse it instead of killing — the user may have chosen
        // "background mode" on previous close.
        // Step 1: Check if gateway is already running
        emit_gateway_status(&app_handle, "starting", "Checking for running gateway...");
        if is_gateway_reachable(&gateway_url).await {
            eprintln!("[MoA] Gateway already running at {gateway_url}");
            gateway_running.store(true, Ordering::SeqCst);
            emit_gateway_status(&app_handle, "ready", "Gateway connected");
            return;
        }

        // Step 2: Launch with auto-retry (up to MAX_SIDECAR_RETRIES attempts)
        for attempt in 1..=MAX_SIDECAR_RETRIES {
            eprintln!("[MoA] Starting backend service (attempt {attempt}/{MAX_SIDECAR_RETRIES})...");
            emit_gateway_status(
                &app_handle,
                "starting",
                &format!("Starting backend service (attempt {attempt}/{MAX_SIDECAR_RETRIES})..."),
            );

            // Track sidecar event receiver (Tauri sidecar) or std Child (PATH fallback)
            // so we can detect early termination and capture stderr.
            let mut sidecar_rx: Option<tauri::async_runtime::Receiver<tauri_plugin_shell::process::CommandEvent>> = None;
            let mut std_child: Option<std::process::Child> = None;

            // Create a fresh sidecar command each attempt (Command is consumed on spawn)
            let launch_result = match app_handle.shell().sidecar("zeroclaw") {
                Ok(sidecar) => {
                    match sidecar
                        .args(["gateway", "--host", DEFAULT_GATEWAY_HOST, "--port", &DEFAULT_GATEWAY_PORT.to_string()])
                        .spawn()
                    {
                        Ok((rx, _child)) => {
                            sidecar_rx = Some(rx);
                            Ok(())
                        }
                        Err(e) => Err(format!("Sidecar spawn failed: {e}")),
                    }
                }
                Err(e) => {
                    eprintln!("[MoA] Sidecar not found ({e}), trying system PATH...");
                    // Fall back: try to find binary on system PATH (development mode)
                    match std::process::Command::new("zeroclaw")
                        .args(["gateway", "--host", DEFAULT_GATEWAY_HOST, "--port", &DEFAULT_GATEWAY_PORT.to_string()])
                        .stdout(std::process::Stdio::piped())
                        .stderr(std::process::Stdio::piped())
                        .spawn()
                    {
                        Ok(child) => {
                            eprintln!("[MoA] Launched backend via system PATH (pid: {})", child.id());
                            std_child = Some(child);
                            Ok(())
                        }
                        Err(e2) => Err(format!("System PATH fallback failed: {e2}")),
                    }
                }
            };

            if let Err(ref err) = launch_result {
                eprintln!("[MoA] Failed to start backend service (attempt {attempt}): {err}");
                emit_gateway_status(
                    &app_handle,
                    "starting",
                    &format!("Retrying... ({attempt}/{MAX_SIDECAR_RETRIES})"),
                );
                if attempt < MAX_SIDECAR_RETRIES {
                    tokio::time::sleep(std::time::Duration::from_secs(2)).await;
                }
                continue;
            }

            // Step 3: Wait for gateway health (up to GATEWAY_READY_TIMEOUT_MS)
            // Also check if the process died early to avoid wasting 30s.
            let poll_interval_ms = 500;
            let max_polls = GATEWAY_READY_TIMEOUT_MS / poll_interval_ms;
            let mut process_died = false;
            let mut stderr_output = String::new();

            for i in 0..max_polls {
                tokio::time::sleep(std::time::Duration::from_millis(poll_interval_ms)).await;

                // Check if sidecar process terminated early (Tauri sidecar path)
                if let Some(ref mut rx) = sidecar_rx {
                    use tauri_plugin_shell::process::CommandEvent;
                    while let Ok(event) = rx.try_recv() {
                        match event {
                            CommandEvent::Stderr(data) => {
                                let line = String::from_utf8_lossy(&data);
                                let trimmed = line.trim();
                                if !trimmed.is_empty() {
                                    eprintln!("[MoA backend] {trimmed}");
                                    // Keep last 1024 chars for error reporting
                                    if stderr_output.len() < 1024 {
                                        stderr_output.push_str(trimmed);
                                        stderr_output.push('\n');
                                    }
                                }
                            }
                            CommandEvent::Stdout(data) => {
                                let line = String::from_utf8_lossy(&data);
                                let trimmed = line.trim();
                                if !trimmed.is_empty() {
                                    eprintln!("[MoA backend] {trimmed}");
                                }
                            }
                            CommandEvent::Error(msg) => {
                                eprintln!("[MoA] Backend process error: {msg}");
                                if stderr_output.len() < 1024 {
                                    stderr_output.push_str(&msg);
                                    stderr_output.push('\n');
                                }
                            }
                            CommandEvent::Terminated(payload) => {
                                eprintln!(
                                    "[MoA] Backend process terminated early (code: {:?}, signal: {:?})",
                                    payload.code, payload.signal
                                );
                                process_died = true;
                            }
                            _ => {}
                        }
                    }
                }

                // Check if std::process child terminated early (PATH fallback)
                if let Some(ref mut child) = std_child {
                    match child.try_wait() {
                        Ok(Some(status)) => {
                            eprintln!("[MoA] Backend process exited early with status: {status}");
                            // Capture stderr from the dead process
                            if let Some(mut stderr) = child.stderr.take() {
                                use std::io::Read;
                                let mut buf = vec![0u8; 4096];
                                if let Ok(n) = stderr.read(&mut buf) {
                                    let msg = String::from_utf8_lossy(&buf[..n]);
                                    let trimmed = msg.trim();
                                    if !trimmed.is_empty() {
                                        eprintln!("[MoA backend stderr] {trimmed}");
                                        stderr_output = trimmed.to_string();
                                    }
                                }
                            }
                            process_died = true;
                        }
                        Ok(None) => {} // still running
                        Err(e) => {
                            eprintln!("[MoA] Failed to check backend process status: {e}");
                        }
                    }
                }

                if process_died {
                    let detail = if stderr_output.is_empty() {
                        "Backend process crashed. Check ZeroClaw logs or run 'zeroclaw gateway' manually.".to_string()
                    } else {
                        format!("Backend process crashed: {}", stderr_output.lines().last().unwrap_or("unknown error"))
                    };
                    eprintln!("[MoA] {detail}");
                    emit_gateway_status(&app_handle, "starting", &detail);
                    break;
                }

                if is_gateway_reachable(&gateway_url).await {
                    eprintln!("[MoA] Backend service ready after {}ms", (i + 1) * poll_interval_ms);
                    gateway_running.store(true, Ordering::SeqCst);
                    emit_gateway_status(&app_handle, "ready", "Backend service ready");
                    return;
                }
            }

            if !process_died {
                eprintln!("[MoA] Backend service not responding after {GATEWAY_READY_TIMEOUT_MS}ms (attempt {attempt})");
            }
            if attempt < MAX_SIDECAR_RETRIES {
                tokio::time::sleep(std::time::Duration::from_secs(2)).await;
            }
        }

        eprintln!("[MoA] Error: Backend service failed to start after {MAX_SIDECAR_RETRIES} attempts");
        eprintln!("[MoA] Troubleshooting: run 'cargo build' from the workspace root, then retry.");
        eprintln!("[MoA] Or run 'zeroclaw gateway --host 127.0.0.1 --port 3000' manually to see errors.");
        emit_gateway_status(
            &app_handle,
            "failed",
            "Backend service failed to start. Run 'cargo build' first or check logs.",
        );
    });

    // ── Watchdog: periodically verify the gateway is still alive ──
    // If it dies after initial startup, mark it as down, notify the
    // frontend, and attempt a restart.
    start_gateway_watchdog(app);
}

/// Background watchdog that monitors the gateway health and restarts it if it dies.
fn start_gateway_watchdog(app: &tauri::App) {
    let gateway_url = format!("http://{DEFAULT_GATEWAY_HOST}:{DEFAULT_GATEWAY_PORT}");
    let state = app.state::<AppState>();
    let gateway_running = state.gateway_running.clone();
    let app_handle = app.handle().clone();

    tauri::async_runtime::spawn(async move {
        // Wait for initial startup to finish (up to 60s)
        let startup_deadline = MAX_SIDECAR_RETRIES as u64 * (GATEWAY_READY_TIMEOUT_MS / 1000 + 4);
        tokio::time::sleep(std::time::Duration::from_secs(startup_deadline.min(60))).await;

        let mut consecutive_failures: u32 = 0;

        loop {
            tokio::time::sleep(std::time::Duration::from_secs(WATCHDOG_INTERVAL_SECS)).await;

            if is_gateway_reachable(&gateway_url).await {
                if !gateway_running.load(Ordering::SeqCst) {
                    // Gateway recovered (e.g. user restarted it manually)
                    gateway_running.store(true, Ordering::SeqCst);
                    emit_gateway_status(&app_handle, "ready", "Backend service reconnected");
                    eprintln!("[MoA] Watchdog: gateway recovered");
                }
                consecutive_failures = 0;
                continue;
            }

            // Gateway is unreachable
            consecutive_failures += 1;

            // Require 2 consecutive failures to avoid false positives from transient hiccups
            if consecutive_failures < 2 {
                continue;
            }

            if gateway_running.load(Ordering::SeqCst) {
                eprintln!("[MoA] Watchdog: gateway is down, attempting restart...");
                gateway_running.store(false, Ordering::SeqCst);
                emit_gateway_status(&app_handle, "starting", "Backend service lost — restarting...");
            }

            // Attempt restart via sidecar
            let launched = match app_handle.shell().sidecar("zeroclaw") {
                Ok(sidecar) => sidecar
                    .args(["gateway", "--host", DEFAULT_GATEWAY_HOST, "--port", &DEFAULT_GATEWAY_PORT.to_string()])
                    .spawn()
                    .is_ok(),
                Err(_) => {
                    // Fallback to system PATH
                    std::process::Command::new("zeroclaw")
                        .args(["gateway", "--host", DEFAULT_GATEWAY_HOST, "--port", &DEFAULT_GATEWAY_PORT.to_string()])
                        .stdout(std::process::Stdio::piped())
                        .stderr(std::process::Stdio::piped())
                        .spawn()
                        .is_ok()
                }
            };

            if !launched {
                eprintln!("[MoA] Watchdog: restart launch failed");
                emit_gateway_status(&app_handle, "failed", "MoA 에이전트를 재시작할 수 없습니다");
                continue;
            }

            // Wait for the restarted gateway to become healthy
            let poll_interval_ms = 500;
            let max_polls = GATEWAY_READY_TIMEOUT_MS / poll_interval_ms;
            let mut recovered = false;
            for _ in 0..max_polls {
                tokio::time::sleep(std::time::Duration::from_millis(poll_interval_ms)).await;
                if is_gateway_reachable(&gateway_url).await {
                    recovered = true;
                    break;
                }
            }

            if recovered {
                eprintln!("[MoA] Watchdog: gateway restarted successfully");
                gateway_running.store(true, Ordering::SeqCst);
                emit_gateway_status(&app_handle, "ready", "Backend service restarted");
                consecutive_failures = 0;
            } else {
                eprintln!("[MoA] Watchdog: gateway restart timed out");
                emit_gateway_status(&app_handle, "failed", "MoA 에이전트가 응답하지 않습니다. 앱을 재시작해주세요.");
            }
        }
    });
}

/// Get the gateway running status.
#[tauri::command]
fn is_gateway_running(state: tauri::State<'_, AppState>) -> bool {
    state.gateway_running.load(Ordering::SeqCst)
}

/// Write a minimal ZeroClaw config.toml with provider and API key.
///
/// Called by the setup wizard to directly configure ZeroClaw before
/// the gateway starts. Creates ~/.zeroclaw/ directory and config.toml
/// with restrictive file permissions (0700 directory, 0600 file) to
/// protect API keys until the ZeroClaw gateway auto-encrypts them
/// via `SecretStore` on first config save.
#[tauri::command]
fn write_zeroclaw_config(
    provider: String,
    api_key: Option<String>,
    model: Option<String>,
) -> Result<String, String> {
    let home = dirs_next::home_dir().ok_or_else(|| "Cannot find home directory".to_string())?;
    let zeroclaw_dir = home.join(".zeroclaw");
    std::fs::create_dir_all(&zeroclaw_dir)
        .map_err(|e| format!("Failed to create ~/.zeroclaw: {e}"))?;

    // Set directory permissions to 0700 (owner-only access)
    #[cfg(unix)]
    {
        use std::os::unix::fs::PermissionsExt;
        let dir_perms = std::fs::Permissions::from_mode(0o700);
        std::fs::set_permissions(&zeroclaw_dir, dir_perms)
            .map_err(|e| format!("Failed to set directory permissions: {e}"))?;
    }

    let config_path = zeroclaw_dir.join("config.toml");

    // Read existing config if present to preserve other settings
    let existing = if config_path.exists() {
        std::fs::read_to_string(&config_path).unwrap_or_default()
    } else {
        String::new()
    };

    // Parse as TOML table for surgical updates
    let mut table: toml::Table = existing.parse().unwrap_or_else(|_| toml::Table::new());

    // Set provider
    table.insert(
        "default_provider".to_string(),
        toml::Value::String(provider.clone()),
    );

    // Set API key if provided — stored as plaintext initially.
    // The ZeroClaw gateway encrypts plaintext keys to `enc2:` format
    // (ChaCha20-Poly1305) on first config save via SecretStore.
    if let Some(ref key) = api_key {
        if !key.is_empty() {
            table.insert("api_key".to_string(), toml::Value::String(key.clone()));
        }
    }

    // Set model if provided
    if let Some(ref m) = model {
        if !m.is_empty() {
            table.insert(
                "default_model".to_string(),
                toml::Value::String(m.clone()),
            );
        }
    }

    // Ensure web_search timeout is at least 30 seconds
    let ws_table = table
        .entry("web_search".to_string())
        .or_insert_with(|| toml::Value::Table(toml::Table::new()));
    if let toml::Value::Table(ref mut wst) = ws_table {
        let current_timeout = wst
            .get("timeout_secs")
            .and_then(|v| v.as_integer())
            .unwrap_or(0);
        if current_timeout < 30 {
            wst.insert(
                "timeout_secs".to_string(),
                toml::Value::Integer(30),
            );
        }
    }

    // Enable secrets encryption so the gateway auto-encrypts on first save
    let secrets_table = table
        .entry("secrets".to_string())
        .or_insert_with(|| toml::Value::Table(toml::Table::new()));
    if let toml::Value::Table(ref mut st) = secrets_table {
        st.entry("encrypt".to_string())
            .or_insert(toml::Value::Boolean(true));
    }

    // Write config file
    let config_content = toml::to_string_pretty(&table)
        .map_err(|e| format!("Failed to serialize config: {e}"))?;
    std::fs::write(&config_path, &config_content)
        .map_err(|e| format!("Failed to write config.toml: {e}"))?;

    // Set file permissions to 0600 (owner read/write only)
    #[cfg(unix)]
    {
        use std::os::unix::fs::PermissionsExt;
        let file_perms = std::fs::Permissions::from_mode(0o600);
        std::fs::set_permissions(&config_path, file_perms)
            .map_err(|e| format!("Failed to set file permissions: {e}"))?;
    }

    Ok(config_path.to_string_lossy().to_string())
}

/// Save a channel configuration block to config.toml.
///
/// Merges the channel settings into the existing `[channels_config.<name>]`
/// section so the user doesn't have to manually edit the TOML file.
#[tauri::command]
fn save_channel_config(
    channel_name: String,
    config_values: std::collections::HashMap<String, String>,
) -> Result<String, String> {
    let home = dirs_next::home_dir().ok_or("Cannot find home directory")?;
    let config_path = home.join(".zeroclaw").join("config.toml");

    let existing = if config_path.exists() {
        std::fs::read_to_string(&config_path).unwrap_or_default()
    } else {
        String::new()
    };

    let mut table: toml::Table = existing
        .parse()
        .unwrap_or_else(|_| toml::Table::new());

    // Migration: if a stale [channels] section exists (from a previous bug),
    // move its contents to [channels_config] so the user's saved tokens aren't lost.
    if let Some(toml::Value::Table(stale)) = table.remove("channels") {
        let dest = table
            .entry("channels_config".to_string())
            .or_insert_with(|| toml::Value::Table(toml::Table::new()));
        if let Some(dest_table) = dest.as_table_mut() {
            for (k, v) in stale {
                dest_table.entry(k).or_insert(v);
            }
        }
    }

    // Get or create [channels_config.<channel_name>]
    // ZeroClaw uses `channels_config` (not `channels`) as the TOML section key.
    let channels = table
        .entry("channels_config".to_string())
        .or_insert_with(|| toml::Value::Table(toml::Table::new()));
    let channels_table = channels
        .as_table_mut()
        .ok_or("channels_config is not a table")?;
    let channel = channels_table
        .entry(channel_name.clone())
        .or_insert_with(|| toml::Value::Table(toml::Table::new()));
    let channel_table = channel
        .as_table_mut()
        .ok_or("channel section is not a table")?;

    // Merge values
    for (key, value) in &config_values {
        if value.starts_with('[') && value.ends_with(']') {
            // Parse as array of strings: ["a", "b"]
            if let Ok(arr) = serde_json::from_str::<Vec<String>>(value) {
                let toml_arr: Vec<toml::Value> = arr
                    .into_iter()
                    .map(toml::Value::String)
                    .collect();
                channel_table.insert(key.clone(), toml::Value::Array(toml_arr));
                continue;
            }
        }
        channel_table.insert(key.clone(), toml::Value::String(value.clone()));
    }

    // Write back
    let output = toml::to_string_pretty(&table)
        .map_err(|e| format!("Failed to serialize config: {e}"))?;
    std::fs::write(&config_path, &output)
        .map_err(|e| format!("Failed to write config.toml: {e}"))?;

    // Set file permissions to 0600
    #[cfg(unix)]
    {
        use std::os::unix::fs::PermissionsExt;
        let _ = std::fs::set_permissions(&config_path, std::fs::Permissions::from_mode(0o600));
    }

    tracing::info!(channel = channel_name.as_str(), "Channel config saved");
    Ok(format!("채널 설정이 저장되었습니다: {channel_name}"))
}

/// Check if ZeroClaw config.toml exists (setup already done).
#[tauri::command]
fn is_zeroclaw_configured() -> bool {
    dirs_next::home_dir()
        .map(|h| h.join(".zeroclaw").join("config.toml").exists())
        .unwrap_or(false)
}

// ── WebView Browsing & Crawling ──────────────────────────────────

/// Fetch a URL via the backend HTTP client, extract readable text,
/// and return it.  This is used when the user asks MoA to "look at
/// a web page" or "summarize this URL" — the frontend sends the URL
/// and receives clean text suitable for LLM summarization.
///
/// The extraction uses a lightweight HTML-to-text conversion so the
/// user never needs to know about DOM selectors.
#[tauri::command]
async fn browse_and_extract(
    url: String,
    state: tauri::State<'_, AppState>,
) -> Result<serde_json::Value, String> {
    let server_url = state.server_url.lock().map_err(|e| e.to_string())?.clone();
    let token = state.token.lock().map_err(|e| e.to_string())?.clone();

    // Delegate to the ZeroClaw gateway's web_fetch tool via the webhook.
    // This reuses the existing WebFetchTool (firecrawl/nanohtml2text)
    // and respects allowed_domains / blocked_domains config.
    let client = reqwest::Client::builder()
        .timeout(std::time::Duration::from_secs(30))
        .build()
        .map_err(|e| e.to_string())?;

    let mut req = client
        .post(format!("{server_url}/webhook"))
        .header("Content-Type", "application/json")
        .json(&serde_json::json!({
            "message": format!("Use the web_fetch tool to fetch and extract the text content from this URL: {url}"),
            "tool_hint": "web_fetch"
        }));

    if let Some(ref t) = token {
        req = req.header("Authorization", format!("Bearer {t}"));
    }

    let res = req.send().await.map_err(|e| format!("Browse failed: {e}"))?;
    if !res.status().is_success() {
        let text = res.text().await.unwrap_or_default();
        return Err(format!("Browse failed: {text}"));
    }

    let data: serde_json::Value = res.json().await.map_err(|e| format!("Invalid response: {e}"))?;
    Ok(data)
}

/// Perform a web search via the ZeroClaw gateway.
/// Supports standard search (DuckDuckGo/Brave/etc.) and
/// AI-powered search (Perplexity Sonar) when the user requests
/// comprehensive answers.
///
/// Parameters:
/// - `query`: The search query
/// - `use_perplexity`: If true, use Perplexity Sonar AI search
/// - `perplexity_api_key`: Optional user-provided Perplexity API key.
///   If absent and use_perplexity is true, the operator key is used
///   (with 2x credit deduction).
#[tauri::command]
async fn web_search(
    query: String,
    use_perplexity: Option<bool>,
    perplexity_api_key: Option<String>,
    state: tauri::State<'_, AppState>,
) -> Result<serde_json::Value, String> {
    let server_url = state.server_url.lock().map_err(|e| e.to_string())?.clone();
    let token = state.token.lock().map_err(|e| e.to_string())?.clone();

    let provider = if use_perplexity.unwrap_or(false) {
        "perplexity"
    } else {
        "duckduckgo"
    };

    let mut message = format!(
        "Use the web_search_tool with provider '{}' to search for: {}",
        provider, query
    );

    // If user provided their own Perplexity API key, include it as context
    if let Some(ref key) = perplexity_api_key {
        if !key.is_empty() {
            message.push_str(&format!("\n[User Perplexity API Key: {}]", key));
        }
    }

    let client = reqwest::Client::builder()
        .timeout(std::time::Duration::from_secs(30))
        .build()
        .map_err(|e| e.to_string())?;

    let mut req = client
        .post(format!("{server_url}/webhook"))
        .header("Content-Type", "application/json")
        .json(&serde_json::json!({
            "message": message,
            "tool_hint": "web_search"
        }));

    if let Some(ref t) = token {
        req = req.header("Authorization", format!("Bearer {t}"));
    }

    let res = req.send().await.map_err(|e| format!("Search failed: {e}"))?;
    if !res.status().is_success() {
        let text = res.text().await.unwrap_or_default();
        return Err(format!("Search failed: {text}"));
    }

    let data: serde_json::Value = res.json().await.map_err(|e| format!("Invalid response: {e}"))?;
    Ok(data)
}

// ── Document Processing Commands ─────────────────────────────────

// ── Embedded Python Environment ──────────────────────────────────
//
// MoA bundles its own Python virtual environment so users never need to
// open a terminal or run `pip install`. On first launch the app:
//   1. Locates a system Python 3 interpreter (python3 / python).
//   2. Creates a venv inside `~/.moa/python-env/`.
//   3. Installs pymupdf4llm (+ markdown) into that venv automatically.
// Subsequent launches reuse the existing venv.

/// Directory name for the embedded Python venv (lives inside ~/.moa/).
const PYTHON_VENV_DIR: &str = "python-env";
/// Packages to auto-install into the embedded venv.
const PYTHON_REQUIRED_PACKAGES: &[&str] = &["pymupdf4llm", "markdown"];

/// Return the path to the MoA-managed venv directory (`~/.moa/python-env`).
fn moa_venv_dir() -> Option<PathBuf> {
    dirs_next::home_dir().map(|h| h.join(".moa").join(PYTHON_VENV_DIR))
}

/// Return the Python binary inside the MoA venv, if the venv exists.
fn venv_python() -> Option<PathBuf> {
    let venv = moa_venv_dir()?;
    let bin = if cfg!(target_os = "windows") {
        venv.join("Scripts").join("python.exe")
    } else {
        venv.join("bin").join("python3")
    };
    if bin.exists() { Some(bin) } else { None }
}

/// Directory holding a python-build-standalone runtime that MoA
/// auto-downloads when no system Python 3 is present. Lives next to
/// the venv at `~/.moa/python-runtime/`.
const PYTHON_RUNTIME_DIR: &str = "python-runtime";

/// Pinned python-build-standalone release. Updating this to a newer
/// release date is the only step needed to ship a Python upgrade —
/// the URL pattern below is stable across releases.
const PBS_RELEASE_DATE: &str = "20240814";
const PBS_PYTHON_VERSION: &str = "3.12.5";

/// Return the path to the python binary inside the bootstrapped
/// runtime, regardless of whether it has been installed yet.
fn moa_runtime_python_path() -> Option<PathBuf> {
    let root = dirs_next::home_dir()?
        .join(".moa")
        .join(PYTHON_RUNTIME_DIR)
        .join("python");
    let bin = if cfg!(target_os = "windows") {
        root.join("python.exe")
    } else {
        root.join("bin").join("python3")
    };
    Some(bin)
}

/// Return the runtime python binary if it actually exists on disk.
fn moa_runtime_python() -> Option<PathBuf> {
    let p = moa_runtime_python_path()?;
    p.exists().then_some(p)
}

/// Find a usable Python 3 binary on the system (for creating the venv).
/// Priority: bootstrapped MoA runtime → system PATH.
fn find_system_python() -> Option<String> {
    if let Some(runtime_py) = moa_runtime_python() {
        return Some(runtime_py.to_string_lossy().into_owned());
    }
    let names = if cfg!(target_os = "windows") {
        vec!["python", "python3"]
    } else {
        vec!["python3", "python"]
    };
    for name in names {
        let check = std::process::Command::new(name)
            .arg("--version")
            .output();
        if let Ok(output) = check {
            if output.status.success() {
                let ver = String::from_utf8_lossy(&output.stdout);
                // Accept Python 3.10+
                if ver.contains("Python 3.") {
                    return Some(name.to_string());
                }
            }
        }
    }
    None
}

/// Download URL for the appropriate python-build-standalone tarball
/// based on the current target OS + architecture. Returns `None` for
/// unsupported targets (mobile, etc.).
fn pbs_download_url() -> Option<String> {
    let triple = if cfg!(all(target_os = "macos", target_arch = "aarch64")) {
        "aarch64-apple-darwin"
    } else if cfg!(all(target_os = "macos", target_arch = "x86_64")) {
        "x86_64-apple-darwin"
    } else if cfg!(all(target_os = "linux", target_arch = "aarch64")) {
        "aarch64-unknown-linux-gnu"
    } else if cfg!(all(target_os = "linux", target_arch = "x86_64")) {
        "x86_64-unknown-linux-gnu"
    } else if cfg!(all(target_os = "windows", target_arch = "x86_64")) {
        "x86_64-pc-windows-msvc-shared"
    } else {
        return None;
    };

    Some(format!(
        "https://github.com/astral-sh/python-build-standalone/releases/download/{date}/cpython-{ver}+{date}-{triple}-install_only.tar.gz",
        date = PBS_RELEASE_DATE,
        ver = PBS_PYTHON_VERSION,
        triple = triple,
    ))
}

/// Download a self-contained Python runtime from python-build-standalone
/// and extract it to `~/.moa/python-runtime/`. Returns the path to the
/// extracted python binary.
///
/// This is the **silent auto-install path**: end users never see a
/// "would you like to install Python?" dialog. We just download
/// (~30 MB) and extract to a folder under the MoA home directory —
/// no admin rights, no system PATH changes, no consent prompt.
async fn download_python_runtime(
    app_handle: &tauri::AppHandle,
) -> Result<PathBuf, String> {
    use tokio::process::Command;

    let emit = |stage: &str, detail: &str| {
        let _ = app_handle.emit(
            "python-env-status",
            serde_json::json!({ "stage": stage, "detail": detail }),
        );
    };

    let url = pbs_download_url().ok_or_else(|| {
        "Auto Python install not supported on this platform. Please install Python 3 manually.".to_string()
    })?;

    let runtime_dir = dirs_next::home_dir()
        .ok_or_else(|| "Cannot determine home directory.".to_string())?
        .join(".moa")
        .join(PYTHON_RUNTIME_DIR);

    if let Some(existing) = moa_runtime_python() {
        return Ok(existing);
    }

    std::fs::create_dir_all(&runtime_dir)
        .map_err(|e| format!("create runtime dir: {e}"))?;

    emit(
        "downloading_python",
        "Python을 자동으로 설치하고 있습니다 (약 30MB, 1회만 진행)...",
    );

    let archive_path = runtime_dir.join("python-runtime.tar.gz");
    let bytes = reqwest::get(&url)
        .await
        .map_err(|e| format!("download Python runtime: {e}"))?
        .error_for_status()
        .map_err(|e| format!("download HTTP error: {e}"))?
        .bytes()
        .await
        .map_err(|e| format!("download body: {e}"))?;
    std::fs::write(&archive_path, &bytes)
        .map_err(|e| format!("save archive: {e}"))?;

    emit("extracting_python", "Python 압축 해제 중...");

    // Extract via system `tar` (universal on macOS/Linux/Windows 10+).
    // python-build-standalone tarballs unpack into a `python/` directory
    // alongside the archive.
    let tar_status = Command::new("tar")
        .arg("-xzf")
        .arg(&archive_path)
        .arg("-C")
        .arg(&runtime_dir)
        .output()
        .await
        .map_err(|e| format!("spawn tar: {e}"))?;
    if !tar_status.status.success() {
        let stderr = String::from_utf8_lossy(&tar_status.stderr);
        return Err(format!("tar extract failed: {stderr}"));
    }

    let _ = std::fs::remove_file(&archive_path);

    let bin = moa_runtime_python_path()
        .ok_or_else(|| "Cannot resolve runtime python path".to_string())?;
    if !bin.exists() {
        return Err(format!(
            "Python runtime extracted but binary missing at {}",
            bin.display()
        ));
    }

    emit("python_installed", "Python 자동 설치 완료.");
    Ok(bin)
}

/// Resolve the best Python binary to use for running scripts.
/// Priority: MoA venv → system python.
fn resolve_python() -> Result<String, String> {
    if let Some(venv_py) = venv_python() {
        return Ok(venv_py.to_string_lossy().to_string());
    }
    find_system_python()
        .ok_or_else(|| "Python 3 not found. Please install Python 3.10 or later.".to_string())
}

/// Ensure the MoA Python venv exists and required packages are installed.
/// Called once at app startup. Emits `python-env-status` events to the
/// frontend so the UI can show a setup progress indicator.
async fn ensure_python_env(app_handle: tauri::AppHandle) {
    use tokio::process::Command;

    let emit = |stage: &str, detail: &str| {
        let _ = app_handle.emit(
            "python-env-status",
            serde_json::json!({ "stage": stage, "detail": detail }),
        );
    };

    // 1. Find a Python — system PATH or previously bootstrapped runtime.
    //    If neither exists, silently auto-download python-build-standalone
    //    (~30MB) into ~/.moa/python-runtime/. The user is informed via
    //    the python-env-status events but is NOT asked for consent —
    //    end users don't know what Python is and the user explicitly
    //    asked for transparent setup.
    let system_python = match find_system_python() {
        Some(p) => p,
        None => match download_python_runtime(&app_handle).await {
            Ok(path) => path.to_string_lossy().into_owned(),
            Err(e) => {
                emit(
                    "error",
                    &format!(
                        "Python 자동 설치에 실패했습니다. PDF 기능을 사용할 수 없습니다.\n원인: {e}"
                    ),
                );
                return;
            }
        },
    };

    let venv_dir = match moa_venv_dir() {
        Some(d) => d,
        None => {
            emit("error", "Cannot determine home directory.");
            return;
        }
    };

    let venv_py = if cfg!(target_os = "windows") {
        venv_dir.join("Scripts").join("python.exe")
    } else {
        venv_dir.join("bin").join("python3")
    };

    // 2. Create venv if it does not exist
    if !venv_py.exists() {
        emit("creating_venv", "Setting up Python environment...");
        let _ = std::fs::create_dir_all(venv_dir.parent().unwrap_or(&venv_dir));
        let result = Command::new(&system_python)
            .arg("-m")
            .arg("venv")
            .arg(venv_dir.to_string_lossy().as_ref())
            .output()
            .await;
        match result {
            Ok(output) if output.status.success() => {
                emit("venv_created", "Python environment created.");
            }
            Ok(output) => {
                let stderr = String::from_utf8_lossy(&output.stderr);
                emit("error", &format!("Failed to create Python venv: {stderr}"));
                return;
            }
            Err(e) => {
                emit("error", &format!("Failed to run Python: {e}"));
                return;
            }
        }
    }

    // 3. Install required packages (idempotent — pip will skip if already present)
    let pip_bin = if cfg!(target_os = "windows") {
        venv_dir.join("Scripts").join("pip.exe")
    } else {
        venv_dir.join("bin").join("pip")
    };

    // Quick check: can we import pymupdf4llm already?
    let check = Command::new(venv_py.to_string_lossy().as_ref())
        .arg("-c")
        .arg("import pymupdf4llm; import markdown")
        .output()
        .await;
    let already_installed = check.map_or(false, |o| o.status.success());

    if !already_installed {
        emit("installing_packages", "Installing PDF processing libraries (first-time setup)...");
        let mut args = vec!["install", "--quiet", "--disable-pip-version-check"];
        for pkg in PYTHON_REQUIRED_PACKAGES {
            args.push(pkg);
        }
        let result = Command::new(pip_bin.to_string_lossy().as_ref())
            .args(&args)
            .output()
            .await;
        match result {
            Ok(output) if output.status.success() => {
                emit("packages_installed", "PDF libraries installed successfully.");
            }
            Ok(output) => {
                let stderr = String::from_utf8_lossy(&output.stderr);
                emit("error", &format!("Failed to install packages: {stderr}"));
                return;
            }
            Err(e) => {
                emit("error", &format!("pip failed: {e}"));
                return;
            }
        }
    }

    emit("ready", "Python environment ready.");
}

/// Check the status of the embedded Python environment.
/// Returns whether venv exists and packages are importable.
#[tauri::command]
async fn check_python_env() -> Result<serde_json::Value, String> {
    let has_venv = venv_python().is_some();
    let mut packages_ok = false;

    if let Some(py) = venv_python() {
        let check = tokio::process::Command::new(py.to_string_lossy().as_ref())
            .arg("-c")
            .arg("import pymupdf4llm; import markdown; print('ok')")
            .output()
            .await;
        packages_ok = check.map_or(false, |o| o.status.success());
    }

    Ok(serde_json::json!({
        "venv_exists": has_venv,
        "packages_installed": packages_ok,
        "python_path": venv_python().map(|p| p.to_string_lossy().to_string()),
    }))
}

// ── PDF Conversion ───────────────────────────────────────────────

/// Convert a digital PDF to HTML/Markdown locally using PyMuPDF.
///
/// Uses the MoA-managed Python venv (auto-installed on first launch).
/// This handles digital (text-based) PDFs entirely on the user's machine —
/// no server or API key required.
///
/// Returns JSON with `html`, `markdown`, `page_count`, and `engine` fields.
#[tauri::command]
async fn convert_pdf_local(
    file_path: String,
    output_dir: Option<String>,
) -> Result<serde_json::Value, String> {
    let python = resolve_python()?;
    let script_path = find_pymupdf_script()
        .ok_or_else(|| "PyMuPDF conversion script not found.".to_string())?;

    let mut cmd = tokio::process::Command::new(&python);
    cmd.arg(&script_path).arg(&file_path);

    if let Some(ref dir) = output_dir {
        cmd.arg("--output-dir").arg(dir);
    }

    cmd.arg("--format").arg("both");

    let output = cmd
        .output()
        .await
        .map_err(|e| format!("Failed to run PyMuPDF: {e}"))?;

    let stdout = String::from_utf8_lossy(&output.stdout);

    match serde_json::from_str::<serde_json::Value>(&stdout) {
        Ok(result) => {
            if result.get("success").and_then(|s| s.as_bool()).unwrap_or(false) {
                Ok(result)
            } else {
                let error = result
                    .get("error")
                    .and_then(|e| e.as_str())
                    .unwrap_or("Unknown error");
                Err(format!("PDF conversion failed: {error}"))
            }
        }
        Err(_) => {
            let stderr = String::from_utf8_lossy(&output.stderr);
            Err(format!(
                "PyMuPDF output parsing failed. stdout: {stdout}, stderr: {stderr}"
            ))
        }
    }
}

/// Find the pymupdf_convert.py script in known locations.
fn find_pymupdf_script() -> Option<String> {
    let candidates = [
        // Development: running from clients/tauri/src-tauri/
        "../../../../scripts/pymupdf_convert.py",
        "../../../scripts/pymupdf_convert.py",
        "../../scripts/pymupdf_convert.py",
        "../scripts/pymupdf_convert.py",
        "scripts/pymupdf_convert.py",
        // Bundled alongside the binary
        "pymupdf_convert.py",
    ];

    // Try relative to current exe (covers bundled production builds)
    if let Ok(exe_path) = std::env::current_exe() {
        if let Some(exe_dir) = exe_path.parent() {
            // Tauri bundles resources next to the binary
            let resource_path = exe_dir.join("pymupdf_convert.py");
            if resource_path.exists() {
                return Some(resource_path.to_string_lossy().to_string());
            }
            for candidate in &candidates {
                let path = exe_dir.join(candidate);
                if path.exists() {
                    return Some(path.to_string_lossy().to_string());
                }
            }
        }
    }

    // Try relative to current dir
    for candidate in &candidates {
        let path = std::path::Path::new(candidate);
        if path.exists() {
            return Some(path.to_string_lossy().to_string());
        }
    }

    // Try from project root (MoA_new/scripts/)
    if let Ok(cwd) = std::env::current_dir() {
        let mut dir = cwd.as_path();
        for _ in 0..5 {
            let script = dir.join("scripts").join("pymupdf_convert.py");
            if script.exists() {
                return Some(script.to_string_lossy().to_string());
            }
            if let Some(parent) = dir.parent() {
                dir = parent;
            } else {
                break;
            }
        }
    }

    None
}

/// Write binary data (base64-encoded) to a temporary file.
/// Returns the generated temp file path. Used by the frontend to stage
/// PDF uploads for local conversion without needing tauri-plugin-path.
///
/// Note: The caller should invoke `cleanup_temp_file` after conversion
/// is complete to avoid leaving stale files in the OS temp directory.
#[tauri::command]
fn write_temp_file(base64_data: String, extension: Option<String>) -> Result<String, String> {
    use base64::Engine;
    let bytes = base64::engine::general_purpose::STANDARD
        .decode(&base64_data)
        .map_err(|e| format!("Base64 decode failed: {e}"))?;
    let ext = extension.unwrap_or_else(|| "tmp".to_string());
    let timestamp = std::time::SystemTime::now()
        .duration_since(std::time::UNIX_EPOCH)
        .map(|d| d.as_millis())
        .unwrap_or(0);
    let temp_path = std::env::temp_dir()
        .join(format!("moa_upload_{}_{}.{}", std::process::id(), timestamp, ext));
    std::fs::write(&temp_path, &bytes)
        .map_err(|e| format!("Failed to write temp file: {e}"))?;
    Ok(temp_path.to_string_lossy().to_string())
}

/// Remove a temporary file created by `write_temp_file`.
/// Only deletes files inside the OS temp directory with the `moa_upload_` prefix
/// to prevent misuse as a general file-deletion command.
#[tauri::command]
fn cleanup_temp_file(file_path: String) -> Result<(), String> {
    let path = std::path::Path::new(&file_path);
    let temp_dir = std::env::temp_dir();

    // Safety: only allow deletion of moa_upload_ files in the temp directory
    let in_temp = path.parent().map_or(false, |p| p == temp_dir);
    let is_moa = path
        .file_name()
        .and_then(|n| n.to_str())
        .map_or(false, |n| n.starts_with("moa_upload_"));

    if in_temp && is_moa {
        let _ = std::fs::remove_file(path);
    }
    Ok(())
}

// ── 2-Layer Document Commands ─────────────────────────────────────

/// Convert a PDF using the dual pipeline:
///   1. pdf2htmlEX → viewer HTML (layout-preserving, read-only)
///   2. PyMuPDF    → Markdown (structure for Tiptap editor)
///
/// If pdf2htmlEX is not installed, falls back to PyMuPDF for both layers.
/// Returns JSON with `viewer_html`, `markdown`, `page_count`, and `engine`.
#[tauri::command]
async fn convert_pdf_dual(
    file_path: String,
) -> Result<serde_json::Value, String> {
    let mut viewer_html = String::new();
    let mut markdown = String::new();
    let mut page_count: u64 = 0;
    let mut engine = String::new();

    // Step 1: Try pdf2htmlEX for viewer HTML (best layout preservation)
    let pdf2htmlex_path = which_pdf2htmlex();
    if let Some(ref pdf2htmlex_bin) = pdf2htmlex_path {
        let output_dir = std::env::temp_dir().join(format!("moa_pdf2html_{}", std::process::id()));
        let _ = std::fs::create_dir_all(&output_dir);

        // pdf2htmlEX names output after input file stem (e.g. "report.pdf" → "report.html")
        let input_stem = std::path::Path::new(&file_path)
            .file_stem()
            .and_then(|s| s.to_str())
            .unwrap_or("output");
        let output_file = output_dir.join(format!("{input_stem}.html"));

        let result = tokio::process::Command::new(pdf2htmlex_bin)
            .arg(&file_path)
            .arg("--dest-dir")
            .arg(output_dir.to_string_lossy().as_ref())
            .arg("--optimize-text")
            .arg("1")
            .arg("--embed-css")
            .arg("1")
            .arg("--embed-font")
            .arg("1")
            .arg("--embed-image")
            .arg("1")
            .arg("--embed-javascript")
            .arg("0")
            .arg("--embed-outline")
            .arg("1")
            .arg("--process-nontext")
            .arg("1")
            .output()
            .await;

        if let Ok(output) = result {
            if output.status.success() {
                // Read the generated HTML
                if let Ok(html) = std::fs::read_to_string(&output_file) {
                    viewer_html = html;
                    engine.push_str("pdf2htmlEX");
                }
            }
        }
        // Clean up temp dir (best effort)
        let _ = std::fs::remove_dir_all(&output_dir);
    }

    // Step 2: PyMuPDF for Markdown (structure extraction for editor)
    let pymupdf_script = find_pymupdf_script();
    if let Some(script_path) = pymupdf_script {
        let python = resolve_python().unwrap_or_else(|_| "python3".to_string());
        let output = tokio::process::Command::new(&python)
            .arg(&script_path)
            .arg(&file_path)
            .arg("--format")
            .arg("both")
            .output()
            .await;

        if let Ok(output) = output {
            let stdout = String::from_utf8_lossy(&output.stdout);
            if let Ok(result) = serde_json::from_str::<serde_json::Value>(&stdout) {
                if result.get("success").and_then(|s| s.as_bool()).unwrap_or(false) {
                    markdown = result.get("markdown").and_then(|m| m.as_str()).unwrap_or("").to_string();
                    page_count = result.get("page_count").and_then(|p| p.as_u64()).unwrap_or(0);

                    // If pdf2htmlEX was not available, use PyMuPDF HTML as viewer fallback
                    if viewer_html.is_empty() {
                        viewer_html = result.get("html").and_then(|h| h.as_str()).unwrap_or("").to_string();
                        engine = "pymupdf4llm".to_string();
                    } else {
                        engine.push_str("+pymupdf4llm");
                    }
                }
            }
        }
    }

    if viewer_html.is_empty() && markdown.is_empty() {
        return Err("Both pdf2htmlEX and PyMuPDF conversion failed. Install pdf2htmlEX and/or pymupdf4llm.".to_string());
    }

    // Handle partial failures: one engine succeeded but the other failed
    let mut warnings = Vec::new();
    if !viewer_html.is_empty() && markdown.is_empty() {
        warnings.push("PyMuPDF markdown extraction failed — editor will show HTML-only preview.");
        // Generate basic markdown from viewer HTML as fallback
        markdown = nanohtml2text::html2text(&viewer_html);
    }
    if viewer_html.is_empty() && !markdown.is_empty() {
        warnings.push("pdf2htmlEX not available — viewer will show simplified HTML from PyMuPDF.");
    }

    Ok(serde_json::json!({
        "success": true,
        "viewer_html": viewer_html,
        "markdown": markdown,
        "page_count": page_count,
        "engine": engine,
        "warnings": warnings,
    }))
}

/// Find the pdf2htmlEX binary on the system.
fn which_pdf2htmlex() -> Option<String> {
    // Common binary names for pdf2htmlEX
    let names = ["pdf2htmlEX", "pdf2htmlex"];
    for name in &names {
        if let Ok(output) = std::process::Command::new("which").arg(name).output() {
            if output.status.success() {
                let path = String::from_utf8_lossy(&output.stdout).trim().to_string();
                if !path.is_empty() {
                    return Some(path);
                }
            }
        }
    }
    None
}

/// Save a document's Markdown, Tiptap JSON, and editor HTML to the local filesystem.
///
/// Files are stored under the MoA documents directory:
///   ~/.moa/documents/<filename_stem>/
///     content.md         — Markdown (primary editable content)
///     content.json       — Tiptap JSON (structured document tree)
///     editor.html        — HTML rendered by Tiptap (for export)
///     viewer.html        — Original pdf2htmlEX HTML (if saved separately)
#[tauri::command]
fn save_document(
    file_name: String,
    markdown: String,
    tiptap_json: Option<String>,
    editor_html: Option<String>,
) -> Result<serde_json::Value, String> {
    let home = dirs_next::home_dir()
        .ok_or_else(|| "Cannot find home directory".to_string())?;
    let doc_dir = home
        .join(".moa")
        .join("documents")
        .join(sanitize_filename(&file_name));

    std::fs::create_dir_all(&doc_dir)
        .map_err(|e| format!("Failed to create document directory: {e}"))?;

    // Save Markdown
    let md_path = doc_dir.join("content.md");
    std::fs::write(&md_path, &markdown)
        .map_err(|e| format!("Failed to write content.md: {e}"))?;

    // Save Tiptap JSON
    if let Some(ref json_str) = tiptap_json {
        let json_path = doc_dir.join("content.json");
        std::fs::write(&json_path, json_str)
            .map_err(|e| format!("Failed to write content.json: {e}"))?;
    }

    // Save editor HTML
    if let Some(ref html) = editor_html {
        let html_path = doc_dir.join("editor.html");
        std::fs::write(&html_path, html)
            .map_err(|e| format!("Failed to write editor.html: {e}"))?;
    }

    Ok(serde_json::json!({
        "success": true,
        "path": doc_dir.to_string_lossy(),
    }))
}

/// Load a previously saved document's Markdown and optional Tiptap JSON.
#[tauri::command]
fn load_document(
    file_name: String,
) -> Result<serde_json::Value, String> {
    let home = dirs_next::home_dir()
        .ok_or_else(|| "Cannot find home directory".to_string())?;
    let doc_dir = home
        .join(".moa")
        .join("documents")
        .join(sanitize_filename(&file_name));

    let md_path = doc_dir.join("content.md");
    let json_path = doc_dir.join("content.json");

    let markdown = std::fs::read_to_string(&md_path).unwrap_or_default();
    let tiptap_json = std::fs::read_to_string(&json_path).ok();

    Ok(serde_json::json!({
        "success": true,
        "markdown": markdown,
        "tiptap_json": tiptap_json,
    }))
}

/// List previously saved documents from ~/.moa/documents/.
/// Returns an array of document names that can be loaded via `load_document`.
#[tauri::command]
fn list_documents() -> Result<serde_json::Value, String> {
    let home = dirs_next::home_dir()
        .ok_or_else(|| "Cannot find home directory".to_string())?;
    let doc_dir = home.join(".moa").join("documents");

    if !doc_dir.exists() {
        return Ok(serde_json::json!({ "documents": [] }));
    }

    let mut docs = Vec::new();
    let entries = std::fs::read_dir(&doc_dir)
        .map_err(|e| format!("Failed to read documents directory: {e}"))?;
    for entry in entries.flatten() {
        let path = entry.path();
        if path.is_dir() && path.join("content.md").exists() {
            if let Some(name) = path.file_name().and_then(|n| n.to_str()) {
                docs.push(name.to_string());
            }
        }
    }
    docs.sort();

    Ok(serde_json::json!({ "documents": docs }))
}

// ── SQLite FTS5 Document Storage ─────────────────────────────

/// Open (or create) the MoA documents SQLite database with FTS5 support.
/// Database path: ~/.moa/moa_documents.db
fn open_documents_db() -> Result<Connection, String> {
    let home = dirs_next::home_dir()
        .ok_or_else(|| "Cannot find home directory".to_string())?;
    let moa_dir = home.join(".moa");
    std::fs::create_dir_all(&moa_dir)
        .map_err(|e| format!("Failed to create .moa directory: {e}"))?;

    let db_path = moa_dir.join("moa_documents.db");
    let conn = Connection::open(&db_path)
        .map_err(|e| format!("Failed to open documents database: {e}"))?;

    // Create documents table
    conn.execute_batch(
        "CREATE TABLE IF NOT EXISTS documents (
            id INTEGER PRIMARY KEY AUTOINCREMENT,
            file_name TEXT NOT NULL,
            markdown TEXT NOT NULL,
            html TEXT,
            tiptap_json TEXT,
            doc_type TEXT,
            engine TEXT,
            created_at TEXT DEFAULT (datetime('now')),
            updated_at TEXT DEFAULT (datetime('now'))
        );
        CREATE UNIQUE INDEX IF NOT EXISTS idx_documents_file_name
            ON documents(file_name);
        "
    ).map_err(|e| format!("Failed to create documents table: {e}"))?;

    // Create FTS5 virtual table for full-text search
    conn.execute_batch(
        "CREATE VIRTUAL TABLE IF NOT EXISTS documents_fts USING fts5(
            file_name,
            markdown,
            content='documents',
            content_rowid='id',
            tokenize='unicode61'
        );
        -- Triggers to keep FTS index in sync
        CREATE TRIGGER IF NOT EXISTS documents_ai AFTER INSERT ON documents BEGIN
            INSERT INTO documents_fts(rowid, file_name, markdown)
            VALUES (new.id, new.file_name, new.markdown);
        END;
        CREATE TRIGGER IF NOT EXISTS documents_ad AFTER DELETE ON documents BEGIN
            INSERT INTO documents_fts(documents_fts, rowid, file_name, markdown)
            VALUES ('delete', old.id, old.file_name, old.markdown);
        END;
        CREATE TRIGGER IF NOT EXISTS documents_au AFTER UPDATE ON documents BEGIN
            INSERT INTO documents_fts(documents_fts, rowid, file_name, markdown)
            VALUES ('delete', old.id, old.file_name, old.markdown);
            INSERT INTO documents_fts(rowid, file_name, markdown)
            VALUES (new.id, new.file_name, new.markdown);
        END;
        "
    ).map_err(|e| format!("Failed to create FTS5 table: {e}"))?;

    Ok(conn)
}

/// Save a document to MoA's local SQLite database with FTS5 indexing.
/// This enables full-text search across all stored documents.
#[tauri::command]
fn save_document_to_sqlite(
    file_name: String,
    markdown: String,
    html: Option<String>,
    tiptap_json: Option<String>,
    doc_type: Option<String>,
    engine: Option<String>,
) -> Result<serde_json::Value, String> {
    let conn = open_documents_db()?;
    let sanitized = sanitize_filename(&file_name);

    conn.execute(
        "INSERT INTO documents (file_name, markdown, html, tiptap_json, doc_type, engine)
         VALUES (?1, ?2, ?3, ?4, ?5, ?6)
         ON CONFLICT(file_name) DO UPDATE SET
            markdown = excluded.markdown,
            html = excluded.html,
            tiptap_json = excluded.tiptap_json,
            doc_type = excluded.doc_type,
            engine = excluded.engine,
            updated_at = datetime('now')",
        rusqlite::params![
            sanitized,
            markdown,
            html,
            tiptap_json,
            doc_type.unwrap_or_default(),
            engine.unwrap_or_default(),
        ],
    ).map_err(|e| format!("Failed to save document to SQLite: {e}"))?;

    Ok(serde_json::json!({
        "success": true,
        "storage": "sqlite",
        "file_name": sanitized,
    }))
}

/// Load a document from MoA's SQLite database.
#[tauri::command]
fn load_document_from_sqlite(
    file_name: String,
) -> Result<serde_json::Value, String> {
    let conn = open_documents_db()?;
    let sanitized = sanitize_filename(&file_name);

    let result = conn.query_row(
        "SELECT file_name, markdown, html, tiptap_json, doc_type, engine, created_at, updated_at
         FROM documents WHERE file_name = ?1",
        rusqlite::params![sanitized],
        |row| {
            Ok(serde_json::json!({
                "success": true,
                "file_name": row.get::<_, String>(0)?,
                "markdown": row.get::<_, String>(1)?,
                "html": row.get::<_, Option<String>>(2)?,
                "tiptap_json": row.get::<_, Option<String>>(3)?,
                "doc_type": row.get::<_, Option<String>>(4)?.unwrap_or_default(),
                "engine": row.get::<_, Option<String>>(5)?.unwrap_or_default(),
                "created_at": row.get::<_, Option<String>>(6)?.unwrap_or_default(),
                "updated_at": row.get::<_, Option<String>>(7)?.unwrap_or_default(),
            }))
        },
    );

    match result {
        Ok(val) => Ok(val),
        Err(rusqlite::Error::QueryReturnedNoRows) => {
            Ok(serde_json::json!({ "success": false, "error": "Document not found" }))
        }
        Err(e) => Err(format!("Failed to load document from SQLite: {e}")),
    }
}

/// List all documents stored in MoA's SQLite database.
#[tauri::command]
fn list_documents_sqlite() -> Result<serde_json::Value, String> {
    let conn = open_documents_db()?;

    let mut stmt = conn.prepare(
        "SELECT file_name, doc_type, engine, created_at, updated_at FROM documents ORDER BY updated_at DESC"
    ).map_err(|e| format!("Failed to query documents: {e}"))?;

    let docs: Vec<serde_json::Value> = stmt.query_map([], |row| {
        Ok(serde_json::json!({
            "file_name": row.get::<_, String>(0)?,
            "doc_type": row.get::<_, Option<String>>(1)?.unwrap_or_default(),
            "engine": row.get::<_, Option<String>>(2)?.unwrap_or_default(),
            "created_at": row.get::<_, Option<String>>(3)?.unwrap_or_default(),
            "updated_at": row.get::<_, Option<String>>(4)?.unwrap_or_default(),
        }))
    }).map_err(|e| format!("Failed to iterate documents: {e}"))?
    .filter_map(|r| r.ok())
    .collect();

    Ok(serde_json::json!({ "documents": docs }))
}

/// Full-text search across all MoA documents using FTS5.
#[tauri::command]
fn search_documents_sqlite(
    query: String,
) -> Result<serde_json::Value, String> {
    let conn = open_documents_db()?;

    let mut stmt = conn.prepare(
        "SELECT d.file_name, d.doc_type, d.engine, d.updated_at,
                snippet(documents_fts, 1, '<mark>', '</mark>', '...', 48) as snippet
         FROM documents_fts f
         JOIN documents d ON d.id = f.rowid
         WHERE documents_fts MATCH ?1
         ORDER BY rank
         LIMIT 20"
    ).map_err(|e| format!("Failed to prepare search query: {e}"))?;

    let results: Vec<serde_json::Value> = stmt.query_map(rusqlite::params![query], |row| {
        Ok(serde_json::json!({
            "file_name": row.get::<_, String>(0)?,
            "doc_type": row.get::<_, Option<String>>(1)?.unwrap_or_default(),
            "engine": row.get::<_, Option<String>>(2)?.unwrap_or_default(),
            "updated_at": row.get::<_, Option<String>>(3)?.unwrap_or_default(),
            "snippet": row.get::<_, Option<String>>(4)?.unwrap_or_default(),
        }))
    }).map_err(|e| format!("Failed to execute search: {e}"))?
    .filter_map(|r| r.ok())
    .collect();

    Ok(serde_json::json!({ "results": results }))
}

/// Delete a document from MoA's SQLite database.
#[tauri::command]
fn delete_document_sqlite(
    file_name: String,
) -> Result<serde_json::Value, String> {
    let conn = open_documents_db()?;
    let sanitized = sanitize_filename(&file_name);

    let rows = conn.execute(
        "DELETE FROM documents WHERE file_name = ?1",
        rusqlite::params![sanitized],
    ).map_err(|e| format!("Failed to delete document: {e}"))?;

    Ok(serde_json::json!({
        "success": rows > 0,
        "deleted": rows,
    }))
}

// ── Hard Disk Save (user-chosen directory) ──────────────────

/// Save document files (markdown + HTML) to a user-specified directory.
/// The directory path is provided by the frontend after the user picks
/// a folder via the Tauri dialog plugin.
#[tauri::command]
fn save_document_to_disk(
    dir_path: String,
    file_name: String,
    markdown: String,
    html: Option<String>,
) -> Result<serde_json::Value, String> {
    let dir = std::path::Path::new(&dir_path);
    if !dir.exists() {
        std::fs::create_dir_all(dir)
            .map_err(|e| format!("Failed to create directory: {e}"))?;
    }

    let stem = sanitize_filename(&file_name);

    // Save Markdown
    let md_path = dir.join(format!("{stem}.md"));
    std::fs::write(&md_path, &markdown)
        .map_err(|e| format!("Failed to write markdown file: {e}"))?;

    // Save HTML if provided
    let mut html_saved = false;
    if let Some(ref html_content) = html {
        if !html_content.is_empty() {
            let html_path = dir.join(format!("{stem}.html"));
            std::fs::write(&html_path, html_content)
                .map_err(|e| format!("Failed to write HTML file: {e}"))?;
            html_saved = true;
        }
    }

    Ok(serde_json::json!({
        "success": true,
        "storage": "disk",
        "dir_path": dir_path,
        "markdown_path": md_path.to_string_lossy(),
        "html_saved": html_saved,
    }))
}

/// Sanitize a filename for use as a directory name (remove path separators and special chars).
fn sanitize_filename(name: &str) -> String {
    let stem = std::path::Path::new(name)
        .file_stem()
        .and_then(|s| s.to_str())
        .unwrap_or("untitled");
    stem.chars()
        .map(|c| if c.is_alphanumeric() || c == '-' || c == '_' || c == '.' { c } else { '_' })
        .collect()
}

// ── PR #6 Archive UI Commands ────────────────────────────────────

fn open_memory_db() -> Result<Connection, String> {
    let home = dirs_next::home_dir()
        .ok_or_else(|| "Cannot find home directory".to_string())?;
    let db_path = home.join(".moa").join("memory").join("brain.db");
    if !db_path.exists() {
        return Err("Memory database not found — start the agent first".into());
    }
    Connection::open(&db_path)
        .map_err(|e| format!("Failed to open memory database: {e}"))
}

#[tauri::command]
fn list_archived_memories() -> Result<serde_json::Value, String> {
    let conn = open_memory_db()?;
    let mut stmt = conn
        .prepare(
            "SELECT m.id, m.key, m.content, m.category, m.updated_at,
                    cm.summary, cm.fact_type
             FROM memories m
             LEFT JOIN consolidated_memories cm
               ON cm.source_ids LIKE '%' || m.id || '%'
             WHERE m.archived = 1
             ORDER BY m.updated_at DESC",
        )
        .map_err(|e| format!("Failed to prepare archive query: {e}"))?;

    let rows: Vec<serde_json::Value> = stmt
        .query_map([], |row| {
            Ok(serde_json::json!({
                "id": row.get::<_, String>(0)?,
                "key": row.get::<_, String>(1)?,
                "content": row.get::<_, String>(2)?,
                "category": row.get::<_, String>(3)?,
                "updated_at": row.get::<_, String>(4)?,
                "consolidated_summary": row.get::<_, Option<String>>(5)?,
                "consolidated_fact_type": row.get::<_, Option<String>>(6)?,
            }))
        })
        .map_err(|e| format!("Failed to query archived memories: {e}"))?
        .filter_map(|r| r.ok())
        .collect();

    Ok(serde_json::json!({ "archived": rows }))
}

#[tauri::command]
fn restore_archived_memory(memory_id: String) -> Result<bool, String> {
    let conn = open_memory_db()?;
    let changed = conn
        .execute(
            "UPDATE memories SET archived = 0 WHERE id = ?1 AND archived = 1",
            rusqlite::params![memory_id],
        )
        .map_err(|e| format!("Failed to restore memory: {e}"))?;
    Ok(changed > 0)
}

// ── Entry Point ──────────────────────────────────────────────────

#[cfg_attr(mobile, tauri::mobile_entry_point)]
pub fn run() {
    tauri::Builder::default()
        .plugin(tauri_plugin_shell::init())
        .plugin(tauri_plugin_dialog::init())
        .manage(AppState {
            // Local ZeroClaw gateway (runs on this device)
            server_url: std::sync::Mutex::new(
                std::env::var("MOA_LOCAL_GATEWAY_URL")
                    .unwrap_or_else(|_| format!("http://{DEFAULT_GATEWAY_HOST}:{DEFAULT_GATEWAY_PORT}")),
            ),
            // Railway relay server (memory sync + operator key fallback only)
            relay_url: std::sync::Mutex::new(
                std::env::var("MOA_RELAY_URL")
                    .unwrap_or_else(|_| "https://api.mymoa.app".to_string()),
            ),
            token: std::sync::Mutex::new(None),
            sync_connected: AtomicBool::new(false),
            sync_stop: AtomicBool::new(false),
            gateway_running: Arc::new(AtomicBool::new(false)),
            data_dir: {
                // Use platform-appropriate data directory
                let dir = if cfg!(target_os = "android") || cfg!(target_os = "ios") {
                    // On mobile, Tauri provides the app data path at runtime.
                    // We use a safe default that Tauri's setup will override.
                    PathBuf::from(".")
                } else {
                    dirs_next::data_dir()
                        .unwrap_or_else(|| PathBuf::from("."))
                        .join("com.moa.agent")
                };
                let _ = std::fs::create_dir_all(&dir);
                dir
            },
        })
        .invoke_handler(tauri::generate_handler![
            greet,
            chat,
            pair,
            auth_login,
            auth_register,
            health_check,
            get_server_url,
            set_server_url,
            get_relay_url,
            set_relay_url,
            save_api_key,
            is_authenticated,
            disconnect,
            get_platform_info,
            get_device_fingerprint,
            get_sync_status,
            trigger_full_sync,
            on_app_pause,
            on_app_resume,
            is_gateway_running,
            write_zeroclaw_config,
            save_channel_config,
            is_zeroclaw_configured,
            browse_and_extract,
            web_search,
            convert_pdf_local,
            convert_pdf_dual,
            check_python_env,
            write_temp_file,
            cleanup_temp_file,
            save_document,
            load_document,
            list_documents,
            save_document_to_sqlite,
            load_document_from_sqlite,
            list_documents_sqlite,
            search_documents_sqlite,
            delete_document_sqlite,
            save_document_to_disk,
            generate_channel_pairing_code,
            get_channel_pairing_status,
            gateway_fetch,
            list_archived_memories,
            restore_archived_memory,
            embedding_status::check_embedding_model,
            embedding_status::monitor_embedding_download,
        ])
        .setup(|app| {
            // Override data_dir with Tauri's actual app data path
            let app_data_dir = app.path().app_data_dir().unwrap_or_default();
            let _ = std::fs::create_dir_all(&app_data_dir);
            if let Some(state) = app.try_state::<AppState>() {
                // Restore persisted token on startup
                let token_path = app_data_dir.join("session_token");
                if let Ok(token) = std::fs::read_to_string(&token_path) {
                    let token = token.trim().to_string();
                    if !token.is_empty() {
                        if let Ok(mut guard) = state.token.lock() {
                            *guard = Some(token);
                        }
                    }
                }
                // Restore persisted server URL
                let url_path = app_data_dir.join("server_url");
                if let Ok(url) = std::fs::read_to_string(&url_path) {
                    let url = url.trim().to_string();
                    if !url.is_empty() {
                        if let Ok(mut guard) = state.server_url.lock() {
                            *guard = url;
                        }
                    }
                }
            }

            // ── Setup embedded Python environment ─────────────────────
            // Auto-create venv and install pymupdf4llm on first launch.
            // Runs in background so the main UI is not blocked.
            let py_handle = app.handle().clone();
            tauri::async_runtime::spawn(async move {
                ensure_python_env(py_handle).await;
            });

            // ── Launch ZeroClaw Gateway ──────────────────────────────
            // MoA's primary mission: start ZeroClaw so the user has a
            // local AI assistant running. Everything else (UI, sync,
            // operator keys) is built around this.
            spawn_zeroclaw_gateway(app);

            // ── Start Device-Link Client ─────────────────────────────
            // Connects to Railway /ws/device-link so this device can
            // receive relayed messages from web chat and channel chat.
            // Runs in background with auto-reconnect.
            if let Some(state) = app.try_state::<AppState>() {
                let relay_url = state.relay_url.lock().map(|g| g.clone()).unwrap_or_default();
                let token = state.token.lock().map(|g| g.clone().unwrap_or_default()).unwrap_or_default();
                let device_id = get_device_id(&app_data_dir);
                let local_gateway_url = state.server_url.lock().map(|g| g.clone()).unwrap_or_default();
                let connected_flag = Arc::new(AtomicBool::new(false));
                let stop_flag = Arc::new(AtomicBool::new(false));

                // Store flags for later access (sync_connected/sync_stop)
                state.sync_connected.store(false, Ordering::Relaxed);
                state.sync_stop.store(false, Ordering::Relaxed);

                let connected = connected_flag.clone();
                let stop = stop_flag.clone();

                tauri::async_runtime::spawn(async move {
                    // Wait for gateway to start before connecting device-link
                    tokio::time::sleep(tokio::time::Duration::from_secs(5)).await;

                    device_link::run_device_link_client(
                        relay_url,
                        token,
                        device_id,
                        local_gateway_url,
                        connected,
                        stop,
                    )
                    .await;
                });
            }

            #[cfg(debug_assertions)]
            {
                if let Some(window) = app.get_webview_window("main") {
                    window.open_devtools();
                }
            }
            Ok(())
        })
        .on_window_event(|window, event| {
            if let tauri::WindowEvent::CloseRequested { api, .. } = event {
                // Prevent default close — show choice dialog
                api.prevent_close();

                let win = window.clone();
                std::thread::spawn(move || {
                    use tauri_plugin_dialog::{DialogExt, MessageDialogButtons, MessageDialogKind};

                    // Ask user: quit completely or run in background?
                    let answer = win.dialog()
                        .message("MoA를 어떻게 종료할까요?\n\n\
                         [예] 완전 종료 — MoA와 백그라운드 서비스를 모두 종료합니다.\n\
                         [아니오] 백그라운드 유지 — 창만 닫고 AI 서비스는 계속 실행합니다.\n\
                                 (채널 메시지 수신, cron 작업 계속 실행)")
                        .title("MoA 종료")
                        .kind(MessageDialogKind::Info)
                        .buttons(MessageDialogButtons::YesNo)
                        .blocking_show();

                    if answer {
                        // YES = 완전 종료: kill zeroclaw + close app
                        #[cfg(target_os = "windows")]
                        {
                            let _ = std::process::Command::new("taskkill")
                                .args(["/f", "/im", "zeroclaw.exe"])
                                .stdout(std::process::Stdio::null())
                                .stderr(std::process::Stdio::null())
                                .status();
                        }
                        #[cfg(not(target_os = "windows"))]
                        {
                            let _ = std::process::Command::new("pkill")
                                .args(["-f", "zeroclaw.*gateway"])
                                .stdout(std::process::Stdio::null())
                                .stderr(std::process::Stdio::null())
                                .status();
                        }
                        // Close window and exit app
                        #[cfg(not(any(target_os = "ios", target_os = "android")))]
                        { let _ = win.destroy(); }
                        std::process::exit(0);
                    } else {
                        // NO = 백그라운드 유지: hide window, keep zeroclaw running
                        #[cfg(not(any(target_os = "ios", target_os = "android")))]
                        { let _ = win.hide(); }
                    }
                });
            }
        })
        .run(tauri::generate_context!())
        .expect("error while running MoA application");
}
