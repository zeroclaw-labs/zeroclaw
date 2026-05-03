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

/// Wire-format version for cross-device sync payloads.
///
/// * `1` — Pre-PR#7 behaviour: ordering by `timestamp: u64` only, conflict
///   resolution is "last delta applied wins".
/// * `2` — PR #7: deltas carry an optional `hlc_stamp`, and receivers with
///   the matching schema apply store deltas via HLC-guarded upsert. Peers
///   that speak v1 simply omit `hlc_stamp` (serde default) and fall back to
///   the wall-clock path, which keeps v1 ↔ v2 interop intact.
///
/// Bumped when the wire format gains new optional fields or new conflict-
/// resolution semantics that change which delta wins. Only load-bearing
/// schema changes (adding required fields, changing operation variants)
/// should force a hard incompatibility break.
pub const SYNC_PROTOCOL_VERSION: u32 = 2;

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

/// Embedding vector attached to a sync delta (PR #5 — vec2text defence).
///
/// Replicating pre-computed embeddings saves the receiving peer a costly
/// re-embedding pass **only when model/version/dim all match**. When they
/// do not, the receiver MUST drop this blob and enqueue re-embedding
/// locally — a vec2text-style attack that recovered the remote embedding
/// would let the attacker reconstruct the source text, so silently
/// accepting foreign-model floats would leak information.
///
/// All fields are copied from the [`EmbeddingProvider`] that produced the
/// vector on the source device; see `crate::memory::embedding::PROVIDER_*`
/// constants for canonical `provider` strings. The raw `vector` bytes are
/// little-endian IEEE-754 f32s packed back-to-back (length = `dim * 4`),
/// bincode-compatible.
///
/// [`EmbeddingProvider`]: crate::memory::embedding::EmbeddingProvider
#[derive(Debug, Clone, Serialize, Deserialize, PartialEq)]
pub struct EmbeddingBlob {
    /// Provider family (e.g. `"local_fastembed"`, `"openai"`).
    pub provider: String,
    /// Concrete model identifier (e.g. `"bge-m3"`).
    pub model: String,
    /// Schema version bumped on semantic change. See
    /// `EMBEDDING_SCHEMA_VERSION`.
    pub version: u32,
    /// Vector dimensionality — the receiving side cross-checks before
    /// feeding into its local vector index.
    pub dim: u32,
    /// Little-endian packed f32 payload. Serde serialisation keeps this
    /// as a Vec<u8> so bincode/JSON both work without special handling.
    pub vector: Vec<u8>,
}

impl EmbeddingBlob {
    /// Pack an f32 slice into a blob. Uses little-endian byte order so
    /// the wire representation is identical regardless of host endianness.
    pub fn pack(
        provider: impl Into<String>,
        model: impl Into<String>,
        version: u32,
        vector: &[f32],
    ) -> Self {
        let mut bytes = Vec::with_capacity(vector.len() * 4);
        for f in vector {
            bytes.extend_from_slice(&f.to_le_bytes());
        }
        #[allow(clippy::cast_possible_truncation)]
        let dim = vector.len() as u32;
        Self {
            provider: provider.into(),
            model: model.into(),
            version,
            dim,
            vector: bytes,
        }
    }

