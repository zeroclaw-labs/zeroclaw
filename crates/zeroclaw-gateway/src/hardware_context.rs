//! Hardware context management endpoints.

use super::AppState;
use axum::{
    extract::State,
    http::{HeaderMap, StatusCode},
    response::{IntoResponse, Json},
};
use serde::{Deserialize, Serialize};
use std::path::PathBuf;
use tokio::fs;
use tokio::io::AsyncWriteExt as _;

/// Maximum bytes allowed in a single append payload.
const MAX_APPEND_BYTES: usize = 32_768; // 32 KB

// ── Auth helper (re-uses the pattern from api.rs) ─────────────────────────────

fn require_auth(
    state: &AppState,
    headers: &HeaderMap,
) -> Result<(), (StatusCode, Json<serde_json::Value>)> {
    if !state.pairing.require_pairing() {
        return Ok(());
    }
    let token = headers
        .get(axum::http::header::AUTHORIZATION)
        .and_then(|v| v.to_str().ok())
        .and_then(|auth| auth.strip_prefix("Bearer "))
        .unwrap_or("");
    if state.pairing.is_authenticated(token) {
        Ok(())
    } else {
        Err((
            StatusCode::UNAUTHORIZED,
            Json(serde_json::json!({
                "error": "Unauthorized — pair first via POST /pair, then send Authorization: Bearer <token>"
            })),
        ))
    }
}

// ── Path helpers ──────────────────────────────────────────────────────────────

/// Return `~/.zeroclaw/hardware/` or an error string.
fn hardware_dir() -> Result<PathBuf, String> {
    directories::BaseDirs::new()
        .map(|b| b.home_dir().join(".zeroclaw").join("hardware"))
        .ok_or_else(|| "Cannot determine home directory".to_string())
}

/// Validate a device alias: must be non-empty, ≤64 chars, and consist only of
/// alphanumerics, hyphens, and underscores.  Returns an error message on failure.
fn validate_device_alias(alias: &str) -> Result<(), &'static str> {
    if alias.is_empty() || alias.len() > 64 {
        return Err("Device alias must be 1–64 characters");
    }
    if !alias
        .chars()
        .all(|c| c.is_alphanumeric() || c == '-' || c == '_')
    {
        return Err("Device alias must contain only alphanumerics, hyphens, and underscores");
    }
    Ok(())
}

/// Return the path to a device context file, after validating the alias.
fn device_file_path(hw_dir: &std::path::Path, alias: &str) -> Result<PathBuf, &'static str> {
    validate_device_alias(alias)?;
    Ok(hw_dir.join("devices").join(format!("{alias}.md")))
}

// ── POST /api/hardware/pin ────────────────────────────────────────────────────

#[derive(Debug, Deserialize)]
pub struct PinRegistrationBody {
    /// Device alias (default: "rpi0").
    #[serde(default = "default_device")]
    pub device: String,
    /// BCM GPIO number.
    pub pin: u32,
    /// Component type/name, e.g. "LED", "Button", "Servo".
    pub component: String,
    /// Optional human notes about this pin, e.g. "red LED, active HIGH".
    #[serde(default)]
    pub notes: String,
}

fn default_device() -> String {
    "rpi0".to_string()
}

