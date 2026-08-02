//! One-shot, streaming, in-place migration from schema_version 1 rows
//! to schema_version 2.

use std::fs::{self, File, OpenOptions};
use std::io::{BufRead, BufReader, BufWriter, Write};
use std::path::{Path, PathBuf};

use anyhow::{Context, Result};
use serde_json::Value;

use crate::event::LogEvent;

/// Detect-and-migrate. No-op when the file contains no recognized legacy rows.
pub fn migrate_legacy_jsonl_in_place(path: &Path) -> Result<()> {
    if !path.exists() {
        return Ok(());
    }
    if !file_needs_migration(path)? {
        return Ok(());
    }

    let tmp = path.with_extension(format!(
        "migrate.{}.{}",
        std::process::id(),
        chrono::Utc::now().timestamp_nanos_opt().unwrap_or_default()
    ));
    let (out_file, mut temp_cleanup) = create_migration_temp(&tmp)?;
    let mut out = BufWriter::new(out_file);

    let in_file =
        File::open(path).with_context(|| format!("opening log for migrate: {}", path.display()))?;
    let mut reader = BufReader::new(in_file);
    let mut migrated: u64 = 0;
    let mut kept: u64 = 0;
    let mut needs_eof_separator = false;

    let mut line = Vec::new();
    loop {
        line.clear();
        if reader
            .read_until(b'\n', &mut line)
            .context("reading log line during migrate")?
            == 0
        {
            break;
        }
        needs_eof_separator = !line.ends_with(b"\n");
        let trimmed = trim_ascii_whitespace(&line);
        if trimmed.is_empty() {
            out.write_all(&line)
                .context("preserving blank line during migrate")?;
            continue;
        }
        let value: Value = match serde_json::from_slice(trimmed) {
            Ok(v) => v,
            Err(err) => {
                tracing::warn!(
                    target: "zeroclaw_log",
                    error = ?err,
                    "log: preserving malformed line during migrate"
                );
                out.write_all(&line)
                    .context("preserving malformed line during migrate")?;
                continue;
            }
        };
        if is_legacy_shape(&value) {
            migrated += 1;
            serde_json::to_writer(&mut out, &convert_legacy_to_current(value))
                .context("writing migrated line")?;
            out.write_all(line_ending(&line))
                .context("preserving migrated line ending")?;
        } else {
            kept += 1;
            out.write_all(&line)
                .context("preserving current line during migrate")?;
        }
    }
    if needs_eof_separator {
        out.write_all(b"\n")
            .context("terminating migrated file for safe append")?;
    }
    out.flush().context("flushing migrated file")?;
    out.into_inner()
        .context("taking migrated file out of buf writer")?
        .sync_all()
        .context("fsync migrated file")?;

    #[cfg(unix)]
    {
        use std::os::unix::fs::PermissionsExt;
        let _ = fs::set_permissions(&tmp, fs::Permissions::from_mode(0o600));
    }
    fs::rename(&tmp, path).with_context(|| {
        format!(
            "renaming migration temp {} → {}",
            tmp.display(),
            path.display()
        )
    })?;
    temp_cleanup.disarm();
    if let Err(err) = sync_parent_directory(path) {
        tracing::warn!(
            target: "zeroclaw_log",
            error = ?err,
            path = %path.display(),
            "log: migration committed but parent directory sync failed"
        );
    }

    if migrated > 0 {
        tracing::info!(
            target: "zeroclaw_log",
            migrated,
            kept,
            path = %path.display(),
            "log: migrated legacy schema-1 rows to schema-2"
        );
    }
    Ok(())
}

struct MigrationTempCleanup {
    path: PathBuf,
    armed: bool,
}

fn create_migration_temp(path: &Path) -> Result<(File, MigrationTempCleanup)> {
    let mut opts = OpenOptions::new();
    opts.create_new(true).write(true);
    #[cfg(unix)]
    {
        use std::os::unix::fs::OpenOptionsExt;
        opts.mode(0o600);
    }
    let file = opts
        .open(path)
        .with_context(|| format!("creating migration temp {}", path.display()))?;
    Ok((file, MigrationTempCleanup::new(path.to_path_buf())))
}

