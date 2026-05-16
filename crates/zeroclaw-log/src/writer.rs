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

use anyhow::{Context, Result};
use parking_lot::Mutex;
use serde_json::Value;
use crate::broadcast::current_broadcast_hook;
use crate::config::{LogConfig, ResolvedPolicy, StoragePolicy};
use crate::event::LogEvent;
use crate::migrate;
use crate::observer_bridge;

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

    if state.policy.storage == StoragePolicy::Rolling {
        trim_to_last_entries(state)?;
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

/// Shared test-time mutex for tests that mutate the global writer state.
/// Re-exported `pub(crate)` so `macro::tests` etc. can serialize against
/// the same lock as `writer::tests`.
#[cfg(test)]
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
}
