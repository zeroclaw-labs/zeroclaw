use crate::dir_monitor::DirMonitor;
use crate::strategy::{CleanupStrategy, SpaceBasedStrategy, StrategyConfig, TimeBasedStrategy};
use anyhow::{Context, Result};
use glob::Pattern;
use serde_json::json;
use std::path::{Component, Path, PathBuf};

fn temp_file_event(attrs: serde_json::Value) -> zeroclaw_log::Event {
    ::zeroclaw_log::Event::new(module_path!(), ::zeroclaw_log::Action::Note).with_attrs(attrs)
}

#[derive(Debug, thiserror::Error)]
pub enum RuleValidationError {
    #[error("Invalid glob pattern: {0}")]
    InvalidGlobPattern(String),
    #[error("Both retention_hours and max_size_mb are zero")]
    NoLimits,
    #[error("Cleanup rule path '{input}' escapes cleanup root '{cleanup_root}'")]
    PathEscapesCleanupRoot { input: String, cleanup_root: String },
    #[error("Failed to resolve cleanup rule path: {0}")]
    PathResolutionFailed(String),
}

pub struct CleanupRule {
    /// Configured cleanup-relative directory path for this cleanup rule.
    rel_path: String,

    /// Optional file name filter.
    pattern: Option<Pattern>,

    /// Cleanup thresholds.
    retention_hours: u64,
    max_size_mb: u64,

    /// Strategy executors.
    time_strategy: TimeBasedStrategy,
    space_strategy: SpaceBasedStrategy,
}

impl CleanupRule {
    pub fn new(
        cleanup_root: &Path,
        rel_path: &str,
        pattern: Option<&str>,
        retention_hours: u64,
        max_size_mb: u64,
    ) -> Result<Self, RuleValidationError> {
        // At least one limit must be configured.
        if retention_hours == 0 && max_size_mb == 0 {
            return Err(RuleValidationError::NoLimits);
        }

        resolve_cleanup_path(cleanup_root, rel_path)?;

        // Parse the glob pattern.
        let parsed_pattern = pattern
            .map(Pattern::new)
            .transpose()
            .map_err(|e| RuleValidationError::InvalidGlobPattern(e.to_string()))?;

        Ok(Self {
            rel_path: rel_path.to_string(),
            pattern: parsed_pattern,
            retention_hours,
            max_size_mb,
            time_strategy: TimeBasedStrategy,
            space_strategy: SpaceBasedStrategy,
        })
    }

