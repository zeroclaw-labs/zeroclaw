//! Read/write endpoints for the per-workspace personality markdown files
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
//! current `workspace_dir`, so user-supplied path components cannot
//! escape the workspace.
//!
//! The `agent` query parameter is reserved for #5890 (multi-agent
//! workspaces). It is accepted on every endpoint and ignored today;
//! when #5890 lands it will validate an alias and append a subdir.

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

use super::AppState;
use super::api::require_auth;

// ── Request / response shapes ───────────────────────────────────────

#[derive(Debug, Deserialize, Default)]
pub struct AgentQuery {
    /// Reserved for #5890. Accepted today, has no effect.
    #[serde(default)]
    pub agent: Option<String>,
}

#[derive(Debug, Deserialize, Default)]
pub struct TemplateQuery {
    /// Preset name. Only `default` is recognised today; unknown values
    /// fall through to the default preset rather than 400-ing.
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
    /// When `false`, MEMORY.md is omitted and AGENTS.md is rendered for
    /// a memory-disabled workspace.
    #[serde(default)]
    pub include_memory: Option<bool>,
    /// Reserved for #5890.
    #[serde(default)]
    pub agent: Option<String>,
}

#[derive(Debug, Serialize)]
pub struct TemplateFile {
    pub filename: &'static str,
    pub content: String,
}

#[derive(Debug, Serialize)]
pub struct TemplateResponse {
    pub preset: &'static str,
    pub files: Vec<TemplateFile>,
}

#[derive(Debug, Serialize)]
pub struct PersonalityIndexEntry {
    pub filename: &'static str,
    pub exists: bool,
    pub size: u64,
    pub mtime_ms: Option<i64>,
}

#[derive(Debug, Serialize)]
pub struct PersonalityIndex {
    pub files: Vec<PersonalityIndexEntry>,
    pub max_chars: usize,
}

#[derive(Debug, Serialize)]
pub struct PersonalityFileResponse {
    pub filename: String,
    pub content: String,
    pub exists: bool,
    pub truncated: bool,
    pub mtime_ms: Option<i64>,
}

#[derive(Debug, Deserialize)]
pub struct PersonalityPutBody {
    pub content: String,
    /// Last `mtime_ms` the editor saw via GET. When provided and the
    /// on-disk mtime differs, the server returns 409 with the current
    /// content + mtime so the editor can resolve the conflict.
    #[serde(default)]
    pub expected_mtime_ms: Option<i64>,
}

#[derive(Debug, Serialize)]
pub struct PersonalityPutResponse {
    pub bytes_written: u64,
    pub mtime_ms: Option<i64>,
}

