//! `canvas/{list,get,history,render,clear}`: Live Canvas (A2UI) content.
//!
//! The daemon owns one [`CanvasStore`]; the canvas tool of every agent the core
//! builds writes to it, and these read and write it. The gateway's
//! `/api/canvas` routes serialize the same bodies and map the same failures to
//! HTTP statuses, so the two surfaces cannot drift.

use serde_json::Value;
use zeroclaw_api::jsonrpc::{JsonRpcError, error_codes};

use crate::tools::{ALLOWED_CONTENT_TYPES, CanvasStore, MAX_CONTENT_SIZE};

/// Why a canvas operation was refused. The message is identical on both
/// surfaces; [`CanvasFailure::http_status`] and [`CanvasFailure::rpc_code`] give each surface its
/// code.
#[derive(Debug, Clone, PartialEq, Eq)]
pub enum CanvasFailure {
    NotFound(String),
    InvalidContentType(String),
    TooLarge,
    CapacityReached,
}

impl CanvasFailure {
    #[must_use]
    pub fn message(&self) -> String {
        match self {
            Self::NotFound(id) => format!("Canvas '{id}' not found"),
            Self::InvalidContentType(content_type) => {
                format!("Invalid content_type '{content_type}'. Allowed: {ALLOWED_CONTENT_TYPES:?}")
            }
            Self::TooLarge => {
                format!("Content exceeds maximum size of {MAX_CONTENT_SIZE} bytes")
            }
            Self::CapacityReached => {
                "Maximum canvas count reached. Clear unused canvases first.".to_string()
            }
        }
    }

    /// The HTTP status the dashboard route answers with.
    #[must_use]
    pub fn http_status(&self) -> u16 {
        match self {
            Self::NotFound(_) => 404,
            Self::InvalidContentType(_) => 400,
            Self::TooLarge => 413,
            Self::CapacityReached => 429,
        }
    }

    #[must_use]
    pub fn rpc_code(&self) -> i32 {
        match self {
            Self::NotFound(_) | Self::InvalidContentType(_) | Self::TooLarge => {
                error_codes::INVALID_PARAMS
            }
            Self::CapacityReached => error_codes::INVALID_REQUEST,
        }
    }
}

impl From<CanvasFailure> for JsonRpcError {
    fn from(failure: CanvasFailure) -> Self {
        JsonRpcError {
            code: failure.rpc_code(),
            message: failure.message(),
            data: None,
        }
    }
}

/// `canvas/list`: every canvas id that currently has content.
#[must_use]
pub fn list_body(store: &CanvasStore) -> Value {
    serde_json::json!({ "canvases": store.list() })
}

/// `canvas/get`: the current frame of one canvas.
pub fn get_body(store: &CanvasStore, canvas_id: &str) -> Result<Value, CanvasFailure> {
    match store.snapshot(canvas_id) {
        Some(frame) => Ok(serde_json::json!({ "canvas_id": canvas_id, "frame": frame })),
        None => Err(CanvasFailure::NotFound(canvas_id.to_string())),
    }
}

/// `canvas/history`: the frame history of one canvas, empty when it has none.
#[must_use]
pub fn history_body(store: &CanvasStore, canvas_id: &str) -> Value {
    serde_json::json!({ "canvas_id": canvas_id, "frames": store.history(canvas_id) })
}

/// `canvas/render`: push content to a canvas. The content type defaults to
/// `html` and must be one the canvas tool accepts, which keeps an `eval` frame
/// from being injected this way; the size cap is the tool's.
pub fn render_body(
    store: &CanvasStore,
    canvas_id: &str,
    content_type: Option<&str>,
    content: &str,
) -> Result<Value, CanvasFailure> {
    let content_type = content_type.unwrap_or("html");
    if !ALLOWED_CONTENT_TYPES.contains(&content_type) {
        return Err(CanvasFailure::InvalidContentType(content_type.to_string()));
    }
    if content.len() > MAX_CONTENT_SIZE {
        return Err(CanvasFailure::TooLarge);
    }
    match store.render(canvas_id, content_type, content) {
        Some(frame) => Ok(serde_json::json!({ "canvas_id": canvas_id, "frame": frame })),
        None => Err(CanvasFailure::CapacityReached),
    }
}

/// `canvas/clear`: remove a canvas's content and history.
#[must_use]
pub fn clear_body(store: &CanvasStore, canvas_id: &str) -> Value {
    store.clear(canvas_id);
    serde_json::json!({ "canvas_id": canvas_id, "status": "cleared" })
}
