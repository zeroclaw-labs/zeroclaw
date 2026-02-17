use anyhow::{Context, Result};
use std::path::{Path, PathBuf};
use tracing::info;

const WORKSPACE_VERSION_FILE: &str = ".zeroclaw-version";

/// Workspace format version written by the current release.
const CURRENT_WORKSPACE_VERSION: &str = env!("CARGO_PKG_VERSION");

/// A workspace migration step.
struct Migration {
    from: semver::Version,
    to: semver::Version,
    description: &'static str,
    apply: fn(&Path) -> Result<()>,
}

/// Registry of all known workspace migrations. Add new entries here as needed.
/// Migrations are applied in order, skipping any whose `from` version is
/// below the workspace's current format stamp.
const MIGRATIONS: &[Migration] = &[
    // Example entry (uncomment and adapt when the first real migration is needed):
    //
    // Migration {
    //     from: semver::Version::new(0, 1, 0),
    //     to: semver::Version::new(0, 2, 0),
    //     description: "Add FTS5 search index to brain.db",
    //     apply: migrate_0_1_to_0_2,
    // },
];

/// Run any pending workspace migrations for the given workspace directory.
/// This is idempotent — re-running is safe.
pub fn run_pending_migrations(workspace_dir: &Path) -> Result<()> {
    if MIGRATIONS.is_empty() {
        // No migrations registered; stamp current version and return.
        stamp_version(workspace_dir)?;
        return Ok(());
    }

    let current = read_workspace_version(workspace_dir);

    let pending: Vec<&Migration> = MIGRATIONS
        .iter()
        .filter(|m| m.from >= current)
        .collect();

    if pending.is_empty() {
        stamp_version(workspace_dir)?;
        return Ok(());
    }

    // Backup before migrating
    backup_workspace(workspace_dir)?;

    for migration in &pending {
        info!(
            "Applying workspace migration: {} (v{} → v{})",
            migration.description, migration.from, migration.to
        );
        (migration.apply)(workspace_dir)
            .with_context(|| format!("Migration failed: {}", migration.description))?;
    }

    stamp_version(workspace_dir)?;

    let backup_dir = last_backup_dir(workspace_dir);
    eprintln!(
        "📦 Workspace migrated — backup saved to: {}",
        backup_dir.display()
    );

    Ok(())
}

/// Read the workspace format version from the marker file.
/// Returns `0.0.0` if the file doesn't exist (pre-versioning era).
fn read_workspace_version(workspace_dir: &Path) -> semver::Version {
    let path = workspace_dir.join(WORKSPACE_VERSION_FILE);
    std::fs::read_to_string(&path)
        .ok()
        .and_then(|s| semver::Version::parse(s.trim()).ok())
        .unwrap_or(semver::Version::new(0, 0, 0))
}

/// Write the current workspace format version to the marker file.
fn stamp_version(workspace_dir: &Path) -> Result<()> {
    let path = workspace_dir.join(WORKSPACE_VERSION_FILE);
    if workspace_dir.exists() {
        std::fs::write(&path, CURRENT_WORKSPACE_VERSION)
            .context("Failed to write workspace version stamp")?;
    }
    Ok(())
}

/// Create a timestamped backup of critical workspace files.
fn backup_workspace(workspace_dir: &Path) -> Result<()> {
    let backup_dir = last_backup_dir(workspace_dir);
    std::fs::create_dir_all(&backup_dir)?;

    let critical_files = [
        "memory/brain.db",
        "memory/response_cache.db",
        "cron/jobs.db",
        "MEMORY.md",
    ];

    for rel_path in &critical_files {
        let src = workspace_dir.join(rel_path);
        if src.exists() {
            let dest = backup_dir.join(rel_path);
            if let Some(parent) = dest.parent() {
                std::fs::create_dir_all(parent)?;
            }
            std::fs::copy(&src, &dest)?;
        }
    }

    // Also backup config.toml from the parent (config dir)
    if let Some(config_dir) = workspace_dir.parent() {
        let config_src = config_dir.join("config.toml");
        if config_src.exists() {
            std::fs::copy(&config_src, backup_dir.join("config.toml"))?;
        }
    }

    Ok(())
}

/// Compute the backup directory path based on current timestamp.
fn last_backup_dir(workspace_dir: &Path) -> PathBuf {
    let timestamp = chrono::Utc::now().format("%Y%m%d-%H%M%S");
    workspace_dir
        .parent()
        .unwrap_or(workspace_dir)
        .join(format!("workspace-backup-{timestamp}"))
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn read_workspace_version_missing_file_returns_zero() {
        let dir = tempfile::tempdir().unwrap();
        let version = read_workspace_version(dir.path());
        assert_eq!(version, semver::Version::new(0, 0, 0));
    }

    #[test]
    fn stamp_and_read_workspace_version_roundtrip() {
        let dir = tempfile::tempdir().unwrap();
        stamp_version(dir.path()).unwrap();
        let version = read_workspace_version(dir.path());
        let expected = semver::Version::parse(env!("CARGO_PKG_VERSION")).unwrap();
        assert_eq!(version, expected);
    }

    #[test]
    fn no_pending_migrations_when_registry_empty() {
        // MIGRATIONS is empty, so run_pending_migrations should succeed trivially
        let dir = tempfile::tempdir().unwrap();
        let result = run_pending_migrations(dir.path());
        assert!(result.is_ok());
    }
}
