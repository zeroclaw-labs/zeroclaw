//! Cross-device memory synchronization module.
//!
//! Enables real-time synchronization of long-term memory entries across
//! multiple ZeroClaw instances running on different devices.
//!
//! ## Design
//! - **Version Vectors**: Causal ordering via Lamport-like version vectors per device
//! - **Delta Journals**: Compact change records (store/forget) with timestamps
//! - **E2E Encryption**: All sync payloads encrypted with ChaCha20-Poly1305 before transit
//! - **Conflict Resolution**: Last-writer-wins with device priority tiebreaker
//! - **Journal Retention**: 30-day rolling window for delta entries
//!
//! ## Sync Modes
//! - **Push**: Local changes are broadcast to connected peers
//! - **Pull**: On startup, request missing deltas from peers
//! - **Full Sync**: Periodic full reconciliation for consistency

use chacha20poly1305::aead::{Aead, KeyInit};
use chacha20poly1305::{ChaCha20Poly1305, Nonce};
use serde::{Deserialize, Serialize};
use std::collections::HashMap;
use std::path::{Path, PathBuf};
use std::time::{SystemTime, UNIX_EPOCH};

/// Maximum age for delta journal entries before pruning (30 days).
const JOURNAL_RETENTION_SECS: u64 = 30 * 24 * 3600;

/// Nonce size for ChaCha20-Poly1305 (12 bytes).
const NONCE_SIZE: usize = 12;

/// Unique identifier for a device in the sync mesh.
#[derive(Debug, Clone, Hash, Eq, PartialEq, Serialize, Deserialize)]
pub struct DeviceId(pub String);

impl DeviceId {
    /// Generate a new random device ID.
    pub fn generate() -> Self {
        Self(uuid::Uuid::new_v4().to_string())
    }
}

/// Version vector tracking causal ordering of changes across devices.
#[derive(Debug, Clone, Default, Serialize, Deserialize)]
pub struct VersionVector {
    /// Map of device_id -> logical clock value.
    pub clocks: HashMap<String, u64>,
}

impl VersionVector {
    /// Increment the clock for the given device.
    pub fn increment(&mut self, device_id: &str) {
        let counter = self.clocks.entry(device_id.to_string()).or_insert(0);
        *counter += 1;
    }

    /// Get the clock value for a device. Returns 0 if not seen.
    pub fn get(&self, device_id: &str) -> u64 {
        self.clocks.get(device_id).copied().unwrap_or(0)
    }

    /// Merge another version vector (take max of each device clock).
    pub fn merge(&mut self, other: &VersionVector) {
        for (device, clock) in &other.clocks {
            let current = self.clocks.entry(device.clone()).or_insert(0);
            *current = (*current).max(*clock);
        }
    }

    /// Check if this version vector dominates (is causally after) another.
    pub fn dominates(&self, other: &VersionVector) -> bool {
        // All devices in other must have equal or lower clocks in self
        for (device, &other_clock) in &other.clocks {
            if self.get(device) < other_clock {
                return false;
            }
        }
        true
    }

    /// Check if two version vectors are concurrent (neither dominates).
    pub fn is_concurrent_with(&self, other: &VersionVector) -> bool {
        !self.dominates(other) && !other.dominates(self)
    }
}

/// Type of change in a delta journal entry.
#[derive(Debug, Clone, Serialize, Deserialize, PartialEq)]
pub enum DeltaOperation {
    /// Memory entry stored or updated.
    Store {
        key: String,
        content: String,
        category: String,
    },
    /// Memory entry deleted.
    Forget { key: String },

    // ── Ontology sync operations ───────────────────────────────────

    /// Ontology object created or updated.
    OntologyObjectUpsert {
        object_id: i64,
        type_name: String,
        title: Option<String>,
        properties_json: String,
        owner_user_id: String,
    },
    /// Ontology link created.
    OntologyLinkCreate {
        link_type_name: String,
        from_object_id: i64,
        to_object_id: i64,
        properties_json: Option<String>,
    },
    /// Ontology action logged (read-only replication — actions are
    /// never replayed, only the log entry is synced).
    OntologyActionLog {
        action_type_name: String,
        actor_user_id: String,
        params_json: String,
        result_json: Option<String>,
        channel: Option<String>,
        /// UTC absolute time — primary sort key for cross-device ordering.
        occurred_at_utc: Option<String>,
        /// Device-local time with timezone offset.
        occurred_at_local: Option<String>,
        /// IANA timezone of the recording device.
        timezone: Option<String>,
        /// Home timezone display time.
        occurred_at_home: Option<String>,
        /// User's home timezone IANA name.
        home_timezone: Option<String>,
        /// Where the action occurred (free-form location string).
        location: Option<String>,
        status: String,
    },
}

