use crate::helpers::filesystem_boundary::{
    FilesystemBoundaryError, open_absolute_dir_nofollow, open_dir_nofollow,
};
use async_trait::async_trait;
use cap_std::fs::Dir;
use serde_json::json;
use std::path::{Path, PathBuf};
use std::sync::Arc;
use zeroclaw_api::tool::{Tool, ToolOutput, ToolResult};
use zeroclaw_config::policy::SecurityPolicy;

/// Workspace data lifecycle tool: retention status, purge preview, and storage
/// statistics. Confirmed purge is currently unavailable.
#[derive(Clone)]
pub struct DataManagementTool {
    data_root: PathBuf,
    retention_days: u64,
    security: Arc<SecurityPolicy>,
}

impl DataManagementTool {
    pub fn new(workspace_dir: PathBuf, retention_days: u64) -> Self {
        let security = Arc::new(SecurityPolicy {
            workspace_dir: workspace_dir.clone(),
            ..SecurityPolicy::default()
        });
        Self::new_with_data_root_and_security(workspace_dir, retention_days, security)
    }

    pub fn new_with_security(retention_days: u64, security: Arc<SecurityPolicy>) -> Self {
        Self::new_with_data_root_and_security(
            security.workspace_dir.clone(),
            retention_days,
            security,
        )
    }

    pub fn new_with_data_root_and_security(
        data_root: PathBuf,
        retention_days: u64,
        security: Arc<SecurityPolicy>,
    ) -> Self {
        // Extend only this tool's policy view; the shared per-agent policy
        // remains unchanged for every other tool.
        let mut scoped_security = (*security).clone();
        if !scoped_security.allowed_roots.contains(&data_root) {
            scoped_security.allowed_roots.push(data_root.clone());
        }
        Self {
            data_root,
            retention_days,
            security: Arc::new(scoped_security),
        }
    }

    fn open_workspace(&self) -> anyhow::Result<Dir> {
        let canonical = std::fs::canonicalize(&self.data_root)?;
        if !self.security.is_resolved_path_readable(&canonical) {
            return Err(data_boundary_violation(tool_text_arg(
                "tool-data-management-error-read-blocked",
                "path",
                &canonical.display().to_string(),
            )));
        }
        Ok(open_absolute_dir_nofollow(&canonical)?)
    }

    fn cmd_retention_status(&self) -> anyhow::Result<ToolResult> {
        let cutoff = retention_cutoff(self.retention_days);
        let cutoff_ts = cutoff.timestamp().try_into().unwrap_or(0u64);
        let workspace = self.open_workspace()?;
        let (count, _) = retention_summary(&workspace, cutoff_ts)?;

        Ok(ToolResult {
            success: true,
            output: json!({
                "retention_days": self.retention_days,
                "cutoff": cutoff.to_rfc3339(),
                "affected_files": count,
            })
            .to_string()
            .into(),
            error: None,
        })
    }

    fn cmd_purge_preview(&self) -> anyhow::Result<ToolResult> {
        let cutoff = retention_cutoff(self.retention_days);
        let cutoff_ts: u64 = cutoff.timestamp().try_into().unwrap_or(0);
        let workspace = self.open_workspace()?;
        let (files, bytes) = retention_summary(&workspace, cutoff_ts)?;

        Ok(ToolResult {
            success: true,
            output: json!({
                "dry_run": true,
                "files": files,
                "bytes_freed": bytes,
                "bytes_freed_human": format_bytes(bytes),
            })
            .to_string()
            .into(),
            error: None,
        })
    }

    fn cmd_stats(&self) -> anyhow::Result<ToolResult> {
        let workspace = self.open_workspace()?;
        let (total_files, total_bytes, breakdown) = dir_stats(&workspace)?;
        Ok(ToolResult {
            success: true,
            output: json!({
                "total_files": total_files,
                "total_size": total_bytes,
                "total_size_human": format_bytes(total_bytes),
                "subdirectories": breakdown,
            })
            .to_string()
            .into(),
            error: None,
        })
    }
}

#[async_trait]
impl Tool for DataManagementTool {
    fn name(&self) -> &str {
        "data_management"
    }

    fn description(&self) -> &str {
        "Workspace retention preview and storage statistics"
    }

