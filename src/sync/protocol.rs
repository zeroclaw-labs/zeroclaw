//! Sync protocol message types and order buffer.
//!
//! Defines the WebSocket broadcast message protocol for multi-device
//! memory synchronization. Implements sequence ordering guarantees
//! per the patent's Layer 2 specification.

use crate::memory::sync::{DeltaEntry, VersionVector};
use serde::{Deserialize, Serialize};
use std::collections::{BTreeMap, HashSet};

/// Maximum entries in the order buffer before forcing a flush.
const ORDER_BUFFER_MAX: usize = 1000;

// ── Broadcast Message Types ─────────────────────────────────────

/// All message types sent over the WebSocket broadcast channel.
/// The server never stores message contents in a database — pure relay.
#[derive(Debug, Clone, Serialize, Deserialize)]
#[serde(tag = "type")]
pub enum BroadcastMessage {
    // ── Layer 1: Real-time relay notification ────────────────
    /// Notify peers that new data was uploaded to the temporary relay.
    #[serde(rename = "relay_notify")]
    RelayNotify {
        from_device_id: String,
        /// Relay entry IDs that were uploaded.
        relay_ids: Vec<String>,
    },

    // ── Layer 2: Delta journal sync ─────────────────────────
    /// Request missing deltas from peers after coming online.
    #[serde(rename = "sync_request")]
    SyncRequest {
        from_device_id: String,
        version_vector: VersionVector,
    },

    /// Response with missing deltas (may be batched).
    #[serde(rename = "sync_response")]
    SyncResponse {
        from_device_id: String,
        deltas: Vec<DeltaEntry>,
        /// True if more batches follow.
        has_more: bool,
    },

    /// Acknowledge receipt of deltas (update peer's view of our state).
    #[serde(rename = "delta_ack")]
    DeltaAck {
        from_device_id: String,
        /// The source device whose deltas we received.
        source_device_id: String,
        /// Last sequence we received from that device.
        last_seq: u64,
    },

    // ── Layer 3: Full sync (manifest-based) ─────────────────
    /// Request full synchronization (user-initiated).
    #[serde(rename = "full_sync_request")]
    FullSyncRequest {
        from_device_id: String,
        manifest: FullSyncManifest,
    },

    /// Response with peer's manifest for comparison.
    #[serde(rename = "full_sync_manifest_response")]
    FullSyncManifestResponse {
        from_device_id: String,
        manifest: FullSyncManifest,
    },

    /// Transfer a single entity during full sync.
    #[serde(rename = "full_sync_data")]
    FullSyncData {
        from_device_id: String,
        entity_type: String,
        entity_id: String,
        /// Encrypted payload (AES-256-GCM or ChaCha20-Poly1305).
        encrypted_payload: String,
        iv: String,
        auth_tag: String,
    },

    /// Signal full sync transfer completion.
    #[serde(rename = "full_sync_complete")]
    FullSyncComplete {
        from_device_id: String,
        sent_count: u64,
    },
}

// ── Full Sync Manifest ──────────────────────────────────────────

/// Manifest of all entities a device holds, used for set-difference
/// comparison during Layer 3 full sync.
#[derive(Debug, Clone, Default, Serialize, Deserialize)]
pub struct FullSyncManifest {
    /// Long-term memory chunk IDs.
    pub memory_chunk_ids: HashSet<String>,
    /// Conversation session IDs.
    pub conversation_ids: HashSet<String>,
    /// Settings keys.
    pub setting_keys: HashSet<String>,
    /// Timestamp when this manifest was generated.
    pub generated_at: u64,
}

impl FullSyncManifest {
    /// Compute which entity IDs exist in `self` but not in `other`.
    pub fn missing_from(&self, other: &FullSyncManifest) -> FullSyncManifest {
        FullSyncManifest {
            memory_chunk_ids: self
                .memory_chunk_ids
                .difference(&other.memory_chunk_ids)
                .cloned()
                .collect(),
            conversation_ids: self
                .conversation_ids
                .difference(&other.conversation_ids)
                .cloned()
                .collect(),
            setting_keys: self
                .setting_keys
                .difference(&other.setting_keys)
                .cloned()
                .collect(),
            generated_at: self.generated_at,
        }
    }