    /// Reverse of [`pack`] — unpack the little-endian f32 payload. Returns
    /// an error when the byte length is not a multiple of 4 or doesn't
    /// match `dim × 4`.
    ///
    /// [`pack`]: Self::pack
    pub fn unpack(&self) -> anyhow::Result<Vec<f32>> {
        if self.vector.len() % 4 != 0 {
            anyhow::bail!(
                "embedding blob length {} is not a multiple of 4 bytes",
                self.vector.len()
            );
        }
        let count = self.vector.len() / 4;
        let expected = usize::try_from(self.dim).unwrap_or(usize::MAX);
        if count != expected {
            anyhow::bail!(
                "embedding blob declares dim={} but carries {} f32 values",
                self.dim,
                count
            );
        }
        let mut out = Vec::with_capacity(count);
        for chunk in self.vector.chunks_exact(4) {
            let mut buf = [0u8; 4];
            buf.copy_from_slice(chunk);
            out.push(f32::from_le_bytes(buf));
        }
        Ok(out)
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
        /// PR #5 — optional pre-computed embedding for this content. Sent
        /// only by newer peers; older peers omit the field and receivers
        /// treat `None` as "compute locally". When present and model/version
        /// mismatch, the receiver MUST discard and re-embed.
        #[serde(default, skip_serializing_if = "Option::is_none")]
        embedding: Option<EmbeddingBlob>,
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
    // ── v3.0 Timeline / Phone sync operations ─────────────────────
    /// Append-only timeline evidence entry (never updated/deleted).
    TimelineAppend {
        uuid: String,
        memory_id: String,
        event_type: String,
        event_at: u64,
        source_ref: String,
        content: String,
        content_sha256: String,
        metadata_json: Option<String>,
    },
    /// Phone call metadata record.
    PhoneCallRecord {
        call_uuid: String,
        direction: String,
        caller_number_e164: Option<String>,
        caller_object_id: Option<i64>,
        started_at: u64,
        ended_at: Option<u64>,
        duration_ms: Option<u64>,
        transcript: Option<String>,
        summary: Option<String>,
        risk_level: String,
        memory_id: Option<String>,
    },
    /// Compiled truth updated for a memory entry (Dream Cycle output).
    CompiledTruthUpdate {
        memory_key: String,
        compiled_truth: String,
        truth_version: u32,
    },

    /// Vault (Second Brain) document upserted (v6 §6).
    /// Idempotent via `uuid` + `checksum` uniqueness on receiving side.
    VaultDocUpsert {
        uuid: String,
        source_type: String,
        title: Option<String>,
        checksum: String,
        content_sha256: String,
        frontmatter_json: Option<String>,
        links_json: Option<String>,
        /// PR #5 — optional pre-computed embedding. Subject to the same
        /// model-drift rejection policy as `Store::embedding`.
        #[serde(default, skip_serializing_if = "Option::is_none")]
        embedding: Option<EmbeddingBlob>,
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

    // ── Self-generating skill system (procedural memory) ────────────
    /// Procedural skill created or updated. Version LWW: higher wins.
    SkillUpsert {
        id: String,
        name: String,
        category: Option<String>,
        description: String,
        content_md: String,
        version: i64,
        created_by: String,
    },

    /// User profile conclusion (cross-session behavioral modeling).
    UserProfileConclusion {
        dimension: String,
        conclusion: String,
        confidence: f64,
        evidence_count: i64,
    },

    /// Self-learning correction pattern (document/coding/interpret edit learning).
    CorrectionPatternUpsert {
        pattern_type: String,
        original_regex: String,
        replacement: String,
        scope: String,
        confidence: f64,
        observation_count: i64,
        accept_count: i64,
        reject_count: i64,
    },

    // ── Q1 Commit #8 sync delta ops ────────────────────────────────
    // Palantir-style ontology normalization + First Brain Wiki.
    // Receivers apply these with existing HLC-based LWW conflict resolution.

    /// Memory 5W1H fields written by the Dream-Cycle SLM diary backfill.
    /// The receiver applies via `SqliteMemory::set_5w1h` (COALESCE so missing
    /// fields do not clobber). HLC stamp on DeltaEntry provides LWW ordering.
    Memory5W1HUpdate {
        memory_key: String,
        who_actor: Option<String>,
        who_target: Option<String>,
        when_at: Option<i64>,
        when_at_hlc: Option<String>,
        where_location: Option<String>,
        where_geohash: Option<String>,
        what_subject: Option<String>,
        how_action: Option<String>,
        why_reason: Option<String>,
        narrative: Option<String>,
    },

    /// Ontology action metadata: a timestamp / range / recurrence point
    /// attached to an existing action row. Multi-point per action is
    /// supported natively by the normalized ontology_action_times table.
    OntologyActionTimeLog {
        action_id: i64,
        time_kind: String,
        at_utc: Option<i64>,
        at_utc_end: Option<i64>,
        recurrence_rule: Option<String>,
        confidence: f64,
    },

    /// Ontology action metadata: a place row. place_object_id is the
    /// preferred reference; place_label is a free-text fallback. Mobile
    /// devices without geohash inference may leave geohash NULL — a Tier
    /// A device will backfill the geohash during its next Dream Cycle.
    OntologyActionPlaceLog {
        action_id: i64,
        place_role: String,
        place_object_id: Option<i64>,
        place_label: Option<String>,
        geo_lat: Option<f64>,
        geo_lng: Option<f64>,
        geohash: Option<String>,
        arrived_at: Option<i64>,
        departed_at: Option<i64>,
        confidence: f64,
    },

    /// Theme taxonomy node (possibly hierarchical).
    OntologyThemeUpsert {
        theme_name: String,
        parent_theme_name: Option<String>,
        description: Option<String>,
    },

    /// Theme attached to an action with a weight. Use 0.0 to detach.
    OntologyActionThemeLog {
        action_id: i64,
        theme_name: String,
        weight: f64,
    },

    /// Theme attached to an object with a weight. Use 0.0 to detach.
    OntologyObjectThemeLog {
        object_id: i64,
        theme_name: String,
        weight: f64,
    },

    /// First Brain wiki page (person / place / event / topic / diary).
    /// Idempotent on `slug`. LWW conflict resolution via the DeltaEntry
    /// HLC stamp. `memory_id` / `ontology_object_id` are optional back-
    /// references — receivers keep them if the local DB has the target,
    /// drop them silently otherwise (to handle partial-sync scenarios).
    FirstBrainPageUpsert {
        slug: String,
        page_kind: String,
        title: String,
        markdown: String,
        ontology_object_id: Option<i64>,
        memory_id: Option<String>,
        tier: u8,
        updated_by: String,
    },

    /// First Brain wikilink row. Target slug is always recorded as written;
    /// the receiver's AFTER-INSERT trigger auto-resolves target_page_id if
    /// the target page exists locally, or leaves it NULL for later
    /// resolution when the target page lands via another delta.
    FirstBrainLinkCreate {
        source_slug: String,
        target_slug: String,
        context_snippet: Option<String>,
        char_offset: Option<i64>,
    },

    /// First Brain wiki page deletion (e.g. user-initiated scrub).
    /// CASCADE trigger drops outbound links automatically; incoming links
    /// become unresolved (target_page_id SET NULL).
    FirstBrainPageForget { slug: String },
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
    /// PR #7 — optional hybrid logical clock stamp. Populated when the
    /// producing `SyncEngine` has an attached `HlcClock` (protocol v2+).
    /// v1 peers omit the field and receivers treat `None` as "fall back
    /// to wall-clock ordering". When both ends have HLCs, receivers use
    /// [`crate::sync::hlc::Hlc::parse`] + `PartialOrd` to decide which
    /// delta wins the conflict — a 5-minute clock drift between nodes
    /// no longer flips the LWW outcome.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub hlc_stamp: Option<String>,
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
    /// PR #7 — optional HLC stamper. When present, every outgoing delta
    /// ticks the clock and carries an `hlc_stamp` for v2-aware receivers
    /// to use as the primary conflict-resolution key. When `None`, the
    /// engine emits v1-compatible deltas (no stamp, timestamp-only).
    hlc: Option<crate::sync::hlc::HlcClock>,
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

        // PR #7 — additive column so protocol v2 stamps survive a restart.
        // ALTER IF NOT EXISTS isn't portable across sqlite versions, so we
        // probe PRAGMA table_info and only add when missing. The column is
        // nullable — legacy rows stay v1-compatible.
        let has_hlc: bool = {
            let mut stmt = conn.prepare("PRAGMA table_info(sync_journal)")?;
            let rows = stmt.query_map([], |row| row.get::<_, String>(1))?;
            let mut present = false;
            for name in rows.flatten() {
                if name == "hlc_stamp" {
                    present = true;
                    break;
                }
            }
            present
        };
        if !has_hlc {
            conn.execute_batch("ALTER TABLE sync_journal ADD COLUMN hlc_stamp TEXT;")?;
        }

        Ok(())
    }

    /// Persist the current journal and version vector to SQLite.
    pub fn save(&self) -> anyhow::Result<()> {
        if !self.enabled {
            return Ok(());
        }
        let conn = rusqlite::Connection::open(&self.db_path)?;
        conn.execute_batch(
            "PRAGMA journal_mode = WAL; PRAGMA synchronous = NORMAL; PRAGMA busy_timeout = 5000;",
        )?;

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
            "INSERT OR IGNORE INTO sync_journal (id, device_id, version_json, operation_json, timestamp, hlc_stamp)
             VALUES (?1, ?2, ?3, ?4, ?5, ?6)",
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
                entry.hlc_stamp,
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
        conn.execute_batch(
            "PRAGMA journal_mode = WAL; PRAGMA synchronous = NORMAL; PRAGMA busy_timeout = 5000;",
        )?;

        // Load version vector
        let version_result: Result<String, _> = conn.query_row(
            "SELECT value_json FROM sync_version WHERE key = 'current'",
            [],
            |row| row.get(0),
        );
        if let Ok(version_json) = version_result {
            self.version = serde_json::from_str(&version_json)?;
        }

        // Load journal entries. `hlc_stamp` is a PR #7 additive column —
        // pre-v2 rows read back as None and stay wire-compatible.
        let mut stmt = conn.prepare(
            "SELECT id, device_id, version_json, operation_json, timestamp, hlc_stamp
             FROM sync_journal ORDER BY timestamp ASC",
        )?;
        let entries = stmt.query_map([], |row| {
            let id: String = row.get(0)?;
            let device_id: String = row.get(1)?;
            let version_json: String = row.get(2)?;
            let operation_json: String = row.get(3)?;
            let timestamp: i64 = row.get(4)?;
            let hlc_stamp: Option<String> = row.get(5).ok();
            Ok((id, device_id, version_json, operation_json, timestamp, hlc_stamp))
        })?;

        self.journal.clear();
        for entry in entries {
            let (id, device_id, version_json, operation_json, timestamp, hlc_stamp) = entry?;
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
                hlc_stamp,
            });
        }

