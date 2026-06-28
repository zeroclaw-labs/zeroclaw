//! Filesystem → SOP event fan-in listener.
//!
//! This is NOT a `Channel` trait implementor — it routes file changes
//! to the SOP engine via `dispatch_sop_event`, not to the chat loop.

use std::collections::HashMap;
use std::path::{Path, PathBuf};
use std::sync::{Arc, Mutex};
use std::time::{Duration, Instant};

use anyhow::Result;
use notify::{Event, EventKind, RecursiveMode, Watcher};
use sha2::{Digest, Sha256};

use zeroclaw_config::schema::FilesystemConfig;
use zeroclaw_runtime::sop::audit::SopAuditLogger;
use zeroclaw_runtime::sop::dispatch::{dispatch_sop_event, process_headless_results};
use zeroclaw_runtime::sop::engine::{SopEngine, now_iso8601};
use zeroclaw_runtime::sop::types::{FilesystemEventKind, SopEvent, SopTriggerSource};

pub async fn run_filesystem_sop_listener(
    config: &FilesystemConfig,
    engine: Arc<Mutex<SopEngine>>,
    audit: Arc<SopAuditLogger>,
) -> Result<()> {
    config.validate()?;

    let (raw_tx, raw_rx) = std::sync::mpsc::channel::<Event>();
    let mut watcher = notify::recommended_watcher(move |res: notify::Result<Event>| {
        if let Ok(event) = res {
            let _ = raw_tx.send(event);
        }
    })?;

    let mode = if config.recursive {
        RecursiveMode::Recursive
    } else {
        RecursiveMode::NonRecursive
    };
    for path in &config.paths {
        watcher.watch(Path::new(path), mode)?;
        ::zeroclaw_log::record!(
            INFO,
            ::zeroclaw_log::Event::new(module_path!(), ::zeroclaw_log::Action::Note)
                .with_attrs(::serde_json::json!({ "path": path })),
            "Filesystem SOP listener: watching ''"
        );
    }

    zeroclaw_runtime::health::mark_component_ok("filesystem");

    let enabled_events = parse_event_kinds(&config.events);
    let include = compile_globs(&config.include);
    let exclude = compile_globs(&config.exclude);
    let debounce = Duration::from_millis(config.debounce_ms);
    let settle = Duration::from_millis(config.settle_ms);
    let mut last_seen: HashMap<(String, FilesystemEventKind), Instant> = HashMap::new();

    loop {
        let event = match raw_rx.recv() {
            Ok(e) => e,
            Err(_) => return Ok(()),
        };

        let Some(kind) = normalize_event_kind(&event.kind) else {
            continue;
        };
        if !enabled_events.contains(&kind) {
            continue;
        }

        let (path, old_path) = resolve_paths(&event);
        let Some(path) = path else {
            continue;
        };
        let path_str = path.to_string_lossy().to_string();

        if !matches_globs(&path_str, &include, &exclude) {
            continue;
        }

        let dedup_key = (path_str.clone(), kind);
        let now = Instant::now();
        if let Some(prev) = last_seen.get(&dedup_key)
            && now.duration_since(*prev) < debounce
        {
            last_seen.insert(dedup_key, now);
            continue;
        }
        last_seen.insert(dedup_key, now);

        if !settle.is_zero() {
            tokio::time::sleep(settle).await;
        }

        let payload = build_payload(kind, &path, old_path.as_deref(), config);

        let sop_event = SopEvent {
            source: SopTriggerSource::Filesystem,
            topic: Some(path_str.clone()),
            payload: Some(payload.to_string()),
            timestamp: now_iso8601(),
        };

        ::zeroclaw_log::record!(
            INFO,
            ::zeroclaw_log::Event::new(module_path!(), ::zeroclaw_log::Action::Note)
                .with_attrs(::serde_json::json!({ "path": path_str, "event": kind.to_string() })),
            "Filesystem SOP listener: dispatching '' ''"
        );

        let results = dispatch_sop_event(&engine, &audit, sop_event).await;
        process_headless_results(&results);
    }
}