/// A single delta journal entry representing one memory change.
#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct DeltaEntry {
    /// Unique ID for this delta.
    pub id: String,
    /// Device that originated this change.
    pub device_id: String,
    /// Version vector at the time of this change.
    pub version: VersionVector,
    /// The actual operation.
    pub operation: DeltaOperation,
    /// Unix timestamp (seconds) when this entry was created.
    pub timestamp: u64,
}

/// Encrypted sync payload for transit between devices.
#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct SyncPayload {
    /// Nonce used for encryption (base64-encoded).
    pub nonce: String,
    /// Encrypted delta entries (base64-encoded ciphertext).
    pub ciphertext: String,
    /// Sending device ID.
    pub sender: String,
    /// Sender's version vector (unencrypted, for filtering).
    pub version: VersionVector,
}

/// Sync engine managing cross-device memory synchronization.
pub struct SyncEngine {
    /// This device's unique identifier.
    device_id: DeviceId,
    /// Current version vector.
    version: VersionVector,
    /// Delta journal (in-memory cache, persisted to SQLite on write).
    journal: Vec<DeltaEntry>,
    /// Encryption key for sync payloads (32 bytes).
    encryption_key: [u8; 32],
    /// Path to the sync state SQLite database.
    db_path: PathBuf,
    /// Whether sync is enabled.
    enabled: bool,
}

impl SyncEngine {
    /// Initialize the SQLite journal database, creating the table if needed.
    fn init_db(db_path: &Path) -> anyhow::Result<()> {
        let conn = rusqlite::Connection::open(db_path)?;
        conn.execute_batch(
            "PRAGMA journal_mode = WAL;
             PRAGMA synchronous  = NORMAL;
             PRAGMA busy_timeout = 5000;
             CREATE TABLE IF NOT EXISTS sync_journal (
                id TEXT PRIMARY KEY,
                device_id TEXT NOT NULL,
                version_json TEXT NOT NULL,
                operation_json TEXT NOT NULL,
                timestamp INTEGER NOT NULL
            );
            CREATE TABLE IF NOT EXISTS sync_version (
                key TEXT PRIMARY KEY,
                value_json TEXT NOT NULL
            );
            CREATE INDEX IF NOT EXISTS idx_journal_timestamp ON sync_journal(timestamp);
            CREATE INDEX IF NOT EXISTS idx_journal_device ON sync_journal(device_id);",
        )?;
        Ok(())
    }

    /// Persist the current journal and version vector to SQLite.
    pub fn save(&self) -> anyhow::Result<()> {
        if !self.enabled {
            return Ok(());
        }
        let conn = rusqlite::Connection::open(&self.db_path)?;
        conn.execute_batch("PRAGMA journal_mode = WAL; PRAGMA synchronous = NORMAL; PRAGMA busy_timeout = 5000;")?;

        // Wrap in a single transaction for atomicity and performance
        // (one fsync instead of N).
        let tx = conn.unchecked_transaction()?;

        // Save version vector
        let version_json = serde_json::to_string(&self.version)?;
        tx.execute(
            "INSERT OR REPLACE INTO sync_version (key, value_json) VALUES ('current', ?1)",
            rusqlite::params![version_json],
        )?;

        // Upsert journal entries
        let mut stmt = tx.prepare_cached(
            "INSERT OR IGNORE INTO sync_journal (id, device_id, version_json, operation_json, timestamp)
             VALUES (?1, ?2, ?3, ?4, ?5)",
        )?;
        for entry in &self.journal {
            let version_json = serde_json::to_string(&entry.version)?;
            let operation_json = serde_json::to_string(&entry.operation)?;
            stmt.execute(rusqlite::params![
                entry.id,
                entry.device_id,
                version_json,
                operation_json,
                entry.timestamp as i64,
            ])?;
        }
        drop(stmt);
        tx.commit()?;

        Ok(())
    }

