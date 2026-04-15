//! `SyncedMemory` — decorator that wires [`SyncEngine`] into any [`Memory`] backend.
//!
//! Every `store()` and `forget()` call is transparently recorded in the
//! delta journal so that changes replicate to peer devices.  Incoming
//! deltas from peers are applied through [`apply_remote_deltas`].
//!
//! Read-only operations (`recall`, `get`, `list`, `count`, `health_check`)
//! pass through to the inner backend without sync overhead.

use crate::memory::sync::{DeltaEntry, DeltaOperation, SyncEngine, SyncPayload, VersionVector};
use crate::memory::traits::{Memory, MemoryCategory, MemoryEntry};
use crate::ontology::OntologyRepo;
use async_trait::async_trait;
use parking_lot::Mutex;
use std::sync::Arc;

/// A memory backend wrapper that records every mutation in a [`SyncEngine`]
/// for cross-device replication.
pub struct SyncedMemory {
    /// The actual persistence backend (sqlite, lucid, markdown, …).
    inner: Arc<dyn Memory>,
    /// Shared, mutex-protected sync engine.
    sync: Arc<Mutex<SyncEngine>>,
}

impl SyncedMemory {
    /// Wrap an existing memory backend with sync support.
    pub fn new(inner: Arc<dyn Memory>, sync: Arc<Mutex<SyncEngine>>) -> Self {
        Self { inner, sync }
    }

    /// Get a reference to the shared sync engine (for gateway / protocol use).
    pub fn sync_engine(&self) -> &Arc<Mutex<SyncEngine>> {
        &self.sync
    }

    /// Get a reference to the inner memory backend.
    pub fn inner(&self) -> &Arc<dyn Memory> {
        &self.inner
    }

