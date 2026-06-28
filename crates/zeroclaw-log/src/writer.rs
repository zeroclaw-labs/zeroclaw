//! JSONL append-only writer + rolling rotation.
//!
//! RAM contract: a single event lands in two allocations (the JSON line
//! that goes to disk + the `serde_json::Value` clone that goes to the
//! broadcast hook). Rolling rotation streams through `BufReader::lines`
//! into a temp file rather than slurping the whole file into a `String`.

use std::fs::{self, OpenOptions};
use std::io::{BufRead, BufReader, BufWriter, Write};
use std::path::{Path, PathBuf};
use std::sync::{Arc, OnceLock};
use std::time::{Duration, SystemTime};

use chrono::{DateTime, Utc};

use crate::broadcast::current_broadcast_hook;
use crate::config::{LlmRequestPayloadPolicy, LogConfig, ResolvedPolicy, StoragePolicy};
use crate::event::LogEvent;
use crate::migrate;
use crate::observer_bridge;
use anyhow::{Context, Result};
use parking_lot::Mutex;
use serde_json::Value;

struct WriterState {
    policy: ResolvedPolicy,
    write_lock: Mutex<()>,
}

static WRITER: OnceLock<parking_lot::RwLock<Option<Arc<WriterState>>>> = OnceLock::new();

fn slot() -> &'static parking_lot::RwLock<Option<Arc<WriterState>>> {
    WRITER.get_or_init(|| parking_lot::RwLock::new(None))
}

fn current_state() -> Option<Arc<WriterState>> {
    slot().read().clone()
}

/// Initialize (or disable) the persistence writer from config. Idempotent.
/// When enabled, runs a streaming in-place migration of any schema-1 rows
/// in the existing file before resuming appends.
pub fn init_from_config(config: &LogConfig, workspace_dir: &Path) {
    let policy = ResolvedPolicy::from_config(config, workspace_dir);

    if policy.storage.is_enabled()
        && policy.path.exists()
        && let Err(err) = migrate::migrate_legacy_jsonl_in_place(&policy.path)
    {
        tracing::warn!(
            target: "zeroclaw_log",
            error = ?err,
            path = %policy.path.display(),
            "log: legacy JSONL migration failed; daemon continuing with mixed-shape file"
        );
    }

    let state = Arc::new(WriterState {
        policy,
        write_lock: Mutex::new(()),
    });
    *slot().write() = Some(state);
}

/// Public accessor for the canonical log file path. Used by the gateway's
/// `/api/logs` endpoint to know which file to stream.
pub fn runtime_trace_path() -> Option<PathBuf> {
    current_state().map(|s| s.policy.path.clone())
}

/// Resolved LLM-request-payload capture policy + the truncate cap, for the
/// turn engine's `announce_llm_request`. `None` when no writer is installed
/// (the policy defaults to `off`, so callers treat "no writer" as "off").
#[must_use]
pub fn llm_request_payload_policy() -> Option<(LlmRequestPayloadPolicy, usize)> {
    current_state().map(|s| {
        (
            s.policy.llm_request_payload,
            s.policy.tool_io_truncate_bytes,
        )
    })
}

/// Emit one event. Always fans out to the broadcast hook + tracing event.
/// If persistence is enabled, also appends a JSON line to disk.
///
/// This is the function the `record!` macro expands into. Direct callers
/// (the schema migration tool, tests) can invoke it too, but production
/// code should go through the macro so the `tracing::event!` carries the
/// correct `file:line` source info.
pub fn record_event(event: LogEvent) {
    let value = match serde_json::to_value(&event) {
        Ok(v) => v,
        Err(err) => {
            tracing::warn!(
                target: "zeroclaw_log_internal",
                error = ?err,
                "log: event serialization failed"
            );
            return;
        }
    };

    observer_bridge::forward(&event);

    if let Some(hook) = current_broadcast_hook() {
        let _ = hook.send(value.clone());
    }

    let Some(state) = current_state() else {
        return;
    };
    if !state.policy.storage.is_enabled() {
        return;
    }

    if let Err(err) = append_line(&state, &value) {
        tracing::warn!(
            target: "zeroclaw_log_internal",
            error = ?err,
            path = %state.policy.path.display(),
            "log: append failed",
        );
    }
}

