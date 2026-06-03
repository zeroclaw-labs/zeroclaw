//! Read/write endpoints for per-agent personality markdown files
//! (`SOUL.md`, `IDENTITY.md`, `USER.md`, `AGENTS.md`, `TOOLS.md`,
//! `HEARTBEAT.md`, `BOOTSTRAP.md`, `MEMORY.md`).
//!
//! The runtime injects these into the system prompt at request time
//! (see `zeroclaw_runtime::agent::personality::load_personality`). This
//! module is the dashboard's authoring surface for them.
//!
//! Sandbox: filenames are matched against the static `EDITABLE_PERSONALITY_FILES`
//! allowlist re-exported from the runtime crate. The on-disk path is
//! built from a `&'static str` taken from that allowlist plus the
//! agent's workspace dir resolved via `Config::agent_workspace_dir`,
//! so user-supplied path components cannot escape the workspace.
//!
//! The `agent` query parameter is required and selects which agent's
//! workspace the endpoint operates against. Each agent has its own
//! `<install>/agents/<alias>/workspace/` per the multi-agent layout.

use axum::{
    Json,
    extract::{Query, State},
    http::{HeaderMap, StatusCode},
    response::IntoResponse,
};
use serde::{Deserialize, Serialize};
use std::path::{Path, PathBuf};
use std::time::SystemTime;
use zeroclaw_runtime::agent::personality::{EDITABLE_PERSONALITY_FILES, MAX_FILE_CHARS};
use zeroclaw_runtime::agent::personality_templates::{TemplateContext, render_preset_default};
use zeroclaw_runtime::rpc::types::{
    PersonalityFileEntry, PersonalityGetResult, PersonalityListResult, PersonalityPutResult,
    PersonalityTemplatesResult, TemplateFileEntry,
};

use super::AppState;
use super::api::require_auth;

// ── HTTP-specific request/response shapes (not shared) ──────────────

#[derive(Debug, Deserialize, Default)]
pub struct AgentQuery {
    #[serde(default)]
    pub agent: Option<String>,
}

#[derive(Debug, Deserialize, Default)]
pub struct TemplateQuery {
    #[serde(default)]
    pub preset: Option<String>,
    #[serde(default)]
    pub agent_name: Option<String>,
    #[serde(default)]
    pub user_name: Option<String>,
    #[serde(default)]
    pub timezone: Option<String>,
    #[serde(default)]
    pub communication_style: Option<String>,
    #[serde(default)]
    pub include_memory: Option<bool>,
    #[serde(default)]
    pub agent: Option<String>,
}

#[derive(Debug, Deserialize)]
pub struct PersonalityPutBody {
    pub content: String,
    #[serde(default)]
    pub expected_mtime_ms: Option<i64>,
}

#[derive(Debug, Serialize)]
pub struct PersonalityConflict {
    pub error: &'static str,
    pub filename: String,
    pub current_content: String,
    pub current_mtime_ms: Option<i64>,
}

// ── Sandbox helpers ─────────────────────────────────────────────────

fn validate_filename(
    filename: &str,
) -> Result<&'static str, (StatusCode, Json<serde_json::Value>)> {
    EDITABLE_PERSONALITY_FILES
        .iter()
        .copied()
        .find(|allowed| *allowed == filename)
        .ok_or_else(|| {
            (
                StatusCode::BAD_REQUEST,
                Json(serde_json::json!({
                    "error": "filename not in personality allowlist",
                    "filename": filename,
                    "allowed": EDITABLE_PERSONALITY_FILES,
                })),
            )
        })
}

fn personality_path(workspace_dir: &Path, filename: &'static str) -> PathBuf {
    workspace_dir.join(filename)
}

/// Resolve the per-agent workspace directory for personality I/O. Returns
/// an error response when `agent` is missing or unknown so callers can
/// short-circuit before touching disk.
fn resolve_agent_workspace(
    state: &AppState,
    agent: Option<&str>,
) -> Result<PathBuf, (StatusCode, Json<serde_json::Value>)> {
    let Some(alias) = agent.map(str::trim).filter(|s| !s.is_empty()) else {
        return Err((
            StatusCode::BAD_REQUEST,
            Json(serde_json::json!({
                "error": "missing required `agent` query parameter",
            })),
        ));
    };
    let cfg = state.config.read();
    if !cfg.agents.contains_key(alias) {
        return Err((
            StatusCode::NOT_FOUND,
            Json(serde_json::json!({
                "error": "unknown agent alias",
                "agent": alias,
            })),
        ));
    }
    Ok(cfg.agent_workspace_dir(alias))
}