impl MigrationTempCleanup {
    fn new(path: PathBuf) -> Self {
        Self { path, armed: true }
    }

    fn disarm(&mut self) {
        self.armed = false;
    }
}

impl Drop for MigrationTempCleanup {
    fn drop(&mut self) {
        if self.armed {
            let _ = fs::remove_file(&self.path);
        }
    }
}

fn file_needs_migration(path: &Path) -> Result<bool> {
    // Mixed-schema files are possible after an interrupted rollout. Scan every
    // parseable row and migrate if any row still has legacy shape. Malformed
    // rows are opaque data: migration preserves them but cannot repair them.
    let file = File::open(path)
        .with_context(|| format!("opening log for schema check: {}", path.display()))?;
    let mut reader = BufReader::new(file);
    let mut line = Vec::new();
    loop {
        line.clear();
        if reader
            .read_until(b'\n', &mut line)
            .context("reading log line for schema check")?
            == 0
        {
            break;
        }
        let trimmed = trim_ascii_whitespace(&line);
        if trimmed.is_empty() {
            continue;
        }
        if let Ok(v) = serde_json::from_slice::<Value>(trimmed)
            && is_legacy_shape(&v)
        {
            return Ok(true);
        }
    }
    Ok(false)
}

fn trim_ascii_whitespace(mut bytes: &[u8]) -> &[u8] {
    while bytes.first().is_some_and(u8::is_ascii_whitespace) {
        bytes = &bytes[1..];
    }
    while bytes.last().is_some_and(u8::is_ascii_whitespace) {
        bytes = &bytes[..bytes.len() - 1];
    }
    bytes
}

fn line_ending(line: &[u8]) -> &'static [u8] {
    if line.ends_with(b"\r\n") {
        b"\r\n"
    } else if line.ends_with(b"\n") {
        b"\n"
    } else {
        b""
    }
}

fn sync_parent_directory(path: &Path) -> Result<()> {
    #[cfg(unix)]
    {
        let parent = path
            .parent()
            .filter(|parent| !parent.as_os_str().is_empty())
            .unwrap_or_else(|| Path::new("."));
        File::open(parent)
            .with_context(|| format!("opening log directory for fsync: {}", parent.display()))?
            .sync_all()
            .with_context(|| format!("fsync log directory after migrate: {}", parent.display()))?;
    }
    #[cfg(not(unix))]
    let _ = path;
    Ok(())
}

fn is_legacy_shape(v: &Value) -> bool {
    v.get("timestamp").and_then(Value::as_str).is_some()
        && v.get("event_type").and_then(Value::as_str).is_some()
        && v.get("@timestamp").is_none()
}