    /// Load journal and version vector from SQLite.
    pub fn load(&mut self) -> anyhow::Result<()> {
        if !self.enabled || !self.db_path.exists() {
            return Ok(());
        }
        let conn = rusqlite::Connection::open(&self.db_path)?;
        conn.execute_batch("PRAGMA journal_mode = WAL; PRAGMA synchronous = NORMAL; PRAGMA busy_timeout = 5000;")?;

        // Load version vector
        let version_result: Result<String, _> = conn.query_row(
            "SELECT value_json FROM sync_version WHERE key = 'current'",
            [],
            |row| row.get(0),
        );
        if let Ok(version_json) = version_result {
            self.version = serde_json::from_str(&version_json)?;
        }

        // Load journal entries
        let mut stmt = conn.prepare(
            "SELECT id, device_id, version_json, operation_json, timestamp FROM sync_journal ORDER BY timestamp ASC",
        )?;
        let entries = stmt.query_map([], |row| {
            let id: String = row.get(0)?;
            let device_id: String = row.get(1)?;
            let version_json: String = row.get(2)?;
            let operation_json: String = row.get(3)?;
            let timestamp: i64 = row.get(4)?;
            Ok((id, device_id, version_json, operation_json, timestamp))
        })?;

        self.journal.clear();
        for entry in entries {
            let (id, device_id, version_json, operation_json, timestamp) = entry?;
            let version: VersionVector = serde_json::from_str(&version_json)
                .map_err(|e| rusqlite::Error::ToSqlConversionFailure(Box::new(e)))?;
            let operation: DeltaOperation = serde_json::from_str(&operation_json)
                .map_err(|e| rusqlite::Error::ToSqlConversionFailure(Box::new(e)))?;
            self.journal.push(DeltaEntry {
                id,
                device_id,
                version,
                operation,
                timestamp: u64::try_from(timestamp).unwrap_or(0),
            });
        }

        Ok(())
    }
}

impl SyncEngine {
    /// Create a new sync engine for the given workspace.
    pub fn new(workspace_dir: &Path, enabled: bool) -> anyhow::Result<Self> {
        let db_path = workspace_dir.join("sync_state.db");

        // Load or generate device ID
        let device_id_path = workspace_dir.join(".device_id");
        let device_id = if device_id_path.exists() {
            let id_str = std::fs::read_to_string(&device_id_path)?;
            DeviceId(id_str.trim().to_string())
        } else {
            let id = DeviceId::generate();
            std::fs::write(&device_id_path, &id.0)?;
            // Restrict key file to owner-only access (0o600).
            #[cfg(unix)]
            {
                use std::os::unix::fs::PermissionsExt;
                std::fs::set_permissions(&device_id_path, std::fs::Permissions::from_mode(0o600))?;
            }
            id
        };

        // Load or generate encryption key
        let key_path = workspace_dir.join(".sync_key");
        let encryption_key = if key_path.exists() {
            let key_bytes = std::fs::read(&key_path)?;
            if key_bytes.len() != 32 {
                anyhow::bail!("Invalid sync key length (expected 32 bytes)");
            }
            let mut key = [0u8; 32];
            key.copy_from_slice(&key_bytes);
            key
        } else {
            let mut key = [0u8; 32];
            rand::fill(&mut key);
            std::fs::write(&key_path, key)?;
            // Restrict key file to owner-only access (0o600).
            #[cfg(unix)]
            {
                use std::os::unix::fs::PermissionsExt;
                std::fs::set_permissions(&key_path, std::fs::Permissions::from_mode(0o600))?;
            }
            key
        };

        if enabled {
            Self::init_db(&db_path)?;
        }

        let mut engine = Self {
            device_id,
            version: VersionVector::default(),
            journal: Vec::new(),
            encryption_key,
            db_path,
            enabled,
        };

        // Load persisted state from SQLite
        engine.load()?;

        Ok(engine)
    }

    /// Get this device's ID.
    pub fn device_id(&self) -> &DeviceId {
        &self.device_id
    }

    /// Check if sync is enabled.
    pub fn is_enabled(&self) -> bool {
        self.enabled
    }