fn mtime_ms_of(meta: &std::fs::Metadata) -> Option<i64> {
    meta.modified()
        .ok()
        .and_then(|t| t.duration_since(SystemTime::UNIX_EPOCH).ok())
        .and_then(|d| i64::try_from(d.as_millis()).ok())
}

fn truncate_to_chars(content: &str, max: usize) -> (String, bool) {
    if content.chars().count() <= max {
        return (content.to_string(), false);
    }
    let cut = content
        .char_indices()
        .nth(max)
        .map(|(idx, _)| &content[..idx])
        .unwrap_or(content);
    (cut.to_string(), true)
}

// ── Handlers ────────────────────────────────────────────────────────

/// GET /api/personality?agent=<alias> — index of all allowlist files in the
/// named agent's workspace.
pub async fn handle_index(
    State(state): State<AppState>,
    headers: HeaderMap,
    Query(q): Query<AgentQuery>,
) -> impl IntoResponse {
    if let Err(e) = require_auth(&state, &headers) {
        return e.into_response();
    }

    let workspace_dir = match resolve_agent_workspace(&state, q.agent.as_deref()) {
        Ok(p) => p,
        Err(resp) => return resp.into_response(),
    };

    let files: Vec<PersonalityFileEntry> = EDITABLE_PERSONALITY_FILES
        .iter()
        .copied()
        .map(|filename| {
            let path = workspace_dir.join(filename);
            match std::fs::metadata(&path) {
                Ok(meta) => PersonalityFileEntry {
                    filename: filename.to_string(),
                    exists: meta.is_file(),
                    size: meta.len(),
                    mtime_ms: mtime_ms_of(&meta),
                },
                Err(_) => PersonalityFileEntry {
                    filename: filename.to_string(),
                    exists: false,
                    size: 0,
                    mtime_ms: None,
                },
            }
        })
        .collect();

    Json(PersonalityListResult {
        files,
        max_chars: MAX_FILE_CHARS,
    })
    .into_response()
}

/// GET /api/personality/{filename} — read one file's full content.
pub async fn handle_get(
    State(state): State<AppState>,
    headers: HeaderMap,
    axum::extract::Path(filename): axum::extract::Path<String>,
    Query(q): Query<AgentQuery>,
) -> impl IntoResponse {
    if let Err(e) = require_auth(&state, &headers) {
        return e.into_response();
    }

    let allowed = match validate_filename(&filename) {
        Ok(f) => f,
        Err(e) => return e.into_response(),
    };

    let workspace_dir = match resolve_agent_workspace(&state, q.agent.as_deref()) {
        Ok(p) => p,
        Err(resp) => return resp.into_response(),
    };
    let path = personality_path(&workspace_dir, allowed);

    match std::fs::read_to_string(&path) {
        Ok(raw) => {
            let (content, truncated) = truncate_to_chars(&raw, MAX_FILE_CHARS);
            let mtime_ms = std::fs::metadata(&path).ok().and_then(|m| mtime_ms_of(&m));
            Json(PersonalityGetResult {
                filename: allowed.to_string(),
                content: Some(content),
                exists: true,
                truncated,
                mtime_ms,
            })
            .into_response()
        }
        Err(err) if err.kind() == std::io::ErrorKind::NotFound => Json(PersonalityGetResult {
            filename: allowed.to_string(),
            content: Some(String::new()),
            exists: false,
            truncated: false,
            mtime_ms: None,
        })
        .into_response(),
        Err(err) => (
            StatusCode::INTERNAL_SERVER_ERROR,
            Json(serde_json::json!({
                "error": "failed to read personality file",
                "filename": allowed,
                "detail": err.to_string(),
            })),
        )
            .into_response(),
    }
}

