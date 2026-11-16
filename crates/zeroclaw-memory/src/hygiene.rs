use anyhow::Result;
use chrono::{DateTime, Duration, Local, NaiveDate, Utc};
use rusqlite::{Connection, params};
use serde::{Deserialize, Serialize};
use std::fs;
use std::path::{Path, PathBuf};
use std::time::{Duration as StdDuration, SystemTime};
use zeroclaw_config::schema::MemoryConfig;

const HYGIENE_INTERVAL_HOURS: i64 = 12;
const STATE_FILE: &str = "memory_hygiene_state.json";
/// Cadence marker for optional LLM enrichment (same 12h window as hygiene).
///
/// SSOT: this file is the source of truth for enrichment due-ness — no
/// in-memory flag is cached across calls.
const ENRICHMENT_STATE_FILE: &str = "memory_enrichment_state.json";

#[derive(Debug, Clone, Default, Serialize, Deserialize)]
struct HygieneReport {
    archived_memory_files: u64,
    archived_session_files: u64,
    purged_memory_archives: u64,
    purged_session_archives: u64,
}

impl HygieneReport {
    fn total_actions(&self) -> u64 {
        self.archived_memory_files
            + self.archived_session_files
            + self.purged_memory_archives
            + self.purged_session_archives
    }
}

#[derive(Debug, Clone, Default, Serialize, Deserialize)]
struct HygieneState {
    last_run_at: Option<String>,
    last_report: HygieneReport,
}

/// Whether the 12h hygiene cadence window has elapsed for `workspace_dir`.
///
/// Does **not** consult `MemoryConfig::hygiene_enabled` — callers combine that
/// themselves when deciding whether to run sibling work (e.g. tachi light-sleep)
/// on the same schedule as [`run_if_due`].
pub fn is_due(workspace_dir: &Path) -> Result<bool> {
    should_run_now(workspace_dir)
}

#[derive(Debug, Clone, Default, Serialize, Deserialize)]
struct EnrichmentCadenceState {
    last_run_at: Option<String>,
}

/// Whether the 12h LLM-enrichment cadence window has elapsed.
///
/// Independent of hygiene's state file so light-sleep/file pruning and LLM
/// enrichment can advance on their own clocks. Same interval as hygiene.
pub fn enrichment_is_due(workspace_dir: &Path) -> Result<bool> {
    let path = enrichment_state_path(workspace_dir);
    if !path.exists() {
        return Ok(true);
    }
    let raw = fs::read_to_string(&path)?;
    let state: EnrichmentCadenceState = match serde_json::from_str(&raw) {
        Ok(s) => s,
        Err(_) => return Ok(true),
    };
    let Some(last_run_at) = state.last_run_at else {
        return Ok(true);
    };
    let last = match DateTime::parse_from_rfc3339(&last_run_at) {
        Ok(ts) => ts.with_timezone(&Utc),
        Err(_) => return Ok(true),
    };
    Ok(Utc::now().signed_duration_since(last) >= Duration::hours(HYGIENE_INTERVAL_HOURS))
}

/// Record that an enrichment pass was attempted (advances the 12h cadence).
pub fn enrichment_mark_ran(workspace_dir: &Path) -> Result<()> {
    let path = enrichment_state_path(workspace_dir);
    if let Some(parent) = path.parent() {
        fs::create_dir_all(parent)?;
    }
    let state = EnrichmentCadenceState {
        last_run_at: Some(Utc::now().to_rfc3339()),
    };
    fs::write(path, serde_json::to_vec_pretty(&state)?)?;
    Ok(())
}

fn enrichment_state_path(workspace_dir: &Path) -> PathBuf {
    workspace_dir.join("state").join(ENRICHMENT_STATE_FILE)
}

