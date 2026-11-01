//! One active format per installation. Session identities live in events;
//! this bounded manifest owns only upgrade phase and writer generation.
use super::*;
use serde::{Deserialize, Serialize};
use std::collections::BTreeMap;
use std::fs::File;
use std::io::{Read, Write};
use std::path::PathBuf;
use zeroclaw_api::session_events::*;

#[derive(Clone, Copy, Debug, PartialEq, Eq, PartialOrd, Ord, Hash, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum NativeBackend {
    Sqlite,
    Jsonl,
}

impl NativeBackend {
    pub fn parse(value: &str) -> Result<Self, AppendError> {
        match value {
            "sqlite" => Ok(Self::Sqlite),
            "jsonl" => Ok(Self::Jsonl),
            _ => Err(AppendError::InvalidReference(
                "unsupported session backend".into(),
            )),
        }
    }
    pub fn name(self) -> &'static str {
        match self {
            Self::Sqlite => "sqlite",
            Self::Jsonl => "jsonl",
        }
    }
}

#[derive(Clone, Debug, PartialEq, Eq, Serialize, Deserialize)]
#[serde(tag = "phase", rename_all = "snake_case")]
pub(super) enum FormatPhase {
    Upgrading {
        target: NativeBackend,
    },
    Switching {
        source: NativeBackend,
        target: NativeBackend,
        image_digest: Digest,
    },
    Ready {
        backend: NativeBackend,
    },
}

#[derive(Serialize, Deserialize)]
struct Manifest {
    version: u32,
    generation: u64,
    #[serde(flatten)]
    phase: FormatPhase,
}

pub struct InstallationGuard {
    directory: PathBuf,
    lock: File,
    manifest: Option<Manifest>,
}

impl InstallationGuard {
    pub(crate) fn directory(&self) -> &Path {
        &self.directory
    }

    pub fn acquire(sessions_dir: &Path) -> Result<Self, AppendError> {
        filesystem::directory(sessions_dir)?;
        // Keep the old lock name so an already-open legacy writer shares the
        // same fence during the upgrade from the previous implementation.
        let lock = filesystem::file(&sessions_dir.join("session-family.lock"), false)
            .map_err(jsonl::storage)?;
        lock.lock().map_err(jsonl::storage)?;
        let manifest = match filesystem::reader(&sessions_dir.join("session-format.json")) {
            Ok(file) => {
                let mut bytes = Vec::new();
                file.take(4097)
                    .read_to_end(&mut bytes)
                    .map_err(jsonl::storage)?;
                if bytes.len() > 4096 {
                    return Err(AppendError::Corrupt(
                        "session format manifest exceeds limit".into(),
                    ));
                }
                let manifest: Manifest = serde_json::from_slice(&bytes)
                    .map_err(|error| AppendError::Corrupt(error.to_string()))?;
                if manifest.version != 1 {
                    return Err(AppendError::UnsupportedSchema("session format".into()));
                }
                Some(manifest)
            }
            Err(error) if error.kind() == std::io::ErrorKind::NotFound => None,
            Err(error) => return Err(jsonl::storage(error)),
        };
        Ok(Self {
            directory: sessions_dir.into(),
            lock,
            manifest,
        })
    }

    pub(super) fn phase(&self) -> Option<&FormatPhase> {
        self.manifest.as_ref().map(|state| &state.phase)
    }
    pub(super) fn generation(&self) -> u64 {
        self.manifest.as_ref().map_or(0, |state| state.generation)
    }

    pub fn active(&self) -> Result<(NativeBackend, u64), AppendError> {
        match &self.manifest {
            Some(Manifest {
                generation,
                phase: FormatPhase::Ready { backend },
                ..
            }) => {
                let data_dir = self.directory.parent().ok_or_else(|| {
                    AppendError::Corrupt("session directory parent missing".into())
                })?;
                let metadata =
                    fs::symlink_metadata(super::upgrade::native_path(data_dir, *backend))
                        .map_err(jsonl::storage)?;
                if match backend {
                    NativeBackend::Sqlite => !metadata.is_file(),
                    NativeBackend::Jsonl => !metadata.is_dir(),
                } {
                    return Err(AppendError::Corrupt(
                        "active session format has an invalid storage path".into(),
                    ));
                }
                Ok((*backend, *generation))
            }
            _ => Err(AppendError::InvalidTransition(
                "session format upgrade is not complete".into(),
            )),
        }
    }

    pub fn require_legacy(&self) -> std::io::Result<()> {
        if self.manifest.is_some()
            || self
                .directory
                .join("session-family-locator.json")
                .try_exists()?
        {
            return Err(std::io::Error::new(
                std::io::ErrorKind::PermissionDenied,
                "legacy session writer fenced by native format upgrade",
            ));
        }
        Ok(())
    }

