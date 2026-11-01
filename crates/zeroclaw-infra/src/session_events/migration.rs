//! Verified installation format transfer. Ordinary append must never be used to
//! transfer committed facts: it would assign new timestamps and commitments.
use super::installation::{FormatPhase, InstallationGuard, NativeBackend};
use super::*;
use serde::{Deserialize, Serialize};
use std::collections::{BTreeMap, BTreeSet};
use zeroclaw_api::session_events::{EventData, StoredPayloadRef};
mod sqlite;

#[derive(Clone, Serialize, Deserialize)]
pub(super) struct NativeBatch {
    pub stream: SessionStreamId,
    pub key: String,
    pub commitment: Digest,
    pub raw: Vec<Vec<u8>>,
}
#[derive(Clone, Serialize, Deserialize)]
pub(super) struct NativePayload {
    pub reference: StoredPayloadRef,
    pub body: Option<Vec<u8>>,
}
#[derive(Clone, Serialize, Deserialize)]
pub(super) struct NativeImage {
    pub batches: Vec<NativeBatch>,
    pub payloads: Vec<NativePayload>,
}
impl NativeImage {
    pub fn digest(&self) -> Result<Digest, AppendError> {
        // Physical commit order may differ across backends. Identity consists
        // of the original per-stream batches, bytes and receipt commitments.
        use sha2::{Digest as _, Sha256};
        // Serialize borrowed fields in canonical key order directly into the
        // hash. A family transfer must not clone every attachment or build a
        // second JSON tree merely to compute its commitment.
        #[derive(Serialize)]
        struct Reference<'a> {
            codec: &'a str,
            payload_id: &'a PayloadId,
        }
        #[derive(Serialize)]
        struct Payload<'a> {
            body: &'a Option<Vec<u8>>,
            reference: Reference<'a>,
        }
        #[derive(Serialize)]
        struct Batch<'a> {
            commitment: &'a Digest,
            key: &'a str,
            raw: &'a Vec<Vec<u8>>,
            stream: &'a SessionStreamId,
        }
        #[derive(Serialize)]
        struct Image<'a> {
            batches: Vec<Batch<'a>>,
            payloads: Vec<Payload<'a>>,
        }
        let mut batches: Vec<_> = self.batches.iter().collect();
        batches.sort_by(|a, b| a.stream.cmp(&b.stream).then_with(|| a.key.cmp(&b.key)));
        let mut payloads: Vec<_> = self.payloads.iter().collect();
        payloads.sort_by(|a, b| a.reference.payload_id.cmp(&b.reference.payload_id));
        let image = Image {
            batches: batches
                .into_iter()
                .map(|b| Batch {
                    commitment: &b.commitment,
                    key: &b.key,
                    raw: &b.raw,
                    stream: &b.stream,
                })
                .collect(),
            payloads: payloads
                .into_iter()
                .map(|p| Payload {
                    body: &p.body,
                    reference: Reference {
                        codec: &p.reference.codec,
                        payload_id: &p.reference.payload_id,
                    },
                })
                .collect(),
        };
        struct HashWriter(Sha256);
        impl std::io::Write for HashWriter {
            fn write(&mut self, bytes: &[u8]) -> std::io::Result<usize> {
                self.0.update(bytes);
                Ok(bytes.len())
            }
            fn flush(&mut self) -> std::io::Result<()> {
                Ok(())
            }
        }
        let mut writer = HashWriter(Sha256::new());
        serde_json::to_writer(&mut writer, &image)
            .map_err(|e| AppendError::Corrupt(e.to_string()))?;
        Ok(Digest::from(format!("{:x}", writer.0.finalize())))
    }

    pub fn events(&self) -> Result<Vec<SessionEvent>, AppendError> {
        self.batches
            .iter()
            .flat_map(|batch| &batch.raw)
            .map(|raw| parse_event(raw).map_err(|error| AppendError::Corrupt(error.to_string())))
            .collect()
    }
    pub fn validate(&self) -> Result<(), AppendError> {
        let mut heads = BTreeMap::new();
        let mut receipts = BTreeSet::new();
        for batch in &self.batches {
            if batch.raw.is_empty() || !receipts.insert((batch.stream.clone(), batch.key.clone())) {
                return Err(AppendError::Corrupt(
                    "empty or duplicate migration receipt".into(),
                ));
            }
            let head = heads.entry(batch.stream.clone()).or_insert(0_u64);
            for raw in &batch.raw {
                let event =
                    parse_event(raw).map_err(|error| AppendError::Corrupt(error.to_string()))?;
                if event.stream_id != batch.stream || event.sequence != *head + 1 {
                    return Err(AppendError::Corrupt("migration event chain differs".into()));
                }
                *head = event.sequence;
            }
        }
        let events = self.events()?;
        let history = zeroclaw_api::session_events::rules::HistoryIndex::from_history(&events)?;
        let mut payload_ids = BTreeSet::new();
        for payload in &self.payloads {
            if !payload_ids.insert(payload.reference.payload_id.clone()) {
                return Err(AppendError::Corrupt("duplicate migration payload".into()));
            }
            if payload.body.is_none()
                && zeroclaw_api::session_events::payloads::disposition(
                    &events,
                    &payload.reference.payload_id,
                ) == zeroclaw_api::session_events::payloads::PayloadDisposition::Present
            {
                return Err(AppendError::Corrupt(
                    "migration live payload missing".into(),
                ));
            }
        }
        zeroclaw_api::session_events::payloads::PayloadIndex::from_index(
            &history,
            &events,
            &self
                .payloads
                .iter()
                .map(
                    |payload| zeroclaw_api::session_events::payloads::StoredPayload {
                        reference: payload.reference.clone(),
                        body: payload.body.clone(),
                    },
                )
                .collect::<Vec<_>>(),
            &|_| Ok(None),
        )?;
        Ok(())
    }
}

