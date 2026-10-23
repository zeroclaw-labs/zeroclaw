//! Immutable batch segments and one atomic commit root. Orphans are never read.
use super::*;
use parking_lot::MutexGuard;
use serde::{Deserialize, Serialize};
use std::collections::{BTreeMap, HashSet};
use std::fs::File;
use std::io::{Read, Write};
use std::path::PathBuf;
use zeroclaw_api::session_events::canonical_json;

#[derive(Clone, Debug, PartialEq, Eq, Serialize, Deserialize)]
struct FileRef {
    id: EventId,
    digest: Digest,
    byte_len: u64,
}
#[derive(Serialize, Deserialize)]
struct Root {
    version: u32,
    tip: Option<FileRef>,
}
#[derive(Serialize, Deserialize)]
struct Commit {
    previous: Option<FileRef>,
    segment: FileRef,
    stream_id: SessionStreamId,
    idempotency_key: String,
    commitment: Digest,
    first: u64,
    last: u64,
    payloads: Vec<PayloadMeta>,
}
#[derive(Clone, Serialize, Deserialize)]
struct PayloadMeta {
    payload_id: PayloadId,
    codec: String,
    digest: Digest,
    byte_len: u64,
}
// Raw event bytes remain canonical in verified immutable segments. Retain
// coordinates instead of a second in-memory copy of every serialized row.
#[derive(Clone, Copy)]
struct RawSpan {
    offset: usize,
    len: usize,
}
type Row = (RawSpan, EventId, Digest);
#[derive(Default)]
struct Snapshot {
    initialized: bool,
    history: zeroclaw_api::session_events::rules::HistoryIndex,
    payload_index: zeroclaw_api::session_events::payloads::PayloadIndex,
    tip: Option<FileRef>,
    // Disposable coordinates derived only from verified committed segments.
    positions: BTreeMap<EventId, (String, usize, FileRef, FileRef)>,
    erased: BTreeMap<PayloadId, zeroclaw_api::session_events::payloads::PayloadDisposition>,
    erasure_targets: BTreeMap<EventId, Vec<PayloadId>>,
    rows: BTreeMap<String, Vec<Row>>,
    receipts: BTreeMap<(String, String), (Digest, AppendReceipt)>,
    payloads: BTreeMap<String, PayloadMeta>,
}

/// The file lock covers publication, including opening and reading snapshots.
/// It is released by closing this separately opened descriptor on every path.
struct PublishGuard(File);
impl Drop for PublishGuard {
    fn drop(&mut self) {
        let _ = self.0.unlock();
    }
}

pub struct JsonlEventStore {
    root: PathBuf,
    // The manifest remains canonical. Refresh only its verified extension;
    // no mutation may publish through this disposable materialized view.
    index: Mutex<Snapshot>,
}
impl JsonlEventStore {
    pub(super) fn root_streams(&self) -> Result<Vec<SessionStreamId>, AppendError> {
        let _guard = self.lock()?;
        let snapshot = self.snapshot()?;
        let mut result = Vec::new();
        for rows in snapshot.rows.values() {
            if let Some((_, id, _)) = rows.first() {
                let event = snapshot
                    .history
                    .get_event(id)
                    .ok_or_else(|| AppendError::Corrupt("family root absent".into()))?;
                self.verify_events(&snapshot, std::iter::once(event))?;
                if matches!(
                    event.body.data,
                    zeroclaw_api::session_events::EventData::SessionStarted { .. }
                ) {
                    result.push(event.stream_id.clone());
                }
            }
        }
        Ok(result)
    }
    pub(super) fn export_native(
        &self,
        root: Option<&SessionStreamId>,
    ) -> Result<super::migration::NativeImage, AppendError> {
        use super::migration::*;
        let _guard = self.lock()?;
        let snapshot = self.snapshot()?;
        let mut chain = Vec::new();
        let mut tip = snapshot.tip.clone();
        while let Some(reference) = tip {
            let commit: Commit = decode(&self.read_file("commits", &reference, "json")?)?;
            let raw = self
                .read_file("segments", &commit.segment, "jsonl")?
                .split(|byte| *byte == b'\n')
                .filter(|line| !line.is_empty())
                .map(<[u8]>::to_vec)
                .collect();
            chain.push(NativeBatch {
                stream: commit.stream_id,
                key: commit.idempotency_key,
                commitment: commit.commitment,
                raw,
            });
            tip = commit.previous;
        }
        chain.reverse();
        let mut payloads = Vec::new();
        for meta in snapshot.payloads.values() {
            let body = if zeroclaw_api::session_events::payloads::content_leaf(&meta.codec) {
                None
            } else {
                match filesystem::read(&self.payload_path(&meta.payload_id)?) {
                    Ok(body) => Some(body),
                    Err(error)
                        if error.kind() == std::io::ErrorKind::NotFound
                            && snapshot.erased.contains_key(&meta.payload_id) =>
                    {
                        None
                    }
                    Err(error) => return Err(AppendError::Corrupt(error.to_string())),
                }
            };
            payloads.push(NativePayload {
                reference: zeroclaw_api::session_events::StoredPayloadRef {
                    payload_id: meta.payload_id.clone(),
                    codec: meta.codec.clone(),
                    body_digest: meta.digest.clone(),
                    byte_len: meta.byte_len,
                },
                body,
            });
        }
        let mut selected = select_family(
            NativeImage {
                batches: chain,
                payloads,
            },
            root,
        )?;
        for payload in &mut selected.payloads {
            if zeroclaw_api::session_events::payloads::content_leaf(&payload.reference.codec) {
                payload.body =
                    match filesystem::read(&self.payload_path(&payload.reference.payload_id)?) {
                        Ok(body) => Some(body),
                        Err(error)
                            if error.kind() == std::io::ErrorKind::NotFound
                                && snapshot.erased.contains_key(&payload.reference.payload_id) =>
                        {
                            None
                        }
                        Err(error) => return Err(AppendError::Corrupt(error.to_string())),
                    };
            }
        }
        selected.validate()?;
        Ok(selected)
    }