    /// Record a memory store operation in the delta journal.
    pub fn record_store(&mut self, key: &str, content: &str, category: &str) {
        if !self.enabled {
            return;
        }

        self.version.increment(&self.device_id.0);
        let seq = self.version.get(&self.device_id.0);

        let entry = DeltaEntry {
            id: uuid::Uuid::new_v4().to_string(),
            device_id: self.device_id.0.clone(),
            version: self.version.clone(),
            operation: DeltaOperation::Store {
                key: key.to_string(),
                content: content.to_string(),
                category: category.to_string(),
            },
            timestamp: current_epoch_secs(),
        };

        self.journal.push(entry);

        tracing::debug!(
            key,
            category,
            seq,
            device_id = %self.device_id.0,
            journal_size = self.journal.len(),
            "Sync: recorded store delta"
        );

        // Persist to SQLite (best-effort; log errors but don't fail)
        if let Err(e) = self.save() {
            tracing::warn!("Failed to persist sync journal: {e}");
        }
    }

    /// Record a memory forget operation in the delta journal.
    pub fn record_forget(&mut self, key: &str) {
        if !self.enabled {
            return;
        }

        self.version.increment(&self.device_id.0);
        let seq = self.version.get(&self.device_id.0);

        let entry = DeltaEntry {
            id: uuid::Uuid::new_v4().to_string(),
            device_id: self.device_id.0.clone(),
            version: self.version.clone(),
            operation: DeltaOperation::Forget {
                key: key.to_string(),
            },
            timestamp: current_epoch_secs(),
        };

        self.journal.push(entry);

        tracing::debug!(
            key,
            seq,
            device_id = %self.device_id.0,
            journal_size = self.journal.len(),
            "Sync: recorded forget delta"
        );

        // Persist to SQLite (best-effort)
        if let Err(e) = self.save() {
            tracing::warn!("Failed to persist sync journal: {e}");
        }
    }

    /// Record an ontology object create/update in the delta journal.
    pub fn record_ontology_object(
        &mut self,
        object_id: i64,
        type_name: &str,
        title: Option<&str>,
        properties_json: &str,
        owner_user_id: &str,
    ) {
        if !self.enabled {
            return;
        }
        self.version.increment(&self.device_id.0);
        let entry = DeltaEntry {
            id: uuid::Uuid::new_v4().to_string(),
            device_id: self.device_id.0.clone(),
            version: self.version.clone(),
            operation: DeltaOperation::OntologyObjectUpsert {
                object_id,
                type_name: type_name.to_string(),
                title: title.map(String::from),
                properties_json: properties_json.to_string(),
                owner_user_id: owner_user_id.to_string(),
            },
            timestamp: current_epoch_secs(),
        };
        self.journal.push(entry);
        if let Err(e) = self.save() {
            tracing::warn!("Failed to persist ontology object sync: {e}");
        }
    }

    /// Record an ontology link creation in the delta journal.
    pub fn record_ontology_link(
        &mut self,
        link_type_name: &str,
        from_object_id: i64,
        to_object_id: i64,
        properties_json: Option<&str>,
    ) {
        if !self.enabled {
            return;
        }
        self.version.increment(&self.device_id.0);
        let entry = DeltaEntry {
            id: uuid::Uuid::new_v4().to_string(),
            device_id: self.device_id.0.clone(),
            version: self.version.clone(),
            operation: DeltaOperation::OntologyLinkCreate {
                link_type_name: link_type_name.to_string(),
                from_object_id,
                to_object_id,
                properties_json: properties_json.map(String::from),
            },
            timestamp: current_epoch_secs(),
        };
        self.journal.push(entry);
        if let Err(e) = self.save() {
            tracing::warn!("Failed to persist ontology link sync: {e}");
        }
    }

    /// Record an ontology action log entry in the delta journal.
    ///
    /// The timestamp triple (UTC / local / home) and location capture the
    /// real-world **when** and **where** of the action, enabling timeline
    /// and location-based queries on remote devices after sync.
    pub fn record_ontology_action(
        &mut self,
        action_type_name: &str,
        actor_user_id: &str,
        params_json: &str,
        result_json: Option<&str>,
        channel: Option<&str>,
        occurred_at_utc: Option<&str>,
        occurred_at_local: Option<&str>,
        timezone: Option<&str>,
        occurred_at_home: Option<&str>,
        home_timezone: Option<&str>,
        location: Option<&str>,
        status: &str,
    ) {
        if !self.enabled {
            return;
        }
        self.version.increment(&self.device_id.0);
        let entry = DeltaEntry {
            id: uuid::Uuid::new_v4().to_string(),
            device_id: self.device_id.0.clone(),
            version: self.version.clone(),
            operation: DeltaOperation::OntologyActionLog {
                action_type_name: action_type_name.to_string(),
                actor_user_id: actor_user_id.to_string(),
                params_json: params_json.to_string(),
                result_json: result_json.map(String::from),
                channel: channel.map(String::from),
                occurred_at_utc: occurred_at_utc.map(String::from),
                occurred_at_local: occurred_at_local.map(String::from),
                timezone: timezone.map(String::from),
                occurred_at_home: occurred_at_home.map(String::from),
                home_timezone: home_timezone.map(String::from),
                location: location.map(String::from),
                status: status.to_string(),
            },
            timestamp: current_epoch_secs(),
        };
        self.journal.push(entry);
        if let Err(e) = self.save() {
            tracing::warn!("Failed to persist ontology action sync: {e}");
        }
    }