fn append_line(state: &Arc<WriterState>, value: &Value) -> Result<()> {
    let _guard = state.write_lock.lock();

    if let Some(parent) = state.policy.path.parent() {
        fs::create_dir_all(parent)
            .with_context(|| format!("creating log directory {}", parent.display()))?;
    }

    // Date-boundary rotation runs *before* the append so a new day's first
    // event lands in a fresh file and the archived file holds exactly the prior
    // day(s). Size rotation runs *after* the append (below), since it depends on
    // the post-append file size.
    if state.policy.storage == StoragePolicy::Rotating {
        maybe_rotate_for_date(state)?;
    }

    let mut options = OpenOptions::new();
    options.create(true).append(true);

    #[cfg(unix)]
    {
        use std::os::unix::fs::OpenOptionsExt;
        options.mode(0o600);
    }

    let file = options
        .open(&state.policy.path)
        .with_context(|| format!("opening log file {}", state.policy.path.display()))?;
    let mut writer = BufWriter::new(file);
    serde_json::to_writer(&mut writer, value).context("serializing log line")?;
    writer.write_all(b"\n").context("writing newline")?;
    writer.flush().context("flushing log line")?;
    let file = writer
        .into_inner()
        .context("taking log file out of buf writer")?;
    file.sync_data().context("fsync log line")?;

    #[cfg(unix)]
    {
        use std::os::unix::fs::PermissionsExt;
        let _ = fs::set_permissions(&state.policy.path, fs::Permissions::from_mode(0o600));
    }

    match state.policy.storage {
        StoragePolicy::Rolling => trim_to_last_entries(state)?,
        StoragePolicy::Rotating => maybe_rotate_for_size(state)?,
        StoragePolicy::None | StoragePolicy::Full => {}
    }

    Ok(())
}

/// Rolling trim. Streams the file line-by-line into a temp file, keeping
/// the last `max_entries` lines, then atomically renames. Never loads the
/// whole file into memory.
fn trim_to_last_entries(state: &Arc<WriterState>) -> Result<()> {
    // Count lines first (cheap pass).
    let total = count_nonempty_lines(&state.policy.path)?;
    if total <= state.policy.max_entries {
        return Ok(());
    }
    let skip = total - state.policy.max_entries;

    let tmp = state.policy.path.with_extension(format!(
        "tmp.{}.{}",
        std::process::id(),
        chrono::Utc::now().timestamp_nanos_opt().unwrap_or_default(),
    ));

    {
        let mut opts = OpenOptions::new();
        opts.create_new(true).write(true);
        #[cfg(unix)]
        {
            use std::os::unix::fs::OpenOptionsExt;
            opts.mode(0o600);
        }
        let out_file = opts
            .open(&tmp)
            .with_context(|| format!("creating trim temp file {}", tmp.display()))?;
        let mut out = BufWriter::new(out_file);

        let in_file = fs::File::open(&state.policy.path)
            .with_context(|| format!("opening log for trim: {}", state.policy.path.display()))?;
        let reader = BufReader::new(in_file);

        let mut index: usize = 0;
        for line in reader.lines() {
            let line = line.context("reading log line during trim")?;
            if line.trim().is_empty() {
                continue;
            }
            if index >= skip {
                out.write_all(line.as_bytes())
                    .context("writing trim line")?;
                out.write_all(b"\n").context("writing trim newline")?;
            }
            index += 1;
        }
        out.flush().context("flushing trim file")?;
        out.into_inner()
            .context("taking trim file out of buf writer")?
            .sync_data()
            .context("fsync trim file")?;
    }

    #[cfg(unix)]
    {
        use std::os::unix::fs::PermissionsExt;
        let _ = fs::set_permissions(&tmp, fs::Permissions::from_mode(0o600));
    }
    fs::rename(&tmp, &state.policy.path).with_context(|| {
        format!(
            "renaming trim temp {} → {}",
            tmp.display(),
            state.policy.path.display()
        )
    })?;

    Ok(())
}

fn count_nonempty_lines(path: &Path) -> Result<usize> {
    let file = fs::File::open(path)
        .with_context(|| format!("opening log to count lines: {}", path.display()))?;
    let reader = BufReader::new(file);
    let mut n = 0usize;
    for line in reader.lines() {
        let line = line.context("reading log line for count")?;
        if !line.trim().is_empty() {
            n += 1;
        }
    }
    Ok(n)
}

// ── Archive rotation (StoragePolicy::Rotating) ───────────────────────
//
// Unlike rolling trim (which discards old entries from the active file),
// rotation renames the active file to a timestamped archive and prunes old
// archives by count/age, preserving rotated events for later diagnostics.

