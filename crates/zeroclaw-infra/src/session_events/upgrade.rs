//! Installation-wide upgrade. All legacy writers share the publication fence;
//! source files remain intact until every imported family has a final receipt.
use self::readers::{LegacySqliteReader, read_jsonl, read_trim_breadcrumb};
use super::import::{LegacyImport, LegacySnapshot, TimestampedMessage, import_snapshot};
use super::installation::{FormatPhase, InstallationGuard, NativeBackend};
use super::*;
use std::collections::BTreeSet;
use zeroclaw_api::session_events::{BranchId, SessionId};
mod acp;
#[cfg(any(test, feature = "test-util"))]
#[doc(hidden)]
pub mod fixtures;
pub(crate) mod legacy_acp;
pub(super) mod legacy_media;
pub(crate) mod readers;

pub fn upgrade_installation(
    data_dir: &Path,
    requested: NativeBackend,
    resolve_alias: impl Fn(Option<&str>) -> String,
) -> Result<(), AppendError> {
    if requested == NativeBackend::Jsonl && !cfg!(unix) {
        return Err(AppendError::StorageUnavailable(
            "JSONL directory durability is unsupported on this platform; use SQLite".into(),
        ));
    }
    let directory = data_dir.join("sessions");
    let mut guard = InstallationGuard::acquire(&directory)?;
    let target = match guard.phase() {
        Some(FormatPhase::Ready { .. } | FormatPhase::Switching { .. }) => {
            return migration::migrate_locked(data_dir, &mut guard, requested);
        }
        Some(FormatPhase::Upgrading { target }) => *target,
        None => {
            guard.begin(FormatPhase::Upgrading { target: requested })?;
            requested
        }
    };
    let store = open_event_store(data_dir, target.name())?;
    let mut existing = installation::session_ids(data_dir, target)?;
    let mut sqlite_keys = BTreeSet::new();
    if directory
        .join("sessions.db")
        .try_exists()
        .map_err(jsonl::storage)?
    {
        let _source_file =
            filesystem::reader(&directory.join("sessions.db")).map_err(jsonl::storage)?;
        let source = LegacySqliteReader::open(&directory).map_err(storage)?;
        for family in source.upgrade_keys().map_err(storage)? {
            sqlite_keys.insert(family.clone());
            if existing.contains_key(&family) && !import_pending(store.as_ref(), &family)? {
                continue;
            }
            let metadata = source.upgrade_metadata(&family).map_err(storage)?;
            let alias = metadata
                .as_ref()
                .and_then(|meta| meta.agent_alias.as_deref())
                .map(str::to_owned)
                .unwrap_or_else(|| {
                    resolve_alias(
                        metadata
                            .as_ref()
                            .and_then(|meta| meta.channel_id.as_deref()),
                    )
                });
            let mut fields = Vec::new();
            if let Some(metadata) = &metadata {
                fields.extend([
                    (
                        "principal_id".into(),
                        serde_json::json!(metadata.principal_id),
                    ),
                    ("title".into(), serde_json::json!(metadata.title)),
                    ("channel_id".into(), serde_json::json!(metadata.channel_id)),
                    ("room_id".into(), serde_json::json!(metadata.room_id)),
                    ("sender_id".into(), serde_json::json!(metadata.sender_id)),
                    (
                        "created_at".into(),
                        serde_json::json!(metadata.created_at.timestamp_millis()),
                    ),
                    (
                        "last_activity".into(),
                        serde_json::json!(metadata.last_activity.timestamp_millis()),
                    ),
                ]);
            }
            import_one(
                store.as_ref(),
                &family,
                &alias,
                target,
                "legacy-sqlite",
                LegacySnapshot {
                    messages: source.upgrade_messages(&family).map_err(storage)?,
                    history_has_trim_breadcrumb: source
                        .get_session_trim_breadcrumb(&family)
                        .map_err(storage)?,
                    metadata: fields,
                },
            )?;
            existing.insert(family.clone(), session_id(&family)?);
        }
    }
    // Inspect disk directly: listing a corrupt source must never become an
    // empty migration followed by archiving unread history.
    let mut backups = Vec::new();
    for entry in fs::read_dir(&directory).map_err(jsonl::storage)? {
        let entry = entry.map_err(jsonl::storage)?;
        let path = entry.path();
        if path
            .extension()
            .is_some_and(|extension| extension == "trim_breadcrumb")
            && !path
                .with_extension("jsonl")
                .try_exists()
                .map_err(jsonl::storage)?
        {
            let family = path
                .file_stem()
                .and_then(|name| name.to_str())
                .ok_or_else(|| AppendError::Corrupt("non-UTF-8 trim source key".into()))?;
            if !existing.contains_key(family) {
                return Err(AppendError::Corrupt(
                    "trim source has no completed history import".into(),
                ));
            }
            let _source_file = filesystem::reader(&path).map_err(jsonl::storage)?;
            backups.push(path);
            continue;
        }
        if path
            .extension()
            .is_none_or(|extension| extension != "jsonl")
        {
            continue;
        }
        let source_file = filesystem::reader(&path).map_err(jsonl::storage)?;
        let modified_at = source_file
            .metadata()
            .map_err(jsonl::storage)?
            .modified()
            .map_err(jsonl::storage)?
            .duration_since(std::time::UNIX_EPOCH)
            .map_err(storage)?
            .as_millis();
        let modified_at = i64::try_from(modified_at).map_err(storage)?;
        match filesystem::reader(&path.with_extension("trim_breadcrumb")) {
            Ok(_) => {}
            Err(error) if error.kind() == std::io::ErrorKind::NotFound => {}
            Err(error) => return Err(jsonl::storage(error)),
        }
        let family = path
            .file_stem()
            .and_then(|name| name.to_str())
            .ok_or_else(|| AppendError::Corrupt("non-UTF-8 legacy session key".into()))?;
        if zeroclaw_api::session_keys::sanitize_session_key(family) != family {
            return Err(AppendError::Corrupt(
                "legacy filename does not match its canonical session key".into(),
            ));
        }
        let messages = read_jsonl(source_file).map_err(storage)?;
        if sqlite_keys.contains(family) {
            let source = LegacySqliteReader::open(&directory).map_err(storage)?;
            if zeroclaw_api::session_events::canonical_json(
                &source
                    .upgrade_messages(family)
                    .map_err(storage)?
                    .into_iter()
                    .map(|record| record.message)
                    .collect::<Vec<_>>(),
            )? != zeroclaw_api::session_events::canonical_json(&messages)?
            {
                return Err(AppendError::Corrupt(
                    "divergent SQLite and JSONL legacy history".into(),
                ));
            }
        } else if !existing.contains_key(family) || import_pending(store.as_ref(), family)? {
            import_one(
                store.as_ref(),
                family,
                &resolve_alias(None),
                target,
                "legacy-jsonl",
                LegacySnapshot {
                    messages: messages
                        .into_iter()
                        .map(|message| TimestampedMessage {
                            message,
                            created_at: None,
                        })
                        .collect(),
                    history_has_trim_breadcrumb: read_trim_breadcrumb(
                        &path.with_extension("trim_breadcrumb"),
                    )
                    .map_err(storage)?,
                    metadata: vec![("last_activity".into(), serde_json::json!(modified_at))],
                },
            )?;
        }
        backups.push(path.clone());
        backups.push(path.with_extension("trim_breadcrumb"));
    }
    acp::import_all(&guard, store.as_ref())?;
    crate::cost_ledger::CostLedger::open_for_backend(data_dir, target)
        .map_err(|error| AppendError::StorageUnavailable(error.to_string()))?;
    #[cfg(test)]
    super::crash_tests::crash_boundary("installation_imported");
    // Renames are idempotent and synced individually. A restart can finish a
    // partially archived source set from the final receipts in the target.
    for path in backups {
        archive_path(&path, "backup")?;
        #[cfg(test)]
        super::crash_tests::crash_boundary("installation_source_archived");
    }
    archive_sqlite(&directory.join("sessions.db"), "backup")?;
    archive_sqlite(&directory.join("acp-sessions.db"), "backup")?;
    #[cfg(test)]
    super::crash_tests::crash_boundary("installation_archived");
    guard.finish(target)?;
    if target != requested {
        migration::migrate_locked(data_dir, &mut guard, requested)?;
    }
    Ok(())
}