/// Run memory/session hygiene if the cadence window has elapsed.
/// This function is intentionally best-effort: callers should log and continue on failure.
pub fn run_if_due(config: &MemoryConfig, workspace_dir: &Path) -> Result<()> {
    if !config.hygiene_enabled {
        return Ok(());
    }

    if !should_run_now(workspace_dir)? {
        return Ok(());
    }

    let report = HygieneReport {
        archived_memory_files: archive_daily_memory_files(
            workspace_dir,
            config.archive_after_days,
        )?,
        archived_session_files: archive_session_files(workspace_dir, config.archive_after_days)?,
        purged_memory_archives: purge_memory_archives(workspace_dir, config.purge_after_days)?,
        purged_session_archives: purge_session_archives(workspace_dir, config.purge_after_days)?,
    };

    // Retention cleanup is independent from new audit collection: disabling
    // audit stops new rows, but existing sensitive rows still honor retention.
    if let Err(e) = prune_audit_entries(workspace_dir, config.audit_retention_days) {
        ::zeroclaw_log::record!(
            DEBUG,
            ::zeroclaw_log::Event::new(module_path!(), ::zeroclaw_log::Action::Note)
                .with_attrs(::serde_json::json!({"error": format!("{}", e)})),
            "audit pruning skipped"
        );
    }

    write_state(workspace_dir, &report)?;

    if report.total_actions() > 0 {
        ::zeroclaw_log::record!(
            INFO,
            ::zeroclaw_log::Event::new(module_path!(), ::zeroclaw_log::Action::Note),
            &format!(
                "memory hygiene complete: archived_memory={} archived_sessions={} purged_memory={} purged_sessions={}",
                report.archived_memory_files,
                report.archived_session_files,
                report.purged_memory_archives,
                report.purged_session_archives,
            )
        );
    }

    Ok(())
}

fn should_run_now(workspace_dir: &Path) -> Result<bool> {
    let path = state_path(workspace_dir);
    if !path.exists() {
        return Ok(true);
    }

    let raw = fs::read_to_string(&path)?;
    let state: HygieneState = match serde_json::from_str(&raw) {
        Ok(s) => s,
        Err(_) => return Ok(true),
    };

    let Some(last_run_at) = state.last_run_at else {
        return Ok(true);
    };

    let last = match DateTime::parse_from_rfc3339(&last_run_at) {
        Ok(ts) => ts.with_timezone(&Utc),
        Err(_) => return Ok(true),
    };

    Ok(Utc::now().signed_duration_since(last) >= Duration::hours(HYGIENE_INTERVAL_HOURS))
}

fn write_state(workspace_dir: &Path, report: &HygieneReport) -> Result<()> {
    let path = state_path(workspace_dir);
    if let Some(parent) = path.parent() {
        fs::create_dir_all(parent)?;
    }

    let state = HygieneState {
        last_run_at: Some(Utc::now().to_rfc3339()),
        last_report: report.clone(),
    };
    let json = serde_json::to_vec_pretty(&state)?;
    fs::write(path, json)?;
    Ok(())
}

fn state_path(workspace_dir: &Path) -> PathBuf {
    workspace_dir.join("state").join(STATE_FILE)
}

fn archive_daily_memory_files(workspace_dir: &Path, archive_after_days: u32) -> Result<u64> {
    if archive_after_days == 0 {
        return Ok(0);
    }

    let memory_dir = workspace_dir.join("memory");
    if !memory_dir.is_dir() {
        return Ok(0);
    }

    let archive_dir = memory_dir.join("archive");
    fs::create_dir_all(&archive_dir)?;

    let cutoff = Local::now().date_naive() - Duration::days(i64::from(archive_after_days));
    let mut moved = 0_u64;

    for entry in fs::read_dir(&memory_dir)? {
        let entry = entry?;
        let path = entry.path();

        if path.is_dir() {
            continue;
        }
        if path.extension().and_then(|e| e.to_str()) != Some("md") {
            continue;
        }

        let Some(filename) = path.file_name().and_then(|f| f.to_str()) else {
            continue;
        };

        let Some(file_date) = memory_date_from_filename(filename) else {
            continue;
        };

        if file_date < cutoff {
            move_to_archive(&path, &archive_dir)?;
            moved += 1;
        }
    }

    Ok(moved)
}

fn archive_session_files(workspace_dir: &Path, archive_after_days: u32) -> Result<u64> {
    if archive_after_days == 0 {
        return Ok(0);
    }

    let sessions_dir = workspace_dir.join("sessions");
    if !sessions_dir.is_dir() {
        return Ok(0);
    }

    let archive_dir = sessions_dir.join("archive");
    fs::create_dir_all(&archive_dir)?;

    let cutoff_date = Local::now().date_naive() - Duration::days(i64::from(archive_after_days));
    let cutoff_time = SystemTime::now()
        .checked_sub(StdDuration::from_secs(
            u64::from(archive_after_days) * 24 * 60 * 60,
        ))
        .unwrap_or(SystemTime::UNIX_EPOCH);

    let mut moved = 0_u64;
    for entry in fs::read_dir(&sessions_dir)? {
        let entry = entry?;
        let path = entry.path();

        if path.is_dir() {
            continue;
        }

        let Some(filename) = path.file_name().and_then(|f| f.to_str()) else {
            continue;
        };

        if !is_legacy_session_artifact(filename) {
            continue;
        }

        let is_old = if let Some(date) = date_prefix(filename) {
            date < cutoff_date
        } else {
            is_older_than(&path, cutoff_time)
        };

        if is_old {
            move_to_archive(&path, &archive_dir)?;
            moved += 1;
        }
    }

    Ok(moved)
}

