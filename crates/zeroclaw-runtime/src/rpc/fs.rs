//! Filesystem RPC methods for remote directory browsing (WSS ACP CWD picker).
//!
//! These methods are only available to authenticated WSS sessions and are
//! subject to daemon-side path policy.

use std::path::Path;
use zeroclaw_api::jsonrpc::error_codes::*;
use zeroclaw_api::jsonrpc::{FsEntry, FsListDirRequest, FsListDirResponse};

/// Handle `fs/list_dir`.
pub async fn handle_fs_list_dir(
    params: &serde_json::Value,
) -> Result<serde_json::Value, zeroclaw_api::jsonrpc::JsonRpcError> {
    let req: FsListDirRequest = serde_json::from_value(params.clone())
        .map_err(|e| rpc_err(INVALID_PARAMS, e.to_string()))?;

    let path = Path::new(&req.path);

    // Basic traversal guard (more sophisticated policy can be added later)
    if path.components().any(|c| c.as_os_str() == "..") {
        return Err(rpc_err(FS_INVALID_PATH, "Path traversal not allowed"));
    }

    if !path.is_dir() {
        return Err(rpc_err(
            FS_NOT_FOUND,
            format!("Not a directory: {}", req.path),
        ));
    }

    let mut entries = Vec::new();
    let read_dir = match std::fs::read_dir(path) {
        Ok(rd) => rd,
        Err(e) => {
            return Err(rpc_err(
                FS_NOT_FOUND,
                format!("Cannot read {}: {e}", req.path),
            ));
        }
    };

    for entry in read_dir {
        let entry = match entry {
            Ok(e) => e,
            Err(_) => continue,
        };
        let meta = match entry.metadata() {
            Ok(m) => m,
            Err(_) => continue,
        };
        let name = entry.file_name().to_string_lossy().to_string();
        let is_hidden = name.starts_with('.');
        if is_hidden && !req.show_hidden {
            continue;
        }

        let full_path = entry.path().to_string_lossy().to_string();
        entries.push(FsEntry {
            name,
            is_dir: meta.is_dir(),
            size: meta.len(),
            is_hidden,
            full_path,
            mtime: meta.modified().ok().and_then(|t| {
                t.duration_since(std::time::UNIX_EPOCH)
                    .ok()
                    .map(|d| d.as_secs())
            }),
        });
    }

    // Sort: directories first, then files, case-insensitive
    entries.sort_by(|a, b| match (a.is_dir, b.is_dir) {
        (true, false) => std::cmp::Ordering::Less,
        (false, true) => std::cmp::Ordering::Greater,
        _ => a.name.to_lowercase().cmp(&b.name.to_lowercase()),
    });

    let cwd = path.to_string_lossy().to_string();
    let resp = FsListDirResponse { entries, cwd };
    serde_json::to_value(resp).map_err(|e| rpc_err(INTERNAL_ERROR, e.to_string()))
}

fn rpc_err(code: i32, msg: impl Into<String>) -> zeroclaw_api::jsonrpc::JsonRpcError {
    zeroclaw_api::jsonrpc::JsonRpcError {
        code,
        message: msg.into(),
        data: None,
    }
}
