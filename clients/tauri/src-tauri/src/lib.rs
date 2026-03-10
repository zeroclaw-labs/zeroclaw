use serde::{Deserialize, Serialize};
use std::path::PathBuf;
use std::sync::atomic::{AtomicBool, Ordering};
use std::sync::Arc;
use tauri::{Emitter, Manager};
use tauri_plugin_shell::ShellExt;

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
#[tauri::command]
async fn chat(
    message: String,
    state: tauri::State<'_, AppState>,
) -> Result<ChatResponse, String> {
    let server_url = state.server_url.lock().map_err(|e| e.to_string())?.clone();
    let token = state
        .token
        .lock()
        .map_err(|e| e.to_string())?
        .clone()
        .ok_or_else(|| "Not authenticated. Please pair first.".to_string())?;

    let client = reqwest::Client::new();
    let res = client
        .post(format!("{server_url}/webhook"))
        .header("Content-Type", "application/json")
        .header("Authorization", format!("Bearer {token}"))
        .json(&serde_json::json!({ "message": message }))
        .send()
        .await
        .map_err(|e| format!("Network error: {e}"))?;

    if res.status() == 401 {
        *state.token.lock().map_err(|e| e.to_string())? = None;
        return Err("Authentication expired. Please re-pair.".to_string());
    }

    if !res.status().is_success() {
        let status = res.status();
        let text = res.text().await.unwrap_or_default();
        return Err(format!("Chat failed ({status}): {text}"));
    }

    res.json::<ChatResponse>()
        .await
        .map_err(|e| format!("Invalid response: {e}"))
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
        let _ = std::fs::write(&token_path, &data.token);
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
        let _ = std::fs::write(&token_path, token);
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
            let _ = std::fs::write(token_path, token);
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

            // Create a fresh sidecar command each attempt (Command is consumed on spawn)
            let launch_result = match app_handle.shell().sidecar("zeroclaw") {
                Ok(sidecar) => {
                    match sidecar
                        .args(["gateway", "--host", DEFAULT_GATEWAY_HOST, "--port", &DEFAULT_GATEWAY_PORT.to_string()])
                        .spawn()
                    {
                        Ok(_) => Ok(()),
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
                        Ok(_) => Ok(()),
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
            let poll_interval_ms = 500;
            let max_polls = GATEWAY_READY_TIMEOUT_MS / poll_interval_ms;
            for i in 0..max_polls {
                tokio::time::sleep(std::time::Duration::from_millis(poll_interval_ms)).await;
                if is_gateway_reachable(&gateway_url).await {
                    eprintln!("[MoA] Backend service ready after {}ms", (i + 1) * poll_interval_ms);
                    gateway_running.store(true, Ordering::SeqCst);
                    emit_gateway_status(&app_handle, "ready", "Backend service ready");
                    return;
                }
            }

            eprintln!("[MoA] Backend service not responding after {GATEWAY_READY_TIMEOUT_MS}ms (attempt {attempt})");
            if attempt < MAX_SIDECAR_RETRIES {
                tokio::time::sleep(std::time::Duration::from_secs(2)).await;
            }
        }

        eprintln!("[MoA] Error: Backend service failed to start after {MAX_SIDECAR_RETRIES} attempts");
        emit_gateway_status(
            &app_handle,
            "failed",
            "Backend service failed to start. Please ensure ZeroClaw is installed or check the logs.",
        );
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
            "tool_hint": "web_search_tool"
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

// ── Entry Point ──────────────────────────────────────────────────

#[cfg_attr(mobile, tauri::mobile_entry_point)]
pub fn run() {
    tauri::Builder::default()
        .plugin(tauri_plugin_shell::init())
        .manage(AppState {
            // Local ZeroClaw gateway (runs on this device)
            server_url: std::sync::Mutex::new(
                std::env::var("MOA_LOCAL_GATEWAY_URL")
                    .unwrap_or_else(|_| format!("http://{DEFAULT_GATEWAY_HOST}:{DEFAULT_GATEWAY_PORT}")),
            ),
            // Railway relay server (memory sync + operator key fallback only)
            relay_url: std::sync::Mutex::new(
                std::env::var("MOA_RELAY_URL")
                    .unwrap_or_else(|_| "https://moanew-production.up.railway.app".to_string()),
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
            get_sync_status,
            trigger_full_sync,
            on_app_pause,
            on_app_resume,
            is_gateway_running,
            write_zeroclaw_config,
            is_zeroclaw_configured,
            browse_and_extract,
            web_search,
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

            // ── Launch ZeroClaw Gateway ──────────────────────────────
            // MoA's primary mission: start ZeroClaw so the user has a
            // local AI assistant running. Everything else (UI, sync,
            // operator keys) is built around this.
            spawn_zeroclaw_gateway(app);

            #[cfg(debug_assertions)]
            {
                if let Some(window) = app.get_webview_window("main") {
                    window.open_devtools();
                }
            }
            Ok(())
        })
        .run(tauri::generate_context!())
        .expect("error while running MoA application");
}