/// Rotate the active file to an archive when it has crossed a UTC day boundary
/// since its last write. No-op when daily rotation is off, the file is absent,
/// or it was last written today.
fn maybe_rotate_for_date(state: &Arc<WriterState>) -> Result<()> {
    if !state.policy.rotate_daily {
        return Ok(());
    }
    let path = &state.policy.path;
    let meta = match fs::metadata(path) {
        Ok(m) => m,
        Err(err) if err.kind() == std::io::ErrorKind::NotFound => return Ok(()),
        Err(err) => {
            return Err(err)
                .with_context(|| format!("stat log for date rotation: {}", path.display()));
        }
    };
    // An empty active file has nothing worth archiving.
    if meta.len() == 0 {
        return Ok(());
    }
    let modified: DateTime<Utc> = meta.modified().unwrap_or(SystemTime::UNIX_EPOCH).into();
    if modified.date_naive() < Utc::now().date_naive() {
        rotate_active(state, modified)?;
    }
    Ok(())
}

/// Rotate the active file when a just-completed append left it at or above the
/// configured byte budget. No-op when size rotation is disabled (`max_bytes`
/// `== 0`) or the file is under budget.
fn maybe_rotate_for_size(state: &Arc<WriterState>) -> Result<()> {
    let max = state.policy.max_bytes;
    if max == 0 {
        return Ok(());
    }
    let path = &state.policy.path;
    let meta = match fs::metadata(path) {
        Ok(m) => m,
        Err(err) if err.kind() == std::io::ErrorKind::NotFound => return Ok(()),
        Err(err) => {
            return Err(err)
                .with_context(|| format!("stat log for size rotation: {}", path.display()));
        }
    };
    if meta.len() >= max {
        // Stamp the archive with the file's last-write time (its newest event),
        // matching date rotation, so the archive name reflects its contents.
        let when: DateTime<Utc> = meta.modified().unwrap_or(SystemTime::UNIX_EPOCH).into();
        rotate_active(state, when)?;
    }
    Ok(())
}

/// Rename the active file to a deterministic, lexically sortable archive name
/// stamped with `when` (the time of the file's last event), then prune old
/// archives. Archive names keep the active file's extension so an operator's
/// `*.jsonl` tooling still matches them, e.g.
/// `runtime-trace.jsonl` → `runtime-trace.20260624-031500.jsonl`.
fn rotate_active(state: &Arc<WriterState>, when: DateTime<Utc>) -> Result<()> {
    let path = &state.policy.path;
    let archive = archive_path(path, when)?;
    fs::rename(path, &archive)
        .with_context(|| format!("rotating log {} → {}", path.display(), archive.display()))?;

    #[cfg(unix)]
    {
        use std::os::unix::fs::PermissionsExt;
        let _ = fs::set_permissions(&archive, fs::Permissions::from_mode(0o600));
    }

    run_retention(state);
    Ok(())
}

/// Build the archive path for `path`, stamping the timestamp before the
/// extension and disambiguating same-second rotations with a numeric suffix.
fn archive_path(path: &Path, when: DateTime<Utc>) -> Result<PathBuf> {
    let dir = path.parent().unwrap_or_else(|| Path::new("."));
    let file_name = path
        .file_name()
        .and_then(|s| s.to_str())
        .context("log path has no file name")?;
    let (base, ext) = split_base_ext(file_name);
    let stamp = when.format("%Y%m%d-%H%M%S").to_string();

    // Callers hold the writer `write_lock` across the existence check and the
    // subsequent `fs::rename`, so the check-then-rename has no in-process race.
    let mut candidate = dir.join(format!("{base}.{stamp}{ext}"));
    let mut n = 1u32;
    while candidate.exists() {
        candidate = dir.join(format!("{base}.{stamp}.{n}{ext}"));
        n += 1;
    }
    Ok(candidate)
}

/// Split a log file name into `(base, ext)` where `ext` includes the leading
/// dot (or is empty). The split is on the *last* dot so multi-dot names keep
/// only their final extension: `runtime-trace.jsonl` → `("runtime-trace",
/// ".jsonl")`; `a.b.jsonl` → `("a.b", ".jsonl")`; `trace` → `("trace", "")`.
fn split_base_ext(file_name: &str) -> (&str, &str) {
    match file_name.rfind('.') {
        Some(i) if i > 0 => (&file_name[..i], &file_name[i..]),
        _ => (file_name, ""),
    }
}

/// True when `s` is exactly a `YYYYMMDD-HHMMSS` stamp: 8 digits, `-`, 6 digits.
fn is_stamp(s: &str) -> bool {
    let b = s.as_bytes();
    b.len() == 15
        && b[..8].iter().all(u8::is_ascii_digit)
        && b[8] == b'-'
        && b[9..].iter().all(u8::is_ascii_digit)
}