    /// Apply delta operations received from a remote device to the local
    /// memory backend and ontology repo.  Returns the number of operations
    /// successfully applied.
    ///
    /// This is the inbound path: peer sends deltas → we apply them locally.
    /// If `ontology` is provided, ontology deltas (object upsert, link create,
    /// action log) are also applied — enabling cross-device knowledge graph sync.
    pub async fn apply_remote_deltas(
        &self,
        deltas: Vec<DeltaEntry>,
        ontology: Option<&OntologyRepo>,
    ) -> usize {
        // Let the sync engine filter duplicates / already-seen entries.
        let ops = {
            let mut engine = self.sync.lock();
            engine.apply_deltas(deltas)
        };

        let mut applied = 0;
        for op in &ops {
            match op {
                DeltaOperation::Store {
                    key,
                    content,
                    category,
                } => {
                    let cat = category_from_str(category);
                    if let Err(e) = self.inner.store(key, content, cat, None).await {
                        tracing::warn!(key, "Failed to apply remote store delta: {e}");
                        continue;
                    }
                    tracing::debug!(key, "Applied remote store delta");
                    applied += 1;
                }
                DeltaOperation::Forget { key } => match self.inner.forget(key).await {
                    Ok(_) => {
                        tracing::debug!(key, "Applied remote forget delta");
                        applied += 1;
                    }
                    Err(e) => {
                        tracing::warn!(key, "Failed to apply remote forget delta: {e}");
                    }
                },
                // ── Ontology sync: apply object/link/action deltas ──
                DeltaOperation::OntologyObjectUpsert {
                    type_name,
                    title,
                    properties_json,
                    owner_user_id,
                    ..
                } => {
                    if let Some(repo) = ontology {
                        let props: serde_json::Value =
                            serde_json::from_str(properties_json).unwrap_or_default();
                        let title_str = title.as_deref().unwrap_or("(untitled)");
                        match repo.ensure_object(
                            type_name,
                            title_str,
                            &props,
                            owner_user_id,
                        ) {
                            Ok(_) => {
                                tracing::debug!(type_name, "Applied remote ontology object");
                                applied += 1;
                            }
                            Err(e) => tracing::warn!("Failed to apply ontology object: {e}"),
                        }
                    }
                }
                DeltaOperation::OntologyLinkCreate {
                    link_type_name,
                    from_object_id,
                    to_object_id,
                    properties_json,
                } => {
                    if let Some(repo) = ontology {
                        let props = properties_json
                            .as_ref()
                            .and_then(|s| serde_json::from_str(s).ok());
                        match repo.create_link(
                            link_type_name,
                            *from_object_id,
                            *to_object_id,
                            props.as_ref(),
                        ) {
                            Ok(_) => {
                                tracing::debug!(link_type_name, "Applied remote ontology link");
                                applied += 1;
                            }
                            Err(e) => tracing::warn!("Failed to apply ontology link: {e}"),
                        }
                    }
                }
                DeltaOperation::OntologyActionLog { .. } => {
                    // Action logs are read-only replications — we don't replay
                    // actions on remote devices, just acknowledge them.
                    tracing::debug!("Received remote ontology action log (read-only)");
                }
                // ── v3.0 Timeline / Phone / Truth deltas ──────────────
                // Delegate to the backend's typed apply hook. SqliteMemory
                // persists into local tables (idempotent via UUID/LWW) and
                // does NOT re-record the delta (no replication loop).
                DeltaOperation::TimelineAppend { uuid, .. } => {
                    match self.inner.apply_remote_v3_delta(op).await {
                        Ok(true) => {
                            tracing::debug!(uuid, "Applied remote timeline append");
                            applied += 1;
                        }
                        Ok(false) => {
                            tracing::trace!(uuid, "Backend ignored timeline delta (not supported)");
                        }
                        Err(e) => {
                            tracing::warn!(uuid, "Failed to apply remote timeline delta: {e}");
                        }
                    }
                }
                DeltaOperation::PhoneCallRecord { call_uuid, .. } => {
                    match self.inner.apply_remote_v3_delta(op).await {
                        Ok(true) => {
                            tracing::debug!(call_uuid, "Applied remote phone call record");
                            applied += 1;
                        }
                        Ok(false) => {
                            tracing::trace!(call_uuid, "Backend ignored phone call delta (not supported)");
                        }
                        Err(e) => {
                            tracing::warn!(call_uuid, "Failed to apply remote phone call delta: {e}");
                        }
                    }
                }
                DeltaOperation::CompiledTruthUpdate { memory_key, .. } => {
                    match self.inner.apply_remote_v3_delta(op).await {
                        Ok(true) => {
                            tracing::debug!(memory_key, "Applied remote compiled truth update");
                            applied += 1;
                        }
                        Ok(false) => {
                            // LWW rejected: local version >= remote. Expected case.
                            tracing::trace!(
                                memory_key,
                                "Remote truth superseded by local version (LWW)"
                            );
                        }
                        Err(e) => {
                            tracing::warn!(memory_key, "Failed to apply remote truth delta: {e}");
                        }
                    }
                }
                // ── v6 Vault (Second Brain) delta ─────────────────
                DeltaOperation::VaultDocUpsert { uuid, .. } => {
                    match self.inner.apply_remote_v3_delta(op).await {
                        Ok(true) => {
                            tracing::debug!(uuid, "Applied remote vault doc upsert");
                            applied += 1;
                        }
                        Ok(false) => {
                            tracing::trace!(uuid, "Backend ignored vault doc delta (not supported)");
                        }
                        Err(e) => {
                            tracing::warn!(uuid, "Failed to apply remote vault doc delta: {e}");
                        }
                    }
                }
            }
        }

        if applied > 0 {
            tracing::info!(applied, total = ops.len(), "Applied remote sync deltas");
        }

        applied
    }

    /// Encrypt deltas that a remote peer has not seen yet.
    pub fn encrypt_deltas_since(
        &self,
        remote_version: &VersionVector,
    ) -> anyhow::Result<Option<SyncPayload>> {
        let engine = self.sync.lock();
        let deltas: Vec<DeltaEntry> = engine
            .get_deltas_since(remote_version)
            .into_iter()
            .cloned()
            .collect();
        if deltas.is_empty() {
            return Ok(None);
        }
        engine.encrypt_deltas(&deltas).map(Some)
    }

    /// Decrypt an incoming sync payload from a remote peer.
    pub fn decrypt_payload(&self, payload: &SyncPayload) -> anyhow::Result<Vec<DeltaEntry>> {
        let engine = self.sync.lock();
        engine.decrypt_payload(payload)
    }

    /// Get the current local version vector (for SyncRequest messages).
    pub fn version(&self) -> VersionVector {
        self.sync.lock().version().clone()
    }

    /// Get this device's ID.
    pub fn device_id(&self) -> String {
        self.sync.lock().device_id().0.clone()
    }

    /// Prune old journal entries (call periodically).
    pub fn prune_journal(&self) {
        self.sync.lock().prune_journal();
    }