    /// Total number of entities in this manifest.
    pub fn total_count(&self) -> usize {
        self.memory_chunk_ids.len() + self.conversation_ids.len() + self.setting_keys.len()
    }
}

// ── Order Buffer (Layer 2 Sequence Guarantee) ───────────────────

/// Ensures deltas from a single source device are applied in sequence order.
///
/// When deltas arrive out of order (e.g., seq 3 before seq 2), the buffer
/// holds later sequences until the gap is filled. Implements the patent's
/// "순서 대기 버퍼" (order waiting buffer).
pub struct OrderBuffer {
    /// Per-device buffer: device_id -> (seq -> delta).
    buffers: BTreeMap<String, BTreeMap<u64, DeltaEntry>>,
    /// Per-device expected next sequence: device_id -> next_expected_seq.
    expected_seq: BTreeMap<String, u64>,
}

impl OrderBuffer {
    pub fn new() -> Self {
        Self {
            buffers: BTreeMap::new(),
            expected_seq: BTreeMap::new(),
        }
    }

    /// Initialize expected sequence for a device from version vector.
    pub fn init_from_version_vector(&mut self, vv: &VersionVector) {
        for (device_id, &clock) in &vv.clocks {
            self.expected_seq.insert(device_id.clone(), clock + 1);
        }
    }

    /// Insert a delta into the buffer. Returns deltas that can be applied
    /// in order (a contiguous run starting from the expected sequence).
    ///
    /// ## Returns
    /// - `Vec<DeltaEntry>` — deltas ready to apply (in order)
    /// - Empty vec if the delta was buffered (gap exists) or was a duplicate
    pub fn insert(&mut self, delta: DeltaEntry) -> Vec<DeltaEntry> {
        let device_id = delta.device_id.clone();
        let seq = delta.version.get(&device_id);

        let expected = self.expected_seq.get(&device_id).copied().unwrap_or(1);

        // Duplicate / already-seen — discard (idempotency)
        if seq < expected {
            return Vec::new();
        }

        // Insert into per-device buffer
        let device_buf = self.buffers.entry(device_id.clone()).or_default();
        device_buf.insert(seq, delta);

        // Enforce buffer size limit
        if device_buf.len() > ORDER_BUFFER_MAX {
            // Drop oldest entries beyond limit
            while device_buf.len() > ORDER_BUFFER_MAX {
                device_buf.pop_first();
            }
        }

        // Flush contiguous run starting from expected
        self.flush(&device_id)
    }

    /// Flush all contiguous deltas starting from the expected sequence
    /// for the given device.
    fn flush(&mut self, device_id: &str) -> Vec<DeltaEntry> {
        let mut ready = Vec::new();
        let expected = self.expected_seq.get(device_id).copied().unwrap_or(1);

        let device_buf = match self.buffers.get_mut(device_id) {
            Some(buf) => buf,
            None => return ready,
        };

        let mut next = expected;
        while let Some(delta) = device_buf.remove(&next) {
            ready.push(delta);
            next += 1;
        }

        if next > expected {
            self.expected_seq.insert(device_id.to_string(), next);
        }

        ready
    }

    /// Check if there are gaps (buffered but not yet flushable deltas)
    /// for any device.
    pub fn has_gaps(&self) -> bool {
        for (device_id, buf) in &self.buffers {
            if buf.is_empty() {
                continue;
            }
            let expected = self.expected_seq.get(device_id).copied().unwrap_or(1);
            let first_buffered = buf.keys().next().copied().unwrap_or(0);
            if first_buffered > expected {
                return true;
            }
        }
        false
    }

    /// Get the set of missing sequences (gaps) for a given device.
    pub fn missing_sequences(&self, device_id: &str) -> Vec<u64> {
        let expected = self.expected_seq.get(device_id).copied().unwrap_or(1);

        let buf = match self.buffers.get(device_id) {
            Some(b) => b,
            None => return Vec::new(),
        };

        let max_buffered = buf.keys().next_back().copied().unwrap_or(expected);
        let mut missing = Vec::new();
        for seq in expected..max_buffered {
            if !buf.contains_key(&seq) {
                missing.push(seq);
            }
        }
        missing
    }
}