    fn parameters_schema(&self) -> serde_json::Value {
        json!({
            "type": "object",
            "properties": {
                "command": {
                    "type": "string",
                    "enum": ["retention_status", "purge", "stats"],
                    "description": "Data management command; purge is preview-only"
                },
                "dry_run": {
                    "type": "boolean",
                    "description": "If true, purge only lists what would be deleted (default true). Confirmed purge is currently unavailable."
                }
            },
            "required": ["command"]
        })
    }

    async fn execute(&self, args: serde_json::Value) -> anyhow::Result<ToolResult> {
        let command = match args.get("command").and_then(|v| v.as_str()) {
            Some(c) => c,
            None => {
                return Ok(ToolResult {
                    success: false,
                    output: ToolOutput::default(),
                    error: Some("Missing 'command' parameter".into()),
                });
            }
        };

        let result = match command {
            "retention_status" => {
                let tool = self.clone();
                tokio::task::spawn_blocking(move || tool.cmd_retention_status()).await?
            }
            "purge" => {
                let dry_run = args
                    .get("dry_run")
                    .and_then(|v| v.as_bool())
                    .unwrap_or(true);
                if !dry_run {
                    Ok(confirmed_purge_unavailable())
                } else {
                    let tool = self.clone();
                    tokio::task::spawn_blocking(move || tool.cmd_purge_preview()).await?
                }
            }
            "stats" => {
                let tool = self.clone();
                tokio::task::spawn_blocking(move || tool.cmd_stats()).await?
            }
            other => Ok(ToolResult {
                success: false,
                output: ToolOutput::default(),
                error: Some(format!("Unknown command: {other}")),
            }),
        };

        match result {
            Ok(result) => Ok(result),
            Err(error) if error.downcast_ref::<DataBoundaryViolation>().is_some() => {
                Ok(ToolResult {
                    success: false,
                    output: ToolOutput::default(),
                    error: Some(error.to_string()),
                })
            }
            Err(error) => match error.downcast_ref::<FilesystemBoundaryError>() {
                Some(boundary) if boundary.is_denied() => Ok(ToolResult {
                    success: false,
                    output: ToolOutput::default(),
                    error: Some(localize_filesystem_boundary(boundary)),
                }),
                _ => Err(error),
            },
        }
    }
}

// -- Helpers ------------------------------------------------------------------

#[derive(Debug)]
struct DataBoundaryViolation(String);

impl std::fmt::Display for DataBoundaryViolation {
    fn fmt(&self, formatter: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        formatter.write_str(&self.0)
    }
}

impl std::error::Error for DataBoundaryViolation {}

fn data_boundary_violation(message: impl Into<String>) -> anyhow::Error {
    DataBoundaryViolation(message.into()).into()
}

fn confirmed_purge_unavailable() -> ToolResult {
    ToolResult {
        success: false,
        output: ToolOutput::default(),
        error: Some(crate::i18n::get_required_tool_string(
            "tool-data-management-error-purge-disabled",
        )),
    }
}

fn tool_text_arg(key: &str, name: &str, value: &str) -> String {
    crate::i18n::get_required_tool_string_with_args(key, &[(name, value)])
}

fn localize_filesystem_boundary(error: &FilesystemBoundaryError) -> String {
    match error.localization() {
        Some((key, path)) => {
            crate::i18n::get_required_tool_string_with_args(key, &[("path", &path)])
        }
        None => error.to_string(),
    }
}

fn format_bytes(bytes: u64) -> String {
    const KB: u64 = 1024;
    const MB: u64 = 1024 * KB;
    const GB: u64 = 1024 * MB;
    if bytes >= GB {
        format!("{:.1} GB", bytes as f64 / GB as f64)
    } else if bytes >= MB {
        format!("{:.1} MB", bytes as f64 / MB as f64)
    } else if bytes >= KB {
        format!("{:.1} KB", bytes as f64 / KB as f64)
    } else {
        format!("{bytes} B")
    }
}

fn retention_cutoff(retention_days: u64) -> chrono::DateTime<chrono::Utc> {
    let Some(duration) = i64::try_from(retention_days)
        .ok()
        .and_then(chrono::TimeDelta::try_days)
    else {
        return chrono::DateTime::<chrono::Utc>::MIN_UTC;
    };
    chrono::Utc::now()
        .checked_sub_signed(duration)
        .unwrap_or(chrono::DateTime::<chrono::Utc>::MIN_UTC)
}

