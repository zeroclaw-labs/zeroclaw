pub mod openclaw;
pub mod report;

use crate::db::Registry;
use anyhow::{bail, Context, Result};
use std::path::{Path, PathBuf};

use self::openclaw::Manifest;

/// Acquire the exclusive migration lock (flock-based).
/// Returns the lock file handle (holds lock until dropped).
pub fn acquire_migration_lock(cp_dir: &Path) -> Result<std::fs::File> {
    use std::os::unix::io::AsRawFd;

    let lock_path = cp_dir.join("migration.lock");
    let lock = std::fs::OpenOptions::new()
        .create(true)
        .write(true)
        .open(&lock_path)
        .with_context(|| format!("Failed to open migration lock: {}", lock_path.display()))?;

    let ret = unsafe { libc::flock(lock.as_raw_fd(), libc::LOCK_EX | libc::LOCK_NB) };
    if ret != 0 {
        bail!("Lock held by another process (server running or migration in progress)");
    }

    Ok(lock)
}

/// Reconcile pending/done migration manifests.
/// Assumes caller holds the migration lock.
/// Returns Ok(true) if all reconciliation succeeded (safe to proceed).
/// Returns Ok(false) if any pending manifests remain unresolved.
pub fn reconcile_inner(
    cp_dir: &Path,
    registry: &Registry,
    instances_dir: &Path,
) -> Result<bool> {
    // Phase 1: clean done bookmarks
    for entry in glob_manifests(cp_dir, "migration-done-") {
        if let Err(e) = std::fs::remove_file(&entry) {
            tracing::warn!("Failed to remove done manifest {}: {e}", entry.display());
        }
    }

    // Phase 2: roll back pending manifests
    let mut all_resolved = true;
    for entry in glob_manifests(cp_dir, "migration-pending-") {
        tracing::warn!("Incomplete migration found: {}", entry.display());
        match process_pending_manifest(&entry, registry, instances_dir, cp_dir) {
            Ok(n) => tracing::info!("Rolled back {n} instances from incomplete migration"),
            Err(e) => {
                tracing::error!("Reconciliation error for {}: {e}", entry.display());
                all_resolved = false;
            }
        }
        if entry.exists() {
            all_resolved = false;
        }
    }

    Ok(all_resolved)
}

fn glob_manifests(cp_dir: &Path, prefix: &str) -> Vec<PathBuf> {
    let mut results = Vec::new();
    let entries = match std::fs::read_dir(cp_dir) {
        Ok(e) => e,
        Err(_) => return results,
    };

    for entry in entries.flatten() {
        let name = entry.file_name();
        let name_str = name.to_string_lossy();
        if name_str.starts_with(prefix) && name_str.ends_with(".json") {
            results.push(entry.path());
        }
    }
    results
}

fn process_pending_manifest(
    path: &Path,
    registry: &Registry,
    instances_dir: &Path,
    cp_dir: &Path,
) -> Result<usize> {
    let raw = std::fs::read_to_string(path)?;
    let manifest: Manifest = serde_json::from_str(&raw)?;
    let mut cleaned = 0;

    // Validate instances_dir matches
    let manifest_idir = std::fs::canonicalize(&manifest.instances_dir)
        .unwrap_or_else(|_| PathBuf::from(&manifest.instances_dir));
    let actual_idir =
        std::fs::canonicalize(instances_dir).unwrap_or_else(|_| instances_dir.to_path_buf());

    if manifest_idir != actual_idir {
        tracing::error!(
            "Manifest instances_dir ({}) differs from current ({}). Moving to quarantine.",
            manifest_idir.display(),
            actual_idir.display()
        );
        quarantine_manifest(path, cp_dir);
        anyhow::bail!(
            "Manifest instances_dir mismatch (quarantined for manual inspection)"
        );
    }

    let run_id = &manifest.run_id;
    let mut any_failures = false;

    for id in &manifest.instance_ids {
        // Validate: must be a bare name
        if id.is_empty() || id == "." || id.contains('/') || id.contains("..") {
            tracing::error!("Invalid instance ID in manifest, skipping: {id}");
            continue;
        }

        // Phase A: DB cleanup (scoped by migration_run_id)
        let db_deleted = match registry.delete_instance_if_migration(id, run_id) {
            Ok(true) => {
                cleaned += 1;
                true
            }
            Ok(false) => false,
            Err(e) => {
                tracing::error!("DB cleanup failed for {id}: {e}");
                any_failures = true;
                continue;
            }
        };

        // Phase B: FS cleanup
        let dir = instances_dir.join(id);
        if dir.exists() {
            let should_delete = if db_deleted {
                true
            } else {
                match registry.get_instance(id) {
                    Ok(None) => true,
                    Ok(Some(_)) => {
                        tracing::warn!(
                            "Skipping FS cleanup for {id}: DB row exists with different migration_run_id"
                        );
                        false
                    }
                    Err(e) => {
                        tracing::error!("DB lookup failed for {id}, skipping FS cleanup: {e}");
                        any_failures = true;
                        false
                    }
                }
            };
            if should_delete {
                // Final guard: marker file must exist
                if !dir.join(".migration-created").exists() {
                    tracing::warn!(
                        "Skipping FS cleanup for {id}: no .migration-created marker \
                         (crash during commit? preserving manifest for retry)"
                    );
                    any_failures = true;
                    continue;
                }
                if let Err(e) = std::fs::remove_dir_all(&dir) {
                    tracing::error!("FS cleanup failed for {}: {e}", dir.display());
                    any_failures = true;
                }
            }
        }
    }

    if any_failures {
        tracing::warn!(
            "Manifest preserved due to cleanup failures (will retry on next startup): {}",
            path.display()
        );
    } else {
        std::fs::remove_file(path)?;
    }

    Ok(cleaned)
}

fn quarantine_manifest(path: &Path, cp_dir: &Path) {
    let quarantine = cp_dir.join("quarantine");
    let _ = std::fs::create_dir_all(&quarantine);
    let fname = path
        .file_name()
        .unwrap_or_default()
        .to_string_lossy();
    let ts = chrono::Utc::now().format("%Y%m%dT%H%M%S%.3f");
    let suffix = &uuid::Uuid::new_v4().to_string()[..8];
    let dest = quarantine.join(format!("{fname}.{ts}-{suffix}"));
    if let Err(e) = std::fs::rename(path, &dest) {
        tracing::error!("Failed to quarantine manifest: {e}. Left in place.");
    }
}