fn parse_event_kinds(events: &[String]) -> Vec<FilesystemEventKind> {
    events
        .iter()
        .filter_map(|e| match e.as_str() {
            "created" => Some(FilesystemEventKind::Created),
            "modified" => Some(FilesystemEventKind::Modified),
            "deleted" => Some(FilesystemEventKind::Deleted),
            "renamed" => Some(FilesystemEventKind::Renamed),
            _ => None,
        })
        .collect()
}

fn normalize_event_kind(kind: &EventKind) -> Option<FilesystemEventKind> {
    match kind {
        EventKind::Create(_) => Some(FilesystemEventKind::Created),
        EventKind::Modify(notify::event::ModifyKind::Name(_)) => Some(FilesystemEventKind::Renamed),
        EventKind::Modify(_) => Some(FilesystemEventKind::Modified),
        EventKind::Remove(_) => Some(FilesystemEventKind::Deleted),
        _ => None,
    }
}

fn resolve_paths(event: &Event) -> (Option<PathBuf>, Option<PathBuf>) {
    let is_rename = matches!(
        event.kind,
        EventKind::Modify(notify::event::ModifyKind::Name(_))
    );
    if is_rename && event.paths.len() >= 2 {
        (Some(event.paths[1].clone()), Some(event.paths[0].clone()))
    } else {
        (event.paths.first().cloned(), None)
    }
}

fn compile_globs(patterns: &[String]) -> Vec<glob::Pattern> {
    patterns
        .iter()
        .filter_map(|p| glob::Pattern::new(p).ok())
        .collect()
}

fn matches_globs(path: &str, include: &[glob::Pattern], exclude: &[glob::Pattern]) -> bool {
    if !include.is_empty() && !include.iter().any(|p| p.matches(path)) {
        return false;
    }
    if exclude.iter().any(|p| p.matches(path)) {
        return false;
    }
    true
}

fn extension_of(path: &Path) -> String {
    path.extension()
        .and_then(|e| e.to_str())
        .unwrap_or_default()
        .to_string()
}

fn file_name_of(path: &Path) -> String {
    path.file_name()
        .and_then(|n| n.to_str())
        .unwrap_or_default()
        .to_string()
}

fn build_payload(
    kind: FilesystemEventKind,
    path: &Path,
    old_path: Option<&Path>,
    config: &FilesystemConfig,
) -> ::serde_json::Value {
    let path_str = path.to_string_lossy().to_string();
    let mut payload = ::serde_json::json!({
        "event": kind.to_string(),
        "path": path_str,
        "file_name": file_name_of(path),
        "extension": extension_of(path),
    });

    let obj = payload.as_object_mut().expect("payload is an object");

    match kind {
        FilesystemEventKind::Created | FilesystemEventKind::Modified => {
            if let Ok(meta) = std::fs::metadata(path) {
                obj.insert("size".into(), ::serde_json::json!(meta.len()));
                if let Ok(modified) = meta.modified() {
                    let dt: chrono::DateTime<chrono::Utc> = modified.into();
                    obj.insert(
                        "modified_at".into(),
                        ::serde_json::json!(dt.format("%Y-%m-%dT%H:%M:%SZ").to_string()),
                    );
                }
                if let Some(hash) = hash_file(path, config.max_content_bytes) {
                    obj.insert("hash".into(), ::serde_json::json!(hash));
                }
                if config.read_content
                    && let Some(content) = read_capped(path, config.max_content_bytes)
                {
                    obj.insert("content".into(), ::serde_json::json!(content));
                }
            }
        }
        FilesystemEventKind::Renamed => {
            if let Some(old) = old_path {
                obj.insert(
                    "old_path".into(),
                    ::serde_json::json!(old.to_string_lossy().to_string()),
                );
            }
        }
        FilesystemEventKind::Deleted => {}
    }

    payload
}

fn hash_file(path: &Path, max_bytes: usize) -> Option<String> {
    let bytes = read_capped_bytes(path, max_bytes)?;
    let mut hasher = Sha256::new();
    hasher.update(&bytes);
    Some(format!("sha256:{:x}", hasher.finalize()))
}

fn read_capped_bytes(path: &Path, max_bytes: usize) -> Option<Vec<u8>> {
    let meta = std::fs::metadata(path).ok()?;
    if meta.len() as usize > max_bytes {
        return None;
    }
    std::fs::read(path).ok()
}

