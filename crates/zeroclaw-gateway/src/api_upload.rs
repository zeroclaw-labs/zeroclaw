//! `POST /api/upload?agent=<alias>&filename=<name>` — web dashboard file upload.
//!
//! Accepts raw file bytes in the request body and mirrors the RPC
//! `file/attach` semantics (`zeroclaw_runtime::rpc::attachments`): any file is
//! accepted, persisted through the shared hardened content-addressed writer
//! into `<agent workspace>/uploads/`, and answered with the marker the
//! dashboard embeds in the next chat message — `[IMAGE:<path>]` when the
//! multimodal loader will actually accept the payload, `[Document: <name>]
//! <path>` otherwise (the agent reads those with its file tools).
//!
//! The image/document decision defers to the canonical provider-loadable
//! contract ([`zeroclaw_api::media::provider_loadable_image_mime_for`]) — the
//! declared Content-Type and client filename can never widen acceptance, so
//! this endpoint can never promise the provider an image it would drop.
//! Filenames are display-only: the on-disk name is the content hash.

use axum::{
    Json,
    body::Bytes,
    extract::{Query, State},
    http::{HeaderMap, StatusCode},
};
use serde::{Deserialize, Serialize};
use zeroclaw_api::media::provider_loadable_image_mime_for;

use super::AppState;
use super::api::require_auth;

/// Per-file cap for non-image files, matching the RPC `file/attach`
/// per-file limit (`zeroclaw_runtime::rpc::attachments::MAX_FILE_BYTES`).
pub const MAX_DOCUMENT_BYTES: usize = 10 * 1024 * 1024;

/// Hard ceiling for the route's body limit: the larger of the document cap
/// and the upper clamp of `MultimodalConfig::effective_limits` (20 MB).
/// The applicable per-kind limit is enforced inside the handler.
pub const UPLOAD_BODY_CEILING_BYTES: usize = 20 * 1024 * 1024;

#[derive(Deserialize)]
pub struct UploadQuery {
    /// Configured agent alias whose workspace receives the file. Required —
    /// mirrors the WebSocket chat contract (`/ws/chat?agent=`): no default
    /// agent exists.
    pub agent: Option<String>,
    /// Original filename, used for the `[Document: <name>]` marker and the
    /// extension of the stored file. Display-only — path separators and NULs
    /// are replaced and the on-disk name is the content hash.
    pub filename: Option<String>,
}

#[derive(Serialize)]
pub struct UploadResponse {
    /// Absolute path of the saved file inside the agent workspace.
    pub path: String,
    /// `[IMAGE:<path>]` or `[Document: <name>] <path>` marker ready to embed
    /// in a chat message.
    pub marker: String,
}

/// What an accepted upload is, for marker purposes.
#[derive(Debug, PartialEq, Eq)]
pub enum UploadKind {
    /// The multimodal loader will accept these bytes under this name.
    Image,
    /// Everything else — referenced by a `[Document: …]` marker.
    Document,
}

/// Why an upload payload was refused. Carries everything the HTTP layer needs
/// so the decision itself stays pure and unit-testable.
#[derive(Debug, PartialEq, Eq)]
pub enum UploadReject {
    Empty,
    TooLarge { limit_bytes: usize },
}

/// The whole acceptance decision: what kind of upload the payload is, or why
/// it is refused. Pure. The image decision consults the canonical
/// provider-loadable contract (filename extension, then magic bytes), so a
/// client-declared type can never widen acceptance. Images are capped by the
/// live multimodal config (`image_max_bytes`) so nothing is stored that the
/// loader would then drop; other files use the RPC per-file cap.
pub fn classify_upload(
    file_name: &str,
    data: &[u8],
    image_max_bytes: usize,
) -> Result<UploadKind, UploadReject> {
    if data.is_empty() {
        return Err(UploadReject::Empty);
    }
    if provider_loadable_image_mime_for(file_name, data).is_some() {
        if data.len() > image_max_bytes {
            return Err(UploadReject::TooLarge {
                limit_bytes: image_max_bytes,
            });
        }
        Ok(UploadKind::Image)
    } else {
        if data.len() > MAX_DOCUMENT_BYTES {
            return Err(UploadReject::TooLarge {
                limit_bytes: MAX_DOCUMENT_BYTES,
            });
        }
        Ok(UploadKind::Document)
    }
}

/// Sanitize a client filename for display: strip path separators and NULs.
/// Mirrors the RPC attachment rule; an empty result falls back to "upload".
pub fn sanitize_filename(name: &str) -> String {
    let cleaned = name.replace(['/', '\\', '\0'], "_");
    if cleaned.is_empty() {
        "upload".to_string()
    } else {
        cleaned
    }
}