fn is_legacy_session_artifact(filename: &str) -> bool {
    date_prefix(filename).is_some()
        || filename.ends_with(".jsonl")
        || filename.ends_with(".jsonl.migrated")
}

fn purge_memory_archives(workspace_dir: &Path, purge_after_days: u32) -> Result<u64> {
    if purge_after_days == 0 {
        return Ok(0);
    }

    let archive_dir = workspace_dir.join("memory").join("archive");
    if !archive_dir.is_dir() {
        return Ok(0);
    }

    let cutoff = Local::now().date_naive() - Duration::days(i64::from(purge_after_days));
    let mut removed = 0_u64;

    for entry in fs::read_dir(&archive_dir)? {
        let entry = entry?;
        let path = entry.path();

        if path.is_dir() {
            continue;
        }

        let Some(filename) = path.file_name().and_then(|f| f.to_str()) else {
            continue;
        };

        let Some(file_date) = memory_date_from_filename(filename) else {
            continue;
        };

        if file_date < cutoff {
            fs::remove_file(&path)?;
            removed += 1;
        }
    }

    Ok(removed)
}

fn purge_session_archives(workspace_dir: &Path, purge_after_days: u32) -> Result<u64> {
    if purge_after_days == 0 {
        return Ok(0);
    }

    let archive_dir = workspace_dir.join("sessions").join("archive");
    if !archive_dir.is_dir() {
        return Ok(0);
    }

    let cutoff_date = Local::now().date_naive() - Duration::days(i64::from(purge_after_days));
    let cutoff_time = SystemTime::now()
        .checked_sub(StdDuration::from_secs(
            u64::from(purge_after_days) * 24 * 60 * 60,
        ))
        .unwrap_or(SystemTime::UNIX_EPOCH);

    let mut removed = 0_u64;
    for entry in fs::read_dir(&archive_dir)? {
        let entry = entry?;
        let path = entry.path();

        if path.is_dir() {
            continue;
        }

        let Some(filename) = path.file_name().and_then(|f| f.to_str()) else {
            continue;
        };

        if !is_legacy_session_artifact(filename) {
            continue;
        }

        let is_old = if let Some(date) = date_prefix(filename) {
            date < cutoff_date
        } else {
            is_older_than(&path, cutoff_time)
        };

        if is_old {
            fs::remove_file(&path)?;
            removed += 1;
        }
    }

    Ok(removed)
}

fn prune_audit_entries(workspace_dir: &Path, retention_days: u32) -> Result<()> {
    if retention_days == 0 {
        return Ok(());
    }

    let db_path = workspace_dir.join("memory").join("audit.db");
    if !db_path.exists() {
        return Ok(());
    }

    let conn = Connection::open(db_path)?;
    conn.execute_batch("PRAGMA journal_mode = WAL; PRAGMA synchronous = NORMAL;")?;
    let cutoff = (Local::now() - Duration::days(i64::from(retention_days))).to_rfc3339();

    let affected = conn.execute(
        "DELETE FROM memory_audit WHERE timestamp < ?1",
        params![cutoff],
    )?;

    if affected > 0 {
        ::zeroclaw_log::record!(
            DEBUG,
            ::zeroclaw_log::Event::new(module_path!(), ::zeroclaw_log::Action::Note).with_attrs(
                ::serde_json::json!({"affected": affected, "retention_days": retention_days})
            ),
            "pruned  audit entries older than  days"
        );
    }

    Ok(())
}

fn memory_date_from_filename(filename: &str) -> Option<NaiveDate> {
    let stem = filename.strip_suffix(".md")?;
    // Split on '_' first (handles YYYY-MM-DD_suffix.md).
    let date_part = stem.split('_').next().unwrap_or(stem);
    // If the date part has more than 10 chars (e.g. 2026-03-28-1442),
    // take only the YYYY-MM-DD prefix.
    let date_str = if date_part.len() > 10 {
        date_part.get(..10)?
    } else {
        date_part
    };
    NaiveDate::parse_from_str(date_str, "%Y-%m-%d").ok()
}