    /// Build a manifest of all memory entries for Layer 3 full sync.
    pub async fn build_manifest(&self) -> anyhow::Result<crate::sync::protocol::FullSyncManifest> {
        use std::collections::HashSet;
        use std::time::{SystemTime, UNIX_EPOCH};

        let entries = self.inner.list(None, None).await?;
        let memory_chunk_ids: HashSet<String> = entries.into_iter().map(|e| e.key).collect();

        Ok(crate::sync::protocol::FullSyncManifest {
            memory_chunk_ids,
            conversation_ids: HashSet::new(),
            setting_keys: HashSet::new(),
            generated_at: SystemTime::now()
                .duration_since(UNIX_EPOCH)
                .unwrap_or_default()
                .as_secs(),
        })
    }

    /// Send missing entities for Layer 3 full sync.
    /// Given a set of keys the remote is missing, encrypt and return them.
    pub async fn export_missing_entries(
        &self,
        missing_keys: &std::collections::HashSet<String>,
    ) -> anyhow::Result<Vec<FullSyncEntry>> {
        let mut result = Vec::new();
        for key in missing_keys {
            if let Ok(Some(entry)) = self.inner.get(key).await {
                let engine = self.sync.lock();
                let payload_json = serde_json::to_string(&entry)?;
                let delta = DeltaEntry {
                    id: uuid::Uuid::new_v4().to_string(),
                    device_id: engine.device_id().0.clone(),
                    version: engine.version().clone(),
                    operation: DeltaOperation::Store {
                        key: entry.key.clone(),
                        content: entry.content.clone(),
                        category: entry.category.to_string(),
                    },
                    timestamp: std::time::SystemTime::now()
                        .duration_since(std::time::UNIX_EPOCH)
                        .unwrap_or_default()
                        .as_secs(),
                };
                let encrypted = engine.encrypt_deltas(&[delta])?;
                result.push(FullSyncEntry {
                    entity_type: "memory".to_string(),
                    entity_id: entry.key,
                    encrypted_payload: encrypted.ciphertext,
                    iv: encrypted.nonce.clone(),
                    auth_tag: String::new(), // ChaCha20-Poly1305 includes auth tag in ciphertext
                    raw_json: payload_json,
                });
            }
        }
        Ok(result)
    }

    /// Import entries received during Layer 3 full sync.
    pub async fn import_full_sync_entries(
        &self,
        entries: Vec<DeltaEntry>,
        ontology: Option<&OntologyRepo>,
    ) -> usize {
        self.apply_remote_deltas(entries, ontology).await
    }
}

/// A single entity prepared for Layer 3 full sync transfer.
pub struct FullSyncEntry {
    pub entity_type: String,
    pub entity_id: String,
    pub encrypted_payload: String,
    pub iv: String,
    pub auth_tag: String,
    pub raw_json: String,
}

#[async_trait]
impl Memory for SyncedMemory {
    fn name(&self) -> &str {
        self.inner.name()
    }

    async fn store(
        &self,
        key: &str,
        content: &str,
        category: MemoryCategory,
        session_id: Option<&str>,
    ) -> anyhow::Result<()> {
        // 1. Persist to the actual backend first.
        self.inner
            .store(key, content, category.clone(), session_id)
            .await?;

        // 2. Record the delta in the sync journal ONLY for long-term categories.
        //    Short-term (Conversation) memory is NOT synced across devices.
        //    Sync only triggers when data is promoted to Core/Daily or ontology.
        if category != MemoryCategory::Conversation {
            let mut engine = self.sync.lock();
            engine.record_store(key, content, &category.to_string());
            tracing::trace!(key, %category, "Sync: recorded store delta");
        } else {
            tracing::trace!(key, %category, "Sync: skipped Conversation category (short-term only)");
        }

        Ok(())
    }

    async fn recall(
        &self,
        query: &str,
        limit: usize,
        session_id: Option<&str>,
    ) -> anyhow::Result<Vec<MemoryEntry>> {
        // Read-only — no sync recording needed.
        self.inner.recall(query, limit, session_id).await
    }

    async fn get(&self, key: &str) -> anyhow::Result<Option<MemoryEntry>> {
        self.inner.get(key).await
    }

    async fn list(
        &self,
        category: Option<&MemoryCategory>,
        session_id: Option<&str>,
    ) -> anyhow::Result<Vec<MemoryEntry>> {
        self.inner.list(category, session_id).await
    }

