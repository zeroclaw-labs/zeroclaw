use async_trait::async_trait;
use serde_json::json;
use sha2::{Digest, Sha256};
use std::collections::HashMap;
use std::path::{Path, PathBuf};
use tokio::fs;
use zeroclaw_api::tool::{Tool, ToolResult};

/// Workspace backup tool: create, list, verify, and restore timestamped backups
/// with SHA-256 manifest integrity checking.
pub struct BackupTool {
    workspace_dir: PathBuf,
    include_dirs: Vec<String>,
    max_keep: usize,
}

impl BackupTool {
    pub fn new(workspace_dir: PathBuf, include_dirs: Vec<String>, max_keep: usize) -> Self {
        Self {
            workspace_dir,
            include_dirs,
            max_keep,
        }
    }

    fn backups_dir(&self) -> PathBuf {
        self.workspace_dir.join("backups")
    }

    async fn cmd_create(&self) -> anyhow::Result<ToolResult> {
        let ts = chrono::Utc::now().format("%Y%m%dT%H%M%SZ");
        let name = format!("backup-{ts}");
        let backup_dir = self.backups_dir().join(&name);
        fs::create_dir_all(&backup_dir).await?;

        for sub in &self.include_dirs {
            let src = self.workspace_dir.join(sub);
            if src.is_dir() {
                let dst = backup_dir.join(sub);
                copy_dir_recursive(&src, &dst).await?;
            }
        }

        let checksums = compute_checksums(&backup_dir).await?;
        let file_count = checksums.len();
        let manifest = serde_json::to_string_pretty(&checksums)?;
        fs::write(backup_dir.join("manifest.json"), &manifest).await?;

        // Enforce max_keep: remove oldest backups beyond the limit.
        self.enforce_max_keep().await?;

        Ok(ToolResult {
            success: true,
            output: json!({
                "backup": name,
                "file_count": file_count,
            })
            .to_string(),
            error: None,
        })
    }

    async fn enforce_max_keep(&self) -> anyhow::Result<()> {
        let mut backups = self.list_backup_dirs().await?;
        // Sorted newest-first; drop excess from the tail.
        while backups.len() > self.max_keep {
            if let Some(old) = backups.pop() {
                fs::remove_dir_all(old).await?;
            }
        }
        Ok(())
    }

    async fn list_backup_dirs(&self) -> anyhow::Result<Vec<PathBuf>> {
        let dir = self.backups_dir();
        if !dir.is_dir() {
            return Ok(Vec::new());
        }
        let mut entries = Vec::new();
        let mut rd = fs::read_dir(&dir).await?;
        while let Some(e) = rd.next_entry().await? {
            let p = e.path();
            if p.is_dir() && e.file_name().to_string_lossy().starts_with("backup-") {
                entries.push(p);
            }
        }
        entries.sort();
        entries.reverse(); // newest first
        Ok(entries)
    }

    async fn cmd_list(&self) -> anyhow::Result<ToolResult> {
        let dirs = self.list_backup_dirs().await?;
        let mut items = Vec::new();
        for d in &dirs {
            let name = d
                .file_name()
                .map(|n| n.to_string_lossy().to_string())
                .unwrap_or_default();
            let manifest_path = d.join("manifest.json");
            let file_count = if manifest_path.is_file() {
                let data = fs::read_to_string(&manifest_path).await?;
                let map: HashMap<String, String> = serde_json::from_str(&data).unwrap_or_default();
                map.len()
            } else {
                0
            };
            let meta = fs::metadata(d).await?;
            let created = meta
                .created()
                .or_else(|_| meta.modified())
                .unwrap_or(std::time::SystemTime::UNIX_EPOCH);
            let dt: chrono::DateTime<chrono::Utc> = created.into();
            items.push(json!({
                "name": name,
                "file_count": file_count,
                "created": dt.to_rfc3339(),
            }));
        }
        Ok(ToolResult {
            success: true,
            output: serde_json::to_string_pretty(&items)?,
            error: None,
        })
    }

    async fn cmd_verify(&self, backup_name: &str) -> anyhow::Result<ToolResult> {
        let backup_dir = self.backups_dir().join(backup_name);
        if !backup_dir.is_dir() {
            return Ok(ToolResult {
                success: false,
                output: String::new(),
                error: Some(format!("Backup not found: {backup_name}")),
            });
        }
        let manifest_path = backup_dir.join("manifest.json");
        let data = fs::read_to_string(&manifest_path).await?;
        let expected: HashMap<String, String> = serde_json::from_str(&data)?;
        let actual = compute_checksums(&backup_dir).await?;

        let mut mismatches = Vec::new();
        for (path, expected_hash) in &expected {
            match actual.get(path) {
                Some(actual_hash) if actual_hash == expected_hash => {}
                Some(actual_hash) => mismatches.push(json!({
                    "file": path,
                    "expected": expected_hash,
                    "actual": actual_hash,
                })),
                None => mismatches.push(json!({
                    "file": path,
                    "error": "missing",
                })),
            }
        }
        let pass = mismatches.is_empty();
        Ok(ToolResult {
            success: pass,
            output: json!({
                "backup": backup_name,
                "pass": pass,
                "checked": expected.len(),
                "mismatches": mismatches,
            })
            .to_string(),
            error: if pass {
                None
            } else {
                Some("Integrity check failed".into())
            },
        })
    }