fn read_capped(path: &Path, max_bytes: usize) -> Option<String> {
    let bytes = read_capped_bytes(path, max_bytes)?;
    String::from_utf8(bytes).ok()
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn parse_event_kinds_filters_unknown() {
        let kinds = parse_event_kinds(&["created".into(), "bogus".into(), "deleted".into()]);
        assert_eq!(
            kinds,
            vec![FilesystemEventKind::Created, FilesystemEventKind::Deleted]
        );
    }

    #[test]
    fn matches_globs_respects_include_and_exclude() {
        let include = compile_globs(&["**/*.json".into()]);
        let exclude = compile_globs(&["**/*.tmp.json".into()]);
        assert!(matches_globs("/var/inbox/order.json", &include, &exclude));
        assert!(!matches_globs(
            "/var/inbox/order.tmp.json",
            &include,
            &exclude
        ));
        assert!(!matches_globs("/var/inbox/order.txt", &include, &exclude));
    }

    #[test]
    fn matches_globs_empty_include_matches_all_but_excludes() {
        let exclude = compile_globs(&["**/*.swp".into()]);
        assert!(matches_globs("/var/inbox/a.json", &[], &exclude));
        assert!(!matches_globs("/var/inbox/.a.swp", &[], &exclude));
    }

    #[test]
    fn normalize_create_modify_remove() {
        assert_eq!(
            normalize_event_kind(&EventKind::Create(notify::event::CreateKind::File)),
            Some(FilesystemEventKind::Created)
        );
        assert_eq!(
            normalize_event_kind(&EventKind::Remove(notify::event::RemoveKind::File)),
            Some(FilesystemEventKind::Deleted)
        );
        assert_eq!(
            normalize_event_kind(&EventKind::Modify(notify::event::ModifyKind::Data(
                notify::event::DataChange::Content
            ))),
            Some(FilesystemEventKind::Modified)
        );
        assert_eq!(
            normalize_event_kind(&EventKind::Modify(notify::event::ModifyKind::Name(
                notify::event::RenameMode::Both
            ))),
            Some(FilesystemEventKind::Renamed)
        );
    }

    #[test]
    fn build_payload_created_has_metadata_fields() {
        let dir = tempfile::tempdir().unwrap();
        let file = dir.path().join("order-123.json");
        std::fs::write(&file, b"{\"id\":1}").unwrap();
        let cfg = FilesystemConfig {
            read_content: true,
            max_content_bytes: 1024,
            ..FilesystemConfig::default()
        };
        let payload = build_payload(FilesystemEventKind::Created, &file, None, &cfg);
        assert_eq!(payload["event"], "created");
        assert_eq!(payload["file_name"], "order-123.json");
        assert_eq!(payload["extension"], "json");
        assert_eq!(payload["size"], 8);
        assert_eq!(payload["content"], "{\"id\":1}");
        assert!(
            payload["hash"]
                .as_str()
                .is_some_and(|h| h.starts_with("sha256:"))
        );
    }

    #[test]
    fn build_payload_delete_omits_metadata() {
        let cfg = FilesystemConfig::default();
        let payload = build_payload(
            FilesystemEventKind::Deleted,
            Path::new("/var/inbox/order-123.json"),
            None,
            &cfg,
        );
        assert_eq!(payload["event"], "deleted");
        assert_eq!(payload["file_name"], "order-123.json");
        assert!(payload.get("size").is_none());
        assert!(payload.get("content").is_none());
    }

    #[test]
    fn build_payload_rename_carries_old_path() {
        let cfg = FilesystemConfig::default();
        let payload = build_payload(
            FilesystemEventKind::Renamed,
            Path::new("/var/inbox/order-123.ready"),
            Some(Path::new("/var/inbox/order-123.tmp")),
            &cfg,
        );
        assert_eq!(payload["event"], "renamed");
        assert_eq!(payload["old_path"], "/var/inbox/order-123.tmp");
        assert_eq!(payload["extension"], "ready");
    }

    #[test]
    fn read_capped_rejects_oversize() {
        let dir = tempfile::tempdir().unwrap();
        let file = dir.path().join("big.bin");
        std::fs::write(&file, vec![0u8; 100]).unwrap();
        assert!(read_capped(&file, 10).is_none());
        assert!(read_capped(&file, 1000).is_some());
    }
}