    pub(super) fn import_native(
        &self,
        image: &super::migration::NativeImage,
    ) -> Result<(), AppendError> {
        image.validate()?;
        let _guard = self.lock()?;
        let snapshot = self.snapshot()?;
        let mut tip = snapshot.tip.clone();
        let mut payloads = Vec::new();
        for payload in &image.payloads {
            let r = &payload.reference;
            if let Some(existing) = snapshot.payloads.get(r.payload_id.as_str()) {
                if existing.codec != r.codec
                    || existing.digest != r.body_digest
                    || existing.byte_len != r.byte_len
                {
                    return Err(AppendError::Corrupt(
                        "migration target payload conflict".into(),
                    ));
                }
                let existing_body = match filesystem::read(&self.payload_path(&r.payload_id)?) {
                    Ok(body) => Some(body),
                    Err(error) if error.kind() == std::io::ErrorKind::NotFound => None,
                    Err(error) => return Err(storage(error)),
                };
                if existing_body != payload.body {
                    return Err(AppendError::Corrupt(
                        "migration target payload bytes differ".into(),
                    ));
                }
                continue;
            }
            if let Some(body) = &payload.body {
                let path = self.payload_path(&r.payload_id)?;
                let tmp = self.path("staging", &EventId::new(), "tmp")?;
                write_synced(&tmp, body)?;
                fs::rename(tmp, path).map_err(storage)?;
            }
            payloads.push(PayloadMeta {
                payload_id: r.payload_id.clone(),
                codec: r.codec.clone(),
                digest: r.body_digest.clone(),
                byte_len: r.byte_len,
            });
        }
        sync_directory(&self.root.join("payloads"))?;
        let mut expected = BTreeMap::new();
        for batch in &image.batches {
            let first = parse_event(
                batch
                    .raw
                    .first()
                    .ok_or_else(|| AppendError::Corrupt("empty migration batch".into()))?,
            )
            .map_err(|error| AppendError::Corrupt(error.to_string()))?;
            let last = parse_event(
                batch
                    .raw
                    .last()
                    .ok_or_else(|| AppendError::Corrupt("empty migration batch".into()))?,
            )
            .map_err(|error| AppendError::Corrupt(error.to_string()))?;
            expected.insert(batch.stream.clone(), last.sequence);
            if let Some((commitment, receipt)) = snapshot
                .receipts
                .get(&(batch.stream.to_string(), batch.key.clone()))
            {
                if *commitment != batch.commitment
                    || receipt.first.sequence != first.sequence
                    || receipt.last.sequence != last.sequence
                {
                    return Err(AppendError::Corrupt(
                        "migration target receipt differs".into(),
                    ));
                }
                for raw in &batch.raw {
                    let event = parse_event(raw)
                        .map_err(|error| AppendError::Corrupt(error.to_string()))?;
                    let (_, _, segment, _) =
                        snapshot.positions.get(&event.event_id).ok_or_else(|| {
                            AppendError::Corrupt("migration target event missing".into())
                        })?;
                    let stored = self.read_file("segments", segment, "jsonl")?;
                    if !stored.split(|byte| *byte == b'\n').any(|line| line == raw) {
                        return Err(AppendError::Corrupt(
                            "migration target raw event differs".into(),
                        ));
                    }
                }
                continue;
            }
            if snapshot
                .rows
                .get(batch.stream.as_str())
                .is_some_and(|rows| rows.len() as u64 >= first.sequence)
            {
                return Err(AppendError::Corrupt(
                    "migration target overlaps unverified events".into(),
                ));
            }
            let mut raw = Vec::new();
            for event in &batch.raw {
                raw.extend(event);
                raw.push(b'\n');
            }
            if raw.len() > MAX_PAGE_BYTES {
                return Err(AppendError::Corrupt("migration segment too large".into()));
            }
            let segment = self.write_file("segments", "jsonl", &raw)?;
            let commit = Commit {
                previous: tip,
                segment,
                stream_id: batch.stream.clone(),
                idempotency_key: batch.key.clone(),
                commitment: batch.commitment.clone(),
                first: first.sequence,
                last: last.sequence,
                payloads: std::mem::take(&mut payloads),
            };
            tip = Some(self.write_file("commits", "json", &canonical_json(&commit)?)?);
        }
        for (stream, sequence) in expected {
            if snapshot
                .rows
                .get(stream.as_str())
                .is_some_and(|rows| rows.len() as u64 > sequence)
            {
                return Err(AppendError::Corrupt(
                    "migration target has additional events".into(),
                ));
            }
        }
        if !payloads.is_empty() {
            return Err(AppendError::Corrupt(
                "migration has payloads without new batches".into(),
            ));
        }
        // All family batches appear through one root publication, never a
        // partly imported family. A crash leaves only unreachable orphans.
        self.publish_root(&Root { version: 1, tip })?;
        drop(snapshot);
        drop(self.snapshot()?);
        Ok(())
    }
    pub fn open(data_dir: &Path) -> Result<Self, AppendError> {
        let root = data_dir.join("sessions/session-events-jsonl");
        filesystem::directory(&data_dir.join("sessions"))?;
        filesystem::directory(&root)?;
        for dir in ["commits", "segments", "payloads", "staging"] {
            filesystem::directory(&root.join(dir))?;
        }
        let store = Self {
            root,
            index: Mutex::new(Snapshot::default()),
        };
        let _guard = store.lock()?;
        if !store.root_path().exists() {
            for directory in ["commits", "segments", "payloads"] {
                if fs::read_dir(store.root.join(directory))
                    .map_err(storage)?
                    .next()
                    .is_some()
                {
                    return Err(AppendError::Corrupt(
                        "commit root missing for nonempty store".into(),
                    ));
                }
            }
            store.publish_root(&Root {
                version: 1,
                tip: None,
            })?;
        }
        drop(store.snapshot()?);
        Ok(store)
    }
    fn root_path(&self) -> PathBuf {
        self.root.join("commit-manifest.json")
    }
    fn lock(&self) -> Result<PublishGuard, AppendError> {
        let file = filesystem::file(&self.root.join("publish.lock"), false).map_err(storage)?;
        file.lock().map_err(storage)?;
        Ok(PublishGuard(file))
    }
    fn path(&self, directory: &str, id: &EventId, extension: &str) -> Result<PathBuf, AppendError> {
        EventId::parse(id.as_str()).map_err(AppendError::from)?;
        Ok(self.root.join(directory).join(format!("{id}.{extension}")))
    }
    fn payload_path(&self, id: &PayloadId) -> Result<PathBuf, AppendError> {
        PayloadId::parse(id.as_str()).map_err(AppendError::from)?;
        Ok(self.root.join("payloads").join(format!("{id}.bin")))
    }
    fn read_file(
        &self,
        directory: &str,
        reference: &FileRef,
        extension: &str,
    ) -> Result<Vec<u8>, AppendError> {
        let limit = reference
            .byte_len
            .checked_add(1)
            .ok_or_else(|| AppendError::Corrupt("committed file size overflow".into()))?;
        if reference.byte_len > MAX_PAGE_BYTES as u64 {
            return Err(AppendError::Corrupt(
                "committed file exceeds size limit".into(),
            ));
        }
        let mut raw = Vec::new();
        filesystem::reader(&self.path(directory, &reference.id, extension)?)
            .and_then(|file| file.take(limit).read_to_end(&mut raw))
            .map_err(|error| {
                AppendError::Corrupt(format!(
                    "committed {directory} file missing or unreadable: {error}"
                ))
            })?;
        if raw.len() as u64 != reference.byte_len || Digest::from_bytes(&raw) != reference.digest {
            return Err(AppendError::Corrupt(format!(
                "invalid {directory} file digest"
            )));
        }
        Ok(raw)
    }
    fn write_file(
        &self,
        directory: &str,
        extension: &str,
        bytes: &[u8],
    ) -> Result<FileRef, AppendError> {
        let id = EventId::new();
        let temporary = self.path("staging", &id, "tmp")?;
        let target = self.path(directory, &id, extension)?;
        write_synced(&temporary, bytes)?;
        fs::rename(&temporary, target).map_err(storage)?;
        sync_directory(&self.root.join(directory))?;
        Ok(FileRef {
            id,
            digest: Digest::from_bytes(bytes),
            byte_len: bytes.len() as u64,
        })
    }
    fn publish_root(&self, root: &Root) -> Result<(), AppendError> {
        let tmp = self.path("staging", &EventId::new(), "tmp")?;
        write_synced(&tmp, &canonical_json(root)?)?;
        // Everything this root names is already durable. A failure after the
        // rename is ambiguous and must never be treated as an aborted append.
        fs::rename(&tmp, self.root_path()).map_err(unknown)?;
        sync_directory(&self.root)
            .map_err(|error| AppendError::CommitOutcomeUnknown(error.to_string()))
    }
    fn snapshot(&self) -> Result<MutexGuard<'_, Snapshot>, AppendError> {
        let mut snapshot = self.index.lock();
        if let Err(error) = self.refresh_snapshot(&mut snapshot) {
            *snapshot = Snapshot::default();
            return Err(error);
        }
        Ok(snapshot)
    }
    fn refresh_snapshot(&self, snapshot: &mut Snapshot) -> Result<(), AppendError> {
        let raw = filesystem::read(&self.root_path()).map_err(storage)?;
        let root: Root = decode(&raw)?;
        if root.version != 1 {
            return Err(AppendError::UnsupportedSchema(root.version.to_string()));
        }
        if snapshot.initialized && root.tip == snapshot.tip {
            if let Some(tip) = &root.tip {
                self.read_file("commits", tip, "json")?;
            }
            return Ok(());
        }
        let target_tip = root.tip.clone();
        let mut next = root.tip;
        let mut seen = HashSet::new();
        let mut commits = Vec::new();
        while let Some(reference) = next.clone() {
            if snapshot.initialized && Some(&reference) == snapshot.tip.as_ref() {
                break;
            }
            if !seen.insert(reference.id.clone()) {
                return Err(AppendError::Corrupt("cyclic commit chain".to_owned()));
            }
            let commit: Commit = decode(&self.read_file("commits", &reference, "json")?)?;
            next = commit.previous.clone();
            commits.push((reference, commit));
        }
        if snapshot.initialized && next != snapshot.tip {
            return Err(AppendError::Corrupt(
                "commit manifest regressed or changed lineage".into(),
            ));
        }
        for (commit_reference, commit) in commits.into_iter().rev() {
            let raw = self.read_file("segments", &commit.segment, "jsonl")?;
            if raw.last() != Some(&b'\n') {
                return Err(AppendError::Corrupt(
                    "committed segment has incomplete tail".to_owned(),
                ));
            }
            let rows = snapshot
                .rows
                .entry(commit.stream_id.to_string())
                .or_default();
            let expected = (rows.len() as u64)
                .checked_add(1)
                .ok_or_else(|| AppendError::Corrupt("sequence overflow".into()))?;
            if expected != commit.first {
                return Err(AppendError::Corrupt("commit sequence gap".to_owned()));
            }
            let mut event_ids = Vec::new();
            let mut committed_events = Vec::new();
            for (offset, line) in raw
                .split(|byte| *byte == b'\n')
                .scan(0_usize, |offset, line| {
                    let start = *offset;
                    *offset += line.len() + 1;
                    Some((start, line))
                })
                .filter(|(_, line)| !line.is_empty())
            {
                let event =
                    parse_event(line).map_err(|error| AppendError::Corrupt(error.to_string()))?;
                let previous = rows
                    .last()
                    .map_or_else(Digest::zero, |(_, _, hash)| hash.clone());
                if event.stream_id != commit.stream_id
                    || event.sequence
                        != commit
                            .first
                            .checked_add(event_ids.len() as u64)
                            .ok_or_else(|| AppendError::Corrupt("sequence overflow".into()))?
                    || event.previous_hash != previous
                    || snapshot.positions.contains_key(&event.event_id)
                {
                    return Err(AppendError::Corrupt(
                        "invalid segment identity, sequence or chain".to_owned(),
                    ));
                }
                use zeroclaw_api::session_events::{EventData, payloads::PayloadDisposition};
                match &event.body.data {
                    EventData::PayloadErasureRequested { targets, .. } => {
                        let ids: Vec<_> = targets
                            .iter()
                            .map(|target| target.payload_id.clone())
                            .collect();
                        for id in &ids {
                            snapshot
                                .erased
                                .insert(id.clone(), PayloadDisposition::ErasurePending);
                        }
                        snapshot.erasure_targets.insert(event.event_id.clone(), ids);
                    }
                    EventData::PayloadRedacted {
                        erasure_requested_event_id,
                    } => {
                        let targets = snapshot
                            .erasure_targets
                            .get(erasure_requested_event_id)
                            .ok_or_else(|| {
                                AppendError::Corrupt("redaction index intent missing".into())
                            })?;
                        for id in targets {
                            snapshot
                                .erased
                                .insert(id.clone(), PayloadDisposition::Redacted);
                        }
                    }
                    _ => {}
                }
                snapshot.positions.insert(
                    event.event_id.clone(),
                    (
                        commit.stream_id.to_string(),
                        rows.len(),
                        commit.segment.clone(),
                        commit_reference.clone(),
                    ),
                );
                let hash = hash_raw(&previous, line);
                event_ids.push(event.event_id.clone());
                committed_events.push(event.clone());
                rows.push((
                    RawSpan {
                        offset,
                        len: line.len(),
                    },
                    event.event_id,
                    hash,
                ));
            }
            let Some((_, _, hash)) = rows.last() else {
                return Err(AppendError::Corrupt("empty segment".to_owned()));
            };
            if event_ids.is_empty() || rows.len() as u64 != commit.last {
                return Err(AppendError::Corrupt("invalid commit range".to_owned()));
            }
            let receipt = AppendReceipt {
                first: EventCursor {
                    stream_id: commit.stream_id.clone(),
                    sequence: commit.first,
                },
                last: EventCursor {
                    stream_id: commit.stream_id.clone(),
                    sequence: commit.last,
                },
                event_ids,
                head_hash: hash.clone(),
            };
            if snapshot
                .receipts
                .insert(
                    (commit.stream_id.to_string(), commit.idempotency_key),
                    (commit.commitment, receipt),
                )
                .is_some()
            {
                return Err(AppendError::Corrupt(
                    "duplicate committed receipt".to_owned(),
                ));
            }
            let mut committed_payloads = Vec::new();
            for payload in commit.payloads {
                PayloadId::parse(payload.payload_id.as_str())?;
                let body = if zeroclaw_api::session_events::payloads::content_leaf(&payload.codec) {
                    None
                } else {
                    let file = filesystem::reader(&self.payload_path(&payload.payload_id)?)
                        .map_err(storage)?;
                    if payload.byte_len > MAX_PAGE_BYTES as u64 {
                        return Err(AppendError::Corrupt(
                            "structural payload exceeds size limit".into(),
                        ));
                    }
                    let mut bytes = Vec::new();
                    file.take(payload.byte_len + 1)
                        .read_to_end(&mut bytes)
                        .map_err(storage)?;
                    if bytes.len() as u64 != payload.byte_len
                        || Digest::from_bytes(&bytes) != payload.digest
                    {
                        return Err(AppendError::Corrupt(
                            "structural payload integrity failure".into(),
                        ));
                    }
                    Some(bytes)
                };
                committed_payloads.push(zeroclaw_api::session_events::payloads::StoredPayload {
                    reference: zeroclaw_api::session_events::StoredPayloadRef {
                        payload_id: payload.payload_id.clone(),
                        codec: payload.codec.clone(),
                        body_digest: payload.digest.clone(),
                        byte_len: payload.byte_len,
                    },
                    body,
                });
                if let Some(old) = snapshot.payloads.get(payload.payload_id.as_str()) {
                    if old.codec != payload.codec
                        || old.digest != payload.digest
                        || old.byte_len != payload.byte_len
                    {
                        return Err(AppendError::Corrupt(
                            "conflicting committed payload".to_owned(),
                        ));
                    }
                } else {
                    snapshot
                        .payloads
                        .insert(payload.payload_id.to_string(), payload);
                }
            }
            snapshot.history.extend_committed(&committed_events)?;
            snapshot.payload_index.extend_committed(
                &committed_events,
                committed_payloads,
                &snapshot.history,
            )?;
        }
        snapshot.tip = target_tip;
        snapshot.initialized = true;
        Ok(())
    }
    fn verify_events<'a>(
        &self,
        snapshot: &Snapshot,
        events: impl IntoIterator<Item = &'a SessionEvent>,
    ) -> Result<(), AppendError> {
        let mut verified = HashSet::new();
        for event in events {
            let (_, _, segment, commit) = snapshot
                .positions
                .get(&event.event_id)
                .ok_or_else(|| AppendError::Corrupt("event segment index missing".into()))?;
            if verified.insert(segment.id.clone()) {
                self.read_file("commits", commit, "json")?;
                self.read_file("segments", segment, "jsonl")?;
            }
        }
        Ok(())
    }
}
pub(super) fn storage(error: std::io::Error) -> AppendError {
    AppendError::StorageUnavailable(error.to_string())
}
fn unknown(error: std::io::Error) -> AppendError {
    AppendError::CommitOutcomeUnknown(error.to_string())
}
fn decode<T: serde::de::DeserializeOwned>(raw: &[u8]) -> Result<T, AppendError> {
    serde_json::from_slice(raw).map_err(|error| AppendError::Corrupt(error.to_string()))
}
fn write_synced(path: &Path, bytes: &[u8]) -> Result<(), AppendError> {
    let mut file = filesystem::file(path, true).map_err(storage)?;
    file.write_all(bytes).map_err(storage)?;
    file.sync_all().map_err(storage)
}
pub(super) fn sync_directory(path: &Path) -> Result<(), AppendError> {
    #[cfg(unix)]
    {
        File::open(path)
            .and_then(|file| file.sync_all())
            .map_err(storage)
    }
    #[cfg(not(unix))]
    {
        let _ = path;
        Err(AppendError::StorageUnavailable(
            "JSONL directory durability is unsupported on this platform".into(),
        ))
    }
}
fn as_read(error: AppendError) -> ReadError {
    match error {
        AppendError::Corrupt(reason) => ReadError::Corrupt(reason),
        AppendError::UnsupportedSchema(reason) => ReadError::UnsupportedSchema(reason),
        other => ReadError::StorageUnavailable(other.to_string()),
    }
}
impl SessionEventStore for JsonlEventStore {
    fn read_latest_event(
        &self,
        stream: &SessionStreamId,
        branch: &zeroclaw_api::session_events::BranchId,
        turn: Option<&zeroclaw_api::session_events::TurnId>,
        kind: &zeroclaw_api::session_events::EventKind,
    ) -> Result<Option<SessionEvent>, ReadError> {
        let _guard = self.lock().map_err(as_read)?;
        let snapshot = self.snapshot().map_err(as_read)?;
        let event = snapshot
            .rows
            .get(stream.as_str())
            .into_iter()
            .flat_map(|rows| rows.iter().rev())
            .filter_map(|(_, id, _)| snapshot.history.get_event(id))
            .find(|event| {
                event.branch_id.as_ref() == Some(branch)
                    && turn.is_none_or(|turn| event.turn_id.as_ref() == Some(turn))
                    && event.body.kind() == *kind
            })
            .cloned();
        self.verify_events(&snapshot, event.iter())
            .map_err(as_read)?;
        Ok(event)
    }
    fn read_event(&self, event_id: &EventId) -> Result<Option<SessionEvent>, ReadError> {
        let _guard = self.lock().map_err(as_read)?;
        let snapshot = self.snapshot().map_err(as_read)?;
        let event = snapshot.history.get_event(event_id).cloned();
        self.verify_events(&snapshot, event.iter())
            .map_err(as_read)?;
        Ok(event)
    }