    /// Run cleanup and return the deleted files.
    pub fn enforce(&self, cleanup_root: &Path) -> Result<Vec<PathBuf>> {
        let mut deleted = Vec::new();

        let Some(rule_dir) = self.resolve_existing_directory(cleanup_root)? else {
            return Ok(deleted);
        };

        // Enumerate candidate files.
        let files = DirMonitor::enumerate_files(&rule_dir, self.pattern.as_ref())
            .with_context(|| format!("Failed to enumerate files in {}", rule_dir.display()))?;

        if files.is_empty() {
            return Ok(deleted);
        }

        // Calculate the current directory size in bytes.
        let current_size_bytes: u64 = files.iter().map(|f| f.size_bytes).sum();
        let max_size_bytes = self.max_size_mb * 1024 * 1024;

        let config = StrategyConfig {
            retention_hours: self.retention_hours,
            max_size_bytes,
            current_dir_size_bytes: current_size_bytes,
        };

        // Time-based expiration cleanup.
        let expired = self.time_strategy.find_files_to_delete(&files, &config);

        if !expired.is_empty() {
            // Log every file removed by the time-based strategy.
            for file in &expired {
                ::zeroclaw_log::record!(
                    INFO,
                    temp_file_event(json!({
                        "path": file.display().to_string(),
                        "rule_path": rule_dir.display().to_string(),
                        "strategy": "time_based",
                        "retention_hours": self.retention_hours
                    })),
                    "Deleting expired temporary file"
                );
            }
            self.delete_files(&expired)?;
            deleted.extend(expired);
        }

        // Re-enumerate after time-based deletions.
        let files_after_time = DirMonitor::enumerate_files(&rule_dir, self.pattern.as_ref())?;
        let current_size_bytes_after: u64 = files_after_time.iter().map(|f| f.size_bytes).sum();
        let max_size_bytes_after = self.max_size_mb * 1024 * 1024;

        let config_after = StrategyConfig {
            retention_hours: self.retention_hours,
            max_size_bytes: max_size_bytes_after,
            current_dir_size_bytes: current_size_bytes_after,
        };

        // Size-limit cleanup.
        let pruned = self
            .space_strategy
            .find_files_to_delete(&files_after_time, &config_after);

        if !pruned.is_empty() {
            // Log every file removed by the size-based strategy.
            for file in &pruned {
                // Best-effort file size lookup for logs.
                let size_mb = file
                    .metadata()
                    .map(|m| m.len() as f64 / 1024.0 / 1024.0)
                    .unwrap_or(0.0);

                ::zeroclaw_log::record!(
                    INFO,
                    temp_file_event(json!({
                            "path": file.display().to_string(),
                            "rule_path": rule_dir.display().to_string(),
                            "strategy": "space_based",
                            "size_mb": size_mb,
                            "limit_mb": self.max_size_mb,
                        "current_size_mb": current_size_bytes_after as f64 / 1024.0 / 1024.0
                    })),
                    "Deleting temporary file due to size limit exceeded"
                );
            }
            self.delete_files(&pruned)?;
            deleted.extend(pruned);
        }

        Ok(deleted)
    }

    /// Register a freshly written file and run matching cleanup rules.
    pub fn register_and_enforce(
        &self,
        cleanup_root: &Path,
        file_path: &Path,
    ) -> Result<Vec<PathBuf>> {
        // Skip files that do not match this rule.
        if !self.matches(cleanup_root, file_path) {
            return Ok(vec![]);
        }

        self.enforce(cleanup_root)
    }

    /// Check whether a file matches this rule.
    pub fn matches(&self, cleanup_root: &Path, path: &Path) -> bool {
        let resolved_path = match std::fs::canonicalize(path) {
            Ok(path) => path,
            Err(_) => return false,
        };

        let rule_dir = match self.resolve_directory(cleanup_root) {
            Ok(path) => path,
            Err(_) => return false,
        };

        // Ensure the file is under the rule directory.
        if !resolved_path.starts_with(&rule_dir) {
            return false;
        }

        // Check the file name pattern.
        if let Some(ref pat) = self.pattern {
            if let Some(file_name) = resolved_path.file_name().and_then(|n| n.to_str()) {
                return pat.matches(file_name);
            }
            // If the file name cannot be read, it does not match.
            return false;
        }

        // No pattern restriction means the file matches.
        true
    }

    /// Delete files and record logs.
    fn delete_files(&self, paths: &[PathBuf]) -> Result<()> {
        for path in paths {
            match std::fs::remove_file(path) {
                Ok(()) => {
                    ::zeroclaw_log::record!(
                        DEBUG,
                        temp_file_event(json!({"path": path.display().to_string()})),
                        "Deleted temporary file"
                    );
                }
                Err(e) => {
                    ::zeroclaw_log::record!(
                        WARN,
                        temp_file_event(
                            json!({"path": path.display().to_string(), "error": e.to_string()})
                        )
                        .with_outcome(::zeroclaw_log::EventOutcome::Failure),
                        "Failed to delete temporary file"
                    );
                }
            }
        }
        Ok(())
    }

    /// Return the display path for this rule.
    pub fn path_display(&self) -> String {
        self.rel_path.clone()
    }

    fn resolve_directory(&self, cleanup_root: &Path) -> Result<PathBuf, RuleValidationError> {
        resolve_cleanup_path(cleanup_root, &self.rel_path)
    }

