//! Static file serving for the web dashboard.
//! Serves the compiled `web/dist/` directory from the filesystem at runtime.
//! The directory path is configured via `gateway.web_dist_dir`.

use axum::{
    Json,
    extract::State,
    http::{StatusCode, Uri, header},
    response::{IntoResponse, Response},
};
#[cfg(not(target_os = "macos"))]
use cap_std::ambient_authority;
use cap_std::fs::Dir as CapabilityDir;
use std::io::Read;
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

    read_fs_file(dist_dir?, Path::new("index.html")).await.ok()
}

async fn serve_fs_file(dist_dir: Option<&PathBuf>, path: &str) -> Response {
    if !is_valid_relative_path(path) {
        return (StatusCode::BAD_REQUEST, "Invalid path").into_response();
    }

    let Some(dir) = dist_dir else {
        return (StatusCode::NOT_FOUND, "Not found").into_response();
    };

    match read_fs_file(dir, Path::new(path)).await {
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
        Err(FsPathError::Invalid) => (StatusCode::BAD_REQUEST, "Invalid path").into_response(),
        Err(FsPathError::Unavailable) => (StatusCode::NOT_FOUND, "Not found").into_response(),
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

async fn read_fs_file(root: &Path, relative: &Path) -> Result<Vec<u8>, FsPathError> {
    let root = root.to_path_buf();
    let relative = relative.to_path_buf();
    tokio::task::spawn_blocking(move || {
        let mut file = open_fs_file(&root, &relative)?;
        let mut bytes = Vec::new();
        file.read_to_end(&mut bytes)
            .map_err(|_| FsPathError::Unavailable)?;
        Ok(bytes)
    })
    .await
    .map_err(|_| FsPathError::Unavailable)?
}

fn open_fs_file(root: &Path, relative: &Path) -> Result<cap_std::fs::File, FsPathError> {
    if relative.as_os_str().is_empty()
        || !relative
            .components()
            .all(|component| matches!(component, Component::Normal(_)))
    {
        return Err(FsPathError::Invalid);
    }

    // Bind authority before resolving names. Canonicalization preserves support
    // for contained absolute symlinks; only the handle-relative open enforces
    // confinement when entries change between resolution and opening.
    #[cfg(not(target_os = "macos"))]
    let directory = CapabilityDir::open_ambient_dir(root, ambient_authority())
        .map_err(|_| FsPathError::Unavailable)?;
    #[cfg(target_os = "macos")]
    let directory = {
        use std::os::unix::fs::OpenOptionsExt;
        let handle = std::fs::OpenOptions::new()
            .read(true)
            .custom_flags(libc::O_SEARCH)
            .open(root)
            .map_err(|_| FsPathError::Unavailable)?;
        CapabilityDir::from_std_file(handle)
    };
    let canonical_root = std::fs::canonicalize(root).map_err(|_| FsPathError::Unavailable)?;
    let canonical_file = std::fs::canonicalize(canonical_root.join(relative))
        .map_err(|_| FsPathError::Unavailable)?;
    let contained = canonical_file
        .strip_prefix(&canonical_root)
        .map_err(|_| FsPathError::Unavailable)?;

    #[cfg(test)]
    run_before_open_hook();

    let mut options = cap_std::fs::OpenOptions::new();
    options.read(true);
    // A regular file may become a FIFO before opening. Avoid blocking a worker
    // waiting for a pipe writer before the opened-handle type check rejects it.
    #[cfg(unix)]
    {
        use cap_std::fs::OpenOptionsExt;
        options.custom_flags(libc::O_NONBLOCK);
    }
    #[cfg(target_os = "macos")]
    let (directory, contained) = open_searchable_parent(directory, contained)?;
    let file = directory
        .open_with(contained, &options)
        .map_err(|_| FsPathError::Unavailable)?;
    if !file
        .metadata()
        .map_err(|_| FsPathError::Unavailable)?
        .is_file()
    {
        return Err(FsPathError::Unavailable);
    }
    Ok(file)
}

// macOS lacks O_PATH, so cap-std's normal directory traversal requires read
// permission. O_SEARCH retains the former search-only directory contract.
#[cfg(target_os = "macos")]
fn open_searchable_parent(
    mut directory: CapabilityDir,
    path: &Path,
) -> Result<(CapabilityDir, &Path), FsPathError> {
    use cap_std::fs::OpenOptionsExt;

    let mut options = cap_std::fs::OpenOptions::new();
    options.read(true).custom_flags(libc::O_SEARCH);
    let parent = path.parent().ok_or(FsPathError::Unavailable)?;
    for component in parent.components() {
        let Component::Normal(name) = component else {
            return Err(FsPathError::Invalid);
        };
        let handle = directory
            .open_with(name, &options)
            .map_err(|_| FsPathError::Unavailable)?;
        directory = CapabilityDir::from_std_file(handle.into_std());
    }
    let filename = path.file_name().ok_or(FsPathError::Unavailable)?;
    Ok((directory, Path::new(filename)))
}

#[cfg(test)]
thread_local! {
    static BEFORE_OPEN_HOOK: std::cell::RefCell<Option<Box<dyn FnOnce()>>> =
        const { std::cell::RefCell::new(None) };
}

#[cfg(test)]
fn run_before_open_hook() {
    let hook = BEFORE_OPEN_HOOK.with(|hook| hook.borrow_mut().take());
    if let Some(hook) = hook {
        hook();
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
    async fn fs_asset_rejects_fifo_without_waiting_for_writer() {
        use std::ffi::CString;
        use std::os::unix::ffi::OsStrExt;
        use std::os::unix::fs::OpenOptionsExt;
        use std::time::Duration;

        let root = tempfile::tempdir().unwrap();
        let fifo = root.path().join("pipe.js");
        let fifo_name = CString::new(fifo.as_os_str().as_bytes()).unwrap();
        // The path is NUL-terminated and remains valid for this syscall.
        assert_eq!(
            unsafe { libc::mkfifo(fifo_name.as_ptr(), 0o600) },
            0,
            "create FIFO: {}",
            std::io::Error::last_os_error()
        );
        let root_path = root.path().to_path_buf();
        let mut request =
            zeroclaw_spawn::spawn!(async move { serve_fs_file(Some(&root_path), "pipe.js").await });

        let result = tokio::time::timeout(Duration::from_secs(2), &mut request).await;
        if result.is_err() {
            // A timed-out spawn_blocking read survives cancellation. Keep a writer
            // open until it exits, so a missing O_NONBLOCK fails instead of hanging
            // the test runtime during shutdown.
            let writer = std::fs::OpenOptions::new()
                .read(true)
                .write(true)
                .custom_flags(libc::O_NONBLOCK)
                .open(&fifo)
                .unwrap();
            let drained = tokio::time::timeout(Duration::from_secs(5), &mut request).await;
            if drained.is_err() {
                // Retain a writer for a delayed open during runtime shutdown.
                std::mem::forget(writer);
            }
            drained
                .expect("FIFO request did not exit after opening a writer")
                .unwrap();
        }

        let response = result
            .expect("FIFO request waited for a writer")
            .expect("FIFO request task panicked");
        assert_eq!(response.status(), StatusCode::NOT_FOUND);
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
        symlink(root.path().join("app.js"), root.path().join("absolute.js")).unwrap();
        let root_path = root.path().to_path_buf();

        for path in ["alias.js", "absolute.js"] {
            let response = serve_fs_file(Some(&root_path), path).await;
            assert_eq!(response.status(), StatusCode::OK);
            assert_eq!(response_body(response).await, b"inside-asset".to_vec());
        }
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

    #[cfg(unix)]
    #[test]
    fn fs_open_rejects_entry_replacement_after_resolution() {
        use std::os::unix::fs::symlink;

        for replace_parent in [false, true] {
            let root = tempfile::tempdir().unwrap();
            let outside = tempfile::tempdir().unwrap();
            std::fs::create_dir(root.path().join("assets")).unwrap();
            std::fs::write(root.path().join("assets/app.js"), b"inside-asset").unwrap();
            std::fs::write(outside.path().join("app.js"), b"outside-secret").unwrap();
            let root_path = root.path().to_path_buf();
            let outside_path = outside.path().to_path_buf();
            BEFORE_OPEN_HOOK.with(|hook| {
                *hook.borrow_mut() = Some(Box::new(move || {
                    if replace_parent {
                        std::fs::rename(root_path.join("assets"), root_path.join("retired"))
                            .unwrap();
                        symlink(outside_path, root_path.join("assets")).unwrap();
                    } else {
                        std::fs::remove_file(root_path.join("assets/app.js")).unwrap();
                        symlink(outside_path.join("app.js"), root_path.join("assets/app.js"))
                            .unwrap();
                    }
                }));
            });

            assert_eq!(
                open_fs_file(root.path(), Path::new("assets/app.js")).unwrap_err(),
                FsPathError::Unavailable,
                "replacement must not escape the opened root; parent={replace_parent}"
            );
        }
    }

    #[test]
    fn fs_open_rejects_non_file_replacement_after_resolution() {
        let root = tempfile::tempdir().unwrap();
        std::fs::write(root.path().join("app.js"), b"inside-asset").unwrap();
        let path = root.path().join("app.js");
        BEFORE_OPEN_HOOK.with(|hook| {
            *hook.borrow_mut() = Some(Box::new(move || {
                std::fs::remove_file(&path).unwrap();
                std::fs::create_dir(path).unwrap();
            }));
        });

        assert_eq!(
            open_fs_file(root.path(), Path::new("app.js")).unwrap_err(),
            FsPathError::Unavailable
        );
    }

    #[test]
    fn fs_open_keeps_file_identity_after_path_replacement() {
        let root = tempfile::tempdir().unwrap();
        let path = root.path().join("app.js");
        std::fs::write(&path, b"opened-asset").unwrap();
        let mut file = open_fs_file(root.path(), Path::new("app.js")).unwrap();
        std::fs::rename(&path, root.path().join("retired.js")).unwrap();
        std::fs::write(&path, b"replacement-asset").unwrap();
        let mut bytes = Vec::new();
        file.read_to_end(&mut bytes).unwrap();

        assert_eq!(bytes, b"opened-asset");
    }

    #[cfg(unix)]
    #[test]
    fn fs_open_keeps_root_identity_after_directory_replacement() {
        use std::os::unix::fs::symlink;

        let parent = tempfile::tempdir().unwrap();
        let root = parent.path().join("dist");
        std::fs::create_dir(&root).unwrap();
        std::fs::write(root.join("app.js"), b"opened-root-asset").unwrap();
        let outside = tempfile::tempdir().unwrap();
        std::fs::write(outside.path().join("app.js"), b"outside-secret").unwrap();
        let original_root = root.clone();
        let retired = parent.path().join("retired");
        let outside_path = outside.path().to_path_buf();
        BEFORE_OPEN_HOOK.with(|hook| {
            *hook.borrow_mut() = Some(Box::new(move || {
                std::fs::rename(&original_root, retired).unwrap();
                symlink(outside_path, original_root).unwrap();
            }));
        });

        let mut file = open_fs_file(&root, Path::new("app.js")).unwrap();
        let mut bytes = Vec::new();
        file.read_to_end(&mut bytes).unwrap();
        assert_eq!(bytes, b"opened-root-asset");
    }

    #[cfg(target_os = "macos")]
    #[tokio::test]
    async fn fs_asset_allows_search_only_directories() {
        use std::os::unix::fs::PermissionsExt;

        let root = tempfile::tempdir().unwrap();
        let assets = root.path().join("assets");
        std::fs::create_dir(&assets).unwrap();
        std::fs::write(assets.join("app.js"), b"search-only-asset").unwrap();
        std::fs::write(root.path().join("index.html"), b"search-only-index").unwrap();
        std::fs::set_permissions(&assets, std::fs::Permissions::from_mode(0o111)).unwrap();
        std::fs::set_permissions(root.path(), std::fs::Permissions::from_mode(0o111)).unwrap();
        let root_path = root.path().to_path_buf();

        let response = serve_fs_file(Some(&root_path), "assets/app.js").await;
        let index = read_fs_file(&root_path, Path::new("index.html")).await;

        std::fs::set_permissions(root.path(), std::fs::Permissions::from_mode(0o755)).unwrap();
        std::fs::set_permissions(&assets, std::fs::Permissions::from_mode(0o755)).unwrap();
        assert_eq!(response.status(), StatusCode::OK);
        assert_eq!(response_body(response).await, b"search-only-asset");
        assert_eq!(index.unwrap(), b"search-only-index");
    }
}