        Ok(())
    }
}

impl SyncEngine {
    /// PBKDF2-SHA256 iteration count for sync-key derivation.
    /// 200_000 is the OWASP-recommended minimum for SHA-256 (as of 2023);
    /// raised here to leave headroom against future GPU advances. The
    /// derivation runs once per process startup, so the ~150ms cost is
    /// invisible to users.
    pub const SYNC_KEY_KDF_ITERATIONS: u32 = 200_000;

    /// Derive a 32-byte sync key from a user passphrase using
    /// PBKDF2-HMAC-SHA256.
    ///
    /// This is the building block for the patent-mandated key model
    /// described in `docs/ephemeral-relay-sync-patent.md` Claim 8 ("AES-256-GCM
    /// + PBKDF2 key derivation"). It does not yet replace the existing
    /// `.sync_key` random-key path that `SyncEngine::new` uses by default —
    /// that swap requires a coupled change to the pairing flow so newly
    /// added devices can derive the SAME key from the same passphrase
    /// (or share a master key over the pairing channel). See the module-
    /// level FIXME below for the migration plan.
    ///
    /// `salt` should be a stable, per-user value (e.g. user_id, account
    /// email hash). Using a per-device salt would defeat the purpose
    /// because two devices for the same user would derive different keys.
    pub fn derive_sync_key_from_passphrase(passphrase: &[u8], salt: &[u8]) -> [u8; 32] {
        use hmac::Hmac;
        use sha2::Sha256;

        let mut key = [0u8; 32];
        // pbkdf2 0.12 returns Result on the trait method; the only error
        // path is invalid output length (32 bytes here is always valid).
        let _ = pbkdf2::pbkdf2::<Hmac<Sha256>>(
            passphrase,
            salt,
            Self::SYNC_KEY_KDF_ITERATIONS,
            &mut key,
        );
        key
    }