fn import_one(
    store: &dyn SessionEventStore,
    family: &str,
    alias: &str,
    target: NativeBackend,
    source_kind: &str,
    snapshot: LegacySnapshot,
) -> Result<(), AppendError> {
    let stream = session_id(family)?;
    import_snapshot(
        store,
        LegacyImport {
            family,
            creation_source: "legacy",
            owning_agent: alias,
            target,
            branch: BranchId::parse(format!("branch-{stream}"))?,
            session: stream.clone(),
            stream,
            source_kind,
        },
        snapshot,
    )?;
    Ok(())
}

fn import_pending(store: &dyn SessionEventStore, family: &str) -> Result<bool, AppendError> {
    let id = session_id(family)?;
    Ok(store
        .lookup_receipt(&id, &format!("legacy-import-root:{id}"))
        .map_err(storage)?
        .is_some()
        && store
            .lookup_receipt(&id, &format!("legacy-family-import:{id}"))
            .map_err(storage)?
            .is_none())
}

pub fn session_id(family: &str) -> Result<SessionId, ContractError> {
    // Old storage accepted keys outside the native ID grammar. Normal paths
    // parse their session ID directly; only import normalizes a legacy key.
    SessionId::parse(family.to_owned()).or_else(|_| {
        SessionId::parse(format!(
            "legacy-{}",
            Digest::from_bytes(family.as_bytes()).as_str()
        ))
    })
}