// ── Last-Writer-Wins Conflict Resolution ─────────────────────────

/// Resolve a concurrent write conflict using Last-Writer-Wins (LWW).
///
/// When two deltas are causally concurrent (neither version vector dominates
/// the other), we break the tie deterministically:
///   1. **Higher timestamp wins** (wall-clock, seconds).
///   2. **On timestamp tie**: lexicographically greater `device_id` wins.
///
/// This ensures every device converges to the same state without coordination.
///
/// Returns `true` if `incoming` should win (overwrite local).
pub fn lww_resolve(local: &DeltaEntry, incoming: &DeltaEntry) -> bool {
    if incoming.timestamp != local.timestamp {
        return incoming.timestamp > local.timestamp;
    }
    // Deterministic tiebreaker: higher device_id wins
    incoming.device_id > local.device_id
}

/// Merge two sets of deltas, resolving conflicts via LWW for concurrent writes
/// to the same key.
///
/// Returns the winning delta for each key.
pub fn merge_deltas_lww(
    local_deltas: &[DeltaEntry],
    incoming_deltas: &[DeltaEntry],
) -> Vec<DeltaEntry> {
    use std::collections::HashMap;

    // Index by key → best delta
    let mut winners: HashMap<String, DeltaEntry> = HashMap::new();

    let mut insert = |delta: &DeltaEntry| {
        let key = match &delta.operation {
            crate::memory::sync::DeltaOperation::Store { key, .. }
            | crate::memory::sync::DeltaOperation::Forget { key } => key.clone(),
            // Ontology deltas use their delta ID as the dedup key.
            crate::memory::sync::DeltaOperation::OntologyObjectUpsert { object_id, .. } => {
                format!("onto_obj_{object_id}")
            }
            crate::memory::sync::DeltaOperation::OntologyLinkCreate {
                from_object_id,
                to_object_id,
                link_type_name,
                ..
            } => format!("onto_link_{link_type_name}_{from_object_id}_{to_object_id}"),
            crate::memory::sync::DeltaOperation::OntologyActionLog { .. } => delta.id.clone(),
        };

        match winners.get(&key) {
            Some(existing) => {
                if lww_resolve(existing, delta) {
                    winners.insert(key, delta.clone());
                }
            }
            None => {
                winners.insert(key, delta.clone());
            }
        }
    };

    for d in local_deltas {
        insert(d);
    }
    for d in incoming_deltas {
        insert(d);
    }

    let mut result: Vec<DeltaEntry> = winners.into_values().collect();
    result.sort_by(|a, b| a.timestamp.cmp(&b.timestamp));
    result
}

// ── Full Sync Protocol Handler ──────────────────────────────────

/// Result of processing an incoming full sync manifest comparison.
pub struct FullSyncPlan {
    /// Keys that the remote has and we are missing.
    pub we_need: FullSyncManifest,
    /// Keys that we have and the remote is missing.
    pub they_need: FullSyncManifest,
}

impl FullSyncPlan {
    /// Compute the bidirectional sync plan from local and remote manifests.
    pub fn compute(local: &FullSyncManifest, remote: &FullSyncManifest) -> Self {
        Self {
            we_need: remote.missing_from(local),
            they_need: local.missing_from(remote),
        }
    }

    /// True if there is nothing to sync in either direction.
    pub fn is_empty(&self) -> bool {
        self.we_need.total_count() == 0 && self.they_need.total_count() == 0
    }
}

/// Generate batched `BroadcastMessage::FullSyncData` messages for entities
/// the remote peer is missing.
pub fn build_full_sync_data_messages<S: std::hash::BuildHasher>(
    from_device_id: &str,
    missing_keys: &HashSet<String, S>,
    entries: &[(String, String, String, String)], // (key, entity_type, encrypted_payload, iv)
) -> Vec<BroadcastMessage> {
    let mut messages = Vec::new();

    for (key, entity_type, encrypted_payload, iv) in entries {
        if missing_keys.contains(key) {
            messages.push(BroadcastMessage::FullSyncData {
                from_device_id: from_device_id.to_string(),
                entity_type: entity_type.clone(),
                entity_id: key.clone(),
                encrypted_payload: encrypted_payload.clone(),
                iv: iv.clone(),
                auth_tag: String::new(),
            });
        }
    }

    messages
}