#[derive(Debug, Serialize)]
pub struct PersonalityDeleteResponse {
    pub filename: String,
    /// `true` when a file was actually removed; `false` when the file did not
    /// exist (DELETE is idempotent — the BOOTSTRAP.md "delete after first
    /// conversation" use case may fire after the file was already removed).
    pub existed: bool,
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

fn personality_path(workspace_dir: &Path, _agent: Option<&str>, filename: &'static str) -> PathBuf {
    // `_agent` is reserved for #5890. Today every personality file
    // lives at the workspace root.
    workspace_dir.join(filename)
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

/// GET /api/personality — index of all allowlist files in the active workspace.
pub async fn handle_index(
    State(state): State<AppState>,
    headers: HeaderMap,
    Query(_q): Query<AgentQuery>,
) -> impl IntoResponse {
    if let Err(e) = require_auth(&state, &headers) {
        return e.into_response();
    }

    let workspace_dir = {
        let cfg = state.config.lock();
        cfg.workspace_dir.clone()
    };

    let files: Vec<PersonalityIndexEntry> = EDITABLE_PERSONALITY_FILES
        .iter()
        .copied()
        .map(|filename| {
            let path = workspace_dir.join(filename);
            match std::fs::metadata(&path) {
                Ok(meta) => PersonalityIndexEntry {
                    filename,
                    exists: meta.is_file(),
                    size: meta.len(),
                    mtime_ms: mtime_ms_of(&meta),
                },
                Err(_) => PersonalityIndexEntry {
                    filename,
                    exists: false,
                    size: 0,
                    mtime_ms: None,
                },
            }
        })
        .collect();

    Json(PersonalityIndex {
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

    let workspace_dir = {
        let cfg = state.config.lock();
        cfg.workspace_dir.clone()
    };
    let path = personality_path(&workspace_dir, q.agent.as_deref(), allowed);

    match std::fs::read_to_string(&path) {
        Ok(raw) => {
            let (content, truncated) = truncate_to_chars(&raw, MAX_FILE_CHARS);
            let mtime_ms = std::fs::metadata(&path).ok().and_then(|m| mtime_ms_of(&m));
            Json(PersonalityFileResponse {
                filename: allowed.to_string(),
                content,
                exists: true,
                truncated,
                mtime_ms,
            })
            .into_response()
        }
        Err(err) if err.kind() == std::io::ErrorKind::NotFound => Json(PersonalityFileResponse {
            filename: allowed.to_string(),
            content: String::new(),
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

    let workspace_dir = {
        let cfg = state.config.lock();
        cfg.workspace_dir.clone()
    };
    let path = personality_path(&workspace_dir, q.agent.as_deref(), allowed);

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

    Json(PersonalityPutResponse {
        bytes_written,
        mtime_ms,
    })
    .into_response()
}

/// DELETE /api/personality/{filename} — remove an allowlisted personality file.
///
/// Idempotent: deleting a file that does not exist returns 200 with
/// `existed: false` rather than 404. The primary motivating use case is the
/// dashboard's "first-run completion" flow for `BOOTSTRAP.md` (the file is
/// supposed to delete itself once the operator has finished onboarding;
/// today it persists silently because there's no UI surface to remove it),
/// but the route accepts any allowlisted filename so operators can reset
/// other personality files to runtime defaults when needed. The runtime
/// reload behaviour matches PUT — changes are picked up at the next
/// session boundary.
pub async fn handle_delete(
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

    let workspace_dir = {
        let cfg = state.config.lock();
        cfg.workspace_dir.clone()
    };
    let path = personality_path(&workspace_dir, q.agent.as_deref(), allowed);

    let existed = path.exists();
    if existed && let Err(err) = std::fs::remove_file(&path) {
        return (
            StatusCode::INTERNAL_SERVER_ERROR,
            Json(serde_json::json!({
                "error": "failed to delete personality file",
                "filename": allowed,
                "detail": err.to_string(),
            })),
        )
            .into_response();
    }

    Json(PersonalityDeleteResponse {
        filename: allowed.to_string(),
        existed,
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

    let memory_default_enabled = {
        let cfg = state.config.lock();
        cfg.memory.backend.as_str() != "none"
    };

    let defaults = TemplateContext::default();
    let ctx = TemplateContext {
        agent: q.agent_name.unwrap_or(defaults.agent),
        user: q.user_name.unwrap_or(defaults.user),
        timezone: q.timezone.unwrap_or(defaults.timezone),
        communication_style: q
            .communication_style
            .unwrap_or(defaults.communication_style),
        include_memory: q.include_memory.unwrap_or(memory_default_enabled),
    };

    let files = render_preset_default(&ctx)
        .into_iter()
        .map(|(filename, content)| TemplateFile { filename, content })
        .collect();

    Json(TemplateResponse {
        preset: "default",
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
        let p = personality_path(Path::new("/tmp/ws"), None, "SOUL.md");
        assert_eq!(p, Path::new("/tmp/ws/SOUL.md"));
    }

    #[test]
    fn personality_path_ignores_agent_for_now() {
        let with_agent = personality_path(Path::new("/tmp/ws"), Some("nova"), "SOUL.md");
        let without = personality_path(Path::new("/tmp/ws"), None, "SOUL.md");
        assert_eq!(with_agent, without);
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
