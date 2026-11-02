//! Physical lifetime of temporary sessions. Callers hold the installation
//! publication lock; ordinary sessions keep their append-only history.
use super::*;
use std::collections::{BTreeMap, BTreeSet};
use zeroclaw_api::session_events::{EventData, StoredPayloadRef, payloads};

// A file lease is the process-lifetime authority. Session metadata records
// its identity; the map holds only the corresponding OS handles, no sessions.
static PROCESS: std::sync::LazyLock<EventId> = std::sync::LazyLock::new(EventId::new);
static LEASES: std::sync::LazyLock<Mutex<BTreeMap<std::path::PathBuf, std::fs::File>>> =
    std::sync::LazyLock::new(|| Mutex::new(BTreeMap::new()));

pub(super) fn candidates(
    data_dir: &Path,
    backend: installation::NativeBackend,
) -> Result<Vec<SessionId>, AppendError> {
    match backend {
        installation::NativeBackend::Sqlite => {
            let store = SqliteEventStore::open(data_dir)?;
            let raw_events = {
                let connection = store.conn.lock();
                let mut statement = connection
                    .prepare("SELECT event_json FROM session_events ORDER BY session_id, sequence")
                    .map_err(map_sqlite)?;
                statement
                    .query_map([], |row| row.get::<_, Vec<u8>>(0))
                    .map_err(map_sqlite)?
                    .collect::<Result<Vec<_>, _>>()
                    .map_err(map_sqlite)?
            };
            let mut sessions = BTreeSet::new();
            for raw in raw_events {
                let event =
                    parse_event(&raw).map_err(|error| AppendError::Corrupt(error.to_string()))?;
                let EventData::SessionMetadataChanged {
                    field,
                    new_value_ref,
                } = event.body.data
                else {
                    continue;
                };
                if field != "session_kind" {
                    continue;
                }
                if let PayloadRead::Present(payload) = store
                    .read_payload(&new_value_ref.payload_id)
                    .map_err(upgrade::storage)?
                    && serde_json::from_slice::<serde_json::Value>(&payload.body)
                        .map_err(|error| AppendError::Corrupt(error.to_string()))?
                        .as_str()
                        == Some("temporary")
                {
                    sessions.insert(event.session_id);
                }
            }
            Ok(sessions.into_iter().collect())
        }
        installation::NativeBackend::Jsonl => JsonlEventStore::open(data_dir)?.temporary_sessions(),
    }
}

pub(super) fn retain_process(data_dir: &Path) -> Result<String, AppendError> {
    let directory = data_dir.join("sessions/temporary-owners");
    filesystem::directory(&directory)?;
    let path = directory.join(format!("{}.lock", *PROCESS));
    let mut leases = LEASES.lock();
    if let std::collections::btree_map::Entry::Vacant(entry) = leases.entry(path) {
        let file = filesystem::file(entry.key(), false).map_err(jsonl::storage)?;
        file.lock().map_err(jsonl::storage)?;
        entry.insert(file);
    }
    Ok(PROCESS.to_string())
}

pub(super) fn owner_is_live(data_dir: &Path, owner: &str) -> Result<bool, AppendError> {
    let id = EventId::parse(owner)?;
    let directory = data_dir.join("sessions/temporary-owners");
    filesystem::directory(&directory)?;
    let file =
        filesystem::file(&directory.join(format!("{id}.lock")), false).map_err(jsonl::storage)?;
    match file.try_lock() {
        Ok(()) => {
            file.unlock().map_err(jsonl::storage)?;
            Ok(false)
        }
        Err(std::fs::TryLockError::WouldBlock) => Ok(true),
        Err(std::fs::TryLockError::Error(error)) => Err(jsonl::storage(error)),
    }
}

pub(super) fn discard_if_closed(
    data_dir: &Path,
    backend: installation::NativeBackend,
    store: &dyn SessionEventStore,
    session_id: &SessionId,
) -> Result<(), AppendError> {
    let mut after = EventCursor::beginning(session_id.clone());
    let head = store.head(session_id).map_err(upgrade::storage)?;
    let mut temporary = false;
    let mut closed = false;
    loop {
        let page = store
            .read_page(session_id, &after, Some(&head), 256)
            .map_err(upgrade::storage)?;
        for event in &page.events {
            match &event.body.data {
                EventData::SessionMetadataChanged {
                    field,
                    new_value_ref,
                } if field == "session_kind" => {
                    if let PayloadRead::Present(payload) = store
                        .read_payload(&new_value_ref.payload_id)
                        .map_err(upgrade::storage)?
                    {
                        temporary = serde_json::from_slice::<serde_json::Value>(&payload.body)
                            .map_err(|error| AppendError::Corrupt(error.to_string()))?
                            .as_str()
                            == Some("temporary");
                    }
                }
                EventData::SessionClosed { .. } => closed = true,
                _ => {}
            }
        }
        if !page.has_more {
            break;
        }
        after.sequence = page
            .events
            .last()
            .ok_or_else(|| AppendError::Corrupt("empty temporary page".into()))?
            .sequence;
    }
    if temporary && closed {
        discard(data_dir, backend, session_id)?;
    }
    Ok(())
}