pub async fn handle_hardware_pin(
    State(state): State<AppState>,
    headers: HeaderMap,
    body: Result<Json<PinRegistrationBody>, axum::extract::rejection::JsonRejection>,
) -> impl IntoResponse {
    if let Err(e) = require_auth(&state, &headers) {
        return e.into_response();
    }

    let Json(req) = match body {
        Ok(b) => b,
        Err(e) => {
            return (
                StatusCode::BAD_REQUEST,
                Json(serde_json::json!({ "error": format!("Invalid JSON: {e}") })),
            )
                .into_response();
        }
    };

    if req.component.is_empty() {
        return (
            StatusCode::BAD_REQUEST,
            Json(serde_json::json!({ "error": "\"component\" must not be empty" })),
        )
            .into_response();
    }
    // Sanitize component + notes: strip newlines to prevent line-injection.
    let component = req.component.replace(['\n', '\r'], " ");
    let notes = req.notes.replace(['\n', '\r'], " ");

    let hw_dir = match hardware_dir() {
        Ok(d) => d,
        Err(e) => {
            return (
                StatusCode::INTERNAL_SERVER_ERROR,
                Json(serde_json::json!({ "error": e })),
            )
                .into_response();
        }
    };

    let device_path = match device_file_path(&hw_dir, &req.device) {
        Ok(p) => p,
        Err(e) => {
            return (
                StatusCode::BAD_REQUEST,
                Json(serde_json::json!({ "error": e })),
            )
                .into_response();
        }
    };

    // Create devices dir + file if missing, then append.
    if let Some(parent) = device_path.parent()
        && let Err(e) = fs::create_dir_all(parent).await
    {
        return (
            StatusCode::INTERNAL_SERVER_ERROR,
            Json(serde_json::json!({ "error": format!("Failed to create directory: {e}") })),
        )
            .into_response();
    }

    let line = if notes.is_empty() {
        format!("- GPIO {}: {}\n", req.pin, component)
    } else {
        format!("- GPIO {}: {} — {}\n", req.pin, component, notes)
    };

    match append_to_file(&device_path, &line).await {
        Ok(()) => {
            let message = format!(
                "GPIO {} registered as {} on {}",
                req.pin, component, req.device
            );
            ::zeroclaw_log::record!(INFO, ::zeroclaw_log::Event::new(module_path!(), ::zeroclaw_log::Action::Note).with_attrs(::serde_json::json!({"device": req.device, "pin": req.pin, "component": component})), &format!("{}", message));
            (
                StatusCode::OK,
                Json(serde_json::json!({ "ok": true, "message": message })),
            )
                .into_response()
        }
        Err(e) => (
            StatusCode::INTERNAL_SERVER_ERROR,
            Json(serde_json::json!({ "error": format!("Failed to write: {e}") })),
        )
            .into_response(),
    }
}

// ── POST /api/hardware/context ────────────────────────────────────────────────

#[derive(Debug, Deserialize)]
pub struct ContextAppendBody {
    /// Device alias (default: "rpi0").
    #[serde(default = "default_device")]
    pub device: String,
    /// Raw markdown string to append to the device file.
    pub content: String,
}

/// `POST /api/hardware/context` — append raw markdown to a device file.
pub async fn handle_hardware_context_post(
    State(state): State<AppState>,
    headers: HeaderMap,
    body: Result<Json<ContextAppendBody>, axum::extract::rejection::JsonRejection>,
) -> impl IntoResponse {
    if let Err(e) = require_auth(&state, &headers) {
        return e.into_response();
    }

    let Json(req) = match body {
        Ok(b) => b,
        Err(e) => {
            return (
                StatusCode::BAD_REQUEST,
                Json(serde_json::json!({ "error": format!("Invalid JSON: {e}") })),
            )
                .into_response();
        }
    };

    if req.content.is_empty() {
        return (
            StatusCode::BAD_REQUEST,
            Json(serde_json::json!({ "error": "\"content\" must not be empty" })),
        )
            .into_response();
    }
    if req.content.len() > MAX_APPEND_BYTES {
        return (
            StatusCode::PAYLOAD_TOO_LARGE,
            Json(serde_json::json!({
                "error": format!("Content too large — max {} bytes", MAX_APPEND_BYTES)
            })),
        )
            .into_response();
    }

    let hw_dir = match hardware_dir() {
        Ok(d) => d,
        Err(e) => {
            return (
                StatusCode::INTERNAL_SERVER_ERROR,
                Json(serde_json::json!({ "error": e })),
            )
                .into_response();
        }
    };

    let device_path = match device_file_path(&hw_dir, &req.device) {
        Ok(p) => p,
        Err(e) => {
            return (
                StatusCode::BAD_REQUEST,
                Json(serde_json::json!({ "error": e })),
            )
                .into_response();
        }
    };

    if let Some(parent) = device_path.parent()
        && let Err(e) = fs::create_dir_all(parent).await
    {
        return (
            StatusCode::INTERNAL_SERVER_ERROR,
            Json(serde_json::json!({ "error": format!("Failed to create directory: {e}") })),
        )
            .into_response();
    }

    // Ensure content ends with a newline so successive appends don't merge lines.
    let mut content = req.content.clone();
    if !content.ends_with('\n') {
        content.push('\n');
    }

    match append_to_file(&device_path, &content).await {
        Ok(()) => {
            ::zeroclaw_log::record!(
                INFO,
                ::zeroclaw_log::Event::new(module_path!(), ::zeroclaw_log::Action::Note)
                    .with_attrs(
                        ::serde_json::json!({"device": req.device, "bytes": content.len()})
                    ),
                "Hardware context appended"
            );
            (StatusCode::OK, Json(serde_json::json!({ "ok": true }))).into_response()
        }
        Err(e) => (
            StatusCode::INTERNAL_SERVER_ERROR,
            Json(serde_json::json!({ "error": format!("Failed to write: {e}") })),
        )
            .into_response(),
    }
}