// The migration codec keeps opaque event JSON. References keep their explicit
// StoredPayloadRef shape even when the event tag is unknown to this version.
fn raw_payload_refs(
    value: &serde_json::Value,
    out: &mut Vec<StoredPayloadRef>,
) -> Result<(), AppendError> {
    match value {
        serde_json::Value::Object(object) if object.contains_key("payload_id") => {
            out.push(
                serde_json::from_value(value.clone())
                    .map_err(|error| AppendError::Corrupt(error.to_string()))?,
            );
        }
        serde_json::Value::Object(object) => {
            for child in object.values() {
                raw_payload_refs(child, out)?;
            }
        }
        serde_json::Value::Array(array) => {
            for child in array {
                raw_payload_refs(child, out)?;
            }
        }
        _ => {}
    }
    Ok(())
}

// Select ownership using structure only. Callers hydrate selected content
// leaves and validate the complete image before exposing it.
pub(super) fn select_family(
    mut image: NativeImage,
    root: Option<&SessionStreamId>,
) -> Result<NativeImage, AppendError> {
    let events = image.events()?;
    if root.is_some_and(|root| {
        !events.iter().any(|event| {
            event.stream_id == *root
                && event.sequence == 1
                && matches!(event.body.data, EventData::SessionStarted { .. })
        })
    }) {
        return Err(AppendError::Corrupt("migration family root missing".into()));
    }
    let by_id: BTreeMap<_, _> = events
        .iter()
        .map(|event| (event.event_id.clone(), event))
        .collect();
    let mut streams: BTreeSet<_> = root.map_or_else(
        || events.iter().map(|event| event.stream_id.clone()).collect(),
        |root| BTreeSet::from([root.clone()]),
    );
    loop {
        let before = streams.len();
        for event in &events {
            if matches!(event.body.data, EventData::DerivedSessionStarted { .. })
                && event
                    .causation_id
                    .as_ref()
                    .and_then(|id| by_id.get(id))
                    .is_some_and(|parent| streams.contains(&parent.stream_id))
            {
                streams.insert(event.stream_id.clone());
            }
        }
        if streams.len() == before {
            break;
        }
    }
    // Payload rows are shared by the physical backend.  A family transfer
    // must not move a payload that is still referenced by an unrelated family:
    // doing so would make the source family and the target family disagree on
    // the same payload identity.  Require the caller to migrate the complete
    // reference component instead.
    let mut payload_streams: BTreeMap<PayloadId, BTreeSet<SessionStreamId>> = BTreeMap::new();
    for batch in &image.batches {
        for raw in &batch.raw {
            let value = serde_json::from_slice(raw)
                .map_err(|error| AppendError::Corrupt(error.to_string()))?;
            let mut references = Vec::new();
            raw_payload_refs(&value, &mut references)?;
            for reference in references {
                payload_streams
                    .entry(reference.payload_id)
                    .or_default()
                    .insert(batch.stream.clone());
            }
        }
    }
    let rows: BTreeMap<_, _> = image
        .payloads
        .iter()
        .map(|payload| (payload.reference.payload_id.clone(), payload))
        .collect();
    let mut pending: Vec<_> = payload_streams
        .iter()
        .flat_map(|(id, owners)| owners.iter().map(move |owner| (id.clone(), owner.clone())))
        .collect();
    while let Some((id, owner)) = pending.pop() {
        let payload = rows
            .get(&id)
            .ok_or_else(|| AppendError::Corrupt("migration payload reference missing".into()))?;
        let children = zeroclaw_api::session_events::payloads::nested_refs(
            &zeroclaw_api::session_events::payloads::StoredPayload {
                reference: payload.reference.clone(),
                body: payload.body.clone(),
            },
        )?;
        for child in children {
            if payload_streams
                .entry(child.payload_id.clone())
                .or_default()
                .insert(owner.clone())
            {
                pending.push((child.payload_id, owner.clone()));
            }
        }
    }
    if payload_streams.values().any(|owners| {
        owners.iter().any(|stream| streams.contains(stream))
            && owners.iter().any(|stream| !streams.contains(stream))
    }) {
        return Err(AppendError::InvalidReference(
            "migration payload is shared with another family".into(),
        ));
    }
    image
        .batches
        .retain(|batch| streams.contains(&batch.stream));
    // Stable causal ordering for JSONL's incremental reconstruction, including
    // parent -> child -> parent settlement in the same millisecond.
    let event_batches: BTreeMap<_, _> = image
        .batches
        .iter()
        .enumerate()
        .flat_map(|(index, batch)| batch.raw.iter().map(move |raw| (index, raw)))
        .map(|(index, raw)| {
            parse_event(raw)
                .map(|event| (event.event_id.to_string(), index))
                .map_err(|error| AppendError::Corrupt(error.to_string()))
        })
        .collect::<Result<_, _>>()?;
    fn strings(value: &serde_json::Value, out: &mut BTreeSet<String>) {
        match value {
            serde_json::Value::String(text) => {
                out.insert(text.clone());
            }
            serde_json::Value::Array(items) => {
                for item in items {
                    strings(item, out);
                }
            }
            serde_json::Value::Object(items) => {
                for item in items.values() {
                    strings(item, out);
                }
            }
            _ => {}
        }
    }
    let mut dependencies = vec![BTreeSet::new(); image.batches.len()];
    let mut stream_batches: BTreeMap<SessionStreamId, Vec<(u64, usize)>> = BTreeMap::new();
    for (index, batch) in image.batches.iter().enumerate() {
        let first = batch
            .raw
            .first()
            .ok_or_else(|| AppendError::Corrupt("migration receipt is empty".into()))?;
        let event = parse_event(first).map_err(|error| AppendError::Corrupt(error.to_string()))?;
        stream_batches
            .entry(batch.stream.clone())
            .or_default()
            .push((event.sequence, index));
        let mut references = BTreeSet::new();
        for raw in &batch.raw {
            strings(
                &serde_json::from_slice(raw)
                    .map_err(|error| AppendError::Corrupt(error.to_string()))?,
                &mut references,
            );
        }
        for reference in references {
            if let Some(prior) = event_batches.get(&reference)
                && *prior != index
            {
                dependencies[index].insert(*prior);
            }
        }
    }
    for batches in stream_batches.values_mut() {
        batches.sort();
        for pair in batches.windows(2) {
            dependencies[pair[1].1].insert(pair[0].1);
        }
    }
    let mut successors = vec![Vec::new(); image.batches.len()];
    let mut indegrees: Vec<_> = dependencies.iter().map(BTreeSet::len).collect();
    for (index, dependencies) in dependencies.iter().enumerate() {
        for dependency in dependencies {
            successors[*dependency].push(index);
        }
    }
    let mut ready: BTreeSet<_> = indegrees
        .iter()
        .enumerate()
        .filter_map(|(index, count)| (*count == 0).then_some(index))
        .collect();
    let mut remaining: Vec<_> = image.batches.drain(..).map(Some).collect();
    while let Some(next) = ready.pop_first() {
        image.batches.push(
            remaining[next]
                .take()
                .ok_or_else(|| AppendError::Corrupt("migration batch missing".into()))?,
        );
        for successor in &successors[next] {
            indegrees[*successor] -= 1;
            if indegrees[*successor] == 0 {
                ready.insert(*successor);
            }
        }
    }
    if image.batches.len() != remaining.len() {
        return Err(AppendError::Corrupt(
            "cyclic migration batch references".into(),
        ));
    }
    let events = image.events()?;
    // References to another live family cannot silently acquire ownership in
    // this migration. All child lineage remains inside the frozen family.
    for event in &events {
        if event
            .causation_id
            .as_ref()
            .and_then(|id| by_id.get(id))
            .is_some_and(|cause| !streams.contains(&cause.stream_id))
        {
            return Err(AppendError::InvalidReference(
                "migration references another family".into(),
            ));
        }
    }
    let payloads: BTreeMap<_, _> = image
        .payloads
        .into_iter()
        .map(|payload| (payload.reference.payload_id.clone(), payload))
        .collect();
    let mut pending = Vec::new();
    for batch in &image.batches {
        for raw in &batch.raw {
            raw_payload_refs(
                &serde_json::from_slice(raw)
                    .map_err(|error| AppendError::Corrupt(error.to_string()))?,
                &mut pending,
            )?;
        }
    }
    let mut selected = BTreeMap::new();
    while let Some(reference) = pending.pop() {
        if selected.contains_key(&reference.payload_id) {
            continue;
        }
        let payload = payloads
            .get(&reference.payload_id)
            .ok_or_else(|| AppendError::Corrupt("migration reference missing".into()))?;
        if payload.reference != reference {
            return Err(AppendError::Corrupt("migration reference mismatch".into()));
        }
        if let Some(body) = &payload.body
            && !zeroclaw_api::session_events::payloads::content_leaf(&reference.codec)
            && let Ok(value) = serde_json::from_slice(body)
        {
            raw_payload_refs(&value, &mut pending)?;
        }
        selected.insert(reference.payload_id, payload.clone());
    }
    image.payloads = selected.into_values().collect();
    Ok(image)
}