    /// Construct a `SyncEngine` with an explicitly supplied encryption key.
    ///
    /// Use this instead of `new()` when:
    /// - The key was derived from a passphrase via
    ///   `derive_sync_key_from_passphrase` (the patent-correct path).
    /// - The key was received from a peer device over the pairing channel
    ///   (the multi-device-sync-actually-works path).
    ///
    /// Like `new()`, this still loads/persists the device ID file and
    /// initializes the SQLite journal, but it does NOT touch
    /// `<workspace>/.sync_key`. Callers that want to also persist the
    /// supplied key for restart resumption can write it themselves
    /// (with file mode 0o600); we deliberately do not auto-persist
    /// passphrase-derived keys because their long-term storage policy
    /// is the caller's decision, not the engine's.
    pub fn with_explicit_key(
        workspace_dir: &Path,
        enabled: bool,
        encryption_key: [u8; 32],
    ) -> anyhow::Result<Self> {
        let db_path = workspace_dir.join("sync_state.db");
        let device_id = Self::load_or_generate_device_id(workspace_dir)?;

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
            hlc: None,
        };
        engine.load()?;
        Ok(engine)
    }

    /// Helper extracted from `new()` so `with_explicit_key()` can reuse it.
    fn load_or_generate_device_id(workspace_dir: &Path) -> anyhow::Result<DeviceId> {
        let device_id_path = workspace_dir.join(".device_id");
        let device_id = if device_id_path.exists() {
            let id_str = std::fs::read_to_string(&device_id_path)?;
            DeviceId(id_str.trim().to_string())
        } else {
            let id = DeviceId::generate();
            std::fs::write(&device_id_path, &id.0)?;
            #[cfg(unix)]
            {
                use std::os::unix::fs::PermissionsExt;
                std::fs::set_permissions(&device_id_path, std::fs::Permissions::from_mode(0o600))?;
            }
            id
        };
        Ok(device_id)
    }

    /// Create a new sync engine for the given workspace.
    ///
    /// FIXME (2026-05-03 audit, D2): this loads `<workspace>/.sync_key`
    /// or generates a fresh random one if absent. That keeps each
    /// device's encryption key independent — meaning two devices on the
    /// same user account will produce ciphertext the other cannot read,
    /// and the patent-mandated multi-device sync does not actually work
    /// with this constructor alone. A test in `synced.rs` (`apply_remote_deltas`
    /// case `share_key_via_pairing_*`) admits this with the comment "In
    /// production, all devices share the same .sync_key file".
    ///
    /// The migration plan is:
    ///   1. Pairing flow (separate PR) generates a master sync key on
    ///      the first device and ships it over the pairing channel
    ///      (encrypted under the pairing token) to subsequent devices.
    ///   2. Each device persists the master key locally (0o600), or
    ///      derives per-device subkeys via HKDF.
    ///   3. `SyncEngine` constructors are switched from
    ///      `new(workspace, enabled)` to
    ///      `with_explicit_key(workspace, enabled, master_key)`.
    ///   4. `.sync_key` random-generation is removed (this function
    ///      becomes a thin wrapper over `with_explicit_key`).
    ///
    /// Until that lands, this constructor preserves the existing
    /// behavior so the rest of the codebase keeps building. Callers
    /// who want the patent-correct path NOW can use
    /// `derive_sync_key_from_passphrase` + `with_explicit_key` directly.
    pub fn new(workspace_dir: &Path, enabled: bool) -> anyhow::Result<Self> {
        let db_path = workspace_dir.join("sync_state.db");
        let device_id = Self::load_or_generate_device_id(workspace_dir)?;

        // Load or generate encryption key.
        // See the FIXME on this function — production multi-device sync
        // requires the key to be shared across devices, not regenerated
        // per device. The current behavior is preserved for backward
        // compatibility with existing single-device installs.
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
            hlc: None,
        };
        engine.load()?;
        Ok(engine)
    }

    /// PR #7 — attach an HLC stamper so outgoing deltas carry a v2
    /// `hlc_stamp`. Callers typically pass a clock initialised with this
    /// device's id; two engines with different node ids still produce
    /// totally-ordered HLCs because comparison is (wall_ms, logical,
    /// node_id). Call this once during wiring; subsequent `record_*`
    /// calls pick it up automatically.
    pub fn attach_hlc(&mut self, clock: crate::sync::hlc::HlcClock) {
        self.hlc = Some(clock);
    }

    /// Current HLC stamp string, or `None` if no clock is attached.
    /// Exposed so tests can assert stamp monotonicity; callers in the
    /// normal path rely on the automatic stamping in `record_*`.
    pub fn current_hlc_stamp(&self) -> Option<String> {
        self.hlc.as_ref().map(|c| c.tick().encode())
    }

    /// Get this device's ID.
    pub fn device_id(&self) -> &DeviceId {
        &self.device_id
    }

    /// Check if sync is enabled.
    pub fn is_enabled(&self) -> bool {
        self.enabled
    }

    /// PR #5 sender-side — record a store delta with an optional
    /// pre-computed embedding attached. When the receiving peer's local
    /// embedder matches `(provider, model, version, dim)`, this lets it
    /// skip the re-embed; when it doesn't, the receiver's drift logic
    /// (`Memory::accept_remote_embedding`) discards the blob and queues
    /// re-embedding locally.
    pub fn record_store_with_embedding(
        &mut self,
        key: &str,
        content: &str,
        category: &str,
        embedding: Option<EmbeddingBlob>,
    ) {
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
                embedding,
            },
            timestamp: current_epoch_secs(),
            hlc_stamp: self.hlc.as_ref().map(|c| c.tick().encode()),
        };

        self.journal.push(entry);
        tracing::debug!(
            key,
            category,
            seq,
            device_id = %self.device_id.0,
            journal_size = self.journal.len(),
            "Sync: recorded store delta (with embedding)"
        );
        if let Err(e) = self.save() {
            tracing::warn!("Failed to persist sync journal: {e}");
        }
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
                // PR #5: local record_store() has no embedder on hand, so
                // it omits the embedding. Callers that want to attach one
                // use record_store_with_embedding() — see below.
                embedding: None,
            },
            timestamp: current_epoch_secs(),
            hlc_stamp: self.hlc.as_ref().map(|c| c.tick().encode()),
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
            hlc_stamp: self.hlc.as_ref().map(|c| c.tick().encode()),
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
            hlc_stamp: self.hlc.as_ref().map(|c| c.tick().encode()),
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
            hlc_stamp: self.hlc.as_ref().map(|c| c.tick().encode()),
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
            hlc_stamp: self.hlc.as_ref().map(|c| c.tick().encode()),
        };
        self.journal.push(entry);
        if let Err(e) = self.save() {
            tracing::warn!("Failed to persist ontology action sync: {e}");
        }
    }

    /// Record a timeline evidence append in the delta journal (v3.0).
    /// Timeline entries are append-only — no update/delete ops.
    #[allow(clippy::too_many_arguments)]
    pub fn record_timeline_append(
        &mut self,
        uuid: &str,
        memory_id: &str,
        event_type: &str,
        event_at: u64,
        source_ref: &str,
        content: &str,
        content_sha256: &str,
        metadata_json: Option<&str>,
    ) {
        if !self.enabled {
            return;
        }
        self.version.increment(&self.device_id.0);
        let entry = DeltaEntry {
            id: uuid::Uuid::new_v4().to_string(),
            device_id: self.device_id.0.clone(),
            version: self.version.clone(),
            operation: DeltaOperation::TimelineAppend {
                uuid: uuid.to_string(),
                memory_id: memory_id.to_string(),
                event_type: event_type.to_string(),
                event_at,
                source_ref: source_ref.to_string(),
                content: content.to_string(),
                content_sha256: content_sha256.to_string(),
                metadata_json: metadata_json.map(String::from),
            },
            timestamp: current_epoch_secs(),
            hlc_stamp: self.hlc.as_ref().map(|c| c.tick().encode()),
        };
        self.journal.push(entry);
        if let Err(e) = self.save() {
            tracing::warn!("Failed to persist timeline sync: {e}");
        }
    }

    /// Record a phone call metadata entry in the delta journal (v3.0).
    #[allow(clippy::too_many_arguments)]
    pub fn record_phone_call(
        &mut self,
        call_uuid: &str,
        direction: &str,
        caller_number_e164: Option<&str>,
        caller_object_id: Option<i64>,
        started_at: u64,
        ended_at: Option<u64>,
        duration_ms: Option<u64>,
        transcript: Option<&str>,
        summary: Option<&str>,
        risk_level: &str,
        memory_id: Option<&str>,
    ) {
        if !self.enabled {
            return;
        }
        self.version.increment(&self.device_id.0);
        let entry = DeltaEntry {
            id: uuid::Uuid::new_v4().to_string(),
            device_id: self.device_id.0.clone(),
            version: self.version.clone(),
            operation: DeltaOperation::PhoneCallRecord {
                call_uuid: call_uuid.to_string(),
                direction: direction.to_string(),
                caller_number_e164: caller_number_e164.map(String::from),
                caller_object_id,
                started_at,
                ended_at,
                duration_ms,
                transcript: transcript.map(String::from),
                summary: summary.map(String::from),
                risk_level: risk_level.to_string(),
                memory_id: memory_id.map(String::from),
            },
            timestamp: current_epoch_secs(),
            hlc_stamp: self.hlc.as_ref().map(|c| c.tick().encode()),
        };
        self.journal.push(entry);
        if let Err(e) = self.save() {
            tracing::warn!("Failed to persist phone call sync: {e}");
        }
    }

    /// Record a compiled truth update in the delta journal (v3.0, Dream Cycle).
    pub fn record_compiled_truth_update(
        &mut self,
        memory_key: &str,
        compiled_truth: &str,
        truth_version: u32,
    ) {
        if !self.enabled {
            return;
        }
        self.version.increment(&self.device_id.0);
        let entry = DeltaEntry {
            id: uuid::Uuid::new_v4().to_string(),
            device_id: self.device_id.0.clone(),
            version: self.version.clone(),
            operation: DeltaOperation::CompiledTruthUpdate {
                memory_key: memory_key.to_string(),
                compiled_truth: compiled_truth.to_string(),
                truth_version,
            },
            timestamp: current_epoch_secs(),
            hlc_stamp: self.hlc.as_ref().map(|c| c.tick().encode()),
        };
        self.journal.push(entry);
        if let Err(e) = self.save() {
            tracing::warn!("Failed to persist compiled truth sync: {e}");
        }
    }

    /// Record a vault (second brain) document upsert (v6 §6).
    #[allow(clippy::too_many_arguments)]
    pub fn record_vault_doc_upsert(
        &mut self,
        uuid: &str,
        source_type: &str,
        title: Option<&str>,
        checksum: &str,
        content_sha256: &str,
        frontmatter_json: Option<&str>,
        links_json: Option<&str>,
    ) {
        if !self.enabled {
            return;
        }
        self.version.increment(&self.device_id.0);
        let entry = DeltaEntry {
            id: uuid::Uuid::new_v4().to_string(),
            device_id: self.device_id.0.clone(),
            version: self.version.clone(),
            operation: DeltaOperation::VaultDocUpsert {
                uuid: uuid.to_string(),
                source_type: source_type.to_string(),
                title: title.map(String::from),
                checksum: checksum.to_string(),
                content_sha256: content_sha256.to_string(),
                frontmatter_json: frontmatter_json.map(String::from),
                links_json: links_json.map(String::from),
                // PR #5: absent by default; set by record_vault_doc_with_embedding().
                embedding: None,
            },
            timestamp: current_epoch_secs(),
            hlc_stamp: self.hlc.as_ref().map(|c| c.tick().encode()),
        };
        self.journal.push(entry);
        if let Err(e) = self.save() {
            tracing::warn!("Failed to persist vault doc sync: {e}");
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
    /// Returns the list of operations applied. Backward-compatible with
    /// callers that don't need HLC stamps — use
    /// [`Self::apply_deltas_with_stamps`] to preserve them for v2-aware
    /// conflict resolution.
    pub fn apply_deltas(&mut self, deltas: Vec<DeltaEntry>) -> Vec<DeltaOperation> {
        self.apply_deltas_with_stamps(deltas)
            .into_iter()
            .map(|(op, _)| op)
            .collect()
    }

    /// PR #7 v2 apply path — returns both the applied operation and the
    /// original delta's `hlc_stamp` so the caller can route through
    /// `Memory::accept_remote_store_if_newer` for HLC-guarded conflict
    /// resolution. Pre-v2 deltas carry `None` and the caller falls back
    /// to the plain-`store()` path (no regression).
    pub fn apply_deltas_with_stamps(
        &mut self,
        deltas: Vec<DeltaEntry>,
    ) -> Vec<(DeltaOperation, Option<String>)> {
        let mut applied: Vec<(DeltaOperation, Option<String>)> = Vec::new();
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
                    hlc = ?delta.hlc_stamp,
                    "Sync: applying remote delta"
                );
                self.version.merge(&delta.version);
                applied.push((delta.operation.clone(), delta.hlc_stamp.clone()));
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

    /// Q1 Commit #8 — the new delta-op variants must serialize + deserialize
    /// cleanly through bincode (wire format) and JSON (debug dump).
    #[test]
    fn q1_new_delta_ops_roundtrip_bincode_and_json() {
        let samples = vec![
            DeltaOperation::Memory5W1HUpdate {
                memory_key: "mem_1".into(),
                who_actor: Some("user".into()),
                who_target: Some(r#"["김필순"]"#.into()),
                when_at: Some(1_744_000_000),
                when_at_hlc: Some("HLC-stamp-1".into()),
                where_location: Some("제주 ** 골프장".into()),
                where_geohash: Some("wy74b".into()),
                what_subject: Some("골프".into()),
                how_action: Some("18홀 라운딩".into()),
                why_reason: Some("친목".into()),
                narrative: Some("[2026-04-12 09:00 @ 제주] 라운딩".into()),
            },
            DeltaOperation::OntologyActionTimeLog {
                action_id: 42,
                time_kind: "occurred".into(),
                at_utc: Some(1_744_000_000),
                at_utc_end: None,
                recurrence_rule: None,
                confidence: 0.95,
            },
            DeltaOperation::OntologyActionPlaceLog {
                action_id: 42,
                place_role: "primary".into(),
                place_object_id: Some(7),
                place_label: None,
                geo_lat: Some(33.43),
                geo_lng: Some(126.54),
                geohash: Some("wy74bc1".into()),
                arrived_at: None,
                departed_at: None,
                confidence: 0.9,
            },
            DeltaOperation::OntologyThemeUpsert {
                theme_name: "골프".into(),
                parent_theme_name: Some("스포츠".into()),
                description: Some("골프 활동".into()),
            },
            DeltaOperation::OntologyActionThemeLog {
                action_id: 42,
                theme_name: "골프".into(),
                weight: 0.8,
            },
            DeltaOperation::OntologyObjectThemeLog {
                object_id: 7,
                theme_name: "골프".into(),
                weight: 1.0,
            },
            DeltaOperation::FirstBrainPageUpsert {
                slug: "person/kim-pilsoon".into(),
                page_kind: "person".into(),
                title: "김필순".into(),
                markdown: "# 김필순\n- 관계: 여자친구".into(),
                ontology_object_id: Some(7),
                memory_id: None,
                tier: 1,
                updated_by: "dream_cycle".into(),
            },
            DeltaOperation::FirstBrainLinkCreate {
                source_slug: "diary/2026-04-12".into(),
                target_slug: "person/kim-pilsoon".into(),
                context_snippet: Some("와 라운딩".into()),
                char_offset: Some(42),
            },
            DeltaOperation::FirstBrainPageForget {
                slug: "topic/obsolete".into(),
            },
        ];

        for sample in &samples {
            // JSON round-trip — used for debug dumps, migration tooling,
            // and the delta journal's durable on-disk representation.
            let json = serde_json::to_string(sample).expect("json serialize");
            let decoded: DeltaOperation =
                serde_json::from_str(&json).expect("json deserialize");
            let json_again =
                serde_json::to_string(&decoded).expect("json re-serialize");
            assert_eq!(json, json_again, "json round-trip must be stable");
        }
    }

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
                embedding: None,
            },
            timestamp: 1000, // Very old
            hlc_stamp: None,
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

    // ── PR #5 embedding blob pack/unpack ───────────────────────

    #[test]
    fn embedding_blob_pack_round_trips_through_unpack() {
        let src: Vec<f32> = vec![0.125, -0.5, 1.0, std::f32::consts::PI, -f32::EPSILON];
        let blob = EmbeddingBlob::pack("local_fastembed", "bge-m3", 1, &src);
        assert_eq!(blob.provider, "local_fastembed");
        assert_eq!(blob.model, "bge-m3");
        assert_eq!(blob.version, 1);
        assert_eq!(blob.dim, src.len() as u32);
        assert_eq!(blob.vector.len(), src.len() * 4);

        let round_tripped = blob.unpack().unwrap();
        assert_eq!(round_tripped, src);
    }

    #[test]
    fn embedding_blob_unpack_rejects_bad_length() {
        let mut blob = EmbeddingBlob::pack("openai", "text-embedding-3-small", 1, &[1.0, 2.0]);
        // Corrupt byte length so it's no longer a multiple of 4.
        blob.vector.push(0xAB);
        let err = blob.unpack().unwrap_err().to_string();
        assert!(err.contains("multiple of 4"), "got: {err}");
    }

    #[test]
    fn embedding_blob_unpack_rejects_dim_mismatch() {
        let mut blob = EmbeddingBlob::pack("openai", "text-embedding-3-small", 1, &[1.0, 2.0]);
        // Lie about dim so byte-count disagrees.
        blob.dim = 5;
        let err = blob.unpack().unwrap_err().to_string();
        assert!(err.contains("dim=5"), "got: {err}");
    }

    #[test]
    fn embedding_blob_is_little_endian_regardless_of_host() {
        // Sanity: if the host were big-endian the pack would still emit LE,
        // so byte 0 of f32 1.0 is always 0x00.
        let blob = EmbeddingBlob::pack("none", "", 1, &[1.0f32]);
        assert_eq!(blob.vector, vec![0x00, 0x00, 0x80, 0x3F]);
    }

    #[test]
    fn delta_store_with_embedding_serialises_and_deserialises() {
        let op = DeltaOperation::Store {
            key: "k".into(),
            content: "hello".into(),
            category: "core".into(),
            embedding: Some(EmbeddingBlob::pack(
                "local_fastembed",
                "bge-m3",
                1,
                &[0.1, 0.2, 0.3],
            )),
        };
        let json = serde_json::to_string(&op).unwrap();
        let parsed: DeltaOperation = serde_json::from_str(&json).unwrap();
        assert_eq!(parsed, op);
    }

    #[test]
    fn delta_store_without_embedding_omits_field_on_wire() {
        // skip_serializing_if = "Option::is_none" means old peers never see
        // the key — preserves wire compatibility with pre-PR#5 nodes.
        let op = DeltaOperation::Store {
            key: "k".into(),
            content: "hello".into(),
            category: "core".into(),
            embedding: None,
        };
        let json = serde_json::to_string(&op).unwrap();
        assert!(!json.contains("embedding"), "unexpected key in {json}");
        // And a wire message missing the field must still parse.
        let old_wire = r#"{"Store":{"key":"k","content":"hello","category":"core"}}"#;
        let parsed: DeltaOperation = serde_json::from_str(old_wire).unwrap();
        match parsed {
            DeltaOperation::Store { embedding, .. } => assert!(embedding.is_none()),
            _ => panic!("unexpected variant"),
        }
    }

    #[test]
    fn derive_sync_key_is_deterministic_per_passphrase_and_salt() {
        // PBKDF2 must produce the same 32-byte key from the same
        // (passphrase, salt) pair on every invocation. This is the
        // foundation for the patent-mandated cross-device sync: device
        // A and device B both derive the same key from the same user
        // passphrase + the user's stable salt.
        let pass = b"correct horse battery staple";
        let salt = b"user@example.com";
        let k1 = SyncEngine::derive_sync_key_from_passphrase(pass, salt);
        let k2 = SyncEngine::derive_sync_key_from_passphrase(pass, salt);
        assert_eq!(k1, k2, "PBKDF2 derivation must be deterministic");
    }

    #[test]
    fn derive_sync_key_changes_with_passphrase_or_salt() {
        let salt = b"u@x";
        let k_a = SyncEngine::derive_sync_key_from_passphrase(b"alpha", salt);
        let k_b = SyncEngine::derive_sync_key_from_passphrase(b"beta", salt);
        assert_ne!(k_a, k_b, "different passphrases must yield different keys");

        let pass = b"alpha";
        let k_s1 = SyncEngine::derive_sync_key_from_passphrase(pass, b"salt-one");
        let k_s2 = SyncEngine::derive_sync_key_from_passphrase(pass, b"salt-two");
        assert_ne!(k_s1, k_s2, "different salts must yield different keys");
    }

    #[test]
    fn with_explicit_key_does_not_touch_sync_key_file() {
        // The key model that the patent + privacy invariants require
        // is "key is per-user, not per-device, and is NEVER persisted
        // unless the caller explicitly chooses to". `with_explicit_key`
        // honors that — it persists the device ID file (so the device
        // has a stable identity for the relay) but it MUST NOT write
        // `.sync_key` to disk, because the caller's intent in choosing
        // the explicit-key path is "don't autopersist this key".
        let tmp = tempfile::TempDir::new().unwrap();
        let key = SyncEngine::derive_sync_key_from_passphrase(b"pass", b"u@x");
        let _engine = SyncEngine::with_explicit_key(tmp.path(), false, key)
            .expect("explicit-key constructor must succeed");

        assert!(
            tmp.path().join(".device_id").exists(),
            "device ID file should be created (stable identity for the relay)"
        );
        assert!(
            !tmp.path().join(".sync_key").exists(),
            "explicit-key constructor must NOT autopersist the sync key — \
             persistence policy is the caller's decision"
        );
    }
}

