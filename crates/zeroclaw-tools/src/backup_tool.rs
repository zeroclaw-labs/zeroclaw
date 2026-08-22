use crate::helpers::filesystem_boundary::{
    FilesystemBoundaryError, copy_file_atomic, create_dir_path_nofollow,
    open_absolute_dir_nofollow, open_dir_nofollow, open_file_nofollow, write_file_atomic,
};
use async_trait::async_trait;
use cap_std::fs::Dir;
use serde_json::json;
use sha2::{Digest, Sha256};
use std::collections::HashMap;
use std::io::Read;
use std::path::{Path, PathBuf};
use std::sync::Arc;
use zeroclaw_api::tool::{Tool, ToolOutput, ToolResult};
use zeroclaw_config::policy::{SecurityPolicy, ToolOperation};

/// Workspace backup tool: create, list, verify, and restore timestamped backups
/// with SHA-256 manifest integrity checking.
#[derive(Clone)]
pub struct BackupTool {
    data_root: PathBuf,
    include_dirs: Vec<String>,
    max_keep: usize,
    security: Arc<SecurityPolicy>,
}

impl BackupTool {
    pub fn new(workspace_dir: PathBuf, include_dirs: Vec<String>, max_keep: usize) -> Self {
        let security = Arc::new(SecurityPolicy {
            workspace_dir: workspace_dir.clone(),
            ..SecurityPolicy::default()
        });
        Self::new_with_data_root_and_security(workspace_dir, include_dirs, max_keep, security)
    }

    pub fn new_with_security(
        include_dirs: Vec<String>,
        max_keep: usize,
        security: Arc<SecurityPolicy>,
    ) -> Self {
        Self::new_with_data_root_and_security(
            security.workspace_dir.clone(),
            include_dirs,
            max_keep,
            security,
        )
    }