    fn resolve_existing_directory(&self, cleanup_root: &Path) -> Result<Option<PathBuf>> {
        match self.resolve_directory(cleanup_root) {
            Ok(path) => {
                if !path.exists() {
                    ::zeroclaw_log::record!(
                        DEBUG,
                        temp_file_event(json!({"path": path.display().to_string()})),
                        "Cleanup rule path does not exist, skipping"
                    );
                    return Ok(None);
                }

                Ok(Some(path))
            }
            Err(RuleValidationError::PathEscapesCleanupRoot {
                input,
                cleanup_root,
            }) => {
                ::zeroclaw_log::record!(
                    WARN,
                    temp_file_event(json!({
                        "rule_path": input,
                        "cleanup_root": cleanup_root
                    }))
                    .with_outcome(::zeroclaw_log::EventOutcome::Failure),
                    "Cleanup rule path escapes cleanup root, skipping"
                );
                Ok(None)
            }
            Err(error) => Err(anyhow::Error::msg(error.to_string())),
        }
    }
}

pub(crate) fn resolve_cleanup_path(
    cleanup_root: &Path,
    raw_path: &str,
) -> Result<PathBuf, RuleValidationError> {
    let canonical_root = canonicalize_or_normalize_path(cleanup_root)?;
    let mut current = canonical_root.clone();

    for component in Path::new(raw_path).components() {
        match component {
            Component::Prefix(_) | Component::RootDir => {
                return Err(RuleValidationError::PathEscapesCleanupRoot {
                    input: raw_path.to_string(),
                    cleanup_root: canonical_root.display().to_string(),
                });
            }
            Component::CurDir => {}
            Component::ParentDir => {
                if current == canonical_root {
                    return Err(RuleValidationError::PathEscapesCleanupRoot {
                        input: raw_path.to_string(),
                        cleanup_root: canonical_root.display().to_string(),
                    });
                }
                current.pop();
            }
            Component::Normal(segment) => {
                current.push(segment);
                if let Ok(metadata) = std::fs::symlink_metadata(&current) {
                    let resolved = std::fs::canonicalize(&current).map_err(|error| {
                        RuleValidationError::PathResolutionFailed(format!(
                            "failed to canonicalize '{}': {error}",
                            current.display()
                        ))
                    })?;
                    if !resolved.starts_with(&canonical_root) {
                        return Err(RuleValidationError::PathEscapesCleanupRoot {
                            input: raw_path.to_string(),
                            cleanup_root: canonical_root.display().to_string(),
                        });
                    }
                    current = resolved;

                    if metadata.is_file() {
                        return Err(RuleValidationError::PathResolutionFailed(format!(
                            "cleanup rule path '{}' resolves to a file, expected directory",
                            current.display()
                        )));
                    }
                }
            }
        }
    }

    match std::fs::canonicalize(&current) {
        Ok(resolved) => {
            if !resolved.starts_with(&canonical_root) {
                return Err(RuleValidationError::PathEscapesCleanupRoot {
                    input: raw_path.to_string(),
                    cleanup_root: canonical_root.display().to_string(),
                });
            }
            Ok(resolved)
        }
        Err(error) if error.kind() == std::io::ErrorKind::NotFound => Ok(current),
        Err(error) => Err(RuleValidationError::PathResolutionFailed(format!(
            "failed to canonicalize '{}': {error}",
            current.display()
        ))),
    }
}

fn canonicalize_or_normalize_path(path: &Path) -> Result<PathBuf, RuleValidationError> {
    match std::fs::canonicalize(path) {
        Ok(resolved) => Ok(resolved),
        Err(error) if error.kind() == std::io::ErrorKind::NotFound => {
            let absolute = if path.is_absolute() {
                path.to_path_buf()
            } else {
                std::env::current_dir()
                    .map_err(|cwd_error| {
                        RuleValidationError::PathResolutionFailed(format!(
                            "failed to resolve current directory: {cwd_error}"
                        ))
                    })?
                    .join(path)
            };
            Ok(normalize_lexical_path(&absolute))
        }
        Err(error) => Err(RuleValidationError::PathResolutionFailed(format!(
            "failed to canonicalize '{}': {error}",
            path.display()
        ))),
    }
}