    fn append_batch(&self, request: AppendRequest) -> Result<AppendReceipt, AppendError> {
        request.validate()?;
        let _guard = self.lock()?;
        let snapshot = self.snapshot()?;
        let commitment = request.commitment()?;
        if let Some((old, receipt)) = snapshot.receipts.get(&(
            request.stream_id.to_string(),
            request.idempotency_key.clone(),
        )) {
            if *old != commitment {
                return Err(AppendError::IdempotencyConflict(request.idempotency_key));
            }
            self.verify_events(&snapshot, request.events.iter())?;
            return Ok(receipt.clone());
        }
        let rows = snapshot.rows.get(request.stream_id.as_str());
        self.verify_events(
            &snapshot,
            rows.and_then(|rows| rows.last())
                .and_then(|(_, id, _)| snapshot.history.get_event(id)),
        )?;
        let sequence = rows.map_or(0, |rows| rows.len() as u64);
        let mut hash = rows
            .and_then(|rows| rows.last())
            .map_or_else(Digest::zero, |(_, _, hash)| hash.clone());
        if sequence != request.expected_head.sequence {
            return Err(AppendError::HeadConflict {
                expected: request.expected_head.sequence,
                current: sequence,
            });
        }
        snapshot.history.plan(&request)?;
        let payload_delta = snapshot.payload_index.plan(&snapshot.history, &request)?;
        let deleting = payload_delta.deleting().to_vec();

        let first = sequence
            .checked_add(1)
            .ok_or_else(|| AppendError::Corrupt("sequence overflow".to_owned()))?;
        let mut raw = Vec::new();
        let mut event_ids = Vec::new();
        let timestamp = now_millis();
        // The request already bounds payload bytes; include them when the
        // committed envelope and JSONL framing consume the remaining budget.
        let payload_bytes: usize = request.payloads.iter().map(|body| body.body.len()).sum();
        for (offset, event) in request.events.iter().enumerate() {
            if snapshot.positions.contains_key(&event.event_id) {
                return Err(AppendError::InvalidReference(
                    "event_id already committed".to_owned(),
                ));
            }
            let mut event = event.clone();
            event.sequence = first
                .checked_add(offset as u64)
                .ok_or_else(|| AppendError::Corrupt("sequence overflow".to_owned()))?;
            event.committed_at = timestamp;
            event.previous_hash = hash.clone();
            let bytes = event.canonical_bytes()?;
            committed_event_bytes(payload_bytes + raw.len(), bytes.len())?;
            hash = hash_raw(&hash, &bytes);
            raw.extend_from_slice(&bytes);
            raw.push(b'\n');
            event_ids.push(event.event_id);
        }
        let mut payloads = Vec::new();
        for payload in &request.payloads {
            if let Some(existing) = snapshot.payloads.get(payload.payload_id.as_str()) {
                if existing.codec != payload.codec
                    || existing.digest != payload.body_digest
                    || existing.byte_len != payload.byte_len
                {
                    return Err(
                        ContractError::PayloadConflict(payload.payload_id.to_string()).into(),
                    );
                }
                // A committed payload must exist and match; never repair corruption
                // silently by substituting caller bytes.
                let stored = filesystem::read(&self.payload_path(&payload.payload_id)?)
                    .map_err(|error| AppendError::Corrupt(error.to_string()))?;
                if stored != payload.body {
                    return Err(AppendError::Corrupt(
                        "committed payload bytes mismatch".to_owned(),
                    ));
                }
                continue;
            }
            let tmp = self.path("staging", &EventId::new(), "tmp")?;
            write_synced(&tmp, &payload.body)?;
            fs::rename(tmp, self.payload_path(&payload.payload_id)?).map_err(storage)?;
            sync_directory(&self.root.join("payloads"))?;
            payloads.push(PayloadMeta {
                payload_id: payload.payload_id.clone(),
                codec: payload.codec.clone(),
                digest: payload.body_digest.clone(),
                byte_len: payload.byte_len,
            });
        }
        let segment = self.write_file("segments", "jsonl", &raw)?;
        #[cfg(test)]
        super::crash_tests::crash_boundary("jsonl_segment_published");
        let last = first
            .checked_add(event_ids.len() as u64 - 1)
            .ok_or_else(|| AppendError::Corrupt("sequence overflow".to_owned()))?;
        let commit = Commit {
            previous: snapshot.tip.clone(),
            segment,
            stream_id: request.stream_id.clone(),
            idempotency_key: request.idempotency_key,
            commitment,
            first,
            last,
            payloads,
        };
        let tip = self.write_file("commits", "json", &canonical_json(&commit)?)?;
        #[cfg(test)]
        super::crash_tests::crash_boundary("jsonl_commit_published");
        // A committed erasure intent already freezes reads before this body
        // deletion. Crash before confirmation leaves ErasurePending and can
        // retry the same frozen confirmation without rerunning an effect.
        for id in deleting {
            match fs::remove_file(self.payload_path(&id)?) {
                Ok(()) => {}
                Err(error) if error.kind() == std::io::ErrorKind::NotFound => {}
                Err(error) => return Err(storage(error)),
            }
        }
        sync_directory(&self.root.join("payloads"))?;
        self.publish_root(&Root {
            version: 1,
            tip: Some(tip),
        })?;
        #[cfg(test)]
        super::crash_tests::crash_boundary("jsonl_root_published");
        Ok(AppendReceipt {
            first: EventCursor {
                stream_id: request.stream_id.clone(),
                sequence: first,
            },
            last: EventCursor {
                stream_id: request.stream_id,
                sequence: last,
            },
            event_ids,
            head_hash: hash,
        })
    }
    fn read_page(
        &self,
        stream_id: &SessionStreamId,
        after: &EventCursor,
        through: Option<&EventCursor>,
        max_events: usize,
        max_bytes: usize,
    ) -> Result<EventPage, ReadError> {
        validate_page_limits(max_events, max_bytes)?;
        let _guard = self.lock().map_err(as_read)?;
        let snapshot = self.snapshot().map_err(as_read)?;
        let rows = snapshot.rows.get(stream_id.as_str());
        let sequence = rows.map_or(0, |rows| rows.len() as u64);
        let through = through.cloned().unwrap_or(EventCursor {
            stream_id: stream_id.clone(),
            sequence,
        });
        if after.stream_id != *stream_id
            || through.stream_id != *stream_id
            || after.sequence > through.sequence
            || through.sequence > sequence
        {
            return Err(ReadError::Gap("invalid stream cursor".to_owned()));
        }
        let start = usize::try_from(after.sequence)
            .map_err(|_| ReadError::Gap("cursor exceeds addressable history".into()))?;
        let end = usize::try_from(through.sequence)
            .map_err(|_| ReadError::Gap("cursor exceeds addressable history".into()))?;
        let selected: Vec<_> = rows
            .into_iter()
            .flat_map(|rows| rows[start..end].iter())
            .take(max_events + 1)
            .cloned()
            .collect();
        self.verify_events(
            &snapshot,
            selected
                .iter()
                .filter_map(|(_, id, _)| snapshot.history.get_event(id)),
        )
        .map_err(as_read)?;
        let has_more = selected.len() > max_events;
        let mut segments = BTreeMap::new();
        let mut materialized = Vec::with_capacity(selected.len());
        for (span, id, hash) in selected {
            let event = snapshot
                .history
                .get_event(&id)
                .ok_or_else(|| ReadError::Corrupt("event identity absent from index".into()))?
                .clone();
            let (_, _, segment, _) = snapshot
                .positions
                .get(&id)
                .ok_or_else(|| ReadError::Corrupt("event segment coordinate missing".into()))?;
            if !segments.contains_key(&segment.id) {
                segments.insert(
                    segment.id.clone(),
                    self.read_file("segments", segment, "jsonl")
                        .map_err(as_read)?,
                );
            }
            let end = span
                .offset
                .checked_add(span.len)
                .ok_or_else(|| ReadError::Corrupt("event span overflow".into()))?;
            let bytes = segments
                .get(&segment.id)
                .and_then(|raw| raw.get(span.offset..end))
                .ok_or_else(|| ReadError::Corrupt("event span outside committed segment".into()))?
                .to_vec();
            materialized.push((bytes, event, hash));
        }
        let anchor_row = rows.and_then(|rows| {
            usize::try_from(after.sequence.checked_sub(1)?)
                .ok()
                .and_then(|position| rows.get(position))
        });
        self.verify_events(
            &snapshot,
            anchor_row.and_then(|(_, id, _)| snapshot.history.get_event(id)),
        )
        .map_err(as_read)?;
        let anchor = anchor_row.map_or_else(Digest::zero, |(_, _, hash)| hash.clone());
        event_page(
            stream_id.clone(),
            after.clone(),
            through,
            materialized,
            anchor,
            max_events,
            max_bytes,
            has_more,
        )
    }
    fn head(&self, stream_id: &SessionStreamId) -> Result<EventCursor, ReadError> {
        let _guard = self.lock().map_err(as_read)?;
        let snapshot = self.snapshot().map_err(as_read)?;
        self.verify_events(
            &snapshot,
            snapshot
                .rows
                .get(stream_id.as_str())
                .and_then(|rows| rows.last())
                .and_then(|(_, id, _)| snapshot.history.get_event(id)),
        )
        .map_err(as_read)?;
        Ok(EventCursor {
            stream_id: stream_id.clone(),
            sequence: snapshot
                .rows
                .get(stream_id.as_str())
                .map_or(0, |rows| rows.len() as u64),
        })
    }
    fn lookup_receipt(
        &self,
        stream: &SessionStreamId,
        key: &str,
    ) -> Result<Option<AppendReceipt>, ReadError> {
        let _guard = self.lock().map_err(as_read)?;
        let snapshot = self.snapshot().map_err(as_read)?;
        let receipt = snapshot
            .receipts
            .get(&(stream.to_string(), key.to_string()))
            .map(|(_, receipt)| receipt.clone());
        if let Some(receipt) = &receipt {
            self.verify_events(
                &snapshot,
                receipt
                    .event_ids
                    .iter()
                    .filter_map(|id| snapshot.positions.get(id))
                    .filter_map(|(stream, position, _, _)| {
                        snapshot
                            .rows
                            .get(stream)
                            .and_then(|rows| rows.get(*position))
                    })
                    .filter_map(|(_, id, _)| snapshot.history.get_event(id)),
            )
            .map_err(as_read)?;
        }
        Ok(receipt)
    }

