use anyhow::Result;
use std::fs;
use std::path::Path;
use tempfile::TempDir;
use zeroclaw::db::Registry;
use zeroclaw::migrate;
use zeroclaw::migrate::openclaw::Manifest;

/// Helper: create a pending manifest JSON file.
fn write_pending_manifest(
    cp_dir: &Path,
    run_id: &str,
    instance_ids: &[&str],
    instances_dir: &Path,
) -> std::path::PathBuf {
    let path = cp_dir.join(format!("migration-pending-{run_id}.json"));
    let manifest = Manifest {
        run_id: run_id.to_string(),
        pid: std::process::id(),
        started_at: chrono::Utc::now().to_rfc3339(),
        source_path: "/test/openclaw.json".to_string(),
        instances_dir: instances_dir.to_string_lossy().to_string(),
        instance_ids: instance_ids.iter().map(|s| s.to_string()).collect(),
    };
    fs::write(&path, serde_json::to_string_pretty(&manifest).unwrap()).unwrap();
    path
}

/// Helper: create an instance directory with the .migration-created marker.
fn create_instance_dir_with_marker(instances_dir: &Path, id: &str) {
    let dir = instances_dir.join(id);
    fs::create_dir_all(&dir).unwrap();
    fs::write(dir.join(".migration-created"), "").unwrap();
}

/// Helper: create an instance directory WITHOUT the marker.
fn create_instance_dir_no_marker(instances_dir: &Path, id: &str) {
    let dir = instances_dir.join(id);
    fs::create_dir_all(&dir).unwrap();
}

#[test]
fn crash_recovery_scoped_by_run_id() -> Result<()> {
    let tmp = TempDir::new()?;
    let cp_dir = tmp.path().join("cp");
    let instances_dir = cp_dir.join("instances");
    fs::create_dir_all(&instances_dir)?;

    let registry = Registry::open(&cp_dir.join("registry.db"))?;

    // Write pending manifest for run_id="test-run" with two instances
    write_pending_manifest(&cp_dir, "test-run", &["id-1", "id-2"], &instances_dir);

    // id-1: matching run_id, has marker
    create_instance_dir_with_marker(&instances_dir, "id-1");
    registry.create_instance(
        "id-1",
        "agent-1",
        18801,
        "/config.toml",
        None,
        Some("test-run"),
    )?;

    // id-2: WRONG run_id, has marker
    create_instance_dir_with_marker(&instances_dir, "id-2");
    registry.create_instance(
        "id-2",
        "agent-2",
        18802,
        "/config.toml",
        None,
        Some("wrong-run"),
    )?;

    // Acquire lock and reconcile
    let _lock = migrate::acquire_migration_lock(&cp_dir)?;
    let result = migrate::reconcile_inner(&cp_dir, &registry, &instances_dir)?;

    // id-1 should be removed (run_id matched)
    assert!(registry.get_instance("id-1")?.is_none(), "id-1 DB row should be deleted");
    assert!(!instances_dir.join("id-1").exists(), "id-1 FS dir should be deleted");

    // id-2 should still exist (run_id mismatch: DB row kept, FS kept)
    assert!(registry.get_instance("id-2")?.is_some(), "id-2 DB row should exist");
    assert!(instances_dir.join("id-2").exists(), "id-2 FS dir should exist");

    // Manifest should be deleted (all actionable cleanups succeeded)
    let pending_files: Vec<_> = fs::read_dir(&cp_dir)?
        .filter_map(|e| e.ok())
        .filter(|e| {
            e.file_name()
                .to_string_lossy()
                .starts_with("migration-pending-")
        })
        .collect();
    assert!(
        pending_files.is_empty(),
        "Manifest should be deleted after successful reconciliation"
    );

    assert!(result, "reconcile_inner should return true (all resolved)");
    Ok(())
}

#[test]
fn crash_recovery_retry_orphaned_dir() -> Result<()> {
    let tmp = TempDir::new()?;
    let cp_dir = tmp.path().join("cp");
    let instances_dir = cp_dir.join("instances");
    fs::create_dir_all(&instances_dir)?;

    let registry = Registry::open(&cp_dir.join("registry.db"))?;

    // Write pending manifest
    write_pending_manifest(&cp_dir, "test-run", &["id-1"], &instances_dir);

    // id-1: FS dir with marker but NO DB record (simulates prior DB cleanup + crash before FS)
    create_instance_dir_with_marker(&instances_dir, "id-1");

    let _lock = migrate::acquire_migration_lock(&cp_dir)?;
    let result = migrate::reconcile_inner(&cp_dir, &registry, &instances_dir)?;

    // id-1 FS should be deleted (orphaned, no DB row, marker present)
    assert!(
        !instances_dir.join("id-1").exists(),
        "Orphaned dir should be cleaned up on retry"
    );

    // Manifest should be deleted
    let pending_files: Vec<_> = fs::read_dir(&cp_dir)?
        .filter_map(|e| e.ok())
        .filter(|e| {
            e.file_name()
                .to_string_lossy()
                .starts_with("migration-pending-")
        })
        .collect();
    assert!(pending_files.is_empty(), "Manifest should be deleted after retry success");
    assert!(result);
    Ok(())
}