    pub fn new_with_data_root_and_security(
        data_root: PathBuf,
        include_dirs: Vec<String>,
        max_keep: usize,
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
            include_dirs,
            max_keep,
            security: Arc::new(scoped_security),
        }
    }

    fn cmd_create(&self) -> anyhow::Result<ToolResult> {
        if self
            .security
            .enforce_tool_operation(ToolOperation::Act, "backup create")
            .is_err()
        {
            return Ok(rejected(tool_text("tool-backup-error-action-blocked")));
        }
        if self.max_keep == 0 {
            return Ok(rejected(tool_text("tool-backup-error-max-keep")));
        }

        let ts = chrono::Utc::now().format("%Y%m%dT%H%M%SZ");
        let name = format!("backup-{ts}");
        let (workspace_path, workspace) = self.open_workspace()?;
        let backups_path = workspace_path.join("backups");
        self.authorize_write(&backups_path)?;

        // Validate every source tree before creating the backup directory so
        // a stable symlink rejection cannot leave a partial backup behind.
        let mut sources = Vec::new();
        for sub in &self.include_dirs {
            let relative = contained_relative_path(sub)?.to_path_buf();
            if relative.starts_with("backups") || Path::new("backups").starts_with(&relative) {
                return Ok(rejected(tool_text_arg(
                    "tool-backup-error-source-overlap",
                    "path",
                    &relative.display().to_string(),
                )));
            }
            if let Some(src) = open_dir_no_symlinks(&workspace, &relative)? {
                validate_tree_no_symlinks(&src, &relative)?;
                sources.push((relative, src));
            }
        }

        #[cfg(windows)]
        {
            let existing = open_dir_no_symlinks(&workspace, Path::new("backups"))?
                .map(|backups| list_backup_names(&backups))
                .transpose()?
                .map_or(0, |names| names.len());
            if existing >= self.max_keep {
                return Err(boundary_violation(tool_text(
                    "tool-backup-error-rotation-platform",
                )));
            }
        }

        let backups = create_dir_path_nofollow(&workspace, Path::new("backups"))?;
        backups.create_dir(&name)?;
        let backup = open_dir_nofollow(&backups, Path::new(&name))?;

        for (relative, src) in sources {
            let dst = create_dir_path_nofollow(&backup, &relative)?;
            copy_dir_recursive(&src, &dst)?;
        }

        let checksums = compute_checksums(&backup)?;
        let file_count = checksums.len();
        let manifest = serde_json::to_string_pretty(&checksums)?;
        write_file_atomic(&backup, Path::new("manifest.json"), manifest.as_bytes())?;

        // Enforce max_keep: remove oldest backups beyond the limit.
        self.enforce_max_keep(&backups, &backups_path)?;

        Ok(ToolResult {
            success: true,
            output: json!({
                "backup": name,
                "file_count": file_count,
            })
            .to_string()
            .into(),
            error: None,
        })
    }

    fn enforce_max_keep(&self, backups_dir: &Dir, backups_path: &Path) -> anyhow::Result<()> {
        let mut backups = list_backup_names(backups_dir)?;
        // Sorted newest-first; drop excess from the tail.
        while backups.len() > self.max_keep {
            if let Some(old) = backups.pop() {
                self.authorize_write(&backups_path.join(&old))?;
                #[cfg(not(windows))]
                backups_dir.remove_dir_all(old)?;
                #[cfg(windows)]
                return Err(boundary_violation(tool_text(
                    "tool-backup-error-rotation-platform",
                )));
            }
        }
        Ok(())
    }

    fn open_workspace(&self) -> anyhow::Result<(PathBuf, Dir)> {
        let canonical = std::fs::canonicalize(&self.data_root)?;
        if !self.security.is_resolved_path_readable(&canonical) {
            return Err(boundary_violation(tool_text_arg(
                "tool-backup-error-read-blocked",
                "path",
                &canonical.display().to_string(),
            )));
        }
        let dir = open_absolute_dir_nofollow(&canonical)?;
        Ok((canonical, dir))
    }

    fn authorize_write(&self, path: &Path) -> anyhow::Result<()> {
        if self.security.is_runtime_config_path(path)
            || !self.security.is_resolved_path_allowed(path)
        {
            return Err(boundary_violation(tool_text_arg(
                "tool-backup-error-write-blocked",
                "path",
                &path.display().to_string(),
            )));
        }
        Ok(())
    }

    fn cmd_list(&self) -> anyhow::Result<ToolResult> {
        let (_, workspace) = self.open_workspace()?;
        let Some(backups) = open_dir_no_symlinks(&workspace, Path::new("backups"))? else {
            return Ok(ToolResult {
                success: true,
                output: "[]".into(),
                error: None,
            });
        };
        let dirs = list_backup_names(&backups)?;
        let mut items = Vec::new();
        for name in dirs {
            let backup = open_named_backup(&backups, &name)?;
            let file_count = match backup.symlink_metadata("manifest.json") {
                Ok(meta) if meta.is_file() && !meta.is_symlink() => {
                    let data = read_file_to_string_nofollow(&backup, Path::new("manifest.json"))?;
                    let map: HashMap<String, String> =
                        serde_json::from_str(&data).unwrap_or_default();
                    map.len()
                }
                Ok(_) => 0,
                Err(error) if error.kind() == std::io::ErrorKind::NotFound => 0,
                Err(error) => return Err(error.into()),
            };
            let meta = backup.dir_metadata()?;
            let created = meta
                .created()
                .or_else(|_| meta.modified())
                .map(cap_std::time::SystemTime::into_std)?;
            let dt: chrono::DateTime<chrono::Utc> = created.into();
            items.push(json!({
                "name": name,
                "file_count": file_count,
                "created": dt.to_rfc3339(),
            }));
        }
        Ok(ToolResult {
            success: true,
            output: serde_json::to_string_pretty(&items)?.into(),
            error: None,
        })
    }

    fn cmd_verify(&self, backup_name: &str) -> anyhow::Result<ToolResult> {
        if let Err(error) = validate_backup_name(backup_name) {
            return Ok(rejected(error.to_string()));
        }
        let (_, workspace) = self.open_workspace()?;
        let Some(backups) = open_dir_no_symlinks(&workspace, Path::new("backups"))? else {
            return Ok(rejected(tool_text_arg(
                "tool-backup-error-not-found",
                "name",
                backup_name,
            )));
        };
        let backup = match open_named_backup(&backups, backup_name) {
            Ok(dir) => dir,
            Err(error) if is_not_found(&error) => {
                return Ok(rejected(tool_text_arg(
                    "tool-backup-error-not-found",
                    "name",
                    backup_name,
                )));
            }
            Err(error) => return Err(error),
        };
        reject_symlink(&backup, Path::new("manifest.json"))?;
        let data = read_file_to_string_nofollow(&backup, Path::new("manifest.json"))?;
        let expected: HashMap<String, String> = serde_json::from_str(&data)?;
        let actual = compute_checksums(&backup)?;

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
        for path in actual.keys().filter(|path| !expected.contains_key(*path)) {
            mismatches.push(json!({
                "file": path,
                "error": "unexpected",
            }));
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
            .to_string()
            .into(),
            error: if pass {
                None
            } else {
                Some(tool_text("tool-backup-error-integrity"))
            },
        })
    }

    fn cmd_restore(&self, backup_name: &str, confirm: bool) -> anyhow::Result<ToolResult> {
        if let Err(error) = validate_backup_name(backup_name) {
            return Ok(rejected(error.to_string()));
        }
        if confirm
            && self
                .security
                .enforce_tool_operation(ToolOperation::Act, "backup restore")
                .is_err()
        {
            return Ok(rejected(tool_text("tool-backup-error-action-blocked")));
        }
        let (workspace_path, workspace) = self.open_workspace()?;
        let Some(backups) = open_dir_no_symlinks(&workspace, Path::new("backups"))? else {
            return Ok(rejected(tool_text_arg(
                "tool-backup-error-not-found",
                "name",
                backup_name,
            )));
        };
        let backup = match open_named_backup(&backups, backup_name) {
            Ok(dir) => dir,
            Err(error) if is_not_found(&error) => {
                return Ok(rejected(tool_text_arg(
                    "tool-backup-error-not-found",
                    "name",
                    backup_name,
                )));
            }
            Err(error) => return Err(error),
        };

        // Collect restorable subdirectories (skip manifest.json).
        let mut restore_items: Vec<String> = Vec::new();
        for entry in backup.entries()? {
            let entry = entry?;
            let name = entry
                .file_name()
                .into_string()
                .map_err(|_| boundary_violation(tool_text("tool-backup-error-non-utf8")))?;
            if name == "manifest.json" {
                continue;
            }
            let file_type = entry.file_type()?;
            if file_type.is_symlink() {
                return Err(boundary_violation(tool_text_arg(
                    "tool-backup-error-symlink",
                    "path",
                    &name,
                )));
            }
            if file_type.is_dir() {
                validate_tree_no_symlinks(
                    &open_dir_nofollow(&backup, Path::new(&name))?,
                    Path::new(&name),
                )?;
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
                .to_string()
                .into(),
                error: None,
            });
        }

        // Validate every source/destination pair before restoring the first
        // directory so a stable destination symlink cannot cause a partial
        // restore before rejection.
        for sub in &restore_items {
            self.authorize_write(&workspace_path.join(sub))?;
            let src = open_dir_no_symlinks(&backup, Path::new(sub))?
                .ok_or_else(|| anyhow::Error::msg(format!("Backup entry disappeared: {sub}")))?;
            let destination = open_dir_no_symlinks(&workspace, Path::new(sub))?;
            validate_copy_destination(&src, destination.as_ref(), Path::new(sub))?;
        }

        for sub in &restore_items {
            let src = open_dir_no_symlinks(&backup, Path::new(sub))?
                .ok_or_else(|| anyhow::Error::msg(format!("Backup entry disappeared: {sub}")))?;
            let dst = create_dir_path_nofollow(&workspace, Path::new(sub))?;
            copy_dir_recursive(&src, &dst)?;
        }
        Ok(ToolResult {
            success: true,
            output: json!({
                "restored": backup_name,
                "directories": restore_items,
            })
            .to_string()
            .into(),
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
                    output: ToolOutput::default(),
                    error: Some("Missing 'command' parameter".into()),
                });
            }
        };

        let result = match command {
            "create" => {
                let tool = self.clone();
                tokio::task::spawn_blocking(move || tool.cmd_create()).await?
            }
            "list" => {
                let tool = self.clone();
                tokio::task::spawn_blocking(move || tool.cmd_list()).await?
            }
            "verify" => {
                let name = args
                    .get("backup_name")
                    .and_then(|v| v.as_str())
                    .ok_or_else(|| {
                        ::zeroclaw_log::record!(
                            WARN,
                            ::zeroclaw_log::Event::new(
                                module_path!(),
                                ::zeroclaw_log::Action::Reject
                            )
                            .with_outcome(::zeroclaw_log::EventOutcome::Failure)
                            .with_attrs(::serde_json::json!({
                                "param": "backup_name",
                                "command": "verify",
                            })),
                            "backup_tool: missing backup_name for verify"
                        );
                        anyhow::Error::msg("Missing 'backup_name' for verify")
                    })?
                    .to_owned();
                let tool = self.clone();
                tokio::task::spawn_blocking(move || tool.cmd_verify(&name)).await?
            }
            "restore" => {
                let name = args
                    .get("backup_name")
                    .and_then(|v| v.as_str())
                    .ok_or_else(|| {
                        ::zeroclaw_log::record!(
                            WARN,
                            ::zeroclaw_log::Event::new(
                                module_path!(),
                                ::zeroclaw_log::Action::Reject
                            )
                            .with_outcome(::zeroclaw_log::EventOutcome::Failure)
                            .with_attrs(::serde_json::json!({
                                "param": "backup_name",
                                "command": "restore",
                            })),
                            "backup_tool: missing backup_name for restore"
                        );
                        anyhow::Error::msg("Missing 'backup_name' for restore")
                    })?
                    .to_owned();
                let confirm = args
                    .get("confirm")
                    .and_then(|v| v.as_bool())
                    .unwrap_or(false);
                let tool = self.clone();
                tokio::task::spawn_blocking(move || tool.cmd_restore(&name, confirm)).await?
            }
            other => Ok(ToolResult {
                success: false,
                output: ToolOutput::default(),
                error: Some(format!("Unknown command: {other}")),
            }),
        };

        match result {
            Ok(result) => Ok(result),
            Err(error) if error.downcast_ref::<BoundaryViolation>().is_some() => {
                Ok(rejected(error.to_string()))
            }
            Err(error) => match error.downcast_ref::<FilesystemBoundaryError>() {
                Some(boundary) if boundary.is_denied() => {
                    Ok(rejected(localize_filesystem_boundary(boundary)))
                }
                _ => Err(error),
            },
        }
    }
}

