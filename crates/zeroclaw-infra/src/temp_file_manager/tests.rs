use crate::strategy::{
    CleanupStrategy, FileMeta, SpaceBasedStrategy, StrategyConfig, TimeBasedStrategy,
};
use std::path::{Path, PathBuf};
use std::sync::Arc;
use std::time::{Duration, SystemTime};
use tempfile::TempDir;

#[test]
fn test_time_strategy_finds_expired() {
    let strategy = TimeBasedStrategy;
    let now = SystemTime::now();
    let old_file = FileMeta {
        path: PathBuf::from("/tmp/old.txt"),
        mtime: now - Duration::from_secs(3600 * 25), // 25 hours ago
        size_bytes: 1024,
    };
    let new_file = FileMeta {
        path: PathBuf::from("/tmp/new.txt"),
        mtime: now - Duration::from_secs(3600), // 1 hour ago
        size_bytes: 1024,
    };

    let config = StrategyConfig {
        retention_hours: 24,
        max_size_bytes: 0,
        current_dir_size_bytes: 0,
    };

    let to_delete = strategy.find_files_to_delete(&[old_file, new_file], &config);
    assert_eq!(to_delete.len(), 1);
    assert_eq!(to_delete[0].file_name().unwrap(), "old.txt");
}

#[test]
fn test_time_strategy_skips_recent() {
    let strategy = TimeBasedStrategy;
    let now = SystemTime::now();
    let recent_file = FileMeta {
        path: PathBuf::from("/tmp/recent.txt"),
        mtime: now - Duration::from_secs(3600), // 1 hour ago
        size_bytes: 1024,
    };

    let config = StrategyConfig {
        retention_hours: 24,
        max_size_bytes: 0,
        current_dir_size_bytes: 0,
    };

    let to_delete = strategy.find_files_to_delete(&[recent_file], &config);
    assert!(to_delete.is_empty());
}

#[test]
fn test_space_strategy_prunes_oldest_first() {
    let strategy = SpaceBasedStrategy;
    let now = SystemTime::now();

    // Create files that sum to 15 MB
    let files = vec![
        FileMeta {
            path: PathBuf::from("/tmp/old.txt"),
            mtime: now - Duration::from_secs(3600 * 3), // 3 hours ago
            size_bytes: 5 * 1024 * 1024,                // 5 MB
        },
        FileMeta {
            path: PathBuf::from("/tmp/middle.txt"),
            mtime: now - Duration::from_secs(3600 * 2), // 2 hours ago
            size_bytes: 5 * 1024 * 1024,                // 5 MB
        },
        FileMeta {
            path: PathBuf::from("/tmp/new.txt"),
            mtime: now - Duration::from_secs(3600), // 1 hour ago
            size_bytes: 5 * 1024 * 1024,            // 5 MB
        },
    ];

    let config = StrategyConfig {
        retention_hours: 0,
        max_size_bytes: 10 * 1024 * 1024,         // Limit to 10 MB
        current_dir_size_bytes: 15 * 1024 * 1024, // Current size is 15 MB
    };

    let to_delete = strategy.find_files_to_delete(&files, &config);
    // Should delete the oldest file (5 MB) to get under 10 MB limit
    assert_eq!(to_delete.len(), 1);
    assert_eq!(to_delete[0].file_name().unwrap(), "old.txt");
}

#[test]
fn test_space_strategy_no_op_when_under_limit() {
    let strategy = SpaceBasedStrategy;
    let now = SystemTime::now();

    let files = vec![FileMeta {
        path: PathBuf::from("/tmp/file.txt"),
        mtime: now,
        size_bytes: 5 * 1024 * 1024, // 5 MB
    }];

    let config = StrategyConfig {
        retention_hours: 0,
        max_size_bytes: 10 * 1024 * 1024,        // Limit to 10 MB
        current_dir_size_bytes: 5 * 1024 * 1024, // Current size is 5 MB
    };

    let to_delete = strategy.find_files_to_delete(&files, &config);
    assert!(to_delete.is_empty());
}

#[test]
fn test_from_params_only_registers_explicit_rules() {
    use crate::temp_file_manager::TempFileManager;

    let manager = TempFileManager::from_params(
        Path::new("/tmp"),
        true,
        false,
        1.0,
        std::iter::empty::<(&str, Option<&str>, u64, u64)>(),
    )
    .unwrap();
    assert_eq!(manager.rules_count(), 0);
}

