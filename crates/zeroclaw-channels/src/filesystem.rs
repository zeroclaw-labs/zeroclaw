use std::collections::HashMap;
use std::path::{Path, PathBuf};
use std::sync::{Arc, Mutex};
use std::time::{Duration, Instant};

use async_trait::async_trait;
use notify::{Event, EventKind, RecursiveMode, Watcher};
use sha2::{Digest, Sha256};
use tokio::sync::mpsc;

use zeroclaw_api::channel::{Channel, ChannelMessage, SendMessage};
use zeroclaw_config::schema::FilesystemConfig;
use zeroclaw_runtime::sop::audit::SopAuditLogger;
use zeroclaw_runtime::sop::dispatch::dispatch_untrusted_fan_in;
use zeroclaw_runtime::sop::engine::SopEngine;
use zeroclaw_runtime::sop::types::{FilesystemEventKind, SopTriggerSource};

/// Filesystem change source as a `Channel`.
///
/// Watches configured paths with a `notify` watcher and routes each file
/// create/modify/delete/rename to the SOP engine via `dispatch_untrusted_fan_in`.
/// It is an input-only source: `Channel::send` has no outbound surface, and
/// `listen` never feeds the chat-loop `tx` — file events drive SOP triggers,
/// not agent turns.
pub struct FilesystemChannel {
    config: FilesystemConfig,
    alias: String,
    engine: Arc<Mutex<SopEngine>>,
    audit: Arc<SopAuditLogger>,
}

/// Construction parameters for [`FilesystemChannel`].
pub struct FilesystemChannelConfig {
    pub config: FilesystemConfig,
    pub alias: String,
    pub engine: Arc<Mutex<SopEngine>>,
    pub audit: Arc<SopAuditLogger>,
}

impl FilesystemChannel {
    pub fn new(cfg: FilesystemChannelConfig) -> Self {
        Self {
            config: cfg.config,
            alias: cfg.alias,
            engine: cfg.engine,
            audit: cfg.audit,
        }
    }

    pub fn alias(&self) -> &str {
        &self.alias
    }

    async fn watch_and_dispatch(&self) -> anyhow::Result<()> {
        use zeroclaw_log::Instrument;
        let span = zeroclaw_log::attribution_span!(self);
        self.watch_and_dispatch_inner().instrument(span).await
    }

    fn compile_globs_or_log(
        &self,
        patterns: &[String],
        list: &str,
    ) -> anyhow::Result<Vec<glob::Pattern>> {
        compile_globs(patterns).inspect_err(|e| {
            ::zeroclaw_log::record!(
                ERROR,
                ::zeroclaw_log::Event::new(module_path!(), ::zeroclaw_log::Action::Note)
                    .with_attrs(::serde_json::json!({ "list": list, "error": e.to_string() })),
                "Filesystem channel: invalid glob pattern, refusing to start listener"
            );
        })
    }

    async fn watch_and_dispatch_inner(&self) -> anyhow::Result<()> {
        let config = &self.config;
        config.validate()?;
        let include = self.compile_globs_or_log(&config.include, "include")?;
        let exclude = self.compile_globs_or_log(&config.exclude, "exclude")?;

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
                "Filesystem channel: watching ''"
            );
        }

        zeroclaw_runtime::health::mark_component_ok("filesystem");

        let enabled_events = parse_event_kinds(&config.events);
        let debounce = Duration::from_millis(config.debounce_ms);
        let settle = Duration::from_millis(config.settle_ms);
        let mut last_seen: HashMap<(String, FilesystemEventKind), Instant> = HashMap::new();
        let mut pending_from: Option<PathBuf> = None;

        loop {
            let event = match raw_rx.recv() {
                Ok(e) => e,
                Err(_) => return Ok(()),
            };

            let (kind, path, old_path) = match classify(&event, &mut pending_from) {
                Classified::Event {
                    kind,
                    path,
                    old_path,
                } => (kind, path, old_path),
                Classified::RenameFrom | Classified::Ignored => continue,
            };
            if !enabled_events.is_empty() && !enabled_events.contains(&kind) {
                continue;
            }

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
            last_seen.retain(|_, seen| now.duration_since(*seen) < debounce);
            last_seen.insert(dedup_key, now);

            if !settle.is_zero() {
                tokio::time::sleep(settle).await;
            }

            let payload = build_payload(kind, &path, old_path.as_deref(), config);

            ::zeroclaw_log::record!(
                INFO,
                ::zeroclaw_log::Event::new(module_path!(), ::zeroclaw_log::Action::Note)
                    .with_attrs(
                        ::serde_json::json!({ "path": path_str, "event": kind.to_string() })
                    ),
                "Filesystem channel: dispatching '' ''"
            );

            dispatch_untrusted_fan_in(
                &self.engine,
                &self.audit,
                SopTriggerSource::Filesystem,
                Some(&path_str),
                Some(&payload.to_string()),
                None,
            )
            .await;
        }
    }
}