pub(super) fn discard(
    data_dir: &Path,
    backend: installation::NativeBackend,
    session_id: &SessionId,
) -> Result<(), AppendError> {
    match backend {
        installation::NativeBackend::Sqlite => {
            let store = SqliteEventStore::open(data_dir)?;
            let image = store.export_native(None)?;
            let (retained, removed) = without_session(image, session_id)?;
            let payloads: BTreeSet<_> = retained
                .payloads
                .iter()
                .map(|payload| payload.reference.payload_id.as_str())
                .collect();
            let mut connection = store.conn.lock();
            let tx = connection
                .transaction_with_behavior(TransactionBehavior::Immediate)
                .map_err(map_sqlite)?;
            for id in removed {
                for table in ["session_events", "session_append_receipts", "session_index"] {
                    tx.execute(
                        &format!("DELETE FROM {table} WHERE session_id=?1"),
                        params![id.as_str()],
                    )
                    .map_err(map_sqlite)?;
                }
            }
            let ids = {
                let mut statement = tx
                    .prepare("SELECT payload_id FROM event_payloads")
                    .map_err(map_sqlite)?;
                statement
                    .query_map([], |row| row.get::<_, String>(0))
                    .map_err(map_sqlite)?
                    .collect::<Result<Vec<_>, _>>()
                    .map_err(map_sqlite)?
            };
            for id in ids {
                if !payloads.contains(id.as_str()) {
                    tx.execute(
                        "DELETE FROM event_payloads WHERE payload_id=?1",
                        params![id],
                    )
                    .map_err(map_sqlite)?;
                }
            }
            tx.commit().map_err(map_sqlite)?;
            checkpoint_erasure(&connection)
        }
        installation::NativeBackend::Jsonl => {
            let store = JsonlEventStore::open(data_dir)?;
            let (retained, _) = without_session(store.export_native(None)?, session_id)?;
            store.replace_native(&retained)
        }
    }
}

