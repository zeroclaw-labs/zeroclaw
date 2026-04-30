//! Static file serving for the web dashboard.
//!
//! Serves the compiled `web/dist/` directory from the filesystem at runtime.
//! The directory path is configured via `gateway.web_dist_dir`.

use axum::{
    extract::State,
    http::{StatusCode, Uri, header},
    response::{IntoResponse, Response},
};
use std::path::PathBuf;

use super::AppState;

#[cfg(feature = "embedded-web")]
use include_dir::{Dir, include_dir};

#[cfg(feature = "embedded-web")]
static EMBEDDED_WEB_DIST: Dir<'_> = include_dir!("$CARGO_MANIFEST_DIR/../../web/dist");

/// Serve static files from `/_app/*` path
pub async fn handle_static(State(state): State<AppState>, uri: Uri) -> Response {
    let path = uri
        .path()
        .strip_prefix("/_app/")
        .unwrap_or(uri.path())
        .trim_start_matches('/');

    #[cfg(feature = "embedded-web")]
    if let Some(resp) = serve_embedded_file(path) {
        return resp;
    }

    serve_fs_file(state.web_dist_dir.as_ref(), path).await
}

/// SPA fallback: serve index.html for any non-API, non-static GET request.
/// Injects `window.__ZEROCLAW_BASE__` so the frontend knows the path prefix.
pub async fn handle_spa_fallback(State(state): State<AppState>) -> Response {
    let Some(bytes) = load_index_html_bytes(state.web_dist_dir.as_ref()).await else {
        return (
            StatusCode::SERVICE_UNAVAILABLE,
            "Web dashboard not available. Set gateway.web_dist_dir in your config \
             and build the frontend with: cd web && npm ci && npm run build",
        )
            .into_response();
    };

    let html = String::from_utf8_lossy(&bytes);

    // Inject path prefix for the SPA and rewrite asset paths in the HTML
    let html = if state.path_prefix.is_empty() {
        html.into_owned()
    } else {
        let pfx = &state.path_prefix;
        // JSON-encode the prefix to safely embed in a <script> block
        let json_pfx = serde_json::to_string(pfx).unwrap_or_else(|_| "\"\"".to_string());
        let script = format!("<script>window.__ZEROCLAW_BASE__={json_pfx};</script>");
        // Rewrite absolute /_app/ references so the browser requests {prefix}/_app/...
        html.replace("/_app/", &format!("{pfx}/_app/"))
            .replace("<head>", &format!("<head>{script}"))
    };

    (
        StatusCode::OK,
        [
            (header::CONTENT_TYPE, "text/html; charset=utf-8".to_string()),
            (header::CACHE_CONTROL, "no-cache".to_string()),
        ],
        html,
    )
        .into_response()
}

async fn load_index_html_bytes(dist_dir: Option<&PathBuf>) -> Option<Vec<u8>> {
    #[cfg(feature = "embedded-web")]
    if let Some(file) = EMBEDDED_WEB_DIST.get_file("index.html") {
        return Some(file.contents().to_vec());
    }

    let dir = dist_dir?;
    let index_path = dir.join("index.html");
    tokio::fs::read(&index_path).await.ok()
}

async fn serve_fs_file(dist_dir: Option<&PathBuf>, path: &str) -> Response {
    let Some(dir) = dist_dir else {
        return (StatusCode::NOT_FOUND, "Not found").into_response();
    };

    // Sanitize: reject path traversal attempts
    if path.contains("..") {
        return (StatusCode::BAD_REQUEST, "Invalid path").into_response();
    }

    let file_path = dir.join(path);

    match tokio::fs::read(&file_path).await {
        Ok(content) => {
            let mime = mime_guess::from_path(path)
                .first_or_octet_stream()
                .to_string();

            (
                StatusCode::OK,
                [
                    (header::CONTENT_TYPE, mime),
                    (
                        header::CACHE_CONTROL,
                        if path.contains("assets/") {
                            // Hashed filenames — immutable cache
                            "public, max-age=31536000, immutable".to_string()
                        } else {
                            // index.html etc — no cache
                            "no-cache".to_string()
                        },
                    ),
                ],
                content,
            )
                .into_response()
        }
        Err(_) => (StatusCode::NOT_FOUND, "Not found").into_response(),
    }
}

#[cfg(feature = "embedded-web")]
fn serve_embedded_file(path: &str) -> Option<Response> {
    if path.contains("..") {
        return Some((StatusCode::BAD_REQUEST, "Invalid path").into_response());
    }

    let file = EMBEDDED_WEB_DIST.get_file(path)?;
    let mime = mime_guess::from_path(path)
        .first_or_octet_stream()
        .to_string();
    let cache = if path.contains("assets/") {
        "public, max-age=31536000, immutable".to_string()
    } else {
        "no-cache".to_string()
    };

    Some(
        (
            StatusCode::OK,
            [(header::CONTENT_TYPE, mime), (header::CACHE_CONTROL, cache)],
            file.contents().to_vec(),
        )
            .into_response(),
    )
}