#[async_trait]
impl Channel for FilesystemChannel {
    fn name(&self) -> &str {
        "filesystem"
    }

    async fn send(&self, _message: &SendMessage) -> anyhow::Result<()> {
        // Filesystem is an input-only SOP trigger source; replies flow through
        // whatever outbound channel the agent's procedure selects, not back to
        // the watched directory.
        Ok(())
    }

    fn supports_outbound_send(&self) -> bool {
        false
    }

    async fn listen(&self, _tx: mpsc::Sender<ChannelMessage>) -> anyhow::Result<()> {
        self.watch_and_dispatch().await
    }

    fn self_handle(&self) -> Option<String> {
        None
    }

    async fn start_typing(&self, _recipient: &str) -> anyhow::Result<()> {
        Ok(())
    }

    async fn stop_typing(&self, _recipient: &str) -> anyhow::Result<()> {
        Ok(())
    }
}

impl ::zeroclaw_api::attribution::Attributable for FilesystemChannel {
    fn role(&self) -> ::zeroclaw_api::attribution::Role {
        ::zeroclaw_api::attribution::Role::Channel(
            ::zeroclaw_api::attribution::ChannelKind::Filesystem,
        )
    }
    fn alias(&self) -> &str {
        &self.alias
    }
}

fn parse_event_kinds(events: &[String]) -> Vec<FilesystemEventKind> {
    events
        .iter()
        .filter_map(|e| e.parse::<FilesystemEventKind>().ok())
        .collect()
}

enum Classified {
    Event {
        kind: FilesystemEventKind,
        path: PathBuf,
        old_path: Option<PathBuf>,
    },
    /// A split-rename `From` half: the path is buffered in `pending_from`;
    /// the loop ignores this until the matching `To` arrives.
    RenameFrom,
    Ignored,
}

fn classify(event: &Event, pending_from: &mut Option<PathBuf>) -> Classified {
    use notify::event::ModifyKind;

    match &event.kind {
        EventKind::Create(_) => match event.paths.first() {
            Some(p) => Classified::Event {
                kind: FilesystemEventKind::Created,
                path: p.clone(),
                old_path: None,
            },
            None => Classified::Ignored,
        },
        EventKind::Remove(_) => match event.paths.first() {
            Some(p) => Classified::Event {
                kind: FilesystemEventKind::Deleted,
                path: p.clone(),
                old_path: None,
            },
            None => Classified::Ignored,
        },
        EventKind::Modify(ModifyKind::Name(mode)) => classify_rename(*mode, event, pending_from),
        EventKind::Modify(_) => match event.paths.first() {
            Some(p) => Classified::Event {
                kind: FilesystemEventKind::Modified,
                path: p.clone(),
                old_path: None,
            },
            None => Classified::Ignored,
        },
        _ => Classified::Ignored,
    }
}

fn classify_rename(
    mode: notify::event::RenameMode,
    event: &Event,
    pending_from: &mut Option<PathBuf>,
) -> Classified {
    use notify::event::RenameMode;

    match mode {
        RenameMode::Both if event.paths.len() >= 2 => Classified::Event {
            kind: FilesystemEventKind::Renamed,
            path: event.paths[1].clone(),
            old_path: Some(event.paths[0].clone()),
        },
        RenameMode::From => match event.paths.first() {
            Some(p) => {
                *pending_from = Some(p.clone());
                Classified::RenameFrom
            }
            None => Classified::Ignored,
        },
        RenameMode::To => match event.paths.first() {
            Some(p) => Classified::Event {
                kind: FilesystemEventKind::Renamed,
                path: p.clone(),
                old_path: pending_from.take(),
            },
            None => Classified::Ignored,
        },
        RenameMode::Any | RenameMode::Both | RenameMode::Other => match event.paths.first() {
            Some(p) => Classified::Event {
                kind: FilesystemEventKind::Renamed,
                path: p.clone(),
                old_path: pending_from.take(),
            },
            None => Classified::Ignored,
        },
    }
}