/// True when `core` is *exactly* the infix [`archive_path`] places between the
/// base prefix and the extension: a `YYYYMMDD-HHMMSS` stamp, optionally followed
/// by `.<digits>` (the same-second collision counter). The match is exact, not a
/// prefix test, so a foreign sibling that merely *starts* with a stamp (e.g.
/// `<stamp>.backup` or `<stamp>.notes`) is never treated as an archive and can
/// never be pruned by retention.
fn is_archive_core(core: &str) -> bool {
    match core.split_once('.') {
        // `<stamp>.<counter>` — counter must be a non-empty run of digits.
        Some((stamp, counter)) => {
            !counter.is_empty() && counter.bytes().all(|b| b.is_ascii_digit()) && is_stamp(stamp)
        }
        // `<stamp>`
        None => is_stamp(core),
    }
}

/// List rotated archive files sitting next to the active file: siblings that
/// share the active file's base and extension but are not the active file
/// itself. Returns `(path, mtime)` pairs.
fn list_archives(active: &Path) -> Result<Vec<(PathBuf, SystemTime)>> {
    let dir = active.parent().unwrap_or_else(|| Path::new("."));
    let active_name = active
        .file_name()
        .and_then(|s| s.to_str())
        .context("log path has no file name")?;
    let (base, ext) = split_base_ext(active_name);
    let prefix = format!("{base}.");

    let mut out = Vec::new();
    let entries = match fs::read_dir(dir) {
        Ok(e) => e,
        Err(err) if err.kind() == std::io::ErrorKind::NotFound => return Ok(out),
        Err(err) => {
            return Err(err).with_context(|| format!("reading log dir {}", dir.display()));
        }
    };
    for entry in entries {
        let entry = entry.with_context(|| format!("reading entry in {}", dir.display()))?;
        let Some(name) = entry.file_name().to_str().map(str::to_string) else {
            continue;
        };
        if name == active_name {
            continue;
        }
        let Some(suffix) = name.strip_prefix(&prefix) else {
            continue;
        };
        // Archives keep the active file's extension, so strip it (and reject any
        // same-stem sibling carrying a different one). What remains must be the
        // *exact* shape `archive_path` generates — `<stamp>` or `<stamp>.<n>` —
        // so retention never prunes a foreign or crash-orphaned sibling that
        // merely starts with a stamp (e.g. `<stamp>.backup.jsonl`), even for an
        // extension-less log path.
        let core = if ext.is_empty() {
            suffix
        } else {
            let Some(core) = suffix.strip_suffix(ext) else {
                continue;
            };
            core
        };
        if !is_archive_core(core) {
            continue;
        }
        let Ok(meta) = entry.metadata() else { continue };
        if !meta.is_file() {
            continue;
        }
        let mtime = meta.modified().unwrap_or(SystemTime::UNIX_EPOCH);
        out.push((entry.path(), mtime));
    }
    Ok(out)
}

/// Prune rotated archives by age then by count. Best-effort: a removal failure
/// is logged but never fails the enclosing append, since retention is
/// housekeeping rather than part of the durability contract.
fn run_retention(state: &Arc<WriterState>) {
    let max_files = state.policy.retention_max_files;
    let max_age_days = state.policy.retention_max_age_days;
    if max_files == 0 && max_age_days == 0 {
        return;
    }

    let mut archives = match list_archives(&state.policy.path) {
        Ok(a) => a,
        Err(err) => {
            tracing::warn!(
                target: "zeroclaw_log_internal",
                error = ?err,
                "log: listing archives for retention failed",
            );
            return;
        }
    };
    // Newest first, so a later count cap keeps the most recent archives.
    archives.sort_by_key(|(_, mtime)| std::cmp::Reverse(*mtime));

    // Age-based cleanup.
    if max_age_days > 0
        && let Some(cutoff) =
            SystemTime::now().checked_sub(Duration::from_secs(max_age_days.saturating_mul(86_400)))
    {
        archives.retain(|(p, mtime)| {
            if *mtime < cutoff {
                remove_archive(p);
                false
            } else {
                true
            }
        });
    }

    // Count-based cleanup: keep the newest `max_files`, drop the rest.
    if max_files > 0 && archives.len() > max_files {
        for (p, _) in archives.iter().skip(max_files) {
            remove_archive(p);
        }
    }
}