#[test]
fn crash_recovery_marker_missing_preserves_manifest() -> Result<()> {
    let tmp = TempDir::new()?;
    let cp_dir = tmp.path().join("cp");
    let instances_dir = cp_dir.join("instances");
    fs::create_dir_all(&instances_dir)?;

    let registry = Registry::open(&cp_dir.join("registry.db"))?;

    // Write pending manifest
    write_pending_manifest(&cp_dir, "test-run", &["id-1"], &instances_dir);

    // id-1: FS dir WITHOUT marker, no DB record
    create_instance_dir_no_marker(&instances_dir, "id-1");

    let _lock = migrate::acquire_migration_lock(&cp_dir)?;
    let result = migrate::reconcile_inner(&cp_dir, &registry, &instances_dir)?;

    // id-1 FS should still exist (no marker = refuse to delete)
    assert!(
        instances_dir.join("id-1").exists(),
        "Dir without marker should NOT be deleted"
    );

    // Manifest should still exist (missing marker = failure, preserve for retry)
    let pending_files: Vec<_> = fs::read_dir(&cp_dir)?
        .filter_map(|e| e.ok())
        .filter(|e| {
            e.file_name()
                .to_string_lossy()
                .starts_with("migration-pending-")
        })
        .collect();
    assert!(
        !pending_files.is_empty(),
        "Manifest should be preserved when marker is missing"
    );
    assert!(!result, "reconcile_inner should return false (unresolved)");
    Ok(())
}

#[test]
fn manifest_mismatch_quarantined() -> Result<()> {
    let tmp = TempDir::new()?;
    let cp_dir = tmp.path().join("cp");
    let instances_dir = cp_dir.join("instances");
    fs::create_dir_all(&instances_dir)?;

    let registry = Registry::open(&cp_dir.join("registry.db"))?;

    // Write manifest with WRONG instances_dir
    let wrong_dir = tmp.path().join("some_other_path");
    fs::create_dir_all(&wrong_dir)?;
    let path = cp_dir.join("migration-pending-mismatch-run.json");
    let manifest = Manifest {
        run_id: "mismatch-run".to_string(),
        pid: std::process::id(),
        started_at: chrono::Utc::now().to_rfc3339(),
        source_path: "/test/oc.json".to_string(),
        instances_dir: wrong_dir.to_string_lossy().to_string(),
        instance_ids: vec!["id-1".to_string()],
    };
    fs::write(&path, serde_json::to_string_pretty(&manifest).unwrap())?;

    let _lock = migrate::acquire_migration_lock(&cp_dir)?;
    let result = migrate::reconcile_inner(&cp_dir, &registry, &instances_dir)?;

    // Manifest should be moved to quarantine
    assert!(!path.exists(), "Original manifest should be gone");
    let quarantine = cp_dir.join("quarantine");
    assert!(quarantine.exists(), "Quarantine dir should be created");
    let q_files: Vec<_> = fs::read_dir(&quarantine)?
        .filter_map(|e| e.ok())
        .collect();
    assert_eq!(q_files.len(), 1, "One file should be in quarantine");

    assert!(!result, "reconcile_inner should return false (unresolved)");
    Ok(())
}

#[test]
fn done_manifest_cleaned_up() -> Result<()> {
    let tmp = TempDir::new()?;
    let cp_dir = tmp.path().join("cp");
    let instances_dir = cp_dir.join("instances");
    fs::create_dir_all(&instances_dir)?;

    let registry = Registry::open(&cp_dir.join("registry.db"))?;

    // Write a done manifest
    let done_path = cp_dir.join("migration-done-completed-run.json");
    fs::write(&done_path, "{}")?;

    let _lock = migrate::acquire_migration_lock(&cp_dir)?;
    let result = migrate::reconcile_inner(&cp_dir, &registry, &instances_dir)?;

    assert!(!done_path.exists(), "Done manifest should be cleaned up");
    assert!(result, "Should return true when only done manifests exist");
    Ok(())
}