fn date_prefix(filename: &str) -> Option<NaiveDate> {
    if filename.len() < 10 {
        return None;
    }
    let boundary = {
        let mut i = 10.min(filename.len());
        while i > 0 && !filename.is_char_boundary(i) {
            i -= 1;
        }
        i
    };
    NaiveDate::parse_from_str(&filename[..boundary], "%Y-%m-%d").ok()
}

fn is_older_than(path: &Path, cutoff: SystemTime) -> bool {
    fs::metadata(path)
        .and_then(|meta| meta.modified())
        .map(|modified| modified < cutoff)
        .unwrap_or(false)
}

fn move_to_archive(src: &Path, archive_dir: &Path) -> Result<()> {
    let Some(filename) = src.file_name().and_then(|f| f.to_str()) else {
        return Ok(());
    };

    let target = unique_archive_target(archive_dir, filename);
    fs::rename(src, target)?;
    Ok(())
}

fn unique_archive_target(archive_dir: &Path, filename: &str) -> PathBuf {
    let direct = archive_dir.join(filename);
    if !direct.exists() {
        return direct;
    }

    let (stem, ext) = split_name(filename);
    for i in 1..10_000 {
        let candidate = if ext.is_empty() {
            archive_dir.join(format!("{stem}_{i}"))
        } else {
            archive_dir.join(format!("{stem}_{i}.{ext}"))
        };
        if !candidate.exists() {
            return candidate;
        }
    }

    direct
}