fn remove_archive(path: &Path) {
    if let Err(err) = fs::remove_file(path) {
        tracing::warn!(
            target: "zeroclaw_log_internal",
            error = ?err,
            path = %path.display(),
            "log: pruning rotated archive failed",
        );
    }
}

/// Shared test-time mutex for tests that mutate the global writer state.
/// Re-exported `pub(crate)` so `macro::tests` etc. can serialize against
/// the same lock as `writer::tests`. Always compiled (not gated behind
/// `#[cfg(test)]`) so peer crates can borrow it via the
/// `__private_test_writer_lock` helper in `lib.rs`. A `parking_lot::Mutex`
/// static costs nothing at runtime when untouched.
pub(crate) static WRITER_TEST_LOCK: parking_lot::Mutex<()> = parking_lot::Mutex::new(());

#[cfg(test)]
mod tests {
    use super::*;
    use crate::event::{EventCategory, Severity};

    fn install_writer(dir: &Path, max_entries: usize) {
        let cfg = LogConfig {
            log_persistence: "rolling".into(),
            log_persistence_max_entries: max_entries,
            ..LogConfig::default()
        };
        init_from_config(&cfg, dir);
    }

    #[test]
    fn append_and_rolling_keeps_only_max_entries() {
        let _guard = WRITER_TEST_LOCK.lock();
        let tmp = tempfile::tempdir().unwrap();
        install_writer(tmp.path(), 3);

        for i in 0..10 {
            let mut ev = LogEvent::new(Severity::Info, "test", EventCategory::Agent);
            ev.message = Some(format!("event-{i}"));
            record_event(ev);
        }

        let path = runtime_trace_path().unwrap();
        let contents = fs::read_to_string(&path).unwrap();
        let lines: Vec<&str> = contents.lines().filter(|l| !l.trim().is_empty()).collect();
        assert_eq!(lines.len(), 3);
        // Last three should be 7, 8, 9 (oldest to newest order preserved).
        for (idx, &line) in lines.iter().enumerate() {
            let v: Value = serde_json::from_str(line).unwrap();
            assert_eq!(v["message"].as_str().unwrap(), format!("event-{}", idx + 7));
        }
    }

    #[test]
    fn disabled_storage_does_not_write_file() {
        let _guard = WRITER_TEST_LOCK.lock();
        let tmp = tempfile::tempdir().unwrap();
        let cfg = LogConfig {
            log_persistence: "none".into(),
            ..LogConfig::default()
        };
        init_from_config(&cfg, tmp.path());

        let event = LogEvent::new(Severity::Info, "test", EventCategory::Agent);
        record_event(event);

        let path = runtime_trace_path().unwrap();
        assert!(
            !path.exists(),
            "no file should exist when storage is disabled"
        );
    }

    // ── Rotation (StoragePolicy::Rotating) ───────────────────────

    fn install_rotating(
        dir: &Path,
        max_bytes: u64,
        rotate_daily: bool,
        max_files: usize,
        max_age_days: u64,
    ) {
        let cfg = LogConfig {
            log_persistence: "rotating".into(),
            log_persistence_max_bytes: max_bytes,
            log_persistence_rotate_daily: rotate_daily,
            log_persistence_retention_max_files: max_files,
            log_persistence_retention_max_age_days: max_age_days,
            ..LogConfig::default()
        };
        init_from_config(&cfg, dir);
    }

    fn emit(msg: &str) {
        let mut ev = LogEvent::new(Severity::Info, "test", EventCategory::Agent);
        ev.message = Some(msg.to_string());
        record_event(ev);
    }

    fn set_mtime(path: &Path, when: SystemTime) {
        OpenOptions::new()
            .write(true)
            .open(path)
            .unwrap()
            .set_modified(when)
            .unwrap();
    }

    fn count_lines(path: &Path) -> usize {
        fs::read_to_string(path)
            .map(|s| s.lines().filter(|l| !l.trim().is_empty()).count())
            .unwrap_or(0)
    }

    /// Total events preserved across the active file plus every archive.
    fn total_events(active: &Path) -> usize {
        let mut n = count_lines(active);
        for (p, _) in list_archives(active).unwrap() {
            n += count_lines(&p);
        }
        n
    }