// ── GET /api/hardware/context ─────────────────────────────────────────────────

#[derive(Debug, Serialize)]
struct HardwareContextResponse {
    hardware_md: String,
    devices: std::collections::HashMap<String, String>,
}

/// `GET /api/hardware/context` — return all current hardware context file contents.
pub async fn handle_hardware_context_get(
    State(state): State<AppState>,
    headers: HeaderMap,
) -> impl IntoResponse {
    if let Err(e) = require_auth(&state, &headers) {
        return e.into_response();
    }

    let hw_dir = match hardware_dir() {
        Ok(d) => d,
        Err(e) => {
            return (
                StatusCode::INTERNAL_SERVER_ERROR,
                Json(serde_json::json!({ "error": e })),
            )
                .into_response();
        }
    };

    // Read HARDWARE.md
    let hardware_md = fs::read_to_string(hw_dir.join("HARDWARE.md"))
        .await
        .unwrap_or_default();

    // Read all device files
    let devices_dir = hw_dir.join("devices");
    let mut devices = std::collections::HashMap::new();
    if let Ok(mut entries) = fs::read_dir(&devices_dir).await {
        while let Ok(Some(entry)) = entries.next_entry().await {
            let path = entry.path();
            if path.extension().and_then(|e| e.to_str()) == Some("md") {
                let alias = path
                    .file_stem()
                    .and_then(|s| s.to_str())
                    .unwrap_or("")
                    .to_string();
                if !alias.is_empty() {
                    let content = fs::read_to_string(&path).await.unwrap_or_default();
                    devices.insert(alias, content);
                }
            }
        }
    }

    let resp = HardwareContextResponse {
        hardware_md,
        devices,
    };
    (StatusCode::OK, Json(resp)).into_response()
}

// ── POST /api/hardware/reload ─────────────────────────────────────────────────

pub async fn handle_hardware_reload(
    State(state): State<AppState>,
    headers: HeaderMap,
) -> impl IntoResponse {
    if let Err(e) = require_auth(&state, &headers) {
        return e.into_response();
    }

    // Count currently-registered tools in the gateway state
    let tool_count = state.tools_registry.len();

    // Reload hardware context from disk (same function used by the agent loop)
    let context = zeroclaw_hardware::load_hardware_context_prompt(&[]);
    let context_length = context.len();

    ::zeroclaw_log::record!(
        INFO,
        ::zeroclaw_log::Event::new(module_path!(), ::zeroclaw_log::Action::Note).with_attrs(
            ::serde_json::json!({"context_length": context_length, "tool_count": tool_count})
        ),
        "Hardware context reloaded (on-disk read)"
    );

    (
        StatusCode::OK,
        Json(serde_json::json!({
            "ok": true,
            "tools": tool_count,
            "context_length": context_length,
        })),
    )
        .into_response()
}

// ── File I/O helper ───────────────────────────────────────────────────────────

async fn append_to_file(path: &std::path::Path, content: &str) -> std::io::Result<()> {
    let mut file = tokio::fs::OpenOptions::new()
        .create(true)
        .append(true)
        .open(path)
        .await?;
    file.write_all(content.as_bytes()).await?;
    file.flush().await?;
    Ok(())
}