fn without_session(
    mut image: migration::NativeImage,
    session_id: &SessionId,
) -> Result<(migration::NativeImage, BTreeSet<SessionId>), AppendError> {
    let events = image.events()?;
    let by_id: BTreeMap<_, _> = events
        .iter()
        .map(|event| (&event.event_id, event))
        .collect();
    let mut removed = BTreeSet::from([session_id.clone()]);
    loop {
        let before = removed.len();
        for event in &events {
            if matches!(event.body.data, EventData::DerivedSessionStarted { .. })
                && event
                    .causation_id
                    .as_ref()
                    .and_then(|id| by_id.get(id))
                    .is_some_and(|parent| removed.contains(&parent.session_id))
            {
                removed.insert(event.session_id.clone());
            }
        }
        if before == removed.len() {
            break;
        }
    }
    image
        .batches
        .retain(|batch| !removed.contains(&batch.stream));
    let rows: BTreeMap<_, _> = image
        .payloads
        .iter()
        .map(|payload| (payload.reference.payload_id.clone(), payload))
        .collect();
    let mut pending: Vec<StoredPayloadRef> = Vec::new();
    for batch in &image.batches {
        for raw in &batch.raw {
            let value = serde_json::from_slice(raw)
                .map_err(|error| AppendError::Corrupt(error.to_string()))?;
            migration::raw_payload_refs(&value, &mut pending)?;
        }
    }
    let mut needed = BTreeSet::new();
    while let Some(reference) = pending.pop() {
        if !needed.insert(reference.payload_id.clone()) {
            continue;
        }
        let payload = rows
            .get(&reference.payload_id)
            .ok_or_else(|| AppendError::Corrupt("retained session payload missing".into()))?;
        pending.extend(payloads::nested_refs(&payloads::StoredPayload {
            reference: payload.reference.clone(),
            body: payload.body.clone(),
        })?);
    }
    image
        .payloads
        .retain(|payload| needed.contains(&payload.reference.payload_id));
    image.validate()?;
    Ok((image, removed))
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::session_events::{
        catalog::{SessionCatalog, SessionCopy},
        installation::NativeBackend,
        scoped::{EventStorePool, ScopedEventStore},
    };
    use std::io::{Read, Write};
    use zeroclaw_api::session_events::{BranchId, EventActor, context_read::CopySource};

    fn create(directory: &Path, backend: NativeBackend) {
        super::super::upgrade::upgrade_installation(directory, backend, |_| "fixture".into())
            .expect("installation");
        let catalog = SessionCatalog::open(directory, &EventStorePool::default())
            .expect("catalog")
            .expect("native");
        catalog
            .commands()
            .create("source", "rpc", "fixture", vec![])
            .expect("source");
        let id = SessionId::parse("source").expect("id");
        let source = CopySource {
            cursor: catalog.store().head(&id).expect("head"),
            branch_id: BranchId::parse("branch-source").expect("branch"),
        };
        let temporary_id = SessionId::parse("temporary").expect("id");
        catalog
            .commands()
            .create_temporary(SessionCopy {
                session_id: &temporary_id,
                source: &source,
                creation_source: "rpc",
                owning_agent: "fixture",
            })
            .expect("temporary");
    }

    #[test]
    fn process_fixture() {
        let Some(directory) = std::env::var_os("ZEROCLAW_TEMPORARY_PROCESS_FIXTURE") else {
            return;
        };
        let directory = std::path::PathBuf::from(directory);
        let backend =
            NativeBackend::parse(&std::env::var("ZEROCLAW_TEMPORARY_BACKEND").expect("backend"))
                .expect("format");
        create(&directory, backend);
        fs::write(directory.join("ready"), b"ready").expect("signal");
        std::io::stdin()
            .read_exact(&mut [0])
            .expect("release process");
    }

    #[test]
    fn live_temporary_is_hidden_and_process_exit_discards_history() {
        for backend in [NativeBackend::Sqlite, NativeBackend::Jsonl] {
            let directory = tempfile::tempdir().expect("directory");
            let mut child =
                std::process::Command::new(std::env::current_exe().expect("test executable"))
                    .args([
                        "--exact",
                        "session_events::temporary::tests::process_fixture",
                        "--nocapture",
                    ])
                    .env("ZEROCLAW_TEMPORARY_PROCESS_FIXTURE", directory.path())
                    .env("ZEROCLAW_TEMPORARY_BACKEND", backend.name())
                    .stdin(std::process::Stdio::piped())
                    .stdout(std::process::Stdio::null())
                    .spawn()
                    .expect("fixture process");
            let deadline = std::time::Instant::now() + std::time::Duration::from_secs(20);
            while !directory.path().join("ready").exists() {
                assert!(
                    child.try_wait().expect("status").is_none(),
                    "fixture exited early"
                );
                assert!(
                    std::time::Instant::now() < deadline,
                    "fixture startup timed out"
                );
                std::thread::sleep(std::time::Duration::from_millis(10));
            }
            let id = SessionId::parse("temporary").expect("id");
            let pool = EventStorePool::default();
            let catalog = SessionCatalog::open(directory.path(), &pool)
                .expect("catalog")
                .expect("native");
            assert!(catalog.store().head(&id).expect("live head").sequence > 0);
            assert_eq!(catalog.query().index_records().expect("list").len(), 1);
            drop(catalog);
            child
                .stdin
                .take()
                .expect("stdin")
                .write_all(b"x")
                .expect("finish process");
            assert!(child.wait().expect("exit").success());
            let catalog = SessionCatalog::open(directory.path(), &pool)
                .expect("cleanup")
                .expect("native");
            assert_eq!(
                catalog.store().head(&id).expect("discarded head").sequence,
                0
            );
            assert_eq!(catalog.query().index_records().expect("list").len(), 1);
        }
    }

    #[test]
    fn close_removes_temporary_bytes_and_fences_escaped_writer() {
        for backend in [NativeBackend::Sqlite, NativeBackend::Jsonl] {
            let directory = tempfile::tempdir().expect("directory");
            create(directory.path(), backend);
            let id = SessionId::parse("temporary").expect("id");
            let store = ScopedEventStore::open(directory.path(), "temporary").expect("store");
            let payload = PayloadRecord::json("session.bytes/v1", &"private temporary value")
                .expect("payload");
            let event = SessionEvent::typed(
                id.clone(),
                EventData::SessionMetadataChanged {
                    field: "title".into(),
                    new_value_ref: StoredPayloadRef::from(&payload),
                },
                EventActor::default(),
            )
            .expect("event");
            store
                .append_batch(AppendRequest {
                    session_id: id.clone(),
                    expected_head: store.head(&id).expect("head"),
                    idempotency_key: "temporary-title".into(),
                    events: vec![event],
                    payloads: vec![payload.clone()],
                    erasure_reference_commitment: None,
                })
                .expect("private value");
            let closed = SessionEvent::typed(
                id.clone(),
                EventData::SessionClosed {
                    reason_key: "user_closed".into(),
                },
                EventActor::default(),
            )
            .expect("close");
            let request = AppendRequest {
                session_id: id.clone(),
                expected_head: store.head(&id).expect("head"),
                idempotency_key: "close".into(),
                events: vec![closed],
                payloads: vec![],
                erasure_reference_commitment: None,
            };
            store.append_batch(request.clone()).expect("close");
            assert_eq!(store.head(&id).expect("head").sequence, 0);
            assert!(matches!(
                store.read_payload(&payload.payload_id),
                Err(ReadError::PayloadMissing(_))
            ));
            assert!(matches!(
                store.append_batch(request),
                Err(AppendError::WriterFenced(_))
            ));
            let source = SessionId::parse("source").expect("source");
            assert!(store.head(&source).expect("source retained").sequence > 0);
        }
    }
}