fn retention_summary(dir: &Dir, cutoff_epoch: u64) -> anyhow::Result<(usize, u64)> {
    let mut count = 0;
    let mut bytes = 0;
    for entry in dir.entries()? {
        let entry = entry?;
        let name = entry.file_name();
        let metadata = dir.symlink_metadata(&name)?;
        let file_type = metadata.file_type();
        if file_type.is_symlink() {
            continue;
        }
        if file_type.is_dir() {
            let (child_count, child_bytes) =
                retention_summary(&open_dir_nofollow(dir, Path::new(&name))?, cutoff_epoch)?;
            count += child_count;
            bytes += child_bytes;
        } else if file_type.is_file() {
            let modified = metadata
                .modified()
                .map(cap_std::time::SystemTime::into_std)?;
            let epoch = modified
                .duration_since(std::time::SystemTime::UNIX_EPOCH)
                .unwrap_or_default()
                .as_secs();
            if epoch < cutoff_epoch {
                count += 1;
                bytes += metadata.len();
            }
        }
    }
    Ok((count, bytes))
}

fn dir_stats(root: &Dir) -> anyhow::Result<(usize, u64, serde_json::Value)> {
    let mut total_files = 0usize;
    let mut total_bytes = 0u64;
    let mut breakdown = serde_json::Map::new();

    for entry in root.entries()? {
        let entry = entry?;
        let name = entry.file_name();
        let metadata = root.symlink_metadata(&name)?;
        let file_type = metadata.file_type();
        if file_type.is_symlink() {
            continue;
        }
        if file_type.is_dir() {
            let display_name = name.to_string_lossy().to_string();
            let (f, b) = count_dir_contents(&open_dir_nofollow(root, Path::new(&name))?)?;
            total_files += f;
            total_bytes += b;
            breakdown.insert(
                display_name,
                json!({"files": f, "size": b, "size_human": format_bytes(b)}),
            );
        } else if file_type.is_file() {
            total_files += 1;
            total_bytes += metadata.len();
        }
    }
    Ok((
        total_files,
        total_bytes,
        serde_json::Value::Object(breakdown),
    ))
}