    async fn cmd_restore(&self, backup_name: &str, confirm: bool) -> anyhow::Result<ToolResult> {
        let backup_dir = self.backups_dir().join(backup_name);
        if !backup_dir.is_dir() {
            return Ok(ToolResult {
                success: false,
                output: String::new(),
                error: Some(format!("Backup not found: {backup_name}")),
            });
        }

        // Collect restorable subdirectories (skip manifest.json).
        let mut restore_items: Vec<String> = Vec::new();
        let mut rd = fs::read_dir(&backup_dir).await?;
        while let Some(e) = rd.next_entry().await? {
            let name = e.file_name().to_string_lossy().to_string();
            if name == "manifest.json" {
                continue;
            }
            if e.path().is_dir() {
                restore_items.push(name);
            }
        }

        if !confirm {
            return Ok(ToolResult {
                success: true,
                output: json!({
                    "dry_run": true,
                    "backup": backup_name,
                    "would_restore": restore_items,
                })
                .to_string(),
                error: None,
            });
        }

        for sub in &restore_items {
            let src = backup_dir.join(sub);
            let dst = self.workspace_dir.join(sub);
            copy_dir_recursive(&src, &dst).await?;
        }
        Ok(ToolResult {
            success: true,
            output: json!({
                "restored": backup_name,
                "directories": restore_items,
            })
            .to_string(),
            error: None,
        })
    }
}

#[async_trait]
impl Tool for BackupTool {
    fn name(&self) -> &str {
        "backup"
    }

    fn description(&self) -> &str {
        "Create, list, verify, and restore workspace backups"
    }