pub async fn handle_upload(
    State(state): State<AppState>,
    Query(query): Query<UploadQuery>,
    headers: HeaderMap,
    body: Bytes,
) -> Result<Json<UploadResponse>, (StatusCode, Json<serde_json::Value>)> {
    require_auth(&state, &headers)?;

    let Some(agent_alias) = query
        .agent
        .as_deref()
        .map(str::trim)
        .filter(|alias| !alias.is_empty())
    else {
        return Err(err(
            StatusCode::BAD_REQUEST,
            "missing_agent",
            "Missing required `agent` query parameter — pass `?agent=<alias>` \
             matching a configured [agents.<alias>] entry.",
        ));
    };

    // Resolve workspace and image size limit from live config at use time.
    let (workspace, image_max_bytes) = {
        let cfg = state.config.read();
        if cfg.agent(agent_alias).is_none() {
            return Err(err(
                StatusCode::NOT_FOUND,
                "unknown_agent",
                &format!(
                    "Unknown agent `{agent_alias}` — no [agents.{agent_alias}] entry configured."
                ),
            ));
        }
        let (_, max_image_size_mb) = cfg.multimodal.effective_limits();
        (
            cfg.agent_workspace_dir(agent_alias),
            max_image_size_mb.saturating_mul(1024 * 1024),
        )
    };

    let file_name = sanitize_filename(query.filename.as_deref().unwrap_or("upload").trim());

    let kind =
        classify_upload(&file_name, &body, image_max_bytes).map_err(|reject| match reject {
            UploadReject::Empty => {
                err(StatusCode::BAD_REQUEST, "empty_body", "Empty request body.")
            }
            UploadReject::TooLarge { limit_bytes } => err(
                StatusCode::PAYLOAD_TOO_LARGE,
                "payload_too_large",
                &format!("File exceeds the {limit_bytes}-byte limit for its kind."),
            ),
        })?;

    // Persist through the shared hardened content-addressed writer — the same
    // filesystem owner the RPC attachment and ACP/MCP blob paths use: the
    // on-disk name is the content hash (never the client filename), the write
    // is directory-handle-bound and no-follow, and identical bytes dedup to
    // one file.
    let ext = std::path::Path::new(&file_name)
        .extension()
        .map(|e| e.to_string_lossy().to_string())
        .unwrap_or_default();
    let dest = zeroclaw_tools::embedded_resource::persist_content_addressed(
        &workspace, &body, &ext,
    )
    .map_err(|e| {
        ::zeroclaw_log::record!(
            WARN,
            ::zeroclaw_log::Event::new(module_path!(), ::zeroclaw_log::Action::Note)
                .with_outcome(::zeroclaw_log::EventOutcome::Unknown)
                .with_attrs(::serde_json::json!({"error": zeroclaw_runtime::security::scrub(&format!("{e}"))})),
            "Failed to save webui upload"
        );
        err(
            StatusCode::INTERNAL_SERVER_ERROR,
            "upload_io_error",
            "Failed to save upload.",
        )
    })?;

    let path = dest.display().to_string();
    let marker = match kind {
        UploadKind::Image => format!("[IMAGE:{path}]"),
        UploadKind::Document => format!("[Document: {file_name}] {path}"),
    };
    Ok(Json(UploadResponse { path, marker }))
}

/// `{code, message}` mirrors the structured error envelope the dashboard's
/// central `apiFetch` already parses into a typed `ApiError`.
fn err(status: StatusCode, code: &str, message: &str) -> (StatusCode, Json<serde_json::Value>) {
    (
        status,
        Json(serde_json::json!({ "code": code, "message": message })),
    )
}

#[cfg(test)]
mod tests {
    use super::*;

    const PNG: &[u8] = &[0x89, b'P', b'N', b'G', b'\r', b'\n', 0x1A, b'\n', 0, 0];
    const JPEG: &[u8] = &[0xFF, 0xD8, 0xFF, 0xE0, 0, 0];
    const IMG_MAX: usize = 1024;

    #[test]
    fn provider_loadable_payloads_classify_as_images() {
        assert_eq!(
            classify_upload("a.png", PNG, IMG_MAX),
            Ok(UploadKind::Image)
        );
        assert_eq!(
            classify_upload("b.jpg", JPEG, IMG_MAX),
            Ok(UploadKind::Image)
        );
        // No useful name: magic bytes alone decide.
        assert_eq!(
            classify_upload("upload", PNG, IMG_MAX),
            Ok(UploadKind::Image)
        );
    }

    #[test]
    fn non_loadable_payloads_classify_as_documents() {
        // SVG is script-bearing markup the provider loader refuses.
        assert_eq!(
            classify_upload(
                "logo.svg",
                b"<svg xmlns='http://www.w3.org/2000/svg'/>",
                IMG_MAX
            ),
            Ok(UploadKind::Document)
        );
        assert_eq!(
            classify_upload("notes.txt", b"plain text", IMG_MAX),
            Ok(UploadKind::Document)
        );
        assert_eq!(
            classify_upload("report.pdf", b"%PDF-1.7 ...", IMG_MAX),
            Ok(UploadKind::Document)
        );
    }

    #[test]
    fn rejects_empty_body() {
        assert_eq!(
            classify_upload("a.png", &[], IMG_MAX),
            Err(UploadReject::Empty)
        );
    }

    #[test]
    fn images_are_capped_by_the_config_limit() {
        assert_eq!(
            classify_upload("a.png", PNG, PNG.len() - 1),
            Err(UploadReject::TooLarge {
                limit_bytes: PNG.len() - 1
            })
        );
        // Exactly at the limit is allowed.
        assert_eq!(
            classify_upload("a.png", PNG, PNG.len()),
            Ok(UploadKind::Image)
        );
    }

    #[test]
    fn documents_are_capped_by_the_rpc_per_file_limit() {
        let big = vec![b'x'; MAX_DOCUMENT_BYTES + 1];
        assert_eq!(
            classify_upload("big.bin", &big, IMG_MAX),
            Err(UploadReject::TooLarge {
                limit_bytes: MAX_DOCUMENT_BYTES
            })
        );
    }

    #[test]
    fn sanitize_strips_separators_and_defaults() {
        assert_eq!(sanitize_filename("normal.txt"), "normal.txt");
        assert_eq!(sanitize_filename("path/to/file.txt"), "path_to_file.txt");
        assert_eq!(sanitize_filename("c:\\evil\\name"), "c:_evil_name");
        assert_eq!(sanitize_filename(""), "upload");
    }
}