fn convert_legacy_to_current(legacy: Value) -> Value {
    let get_str = |key: &str| -> Option<String> {
        legacy.get(key).and_then(Value::as_str).map(str::to_string)
    };

    let id = get_str("id").unwrap_or_else(|| uuid::Uuid::new_v4().to_string());
    let timestamp = get_str("timestamp")
        .unwrap_or_else(|| chrono::Utc::now().to_rfc3339_opts(chrono::SecondsFormat::Millis, true));
    let event_type = get_str("event_type").unwrap_or_else(|| "legacy".to_string());
    let success = legacy.get("success").and_then(Value::as_bool);
    let outcome = match success {
        Some(true) => "success",
        Some(false) => "failure",
        None => "unknown",
    };

    let mut zeroclaw = serde_json::Map::new();
    if let Some(agent) = get_str("agent_alias") {
        zeroclaw.insert("agent_alias".into(), Value::String(agent));
    }
    use crate::event::{alias_field, type_field};
    if let Some(channel) = get_str("channel") {
        // Legacy "channel" might be bare type or composite. If it
        // contains `.`, treat as composite and split.
        if let Some((ty, alias)) = channel.split_once('.') {
            zeroclaw.insert("channel".into(), Value::String(channel.clone()));
            zeroclaw.insert(type_field("channel"), Value::String(ty.to_string()));
            zeroclaw.insert(alias_field("channel"), Value::String(alias.to_string()));
        } else {
            zeroclaw.insert("channel".into(), Value::String(channel.clone()));
            zeroclaw.insert(type_field("channel"), Value::String(channel));
        }
    }
    if let Some(mp) = get_str("model_provider") {
        if let Some((ty, alias)) = mp.split_once('.') {
            zeroclaw.insert("model_provider".into(), Value::String(mp.clone()));
            zeroclaw.insert(type_field("model_provider"), Value::String(ty.to_string()));
            zeroclaw.insert(
                alias_field("model_provider"),
                Value::String(alias.to_string()),
            );
        } else {
            zeroclaw.insert("model_provider".into(), Value::String(mp.clone()));
            zeroclaw.insert(type_field("model_provider"), Value::String(mp));
        }
    }
    if let Some(model) = get_str("model") {
        zeroclaw.insert("model".into(), Value::String(model));
    }

    let trace_id = get_str("turn_id");
    let message = get_str("message");
    let attributes = legacy.get("payload").cloned().unwrap_or(Value::Null);

    // Map event_type → category heuristically. Unknown types fall under
    // "system".
    let category = category_for_action(&event_type);
    let severity = if matches!(success, Some(false)) {
        ("WARN", 13u8)
    } else {
        ("INFO", 9u8)
    };

    serde_json::json!({
        "id": id,
        "@timestamp": timestamp,
        "severity_number": severity.1,
        "severity_text": severity.0,
        "event": {
            "category": category,
            "action": event_type,
            "outcome": outcome,
        },
        "service": { "name": "zeroclaw", "version": env!("CARGO_PKG_VERSION") },
        "trace_id": trace_id,
        "zeroclaw": Value::Object(zeroclaw),
        "message": message,
        "attributes": attributes,
        "schema_version": LogEvent::SCHEMA_VERSION,
    })
}

