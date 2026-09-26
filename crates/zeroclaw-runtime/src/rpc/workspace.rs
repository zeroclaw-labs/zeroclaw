//! `workspace/list` and `fs/{mkdir,rmdir,read,delete,move}`: RPC access to the
//! shared area under `<install>/shared/` and to each agent's workspace.
//!
//! These call the same [`crate::browse`] functions as the dashboard's HTTP
//! adapter and return the same bodies, so the two surfaces cannot drift. The
//! dispatcher authorizes the principal's agent selector before any of these
//! run; nothing here touches the filesystem for a principal it has refused.

use serde_json::Value;
use zeroclaw_api::jsonrpc::{
    FsDeleteRequest, FsMkdirRequest, FsMoveRequest, FsReadRequest, FsRmdirRequest, JsonRpcError,
    WorkspaceListRequest, error_codes,
};
use zeroclaw_config::schema::Config;

use crate::browse::{
    BrowseError, BrowseListing, FileReadBody, delete_agent_workspace_path, list_agent_workspace,
    list_directory, make_agent_workspace_directory, make_directory, move_agent_workspace_path,
    read_agent_workspace_file, remove_directory,
};

type RpcResult = Result<Value, JsonRpcError>;

/// Map a browse failure to the JSON-RPC error that parallels the HTTP
/// adapter's status for the same failure. The message is the error's own
/// text on both surfaces.
fn browse_error(err: BrowseError) -> JsonRpcError {
    let code = match &err {
        BrowseError::Escape(_) | BrowseError::NotADirectory(_) => error_codes::FS_INVALID_PATH,
        BrowseError::NotFound(_) => error_codes::FS_NOT_FOUND,
        BrowseError::Protected(_) | BrowseError::ProtectedFile(_) => {
            error_codes::FS_PERMISSION_DENIED
        }
        BrowseError::InvalidAgent(_) | BrowseError::TooLarge(_, _) => error_codes::INVALID_PARAMS,
        BrowseError::Io(_) => error_codes::INTERNAL_ERROR,
    };
    JsonRpcError {
        code,
        message: err.to_string(),
        data: None,
    }
}

fn to_value(body: impl serde::Serialize) -> RpcResult {
    serde_json::to_value(body).map_err(|e| JsonRpcError {
        code: error_codes::INTERNAL_ERROR,
        message: format!("failed to serialize response: {e}"),
        data: None,
    })
}

pub fn handle_workspace_list(config: &Config, req: &WorkspaceListRequest) -> RpcResult {
    let raw = req.path.as_deref().unwrap_or_default();
    let listing = match req.agent.as_deref() {
        Some(agent) => list_agent_workspace(config, agent, raw),
        None => list_directory(config, raw),
    }
    .map_err(browse_error)?;
    to_value(BrowseListing::from(listing))
}

pub fn handle_fs_mkdir(config: &Config, req: &FsMkdirRequest) -> RpcResult {
    match req.agent.as_deref() {
        Some(agent) => make_agent_workspace_directory(config, agent, &req.path),
        None => make_directory(config, &req.path),
    }
    .map_err(browse_error)?;
    Ok(serde_json::json!({ "created": req.path }))
}

pub fn handle_fs_rmdir(config: &Config, req: &FsRmdirRequest) -> RpcResult {
    remove_directory(config, &req.path).map_err(browse_error)?;
    Ok(serde_json::json!({ "removed": req.path }))
}

pub fn handle_fs_read(config: &Config, req: &FsReadRequest) -> RpcResult {
    let read = read_agent_workspace_file(config, &req.agent, &req.path).map_err(browse_error)?;
    to_value(FileReadBody::from(read))
}

pub fn handle_fs_delete(config: &Config, req: &FsDeleteRequest) -> RpcResult {
    delete_agent_workspace_path(config, &req.agent, &req.path).map_err(browse_error)?;
    Ok(serde_json::json!({ "removed": req.path }))
}

pub fn handle_fs_move(config: &Config, req: &FsMoveRequest) -> RpcResult {
    move_agent_workspace_path(config, &req.agent, &req.from, &req.to).map_err(browse_error)?;
    Ok(serde_json::json!({ "from": req.from, "to": req.to }))
}