    /// Get deltas that the remote device hasn't seen yet.
    pub fn get_deltas_since(&self, remote_version: &VersionVector) -> Vec<&DeltaEntry> {
        self.journal
            .iter()
            .filter(|entry| {
                let remote_clock = remote_version.get(&entry.device_id);
                entry.version.get(&entry.device_id) > remote_clock
            })
            .collect()
    }

    /// Apply incoming deltas from a remote device.
    /// Returns the list of operations applied.
    pub fn apply_deltas(&mut self, deltas: Vec<DeltaEntry>) -> Vec<DeltaOperation> {
        let mut applied = Vec::new();
        let total_incoming = deltas.len();
        let mut skipped = 0usize;

        for delta in deltas {
            let local_clock = self.version.get(&delta.device_id);
            let remote_clock = delta.version.get(&delta.device_id);

            // Only apply if this is newer than what we've seen from this device
            if remote_clock > local_clock {
                tracing::debug!(
                    from_device = %delta.device_id,
                    remote_clock,
                    local_clock,
                    op = ?delta.operation,
                    "Sync: applying remote delta"
                );
                self.version.merge(&delta.version);
                applied.push(delta.operation.clone());
                self.journal.push(delta);
            } else {
                skipped += 1;
            }
        }

        if !applied.is_empty() {
            tracing::info!(
                applied = applied.len(),
                skipped,
                total_incoming,
                "Sync: applied incoming deltas from remote"
            );
            if let Err(e) = self.save() {
                tracing::warn!("Failed to persist sync journal after apply: {e}");
            }
        } else if total_incoming > 0 {
            tracing::debug!(
                skipped = total_incoming,
                "Sync: all incoming deltas already seen"
            );
        }

        applied
    }

    /// Encrypt delta entries for transit.
    pub fn encrypt_deltas(&self, deltas: &[DeltaEntry]) -> anyhow::Result<SyncPayload> {
        let plaintext = serde_json::to_vec(deltas)?;

        let cipher = ChaCha20Poly1305::new_from_slice(&self.encryption_key)
            .map_err(|e| anyhow::anyhow!("Failed to create cipher: {e}"))?;

        let mut nonce_bytes = [0u8; NONCE_SIZE];
        rand::fill(&mut nonce_bytes);
        let nonce = Nonce::from_slice(&nonce_bytes);

        let ciphertext = cipher
            .encrypt(nonce, plaintext.as_ref())
            .map_err(|e| anyhow::anyhow!("Encryption failed: {e}"))?;

        use base64::Engine;
        Ok(SyncPayload {
            nonce: base64::engine::general_purpose::STANDARD.encode(nonce_bytes),
            ciphertext: base64::engine::general_purpose::STANDARD.encode(ciphertext),
            sender: self.device_id.0.clone(),
            version: self.version.clone(),
        })
    }

    /// Decrypt a sync payload from a remote device.
    pub fn decrypt_payload(&self, payload: &SyncPayload) -> anyhow::Result<Vec<DeltaEntry>> {
        use base64::Engine;

        let nonce_bytes = base64::engine::general_purpose::STANDARD.decode(&payload.nonce)?;
        if nonce_bytes.len() != NONCE_SIZE {
            anyhow::bail!("Invalid nonce length");
        }

        let ciphertext = base64::engine::general_purpose::STANDARD.decode(&payload.ciphertext)?;

        let cipher = ChaCha20Poly1305::new_from_slice(&self.encryption_key)
            .map_err(|e| anyhow::anyhow!("Failed to create cipher: {e}"))?;

        let nonce = Nonce::from_slice(&nonce_bytes);
        let plaintext = cipher
            .decrypt(nonce, ciphertext.as_ref())
            .map_err(|e| anyhow::anyhow!("Decryption failed: {e}"))?;

        let deltas: Vec<DeltaEntry> = serde_json::from_slice(&plaintext)?;
        Ok(deltas)
    }