    pub(super) fn begin(&mut self, phase: FormatPhase) -> Result<(), AppendError> {
        let generation = self.manifest.as_ref().map_or(Ok(1), |state| {
            state
                .generation
                .checked_add(1)
                .ok_or_else(|| AppendError::Corrupt("session writer generation exhausted".into()))
        })?;
        self.manifest = Some(Manifest {
            version: 1,
            generation,
            phase,
        });
        self.publish()
    }

    pub(super) fn finish(&mut self, backend: NativeBackend) -> Result<(), AppendError> {
        let state = self
            .manifest
            .as_mut()
            .ok_or_else(|| AppendError::InvalidTransition("upgrade was not started".into()))?;
        state.phase = FormatPhase::Ready { backend };
        self.publish()
    }

    fn publish(&self) -> Result<(), AppendError> {
        let bytes = zeroclaw_api::session_events::canonical_json(&self.manifest)?;
        if bytes.len() > 4096 {
            return Err(AppendError::InvalidReference(
                "session format manifest exceeds limit".into(),
            ));
        }
        let temporary = self
            .directory
            .join(format!("session-format-{}.tmp", EventId::new()));
        let mut file = filesystem::file(&temporary, true).map_err(jsonl::storage)?;
        file.write_all(&bytes).map_err(jsonl::storage)?;
        file.sync_all().map_err(jsonl::storage)?;
        drop(file);
        fs::rename(&temporary, self.directory.join("session-format.json"))
            .map_err(|error| AppendError::CommitOutcomeUnknown(error.to_string()))?;
        filesystem::sync_directory(&self.directory)
            .map_err(|error| AppendError::CommitOutcomeUnknown(error.to_string()))
    }
}
impl Drop for InstallationGuard {
    fn drop(&mut self) {
        let _ = self.lock.unlock();
    }
}

pub(crate) fn legacy_guard(directory: &Path) -> std::io::Result<InstallationGuard> {
    let guard = InstallationGuard::acquire(directory).map_err(std::io::Error::other)?;
    guard.require_legacy()?;
    Ok(guard)
}

/// Enumerate native sessions from their canonical root event. Legacy session keys
/// are resolved only by the migration readers before they become SessionStarted IDs.
pub fn creation_source(
    session_id: SessionId,
    source: &str,
) -> Result<(SessionEvent, PayloadRecord), AppendError> {
    if source.is_empty() || source.len() > 128 {
        return Err(AppendError::InvalidReference(
            "invalid session creation source".into(),
        ));
    }
    let value = PayloadRecord::json("session.authority/v1", &source)?;
    let event = SessionEvent::typed(
        session_id,
        EventData::SessionMetadataChanged {
            field: "creation_source".into(),
            new_value_ref: StoredPayloadRef::from(&value),
        },
        EventActor::System {
            component: "creation_source".into(),
        },
    )?;
    Ok((event, value))
}

pub fn session_ids(
    data_dir: &Path,
    backend: NativeBackend,
) -> Result<BTreeMap<String, SessionId>, AppendError> {
    let (store, streams): (Arc<dyn SessionEventStore>, Vec<SessionId>) = match backend {
        NativeBackend::Sqlite => {
            let store = SqliteEventStore::open(data_dir)?;
            let streams = {
                let conn = store.conn.lock();
                let mut stmt = conn
                    .prepare("SELECT session_id FROM session_events WHERE sequence=1 AND json_extract(event_json,'$.kind')='session_started'")
                    .map_err(map_sqlite)?;
                let rows = stmt
                    .query_map([], |row| row.get::<_, String>(0))
                    .map_err(map_sqlite)?;
                rows.map(|row| {
                    SessionId::parse(row.map_err(map_sqlite)?).map_err(AppendError::from)
                })
                .collect::<Result<Vec<_>, _>>()?
            };
            (Arc::new(store), streams)
        }
        NativeBackend::Jsonl => {
            let store = JsonlEventStore::open(data_dir)?;
            let streams = store.root_sessions()?;
            (Arc::new(store), streams)
        }
    };
    let mut result = BTreeMap::new();
    for session_id in streams {
        result.insert(session_id.as_str().to_owned(), session_id);
    }
    let _ = store;
    Ok(result)
}

/// Enumerate canonical sessions, including derived sessions. Creation source
/// and adapter keys do not participate in this inventory.
pub(crate) fn streams(
    data_dir: &Path,
    backend: NativeBackend,
) -> Result<Vec<SessionId>, AppendError> {
    match backend {
        NativeBackend::Sqlite => {
            let store = SqliteEventStore::open(data_dir)?;
            let conn = store.conn.lock();
            let mut statement = conn
                .prepare(
                    "SELECT session_id FROM session_events WHERE sequence=1 ORDER BY session_id",
                )
                .map_err(map_sqlite)?;
            statement
                .query_map([], |row| row.get::<_, String>(0))
                .map_err(map_sqlite)?
                .map(|row| SessionId::parse(row.map_err(map_sqlite)?).map_err(AppendError::from))
                .collect()
        }
        NativeBackend::Jsonl => JsonlEventStore::open(data_dir)?.session_streams(),
    }
}