    #[test]
    fn rotating_size_triggers_archive_without_data_loss() {
        let _guard = WRITER_TEST_LOCK.lock();
        let tmp = tempfile::tempdir().unwrap();
        // Tiny byte budget; daily off so only size drives rotation.
        install_rotating(tmp.path(), 200, false, 0, 0);

        for i in 0..20 {
            emit(&format!("event-{i}"));
        }

        let path = runtime_trace_path().unwrap();
        let archives = list_archives(&path).unwrap();
        assert!(
            !archives.is_empty(),
            "size rotation should have produced at least one archive"
        );
        // Rotation archives rather than discards: every event is still on disk.
        assert_eq!(
            total_events(&path),
            20,
            "no events should be lost across rotation"
        );
    }

    #[test]
    fn rotating_daily_archives_previous_day_file() {
        let _guard = WRITER_TEST_LOCK.lock();
        let tmp = tempfile::tempdir().unwrap();
        install_rotating(tmp.path(), 0, true, 0, 0); // daily only

        let path = runtime_trace_path().unwrap();
        fs::create_dir_all(path.parent().unwrap()).unwrap();
        // Seed an active file last written two days ago.
        fs::write(&path, "{\"message\":\"yesterday\"}\n").unwrap();
        set_mtime(&path, SystemTime::now() - Duration::from_secs(2 * 86_400));

        // Today's first event must archive the stale file and start fresh.
        emit("today");

        let archives = list_archives(&path).unwrap();
        assert_eq!(
            archives.len(),
            1,
            "the previous-day file should be archived exactly once"
        );
        let archive_body = fs::read_to_string(&archives[0].0).unwrap();
        assert!(archive_body.contains("yesterday"));
        assert!(!archive_body.contains("today"));
        let active_body = fs::read_to_string(&path).unwrap();
        assert!(active_body.contains("today"));
        assert!(!active_body.contains("yesterday"));
    }

    #[test]
    fn rotating_without_triggers_keeps_all_in_active_file() {
        let _guard = WRITER_TEST_LOCK.lock();
        let tmp = tempfile::tempdir().unwrap();
        // No size budget and no daily boundary: behaves like `full`.
        install_rotating(tmp.path(), 0, false, 0, 0);

        for i in 0..15 {
            emit(&format!("e-{i}"));
        }

        let path = runtime_trace_path().unwrap();
        assert!(
            list_archives(&path).unwrap().is_empty(),
            "no rotation should occur when both triggers are disabled"
        );
        assert_eq!(total_events(&path), 15);
    }

    #[test]
    fn full_mode_persists_all_without_trim_or_rotation() {
        let _guard = WRITER_TEST_LOCK.lock();
        let tmp = tempfile::tempdir().unwrap();
        // Backwards-compat: `full` ignores max_entries and never rotates.
        let cfg = LogConfig {
            log_persistence: "full".into(),
            log_persistence_max_entries: 2,
            ..LogConfig::default()
        };
        init_from_config(&cfg, tmp.path());

        for i in 0..6 {
            emit(&format!("f-{i}"));
        }

        let path = runtime_trace_path().unwrap();
        assert_eq!(total_events(&path), 6, "full keeps every event");
        assert!(list_archives(&path).unwrap().is_empty());
    }

    #[test]
    fn retention_prunes_oldest_archives_by_count() {
        let _guard = WRITER_TEST_LOCK.lock();
        let tmp = tempfile::tempdir().unwrap();
        install_rotating(tmp.path(), 0, false, 2, 0); // keep newest 2

        let path = runtime_trace_path().unwrap();
        fs::create_dir_all(path.parent().unwrap()).unwrap();
        let dir = path.parent().unwrap();
        let base = SystemTime::now() - Duration::from_secs(10 * 86_400);
        let mut archives = Vec::new();
        for i in 0..4u64 {
            let p = dir.join(format!("runtime-trace.2026010{i}-000000.jsonl"));
            fs::write(&p, "{}\n").unwrap();
            set_mtime(&p, base + Duration::from_secs(i * 3_600));
            archives.push(p);
        }

        run_retention(&current_state().unwrap());

        assert!(
            !archives[0].exists() && !archives[1].exists(),
            "the two oldest archives should be pruned"
        );
        assert!(
            archives[2].exists() && archives[3].exists(),
            "the two newest archives should be kept"
        );
    }

