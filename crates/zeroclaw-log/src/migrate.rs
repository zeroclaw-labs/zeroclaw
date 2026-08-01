//! One-shot, streaming, in-place migration from schema_version 1 rows
//! to schema_version 2.

use std::fs::{self, File, OpenOptions};
use std::io::{BufRead, BufReader, BufWriter, Write};
use std::path::Path;

use anyhow::{Context, Result};
use serde_json::Value;

use crate::event::LogEvent;

/// Detect-and-migrate. No-op when the file is already at schema_version 2.
pub fn migrate_legacy_jsonl_in_place(path: &Path) -> Result<()> {
    if !path.exists() {
        return Ok(());
    }
    if file_already_at_current_schema(path)? {
        return Ok(());
    }

    let tmp = path.with_extension(format!(
        "migrate.{}.{}",
        std::process::id(),
        chrono::Utc::now().timestamp_nanos_opt().unwrap_or_default()
    ));

    let mut opts = OpenOptions::new();
    opts.create_new(true).write(true);
    #[cfg(unix)]
    {
        use std::os::unix::fs::OpenOptionsExt;
        opts.mode(0o600);
    }
    let out_file = opts
        .open(&tmp)
        .with_context(|| format!("creating migration temp {}", tmp.display()))?;
    let mut out = BufWriter::new(out_file);

    let in_file =
        File::open(path).with_context(|| format!("opening log for migrate: {}", path.display()))?;
    let reader = BufReader::new(in_file);
    let mut migrated: u64 = 0;
    let mut kept: u64 = 0;

    for line in reader.lines() {
        let line = line.context("reading log line during migrate")?;
        let trimmed = line.trim();
        if trimmed.is_empty() {
            continue;
        }
        let value: Value = match serde_json::from_str(trimmed) {
            Ok(v) => v,
            Err(err) => {
                tracing::warn!(
                    target: "zeroclaw_log",
                    error = ?err,
                    "log: skipping malformed line during migrate"
                );
                continue;
            }
        };
        let migrated_value = if is_legacy_shape(&value) {
            migrated += 1;
            convert_legacy_to_current(value)
        } else {
            kept += 1;
            value
        };
        serde_json::to_writer(&mut out, &migrated_value).context("writing migrated line")?;
        out.write_all(b"\n").context("writing migrated newline")?;
    }
    out.flush().context("flushing migrated file")?;
    out.into_inner()
        .context("taking migrated file out of buf writer")?
        .sync_data()
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

fn file_already_at_current_schema(path: &Path) -> Result<bool> {
    // Sample the LAST few lines: a file that's been written by the new
    // writer (or migrated previously) will have current-shape rows at
    // the tail. Streaming the tail (without rev-iterating) is annoying;
    // a forward scan of just the first non-empty line is a cheap heuristic.
    let file = File::open(path)
        .with_context(|| format!("opening log for schema check: {}", path.display()))?;
    let reader = BufReader::new(file);
    for line in reader.lines() {
        let line = line.context("reading log line for schema check")?;
        let trimmed = line.trim();
        if trimmed.is_empty() {
            continue;
        }
        if let Ok(v) = serde_json::from_str::<Value>(trimmed) {
            return Ok(!is_legacy_shape(&v));
        }
        // Malformed first line: assume legacy (migration is best-effort).
        return Ok(false);
    }
    Ok(true) // empty file — nothing to migrate
}

fn is_legacy_shape(v: &Value) -> bool {
    let has_legacy = v.get("timestamp").is_some();
    let has_new = v.get("@timestamp").is_some();
    has_legacy && !has_new
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