    fn parameters_schema(&self) -> serde_json::Value {
        json!({
            "type": "object",
            "properties": {
                "command": {
                    "type": "string",
                    "enum": ["create", "list", "verify", "restore"],
                    "description": "Backup command to execute"
                },
                "backup_name": {
                    "type": "string",
                    "description": "Name of backup (for verify/restore)"
                },
                "confirm": {
                    "type": "boolean",
                    "description": "Confirm restore (required for actual restore, default false)"
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
            "create" => self.cmd_create().await,
            "list" => self.cmd_list().await,
            "verify" => {
                let name = args
                    .get("backup_name")
                    .and_then(|v| v.as_str())
                    .ok_or_else(|| anyhow::anyhow!("Missing 'backup_name' for verify"))?;
                self.cmd_verify(name).await
            }
            "restore" => {
                let name = args
                    .get("backup_name")
                    .and_then(|v| v.as_str())
                    .ok_or_else(|| anyhow::anyhow!("Missing 'backup_name' for restore"))?;
                let confirm = args
                    .get("confirm")
                    .and_then(|v| v.as_bool())
                    .unwrap_or(false);
                self.cmd_restore(name, confirm).await
            }
            other => Ok(ToolResult {
                success: false,
                output: String::new(),
                error: Some(format!("Unknown command: {other}")),
            }),
        }
    }
}

// -- Helpers ------------------------------------------------------------------

async fn copy_dir_recursive(src: &Path, dst: &Path) -> anyhow::Result<()> {
    fs::create_dir_all(dst).await?;
    let mut rd = fs::read_dir(src).await?;
    while let Some(entry) = rd.next_entry().await? {
        let src_path = entry.path();
        let dst_path = dst.join(entry.file_name());
        if src_path.is_dir() {
            Box::pin(copy_dir_recursive(&src_path, &dst_path)).await?;
        } else {
            fs::copy(&src_path, &dst_path).await?;
        }
    }
    Ok(())
}

async fn compute_checksums(dir: &Path) -> anyhow::Result<HashMap<String, String>> {
    let mut map = HashMap::new();
    let base = dir.to_path_buf();
    walk_and_hash(&base, dir, &mut map).await?;
    Ok(map)
}

async fn walk_and_hash(
    base: &Path,
    dir: &Path,
    map: &mut HashMap<String, String>,
) -> anyhow::Result<()> {
    let mut rd = fs::read_dir(dir).await?;
    while let Some(entry) = rd.next_entry().await? {
        let path = entry.path();
        if path.is_dir() {
            Box::pin(walk_and_hash(base, &path, map)).await?;
        } else {
            let rel = path
                .strip_prefix(base)
                .unwrap_or(&path)
                .to_string_lossy()
                .replace('\\', "/");
            if rel == "manifest.json" {
                continue;
            }
            let bytes = fs::read(&path).await?;
            let hash = hex::encode(Sha256::digest(&bytes));
            map.insert(rel, hash);
        }
    }
    Ok(())
}

#[cfg(test)]
mod tests {
    use super::*;
    use tempfile::TempDir;

    fn make_tool(tmp: &TempDir) -> BackupTool {
        BackupTool::new(
            tmp.path().to_path_buf(),
            vec!["config".into(), "memory".into()],
            10,
        )
    }

    #[tokio::test]
    async fn create_backup_produces_manifest() {
        let tmp = TempDir::new().unwrap();
        // Seed workspace subdirectories.
        let cfg_dir = tmp.path().join("config");
        std::fs::create_dir_all(&cfg_dir).unwrap();
        std::fs::write(cfg_dir.join("a.toml"), "key = 1").unwrap();

        let tool = make_tool(&tmp);
        let res = tool.execute(json!({"command": "create"})).await.unwrap();
        assert!(res.success, "create failed: {:?}", res.error);

        let parsed: serde_json::Value = serde_json::from_str(&res.output).unwrap();
        assert_eq!(parsed["file_count"], 1);

        // Manifest should exist inside the backup directory.
        let backup_name = parsed["backup"].as_str().unwrap();
        let manifest = tmp
            .path()
            .join("backups")
            .join(backup_name)
            .join("manifest.json");
        assert!(manifest.exists());
    }

    #[tokio::test]
    async fn verify_backup_detects_corruption() {
        let tmp = TempDir::new().unwrap();
        let cfg_dir = tmp.path().join("config");
        std::fs::create_dir_all(&cfg_dir).unwrap();
        std::fs::write(cfg_dir.join("a.toml"), "original").unwrap();

        let tool = make_tool(&tmp);
        let res = tool.execute(json!({"command": "create"})).await.unwrap();
        let parsed: serde_json::Value = serde_json::from_str(&res.output).unwrap();
        let name = parsed["backup"].as_str().unwrap();

        // Corrupt a file inside the backup.
        let backed_up = tmp.path().join("backups").join(name).join("config/a.toml");
        std::fs::write(&backed_up, "corrupted").unwrap();

        let res = tool
            .execute(json!({"command": "verify", "backup_name": name}))
            .await
            .unwrap();
        assert!(!res.success);
        let v: serde_json::Value = serde_json::from_str(&res.output).unwrap();
        assert!(!v["mismatches"].as_array().unwrap().is_empty());
    }

    #[tokio::test]
    async fn restore_requires_confirmation() {
        let tmp = TempDir::new().unwrap();
        let cfg_dir = tmp.path().join("config");
        std::fs::create_dir_all(&cfg_dir).unwrap();
        std::fs::write(cfg_dir.join("a.toml"), "v1").unwrap();

        let tool = make_tool(&tmp);
        let res = tool.execute(json!({"command": "create"})).await.unwrap();
        let parsed: serde_json::Value = serde_json::from_str(&res.output).unwrap();
        let name = parsed["backup"].as_str().unwrap();

        // Without confirm: dry-run.
        let res = tool
            .execute(json!({"command": "restore", "backup_name": name}))
            .await
            .unwrap();
        assert!(res.success);
        let v: serde_json::Value = serde_json::from_str(&res.output).unwrap();
        assert_eq!(v["dry_run"], true);

        // With confirm: actual restore.
        let res = tool
            .execute(json!({"command": "restore", "backup_name": name, "confirm": true}))
            .await
            .unwrap();
        assert!(res.success);
        let v: serde_json::Value = serde_json::from_str(&res.output).unwrap();
        assert!(v.get("restored").is_some());
    }

    #[tokio::test]
    async fn list_backups_sorted_newest_first() {
        let tmp = TempDir::new().unwrap();
        let cfg_dir = tmp.path().join("config");
        std::fs::create_dir_all(&cfg_dir).unwrap();
        std::fs::write(cfg_dir.join("a.toml"), "v1").unwrap();

        let tool = make_tool(&tmp);
        tool.execute(json!({"command": "create"})).await.unwrap();
        // Delay to ensure different second-resolution timestamps.
        tokio::time::sleep(std::time::Duration::from_secs(1)).await;
        tool.execute(json!({"command": "create"})).await.unwrap();

        let res = tool.execute(json!({"command": "list"})).await.unwrap();
        assert!(res.success);
        let items: Vec<serde_json::Value> = serde_json::from_str(&res.output).unwrap();
        assert_eq!(items.len(), 2);
        // Newest first by name (ISO8601 names sort lexicographically).
        assert!(items[0]["name"].as_str().unwrap() >= items[1]["name"].as_str().unwrap());
    }
}
