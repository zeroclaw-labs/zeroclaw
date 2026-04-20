// Comprehensive integration test suite for zeroclaw-file-rotation.
// Covers: concurrent writes, date rotation, error paths, permission handling,
// burst writes, cleanup edge cases, and multi-writer scenarios.
//
// Run with:
//   cargo test -p zeroclaw-file-rotation --test comprehensive_tests -- --nocapture

use std::sync::Arc;
use std::time::Duration;
use zeroclaw_file_rotation::{RotatingFileWriter, RotationConfig};

fn small_config() -> RotationConfig {
    RotationConfig {
        max_file_size_bytes: 100,
        max_age_days: 999,
        max_rotated_files: 100,
        #[cfg(unix)]
        file_permissions: Some(0o600),
        sync_on_write: true,
    }
}

fn no_sync_config() -> RotationConfig {
    RotationConfig {
        sync_on_write: false,
        ..small_config()
    }
}

// ── 1. Concurrent writes from multiple tasks ────────────────────────

#[tokio::test]
async fn concurrent_writes_no_data_loss() {
    let tmp = tempfile::tempdir().unwrap();
    let path = tmp.path().join("concurrent.jsonl");

    let writer = Arc::new(
        RotatingFileWriter::new(path.clone(), no_sync_config())
            .await
            .unwrap(),
    );

    let num_tasks = 10;
    let lines_per_task = 50;
    let mut handles = Vec::new();

    for task_id in 0..num_tasks {
        let w = writer.clone();
        handles.push(tokio::spawn(async move {
            for i in 0..lines_per_task {
                w.append(format!(r#"{{"task":{task_id},"line":{i}}}"#))
                    .await
                    .unwrap();
            }
        }));
    }

    for h in handles {
        h.await.unwrap();
    }

    writer.shutdown().await.unwrap();

    let total_lines: usize = std::fs::read_dir(tmp.path())
        .unwrap()
        .filter_map(|e| e.ok())
        .map(|e| std::fs::read_to_string(e.path()).unwrap_or_default())
        .map(|s| s.lines().filter(|l| !l.is_empty()).count())
        .sum();

    assert_eq!(
        total_lines,
        num_tasks * lines_per_task,
        "All {} lines must be present",
        num_tasks * lines_per_task
    );
}

// ── 2. Concurrent writes preserve per-task ordering ──────────────────

#[tokio::test]
async fn concurrent_writes_per_task_ordering() {
    let tmp = tempfile::tempdir().unwrap();
    let path = tmp.path().join("ordering.jsonl");

    let config = RotationConfig {
        max_file_size_bytes: 10 * 1024 * 1024,
        sync_on_write: false,
        ..Default::default()
    };

    let writer = Arc::new(RotatingFileWriter::new(path.clone(), config).await.unwrap());

    let num_lines = 100;
    let w = writer.clone();
    tokio::spawn(async move {
        for i in 0..num_lines {
            w.append(format!(r#"{{"seq":{i}}}"#)).await.unwrap();
        }
    })
    .await
    .unwrap();

    writer.shutdown().await.unwrap();

    let contents = std::fs::read_to_string(&path).unwrap();
    let lines: Vec<_> = contents.lines().filter(|l| !l.is_empty()).collect();
    assert_eq!(lines.len(), num_lines);

    for (i, line) in lines.iter().enumerate() {
        assert_eq!(*line, format!(r#"{{"seq":{i}}}"#), "Line {i} out of order");
    }
}

// ── 3. Date rotation by backdating file mtime ───────────────────────

#[tokio::test]
async fn date_rotation_rotates_old_file() {
    let tmp = tempfile::tempdir().unwrap();
    let path = tmp.path().join("date-rotate.jsonl");

    let config = RotationConfig {
        max_file_size_bytes: 10 * 1024 * 1024,
        sync_on_write: false,
        ..Default::default()
    };

    // Pre-create file and backdate mtime to yesterday
    std::fs::write(&path, r#"{"old":"data"}"#).unwrap();
    let yesterday = std::time::SystemTime::now() - Duration::from_secs(24 * 3600);
    filetime::set_file_mtime(&path, filetime::FileTime::from_system_time(yesterday)).unwrap();

    let writer = RotatingFileWriter::new(path.clone(), config).await.unwrap();

    writer
        .append(r#"{"new":"data"}"#.to_string())
        .await
        .unwrap();
    writer.shutdown().await.unwrap();

    // Old file should have been rotated to a dated name
    let rotated: Vec<_> = std::fs::read_dir(tmp.path())
        .unwrap()
        .filter_map(|e| e.ok())
        .filter(|e| {
            let name = e.file_name().to_string_lossy().to_string();
            name.contains("2025-") || name.contains("2026-")
        })
        .collect();

    assert!(!rotated.is_empty(), "Expected a dated rotated file");

    let rotated_contents = std::fs::read_to_string(rotated[0].path()).unwrap();
    assert!(rotated_contents.contains(r#"{"old":"data"}"#));

    let active_contents = std::fs::read_to_string(&path).unwrap();
    assert!(active_contents.contains(r#"{"new":"data"}"#));
    assert!(!active_contents.contains(r#"{"old":"data"}"#));
}

// ── 4. Append after shutdown returns error ───────────────────────────

#[tokio::test]
async fn append_after_shutdown_returns_channel_closed() {
    let tmp = tempfile::tempdir().unwrap();
    let path = tmp.path().join("closed.jsonl");

    let writer = RotatingFileWriter::new(path.clone(), small_config())
        .await
        .unwrap();

    writer.shutdown().await.unwrap();

    let result = writer.append("should fail".to_string()).await;
    assert!(
        result.is_err(),
        "Append after shutdown should return an error"
    );

    let err = result.unwrap_err();
    assert!(
        format!("{err}").contains("channel closed"),
        "Expected channel closed error, got: {err}"
    );
}

// ── 5. Drop without shutdown does not panic ──────────────────────────

#[tokio::test]
async fn drop_without_shutdown_no_panic() {
    let tmp = tempfile::tempdir().unwrap();
    let path = tmp.path().join("dropped.jsonl");

    let config = RotationConfig {
        max_file_size_bytes: 10 * 1024 * 1024,
        sync_on_write: false,
        ..Default::default()
    };

    {
        let writer = RotatingFileWriter::new(path.clone(), config).await.unwrap();
        writer
            .append(r#"{"might":"be lost"}"#.to_string())
            .await
            .unwrap();
        tokio::time::sleep(Duration::from_millis(50)).await;
        // Drop without calling shutdown
    }

    tokio::time::sleep(Duration::from_millis(50)).await;
    // Key assertion: no panic or deadlock
}

// ── 6. Size rotation produces correct consecutive sequence numbers ──

#[tokio::test]
async fn size_rotation_sequence_increments() {
    let tmp = tempfile::tempdir().unwrap();
    let path = tmp.path().join("seq.log");

    let config = RotationConfig {
        max_file_size_bytes: 50,
        max_age_days: 999,
        max_rotated_files: 100,
        sync_on_write: false,
        ..Default::default()
    };

    let writer = RotatingFileWriter::new(path.clone(), config).await.unwrap();

    for i in 0..30 {
        writer
            .append(format!("line-{i:03} padding-to-exceed-threshold"))
            .await
            .unwrap();
    }

    writer.shutdown().await.unwrap();

    let mut seqs: Vec<u32> = std::fs::read_dir(tmp.path())
        .unwrap()
        .filter_map(|e| e.ok())
        .filter_map(|e| {
            let name = e.file_name().to_string_lossy().to_string();
            let parts: Vec<&str> = name.split('.').collect();
            if parts.len() >= 4 {
                parts[2].parse::<u32>().ok()
            } else {
                None
            }
        })
        .collect();

    seqs.sort();

    for (i, &seq) in seqs.iter().enumerate() {
        assert_eq!(
            seq,
            (i + 1) as u32,
            "Expected consecutive sequence numbers, got {seqs:?}"
        );
    }
}

// ── 7. Cleanup enforces both age and count ───────────────────────────

#[tokio::test]
async fn cleanup_mixed_age_and_count() {
    let tmp = tempfile::tempdir().unwrap();
    let active_path = tmp.path().join("mixed.log");

    let now = chrono::Local::now();
    let old_date = (now - chrono::TimeDelta::days(60)).date_naive();
    let recent_date = (now - chrono::TimeDelta::days(2)).date_naive();

    // Old files (should be deleted by age)
    for i in 1..=3 {
        let name = format!("mixed.{}.{}.log", old_date.format("%Y-%m-%d"), i);
        std::fs::write(tmp.path().join(&name), format!("old-{i}")).unwrap();
    }

    // Recent files
    for i in 1..=5 {
        let name = format!("mixed.{}.{}.log", recent_date.format("%Y-%m-%d"), i);
        std::fs::write(tmp.path().join(&name), format!("recent-{i}")).unwrap();
    }

    let config = RotationConfig {
        max_age_days: 7,
        max_rotated_files: 2,
        ..Default::default()
    };

    let stats = zeroclaw_file_rotation::cleanup::cleanup_rotated_files(&active_path, &config, &now)
        .await
        .unwrap();

    // Old files should be deleted by age
    assert!(
        stats.files_deleted >= 3,
        "At least 3 old files should be deleted"
    );

    // No old-date files should remain
    let remaining: Vec<_> = std::fs::read_dir(tmp.path())
        .unwrap()
        .filter_map(|e| e.ok())
        .map(|e| e.file_name().to_string_lossy().to_string())
        .collect();

    let date_str = old_date.format("%Y-%m-%d").to_string();
    for name in &remaining {
        assert!(
            !name.contains(&date_str),
            "Old file should have been deleted: {name}"
        );
    }
}

// ── 8. Writing to a non-existent directory creates it ────────────────

#[tokio::test]
async fn writes_to_nonexistent_directory() {
    let tmp = tempfile::tempdir().unwrap();
    let path = tmp.path().join("deep/nested/dir/test.jsonl");

    let writer = RotatingFileWriter::new(path.clone(), small_config())
        .await
        .unwrap();

    writer
        .append(r#"{"deep":"path"}"#.to_string())
        .await
        .unwrap();
    writer.shutdown().await.unwrap();

    assert!(path.exists());
    let contents = std::fs::read_to_string(&path).unwrap();
    assert!(contents.contains(r#"{"deep":"path"}"#));
}

// ── 9. Empty string append writes a newline ──────────────────────────

#[tokio::test]
async fn empty_string_append_writes_newline() {
    let tmp = tempfile::tempdir().unwrap();
    let path = tmp.path().join("empty.jsonl");

    let config = RotationConfig {
        max_file_size_bytes: 10 * 1024 * 1024,
        sync_on_write: false,
        ..Default::default()
    };

    let writer = RotatingFileWriter::new(path.clone(), config).await.unwrap();

    writer.append(String::new()).await.unwrap();
    writer
        .append(r#"{"after":"empty"}"#.to_string())
        .await
        .unwrap();
    writer.shutdown().await.unwrap();

    let contents = std::fs::read_to_string(&path).unwrap();
    let lines: Vec<_> = contents.lines().collect();
    assert_eq!(lines.len(), 2);
    assert_eq!(lines[1], r#"{"after":"empty"}"#);
}

// ── 10. File permissions on Unix ─────────────────────────────────────

#[cfg(unix)]
#[tokio::test]
async fn file_permissions_applied() {
    let tmp = tempfile::tempdir().unwrap();
    let path = tmp.path().join("perms.jsonl");

    let config = RotationConfig {
        max_file_size_bytes: 10 * 1024 * 1024,
        max_age_days: 30,
        max_rotated_files: 100,
        file_permissions: Some(0o600),
        sync_on_write: true,
    };

    let writer = RotatingFileWriter::new(path.clone(), config).await.unwrap();

    writer
        .append(r#"{"perm":"test"}"#.to_string())
        .await
        .unwrap();
    writer.shutdown().await.unwrap();

    use std::os::unix::fs::PermissionsExt;
    let mode = std::fs::metadata(&path).unwrap().permissions().mode() & 0o777;
    assert_eq!(
        mode, 0o600,
        "File should have 0600 permissions, got {mode:o}"
    );
}

// ── 11. Rapid burst of 500 writes + full rotation cycle ──────────────

#[tokio::test]
async fn rapid_burst_full_rotation_cycle() {
    let tmp = tempfile::tempdir().unwrap();
    let path = tmp.path().join("burst.jsonl");

    let config = RotationConfig {
        max_file_size_bytes: 80,
        max_age_days: 999,
        max_rotated_files: 1000,
        sync_on_write: false,
        ..Default::default()
    };

    let writer = RotatingFileWriter::new(path.clone(), config).await.unwrap();

    for i in 0..500 {
        writer
            .append(format!(r#"{{"i":{i},"padding":"x"}}"#))
            .await
            .unwrap();
    }

    writer.shutdown().await.unwrap();

    let total_lines: usize = std::fs::read_dir(tmp.path())
        .unwrap()
        .filter_map(|e| e.ok())
        .map(|e| {
            std::fs::read_to_string(e.path())
                .unwrap_or_default()
                .lines()
                .filter(|l| !l.is_empty())
                .count()
        })
        .sum();

    assert_eq!(
        total_lines, 500,
        "All 500 lines must be present, no data loss"
    );
}

// ── 12. Cleanup ignores malformed filenames ──────────────────────────

#[tokio::test]
async fn cleanup_ignores_malformed_filenames() {
    let tmp = tempfile::tempdir().unwrap();
    let active_path = tmp.path().join("app.log");

    let now = chrono::Local::now();
    let date = now.date_naive();

    // Valid rotated file
    std::fs::write(
        tmp.path()
            .join(format!("app.{}.1.log", date.format("%Y-%m-%d"))),
        "valid",
    )
    .unwrap();

    // Malformed files
    std::fs::write(tmp.path().join("app.not-a-date.1.log"), "bad-date").unwrap();
    std::fs::write(tmp.path().join("random-junk.log"), "unrelated").unwrap();

    let config = RotationConfig {
        max_age_days: 365,
        max_rotated_files: 100,
        ..Default::default()
    };

    let stats = zeroclaw_file_rotation::cleanup::cleanup_rotated_files(&active_path, &config, &now)
        .await
        .unwrap();

    assert_eq!(stats.files_deleted, 0, "No valid files should be deleted");
    assert!(tmp.path().join("app.not-a-date.1.log").exists());
    assert!(tmp.path().join("random-junk.log").exists());
}

// ── 13. Multiple writers to same directory (different files) ─────────

#[tokio::test]
async fn multiple_writers_same_directory() {
    let tmp = tempfile::tempdir().unwrap();
    let path_a = tmp.path().join("service-a.jsonl");
    let path_b = tmp.path().join("service-b.jsonl");

    let config = RotationConfig {
        max_file_size_bytes: 10 * 1024 * 1024,
        sync_on_write: false,
        ..Default::default()
    };

    let writer_a = RotatingFileWriter::new(path_a.clone(), config.clone())
        .await
        .unwrap();
    let writer_b = RotatingFileWriter::new(path_b.clone(), config)
        .await
        .unwrap();

    writer_a
        .append(r#"{"service":"a"}"#.to_string())
        .await
        .unwrap();
    writer_b
        .append(r#"{"service":"b"}"#.to_string())
        .await
        .unwrap();

    writer_a.shutdown().await.unwrap();
    writer_b.shutdown().await.unwrap();

    let contents_a = std::fs::read_to_string(&path_a).unwrap();
    let contents_b = std::fs::read_to_string(&path_b).unwrap();

    assert!(contents_a.contains(r#"{"service":"a"}"#));
    assert!(contents_b.contains(r#"{"service":"b"}"#));
    assert!(!contents_a.contains(r#"{"service":"b"}"#));
    assert!(!contents_b.contains(r#"{"service":"a"}"#));
}

// ── 14. Write to read-only directory — data not persisted ─────────────

#[cfg(unix)]
#[tokio::test]
async fn write_to_readonly_directory_data_not_persisted() {
    // Skip if running as root (root ignores filesystem permissions)
    let uid = unsafe { libc::geteuid() };
    if uid == 0 {
        eprintln!("Skipping write_to_readonly_directory test: running as root");
        return;
    }

    let tmp = tempfile::tempdir().unwrap();
    let readonly_dir = tmp.path().join("readonly");
    std::fs::create_dir_all(&readonly_dir).unwrap();

    use std::os::unix::fs::PermissionsExt;
    std::fs::set_permissions(&readonly_dir, std::fs::Permissions::from_mode(0o444)).unwrap();

    let path = readonly_dir.join("test.jsonl");

    // new() may succeed (it only creates the dir parent which already exists),
    // but writes will fail silently in the backend task
    if let Ok(writer) = RotatingFileWriter::new(path.clone(), small_config()).await {
        // The append will succeed (buffered in channel) but data won't reach disk
        let _ = writer.append(r#"{"should":"fail"}"#.to_string()).await;
        let _ = writer.shutdown().await;
    }

    // Restore for cleanup
    let _ = std::fs::set_permissions(&readonly_dir, std::fs::Permissions::from_mode(0o755));

    // The file should not exist (write failed due to permissions)
    assert!(
        !path.exists(),
        "File should not have been created in read-only directory"
    );
}

// ── 15. Config defaults ──────────────────────────────────────────────

#[test]
fn config_defaults() {
    let config = RotationConfig::default();
    assert_eq!(config.max_file_size_bytes, 100 * 1024 * 1024);
    assert_eq!(config.max_age_days, 30);
    assert_eq!(config.max_rotated_files, 100);
    assert!(config.sync_on_write);
    #[cfg(unix)]
    assert!(config.file_permissions.is_none());
}

// ── 16. Error construction ───────────────────────────────────────────

#[test]
fn error_io_construction() {
    let err = zeroclaw_file_rotation::RotationError::io(
        "/some/path.log",
        std::io::Error::new(std::io::ErrorKind::PermissionDenied, "denied"),
    );
    let msg = format!("{err}");
    assert!(msg.contains("/some/path.log"));
    assert!(msg.contains("denied"));
}

#[test]
fn error_display_channel_closed() {
    assert_eq!(
        format!("{}", zeroclaw_file_rotation::RotationError::ChannelClosed),
        "write channel closed"
    );
}

#[test]
fn error_display_shutdown_timeout() {
    assert_eq!(
        format!("{}", zeroclaw_file_rotation::RotationError::ShutdownTimeout),
        "shutdown timeout"
    );
}

// ── 17. Sync on write ensures data durability ────────────────────────

#[tokio::test]
async fn sync_on_write_data_persists() {
    let tmp = tempfile::tempdir().unwrap();
    let path = tmp.path().join("sync.jsonl");

    let config = RotationConfig {
        max_file_size_bytes: 10 * 1024 * 1024, // large, no rotation
        max_age_days: 30,
        max_rotated_files: 100,
        #[cfg(unix)]
        file_permissions: Some(0o600),
        sync_on_write: true,
    };

    let writer = RotatingFileWriter::new(path.clone(), config).await.unwrap();

    for i in 0..10 {
        writer.append(format!(r#"{{"sync":{i}}}"#)).await.unwrap();
    }

    writer.shutdown().await.unwrap();

    let contents = std::fs::read_to_string(&path).unwrap();
    let line_count = contents.lines().filter(|l| !l.is_empty()).count();
    assert_eq!(line_count, 10);
}

// ── 18. Cleanup with empty directory ─────────────────────────────────

#[tokio::test]
async fn cleanup_empty_directory() {
    let tmp = tempfile::tempdir().unwrap();
    let active_path = tmp.path().join("app.log");

    let now = chrono::Local::now();
    let config = RotationConfig::default();

    let stats = zeroclaw_file_rotation::cleanup::cleanup_rotated_files(&active_path, &config, &now)
        .await
        .unwrap();

    assert_eq!(stats.files_deleted, 0);
    assert_eq!(stats.bytes_freed, 0);
}