pub(super) fn native_path(data_dir: &Path, backend: NativeBackend) -> std::path::PathBuf {
    data_dir.join(match backend {
        NativeBackend::Sqlite => "sessions/session-events.db",
        NativeBackend::Jsonl => "sessions/session-events-jsonl",
    })
}

/// Archive the previous native backend after a verified backend transfer.
/// This is current-format backend switching, not legacy session import.
pub(super) fn archive_native(
    data_dir: &Path,
    backend: NativeBackend,
    generation: u64,
) -> Result<(), AppendError> {
    let suffix = format!("backup-{generation}");
    match backend {
        NativeBackend::Sqlite => {
            archive_sqlite(&data_dir.join("sessions/session-events.db"), &suffix)
        }
        NativeBackend::Jsonl => {
            archive_path(&data_dir.join("sessions/session-events-jsonl"), &suffix)
        }
    }
}

pub fn archive_path(path: &Path, suffix: &str) -> Result<(), AppendError> {
    if suffix.is_empty()
        || !suffix
            .bytes()
            .all(|byte| byte.is_ascii_alphanumeric() || byte == b'-')
    {
        return Err(AppendError::InvalidReference(
            "invalid backup suffix".into(),
        ));
    }
    match fs::symlink_metadata(path) {
        Ok(meta) if !meta.file_type().is_symlink() => {}
        Ok(_) => {
            return Err(AppendError::StorageUnavailable(
                "upgrade source is a symlink".into(),
            ));
        }
        Err(error) if error.kind() == std::io::ErrorKind::NotFound => return Ok(()),
        Err(error) => return Err(jsonl::storage(error)),
    }
    let mut name = path.as_os_str().to_os_string();
    name.push(format!(".{suffix}"));
    let backup = std::path::PathBuf::from(name);
    match fs::symlink_metadata(&backup) {
        Ok(_) => {
            return Err(AppendError::StorageUnavailable(
                "upgrade backup already exists".into(),
            ));
        }
        Err(error) if error.kind() == std::io::ErrorKind::NotFound => {}
        Err(error) => return Err(jsonl::storage(error)),
    }
    fs::rename(path, backup)
        .map_err(|error| AppendError::CommitOutcomeUnknown(error.to_string()))?;
    filesystem::sync_directory(
        path.parent()
            .ok_or_else(|| AppendError::InvalidReference("upgrade source parent missing".into()))?,
    )
}

fn archive_sqlite(path: &Path, suffix: &str) -> Result<(), AppendError> {
    if path.try_exists().map_err(jsonl::storage)? {
        // Windows requires a writable handle for FlushFileBuffers/sync_all.
        // The file is not modified here; the write access only supplies the
        // durability handle used after the WAL checkpoint.
        let file = filesystem::file(path, false).map_err(jsonl::storage)?;
        let conn = Connection::open(path).map_err(map_sqlite)?;
        let (busy, _, _): (i64, i64, i64) = conn
            .query_row("PRAGMA wal_checkpoint(TRUNCATE)", [], |row| {
                Ok((row.get(0)?, row.get(1)?, row.get(2)?))
            })
            .map_err(map_sqlite)?;
        if busy != 0 {
            return Err(AppendError::StorageUnavailable(
                "legacy SQLite readers must drain before upgrade backup".into(),
            ));
        }
        drop(conn);
        file.sync_all().map_err(jsonl::storage)?;
        drop(file);
        archive_path(path, suffix)?;
    }
    for sidecar in ["-wal", "-shm"] {
        let mut name = path.as_os_str().to_os_string();
        name.push(sidecar);
        archive_path(Path::new(&name), suffix)?;
    }
    Ok(())
}
pub(super) fn storage(error: impl std::fmt::Display) -> AppendError {
    AppendError::StorageUnavailable(error.to_string())
}

#[cfg(test)]
mod tests;
