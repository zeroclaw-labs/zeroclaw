//! Static file serving for the web dashboard.
//! Serves the compiled `web/dist/` directory from the filesystem at runtime.
//! The directory path is configured via `gateway.web_dist_dir`.

use axum::{
    Json,
    extract::State,
    http::{StatusCode, Uri, header},
    response::{IntoResponse, Response},
};
use std::path::{Component, Path, PathBuf};

use super::AppState;

#[cfg(feature = "embedded-web")]
use include_dir::{Dir, include_dir};

#[cfg(feature = "embedded-web")]
static EMBEDDED_WEB_DIST: Dir<'_> = include_dir!("$CARGO_MANIFEST_DIR/../../web/dist");

/// Serve static files from `/_app/*` path
pub async fn handle_static(State(state): State<AppState>, uri: Uri) -> Response {
    let Some(path) = static_request_path(&uri) else {
        return (StatusCode::BAD_REQUEST, "Invalid path").into_response();
    };

    #[cfg(feature = "embedded-web")]
    if let Some(resp) = serve_embedded_file(path) {
        return resp;
    }

    serve_fs_file(state.web_dist_dir.as_ref(), path).await
}

/// SPA fallback: serve index.html for any non-API, non-static GET request.
/// Injects `window.__ZEROCLAW_BASE__` so the frontend knows the path prefix.
pub async fn handle_spa_fallback(State(state): State<AppState>, uri: Uri) -> Response {
    if let Some(path) = api_fallback_path(uri.path(), &state.path_prefix) {
        let body = serde_json::json!({
            "error": "not_found",
            "message": "No backend route matched this path.",
            "path": path,
        });
        return (StatusCode::NOT_FOUND, Json(body)).into_response();
    }

    let Some(bytes) = load_index_html_bytes(state.web_dist_dir.as_ref()).await else {
        return (
            StatusCode::SERVICE_UNAVAILABLE,
            "Web dashboard not available. Reinstall with the supported installer \
             so the dashboard is built and placed where the gateway looks for it: \
             `./install.sh --source` on Linux/macOS, or `setup.bat` on Windows. \
             The daemon's API endpoints remain reachable independently of the \
             dashboard.",
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

fn api_fallback_path<'a>(path: &'a str, path_prefix: &str) -> Option<&'a str> {
    let path = strip_path_prefix(path, path_prefix);
    (path == "/api" || path.strip_prefix("/api/").is_some()).then_some(path)
}

fn strip_path_prefix<'a>(path: &'a str, path_prefix: &str) -> &'a str {
    if path_prefix.is_empty() || path_prefix == "/" {
        return path;
    }

    if path == path_prefix {
        return "/";
    }

    path.strip_prefix(path_prefix)
        .filter(|rest| rest.starts_with('/'))
        .unwrap_or(path)
}

async fn load_index_html_bytes(dist_dir: Option<&PathBuf>) -> Option<Vec<u8>> {
    #[cfg(feature = "embedded-web")]
    if let Some(file) = EMBEDDED_WEB_DIST.get_file("index.html") {
        return Some(file.contents().to_vec());
    }

    let index_path = resolve_fs_file(dist_dir?, Path::new("index.html"))
        .await
        .ok()?;
    tokio::fs::read(&index_path).await.ok()
}