fn normalize_lexical_path(path: &Path) -> PathBuf {
    let mut normalized = PathBuf::new();
    for component in path.components() {
        match component {
            Component::CurDir => {}
            Component::ParentDir => {
                normalized.pop();
            }
            other => normalized.push(other.as_os_str()),
        }
    }
    normalized
}

#[cfg(test)]
mod tests {
    use super::{CleanupRule, RuleValidationError, resolve_cleanup_path};
    use std::fs;
    use tempfile::TempDir;

    #[test]
    fn resolve_cleanup_path_rejects_dotdot_escape() {
        let install_root = TempDir::new().unwrap();

        let err = resolve_cleanup_path(install_root.path(), "../outside").unwrap_err();
        assert!(matches!(
            err,
            RuleValidationError::PathEscapesCleanupRoot { .. }
        ));
    }

    #[test]
    fn resolve_cleanup_path_uses_install_root_as_base() {
        let install_root = TempDir::new().unwrap();
        let data_dir = install_root.path().join("data");
        let shared_dir = install_root.path().join("shared");
        fs::create_dir_all(&data_dir).unwrap();
        fs::create_dir_all(&shared_dir).unwrap();

        let resolved = resolve_cleanup_path(install_root.path(), "data").unwrap();
        assert_eq!(resolved, data_dir);

        let shared_resolved = resolve_cleanup_path(install_root.path(), "shared").unwrap();
        assert_eq!(shared_resolved, shared_dir);
    }

    #[cfg(unix)]
    #[test]
    fn resolve_cleanup_path_rejects_symlink_escape() {
        use std::os::unix::fs::symlink;

        let install_root = TempDir::new().unwrap();
        let outside = TempDir::new().unwrap();

        symlink(outside.path(), install_root.path().join("escape")).unwrap();

        let err = match CleanupRule::new(install_root.path(), "escape", None, 24, 0) {
            Ok(_) => panic!("symlinked rule path must be rejected"),
            Err(err) => err,
        };
        assert!(matches!(
            err,
            RuleValidationError::PathEscapesCleanupRoot { .. }
        ));
    }

    #[cfg(unix)]
    #[test]
    fn matches_resolves_symlinked_rule_paths_inside_workspace() {
        use std::os::unix::fs::symlink;

        let install_root = TempDir::new().unwrap();
        let actual = install_root.path().join("shared");
        let rule_file = actual.join("cleanup.log");

        fs::create_dir_all(&actual).unwrap();
        fs::write(&rule_file, "cleanup").unwrap();
        symlink(&actual, install_root.path().join("shared-link")).unwrap();

        let rule =
            CleanupRule::new(install_root.path(), "shared-link", Some("*.log"), 24, 0).unwrap();
        assert!(rule.matches(
            install_root.path(),
            &install_root.path().join("shared-link/cleanup.log")
        ));
    }

    #[cfg(unix)]
    #[test]
    fn matches_rejects_symlinked_files_that_escape_workspace() {
        use std::os::unix::fs::symlink;

        let install_root = TempDir::new().unwrap();
        let outside = TempDir::new().unwrap();

        fs::write(outside.path().join("secret.txt"), "secret").unwrap();
        symlink(
            outside.path().join("secret.txt"),
            install_root.path().join("escape.txt"),
        )
        .unwrap();

        let rule = CleanupRule::new(install_root.path(), ".", Some("*.txt"), 24, 0).unwrap();
        assert!(!rule.matches(install_root.path(), &install_root.path().join("escape.txt")));
    }
}