fn category_for_action(action: &str) -> &'static str {
    match action {
        "llm_request" | "agent_start" | "agent_end" => "agent",
        "tool_call" | "tool_call_start" | "tool_call_result" => "tool",
        "channel_message_inbound" | "channel_send" => "channel",
        "cron_run" => "cron",
        "memory_store" | "memory_recall" | "memory_forget" => "memory",
        "session_open" | "session_close" => "session",
        "error" => "system",
        "gateway_ws_turn" => "session",
        _ => "system",
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use std::io::Write;

    fn write_jsonl(path: &Path, lines: &[&str]) {
        let mut f = File::create(path).unwrap();
        for line in lines {
            f.write_all(line.as_bytes()).unwrap();
            f.write_all(b"\n").unwrap();
        }
    }

    fn read_all_lines(path: &Path) -> Vec<String> {
        let f = File::open(path).unwrap();
        BufReader::new(f)
            .lines()
            .map(|l| l.unwrap())
            .filter(|l| !l.trim().is_empty())
            .collect()
    }

    #[test]
    fn migrates_legacy_to_current_shape() {
        let tmp = tempfile::tempdir().unwrap();
        let path = tmp.path().join("trace.jsonl");
        write_jsonl(
            &path,
            &[
                r#"{"id":"id-1","timestamp":"2026-05-15T19:00:00Z","event_type":"llm_request","channel":"discord.clamps","model_provider":"anthropic.clamps","model":"claude-sonnet-4-6","turn_id":"t1","success":true,"agent_alias":"clamps","message":"call","payload":{"tokens":10}}"#,
            ],
        );

        migrate_legacy_jsonl_in_place(&path).unwrap();

        let lines = read_all_lines(&path);
        let v: Value = serde_json::from_str(&lines[0]).unwrap();
        assert_eq!(v["@timestamp"], "2026-05-15T19:00:00Z");
        assert!(v.get("timestamp").is_none());
        assert_eq!(v["event"]["action"], "llm_request");
        assert_eq!(v["event"]["category"], "agent");
        assert_eq!(v["event"]["outcome"], "success");
        assert_eq!(v["zeroclaw"]["agent_alias"], "clamps");
        assert_eq!(v["zeroclaw"]["channel"], "discord.clamps");
        assert_eq!(v["zeroclaw"]["channel_type"], "discord");
        assert_eq!(v["zeroclaw"]["channel_alias"], "clamps");
        assert_eq!(v["zeroclaw"]["model_provider"], "anthropic.clamps");
        assert_eq!(v["trace_id"], "t1");
        assert_eq!(v["attributes"]["tokens"], 10);
        assert_eq!(v["schema_version"], LogEvent::SCHEMA_VERSION);
    }

    #[test]
    fn already_current_is_noop() {
        let tmp = tempfile::tempdir().unwrap();
        let path = tmp.path().join("trace.jsonl");
        let line = r#"{"id":"id","@timestamp":"2026-05-15T19:00:00Z","severity_number":9,"severity_text":"INFO","event":{"category":"agent","action":"x","outcome":"success"},"service":{"name":"zeroclaw","version":"0.7.5"},"zeroclaw":{},"schema_version":2}"#;
        write_jsonl(&path, &[line]);
        migrate_legacy_jsonl_in_place(&path).unwrap();
        let lines = read_all_lines(&path);
        let v: Value = serde_json::from_str(&lines[0]).unwrap();
        assert_eq!(v["schema_version"], 2);
    }

    #[test]
    fn mixed_schema_file_migrates_legacy_rows_after_current_rows() {
        let tmp = tempfile::tempdir().unwrap();
        let path = tmp.path().join("trace.jsonl");
        let current = r#"{"id":"current","@timestamp":"2026-05-15T19:00:00Z","severity_number":9,"severity_text":"INFO","event":{"category":"agent","action":"x","outcome":"success"},"service":{"name":"zeroclaw","version":"0.8.3"},"zeroclaw":{},"schema_version":2}"#;
        let legacy = r#"{"id":"legacy","timestamp":"2026-05-15T19:01:00Z","event_type":"agent_end","success":true}"#;
        write_jsonl(&path, &[current, legacy]);

        migrate_legacy_jsonl_in_place(&path).unwrap();

        let lines = read_all_lines(&path);
        assert_eq!(lines.len(), 2);
        let first: Value = serde_json::from_str(&lines[0]).unwrap();
        let second: Value = serde_json::from_str(&lines[1]).unwrap();
        assert_eq!(first["id"], "current");
        assert_eq!(first["schema_version"], LogEvent::SCHEMA_VERSION);
        assert_eq!(second["id"], "legacy");
        assert_eq!(second["@timestamp"], "2026-05-15T19:01:00Z");
        assert!(second.get("timestamp").is_none());
        assert_eq!(second["schema_version"], LogEvent::SCHEMA_VERSION);
    }

    #[test]
    fn migration_preserves_malformed_rows() {
        let tmp = tempfile::tempdir().unwrap();
        let path = tmp.path().join("trace.jsonl");
        let malformed = "  {not valid json}  ";
        let legacy =
            r#"{"id":"legacy","timestamp":"2026-05-15T19:01:00Z","event_type":"agent_end"}"#;
        write_jsonl(&path, &[malformed, legacy]);

        migrate_legacy_jsonl_in_place(&path).unwrap();

        let contents = fs::read_to_string(&path).unwrap();
        assert!(
            contents.lines().any(|line| line == malformed),
            "malformed row content must survive migration"
        );
        let migrated = contents
            .lines()
            .find_map(|line| serde_json::from_str::<Value>(line).ok())
            .expect("legacy row converted to current JSON");
        assert_eq!(migrated["id"], "legacy");
        assert_eq!(migrated["schema_version"], LogEvent::SCHEMA_VERSION);
    }

    #[test]
    fn migration_preserves_nonlegacy_bytes_and_line_endings() {
        let tmp = tempfile::tempdir().unwrap();
        let path = tmp.path().join("trace.jsonl");
        let preserved = concat!(
            "{\"id\":\"current\",\"@timestamp\":\"2026-05-15T19:00:00Z\",",
            "\"number\":1.00,\"number\":2}\r\n",
            "\r\n",
            "  {not valid json}  \n"
        );
        let legacy =
            r#"{"id":"legacy","timestamp":"2026-05-15T19:01:00Z","event_type":"agent_end"}"#;
        fs::write(&path, [preserved.as_bytes(), legacy.as_bytes()].concat()).unwrap();

        migrate_legacy_jsonl_in_place(&path).unwrap();

        let contents = fs::read(&path).unwrap();
        assert!(contents.starts_with(preserved.as_bytes()));
        assert!(
            contents.ends_with(b"\n"),
            "migration must add the separator required for the next append"
        );
        let migrated: Value =
            serde_json::from_slice(trim_ascii_whitespace(&contents[preserved.len()..])).unwrap();
        assert_eq!(migrated["id"], "legacy");
        assert_eq!(migrated["schema_version"], LogEvent::SCHEMA_VERSION);
    }

    #[test]
    fn timestamp_lookalike_is_preserved_during_real_migration() {
        let tmp = tempfile::tempdir().unwrap();
        let path = tmp.path().join("trace.jsonl");
        let lookalike = r#"{"timestamp":"2026-05-15T19:00:00Z","note":"foreign record"}"#;
        let legacy =
            r#"{"id":"legacy","timestamp":"2026-05-15T19:01:00Z","event_type":"agent_end"}"#;
        write_jsonl(&path, &[lookalike, legacy]);

        migrate_legacy_jsonl_in_place(&path).unwrap();

        let lines = read_all_lines(&path);
        assert_eq!(lines[0], lookalike);
        let migrated: Value = serde_json::from_str(&lines[1]).unwrap();
        assert_eq!(migrated["id"], "legacy");
        assert_eq!(migrated["schema_version"], LogEvent::SCHEMA_VERSION);
    }

    #[test]
    fn temp_cleanup_removes_only_uncommitted_file() {
        let tmp = tempfile::tempdir().unwrap();
        let path = tmp.path().join("migration.tmp");
        fs::write(&path, "pending").unwrap();
        {
            let _cleanup = MigrationTempCleanup::new(path.clone());
        }
        assert!(!path.exists(), "uncommitted temp file must be removed");

        fs::write(&path, "committed").unwrap();
        {
            let mut cleanup = MigrationTempCleanup::new(path.clone());
            cleanup.disarm();
        }
        assert_eq!(fs::read_to_string(&path).unwrap(), "committed");
    }

    #[test]
    fn temp_creation_collision_preserves_existing_file() {
        let tmp = tempfile::tempdir().unwrap();
        let path = tmp.path().join("migration.tmp");
        fs::write(&path, "owned elsewhere").unwrap();

        assert!(create_migration_temp(&path).is_err());
        assert_eq!(fs::read_to_string(&path).unwrap(), "owned elsewhere");
    }

    #[test]
    fn empty_file_is_noop() {
        let tmp = tempfile::tempdir().unwrap();
        let path = tmp.path().join("trace.jsonl");
        File::create(&path).unwrap();
        migrate_legacy_jsonl_in_place(&path).unwrap();
        let lines = read_all_lines(&path);
        assert!(lines.is_empty());
    }

    #[test]
    fn category_for_action_maps_every_known_action() {
        assert_eq!(category_for_action("llm_request"), "agent");
        assert_eq!(category_for_action("agent_start"), "agent");
        assert_eq!(category_for_action("agent_end"), "agent");
        assert_eq!(category_for_action("tool_call"), "tool");
        assert_eq!(category_for_action("tool_call_start"), "tool");
        assert_eq!(category_for_action("tool_call_result"), "tool");
        assert_eq!(category_for_action("channel_message_inbound"), "channel");
        assert_eq!(category_for_action("channel_send"), "channel");
        assert_eq!(category_for_action("cron_run"), "cron");
        assert_eq!(category_for_action("memory_store"), "memory");
        assert_eq!(category_for_action("memory_recall"), "memory");
        assert_eq!(category_for_action("memory_forget"), "memory");
        assert_eq!(category_for_action("session_open"), "session");
        assert_eq!(category_for_action("session_close"), "session");
        assert_eq!(category_for_action("gateway_ws_turn"), "session");
        assert_eq!(category_for_action("error"), "system");
    }

    #[test]
    fn category_for_action_defaults_unknown_to_system() {
        assert_eq!(category_for_action("totally_unknown"), "system");
        assert_eq!(category_for_action(""), "system");
    }
}