async fn serve_fs_file(dist_dir: Option<&PathBuf>, path: &str) -> Response {
    if !is_valid_relative_path(path) {
        return (StatusCode::BAD_REQUEST, "Invalid path").into_response();
    }

    let Some(dir) = dist_dir else {
        return (StatusCode::NOT_FOUND, "Not found").into_response();
    };

    let file_path = match resolve_fs_file(dir, Path::new(path)).await {
        Ok(path) => path,
        Err(FsPathError::Invalid) => {
            return (StatusCode::BAD_REQUEST, "Invalid path").into_response();
        }
        Err(FsPathError::Unavailable) => {
            return (StatusCode::NOT_FOUND, "Not found").into_response();
        }
    };

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

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
enum FsPathError {
    Invalid,
    Unavailable,
}

fn static_request_path(uri: &Uri) -> Option<&str> {
    let path = uri.path().strip_prefix("/_app/").unwrap_or(uri.path());

    is_valid_relative_path(path).then_some(path)
}

fn is_valid_relative_path(path: &str) -> bool {
    !path.is_empty()
        && !path.starts_with('/')
        && !path.ends_with('/')
        && !path.contains('\\')
        && path
            .split('/')
            .all(|component| !matches!(component, "" | "." | ".."))
        && Path::new(path)
            .components()
            .all(|component| matches!(component, Component::Normal(_)))
}

async fn resolve_fs_file(root: &Path, relative: &Path) -> Result<PathBuf, FsPathError> {
    if relative.as_os_str().is_empty()
        || !relative
            .components()
            .all(|component| matches!(component, Component::Normal(_)))
    {
        return Err(FsPathError::Invalid);
    }

    let canonical_root = tokio::fs::canonicalize(root)
        .await
        .map_err(|_| FsPathError::Unavailable)?;
    let canonical_file = tokio::fs::canonicalize(canonical_root.join(relative))
        .await
        .map_err(|_| FsPathError::Unavailable)?;

    if !canonical_file.starts_with(&canonical_root) {
        return Err(FsPathError::Unavailable);
    }

    let metadata = tokio::fs::metadata(&canonical_file)
        .await
        .map_err(|_| FsPathError::Unavailable)?;
    if !metadata.is_file() {
        return Err(FsPathError::Unavailable);
    }

    Ok(canonical_file)
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

#[cfg(test)]
mod tests {
    use super::*;
    use axum::body::to_bytes;

    async fn response_body(response: Response) -> Vec<u8> {
        to_bytes(response.into_body(), usize::MAX)
            .await
            .expect("response body")
            .to_vec()
    }

    #[test]
    fn static_route_rejects_malformed_path_syntax() {
        for path in [
            "/_app//index.html",
            "/_app/assets//app.js",
            "/_app/assets/./app.js",
            "/_app/assets/app.js/",
        ] {
            let uri: Uri = path.parse().unwrap();
            assert_eq!(
                static_request_path(&uri),
                None,
                "route path should be rejected: {path}"
            );
        }
    }

    #[tokio::test]
    async fn fs_asset_serves_contained_nested_file() {
        let tmp = tempfile::tempdir().unwrap();
        let assets = tmp.path().join("assets");
        std::fs::create_dir(&assets).unwrap();
        std::fs::write(assets.join("app.js"), b"console.log('ok');").unwrap();
        let root = tmp.path().to_path_buf();

        let response = serve_fs_file(Some(&root), "assets/app.js").await;

        assert_eq!(response.status(), StatusCode::OK);
        assert_eq!(
            response_body(response).await,
            b"console.log('ok');".to_vec()
        );
    }

    #[tokio::test]
    async fn fs_asset_rejects_invalid_components_before_lookup() {
        let tmp = tempfile::tempdir().unwrap();
        let root = tmp.path().to_path_buf();

        for path in [
            "../secret",
            "assets/../secret",
            "./index.html",
            "/etc/passwd",
            "assets//app.js",
            "assets/./app.js",
            "assets/app.js/",
            r"assets\\app.js",
            r"assets\.\app.js",
            r"\assets\app.js",
            r"assets\app.js\",
        ] {
            let response = serve_fs_file(Some(&root), path).await;
            assert_eq!(
                response.status(),
                StatusCode::BAD_REQUEST,
                "path should be rejected: {path}"
            );
        }
    }

    #[cfg(windows)]
    #[tokio::test]
    async fn fs_asset_rejects_windows_prefix() {
        let tmp = tempfile::tempdir().unwrap();
        let root = tmp.path().to_path_buf();

        let response = serve_fs_file(Some(&root), r"C:\\Windows\\win.ini").await;

        assert_eq!(response.status(), StatusCode::BAD_REQUEST);
    }

    #[tokio::test]
    async fn fs_asset_returns_not_found_for_missing_or_non_file_targets() {
        let tmp = tempfile::tempdir().unwrap();
        std::fs::create_dir(tmp.path().join("assets")).unwrap();
        let root = tmp.path().to_path_buf();

        for path in ["missing.js", "assets"] {
            let response = serve_fs_file(Some(&root), path).await;
            assert_eq!(
                response.status(),
                StatusCode::NOT_FOUND,
                "target should not be served: {path}"
            );
        }
    }

    #[cfg(unix)]
    #[tokio::test]
    async fn fs_asset_rejects_symlink_that_resolves_outside_root() {
        use std::os::unix::fs::symlink;

        let root = tempfile::tempdir().unwrap();
        let outside = tempfile::tempdir().unwrap();
        std::fs::write(outside.path().join("secret.txt"), b"outside-secret").unwrap();
        symlink(
            outside.path().join("secret.txt"),
            root.path().join("escape.txt"),
        )
        .unwrap();
        let root_path = root.path().to_path_buf();

        let response = serve_fs_file(Some(&root_path), "escape.txt").await;

        assert_eq!(response.status(), StatusCode::NOT_FOUND);
        assert_ne!(response_body(response).await, b"outside-secret".to_vec());
    }

    #[cfg(unix)]
    #[tokio::test]
    async fn fs_asset_allows_symlink_that_resolves_inside_root() {
        use std::os::unix::fs::symlink;

        let root = tempfile::tempdir().unwrap();
        std::fs::write(root.path().join("app.js"), b"inside-asset").unwrap();
        symlink("app.js", root.path().join("alias.js")).unwrap();
        let root_path = root.path().to_path_buf();

        let response = serve_fs_file(Some(&root_path), "alias.js").await;

        assert_eq!(response.status(), StatusCode::OK);
        assert_eq!(response_body(response).await, b"inside-asset".to_vec());
    }

    #[cfg(unix)]
    #[tokio::test]
    async fn spa_index_rejects_symlink_that_resolves_outside_root() {
        use std::os::unix::fs::symlink;

        let root = tempfile::tempdir().unwrap();
        let outside = tempfile::tempdir().unwrap();
        std::fs::write(outside.path().join("index.html"), b"outside-shell").unwrap();
        symlink(
            outside.path().join("index.html"),
            root.path().join("index.html"),
        )
        .unwrap();
        let root_path = root.path().to_path_buf();

        assert!(load_index_html_bytes(Some(&root_path)).await.is_none());
    }
}