/// Build the completion message for a full sync transfer.
pub fn build_full_sync_complete(from_device_id: &str, sent_count: u64) -> BroadcastMessage {
    BroadcastMessage::FullSyncComplete {
        from_device_id: from_device_id.to_string(),
        sent_count,
    }
}

// ── Tests ───────────────────────────────────────────────────────

#[cfg(test)]
mod tests {
    use super::*;
    use crate::memory::sync::DeltaOperation;

    fn make_delta(device_id: &str, seq: u64) -> DeltaEntry {
        let mut vv = VersionVector::default();
        for _ in 0..seq {
            vv.increment(device_id);
        }
        DeltaEntry {
            id: format!("{device_id}-{seq}"),
            device_id: device_id.to_string(),
            version: vv,
            operation: DeltaOperation::Store {
                key: format!("key_{seq}"),
                content: format!("value_{seq}"),
                category: "general".into(),
            },
            timestamp: 1000 + seq,
        }
    }

    #[test]
    fn order_buffer_in_order_delivery() {
        let mut buf = OrderBuffer::new();

        let ready = buf.insert(make_delta("dev_a", 1));
        assert_eq!(ready.len(), 1);

        let ready = buf.insert(make_delta("dev_a", 2));
        assert_eq!(ready.len(), 1);

        let ready = buf.insert(make_delta("dev_a", 3));
        assert_eq!(ready.len(), 1);
    }

    #[test]
    fn order_buffer_out_of_order_delivery() {
        let mut buf = OrderBuffer::new();

        // Receive seq 1 — immediate
        let ready = buf.insert(make_delta("dev_a", 1));
        assert_eq!(ready.len(), 1);

        // Receive seq 3 — buffered (gap at seq 2)
        let ready = buf.insert(make_delta("dev_a", 3));
        assert_eq!(ready.len(), 0);
        assert!(buf.has_gaps());

        // Receive seq 2 — fills gap, flushes seq 2 + seq 3
        let ready = buf.insert(make_delta("dev_a", 2));
        assert_eq!(ready.len(), 2);
        assert_eq!(ready[0].id, "dev_a-2");
        assert_eq!(ready[1].id, "dev_a-3");
        assert!(!buf.has_gaps());
    }

    #[test]
    fn order_buffer_duplicate_ignored() {
        let mut buf = OrderBuffer::new();

        let ready = buf.insert(make_delta("dev_a", 1));
        assert_eq!(ready.len(), 1);

        // Duplicate seq 1 — ignored
        let ready = buf.insert(make_delta("dev_a", 1));
        assert_eq!(ready.len(), 0);
    }

    #[test]
    fn order_buffer_multiple_devices() {
        let mut buf = OrderBuffer::new();

        // Device A seq 1
        let ready = buf.insert(make_delta("dev_a", 1));
        assert_eq!(ready.len(), 1);

        // Device B seq 1
        let ready = buf.insert(make_delta("dev_b", 1));
        assert_eq!(ready.len(), 1);

        // Device A seq 3 (gap)
        let ready = buf.insert(make_delta("dev_a", 3));
        assert_eq!(ready.len(), 0);

        // Device B seq 2 (no gap for B)
        let ready = buf.insert(make_delta("dev_b", 2));
        assert_eq!(ready.len(), 1);

        // Device A seq 2 — fills gap
        let ready = buf.insert(make_delta("dev_a", 2));
        assert_eq!(ready.len(), 2);
    }

    #[test]
    fn order_buffer_init_from_version_vector() {
        let mut vv = VersionVector::default();
        for _ in 0..5 {
            vv.increment("dev_a");
        }

        let mut buf = OrderBuffer::new();
        buf.init_from_version_vector(&vv);

        // seq 5 already seen, seq 6 is expected next
        let ready = buf.insert(make_delta("dev_a", 5));
        assert_eq!(ready.len(), 0); // duplicate

        let ready = buf.insert(make_delta("dev_a", 6));
        assert_eq!(ready.len(), 1);
    }