/// PUT /api/personality/{filename} — overwrite the file.
pub async fn handle_put(
    State(state): State<AppState>,
    headers: HeaderMap,
    axum::extract::Path(filename): axum::extract::Path<String>,
    Query(q): Query<AgentQuery>,
    Json(body): Json<PersonalityPutBody>,
) -> impl IntoResponse {
    if let Err(e) = require_auth(&state, &headers) {
        return e.into_response();
    }

    let allowed = match validate_filename(&filename) {
        Ok(f) => f,
        Err(e) => return e.into_response(),
    };

    if body.content.chars().count() > MAX_FILE_CHARS {
        return (
            StatusCode::PAYLOAD_TOO_LARGE,
            Json(serde_json::json!({
                "error": "content exceeds MAX_FILE_CHARS",
                "max_chars": MAX_FILE_CHARS,
            })),
        )
            .into_response();
    }

    let workspace_dir = match resolve_agent_workspace(&state, q.agent.as_deref()) {
        Ok(p) => p,
        Err(resp) => return resp.into_response(),
    };
    let path = personality_path(&workspace_dir, allowed);

    // Disk-drift guard: if the editor told us what mtime it saw, reject
    // the write when disk has moved since.
    if let Some(expected) = body.expected_mtime_ms {
        let current = std::fs::metadata(&path).ok().and_then(|m| mtime_ms_of(&m));
        if current != Some(expected) {
            let current_content = std::fs::read_to_string(&path).unwrap_or_default();
            return (
                StatusCode::CONFLICT,
                Json(PersonalityConflict {
                    error: "personality_disk_drift",
                    filename: allowed.to_string(),
                    current_content,
                    current_mtime_ms: current,
                }),
            )
                .into_response();
        }
    }

    if let Some(parent) = path.parent()
        && let Err(err) = std::fs::create_dir_all(parent)
    {
        return (
            StatusCode::INTERNAL_SERVER_ERROR,
            Json(serde_json::json!({
                "error": "failed to create workspace dir",
                "detail": err.to_string(),
            })),
        )
            .into_response();
    }

    if let Err(err) = std::fs::write(&path, body.content.as_bytes()) {
        return (
            StatusCode::INTERNAL_SERVER_ERROR,
            Json(serde_json::json!({
                "error": "failed to write personality file",
                "filename": allowed,
                "detail": err.to_string(),
            })),
        )
            .into_response();
    }

    let meta = std::fs::metadata(&path).ok();
    let bytes_written = meta.as_ref().map(|m| m.len()).unwrap_or(0);
    let mtime_ms = meta.as_ref().and_then(mtime_ms_of);

    Json(PersonalityPutResult {
        bytes_written,
        mtime_ms,
    })
    .into_response()
}

/// GET /api/personality/templates — render the default starter set.
///
/// Reuses `TemplateContext::default()` for any field the caller didn't
/// override. The `memory.backend` config is consulted as a sensible
/// default for `include_memory` when the query parameter is absent, so
/// onboarding picks the right MEMORY.md behaviour without the user
/// having to repeat themselves.
pub async fn handle_templates(
    State(state): State<AppState>,
    headers: HeaderMap,
    Query(q): Query<TemplateQuery>,
) -> impl IntoResponse {
    if let Err(e) = require_auth(&state, &headers) {
        return e.into_response();
    }

    let (memory_default_enabled, agent_display_default) = {
        let cfg = state.config.read();
        let mem = cfg.memory.backend.as_str() != "none";
        let display = q
            .agent
            .as_deref()
            .map(str::to_string)
            .filter(|alias| cfg.agents.contains_key(alias));
        (mem, display)
    };

    let defaults = TemplateContext::default();
    let ctx = TemplateContext {
        agent: q
            .agent_name
            .or(agent_display_default)
            .unwrap_or(defaults.agent),
        user: q.user_name.unwrap_or(defaults.user),
        timezone: q.timezone.unwrap_or(defaults.timezone),
        communication_style: q
            .communication_style
            .unwrap_or(defaults.communication_style),
        include_memory: q.include_memory.unwrap_or(memory_default_enabled),
    };

    let files = render_preset_default(&ctx)
        .into_iter()
        .map(|(filename, content)| TemplateFileEntry {
            filename: filename.to_string(),
            content,
        })
        .collect();

    Json(PersonalityTemplatesResult {
        preset: "default".to_string(),
        files,
    })
    .into_response()
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn validate_filename_accepts_allowlist() {
        for f in EDITABLE_PERSONALITY_FILES {
            assert!(validate_filename(f).is_ok());
        }
    }

    #[test]
    fn validate_filename_rejects_traversal() {
        for bad in [
            "../etc/passwd",
            "IDENTITY.md/foo",
            "OTHER.md",
            "identity.md", // case-sensitive on purpose; matches runtime
            "",
        ] {
            assert!(validate_filename(bad).is_err());
        }
    }

    #[test]
    fn personality_path_joins_workspace_root() {
        let p = personality_path(Path::new("/tmp/ws"), "SOUL.md");
        assert_eq!(p, Path::new("/tmp/ws/SOUL.md"));
    }

    #[test]
    fn truncate_at_max_chars() {
        let s = "x".repeat(MAX_FILE_CHARS + 100);
        let (out, trunc) = truncate_to_chars(&s, MAX_FILE_CHARS);
        assert!(trunc);
        assert_eq!(out.chars().count(), MAX_FILE_CHARS);
    }

    #[test]
    fn no_truncation_when_under_limit() {
        let (out, trunc) = truncate_to_chars("hello", MAX_FILE_CHARS);
        assert!(!trunc);
        assert_eq!(out, "hello");
    }
}