    fn erasure_scope(
        &self,
        targets: &[zeroclaw_api::session_events::StoredPayloadRef],
    ) -> Result<(Vec<SessionStreamId>, Digest), AppendError> {
        let _guard = self.lock()?;
        let snapshot = self.snapshot()?;
        let history: Vec<_> = snapshot
            .rows
            .values()
            .flatten()
            .filter_map(|(_, id, _)| snapshot.history.get_event(id).cloned())
            .collect();
        let mut payloads = Vec::new();
        for meta in snapshot.payloads.values() {
            let body = if zeroclaw_api::session_events::payloads::content_leaf(&meta.codec) {
                None
            } else {
                Some(filesystem::read(&self.payload_path(&meta.payload_id)?).map_err(storage)?)
            };
            payloads.push(zeroclaw_api::session_events::payloads::StoredPayload {
                reference: zeroclaw_api::session_events::StoredPayloadRef {
                    payload_id: meta.payload_id.clone(),
                    codec: meta.codec.clone(),
                    body_digest: meta.digest.clone(),
                    byte_len: meta.byte_len,
                },
                body,
            });
        }
        zeroclaw_api::session_events::payloads::erasure_scope(&history, &payloads, targets)
    }

    fn read_payload_bounded(
        &self,
        payload_id: &PayloadId,
        max_bytes: usize,
    ) -> Result<PayloadRead, ReadError> {
        if max_bytes == 0 || max_bytes > MAX_PAGE_BYTES {
            return Err(ReadError::TooLarge);
        }
        let _guard = self.lock().map_err(as_read)?;
        let snapshot = self.snapshot().map_err(as_read)?;
        let meta = snapshot
            .payloads
            .get(payload_id.as_str())
            .ok_or_else(|| ReadError::PayloadMissing(payload_id.to_string()))?;
        use zeroclaw_api::session_events::payloads::PayloadDisposition;
        match snapshot
            .erased
            .get(payload_id)
            .copied()
            .unwrap_or(PayloadDisposition::Present)
        {
            PayloadDisposition::ErasurePending => return Ok(PayloadRead::ErasurePending),
            PayloadDisposition::Redacted => return Ok(PayloadRead::Redacted),
            PayloadDisposition::Present => {}
        }
        if meta.byte_len > max_bytes as u64 {
            return Err(ReadError::TooLarge);
        }
        let file = filesystem::reader(&self.payload_path(payload_id).map_err(as_read)?).map_err(
            |error| {
                if error.kind() == std::io::ErrorKind::NotFound {
                    ReadError::PayloadMissing(payload_id.to_string())
                } else {
                    ReadError::StorageUnavailable(error.to_string())
                }
            },
        )?;
        let mut body = Vec::new();
        file.take(max_bytes as u64 + 1)
            .read_to_end(&mut body)
            .map_err(|error| ReadError::StorageUnavailable(error.to_string()))?;
        if body.len() > max_bytes {
            return Err(ReadError::TooLarge);
        }
        let record = PayloadRecord {
            payload_id: payload_id.clone(),
            codec: meta.codec.clone(),
            body,
            body_digest: meta.digest.clone(),
            byte_len: meta.byte_len,
        };
        record
            .validate()
            .map_err(|error| ReadError::Corrupt(error.to_string()))?;
        Ok(PayloadRead::Present(record))
    }
}