    /// Prune old journal entries beyond the retention period.
    pub fn prune_journal(&mut self) {
        let cutoff = current_epoch_secs().saturating_sub(JOURNAL_RETENTION_SECS);
        let before = self.journal.len();
        self.journal.retain(|entry| entry.timestamp >= cutoff);

        let pruned = before - self.journal.len();
        if pruned > 0 {
            tracing::info!(
                pruned,
                remaining = self.journal.len(),
                cutoff_secs_ago = JOURNAL_RETENTION_SECS,
                "Sync: pruned old journal entries"
            );
            // Delete pruned entries from SQLite too
            if let Ok(conn) = rusqlite::Connection::open(&self.db_path) {
                let _ = conn.execute_batch("PRAGMA busy_timeout = 5000;");
                let _ = conn.execute(
                    "DELETE FROM sync_journal WHERE timestamp < ?1",
                    rusqlite::params![cutoff as i64],
                );
            }
        }
    }

    /// Get the current version vector.
    pub fn version(&self) -> &VersionVector {
        &self.version
    }

    /// Get the journal size.
    pub fn journal_len(&self) -> usize {
        self.journal.len()
    }
}

fn current_epoch_secs() -> u64 {
    SystemTime::now()
        .duration_since(UNIX_EPOCH)
        .unwrap_or_default()
        .as_secs()
}

#[cfg(test)]
mod tests {
    use super::*;
    use tempfile::TempDir;

    #[test]
    fn version_vector_increment_and_get() {
        let mut vv = VersionVector::default();
        assert_eq!(vv.get("device_a"), 0);

        vv.increment("device_a");
        assert_eq!(vv.get("device_a"), 1);

        vv.increment("device_a");
        assert_eq!(vv.get("device_a"), 2);
    }

    #[test]
    fn version_vector_merge() {
        let mut vv1 = VersionVector::default();
        vv1.increment("device_a");
        vv1.increment("device_a");

        let mut vv2 = VersionVector::default();
        vv2.increment("device_b");
        vv2.increment("device_a");

        vv1.merge(&vv2);
        assert_eq!(vv1.get("device_a"), 2); // max(2, 1)
        assert_eq!(vv1.get("device_b"), 1); // max(0, 1)
    }

    #[test]
    fn version_vector_dominates() {
        let mut vv1 = VersionVector::default();
        vv1.increment("device_a");
        vv1.increment("device_a");
        vv1.increment("device_b");

        let mut vv2 = VersionVector::default();
        vv2.increment("device_a");

        assert!(vv1.dominates(&vv2));
        assert!(!vv2.dominates(&vv1));
    }

    #[test]
    fn version_vector_concurrent() {
        let mut vv1 = VersionVector::default();
        vv1.increment("device_a");

        let mut vv2 = VersionVector::default();
        vv2.increment("device_b");

        assert!(vv1.is_concurrent_with(&vv2));
        assert!(vv2.is_concurrent_with(&vv1));
    }

    #[test]
    fn sync_engine_record_and_get_deltas() {
        let tmp = TempDir::new().unwrap();
        let mut engine = SyncEngine::new(tmp.path(), true).unwrap();

        engine.record_store("key1", "value1", "general");
        engine.record_store("key2", "value2", "conversation");
        engine.record_forget("key1");

        assert_eq!(engine.journal_len(), 3);

        let empty_vv = VersionVector::default();
        let deltas = engine.get_deltas_since(&empty_vv);
        assert_eq!(deltas.len(), 3);
    }

    #[test]
    fn sync_engine_apply_deltas() {
        let tmp1 = TempDir::new().unwrap();
        let tmp2 = TempDir::new().unwrap();

        let mut engine1 = SyncEngine::new(tmp1.path(), true).unwrap();
        let mut engine2 = SyncEngine::new(tmp2.path(), true).unwrap();

        engine1.record_store("shared_key", "from_device_1", "general");

        let empty_vv = VersionVector::default();
        let deltas: Vec<DeltaEntry> = engine1
            .get_deltas_since(&empty_vv)
            .into_iter()
            .cloned()
            .collect();

        let applied = engine2.apply_deltas(deltas);
        assert_eq!(applied.len(), 1);
        assert!(matches!(
            &applied[0],
            DeltaOperation::Store { key, content, .. }
            if key == "shared_key" && content == "from_device_1"
        ));
    }