    #[test]
    fn order_buffer_missing_sequences() {
        let mut buf = OrderBuffer::new();

        buf.insert(make_delta("dev_a", 1));
        buf.insert(make_delta("dev_a", 4)); // gap at 2, 3
        buf.insert(make_delta("dev_a", 6)); // gap at 5 too

        let missing = buf.missing_sequences("dev_a");
        assert_eq!(missing, vec![2, 3, 5]); // 4 and 6 are buffered; 2, 3, 5 are gaps
    }

    #[test]
    fn manifest_missing_from() {
        let mut m1 = FullSyncManifest::default();
        m1.memory_chunk_ids.insert("m1".into());
        m1.memory_chunk_ids.insert("m2".into());
        m1.memory_chunk_ids.insert("m3".into());

        let mut m2 = FullSyncManifest::default();
        m2.memory_chunk_ids.insert("m1".into());
        m2.memory_chunk_ids.insert("m4".into());
        m2.memory_chunk_ids.insert("m5".into());

        // What m1 has that m2 doesn't
        let diff = m1.missing_from(&m2);
        assert_eq!(diff.memory_chunk_ids.len(), 2);
        assert!(diff.memory_chunk_ids.contains("m2"));
        assert!(diff.memory_chunk_ids.contains("m3"));

        // What m2 has that m1 doesn't
        let diff2 = m2.missing_from(&m1);
        assert_eq!(diff2.memory_chunk_ids.len(), 2);
        assert!(diff2.memory_chunk_ids.contains("m4"));
        assert!(diff2.memory_chunk_ids.contains("m5"));
    }

    #[test]
    fn manifest_total_count() {
        let mut m = FullSyncManifest::default();
        m.memory_chunk_ids.insert("m1".into());
        m.conversation_ids.insert("c1".into());
        m.conversation_ids.insert("c2".into());
        m.setting_keys.insert("s1".into());

        assert_eq!(m.total_count(), 4);
    }

    // ── LWW Conflict Resolution Tests ─────────────────────────

    #[test]
    fn lww_higher_timestamp_wins() {
        let local = DeltaEntry {
            id: "d1".into(),
            device_id: "dev_a".into(),
            version: VersionVector::default(),
            operation: DeltaOperation::Store {
                key: "k".into(),
                content: "old".into(),
                category: "core".into(),
            },
            timestamp: 1000,
        };
        let incoming = DeltaEntry {
            id: "d2".into(),
            device_id: "dev_b".into(),
            version: VersionVector::default(),
            operation: DeltaOperation::Store {
                key: "k".into(),
                content: "new".into(),
                category: "core".into(),
            },
            timestamp: 2000,
        };

        assert!(super::lww_resolve(&local, &incoming)); // incoming wins (higher ts)
        assert!(!super::lww_resolve(&incoming, &local)); // local is older
    }

    #[test]
    fn lww_same_timestamp_higher_device_id_wins() {
        let local = DeltaEntry {
            id: "d1".into(),
            device_id: "aaa".into(),
            version: VersionVector::default(),
            operation: DeltaOperation::Store {
                key: "k".into(),
                content: "from_aaa".into(),
                category: "core".into(),
            },
            timestamp: 1000,
        };
        let incoming = DeltaEntry {
            id: "d2".into(),
            device_id: "zzz".into(),
            version: VersionVector::default(),
            operation: DeltaOperation::Store {
                key: "k".into(),
                content: "from_zzz".into(),
                category: "core".into(),
            },
            timestamp: 1000,
        };

        assert!(super::lww_resolve(&local, &incoming)); // zzz > aaa
        assert!(!super::lww_resolve(&incoming, &local)); // aaa < zzz
    }

