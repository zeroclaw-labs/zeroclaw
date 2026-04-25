use chrono::Local;
use std::path::Path;

use crate::error::{Result, RotationError};

/// Check whether the active file was last modified on a different local date
/// than `now`. Returns `false` if the file does not exist.
pub fn should_rotate_by_date(path: &Path, now: &chrono::DateTime<Local>) -> bool {
    let Ok(metadata) = std::fs::metadata(path) else {
        return false;
    };
    let Ok(modified) = metadata.modified() else {
        return false;
    };
    let modified_date: chrono::DateTime<Local> = modified.into();
    modified_date.date_naive() != now.date_naive()
}

/// Check whether the file at `path` exceeds `max_size` bytes.
/// Returns `false` if the file does not exist.
pub fn should_rotate_by_size(path: &Path, max_size: u64) -> bool {
    let Ok(metadata) = std::fs::metadata(path) else {
        return false;
    };
    metadata.len() >= max_size
}

/// Rotate the active file by renaming it to a date + sequence-number pattern.
///
/// The rotated file name format is: `<stem>.YYYY-MM-DD.<seq>.<ext>`
/// Sequence numbers start at 1 and increment for same-day rotations.
pub async fn rotate_file(active_path: &Path, now: &chrono::DateTime<Local>) -> Result<()> {
    if !active_path.exists() {
        return Ok(());
    }

    let seq = next_sequence(active_path, now.date_naive());
    let rotated_name = format_rotated_name(active_path, now.date_naive(), seq);
    let rotated_path = active_path.with_file_name(&rotated_name);

    tokio::fs::rename(active_path, &rotated_path)
        .await
        .map_err(|e| RotationError::io(&rotated_path, e))?;

    tracing::debug!(
        active = %active_path.display(),
        rotated = %rotated_path.display(),
        "Rotated file"
    );

    Ok(())
}

/// Format a rotated file name from the active file path, a date, and a sequence number.
///
/// E.g. `runtime-trace.jsonl` + `2026-04-20` + 1 → `runtime-trace.2026-04-20.1.jsonl`
pub fn format_rotated_name(active_path: &Path, date: chrono::NaiveDate, seq: u32) -> String {
    let stem = active_path
        .file_stem()
        .and_then(|s| s.to_str())
        .unwrap_or("log");
    let ext = active_path
        .extension()
        .and_then(|s| s.to_str())
        .unwrap_or("log");
    format!("{stem}.{date}.{seq}.{ext}")
}

/// Scan the directory for rotated files with the same stem and date, and return
/// the next available sequence number (max existing + 1, or 1 if none exist).
fn next_sequence(active_path: &Path, date: chrono::NaiveDate) -> u32 {
    let dir = active_path.parent().unwrap_or(Path::new("."));
    let stem = active_path
        .file_stem()
        .and_then(|s| s.to_str())
        .unwrap_or("log");
    let ext = active_path
        .extension()
        .and_then(|s| s.to_str())
        .unwrap_or("log");

    let date_str = date.format("%Y-%m-%d").to_string();
    let prefix = format!("{stem}.{date_str}.");

    let Ok(entries) = std::fs::read_dir(dir) else {
        return 1;
    };

    let mut max_seq: u32 = 0;
    for entry in entries.flatten() {
        let name = entry.file_name();
        let name = name.to_string_lossy();
        if name.starts_with(&prefix) && name.ends_with(&format!(".{ext}")) {
            let middle = &name[prefix.len()..name.len() - ext.len() - 1];
            if let Ok(seq) = middle.parse::<u32>() {
                max_seq = max_seq.max(seq);
            }
        }
    }

    max_seq + 1
}

#[cfg(test)]
mod tests {
    use super::*;
    use std::fs;

    #[test]
    fn format_rotated_name_basic() {
        let path = Path::new("state/runtime-trace.jsonl");
        let date = chrono::NaiveDate::from_ymd_opt(2026, 4, 20).unwrap();
        assert_eq!(
            format_rotated_name(path, date, 1),
            "runtime-trace.2026-04-20.1.jsonl"
        );
        assert_eq!(
            format_rotated_name(path, date, 3),
            "runtime-trace.2026-04-20.3.jsonl"
        );
    }