fn compile_globs(patterns: &[String]) -> anyhow::Result<Vec<glob::Pattern>> {
    patterns
        .iter()
        .map(|p| {
            glob::Pattern::new(p)
                .map_err(|e| anyhow::Error::msg(format!("invalid glob pattern '{p}': {e}")))
        })
        .collect()
}

fn matches_globs(path: &str, include: &[glob::Pattern], exclude: &[glob::Pattern]) -> bool {
    let normalized = normalize_separators(path);
    let path = normalized.as_str();
    if !include.is_empty() && !include.iter().any(|p| p.matches(path)) {
        return false;
    }
    if exclude.iter().any(|p| p.matches(path)) {
        return false;
    }
    true
}

/// Glob patterns are written with `/`, but Windows paths arrive with `\`.
/// Normalize to `/` so include/exclude matching is platform-agnostic.
fn normalize_separators(path: &str) -> String {
    if std::path::MAIN_SEPARATOR == '/' {
        path.to_string()
    } else {
        path.replace(std::path::MAIN_SEPARATOR, "/")
    }
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

fn symlink_path_admitted(path: &Path, config: &FilesystemConfig) -> bool {
    let is_symlink = std::fs::symlink_metadata(path)
        .map(|m| m.file_type().is_symlink())
        .unwrap_or(false);
    if !is_symlink {
        return true;
    }
    if !config.follow_symlinks {
        ::zeroclaw_log::record!(
            WARN,
            ::zeroclaw_log::Event::new(module_path!(), ::zeroclaw_log::Action::Note)
                .with_attrs(::serde_json::json!({ "path": path.to_string_lossy() })),
            "Filesystem channel: rejecting symlink event path (follow_symlinks is off)"
        );
        return false;
    }
    let Ok(target) = std::fs::canonicalize(path) else {
        return false;
    };
    if canonical_target_within_roots(&target, &config.paths) {
        return true;
    }
    ::zeroclaw_log::record!(
        WARN,
        ::zeroclaw_log::Event::new(module_path!(), ::zeroclaw_log::Action::Note).with_attrs(
            ::serde_json::json!({
                "path": path.to_string_lossy(),
                "target": target.to_string_lossy(),
            })
        ),
        "Filesystem channel: rejecting symlink whose target escapes the watched roots"
    );
    false
}

fn canonical_target_within_roots(target: &Path, roots: &[String]) -> bool {
    roots.iter().any(|root| {
        std::fs::canonicalize(root)
            .map(|canonical_root| target.starts_with(&canonical_root))
            .unwrap_or(false)
    })
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
            if !symlink_path_admitted(path, config) {
                return payload;
            }
            if let Ok(meta) = std::fs::metadata(path) {
                obj.insert("size".into(), ::serde_json::json!(meta.len()));
                if let Ok(modified) = meta.modified() {
                    let dt: chrono::DateTime<chrono::Utc> = modified.into();
                    obj.insert(
                        "modified_at".into(),
                        ::serde_json::json!(dt.format("%Y-%m-%dT%H:%M:%SZ").to_string()),
                    );
                }
                let content_cap = config.max_content_bytes.unwrap_or(usize::MAX);
                if let Some(hash) = hash_file(path, content_cap) {
                    obj.insert("hash".into(), ::serde_json::json!(hash));
                }
                if config.read_content
                    && let Some(content) = read_capped(path, content_cap)
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
    use std::io::Read;
    let meta = std::fs::metadata(path).ok()?;
    if meta.len() as usize > max_bytes {
        return None;
    }
    let mut file = std::fs::File::open(path).ok()?;
    let mut hasher = Sha256::new();
    let mut buf = [0u8; 65536];
    loop {
        let n = file.read(&mut buf).ok()?;
        if n == 0 {
            break;
        }
        hasher.update(&buf[..n]);
    }
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
        let include = compile_globs(&["**/*.json".into()]).unwrap();
        let exclude = compile_globs(&["**/*.tmp.json".into()]).unwrap();
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
        let exclude = compile_globs(&["**/*.swp".into()]).unwrap();
        assert!(matches_globs("/var/inbox/a.json", &[], &exclude));
        assert!(!matches_globs("/var/inbox/.a.swp", &[], &exclude));
    }

    #[test]
    fn compile_globs_rejects_invalid_pattern() {
        let err = compile_globs(&["**/[broken".into()]).expect_err("invalid glob must fail closed");
        assert!(
            err.to_string().contains("**/[broken"),
            "error must name the offending pattern; got: {err}"
        );
    }

    #[test]
    fn classify_create_modify_remove() {
        let mut pending = None;
        let ev = mk_event(
            EventKind::Create(notify::event::CreateKind::File),
            vec!["/w/a.txt"],
        );
        assert!(matches!(
            classify(&ev, &mut pending),
            Classified::Event {
                kind: FilesystemEventKind::Created,
                ..
            }
        ));

        let ev = mk_event(
            EventKind::Remove(notify::event::RemoveKind::File),
            vec!["/w/a.txt"],
        );
        assert!(matches!(
            classify(&ev, &mut pending),
            Classified::Event {
                kind: FilesystemEventKind::Deleted,
                ..
            }
        ));

        let ev = mk_event(
            EventKind::Modify(notify::event::ModifyKind::Data(
                notify::event::DataChange::Content,
            )),
            vec!["/w/a.txt"],
        );
        assert!(matches!(
            classify(&ev, &mut pending),
            Classified::Event {
                kind: FilesystemEventKind::Modified,
                ..
            }
        ));
    }

    #[test]
    fn classify_rename_both_is_linux_inotify_shape() {
        // inotify: one event, RenameMode::Both, paths = [from, to].
        let mut pending = None;
        let ev = mk_event(
            EventKind::Modify(notify::event::ModifyKind::Name(
                notify::event::RenameMode::Both,
            )),
            vec!["/w/old.txt", "/w/new.txt"],
        );
        match classify(&ev, &mut pending) {
            Classified::Event {
                kind,
                path,
                old_path,
            } => {
                assert_eq!(kind, FilesystemEventKind::Renamed);
                assert_eq!(path, PathBuf::from("/w/new.txt"));
                assert_eq!(old_path, Some(PathBuf::from("/w/old.txt")));
            }
            _ => panic!("expected paired rename"),
        }
        assert!(pending.is_none());
    }

    #[test]
    fn classify_rename_split_pairs_from_then_to() {
        // Windows ReadDirectoryChangesW / macOS FSEvents: From then To, one path each.
        let mut pending = None;
        let from = mk_event(
            EventKind::Modify(notify::event::ModifyKind::Name(
                notify::event::RenameMode::From,
            )),
            vec!["/w/old.txt"],
        );
        match classify(&from, &mut pending) {
            Classified::RenameFrom => {}
            _ => panic!("expected RenameFrom to be buffered"),
        }
        assert_eq!(pending, Some(PathBuf::from("/w/old.txt")));

        let to = mk_event(
            EventKind::Modify(notify::event::ModifyKind::Name(
                notify::event::RenameMode::To,
            )),
            vec!["/w/new.txt"],
        );
        match classify(&to, &mut pending) {
            Classified::Event {
                kind,
                path,
                old_path,
            } => {
                assert_eq!(kind, FilesystemEventKind::Renamed);
                assert_eq!(path, PathBuf::from("/w/new.txt"));
                assert_eq!(old_path, Some(PathBuf::from("/w/old.txt")));
            }
            _ => panic!("expected paired rename on To"),
        }
        assert!(pending.is_none(), "pending_from consumed by To");
    }

    #[test]
    fn classify_rename_to_without_from_has_no_old_path() {
        // A To with no preceding From (dropped/missed) still surfaces as a rename.
        let mut pending = None;
        let to = mk_event(
            EventKind::Modify(notify::event::ModifyKind::Name(
                notify::event::RenameMode::To,
            )),
            vec!["/w/new.txt"],
        );
        match classify(&to, &mut pending) {
            Classified::Event { kind, old_path, .. } => {
                assert_eq!(kind, FilesystemEventKind::Renamed);
                assert_eq!(old_path, None);
            }
            _ => panic!("expected rename"),
        }
    }

    #[test]
    fn classify_rename_any_is_macos_fsevents_fallback() {
        // FSEvents often reports RenameMode::Any single-path.
        let mut pending = None;
        let ev = mk_event(
            EventKind::Modify(notify::event::ModifyKind::Name(
                notify::event::RenameMode::Any,
            )),
            vec!["/w/x.txt"],
        );
        assert!(matches!(
            classify(&ev, &mut pending),
            Classified::Event {
                kind: FilesystemEventKind::Renamed,
                ..
            }
        ));
    }

    #[test]
    fn normalize_separators_rewrites_only_on_backslash_platforms() {
        if std::path::MAIN_SEPARATOR == '\\' {
            assert_eq!(normalize_separators("C:\\w\\a.txt"), "C:/w/a.txt");
        } else {
            assert_eq!(normalize_separators("/w/a.txt"), "/w/a.txt");
        }
    }

    fn mk_event(kind: EventKind, paths: Vec<&str>) -> Event {
        Event {
            kind,
            paths: paths.into_iter().map(PathBuf::from).collect(),
            attrs: Default::default(),
        }
    }

    #[test]
    fn build_payload_created_has_metadata_fields() {
        let dir = tempfile::tempdir().unwrap();
        let file = dir.path().join("order-123.json");
        std::fs::write(&file, b"{\"id\":1}").unwrap();
        let cfg = FilesystemConfig {
            read_content: true,
            max_content_bytes: Some(1024),
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

    #[test]
    fn build_payload_default_cap_skips_oversize_content_and_hash() {
        let dir = tempfile::tempdir().unwrap();
        let file = dir.path().join("big.json");
        std::fs::write(&file, vec![b'x'; 65537]).unwrap();
        let cfg = FilesystemConfig {
            read_content: true,
            ..FilesystemConfig::default()
        };
        let payload = build_payload(FilesystemEventKind::Created, &file, None, &cfg);
        assert_eq!(payload["size"], 65537);
        assert!(
            payload.get("content").is_none(),
            "default 64 KiB cap must skip oversize content"
        );
        assert!(
            payload.get("hash").is_none(),
            "default 64 KiB cap must skip hashing oversize files"
        );
    }

    #[test]
    fn build_payload_none_cap_reads_unbounded() {
        let dir = tempfile::tempdir().unwrap();
        let file = dir.path().join("big.json");
        std::fs::write(&file, vec![b'y'; 65537]).unwrap();
        let cfg = FilesystemConfig {
            read_content: true,
            max_content_bytes: None,
            ..FilesystemConfig::default()
        };
        let payload = build_payload(FilesystemEventKind::Created, &file, None, &cfg);
        assert_eq!(
            payload["content"].as_str().map(|c| c.len()),
            Some(65537),
            "None cap reads the whole file"
        );
        assert!(
            payload["hash"]
                .as_str()
                .is_some_and(|h| h.starts_with("sha256:"))
        );
    }

    #[cfg(unix)]
    #[test]
    fn build_payload_rejects_symlink_escaping_watched_root_by_default() {
        let outside = tempfile::tempdir().unwrap();
        let secret = outside.path().join("secret.txt");
        std::fs::write(&secret, b"top-secret").unwrap();

        let watched = tempfile::tempdir().unwrap();
        let link = watched.path().join("order-123.json");
        std::os::unix::fs::symlink(&secret, &link).unwrap();

        let cfg = FilesystemConfig {
            read_content: true,
            max_content_bytes: Some(1024),
            follow_symlinks: false,
            paths: vec![watched.path().to_string_lossy().to_string()],
            ..FilesystemConfig::default()
        };
        let payload = build_payload(FilesystemEventKind::Created, &link, None, &cfg);
        assert_eq!(payload["event"], "created");
        assert!(payload.get("size").is_none(), "symlink target size leaked");
        assert!(payload.get("hash").is_none(), "symlink target hashed");
        assert!(
            payload.get("content").is_none(),
            "symlink target content leaked"
        );
    }

    #[cfg(unix)]
    #[test]
    fn build_payload_follow_symlinks_rejects_target_outside_roots() {
        let outside = tempfile::tempdir().unwrap();
        let secret = outside.path().join("secret.txt");
        std::fs::write(&secret, b"top-secret").unwrap();

        let watched = tempfile::tempdir().unwrap();
        let link = watched.path().join("order-123.json");
        std::os::unix::fs::symlink(&secret, &link).unwrap();

        let cfg = FilesystemConfig {
            read_content: true,
            max_content_bytes: Some(1024),
            follow_symlinks: true,
            paths: vec![watched.path().to_string_lossy().to_string()],
            ..FilesystemConfig::default()
        };
        let payload = build_payload(FilesystemEventKind::Created, &link, None, &cfg);
        assert!(
            payload.get("content").is_none(),
            "escaping symlink target read under follow_symlinks"
        );
    }

    #[cfg(unix)]
    #[test]
    fn build_payload_follow_symlinks_admits_target_inside_roots() {
        let watched = tempfile::tempdir().unwrap();
        let real = watched.path().join("real-order.json");
        std::fs::write(&real, b"{\"id\":1}").unwrap();
        let link = watched.path().join("order-123.json");
        std::os::unix::fs::symlink(&real, &link).unwrap();

        let cfg = FilesystemConfig {
            read_content: true,
            max_content_bytes: Some(1024),
            follow_symlinks: true,
            paths: vec![watched.path().to_string_lossy().to_string()],
            ..FilesystemConfig::default()
        };
        let payload = build_payload(FilesystemEventKind::Created, &link, None, &cfg);
        assert_eq!(payload["content"], "{\"id\":1}");
    }

    #[allow(clippy::await_holding_lock)]
    #[tokio::test]
    async fn invalid_glob_rejection_is_span_attributed() {
        use std::sync::Arc;
        use zeroclaw_runtime::sop::engine::SopEngine;

        let _writer_guard = zeroclaw_log::__private_test_writer_lock();
        let _hook_guard = zeroclaw_log::__private_test_hook_lock();
        zeroclaw_log::try_install_capture_subscriber();
        let mut rx = zeroclaw_log::subscribe_or_install();
        while rx.try_recv().is_ok() {}

        let watched = tempfile::tempdir().unwrap();
        let config = FilesystemConfig {
            paths: vec![watched.path().to_string_lossy().to_string()],
            include: vec!["**/[broken".into()],
            ..FilesystemConfig::default()
        };
        let memory = Arc::new(zeroclaw_memory::NoneMemory::new("fs-proof"));
        let channel = FilesystemChannel::new(FilesystemChannelConfig {
            config,
            alias: "fs-proof".into(),
            engine: Arc::new(Mutex::new(SopEngine::new(Default::default()))),
            audit: Arc::new(SopAuditLogger::new(memory)),
        });

        let err = channel
            .watch_and_dispatch()
            .await
            .expect_err("invalid include glob must fail the listener closed");
        assert!(
            err.to_string().contains("**/[broken"),
            "error must name the bad pattern; got: {err}"
        );

        let deadline = std::time::Instant::now() + std::time::Duration::from_secs(2);
        let mut found = None;
        while found.is_none() && std::time::Instant::now() < deadline {
            let remaining = deadline.saturating_duration_since(std::time::Instant::now());
            let step = remaining.min(std::time::Duration::from_millis(50));
            match tokio::time::timeout(step, rx.recv()).await {
                Ok(Ok(value)) => {
                    if value
                        .get("message")
                        .and_then(|v| v.as_str())
                        .map(|s| s.contains("invalid glob pattern, refusing to start listener"))
                        .unwrap_or(false)
                    {
                        found = Some(value);
                    }
                }
                Ok(Err(tokio::sync::broadcast::error::RecvError::Lagged(_))) => {}
                Ok(Err(tokio::sync::broadcast::error::RecvError::Closed)) => break,
                Err(_elapsed) => {}
            }
        }

        let value = found.expect("expected the invalid-glob rejection event on the broadcast hook");
        assert_eq!(value["severity_text"], "ERROR");
        let zc = value
            .get("zeroclaw")
            .expect("zeroclaw attribution block present");
        assert_eq!(
            zc.get("channel").and_then(|v| v.as_str()),
            Some("filesystem.fs-proof"),
            "rejection must be span-attributed to the filesystem channel; got: {zc:?}"
        );
        assert_eq!(
            zc.get("channel_type").and_then(|v| v.as_str()),
            Some("filesystem")
        );
        assert_eq!(
            zc.get("channel_alias").and_then(|v| v.as_str()),
            Some("fs-proof")
        );
        assert_eq!(value["attributes"]["list"], "include");
    }
}