#[test]
fn test_from_params_accepts_previously_protected_paths() {
    use crate::temp_file_manager::TempFileManager;

    let manager = TempFileManager::from_params(
        Path::new("/tmp"),
        true,
        false,
        1.0,
        [
            ("data/sessions", None::<&str>, 24, 0),
            ("data/state/cache", None::<&str>, 24, 0),
        ],
    )
    .unwrap();
    assert_eq!(manager.rules_count(), 2);
}

#[test]
fn test_interval_conversion_minutes() {
    use crate::temp_file_manager::TempFileManager;

    let manager = TempFileManager::from_params(
        Path::new("/tmp"),
        true,
        true,
        0.1, // 6 minutes
        std::iter::empty::<(&str, Option<&str>, u64, u64)>(),
    )
    .unwrap();
    let duration = manager.calculate_interval_duration();

    // 0.1 hours = 6 minutes = 360 seconds
    assert_eq!(duration.as_secs(), 360);
}

#[test]
fn test_interval_minimum_enforcement() {
    use crate::temp_file_manager::TempFileManager;

    let manager = TempFileManager::from_params(
        Path::new("/tmp"),
        true,
        true,
        0.01, // 0.6 minutes (< 1 minute)
        std::iter::empty::<(&str, Option<&str>, u64, u64)>(),
    )
    .unwrap();
    let duration = manager.calculate_interval_duration();

    // Should be adjusted to 1 minute = 60 seconds
    assert_eq!(duration.as_secs(), 60);
}

#[test]
fn test_get_usage_rejects_workspace_escape() {
    use crate::temp_file_manager::TempFileManager;

    let install_root = TempDir::new().unwrap();
    let manager = TempFileManager::from_params(
        install_root.path(),
        true,
        false,
        1.0,
        std::iter::empty::<(&str, Option<&str>, u64, u64)>(),
    )
    .unwrap();

    let err = match manager.get_usage(install_root.path(), "../outside") {
        Ok(_) => panic!("workspace escape must be rejected"),
        Err(err) => err,
    };
    assert!(
        err.to_string().contains("escapes cleanup root"),
        "unexpected error: {err}"
    );
}

#[test]
fn test_from_params_accepts_install_root_sibling_path() {
    use crate::temp_file_manager::TempFileManager;

    let install_root = TempDir::new().unwrap();

    let manager = TempFileManager::from_params(
        install_root.path(),
        true,
        false,
        1.0,
        [("shared", None::<&str>, 24, 0)],
    )
    .unwrap();

    assert_eq!(manager.rules_count(), 1);
}

#[cfg(unix)]
#[test]
fn test_enforce_all_skips_rule_path_that_becomes_symlink_escape() {
    use crate::temp_file_manager::TempFileManager;
    use std::fs;
    use std::os::unix::fs::symlink;

    let install_root = TempDir::new().unwrap();
    let outside = TempDir::new().unwrap();
    let outside_file = outside.path().join("victim.log");

    fs::write(&outside_file, vec![0_u8; 2 * 1024 * 1024]).unwrap();

    let manager = TempFileManager::from_params(
        install_root.path(),
        true,
        false,
        1.0,
        [("cache", Some("*.log"), 0, 1)],
    )
    .unwrap();

    symlink(outside.path(), install_root.path().join("cache")).unwrap();

    let report = manager.enforce_all().unwrap();

    assert_eq!(report.files_deleted, 0);
    assert!(
        report.errors.is_empty(),
        "symlink escape should be skipped, got errors: {:?}",
        report.errors
    );
    assert!(outside_file.exists(), "outside file must not be deleted");
}

#[tokio::test]
async fn test_start_scheduled_cleanup_returns_when_pre_cancelled() {
    use crate::temp_file_manager::TempFileManager;

    let install_root = TempDir::new().unwrap();
    let manager = Arc::new(
        TempFileManager::from_params(
            install_root.path(),
            true,
            true,
            1.0,
            std::iter::empty::<(&str, Option<&str>, u64, u64)>(),
        )
        .unwrap(),
    );
    let cancel = tokio_util::sync::CancellationToken::new();
    cancel.cancel();

    let result = tokio::time::timeout(
        Duration::from_millis(100),
        manager.start_scheduled_cleanup(cancel),
    )
    .await;

    assert!(
        result.is_ok(),
        "pre-cancelled cleanup task should exit promptly"
    );
    assert!(result.unwrap().is_ok());
}