    #[test]
    fn next_sequence_returns_one_when_no_files() {
        let tmp = tempfile::tempdir().unwrap();
        let path = tmp.path().join("app.log");
        let date = chrono::NaiveDate::from_ymd_opt(2026, 4, 20).unwrap();
        assert_eq!(next_sequence(&path, date), 1);
    }

    #[test]
    fn next_sequence_increments_existing() {
        let tmp = tempfile::tempdir().unwrap();
        let date = chrono::NaiveDate::from_ymd_opt(2026, 4, 20).unwrap();

        // Create existing rotated files
        fs::write(tmp.path().join("app.2026-04-20.1.log"), "").unwrap();
        fs::write(tmp.path().join("app.2026-04-20.2.log"), "").unwrap();

        let path = tmp.path().join("app.log");
        assert_eq!(next_sequence(&path, date), 3);
    }

    #[test]
    fn should_rotate_by_size_returns_false_for_missing() {
        assert!(!should_rotate_by_size(Path::new("/nonexistent"), 100));
    }

    #[test]
    fn should_rotate_by_size_checks_threshold() {
        let tmp = tempfile::tempdir().unwrap();
        let path = tmp.path().join("test.log");
        fs::write(&path, "hello").unwrap();
        assert!(!should_rotate_by_size(&path, 100));
        assert!(should_rotate_by_size(&path, 3));
    }

    #[test]
    fn should_rotate_by_date_returns_false_for_missing() {
        let now = Local::now();
        assert!(
            !should_rotate_by_date(Path::new("/nonexistent_12345"), &now),
            "Missing file should not trigger date rotation"
        );
    }

    #[test]
    fn should_rotate_by_date_returns_true_for_old_mtime() {
        let tmp = tempfile::tempdir().unwrap();
        let path = tmp.path().join("old.log");
        fs::write(&path, "data").unwrap();

        let yesterday = std::time::SystemTime::now() - std::time::Duration::from_secs(24 * 3600);
        filetime::set_file_mtime(&path, filetime::FileTime::from_system_time(yesterday)).unwrap();

        let now = Local::now();
        assert!(
            should_rotate_by_date(&path, &now),
            "File with yesterday's mtime should trigger date rotation"
        );
    }

    #[test]
    fn should_rotate_by_date_returns_false_for_today() {
        let tmp = tempfile::tempdir().unwrap();
        let path = tmp.path().join("today.log");
        fs::write(&path, "data").unwrap();

        let now = Local::now();
        assert!(
            !should_rotate_by_date(&path, &now),
            "File modified today should not trigger date rotation"
        );
    }

    #[tokio::test]
    async fn rotate_file_missing_path_is_noop() {
        let now = Local::now();
        let result = rotate_file(Path::new("/nonexistent_12345.log"), &now).await;
        assert!(result.is_ok(), "Rotating a missing file should be Ok(())");
    }

    #[test]
    fn next_sequence_with_gap() {
        let tmp = tempfile::tempdir().unwrap();
        let date = chrono::NaiveDate::from_ymd_opt(2026, 4, 20).unwrap();

        // Create files with a gap: seq 1 and 5 exist, 2-4 are missing
        fs::write(tmp.path().join("app.2026-04-20.1.log"), "").unwrap();
        fs::write(tmp.path().join("app.2026-04-20.5.log"), "").unwrap();

        let path = tmp.path().join("app.log");
        // Should return max(1,5) + 1 = 6
        assert_eq!(
            next_sequence(&path, date),
            6,
            "Should return max sequence + 1 even with gaps"
        );
    }

    #[test]
    fn format_rotated_name_with_no_extension() {
        let path = Path::new("state/mylog");
        let date = chrono::NaiveDate::from_ymd_opt(2026, 4, 20).unwrap();
        let name = format_rotated_name(path, date, 1);
        assert_eq!(name, "mylog.2026-04-20.1.log");
    }
}
