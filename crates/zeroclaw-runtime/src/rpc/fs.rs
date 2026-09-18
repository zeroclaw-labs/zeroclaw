//! Filesystem RPC methods for remote directory browsing (WSS ACP CWD picker).
//!
//! `fs/list_dir` requires the `Files:Read` grant, and the dispatcher confines
//! it with [`listing_is_authorized`] before the handler touches the path: an
//! operator-level principal may list anything the daemon account can read,
//! and every other principal only absolute paths, without '..' components or
//! Windows network and device prefixes, that an enabled agent it is entitled
//! to use may read under that agent's resolved policy.

use std::path::Path;
use zeroclaw_api::grants::ResolvedGrants;
use zeroclaw_api::jsonrpc::error_codes::*;
use zeroclaw_api::jsonrpc::{FsEntry, FsListDirRequest, FsListDirResponse};
use zeroclaw_config::policy::SecurityPolicy;
use zeroclaw_config::schema::Config;

/// Whether resolving `path` stays on local storage and cannot climb out of what
/// it names: no `..` components and, on Windows, no UNC, verbatim-UNC, or
/// device-namespace prefix. Resolving such a path would make the daemon open a
/// network share or device while merely checking it, so a scoped principal's
/// path is tested lexically first and refused without being resolved.
pub fn resolves_locally(path: &Path) -> bool {
    use std::path::{Component, Prefix};
    path.components().all(|component| match component {
        Component::ParentDir => false,
        Component::Prefix(prefix) => {
            matches!(prefix.kind(), Prefix::Disk(_) | Prefix::VerbatimDisk(_))
        }
        _ => true,
    })
}

/// Whether a principal holding `grants` may list the directory at `requested`.
///
/// Operator-level principals may list anything. Every other principal may list
/// only a path that an enabled agent it is entitled to use may read, judged by
/// that agent's resolved policy: its workspace, readable allowed roots,
/// readable sibling workspaces, and the shared skills directory when the
/// policy is workspace-only, and anything outside its forbidden paths when it
/// is not. The policy resolves the path first, so a symlink is judged by its
/// target and a path that cannot be resolved is refused rather than assumed
/// benign.
///
/// Resolving an agent's policy creates its workspace directory if it is
/// missing, as every other use of that policy does.
pub fn listing_is_authorized(config: &Config, grants: &ResolvedGrants, requested: &Path) -> bool {
    if grants.admin {
        return true;
    }
    config
        .agents
        .iter()
        .filter(|(alias, agent)| agent.enabled && grants.may_use_agent(alias))
        .any(|(alias, _)| {
            SecurityPolicy::for_agent(config, alias)
                .is_ok_and(|policy| policy.is_resolved_path_readable(requested))
        })
}

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

#[cfg(test)]
mod tests {
    use super::resolves_locally;
    use std::path::Path;

    #[test]
    fn resolves_locally_refuses_parent_components() {
        assert!(resolves_locally(Path::new("/srv/agent/workspace")));
        assert!(resolves_locally(Path::new("relative/dir")));
        assert!(!resolves_locally(Path::new("/srv/agent/../other")));
        assert!(!resolves_locally(Path::new("..")));
    }

    #[cfg(windows)]
    #[test]
    fn resolves_locally_refuses_network_and_device_prefixes() {
        assert!(resolves_locally(Path::new(r"C:\agents\workspace")));
        assert!(resolves_locally(Path::new(r"\\?\C:\agents\workspace")));
        assert!(!resolves_locally(Path::new(r"\\attacker.example\share\x")));
        assert!(!resolves_locally(Path::new(r"\\attacker.example@80\x")));
        assert!(!resolves_locally(Path::new(
            r"\\?\UNC\attacker.example\share"
        )));
        assert!(!resolves_locally(Path::new(r"\\.\pipe\zeroclaw")));
    }
}