// -- Helpers ------------------------------------------------------------------

#[derive(Debug)]
struct BoundaryViolation(String);

impl std::fmt::Display for BoundaryViolation {
    fn fmt(&self, formatter: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        formatter.write_str(&self.0)
    }
}

impl std::error::Error for BoundaryViolation {}

fn boundary_violation(message: impl Into<String>) -> anyhow::Error {
    BoundaryViolation(message.into()).into()
}

fn tool_text(key: &str) -> String {
    crate::i18n::get_required_tool_string(key)
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

fn is_not_found(error: &anyhow::Error) -> bool {
    error
        .downcast_ref::<std::io::Error>()
        .is_some_and(|error| error.kind() == std::io::ErrorKind::NotFound)
}

fn rejected(error: String) -> ToolResult {
    ToolResult {
        success: false,
        output: ToolOutput::default(),
        error: Some(error),
    }
}

fn read_file_to_string_nofollow(dir: &Dir, path: &Path) -> anyhow::Result<String> {
    let mut file = open_file_nofollow(dir, path)?;
    let mut content = String::new();
    file.read_to_string(&mut content)?;
    Ok(content)
}

fn contained_relative_path(path: &str) -> anyhow::Result<&Path> {
    let path = Path::new(path);
    if path.as_os_str().is_empty()
        || path
            .components()
            .any(|component| !matches!(component, std::path::Component::Normal(_)))
    {
        return Err(boundary_violation(tool_text_arg(
            "tool-backup-error-contained",
            "path",
            &path.display().to_string(),
        )));
    }
    Ok(path)
}

fn validate_backup_name(name: &str) -> anyhow::Result<()> {
    let path = contained_relative_path(name)?;
    if path.components().count() != 1 || !name.starts_with("backup-") {
        return Err(boundary_violation(tool_text_arg(
            "tool-backup-error-invalid-name",
            "name",
            name,
        )));
    }
    Ok(())
}

fn reject_symlink(dir: &Dir, path: &Path) -> anyhow::Result<()> {
    match dir.symlink_metadata(path) {
        Ok(metadata) if metadata.is_symlink() => {
            return Err(boundary_violation(tool_text_arg(
                "tool-backup-error-symlink",
                "path",
                &path.display().to_string(),
            )));
        }
        Ok(_) => {}
        Err(error) if error.kind() == std::io::ErrorKind::NotFound => {}
        Err(error) => return Err(error.into()),
    }
    Ok(())
}

fn open_dir_no_symlinks(root: &Dir, relative: &Path) -> anyhow::Result<Option<Dir>> {
    let mut current = root.try_clone()?;
    for component in relative.components() {
        let std::path::Component::Normal(name) = component else {
            return Err(boundary_violation(tool_text_arg(
                "tool-backup-error-contained",
                "path",
                &relative.display().to_string(),
            )));
        };
        let metadata = match current.symlink_metadata(name) {
            Ok(metadata) => metadata,
            Err(error) if error.kind() == std::io::ErrorKind::NotFound => return Ok(None),
            Err(error) => return Err(error.into()),
        };
        if metadata.is_symlink() {
            return Err(boundary_violation(tool_text_arg(
                "tool-backup-error-symlink",
                "path",
                &relative.display().to_string(),
            )));
        }
        if !metadata.is_dir() {
            return Ok(None);
        }
        current = open_dir_nofollow(&current, Path::new(name))?;
    }
    Ok(Some(current))
}

fn open_named_backup(backups: &Dir, name: &str) -> anyhow::Result<Dir> {
    validate_backup_name(name)?;
    reject_symlink(backups, Path::new(name))?;
    let metadata = backups.symlink_metadata(name)?;
    if !metadata.is_dir() {
        return Err(boundary_violation(tool_text_arg(
            "tool-backup-error-not-found",
            "name",
            name,
        )));
    }
    Ok(open_dir_nofollow(backups, Path::new(name))?)
}

fn list_backup_names(backups: &Dir) -> anyhow::Result<Vec<String>> {
    let mut names = Vec::new();
    for entry in backups.entries()? {
        let entry = entry?;
        let Ok(name) = entry.file_name().into_string() else {
            continue;
        };
        let file_type = entry.file_type()?;
        if !file_type.is_symlink() && file_type.is_dir() && validate_backup_name(&name).is_ok() {
            names.push(name);
        }
    }
    names.sort();
    names.reverse();
    Ok(names)
}

fn copy_dir_recursive(src: &Dir, dst: &Dir) -> anyhow::Result<()> {
    for entry in src.entries()? {
        let entry = entry?;
        let name = entry.file_name();
        let file_type = entry.file_type()?;
        if file_type.is_symlink() {
            return Err(boundary_violation(tool_text_arg(
                "tool-backup-error-symlink",
                "path",
                &name.to_string_lossy(),
            )));
        }
        if file_type.is_dir() {
            match dst.symlink_metadata(&name) {
                Ok(metadata) if metadata.is_symlink() => {
                    return Err(boundary_violation(tool_text_arg(
                        "tool-backup-error-symlink",
                        "path",
                        &name.to_string_lossy(),
                    )));
                }
                Ok(metadata) if metadata.is_dir() => {}
                Ok(_) => {
                    return Err(boundary_violation(tool_text_arg(
                        "tool-backup-error-not-directory",
                        "path",
                        &name.to_string_lossy(),
                    )));
                }
                Err(error) if error.kind() == std::io::ErrorKind::NotFound => {
                    dst.create_dir(&name)?;
                }
                Err(error) => return Err(error.into()),
            }
            let src_child = open_dir_nofollow(src, Path::new(&name))?;
            let dst_child = open_dir_nofollow(dst, Path::new(&name))?;
            copy_dir_recursive(&src_child, &dst_child)?;
        } else if file_type.is_file() {
            reject_symlink(dst, Path::new(&name))?;
            let mut input = open_file_nofollow(src, Path::new(&name))?;
            let permissions = input.metadata()?.permissions();
            copy_file_atomic(dst, Path::new(&name), &mut input, Some(permissions))?;
        } else {
            return Err(boundary_violation(tool_text_arg(
                "tool-backup-error-special-file",
                "path",
                &name.to_string_lossy(),
            )));
        }
    }
    Ok(())
}

fn validate_tree_no_symlinks(dir: &Dir, relative: &Path) -> anyhow::Result<()> {
    for entry in dir.entries()? {
        let entry = entry?;
        let name = entry.file_name();
        let path = relative.join(&name);
        let file_type = entry.file_type()?;
        if file_type.is_symlink() {
            return Err(boundary_violation(tool_text_arg(
                "tool-backup-error-symlink",
                "path",
                &path.display().to_string(),
            )));
        }
        if file_type.is_dir() {
            validate_tree_no_symlinks(&open_dir_nofollow(dir, Path::new(&name))?, &path)?;
        } else if !file_type.is_file() {
            return Err(boundary_violation(tool_text_arg(
                "tool-backup-error-special-file",
                "path",
                &path.display().to_string(),
            )));
        }
    }
    Ok(())
}

fn validate_copy_destination(src: &Dir, dst: Option<&Dir>, relative: &Path) -> anyhow::Result<()> {
    for entry in src.entries()? {
        let entry = entry?;
        let name = entry.file_name();
        let path = relative.join(&name);
        let file_type = entry.file_type()?;
        let destination_metadata = match dst {
            Some(dir) => match dir.symlink_metadata(&name) {
                Ok(metadata) => Some(metadata),
                Err(error) if error.kind() == std::io::ErrorKind::NotFound => None,
                Err(error) => return Err(error.into()),
            },
            None => None,
        };
        if destination_metadata
            .as_ref()
            .is_some_and(cap_std::fs::Metadata::is_symlink)
        {
            return Err(boundary_violation(tool_text_arg(
                "tool-backup-error-symlink",
                "path",
                &path.display().to_string(),
            )));
        }
        if file_type.is_dir() {
            let destination_child = match (dst, destination_metadata) {
                (Some(dir), Some(metadata)) if metadata.is_dir() => {
                    Some(open_dir_nofollow(dir, Path::new(&name))?)
                }
                (_, Some(_)) => {
                    return Err(boundary_violation(tool_text_arg(
                        "tool-backup-error-not-directory",
                        "path",
                        &path.display().to_string(),
                    )));
                }
                _ => None,
            };
            validate_copy_destination(
                &open_dir_nofollow(src, Path::new(&name))?,
                destination_child.as_ref(),
                &path,
            )?;
        } else if file_type.is_file()
            && destination_metadata
                .as_ref()
                .is_some_and(cap_std::fs::Metadata::is_dir)
        {
            return Err(boundary_violation(tool_text_arg(
                "tool-backup-error-is-directory",
                "path",
                &path.display().to_string(),
            )));
        }
    }
    Ok(())
}

fn compute_checksums(dir: &Dir) -> anyhow::Result<HashMap<String, String>> {
    let mut map = HashMap::new();
    walk_and_hash(dir, Path::new(""), &mut map)?;
    Ok(map)
}

fn walk_and_hash(
    dir: &Dir,
    relative: &Path,
    map: &mut HashMap<String, String>,
) -> anyhow::Result<()> {
    for entry in dir.entries()? {
        let entry = entry?;
        let name = entry.file_name();
        let path = relative.join(&name);
        let file_type = entry.file_type()?;
        if file_type.is_symlink() {
            return Err(boundary_violation(tool_text_arg(
                "tool-backup-error-symlink",
                "path",
                &path.display().to_string(),
            )));
        }
        if file_type.is_dir() {
            let child = open_dir_nofollow(dir, Path::new(&name))?;
            walk_and_hash(&child, &path, map)?;
        } else if file_type.is_file() {
            let rel = path.to_string_lossy().replace('\\', "/");
            if rel == "manifest.json" {
                continue;
            }
            let mut input = open_file_nofollow(dir, Path::new(&name))?;
            let mut hasher = Sha256::new();
            let mut buffer = [0u8; 64 * 1024];
            loop {
                let count = input.read(&mut buffer)?;
                if count == 0 {
                    break;
                }
                hasher.update(&buffer[..count]);
            }
            let hash = hex::encode(hasher.finalize());
            map.insert(rel, hash);
        } else {
            return Err(boundary_violation(tool_text_arg(
                "tool-backup-error-special-file",
                "path",
                &path.display().to_string(),
            )));
        }
    }
    Ok(())
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn legacy_constructor_remains_available() {
        let _ = BackupTool::new(
            std::env::temp_dir().join("workspace"),
            vec!["config".into()],
            3,
        );
    }
    use tempfile::TempDir;
    use zeroclaw_config::autonomy::AutonomyLevel;

    fn make_tool(tmp: &TempDir) -> BackupTool {
        make_tool_at(tmp.path(), AutonomyLevel::Supervised)
    }

    fn make_tool_with_autonomy(tmp: &TempDir, autonomy: AutonomyLevel) -> BackupTool {
        make_tool_at(tmp.path(), autonomy)
    }

    fn make_tool_at(workspace: &Path, autonomy: AutonomyLevel) -> BackupTool {
        make_tool_at_with_max_keep(workspace, autonomy, 10)
    }

    fn make_tool_at_with_max_keep(
        workspace: &Path,
        autonomy: AutonomyLevel,
        max_keep: usize,
    ) -> BackupTool {
        let security = Arc::new(SecurityPolicy {
            autonomy,
            workspace_dir: workspace.to_path_buf(),
            ..SecurityPolicy::default()
        });
        BackupTool::new_with_security(vec!["config".into(), "memory".into()], max_keep, security)
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
    async fn create_backup_rejects_source_containing_backup_output() {
        let tmp = TempDir::new().unwrap();
        std::fs::create_dir_all(tmp.path().join("backups/backup-existing")).unwrap();
        std::fs::write(
            tmp.path().join("backups/backup-existing/manifest.json"),
            "{}",
        )
        .unwrap();
        let security = Arc::new(SecurityPolicy {
            autonomy: AutonomyLevel::Supervised,
            workspace_dir: tmp.path().to_path_buf(),
            ..SecurityPolicy::default()
        });
        let tool = BackupTool::new_with_security(vec!["backups".into()], 10, security);

        let result = tool.execute(json!({"command": "create"})).await.unwrap();

        assert!(!result.success);
        assert_eq!(
            std::fs::read_dir(tmp.path().join("backups"))
                .unwrap()
                .count(),
            1
        );
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
    async fn verify_backup_rejects_unmanifested_file() {
        let tmp = TempDir::new().unwrap();
        let cfg_dir = tmp.path().join("config");
        std::fs::create_dir_all(&cfg_dir).unwrap();
        std::fs::write(cfg_dir.join("a.toml"), "original").unwrap();
        let tool = make_tool(&tmp);
        let created = tool.execute(json!({"command": "create"})).await.unwrap();
        let created: serde_json::Value = serde_json::from_str(&created.output).unwrap();
        let backup_name = created["backup"].as_str().unwrap();
        std::fs::write(
            tmp.path()
                .join("backups")
                .join(backup_name)
                .join("config/unexpected.toml"),
            "unexpected",
        )
        .unwrap();

        let verified = tool
            .execute(json!({"command": "verify", "backup_name": backup_name}))
            .await
            .unwrap();
        assert!(!verified.success);
        let output: serde_json::Value = serde_json::from_str(&verified.output).unwrap();
        assert!(output["mismatches"].as_array().unwrap().iter().any(|item| {
            item["file"] == "config/unexpected.toml" && item["error"] == "unexpected"
        }));
    }

    #[tokio::test]
    async fn verify_and_restore_reject_escaping_backup_names() {
        let root = TempDir::new().unwrap();
        let workspace = root.path().join("workspace");
        let outside = root.path().join("backup-escape");
        std::fs::create_dir_all(&workspace).unwrap();
        std::fs::create_dir_all(&outside).unwrap();
        std::fs::write(outside.join("manifest.json"), "{}").unwrap();

        let tool = make_tool_at(&workspace, AutonomyLevel::Supervised);
        let expected_error =
            tool_text_arg("tool-backup-error-contained", "path", "../backup-escape");
        for command in ["verify", "restore"] {
            let result = tool
                .execute(json!({
                    "command": command,
                    "backup_name": "../backup-escape",
                    "confirm": true
                }))
                .await
                .unwrap();
            assert!(!result.success);
            assert_eq!(result.error.as_deref(), Some(expected_error.as_str()));
        }

        assert!(outside.join("manifest.json").exists());
    }

    #[cfg(unix)]
    #[tokio::test]
    async fn list_and_verify_do_not_follow_symlinked_manifest() {
        use std::os::unix::fs::symlink;

        let workspace = TempDir::new().unwrap();
        let outside = TempDir::new().unwrap();
        std::fs::create_dir_all(workspace.path().join("config")).unwrap();
        std::fs::write(workspace.path().join("config/value.txt"), "value").unwrap();
        let tool = make_tool(&workspace);
        let created = tool.execute(json!({"command": "create"})).await.unwrap();
        let created: serde_json::Value = serde_json::from_str(&created.output).unwrap();
        let backup_name = created["backup"].as_str().unwrap();
        let manifest = workspace
            .path()
            .join("backups")
            .join(backup_name)
            .join("manifest.json");
        let outside_manifest = outside.path().join("manifest.json");
        std::fs::write(&outside_manifest, "{}").unwrap();
        std::fs::remove_file(&manifest).unwrap();
        symlink(&outside_manifest, &manifest).unwrap();

        let listed = tool.execute(json!({"command": "list"})).await.unwrap();
        assert!(listed.success);
        let listed: serde_json::Value = serde_json::from_str(&listed.output).unwrap();
        assert_eq!(listed[0]["file_count"], 0);

        let verified = tool
            .execute(json!({"command": "verify", "backup_name": backup_name}))
            .await
            .unwrap();
        assert!(!verified.success);
        assert!(verified.error.as_deref().unwrap_or("").contains("symlink"));
    }

    #[tokio::test]
    async fn read_only_policy_blocks_backup_mutations() {
        let tmp = TempDir::new().unwrap();
        let tool = make_tool_with_autonomy(&tmp, AutonomyLevel::ReadOnly);

        let result = tool.execute(json!({"command": "create"})).await.unwrap();

        assert!(!result.success);
        assert!(!tmp.path().join("backups").exists());
    }

    #[tokio::test]
    async fn read_only_policy_blocks_restore_before_any_write() {
        let workspace = TempDir::new().unwrap();
        for directory in ["config", "memory"] {
            std::fs::create_dir_all(workspace.path().join(directory)).unwrap();
            std::fs::write(workspace.path().join(directory).join("value.txt"), "backup").unwrap();
        }

        let creator = make_tool(&workspace);
        let created = creator.execute(json!({"command": "create"})).await.unwrap();
        let created: serde_json::Value = serde_json::from_str(&created.output).unwrap();
        let backup_name = created["backup"].as_str().unwrap();

        std::fs::write(workspace.path().join("config/value.txt"), "current-config").unwrap();
        std::fs::write(workspace.path().join("memory/value.txt"), "current-memory").unwrap();
        let read_only = make_tool_with_autonomy(&workspace, AutonomyLevel::ReadOnly);

        let result = read_only
            .execute(json!({
                "command": "restore",
                "backup_name": backup_name,
                "confirm": true
            }))
            .await
            .unwrap();

        assert!(!result.success);
        let action_blocked = tool_text("tool-backup-error-action-blocked");
        assert_eq!(result.error.as_deref(), Some(action_blocked.as_str()));
        assert_eq!(
            std::fs::read_to_string(workspace.path().join("config/value.txt")).unwrap(),
            "current-config"
        );
        assert_eq!(
            std::fs::read_to_string(workspace.path().join("memory/value.txt")).unwrap(),
            "current-memory"
        );
    }

    #[tokio::test]
    async fn read_only_confirmed_restore_blocks_before_missing_root_access() {
        let tmp = TempDir::new().unwrap();
        let shared_data_root = tmp.path().join("missing-shared-data");
        let agent_workspace = tmp.path().join("missing-agent-workspace");
        let security = Arc::new(SecurityPolicy {
            autonomy: AutonomyLevel::ReadOnly,
            workspace_dir: agent_workspace,
            ..SecurityPolicy::default()
        });
        let tool = BackupTool::new_with_data_root_and_security(
            shared_data_root,
            vec!["config".into()],
            10,
            security,
        );

        let result = tool
            .execute(json!({
                "command": "restore",
                "backup_name": "backup-missing",
                "confirm": true
            }))
            .await
            .unwrap();

        assert!(!result.success);
        let action_blocked = tool_text("tool-backup-error-action-blocked");
        assert_eq!(result.error.as_deref(), Some(action_blocked.as_str()));
    }

    #[tokio::test]
    async fn scoped_policy_shares_action_budget_with_original_policy() {
        let tmp = TempDir::new().unwrap();
        let agent_workspace = tmp.path().join("agent-workspace");
        std::fs::create_dir_all(&agent_workspace).unwrap();
        let security = Arc::new(SecurityPolicy {
            autonomy: AutonomyLevel::Supervised,
            workspace_dir: agent_workspace,
            max_actions_per_hour: 1,
            ..SecurityPolicy::default()
        });
        let tool = BackupTool::new_with_data_root_and_security(
            tmp.path().to_path_buf(),
            vec!["config".into()],
            10,
            security.clone(),
        );
        security
            .enforce_tool_operation(ToolOperation::Act, "test setup")
            .unwrap();

        let result = tool.execute(json!({"command": "create"})).await.unwrap();

        assert!(!result.success);
        let action_blocked = tool_text("tool-backup-error-action-blocked");
        assert_eq!(result.error.as_deref(), Some(action_blocked.as_str()));
        assert!(!tmp.path().join("backups").exists());
    }

    #[cfg(unix)]
    #[tokio::test]
    async fn create_refuses_symlinked_include_directory() {
        use std::os::unix::fs::symlink;

        let tmp = TempDir::new().unwrap();
        let outside = TempDir::new().unwrap();
        std::fs::write(outside.path().join("secret.txt"), "secret").unwrap();
        symlink(outside.path(), tmp.path().join("config")).unwrap();

        let tool = make_tool(&tmp);
        let result = tool.execute(json!({"command": "create"})).await.unwrap();

        assert!(!result.success);
        assert!(result.error.as_deref().unwrap_or("").contains("symlink"));
        let backups = tmp.path().join("backups");
        assert!(
            !backups.exists(),
            "source validation must precede backup directory creation"
        );
        assert!(
            !walkdir_contains(&backups, "secret.txt"),
            "backup traversal must not follow workspace symlinks"
        );
    }

    #[cfg(unix)]
    #[tokio::test]
    async fn rotation_does_not_follow_symlinked_backup_directory() {
        use std::os::unix::fs::symlink;

        let workspace = TempDir::new().unwrap();
        let outside = TempDir::new().unwrap();
        std::fs::write(outside.path().join("keep.txt"), "keep").unwrap();
        let backups = workspace.path().join("backups");
        std::fs::create_dir_all(&backups).unwrap();
        symlink(outside.path(), backups.join("backup-00000000T000000Z")).unwrap();

        let tool = make_tool_at_with_max_keep(workspace.path(), AutonomyLevel::Supervised, 1);
        let result = tool.execute(json!({"command": "create"})).await.unwrap();

        assert!(result.success);
        assert!(outside.path().join("keep.txt").exists());
        assert!(backups.join("backup-00000000T000000Z").is_symlink());
    }

    #[cfg(unix)]
    #[tokio::test]
    async fn restore_rejects_destination_symlink_before_any_write() {
        use std::os::unix::fs::symlink;

        let workspace = TempDir::new().unwrap();
        let outside = TempDir::new().unwrap();
        for directory in ["config", "memory"] {
            std::fs::create_dir_all(workspace.path().join(directory)).unwrap();
            std::fs::write(workspace.path().join(directory).join("value.txt"), "backup").unwrap();
        }
        let tool = make_tool(&workspace);
        let created = tool.execute(json!({"command": "create"})).await.unwrap();
        let created: serde_json::Value = serde_json::from_str(&created.output).unwrap();
        let backup_name = created["backup"].as_str().unwrap();

        std::fs::write(workspace.path().join("memory/value.txt"), "current").unwrap();
        std::fs::remove_dir_all(workspace.path().join("config")).unwrap();
        symlink(outside.path(), workspace.path().join("config")).unwrap();

        let result = tool
            .execute(json!({
                "command": "restore",
                "backup_name": backup_name,
                "confirm": true
            }))
            .await
            .unwrap();

        assert!(!result.success);
        assert!(result.error.as_deref().unwrap_or("").contains("symlink"));
        assert_eq!(
            std::fs::read_to_string(workspace.path().join("memory/value.txt")).unwrap(),
            "current"
        );
        assert!(!outside.path().join("value.txt").exists());
    }

    #[cfg(unix)]
    #[tokio::test]
    async fn restore_replaces_hard_link_without_mutating_external_inode() {
        let workspace = TempDir::new().unwrap();
        let outside = TempDir::new().unwrap();
        std::fs::create_dir_all(workspace.path().join("config")).unwrap();
        let workspace_value = workspace.path().join("config/value.txt");
        std::fs::write(&workspace_value, "backup").unwrap();
        let tool = make_tool(&workspace);
        let created = tool.execute(json!({"command": "create"})).await.unwrap();
        let created: serde_json::Value = serde_json::from_str(&created.output).unwrap();
        let backup_name = created["backup"].as_str().unwrap();

        let outside_value = outside.path().join("outside.txt");
        std::fs::write(&outside_value, "outside").unwrap();
        std::fs::remove_file(&workspace_value).unwrap();
        std::fs::hard_link(&outside_value, &workspace_value).unwrap();

        let restored = tool
            .execute(json!({
                "command": "restore",
                "backup_name": backup_name,
                "confirm": true
            }))
            .await
            .unwrap();

        assert!(restored.success, "error: {:?}", restored.error);
        assert_eq!(std::fs::read_to_string(outside_value).unwrap(), "outside");
        assert_eq!(std::fs::read_to_string(workspace_value).unwrap(), "backup");
    }

    #[cfg(unix)]
    #[tokio::test]
    async fn backup_and_restore_preserve_private_file_mode() {
        use std::os::unix::fs::PermissionsExt;

        let workspace = TempDir::new().unwrap();
        let config = workspace.path().join("config");
        std::fs::create_dir_all(&config).unwrap();
        let secret = config.join("secret.txt");
        std::fs::write(&secret, "secret").unwrap();
        std::fs::set_permissions(&secret, std::fs::Permissions::from_mode(0o600)).unwrap();
        let tool = make_tool(&workspace);

        let created = tool.execute(json!({"command": "create"})).await.unwrap();
        let created: serde_json::Value = serde_json::from_str(&created.output).unwrap();
        let backup_name = created["backup"].as_str().unwrap();
        let backup_secret = workspace
            .path()
            .join("backups")
            .join(backup_name)
            .join("config/secret.txt");
        assert_eq!(
            std::fs::metadata(&backup_secret)
                .unwrap()
                .permissions()
                .mode()
                & 0o777,
            0o600
        );

        std::fs::set_permissions(&secret, std::fs::Permissions::from_mode(0o644)).unwrap();
        let restored = tool
            .execute(json!({
                "command": "restore",
                "backup_name": backup_name,
                "confirm": true
            }))
            .await
            .unwrap();

        assert!(restored.success, "error: {:?}", restored.error);
        assert_eq!(
            std::fs::metadata(secret).unwrap().permissions().mode() & 0o777,
            0o600
        );
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
        let read_only = make_tool_with_autonomy(&tmp, AutonomyLevel::ReadOnly);
        let res = read_only
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

    #[cfg(unix)]
    fn walkdir_contains(root: &Path, needle: &str) -> bool {
        let Ok(entries) = std::fs::read_dir(root) else {
            return false;
        };
        entries.flatten().any(|entry| {
            entry.file_name() == needle
                || entry.file_type().is_ok_and(|file_type| file_type.is_dir())
                    && walkdir_contains(&entry.path(), needle)
        })
    }
}