fn split_name(filename: &str) -> (&str, &str) {
    match filename.rsplit_once('.') {
        Some((stem, ext)) => (stem, ext),
        None => (filename, ""),
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use filetime::{FileTime, set_file_mtime};
    use tempfile::TempDir;

    fn default_cfg() -> MemoryConfig {
        MemoryConfig::default()
    }

    fn set_old_mtime(path: &Path, days_old: i64) {
        let old = FileTime::from_system_time(
            (SystemTime::now() - StdDuration::from_secs(days_old as u64 * 24 * 60 * 60))
                .max(SystemTime::UNIX_EPOCH),
        );
        set_file_mtime(path, old).unwrap();
    }

    #[test]
    fn enrichment_cadence_state_file_is_independent_ssot() {
        let tmp = TempDir::new().unwrap();
        let workspace = tmp.path();
        assert!(enrichment_is_due(workspace).unwrap());
        enrichment_mark_ran(workspace).unwrap();
        assert!(!enrichment_is_due(workspace).unwrap());
        // Hygiene state is a separate file — marking enrichment must not
        // satisfy hygiene due-ness.
        assert!(is_due(workspace).unwrap());
    }

    #[test]
    fn archives_old_daily_memory_files() {
        let tmp = TempDir::new().unwrap();
        let workspace = tmp.path();
        fs::create_dir_all(workspace.join("memory")).unwrap();

        let old = (Local::now().date_naive() - Duration::days(10))
            .format("%Y-%m-%d")
            .to_string();
        let today = Local::now().date_naive().format("%Y-%m-%d").to_string();

        let old_file = workspace.join("memory").join(format!("{old}.md"));
        let today_file = workspace.join("memory").join(format!("{today}.md"));
        fs::write(&old_file, "old note").unwrap();
        fs::write(&today_file, "fresh note").unwrap();

        run_if_due(&default_cfg(), workspace).unwrap();

        assert!(!old_file.exists(), "old daily file should be archived");
        assert!(
            workspace
                .join("memory")
                .join("archive")
                .join(format!("{old}.md"))
                .exists(),
            "old daily file should exist in memory/archive"
        );
        assert!(today_file.exists(), "today file should remain in place");
    }

    #[test]
    fn archives_old_session_files() {
        let tmp = TempDir::new().unwrap();
        let workspace = tmp.path();
        fs::create_dir_all(workspace.join("sessions")).unwrap();

        let old = (Local::now().date_naive() - Duration::days(10))
            .format("%Y-%m-%d")
            .to_string();
        let old_name = format!("{old}-agent.log");
        let old_file = workspace.join("sessions").join(&old_name);
        fs::write(&old_file, "old session").unwrap();

        run_if_due(&default_cfg(), workspace).unwrap();

        assert!(!old_file.exists(), "old session file should be archived");
        assert!(
            workspace
                .join("sessions")
                .join("archive")
                .join(&old_name)
                .exists(),
            "archived session file should exist"
        );
    }

    #[test]
    fn keeps_sqlite_session_artifacts_out_of_archives() {
        let tmp = TempDir::new().unwrap();
        let workspace = tmp.path();
        let sessions_dir = workspace.join("sessions");
        fs::create_dir_all(&sessions_dir).unwrap();

        let protected = ["sessions.db", "sessions.db-wal", "sessions.db-shm"];
        for filename in protected {
            let path = sessions_dir.join(filename);
            fs::write(&path, "sqlite artifact").unwrap();
            set_old_mtime(&path, 10);
        }

        run_if_due(&default_cfg(), workspace).unwrap();

        for filename in protected {
            assert!(
                sessions_dir.join(filename).exists(),
                "{filename} should remain in the hot sessions directory"
            );
            assert!(
                !sessions_dir.join("archive").join(filename).exists(),
                "{filename} must not be moved into the session archive"
            );
        }
    }

    #[test]
    fn archives_old_legacy_jsonl_session_files() {
        let tmp = TempDir::new().unwrap();
        let workspace = tmp.path();
        let sessions_dir = workspace.join("sessions");
        fs::create_dir_all(&sessions_dir).unwrap();

        let legacy_file = sessions_dir.join("legacy_session.jsonl");
        fs::write(&legacy_file, "legacy session").unwrap();
        set_old_mtime(&legacy_file, 10);

        run_if_due(&default_cfg(), workspace).unwrap();

        assert!(
            !legacy_file.exists(),
            "old legacy JSONL session file should be archived"
        );
        assert!(
            sessions_dir
                .join("archive")
                .join("legacy_session.jsonl")
                .exists(),
            "archived legacy JSONL session file should exist"
        );
    }

    #[test]
    fn purges_old_legacy_session_archives_but_keeps_sqlite_artifacts() {
        let tmp = TempDir::new().unwrap();
        let workspace = tmp.path();
        let archive_dir = workspace.join("sessions").join("archive");
        fs::create_dir_all(&archive_dir).unwrap();

        let protected = ["sessions.db", "sessions.db-wal", "sessions.db-shm"];
        for filename in protected {
            let path = archive_dir.join(filename);
            fs::write(&path, "sqlite artifact").unwrap();
            set_old_mtime(&path, 40);
        }

        let legacy_file = archive_dir.join("legacy_session.jsonl");
        fs::write(&legacy_file, "legacy session").unwrap();
        set_old_mtime(&legacy_file, 40);

        run_if_due(&default_cfg(), workspace).unwrap();

        assert!(
            !legacy_file.exists(),
            "old archived legacy session file should be purged"
        );
        for filename in protected {
            assert!(
                archive_dir.join(filename).exists(),
                "{filename} should remain in the session archive"
            );
        }
    }

    #[test]
    fn skips_second_run_within_cadence_window() {
        let tmp = TempDir::new().unwrap();
        let workspace = tmp.path();
        fs::create_dir_all(workspace.join("memory")).unwrap();

        let old_a = (Local::now().date_naive() - Duration::days(10))
            .format("%Y-%m-%d")
            .to_string();
        let file_a = workspace.join("memory").join(format!("{old_a}.md"));
        fs::write(&file_a, "first").unwrap();

        run_if_due(&default_cfg(), workspace).unwrap();
        assert!(!file_a.exists(), "first old file should be archived");

        let old_b = (Local::now().date_naive() - Duration::days(9))
            .format("%Y-%m-%d")
            .to_string();
        let file_b = workspace.join("memory").join(format!("{old_b}.md"));
        fs::write(&file_b, "second").unwrap();

        // Should skip because cadence gate prevents a second immediate run.
        run_if_due(&default_cfg(), workspace).unwrap();
        assert!(
            file_b.exists(),
            "second file should remain because run is throttled"
        );
    }

    #[test]
    fn purges_old_memory_archives() {
        let tmp = TempDir::new().unwrap();
        let workspace = tmp.path();
        let archive_dir = workspace.join("memory").join("archive");
        fs::create_dir_all(&archive_dir).unwrap();

        let old = (Local::now().date_naive() - Duration::days(40))
            .format("%Y-%m-%d")
            .to_string();
        let keep = (Local::now().date_naive() - Duration::days(5))
            .format("%Y-%m-%d")
            .to_string();

        let old_file = archive_dir.join(format!("{old}.md"));
        let keep_file = archive_dir.join(format!("{keep}.md"));
        fs::write(&old_file, "expired").unwrap();
        fs::write(&keep_file, "recent").unwrap();

        run_if_due(&default_cfg(), workspace).unwrap();

        assert!(!old_file.exists(), "old archived file should be purged");
        assert!(keep_file.exists(), "recent archived file should remain");
    }

    #[test]
    fn date_from_filename_handles_hyphen_suffix() {
        let d = memory_date_from_filename("2026-03-28-1442.md");
        assert!(d.is_some(), "YYYY-MM-DD-HHMM.md should be parsed");
        assert_eq!(d.unwrap(), NaiveDate::from_ymd_opt(2026, 3, 28).unwrap());
    }

    #[test]
    fn date_from_filename_handles_underscore_suffix() {
        let d = memory_date_from_filename("2026-03-28_session_notes.md");
        assert!(d.is_some(), "YYYY-MM-DD_suffix.md should be parsed");
        assert_eq!(d.unwrap(), NaiveDate::from_ymd_opt(2026, 3, 28).unwrap());
    }

    #[test]
    fn date_from_filename_plain_date() {
        let d = memory_date_from_filename("2026-03-28.md");
        assert!(d.is_some(), "YYYY-MM-DD.md should be parsed");
        assert_eq!(d.unwrap(), NaiveDate::from_ymd_opt(2026, 3, 28).unwrap());
    }

    fn seed_audit_db(workspace: &Path) -> PathBuf {
        fs::create_dir_all(workspace.join("memory")).unwrap();
        let db_path = workspace.join("memory").join("audit.db");
        let conn = Connection::open(&db_path).unwrap();
        conn.execute_batch(
            "CREATE TABLE IF NOT EXISTS memory_audit (
                 id INTEGER PRIMARY KEY AUTOINCREMENT,
                 operation TEXT NOT NULL,
                 key TEXT,
                 namespace TEXT,
                 session_id TEXT,
                 timestamp TEXT NOT NULL,
                 metadata TEXT
             );",
        )
        .unwrap();
        let stale = (Local::now() - Duration::days(45)).to_rfc3339();
        let fresh = Local::now().to_rfc3339();
        conn.execute(
            "INSERT INTO memory_audit (operation, timestamp) VALUES ('store', ?1)",
            params![stale],
        )
        .unwrap();
        conn.execute(
            "INSERT INTO memory_audit (operation, timestamp) VALUES ('store', ?1)",
            params![fresh],
        )
        .unwrap();
        db_path
    }

    fn audit_row_count(db_path: &Path) -> i64 {
        let conn = Connection::open(db_path).unwrap();
        conn.query_row("SELECT COUNT(*) FROM memory_audit", [], |row| row.get(0))
            .unwrap()
    }

    #[test]
    fn prunes_stale_audit_rows_when_audit_enabled() {
        let tmp = TempDir::new().unwrap();
        let workspace = tmp.path();
        let db_path = seed_audit_db(workspace);

        let mut cfg = default_cfg();
        cfg.audit_enabled = true;
        assert_eq!(cfg.audit_retention_days, 30);
        run_if_due(&cfg, workspace).unwrap();

        assert_eq!(
            audit_row_count(&db_path),
            1,
            "audit rows past audit_retention_days must be pruned"
        );
    }

    #[test]
    fn prunes_existing_audit_rows_when_audit_disabled() {
        let tmp = TempDir::new().unwrap();
        let workspace = tmp.path();
        let db_path = seed_audit_db(workspace);

        let cfg = default_cfg();
        assert!(!cfg.audit_enabled);
        run_if_due(&cfg, workspace).unwrap();

        assert_eq!(
            audit_row_count(&db_path),
            1,
            "disabling collection must not disable retention for existing audit rows"
        );
    }

    #[test]
    fn disabled_audit_does_not_create_missing_audit_db_for_pruning() {
        let tmp = TempDir::new().unwrap();
        let workspace = tmp.path();
        let audit_db = workspace.join("memory").join("audit.db");

        let cfg = default_cfg();
        assert!(!cfg.audit_enabled);
        run_if_due(&cfg, workspace).unwrap();

        assert!(
            !audit_db.exists(),
            "default-off audit must not create an audit db just to prune"
        );
    }
}