    #[test]
    fn merge_deltas_lww_picks_winner_per_key() {
        let local_deltas = vec![DeltaEntry {
            id: "d1".into(),
            device_id: "dev_a".into(),
            version: VersionVector::default(),
            operation: DeltaOperation::Store {
                key: "shared".into(),
                content: "from_a".into(),
                category: "core".into(),
            },
            timestamp: 1000,
        }];

        let incoming_deltas = vec![DeltaEntry {
            id: "d2".into(),
            device_id: "dev_b".into(),
            version: VersionVector::default(),
            operation: DeltaOperation::Store {
                key: "shared".into(),
                content: "from_b".into(),
                category: "core".into(),
            },
            timestamp: 2000, // newer
        }];

        let result = super::merge_deltas_lww(&local_deltas, &incoming_deltas);
        assert_eq!(result.len(), 1);
        assert!(matches!(
            &result[0].operation,
            DeltaOperation::Store { content, .. } if content == "from_b"
        ));
    }

    #[test]
    fn merge_deltas_lww_keeps_distinct_keys() {
        let local_deltas = vec![DeltaEntry {
            id: "d1".into(),
            device_id: "dev_a".into(),
            version: VersionVector::default(),
            operation: DeltaOperation::Store {
                key: "key_a".into(),
                content: "val_a".into(),
                category: "core".into(),
            },
            timestamp: 1000,
        }];

        let incoming_deltas = vec![DeltaEntry {
            id: "d2".into(),
            device_id: "dev_b".into(),
            version: VersionVector::default(),
            operation: DeltaOperation::Store {
                key: "key_b".into(),
                content: "val_b".into(),
                category: "core".into(),
            },
            timestamp: 2000,
        }];

        let result = super::merge_deltas_lww(&local_deltas, &incoming_deltas);
        assert_eq!(result.len(), 2);
    }

    // ── Full Sync Plan Tests ─────────────────────────────────────

    #[test]
    fn full_sync_plan_compute_bidirectional() {
        let mut local = FullSyncManifest::default();
        local.memory_chunk_ids.insert("m1".into());
        local.memory_chunk_ids.insert("m2".into());

        let mut remote = FullSyncManifest::default();
        remote.memory_chunk_ids.insert("m2".into());
        remote.memory_chunk_ids.insert("m3".into());

        let plan = super::FullSyncPlan::compute(&local, &remote);

        // We need m3 (remote has it, we don't)
        assert!(plan.we_need.memory_chunk_ids.contains("m3"));
        assert_eq!(plan.we_need.memory_chunk_ids.len(), 1);

        // They need m1 (we have it, they don't)
        assert!(plan.they_need.memory_chunk_ids.contains("m1"));
        assert_eq!(plan.they_need.memory_chunk_ids.len(), 1);
    }

    #[test]
    fn full_sync_plan_empty_when_identical() {
        let mut local = FullSyncManifest::default();
        local.memory_chunk_ids.insert("m1".into());

        let remote = local.clone();

        let plan = super::FullSyncPlan::compute(&local, &remote);
        assert!(plan.is_empty());
    }

    #[test]
    fn build_full_sync_data_messages_filters_by_missing() {
        let mut missing = HashSet::new();
        missing.insert("k1".to_string());
        missing.insert("k3".to_string());

        let entries = vec![
            ("k1".into(), "memory".into(), "enc1".into(), "iv1".into()),
            ("k2".into(), "memory".into(), "enc2".into(), "iv2".into()),
            ("k3".into(), "memory".into(), "enc3".into(), "iv3".into()),
        ];

        let msgs = super::build_full_sync_data_messages("dev_a", &missing, &entries);
        assert_eq!(msgs.len(), 2);
    }

    #[test]
    fn broadcast_message_serialization_roundtrip() {
        let msg = BroadcastMessage::SyncRequest {
            from_device_id: "dev_a".into(),
            version_vector: VersionVector::default(),
        };

        let json = serde_json::to_string(&msg).unwrap();
        let parsed: BroadcastMessage = serde_json::from_str(&json).unwrap();

        match parsed {
            BroadcastMessage::SyncRequest {
                from_device_id,
                version_vector,
            } => {
                assert_eq!(from_device_id, "dev_a");
                assert!(version_vector.clocks.is_empty());
            }
            _ => panic!("Wrong message type"),
        }
    }
}