    #[test]
    fn sync_engine_encrypt_decrypt_roundtrip() {
        let tmp = TempDir::new().unwrap();
        let mut engine = SyncEngine::new(tmp.path(), true).unwrap();

        engine.record_store("secret_key", "secret_value", "general");

        let deltas: Vec<DeltaEntry> = engine
            .get_deltas_since(&VersionVector::default())
            .into_iter()
            .cloned()
            .collect();

        let payload = engine.encrypt_deltas(&deltas).unwrap();
        let decrypted = engine.decrypt_payload(&payload).unwrap();

        assert_eq!(decrypted.len(), 1);
        assert!(matches!(
            &decrypted[0].operation,
            DeltaOperation::Store { key, content, .. }
            if key == "secret_key" && content == "secret_value"
        ));
    }

    #[test]
    fn sync_engine_prune_journal() {
        let tmp = TempDir::new().unwrap();
        let mut engine = SyncEngine::new(tmp.path(), true).unwrap();

        // Add an entry with a very old timestamp
        engine.journal.push(DeltaEntry {
            id: "old_entry".into(),
            device_id: engine.device_id.0.clone(),
            version: VersionVector::default(),
            operation: DeltaOperation::Store {
                key: "old".into(),
                content: "stale".into(),
                category: "general".into(),
            },
            timestamp: 1000, // Very old
        });

        engine.record_store("new_key", "new_value", "general");

        assert_eq!(engine.journal_len(), 2);
        engine.prune_journal();
        assert_eq!(engine.journal_len(), 1);
    }

    #[test]
    fn sync_engine_disabled_skips_recording() {
        let tmp = TempDir::new().unwrap();
        let mut engine = SyncEngine::new(tmp.path(), false).unwrap();

        engine.record_store("key", "value", "general");
        assert_eq!(engine.journal_len(), 0);
    }

    #[test]
    fn device_id_persists_across_instances() {
        let tmp = TempDir::new().unwrap();

        let engine1 = SyncEngine::new(tmp.path(), true).unwrap();
        let id1 = engine1.device_id().0.clone();

        let engine2 = SyncEngine::new(tmp.path(), true).unwrap();
        let id2 = engine2.device_id().0.clone();

        assert_eq!(id1, id2);
    }

    #[test]
    fn journal_persists_across_instances() {
        let tmp = TempDir::new().unwrap();

        // Create engine and record some entries
        {
            let mut engine = SyncEngine::new(tmp.path(), true).unwrap();
            engine.record_store("persistent_key", "persistent_value", "general");
            engine.record_forget("old_key");
            assert_eq!(engine.journal_len(), 2);
        }

        // Create new engine from same directory — should load persisted journal
        {
            let engine = SyncEngine::new(tmp.path(), true).unwrap();
            assert_eq!(engine.journal_len(), 2);

            // Verify the operations were preserved
            let ops: Vec<_> = engine.journal.iter().map(|e| &e.operation).collect();
            assert!(matches!(
                ops[0],
                DeltaOperation::Store { key, .. } if key == "persistent_key"
            ));
            assert!(matches!(
                ops[1],
                DeltaOperation::Forget { key } if key == "old_key"
            ));
        }
    }

    #[test]
    fn duplicate_deltas_are_not_applied_twice() {
        let tmp1 = TempDir::new().unwrap();
        let tmp2 = TempDir::new().unwrap();

        let mut engine1 = SyncEngine::new(tmp1.path(), true).unwrap();
        let mut engine2 = SyncEngine::new(tmp2.path(), true).unwrap();

        engine1.record_store("key", "value", "general");

        let deltas: Vec<DeltaEntry> = engine1
            .get_deltas_since(&VersionVector::default())
            .into_iter()
            .cloned()
            .collect();

        // Apply once
        let applied1 = engine2.apply_deltas(deltas.clone());
        assert_eq!(applied1.len(), 1);

        // Apply same deltas again — should be idempotent
        let applied2 = engine2.apply_deltas(deltas);
        assert_eq!(applied2.len(), 0);
    }
}