fn count_dir_contents(dir: &Dir) -> anyhow::Result<(usize, u64)> {
    let mut files = 0usize;
    let mut bytes = 0u64;
    for entry in dir.entries()? {
        let entry = entry?;
        let name = entry.file_name();
        let metadata = dir.symlink_metadata(&name)?;
        let file_type = metadata.file_type();
        if file_type.is_symlink() {
            continue;
        }
        if file_type.is_dir() {
            let (f, b) = count_dir_contents(&open_dir_nofollow(dir, Path::new(&name))?)?;
            files += f;
            bytes += b;
        } else if file_type.is_file() {
            files += 1;
            bytes += metadata.len();
        }
    }
    Ok((files, bytes))
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn legacy_constructor_remains_available() {
        let _ = DataManagementTool::new(std::env::temp_dir().join("workspace"), 30);
    }
    use tempfile::TempDir;

    fn make_tool(tmp: &TempDir) -> DataManagementTool {
        make_tool_with_retention(tmp, 90)
    }

    fn make_tool_with_retention(tmp: &TempDir, retention_days: u64) -> DataManagementTool {
        let security = Arc::new(SecurityPolicy {
            workspace_dir: tmp.path().to_path_buf(),
            ..SecurityPolicy::default()
        });
        DataManagementTool::new_with_security(retention_days, security)
    }

    #[tokio::test]
    async fn retention_status_reports_correct_cutoff() {
        let tmp = TempDir::new().unwrap();
        let tool = make_tool(&tmp);
        let res = tool
            .execute(json!({"command": "retention_status"}))
            .await
            .unwrap();
        assert!(res.success);
        let v: serde_json::Value = serde_json::from_str(&res.output).unwrap();
        assert_eq!(v["retention_days"], 90);
        assert!(v["cutoff"].is_string());
    }

    #[tokio::test]
    async fn purge_preview_reports_only_eligible_files_without_deleting() {
        let tmp = TempDir::new().unwrap();
        let old = tmp.path().join("old.txt");
        let recent = tmp.path().join("recent.txt");
        std::fs::write(&old, "old!").unwrap();
        std::fs::write(&recent, "recent").unwrap();
        std::fs::File::options()
            .write(true)
            .open(&old)
            .unwrap()
            .set_modified(std::time::SystemTime::now() - std::time::Duration::from_secs(2 * 86_400))
            .unwrap();

        let tool = make_tool_with_retention(&tmp, 1);
        let res = tool
            .execute(json!({"command": "purge", "dry_run": true}))
            .await
            .unwrap();
        assert!(res.success);
        let v: serde_json::Value = serde_json::from_str(&res.output).unwrap();
        assert_eq!(v["dry_run"], true);
        assert_eq!(v["files"], 1);
        assert_eq!(v["bytes_freed"], 4);
        assert!(old.exists());
        assert!(recent.exists());
    }

    #[tokio::test]
    async fn confirmed_purge_is_unavailable_and_preserves_eligible_file() {
        let tmp = TempDir::new().unwrap();
        let old = tmp.path().join("old.txt");
        let file = std::fs::OpenOptions::new()
            .write(true)
            .create(true)
            .truncate(true)
            .open(&old)
            .unwrap();
        file.set_len(4).unwrap();
        file.set_modified(
            std::time::SystemTime::now() - std::time::Duration::from_secs(2 * 86_400),
        )
        .unwrap();
        let tool = make_tool_with_retention(&tmp, 1);

        let result = tool
            .execute(json!({"command": "purge", "dry_run": false}))
            .await
            .unwrap();

        assert!(!result.success);
        assert!(old.exists());
        assert!(
            result
                .error
                .as_deref()
                .unwrap_or("")
                .contains("unavailable")
        );
    }

    #[tokio::test]
    async fn confirmed_purge_fails_closed_before_workspace_access() {
        let tmp = TempDir::new().unwrap();
        let missing_workspace = tmp.path().join("missing");
        let tool = DataManagementTool::new(missing_workspace, 90);

        let result = tool
            .execute(json!({"command": "purge", "dry_run": false}))
            .await
            .unwrap();

        assert!(!result.success);
    }

    #[tokio::test]
    async fn extreme_retention_window_returns_a_bounded_preview() {
        let tmp = TempDir::new().unwrap();
        std::fs::write(tmp.path().join("file.txt"), "data").unwrap();
        let tool = make_tool_with_retention(&tmp, u64::MAX);

        let status = tool
            .execute(json!({"command": "retention_status"}))
            .await
            .unwrap();
        let preview = tool
            .execute(json!({"command": "purge", "dry_run": true}))
            .await
            .unwrap();

        assert!(status.success);
        assert!(preview.success);
        let preview: serde_json::Value = serde_json::from_str(&preview.output).unwrap();
        assert_eq!(preview["files"], 0);
    }

    #[cfg(unix)]
    #[tokio::test]
    async fn retention_walks_do_not_follow_symlinked_directories() {
        use std::os::unix::fs::symlink;

        let tmp = TempDir::new().unwrap();
        let outside = TempDir::new().unwrap();
        std::fs::write(outside.path().join("outside.txt"), "outside").unwrap();
        symlink(outside.path(), tmp.path().join("linked-outside")).unwrap();
        let tool = make_tool(&tmp);

        let stats = tool.execute(json!({"command": "stats"})).await.unwrap();
        let stats: serde_json::Value = serde_json::from_str(&stats.output).unwrap();
        assert_eq!(stats["total_files"], 0);

        let purge = tool
            .execute(json!({"command": "purge", "dry_run": true}))
            .await
            .unwrap();
        assert!(purge.success);
        assert!(outside.path().join("outside.txt").exists());
    }

    #[tokio::test]
    async fn stats_counts_files_correctly() {
        let tmp = TempDir::new().unwrap();
        let sub = tmp.path().join("subdir");
        std::fs::create_dir_all(&sub).unwrap();
        std::fs::write(sub.join("a.txt"), "hello").unwrap();
        std::fs::write(sub.join("b.txt"), "world").unwrap();
        std::fs::write(tmp.path().join("root.txt"), "top").unwrap();

        let tool = make_tool(&tmp);
        let res = tool.execute(json!({"command": "stats"})).await.unwrap();
        assert!(res.success);
        let v: serde_json::Value = serde_json::from_str(&res.output).unwrap();
        assert_eq!(v["total_files"], 3);
    }
}