    #[test]
    fn retention_prunes_archives_by_age() {
        let _guard = WRITER_TEST_LOCK.lock();
        let tmp = tempfile::tempdir().unwrap();
        install_rotating(tmp.path(), 0, false, 0, 1); // keep <= 1 day, no count cap

        let path = runtime_trace_path().unwrap();
        fs::create_dir_all(path.parent().unwrap()).unwrap();
        let dir = path.parent().unwrap();
        let old = dir.join("runtime-trace.20260101-000000.jsonl");
        let recent = dir.join("runtime-trace.20260109-000000.jsonl");
        fs::write(&old, "{}\n").unwrap();
        fs::write(&recent, "{}\n").unwrap();
        set_mtime(&old, SystemTime::now() - Duration::from_secs(3 * 86_400));
        set_mtime(&recent, SystemTime::now() - Duration::from_secs(3_600));

        run_retention(&current_state().unwrap());

        assert!(!old.exists(), "archive older than the age cap is pruned");
        assert!(recent.exists(), "recent archive is kept");
    }

    #[test]
    fn archive_path_places_stamp_before_extension_and_dedupes() {
        use chrono::TimeZone;
        let tmp = tempfile::tempdir().unwrap();
        let active = tmp.path().join("runtime-trace.jsonl");
        let when = Utc.with_ymd_and_hms(2026, 6, 24, 3, 15, 0).unwrap();

        let a1 = archive_path(&active, when).unwrap();
        assert_eq!(
            a1.file_name().unwrap().to_str().unwrap(),
            "runtime-trace.20260624-031500.jsonl"
        );
        // A same-second collision is disambiguated with a numeric suffix.
        fs::write(&a1, "x").unwrap();
        let a2 = archive_path(&active, when).unwrap();
        assert_eq!(
            a2.file_name().unwrap().to_str().unwrap(),
            "runtime-trace.20260624-031500.1.jsonl"
        );
    }

    #[test]
    fn split_base_ext_cases() {
        assert_eq!(
            split_base_ext("runtime-trace.jsonl"),
            ("runtime-trace", ".jsonl")
        );
        assert_eq!(split_base_ext("a.b.jsonl"), ("a.b", ".jsonl"));
        assert_eq!(split_base_ext("trace"), ("trace", ""));
        assert_eq!(split_base_ext(".hidden"), (".hidden", ""));
    }

    #[test]
    fn is_archive_core_matches_only_generated_shapes() {
        // `core` is the suffix with the base prefix and extension stripped, i.e.
        // exactly what `archive_path` puts between them: `<stamp>` or
        // `<stamp>.<counter>`.
        assert!(is_archive_core("20260624-031500")); // <stamp>
        assert!(is_archive_core("20260624-031500.1")); // <stamp>.<counter>
        assert!(is_archive_core("20260624-031500.42"));
        // Foreign siblings that merely *start* with a stamp must be rejected, so
        // they are never pruned (this is the boundary the review flagged).
        assert!(!is_archive_core("20260624-031500.backup"));
        assert!(!is_archive_core("20260624-031500.notes"));
        assert!(!is_archive_core("20260624-031500.1.2")); // counter is not multi-segment
        assert!(!is_archive_core("20260624-031500.")); // empty counter
        // Not a stamp at all.
        assert!(!is_archive_core("notes"));
        assert!(!is_archive_core("migrate.123.456"));
        assert!(!is_archive_core("2026-06-24")); // dashes in the wrong place
        assert!(!is_archive_core("20260624-0315000")); // too long
        // is_stamp is strict about the exact 15-char shape.
        assert!(is_stamp("20260624-031500"));
        assert!(!is_stamp("20260624-03150")); // too short
        assert!(!is_stamp("2026062a-031500")); // non-digit
    }

    #[test]
    fn rotation_through_append_prunes_to_retention_cap() {
        let _guard = WRITER_TEST_LOCK.lock();
        let tmp = tempfile::tempdir().unwrap();
        // Every append rotates (max_bytes = 1); retention keeps the newest 2.
        install_rotating(tmp.path(), 1, false, 2, 0);

        for i in 0..10 {
            emit(&format!("event-{i}"));
        }

        let path = runtime_trace_path().unwrap();
        // Retention ran as a side effect of the real append path, capping the
        // archive set even though many more rotations occurred.
        assert_eq!(
            list_archives(&path).unwrap().len(),
            2,
            "retention cap should hold across rotations driven by append_line"
        );
    }

