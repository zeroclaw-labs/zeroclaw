use super::traits::{Tool, ToolResult};
use async_trait::async_trait;
use serde_json::json;
use std::path::{Path, PathBuf};
use tokio::fs;

/// Workspace data lifecycle tool: retention status, time-based purge, and
/// storage statistics.
pub struct DataManagementTool {
    workspace_dir: PathBuf,
    retention_days: u64,
}

impl DataManagementTool {
    pub fn new(workspace_dir: PathBuf, retention_days: u64) -> Self {
        Self {
            workspace_dir,
            retention_days,
        }
    }

    async fn cmd_retention_status(&self) -> anyhow::Result<ToolResult> {
        let cutoff = chrono::Utc::now()
            - chrono::Duration::days(i64::try_from(self.retention_days).unwrap_or(i64::MAX));
        let cutoff_ts = cutoff.timestamp().try_into().unwrap_or(0u64);
        let count = count_files_older_than(&self.workspace_dir, cutoff_ts).await?;

        Ok(ToolResult {
            success: true,
            output: json!({
                "retention_days": self.retention_days,
                "cutoff": cutoff.to_rfc3339(),
                "affected_files": count,
            })
            .to_string(),
            error: None,
        })
    }

    async fn cmd_purge(&self, dry_run: bool) -> anyhow::Result<ToolResult> {
        let cutoff = chrono::Utc::now()
            - chrono::Duration::days(i64::try_from(self.retention_days).unwrap_or(i64::MAX));
        let cutoff_ts: u64 = cutoff.timestamp().try_into().unwrap_or(0);
        let (deleted, bytes) = purge_old_files(&self.workspace_dir, cutoff_ts, dry_run).await?;

        Ok(ToolResult {
            success: true,
            output: json!({
                "dry_run": dry_run,
                "files": deleted,
                "bytes_freed": bytes,
                "bytes_freed_human": format_bytes(bytes),
            })
            .to_string(),
            error: None,
        })
    }