pub(super) fn export(
    data_dir: &Path,
    backend: NativeBackend,
    root: Option<&SessionStreamId>,
) -> Result<NativeImage, AppendError> {
    match backend {
        NativeBackend::Sqlite => SqliteEventStore::open(data_dir)?.export_native(root),
        NativeBackend::Jsonl => JsonlEventStore::open(data_dir)?.export_native(root),
    }
}

/// Must run after the host drains all coordinators and escaped observations.
/// Exclusive family leases prove that no other executor remains active.
/// Interrupted transfer resumes from the durable format manifest; writers stay
/// closed until the exact source image is verified in the target.
pub fn migrate_installation(data_dir: &Path, target: NativeBackend) -> Result<(), AppendError> {
    let mut installation = InstallationGuard::acquire(&data_dir.join("sessions"))?;
    migrate_locked(data_dir, &mut installation, target)
}

pub(super) fn migrate_locked(
    data_dir: &Path,
    installation: &mut InstallationGuard,
    target: NativeBackend,
) -> Result<(), AppendError> {
    let (source, expected) = match installation.phase().cloned() {
        Some(FormatPhase::Ready { backend }) if backend == target => return Ok(()),
        Some(FormatPhase::Ready { backend }) => (backend, None),
        Some(FormatPhase::Switching {
            source,
            target: chosen,
            image_digest,
        }) if chosen == target => (source, Some(image_digest)),
        _ => {
            return Err(AppendError::InvalidTransition(
                "installation has no matching native migration source".into(),
            ));
        }
    };
    let source_path = super::upgrade::native_path(data_dir, source);
    if !source_path.try_exists().map_err(jsonl::storage)? {
        let expected = expected
            .ok_or_else(|| AppendError::Corrupt("native migration source missing".into()))?;
        let mut backup = source_path.as_os_str().to_os_string();
        backup.push(format!(".backup-{}", installation.generation()));
        if !Path::new(&backup).try_exists().map_err(jsonl::storage)?
            || export(data_dir, target, None)?.digest()? != expected
        {
            return Err(AppendError::Corrupt(
                "archived source or verified target missing".into(),
            ));
        }
        super::upgrade::archive_native(data_dir, source, installation.generation())?;
        return installation.finish(target);
    }
    let image = export(data_dir, source, None)?;
    let families = installation::families(data_dir, source)?;
    let _leases = families
        .keys()
        .map(|family| ownership::FamilyExecutionLease::acquire(data_dir, family))
        .collect::<Result<Vec<_>, _>>()?;
    let digest = image.digest()?;
    if let Some(expected) = expected {
        if expected != digest {
            return Err(AppendError::Corrupt(
                "frozen migration source changed".into(),
            ));
        }
    } else {
        installation.begin(FormatPhase::Switching {
            source,
            target,
            image_digest: digest.clone(),
        })?;
    }
    #[cfg(test)]
    super::crash_tests::crash_boundary("native_migration_frozen");
    match target {
        NativeBackend::Sqlite => SqliteEventStore::open(data_dir)?.import_native(&image)?,
        NativeBackend::Jsonl => JsonlEventStore::open(data_dir)?.import_native(&image)?,
    }
    #[cfg(test)]
    super::crash_tests::crash_boundary("native_migration_imported");
    let verified = export(data_dir, target, None)?.digest()?;
    if verified != digest {
        return Err(AppendError::Corrupt(
            "migration target differs from frozen source".into(),
        ));
    }
    // The source becomes a backup before publishing the new active format.
    // A retry after rename verifies the target against the frozen commitment.
    super::upgrade::archive_native(data_dir, source, installation.generation())?;
    #[cfg(test)]
    super::crash_tests::crash_boundary("native_migration_archived");
    installation.finish(target)?;
    #[cfg(test)]
    super::crash_tests::crash_boundary("native_migration_published");
    Ok(())
}

#[cfg(test)]
mod tests;