    #[test]
    fn rotating_size_and_daily_both_active() {
        let _guard = WRITER_TEST_LOCK.lock();
        let tmp = tempfile::tempdir().unwrap();
        // Both triggers on, no retention so every event is preserved.
        install_rotating(tmp.path(), 200, true, 0, 0);

        let path = runtime_trace_path().unwrap();
        fs::create_dir_all(path.parent().unwrap()).unwrap();
        // Seed a stale (two-days-ago) active file.
        fs::write(&path, "{\"message\":\"old-day\"}\n").unwrap();
        set_mtime(&path, SystemTime::now() - Duration::from_secs(2 * 86_400));

        for i in 0..20 {
            emit(&format!("burst-{i}"));
        }

        let archives = list_archives(&path).unwrap();
        // Daily rotation archives the stale file; size rotation adds more.
        assert!(
            archives.len() >= 2,
            "expected a daily archive plus size archives, got {}",
            archives.len()
        );
        // 1 seeded event + 20 emitted, all preserved across active + archives.
        assert_eq!(total_events(&path), 21, "no events lost with both triggers");
        // The stale day's event lives in an archive, never in the active file.
        // (The active file may not exist if the final append also rotated; it is
        // recreated lazily on the next append, exactly as the reader expects.)
        let active = fs::read_to_string(&path).unwrap_or_default();
        assert!(
            !active.contains("old-day"),
            "stale day's event must not remain in the active file"
        );
        let archived_old_day = list_archives(&path).unwrap().iter().any(|(p, _)| {
            fs::read_to_string(p)
                .unwrap_or_default()
                .contains("old-day")
        });
        assert!(
            archived_old_day,
            "stale day's event must be preserved in an archive"
        );
    }

    #[test]
    fn rotating_extensionless_path_isolates_foreign_siblings() {
        let _guard = WRITER_TEST_LOCK.lock();
        let tmp = tempfile::tempdir().unwrap();
        // Custom path with no extension; every append rotates; keep newest 1.
        let cfg = LogConfig {
            log_persistence: "rotating".into(),
            log_persistence_path: tmp.path().join("trace").to_string_lossy().into_owned(),
            log_persistence_max_bytes: 1,
            log_persistence_rotate_daily: false,
            log_persistence_retention_max_files: 1,
            ..LogConfig::default()
        };
        init_from_config(&cfg, tmp.path());
        let path = runtime_trace_path().unwrap();

        // Two foreign siblings that share the base prefix must never be pruned:
        // one with no stamp, and one that *starts* with a valid stamp but is not
        // a shape `archive_path` generates (the boundary the review flagged).
        let foreign_plain = tmp.path().join("trace.notes");
        let foreign_stamped = tmp.path().join("trace.20260101-000000.notes");
        fs::write(&foreign_plain, "keep me\n").unwrap();
        fs::write(&foreign_stamped, "keep me too\n").unwrap();

        for i in 0..6 {
            emit(&format!("e-{i}"));
        }

        let archives = list_archives(&path).unwrap();
        assert_eq!(
            archives.len(),
            1,
            "retention cap applies for extension-less paths"
        );
        assert!(
            archives
                .iter()
                .all(|(p, _)| p != &foreign_plain && p != &foreign_stamped),
            "foreign siblings must not be classified as archives"
        );
        assert!(
            foreign_plain.exists() && foreign_stamped.exists(),
            "foreign siblings must survive retention"
        );
        for (p, _) in &archives {
            let name = p.file_name().unwrap().to_str().unwrap();
            assert!(
                is_archive_core(name.strip_prefix("trace.").unwrap()),
                "archive {name} must carry the exact archive shape"
            );
        }
    }

    #[test]
    fn retention_spares_stamp_prefixed_foreign_sibling_with_extension() {
        let _guard = WRITER_TEST_LOCK.lock();
        let tmp = tempfile::tempdir().unwrap();
        // Default `.jsonl` path; every append rotates; keep newest 1.
        install_rotating(tmp.path(), 1, false, 1, 0);

        let path = runtime_trace_path().unwrap();
        fs::create_dir_all(path.parent().unwrap()).unwrap();
        let dir = path.parent().unwrap();

        // A foreign file that starts with a valid stamp and carries the `.jsonl`
        // extension, but is NOT a shape `archive_path` generates. It must never
        // be classified as an archive nor pruned, no matter how many real
        // rotations the retention cap triggers.
        let foreign = dir.join("runtime-trace.20260101-000000.backup.jsonl");
        fs::write(&foreign, "do not delete\n").unwrap();

        for i in 0..6 {
            emit(&format!("e-{i}"));
        }

        let archives = list_archives(&path).unwrap();
        assert!(
            archives.iter().all(|(p, _)| p != &foreign),
            "stamp-prefixed foreign sibling must not be classified as an archive"
        );
        assert!(
            foreign.exists(),
            "stamp-prefixed foreign sibling must survive retention"
        );
        // Real archives are still pruned to the cap.
        assert_eq!(archives.len(), 1, "real archives are still capped");
    }
}