    async fn forget(&self, key: &str) -> anyhow::Result<bool> {
        // 1. Delete from the actual backend.
        let deleted = self.inner.forget(key).await?;

        // 2. Record the delta only if something was actually deleted.
        if deleted {
            let mut engine = self.sync.lock();
            engine.record_forget(key);
            tracing::trace!(key, "Sync: recorded forget delta");
        }

        Ok(deleted)
    }

    async fn count(&self) -> anyhow::Result<usize> {
        self.inner.count().await
    }

    async fn health_check(&self) -> bool {
        self.inner.health_check().await
    }
}

/// Convert a category string back to a [`MemoryCategory`].
fn category_from_str(s: &str) -> MemoryCategory {
    match s {
        "core" => MemoryCategory::Core,
        "daily" => MemoryCategory::Daily,
        "conversation" => MemoryCategory::Conversation,
        other => MemoryCategory::Custom(other.to_string()),
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::memory::sync::SyncEngine;
    use crate::memory::SqliteMemory;
    use tempfile::TempDir;

    fn setup() -> (TempDir, Arc<dyn Memory>, Arc<Mutex<SyncEngine>>) {
        let tmp = TempDir::new().unwrap();
        let mem: Arc<dyn Memory> = Arc::new(SqliteMemory::new(tmp.path()).unwrap());
        let sync = Arc::new(Mutex::new(SyncEngine::new(tmp.path(), true).unwrap()));
        (tmp, mem, sync)
    }

    #[tokio::test]
    async fn store_records_delta_in_sync_engine() {
        let (_tmp, mem, sync) = setup();
        let synced = SyncedMemory::new(mem, sync.clone());

        synced
            .store("lang", "Rust", MemoryCategory::Core, None)
            .await
            .unwrap();

        let engine = sync.lock();
        assert_eq!(engine.journal_len(), 1);
    }

    #[tokio::test]
    async fn forget_records_delta_in_sync_engine() {
        let (_tmp, mem, sync) = setup();
        let synced = SyncedMemory::new(mem, sync.clone());

        synced
            .store("tmp", "data", MemoryCategory::Core, None)
            .await
            .unwrap();
        synced.forget("tmp").await.unwrap();

        let engine = sync.lock();
        assert_eq!(engine.journal_len(), 2); // store + forget
    }

    #[tokio::test]
    async fn forget_nonexistent_does_not_record_delta() {
        let (_tmp, mem, sync) = setup();
        let synced = SyncedMemory::new(mem, sync.clone());

        let deleted = synced.forget("nonexistent").await.unwrap();
        assert!(!deleted);

        let engine = sync.lock();
        assert_eq!(engine.journal_len(), 0);
    }

    #[tokio::test]
    async fn recall_does_not_record_deltas() {
        let (_tmp, mem, sync) = setup();
        let synced = SyncedMemory::new(mem, sync.clone());

        synced
            .store("fact", "Rust is fast", MemoryCategory::Core, None)
            .await
            .unwrap();

        let results = synced.recall("Rust", 5, None).await.unwrap();
        assert!(!results.is_empty());

        let engine = sync.lock();
        assert_eq!(engine.journal_len(), 1); // only the store
    }

    #[tokio::test]
    async fn apply_remote_deltas_stores_to_inner_backend() {
        let (_tmp, mem, sync) = setup();
        let synced = SyncedMemory::new(mem.clone(), sync);

        let mut remote_vv = VersionVector::default();
        remote_vv.increment("remote_device");

        let remote_deltas = vec![DeltaEntry {
            id: "rd-1".into(),
            device_id: "remote_device".into(),
            version: remote_vv,
            operation: DeltaOperation::Store {
                key: "remote_key".into(),
                content: "remote_value".into(),
                category: "core".into(),
            },
            timestamp: 9999,
        }];

        let applied = synced.apply_remote_deltas(remote_deltas, None).await;
        assert_eq!(applied, 1);

        let entry = mem.get("remote_key").await.unwrap();
        assert!(entry.is_some());
        assert_eq!(entry.unwrap().content, "remote_value");
    }

    #[tokio::test]
    async fn apply_remote_forget_removes_from_inner_backend() {
        let (_tmp, mem, sync) = setup();
        let synced = SyncedMemory::new(mem.clone(), sync);

        // Pre-populate
        mem.store("to_delete", "value", MemoryCategory::Core, None)
            .await
            .unwrap();

        let mut vv = VersionVector::default();
        vv.increment("remote_device");

        let remote_deltas = vec![DeltaEntry {
            id: "rd-2".into(),
            device_id: "remote_device".into(),
            version: vv,
            operation: DeltaOperation::Forget {
                key: "to_delete".into(),
            },
            timestamp: 9999,
        }];

        let applied = synced.apply_remote_deltas(remote_deltas, None).await;
        assert_eq!(applied, 1);

        assert!(mem.get("to_delete").await.unwrap().is_none());
    }

    #[tokio::test]
    async fn duplicate_remote_deltas_are_idempotent() {
        let (_tmp, mem, sync) = setup();
        let synced = SyncedMemory::new(mem, sync);

        let mut vv = VersionVector::default();
        vv.increment("remote_device");

        let delta = DeltaEntry {
            id: "rd-dup".into(),
            device_id: "remote_device".into(),
            version: vv.clone(),
            operation: DeltaOperation::Store {
                key: "dup_key".into(),
                content: "dup_value".into(),
                category: "core".into(),
            },
            timestamp: 9999,
        };

        let applied1 = synced.apply_remote_deltas(vec![delta.clone()], None).await;
        assert_eq!(applied1, 1);

        let applied2 = synced.apply_remote_deltas(vec![delta], None).await;
        assert_eq!(applied2, 0); // duplicate — already seen
    }

    #[tokio::test]
    async fn two_device_roundtrip_sync() {
        // Simulate: device A stores → sync to device B → B has the data
        let tmp_a = TempDir::new().unwrap();
        let tmp_b = TempDir::new().unwrap();

        let mem_a: Arc<dyn Memory> = Arc::new(SqliteMemory::new(tmp_a.path()).unwrap());
        let sync_a = Arc::new(Mutex::new(SyncEngine::new(tmp_a.path(), true).unwrap()));
        let synced_a = SyncedMemory::new(mem_a, sync_a.clone());

        let mem_b: Arc<dyn Memory> = Arc::new(SqliteMemory::new(tmp_b.path()).unwrap());
        let sync_b = Arc::new(Mutex::new(SyncEngine::new(tmp_b.path(), true).unwrap()));
        let synced_b = SyncedMemory::new(mem_b.clone(), sync_b);

        // Device A stores a memory
        synced_a
            .store(
                "shared_fact",
                "42 is the answer",
                MemoryCategory::Core,
                None,
            )
            .await
            .unwrap();

        // Device A encrypts deltas for device B
        let version_b = synced_b.version();
        let payload = synced_a.encrypt_deltas_since(&version_b).unwrap().unwrap();

        // Device B decrypts and applies
        let deltas = synced_a.decrypt_payload(&payload).unwrap();
        // For this test, B needs same key — simulate shared key by using A's engine
        // In production, all devices share the same .sync_key file
        let applied = synced_b.apply_remote_deltas(deltas, None).await;
        assert_eq!(applied, 1);

        // Device B now has the data
        let entry = mem_b.get("shared_fact").await.unwrap();
        assert!(entry.is_some());
        assert_eq!(entry.unwrap().content, "42 is the answer");
    }

    #[tokio::test]
    async fn build_manifest_lists_all_keys() {
        let (_tmp, mem, sync) = setup();
        let synced = SyncedMemory::new(mem, sync);

        synced
            .store("k1", "v1", MemoryCategory::Core, None)
            .await
            .unwrap();
        synced
            .store("k2", "v2", MemoryCategory::Daily, None)
            .await
            .unwrap();

        let manifest = synced.build_manifest().await.unwrap();
        assert_eq!(manifest.memory_chunk_ids.len(), 2);
        assert!(manifest.memory_chunk_ids.contains("k1"));
        assert!(manifest.memory_chunk_ids.contains("k2"));
    }

    #[tokio::test]
    async fn name_delegates_to_inner() {
        let (_tmp, mem, sync) = setup();
        let synced = SyncedMemory::new(mem.clone(), sync);
        assert_eq!(synced.name(), mem.name());
    }

    #[tokio::test]
    async fn health_check_delegates_to_inner() {
        let (_tmp, mem, sync) = setup();
        let synced = SyncedMemory::new(mem, sync);
        assert!(synced.health_check().await);
    }
}