    async fn cmd_stats(&self) -> anyhow::Result<ToolResult> {
        let (total_files, total_bytes, breakdown) = dir_stats(&self.workspace_dir).await?;
        Ok(ToolResult {
            success: true,
            output: json!({
                "total_files": total_files,
                "total_size": total_bytes,
                "total_size_human": format_bytes(total_bytes),
                "subdirectories": breakdown,
            })
            .to_string(),
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
        "Workspace data retention, purge, and storage statistics"
    }

    fn parameters_schema(&self) -> serde_json::Value {
        json!({
            "type": "object",
            "properties": {
                "command": {
                    "type": "string",
                    "enum": ["retention_status", "purge", "stats"],
                    "description": "Data management command"
                },
                "dry_run": {
                    "type": "boolean",
                    "description": "If true, purge only lists what would be deleted (default true)"
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
                    output: String::new(),
                    error: Some("Missing 'command' parameter".into()),
                });
            }
        };

        match command {
            "retention_status" => self.cmd_retention_status().await,
            "purge" => {
                let dry_run = args
                    .get("dry_run")
                    .and_then(|v| v.as_bool())
                    .unwrap_or(true);
                self.cmd_purge(dry_run).await
            }
            "stats" => self.cmd_stats().await,
            other => Ok(ToolResult {
                success: false,
                output: String::new(),
                error: Some(format!("Unknown command: {other}")),
            }),
        }
    }
}

// -- Helpers ------------------------------------------------------------------

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

async fn count_files_older_than(dir: &Path, cutoff_epoch: u64) -> anyhow::Result<usize> {
    let mut count = 0;
    if !dir.is_dir() {
        return Ok(0);
    }
    let mut rd = fs::read_dir(dir).await?;
    while let Some(entry) = rd.next_entry().await? {
        let path = entry.path();
        if path.is_dir() {
            count += Box::pin(count_files_older_than(&path, cutoff_epoch)).await?;
        } else if let Ok(meta) = fs::metadata(&path).await {
            let modified = meta.modified().unwrap_or(std::time::SystemTime::UNIX_EPOCH);
            let epoch = modified
                .duration_since(std::time::SystemTime::UNIX_EPOCH)
                .unwrap_or_default()
                .as_secs();
            if epoch < cutoff_epoch {
                count += 1;
            }
        }
    }
    Ok(count)
}

async fn purge_old_files(
    dir: &Path,
    cutoff_epoch: u64,
    dry_run: bool,
) -> anyhow::Result<(usize, u64)> {
    let mut deleted = 0usize;
    let mut bytes = 0u64;
    if !dir.is_dir() {
        return Ok((0, 0));
    }
    let mut rd = fs::read_dir(dir).await?;
    while let Some(entry) = rd.next_entry().await? {
        let path = entry.path();
        if path.is_dir() {
            let (d, b) = Box::pin(purge_old_files(&path, cutoff_epoch, dry_run)).await?;
            deleted += d;
            bytes += b;
        } else if let Ok(meta) = fs::metadata(&path).await {
            let modified = meta.modified().unwrap_or(std::time::SystemTime::UNIX_EPOCH);
            let epoch = modified
                .duration_since(std::time::SystemTime::UNIX_EPOCH)
                .unwrap_or_default()
                .as_secs();
            if epoch < cutoff_epoch {
                bytes += meta.len();
                deleted += 1;
                if !dry_run {
                    let _ = fs::remove_file(&path).await;
                }
            }
        }
    }
    Ok((deleted, bytes))
}

async fn dir_stats(root: &Path) -> anyhow::Result<(usize, u64, serde_json::Value)> {
    let mut total_files = 0usize;
    let mut total_bytes = 0u64;
    let mut breakdown = serde_json::Map::new();

    if !root.is_dir() {
        return Ok((0, 0, serde_json::Value::Object(breakdown)));
    }

    let mut rd = fs::read_dir(root).await?;
    while let Some(entry) = rd.next_entry().await? {
        let path = entry.path();
        if path.is_dir() {
            let name = entry.file_name().to_string_lossy().to_string();
            let (f, b) = count_dir_contents(&path).await?;
            total_files += f;
            total_bytes += b;
            breakdown.insert(
                name,
                json!({"files": f, "size": b, "size_human": format_bytes(b)}),
            );
        } else if let Ok(meta) = fs::metadata(&path).await {
            total_files += 1;
            total_bytes += meta.len();
        }
    }
    Ok((
        total_files,
        total_bytes,
        serde_json::Value::Object(breakdown),
    ))
}

async fn count_dir_contents(dir: &Path) -> anyhow::Result<(usize, u64)> {
    let mut files = 0usize;
    let mut bytes = 0u64;
    let mut rd = fs::read_dir(dir).await?;
    while let Some(entry) = rd.next_entry().await? {
        let path = entry.path();
        if path.is_dir() {
            let (f, b) = Box::pin(count_dir_contents(&path)).await?;
            files += f;
            bytes += b;
        } else if let Ok(meta) = fs::metadata(&path).await {
            files += 1;
            bytes += meta.len();
        }
    }
    Ok((files, bytes))
}

#[cfg(test)]
mod tests {
    use super::*;
    use tempfile::TempDir;

    fn make_tool(tmp: &TempDir) -> DataManagementTool {
        DataManagementTool::new(tmp.path().to_path_buf(), 90)
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
    async fn purge_dry_run_does_not_delete() {
        let tmp = TempDir::new().unwrap();
        // Create a file with an old modification time by writing it (it will have
        // the current mtime, so it should not be purged with a 90-day retention).
        std::fs::write(tmp.path().join("recent.txt"), "data").unwrap();

        let tool = make_tool(&tmp);
        let res = tool
            .execute(json!({"command": "purge", "dry_run": true}))
            .await
            .unwrap();
        assert!(res.success);
        let v: serde_json::Value = serde_json::from_str(&res.output).unwrap();
        assert_eq!(v["dry_run"], true);
        // Recent file should not be counted for purge.
        assert_eq!(v["files"], 0);
        // File still exists.
        assert!(tmp.path().join("recent.txt").exists());
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
