//! Audit logging for security events
//!
//! Each audit entry is chained via a Merkle hash: `entry_hash = SHA-256(prev_hash || canonical_json)`.
//! This makes the trail tamper-evident — modifying any entry invalidates all subsequent hashes.
//!
//! Storage: SQLite database at `workspace/audit/audit.db`.

use anyhow::{Context, Result, bail};
use chrono::{DateTime, Utc};
use rusqlite::{Connection, params};
use serde::{Deserialize, Serialize};
use sha2::{Digest, Sha256};
use std::path::{Path, PathBuf};
use uuid::Uuid;
use daemonclaw_config::schema::AuditConfig;

/// Well-known seed for the genesis entry's `prev_hash`.
const GENESIS_PREV_HASH: &str = "0000000000000000000000000000000000000000000000000000000000000000";

/// RETIRED (Track H): audit chain is append-only. Retention is now an out-of-process operation.
#[cfg(test)]
const MAX_AUDIT_EVENTS: i64 = 500_000;
/// RETIRED (Track H): audit chain is append-only. Retention is now an out-of-process operation.
#[cfg(test)]
const RETENTION_CHECK_INTERVAL: u64 = 1000;

/// Audit event types
#[derive(Debug, Clone, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum AuditEventType {
    CommandExecution,
    FileAccess,
    ConfigChange,
    AuthSuccess,
    AuthFailure,
    PolicyViolation,
    SecurityEvent,
    ChainTruncation,
    ChainBoundary,
    TaskTransition,
}

/// Actor information (who performed the action)
#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct Actor {
    pub channel: String,
    pub user_id: Option<String>,
    pub username: Option<String>,
}

/// Action information (what was done)
#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct Action {
    pub command: Option<String>,
    pub risk_level: Option<String>,
    pub approved: bool,
    pub allowed: bool,
}

/// Execution result
#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct ExecutionResult {
    pub success: bool,
    pub exit_code: Option<i32>,
    pub duration_ms: Option<u64>,
    pub error: Option<String>,
}

/// Security context
#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct SecurityContext {
    pub policy_violation: bool,
    pub rate_limit_remaining: Option<u32>,
    pub sandbox_backend: Option<String>,
}

/// Complete audit event with Merkle hash-chain fields.
#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct AuditEvent {
    pub timestamp: DateTime<Utc>,
    pub event_id: String,
    pub event_type: AuditEventType,
    pub actor: Option<Actor>,
    pub action: Option<Action>,
    pub result: Option<ExecutionResult>,
    pub security: SecurityContext,

    /// Monotonically increasing sequence number.
    #[serde(default)]
    pub sequence: u64,
    /// SHA-256 hash of the previous entry (genesis uses [`GENESIS_PREV_HASH`]).
    #[serde(default)]
    pub prev_hash: String,
    /// SHA-256 hash of (`prev_hash` || canonical JSON of this entry's content fields).
    #[serde(default)]
    pub entry_hash: String,

    /// Optional HMAC-SHA256 signature over entry_hash (present only when sign_events enabled)
    #[serde(skip_serializing_if = "Option::is_none", default)]
    pub signature: Option<String>,
}

impl AuditEvent {
    /// Create a new audit event
    pub fn new(event_type: AuditEventType) -> Self {
        Self {
            timestamp: Utc::now(),
            event_id: Uuid::new_v4().to_string(),
            event_type,
            actor: None,
            action: None,
            result: None,
            security: SecurityContext {
                policy_violation: false,
                rate_limit_remaining: None,
                sandbox_backend: None,
            },
            sequence: 0,
            prev_hash: String::new(),
            entry_hash: String::new(),
            signature: None,
        }
    }

    /// Set the actor
    pub fn with_actor(
        mut self,
        channel: String,
        user_id: Option<String>,
        username: Option<String>,
    ) -> Self {
        self.actor = Some(Actor {
            channel,
            user_id,
            username,
        });
        self
    }

    /// Set the action
    pub fn with_action(
        mut self,
        command: String,
        risk_level: String,
        approved: bool,
        allowed: bool,
    ) -> Self {
        self.action = Some(Action {
            command: Some(command),
            risk_level: Some(risk_level),
            approved,
            allowed,
        });
        self
    }

    /// Set the result
    pub fn with_result(
        mut self,
        success: bool,
        exit_code: Option<i32>,
        duration_ms: u64,
        error: Option<String>,
    ) -> Self {
        self.result = Some(ExecutionResult {
            success,
            exit_code,
            duration_ms: Some(duration_ms),
            error,
        });
        self
    }

    /// Set security context
    pub fn with_security(mut self, sandbox_backend: Option<String>) -> Self {
        self.security.sandbox_backend = sandbox_backend;
        self
    }
}

/// Compute the SHA-256 entry hash: `H(prev_hash || content_json)`.
///
/// `content_json` is the canonical JSON of the event *without* the chain fields
/// (`sequence`, `prev_hash`, `entry_hash`), so the hash covers only the payload.
fn compute_entry_hash(prev_hash: &str, event: &AuditEvent) -> String {
    // Build a canonical representation of the content fields only.
    let content = serde_json::json!({
        "timestamp": event.timestamp,
        "event_id": event.event_id,
        "event_type": event.event_type,
        "actor": event.actor,
        "action": event.action,
        "result": event.result,
        "security": event.security,
        "sequence": event.sequence,
    });
    let content_json = serde_json::to_string(&content).expect("serialize canonical content");

    let mut hasher = Sha256::new();
    hasher.update(prev_hash.as_bytes());
    hasher.update(content_json.as_bytes());
    hex::encode(hasher.finalize())
}

/// Chain tip state recovered from the database.
struct ChainState {
    prev_hash: String,
    sequence: u64,
}

/// Audit logger backed by SQLite (`workspace/audit/audit.db`).
///
/// Chain integrity is guaranteed at the database level: each `log()` call
/// reads the current chain tip and appends atomically within a single
/// `BEGIN IMMEDIATE` transaction. This serializes concurrent writers
/// (both intra-process threads and inter-process instances) via SQLite's
/// WAL write lock, preventing chain forks.
pub struct AuditLogger {
    db_path: PathBuf,
    config: AuditConfig,
    signing_key: Option<Vec<u8>>,
}

/// Structured command execution details for audit logging.
#[derive(Debug, Clone)]
pub struct CommandExecutionLog<'a> {
    pub channel: &'a str,
    pub command: &'a str,
    pub risk_level: &'a str,
    pub approved: bool,
    pub allowed: bool,
    pub success: bool,
    pub duration_ms: u64,
}

impl AuditLogger {
    /// Create a new audit logger backed by SQLite.
    ///
    /// Opens (or creates) `workspace_dir/audit/audit.db`, sets WAL mode,
    /// and recovers chain state from the last entry.
    ///
    /// If `config.sign_events` is true, requires `DAEMONCLAW_AUDIT_SIGNING_KEY` env var
    /// to be set with a hex-encoded 32-byte key. Fails if key is missing or invalid.
    pub fn new(config: AuditConfig, workspace_dir: PathBuf) -> Result<Self> {
        let signing_key = if config.sign_events {
            let key_hex = std::env::var("DAEMONCLAW_AUDIT_SIGNING_KEY").map_err(|_| {
                anyhow::anyhow!("sign_events enabled but DAEMONCLAW_AUDIT_SIGNING_KEY not set")
            })?;

            let key_bytes = hex::decode(&key_hex)
                .map_err(|_| anyhow::anyhow!("DAEMONCLAW_AUDIT_SIGNING_KEY must be hex-encoded"))?;

            if key_bytes.len() != 32 {
                bail!(
                    "DAEMONCLAW_AUDIT_SIGNING_KEY must be 32 bytes (64 hex chars), got {}",
                    key_bytes.len()
                );
            }

            Some(key_bytes)
        } else {
            None
        };

        let audit_dir = workspace_dir.join("audit");
        std::fs::create_dir_all(&audit_dir)
            .with_context(|| format!("Failed to create audit directory: {}", audit_dir.display()))?;
        let db_path = audit_dir.join("audit.db");

        let conn = Self::open_conn(&db_path)?;
        conn.execute_batch(
            "CREATE TABLE IF NOT EXISTS audit_events (
                 id          INTEGER PRIMARY KEY AUTOINCREMENT,
                 event_json  TEXT NOT NULL,
                 event_type  TEXT NOT NULL,
                 timestamp   TEXT NOT NULL,
                 sequence    INTEGER NOT NULL,
                 entry_hash  TEXT NOT NULL
             );
             CREATE INDEX IF NOT EXISTS idx_audit_timestamp ON audit_events(timestamp);
             CREATE UNIQUE INDEX IF NOT EXISTS idx_audit_sequence_unique ON audit_events(sequence);
             CREATE INDEX IF NOT EXISTS idx_audit_type ON audit_events(event_type);",
        )
        .context("Failed to create audit_events table")?;

        Self::install_append_only_triggers(&conn)?;

        Ok(Self {
            db_path,
            config,
            signing_key,
        })
    }

    fn open_conn(db_path: &Path) -> Result<Connection> {
        let conn = Connection::open(db_path)
            .with_context(|| format!("Failed to open audit.db: {}", db_path.display()))?;
        conn.execute_batch(
            "PRAGMA journal_mode = WAL;
             PRAGMA synchronous = NORMAL;
             PRAGMA temp_store = MEMORY;
             PRAGMA busy_timeout = 5000;",
        )?;
        Ok(conn)
    }

    fn connect(&self) -> Result<Connection> {
        Self::open_conn(&self.db_path)
    }

    fn install_append_only_triggers(conn: &Connection) -> Result<()> {
        conn.execute_batch(
            "CREATE TRIGGER IF NOT EXISTS audit_no_update
                 BEFORE UPDATE ON audit_events
                 BEGIN
                     SELECT RAISE(ABORT, 'audit_events is append-only: UPDATE prohibited');
                 END;
             CREATE TRIGGER IF NOT EXISTS audit_no_delete
                 BEFORE DELETE ON audit_events
                 BEGIN
                     SELECT RAISE(ABORT, 'audit_events is append-only: DELETE prohibited — use retention API');
                 END;",
        )
        .context("Failed to install append-only triggers")
    }

    /// Path to the underlying database file.
    pub fn db_path(&self) -> &Path {
        &self.db_path
    }

    /// Compute HMAC-SHA256 signature over entry_hash when sign_events enabled.
    fn compute_signature(&self, entry_hash: &str) -> Result<Option<String>> {
        if let Some(ref key_bytes) = self.signing_key {
            use hmac::{Hmac, Mac};
            use sha2::Sha256;

            let mut mac = Hmac::<Sha256>::new_from_slice(key_bytes)
                .map_err(|_| anyhow::anyhow!("Invalid HMAC key length"))?;
            mac.update(entry_hash.as_bytes());

            Ok(Some(hex::encode(mac.finalize().into_bytes())))
        } else {
            Ok(None)
        }
    }

    /// Log an event (INSERT into audit.db with Merkle chain).
    ///
    /// The chain tip is read and the new entry inserted within a single
    /// `BEGIN IMMEDIATE` transaction, so concurrent writers (threads or
    /// separate processes) serialize via SQLite's write lock. This prevents
    /// chain forks where two writers read the same tip and produce
    /// duplicate sequence numbers.
    pub fn log(&self, event: &AuditEvent) -> Result<()> {
        if !self.config.enabled {
            return Ok(());
        }

        let conn = self.connect()?;

        // BEGIN IMMEDIATE acquires the write lock before any reads, ensuring
        // no other writer can read the same chain tip concurrently.
        conn.execute_batch("BEGIN IMMEDIATE")?;

        let result = (|| -> Result<u64> {
            let state = recover_chain_state(&conn);

            let mut ev = event.clone();
            ev.sequence = state.sequence;
            ev.prev_hash = state.prev_hash.clone();
            ev.entry_hash = compute_entry_hash(&state.prev_hash, &ev);
            ev.signature = self.compute_signature(&ev.entry_hash)?;

            let json = serde_json::to_string(&ev)?;
            let event_type_val = serde_json::to_value(&ev.event_type)?;
            conn.execute(
                "INSERT INTO audit_events (event_json, event_type, timestamp, sequence, entry_hash)
                 VALUES (?1, ?2, ?3, ?4, ?5)",
                params![
                    json,
                    event_type_val.as_str().unwrap_or("unknown"),
                    ev.timestamp.to_rfc3339(),
                    ev.sequence as i64,
                    ev.entry_hash,
                ],
            )?;

            Ok(ev.sequence + 1)
        })();

        match result {
            Ok(_next_seq) => {
                conn.execute_batch("COMMIT")?;
                Ok(())
            }
            Err(e) => {
                let _ = conn.execute_batch("ROLLBACK");
                Err(e)
            }
        }
    }

    /// Log a command execution event.
    pub fn log_command_event(&self, entry: CommandExecutionLog<'_>) -> Result<()> {
        let event = AuditEvent::new(AuditEventType::CommandExecution)
            .with_actor(entry.channel.to_string(), None, None)
            .with_action(
                entry.command.to_string(),
                entry.risk_level.to_string(),
                entry.approved,
                entry.allowed,
            )
            .with_result(entry.success, None, entry.duration_ms, None);

        self.log(&event)
    }

    /// Backward-compatible helper to log a command execution event.
    #[allow(clippy::too_many_arguments)]
    pub fn log_command(
        &self,
        channel: &str,
        command: &str,
        risk_level: &str,
        approved: bool,
        allowed: bool,
        success: bool,
        duration_ms: u64,
    ) -> Result<()> {
        self.log_command_event(CommandExecutionLog {
            channel,
            command,
            risk_level,
            approved,
            allowed,
            success,
            duration_ms,
        })
    }

    /// RETIRED (Track H): audit chain is append-only. Retention is now an out-of-process operation.
    ///
    /// Perform retention pruning as a recorded operation.
    ///
    /// Appends a `chain_truncation` event documenting the removed sequence
    /// range, then temporarily drops the append-only trigger to execute the
    /// DELETE, and reinstalls it. The chain always documents its own
    /// truncation — a clean `verify_chain` cannot result from silent removal.
    #[cfg(test)]
    fn run_retention(&self) -> Result<()> {
        let conn = self.connect()?;

        let total: i64 = conn.query_row(
            "SELECT COUNT(*) FROM audit_events",
            [],
            |r| r.get(0),
        )?;
        if total <= MAX_AUDIT_EVENTS {
            return Ok(());
        }

        let to_remove = total - MAX_AUDIT_EVENTS;
        let (min_seq, max_seq): (i64, i64) = conn.query_row(
            "SELECT MIN(sequence), MAX(sequence) FROM audit_events
             WHERE id NOT IN (SELECT id FROM audit_events ORDER BY id DESC LIMIT ?1)",
            params![MAX_AUDIT_EVENTS],
            |r| Ok((r.get(0)?, r.get(1)?)),
        )?;

        let truncation_event = AuditEvent::new(AuditEventType::ChainTruncation)
            .with_actor("system".to_string(), None, None)
            .with_action(
                format!("retention_prune: removed {to_remove} events, sequences {min_seq}..{max_seq}"),
                "system".to_string(),
                true,
                true,
            );
        self.log(&truncation_event)?;

        conn.execute_batch("BEGIN IMMEDIATE")?;
        let delete_result = (|| -> Result<()> {
            conn.execute_batch("DROP TRIGGER IF EXISTS audit_no_delete")?;
            conn.execute(
                "DELETE FROM audit_events WHERE id NOT IN
                 (SELECT id FROM audit_events ORDER BY id DESC LIMIT ?1)",
                params![MAX_AUDIT_EVENTS + 1], // +1 for the truncation event we just added
            )?;
            Self::install_append_only_triggers(&conn)?;
            Ok(())
        })();

        match delete_result {
            Ok(()) => {
                conn.execute_batch("COMMIT")?;
                tracing::info!(
                    removed = to_remove,
                    seq_range = format!("{min_seq}..{max_seq}").as_str(),
                    "Audit retention: pruned old events (recorded in chain)"
                );
                Ok(())
            }
            Err(e) => {
                let _ = conn.execute_batch("ROLLBACK");
                let _ = Self::install_append_only_triggers(&conn);
                Err(e)
            }
        }
    }

    /// Record a chain boundary — a permanent note that specific sequences
    /// were removed from the chain at a known point. Used to make past
    /// deletions (like fork cleanup) honest rather than invisible.
    pub fn record_chain_boundary(&self, description: &str) -> Result<()> {
        let event = AuditEvent::new(AuditEventType::ChainBoundary)
            .with_actor("system".to_string(), None, None)
            .with_action(
                description.to_string(),
                "administrative".to_string(),
                true,
                true,
            );
        self.log(&event)
    }
}

/// Recover chain state from the last entry in the database.
///
/// Returns the genesis state if the table is empty.
fn recover_chain_state(conn: &Connection) -> ChainState {
    let result: std::result::Result<String, _> = conn.query_row(
        "SELECT event_json FROM audit_events ORDER BY id DESC LIMIT 1",
        [],
        |row| row.get(0),
    );

    match result {
        Ok(json) => {
            if let Ok(entry) = serde_json::from_str::<AuditEvent>(&json) {
                ChainState {
                    prev_hash: entry.entry_hash,
                    sequence: entry.sequence + 1,
                }
            } else {
                ChainState {
                    prev_hash: GENESIS_PREV_HASH.to_string(),
                    sequence: 0,
                }
            }
        }
        Err(_) => ChainState {
            prev_hash: GENESIS_PREV_HASH.to_string(),
            sequence: 0,
        },
    }
}

/// Verify the integrity of an audit database's Merkle hash chain.
///
/// Reads every entry from audit.db and checks:
/// - Each `entry_hash` matches the recomputed `SHA-256(prev_hash || content)`.
/// - `prev_hash` links to the preceding entry (or the genesis seed for the first).
/// - Sequence numbers are contiguous starting from 0.
/// - If a record has a `signature` field and `DAEMONCLAW_AUDIT_SIGNING_KEY` is available,
///   verifies the HMAC-SHA256 signature over `entry_hash`.
///
/// Returns `Ok(entry_count)` on success, or an error describing the first violation.
pub fn verify_chain(db_path: &Path) -> Result<u64> {
    let conn = Connection::open(db_path)?;
    let mut stmt = conn.prepare(
        "SELECT event_json FROM audit_events ORDER BY id ASC",
    )?;

    let mut expected_prev_hash = GENESIS_PREV_HASH.to_string();
    let mut expected_sequence: u64 = 0;

    let signing_key = std::env::var("DAEMONCLAW_AUDIT_SIGNING_KEY")
        .ok()
        .and_then(|key_hex| hex::decode(&key_hex).ok())
        .filter(|key_bytes| key_bytes.len() == 32);

    let rows = stmt.query_map([], |row| row.get::<_, String>(0))?;

    for (row_idx, row_result) in rows.enumerate() {
        let json = row_result?;
        let entry: AuditEvent = serde_json::from_str(&json)?;

        if entry.sequence != expected_sequence {
            bail!(
                "sequence gap at row {}: expected {}, got {}",
                row_idx + 1,
                expected_sequence,
                entry.sequence
            );
        }

        if entry.prev_hash != expected_prev_hash {
            bail!(
                "prev_hash mismatch at row {} (sequence {}): expected {}, got {}",
                row_idx + 1,
                entry.sequence,
                expected_prev_hash,
                entry.prev_hash
            );
        }

        let recomputed = compute_entry_hash(&entry.prev_hash, &entry);
        if entry.entry_hash != recomputed {
            bail!(
                "entry_hash mismatch at row {} (sequence {}): expected {}, got {}",
                row_idx + 1,
                entry.sequence,
                recomputed,
                entry.entry_hash
            );
        }

        if let Some(ref signature) = entry.signature
            && let Some(ref key_bytes) = signing_key
        {
            use hmac::{Hmac, Mac};
            use sha2::Sha256;

            let mut mac = Hmac::<Sha256>::new_from_slice(key_bytes)
                .map_err(|_| anyhow::anyhow!("Invalid HMAC key length during verification"))?;
            mac.update(entry.entry_hash.as_bytes());
            let expected_sig = hex::encode(mac.finalize().into_bytes());

            if signature != &expected_sig {
                bail!(
                    "signature verification failed at row {} (sequence {}): signature mismatch",
                    row_idx + 1,
                    entry.sequence
                );
            }
        }

        expected_prev_hash = entry.entry_hash.clone();
        expected_sequence += 1;
    }

    Ok(expected_sequence)
}

// ── Anchor-chain anchoring (Track H) ─────────────────────────────
//
// A root-run systemd timer reads the chain tip (read-only) and appends
// it to a root-owned anchor file. The daemon has verification-only
// access. This makes tail-truncation detectable even if the daemon
// user has owner-write on audit.db.

/// A single witness anchor record written to the anchor file.
#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct AnchorRecord {
    pub sequence: u64,
    pub entry_hash: String,
    pub ts: String,
    pub chain_id: String,
    #[serde(skip_serializing_if = "Option::is_none", default)]
    pub signature: Option<String>,
}

/// Result of anchor verification against the audit chain.
#[derive(Debug)]
pub enum AnchorStatus {
    /// The latest anchor matches the audit chain.
    Verified,
    /// No anchor file or no anchor records found.
    NoAnchor,
    /// The anchor does not match the current chain state.
    AnchorMismatch {
        expected_seq: u64,
        expected_hash: String,
        found_hash: Option<String>,
    },
    /// The anchor file could not be read.
    AnchorUnreadable(String),
}

/// Combined result of internal chain verification + anchor check.
#[derive(Debug)]
pub struct AnchoredVerification {
    pub chain_length: u64,
    pub internal_ok: bool,
    pub anchor_status: AnchorStatus,
    pub latest_anchor: Option<AnchorRecord>,
}

/// Read the chain tip (latest sequence + entry_hash) from audit.db using a read-only connection.
pub fn read_chain_tip(db_path: &Path) -> Result<Option<(u64, String)>> {
    let flags = rusqlite::OpenFlags::SQLITE_OPEN_READ_ONLY | rusqlite::OpenFlags::SQLITE_OPEN_NO_MUTEX;
    let conn = Connection::open_with_flags(db_path, flags)
        .with_context(|| format!("Failed to open audit.db read-only: {}", db_path.display()))?;

    let result: std::result::Result<(i64, String), _> = conn.query_row(
        "SELECT sequence, entry_hash FROM audit_events ORDER BY id DESC LIMIT 1",
        [],
        |row| Ok((row.get(0)?, row.get(1)?)),
    );

    match result {
        Ok((seq, hash)) => Ok(Some((seq as u64, hash))),
        Err(rusqlite::Error::QueryReturnedNoRows) => Ok(None),
        Err(e) => Err(e.into()),
    }
}

/// Read the genesis entry's entry_hash from audit.db (used to compute chain_id).
pub fn read_genesis_hash(db_path: &Path) -> Result<Option<String>> {
    let flags = rusqlite::OpenFlags::SQLITE_OPEN_READ_ONLY | rusqlite::OpenFlags::SQLITE_OPEN_NO_MUTEX;
    let conn = Connection::open_with_flags(db_path, flags)?;

    let result: std::result::Result<String, _> = conn.query_row(
        "SELECT entry_hash FROM audit_events ORDER BY id ASC LIMIT 1",
        [],
        |row| row.get(0),
    );

    match result {
        Ok(hash) => Ok(Some(hash)),
        Err(rusqlite::Error::QueryReturnedNoRows) => Ok(None),
        Err(e) => Err(e.into()),
    }
}

/// Compute the chain_id: SHA-256 of the genesis entry's entry_hash.
pub fn compute_chain_id(genesis_hash: &str) -> String {
    let mut hasher = Sha256::new();
    hasher.update(genesis_hash.as_bytes());
    hex::encode(hasher.finalize())
}

/// Read the latest anchor record from the anchor file (last non-empty line).
pub fn read_latest_anchor(anchor_path: &Path) -> Result<Option<AnchorRecord>> {
    let content = match std::fs::read_to_string(anchor_path) {
        Ok(c) => c,
        Err(e) if e.kind() == std::io::ErrorKind::NotFound => return Ok(None),
        Err(e) => return Err(e.into()),
    };

    let last_line = content
        .lines()
        .rev()
        .find(|line| !line.trim().is_empty());

    match last_line {
        Some(line) => {
            let record: AnchorRecord = serde_json::from_str(line)
                .with_context(|| "Failed to parse last anchor record")?;
            Ok(Some(record))
        }
        None => Ok(None),
    }
}

/// Verify audit chain integrity with anchor cross-check.
///
/// 1. Runs internal chain verification via `verify_chain`.
/// 2. Reads the latest anchor from the anchor file.
/// 3. If an anchor exists, verifies that the anchored (sequence, entry_hash)
///    pair still exists in the database and the chain tip is >= anchor sequence.
pub fn verify_chain_anchored(db_path: &Path, anchor_path: &Path) -> Result<AnchoredVerification> {
    // Internal linkage check
    let (chain_length, internal_ok) = match verify_chain(db_path) {
        Ok(count) => (count, true),
        Err(_) => {
            // Try to get chain length even on failure
            let len = read_chain_tip(db_path)
                .ok()
                .flatten()
                .map(|(seq, _)| seq + 1)
                .unwrap_or(0);
            (len, false)
        }
    };

    // Read latest anchor
    let latest_anchor = match read_latest_anchor(anchor_path) {
        Ok(a) => a,
        Err(e) => {
            return Ok(AnchoredVerification {
                chain_length,
                internal_ok,
                anchor_status: AnchorStatus::AnchorUnreadable(e.to_string()),
                latest_anchor: None,
            });
        }
    };

    let anchor_status = match &latest_anchor {
        None => AnchorStatus::NoAnchor,
        Some(anchor) => {
            // Verify that the anchor's (sequence, entry_hash) exists in the DB
            let flags = rusqlite::OpenFlags::SQLITE_OPEN_READ_ONLY | rusqlite::OpenFlags::SQLITE_OPEN_NO_MUTEX;
            let conn = Connection::open_with_flags(db_path, flags)?;

            let db_hash: std::result::Result<String, _> = conn.query_row(
                "SELECT entry_hash FROM audit_events WHERE sequence = ?1",
                params![anchor.sequence as i64],
                |row| row.get(0),
            );

            match db_hash {
                Ok(hash) if hash == anchor.entry_hash => {
                    // Also verify chain tip >= anchor sequence
                    if chain_length > 0 && (chain_length - 1) >= anchor.sequence {
                        AnchorStatus::Verified
                    } else {
                        AnchorStatus::AnchorMismatch {
                            expected_seq: anchor.sequence,
                            expected_hash: anchor.entry_hash.clone(),
                            found_hash: Some(hash),
                        }
                    }
                }
                Ok(hash) => AnchorStatus::AnchorMismatch {
                    expected_seq: anchor.sequence,
                    expected_hash: anchor.entry_hash.clone(),
                    found_hash: Some(hash),
                },
                Err(_) => AnchorStatus::AnchorMismatch {
                    expected_seq: anchor.sequence,
                    expected_hash: anchor.entry_hash.clone(),
                    found_hash: None,
                },
            }
        }
    };

    Ok(AnchoredVerification {
        chain_length,
        internal_ok,
        anchor_status,
        latest_anchor,
    })
}

/// Append an anchor record to the anchor file (JSON Lines format).
pub fn append_anchor(anchor_path: &Path, record: &AnchorRecord) -> Result<()> {
    if let Some(parent) = anchor_path.parent() {
        std::fs::create_dir_all(parent)
            .with_context(|| format!("Failed to create anchor directory: {}", parent.display()))?;
    }

    let mut file = std::fs::OpenOptions::new()
        .create(true)
        .append(true)
        .open(anchor_path)
        .with_context(|| format!("Failed to open anchor file: {}", anchor_path.display()))?;

    let json = serde_json::to_string(record)?;
    use std::io::Write;
    writeln!(file, "{}", json)?;
    Ok(())
}

#[cfg(test)]
mod tests {
    use super::*;
    use scopeguard::defer;
    use std::sync::{Arc, Mutex};
    use tempfile::TempDir;

    /// Mutex to serialize tests that read/write DAEMONCLAW_AUDIT_SIGNING_KEY env var.
    static ENV_MUTEX: Mutex<()> = Mutex::new(());

    #[test]
    fn audit_event_new_creates_unique_id() {
        let event1 = AuditEvent::new(AuditEventType::CommandExecution);
        let event2 = AuditEvent::new(AuditEventType::CommandExecution);
        assert_ne!(event1.event_id, event2.event_id);
    }

    #[test]
    fn audit_event_with_actor() {
        let event = AuditEvent::new(AuditEventType::CommandExecution).with_actor(
            "telegram".to_string(),
            Some("123".to_string()),
            Some("@daemonclaw_user".to_string()),
        );

        assert!(event.actor.is_some());
        let actor = event.actor.as_ref().unwrap();
        assert_eq!(actor.channel, "telegram");
        assert_eq!(actor.user_id, Some("123".to_string()));
        assert_eq!(actor.username, Some("@daemonclaw_user".to_string()));
    }

    #[test]
    fn audit_event_with_action() {
        let event = AuditEvent::new(AuditEventType::CommandExecution).with_action(
            "ls -la".to_string(),
            "low".to_string(),
            false,
            true,
        );

        assert!(event.action.is_some());
        let action = event.action.as_ref().unwrap();
        assert_eq!(action.command, Some("ls -la".to_string()));
        assert_eq!(action.risk_level, Some("low".to_string()));
    }

    #[test]
    fn audit_event_serializes_to_json() {
        let event = AuditEvent::new(AuditEventType::CommandExecution)
            .with_actor("telegram".to_string(), None, None)
            .with_action("ls".to_string(), "low".to_string(), false, true)
            .with_result(true, Some(0), 15, None);

        let json = serde_json::to_string(&event);
        assert!(json.is_ok());
        let json = json.expect("serialize");
        let parsed: AuditEvent = serde_json::from_str(json.as_str()).expect("parse");
        assert!(parsed.actor.is_some());
        assert!(parsed.action.is_some());
        assert!(parsed.result.is_some());
    }

    #[test]
    fn audit_logger_disabled_does_not_insert_events() -> Result<()> {
        let tmp = TempDir::new()?;
        let config = AuditConfig {
            enabled: false,
            ..Default::default()
        };
        let logger = AuditLogger::new(config, tmp.path().to_path_buf())?;
        let event = AuditEvent::new(AuditEventType::CommandExecution);

        logger.log(&event)?;

        let conn = Connection::open(logger.db_path())?;
        let count: i64 = conn.query_row("SELECT COUNT(*) FROM audit_events", [], |r| r.get(0))?;
        assert_eq!(count, 0);
        Ok(())
    }

    // ── §8.1 SQLite storage tests ───────────────────────────────

    #[tokio::test]
    async fn audit_logger_writes_event_when_enabled() -> Result<()> {
        let tmp = TempDir::new()?;
        let config = AuditConfig {
            enabled: true,
            max_size_mb: 10,
            ..Default::default()
        };
        let logger = AuditLogger::new(config, tmp.path().to_path_buf())?;
        let event = AuditEvent::new(AuditEventType::CommandExecution)
            .with_actor("cli".to_string(), None, None)
            .with_action("ls".to_string(), "low".to_string(), false, true);

        logger.log(&event)?;

        let conn = Connection::open(logger.db_path())?;
        let count: i64 = conn.query_row("SELECT COUNT(*) FROM audit_events", [], |r| r.get(0))?;
        assert_eq!(count, 1);

        let json: String = conn.query_row(
            "SELECT event_json FROM audit_events LIMIT 1",
            [],
            |r| r.get(0),
        )?;
        let parsed: AuditEvent = serde_json::from_str(&json)?;
        assert!(parsed.action.is_some());
        Ok(())
    }

    #[tokio::test]
    async fn audit_log_command_event_writes_structured_entry() -> Result<()> {
        let tmp = TempDir::new()?;
        let config = AuditConfig {
            enabled: true,
            max_size_mb: 10,
            ..Default::default()
        };
        let logger = AuditLogger::new(config, tmp.path().to_path_buf())?;

        logger.log_command_event(CommandExecutionLog {
            channel: "telegram",
            command: "echo test",
            risk_level: "low",
            approved: false,
            allowed: true,
            success: true,
            duration_ms: 42,
        })?;

        let conn = Connection::open(logger.db_path())?;
        let json: String = conn.query_row(
            "SELECT event_json FROM audit_events LIMIT 1",
            [],
            |r| r.get(0),
        )?;
        let parsed: AuditEvent = serde_json::from_str(&json)?;

        let action = parsed.action.unwrap();
        assert_eq!(action.command, Some("echo test".to_string()));
        assert_eq!(action.risk_level, Some("low".to_string()));
        assert!(action.allowed);

        let result = parsed.result.unwrap();
        assert!(result.success);
        assert_eq!(result.duration_ms, Some(42));
        Ok(())
    }

    #[test]
    fn audit_db_created_in_audit_subdirectory() -> Result<()> {
        let tmp = TempDir::new()?;
        let config = AuditConfig {
            enabled: true,
            ..Default::default()
        };
        let logger = AuditLogger::new(config, tmp.path().to_path_buf())?;

        assert!(logger.db_path().exists());
        assert_eq!(
            logger.db_path(),
            tmp.path().join("audit").join("audit.db")
        );
        Ok(())
    }

    // ── Merkle hash-chain tests ─────────────────────────────────

    #[test]
    fn merkle_chain_genesis_uses_well_known_seed() -> Result<()> {
        let tmp = TempDir::new()?;
        let config = AuditConfig {
            enabled: true,
            max_size_mb: 10,
            ..Default::default()
        };
        let logger = AuditLogger::new(config, tmp.path().to_path_buf())?;

        let event = AuditEvent::new(AuditEventType::SecurityEvent);
        logger.log(&event)?;

        let conn = Connection::open(logger.db_path())?;
        let json: String = conn.query_row(
            "SELECT event_json FROM audit_events LIMIT 1",
            [],
            |r| r.get(0),
        )?;
        let parsed: AuditEvent = serde_json::from_str(&json)?;

        assert_eq!(parsed.sequence, 0);
        assert_eq!(parsed.prev_hash, GENESIS_PREV_HASH);
        assert!(!parsed.entry_hash.is_empty());
        Ok(())
    }

    #[test]
    fn merkle_chain_multiple_entries_verify() -> Result<()> {
        let tmp = TempDir::new()?;
        let config = AuditConfig {
            enabled: true,
            max_size_mb: 10,
            ..Default::default()
        };
        let logger = AuditLogger::new(config, tmp.path().to_path_buf())?;

        for i in 0..5 {
            let event = AuditEvent::new(AuditEventType::CommandExecution).with_action(
                format!("cmd-{}", i),
                "low".to_string(),
                false,
                true,
            );
            logger.log(&event)?;
        }

        let count = verify_chain(logger.db_path())?;
        assert_eq!(count, 5);
        Ok(())
    }

    #[test]
    fn merkle_chain_detects_tampered_entry() -> Result<()> {
        let tmp = TempDir::new()?;
        let config = AuditConfig {
            enabled: true,
            max_size_mb: 10,
            ..Default::default()
        };
        let logger = AuditLogger::new(config, tmp.path().to_path_buf())?;

        for i in 0..3 {
            let event = AuditEvent::new(AuditEventType::CommandExecution).with_action(
                format!("cmd-{}", i),
                "low".to_string(),
                false,
                true,
            );
            logger.log(&event)?;
        }

        // Simulate external tampering (bypass triggers to set up tampered state)
        let conn = Connection::open(logger.db_path())?;
        conn.execute_batch("DROP TRIGGER IF EXISTS audit_no_update")?;
        let json: String = conn.query_row(
            "SELECT event_json FROM audit_events WHERE sequence = 1",
            [],
            |r| r.get(0),
        )?;
        let mut entry: serde_json::Value = serde_json::from_str(&json)?;
        entry["action"]["command"] = serde_json::Value::String("TAMPERED".to_string());
        let tampered_json = serde_json::to_string(&entry)?;
        conn.execute(
            "UPDATE audit_events SET event_json = ?1 WHERE sequence = 1",
            params![tampered_json],
        )?;

        let result = verify_chain(logger.db_path());
        assert!(result.is_err());
        let err_msg = result.unwrap_err().to_string();
        assert!(
            err_msg.contains("entry_hash mismatch"),
            "expected entry_hash mismatch, got: {}",
            err_msg
        );
        Ok(())
    }

    #[test]
    fn merkle_chain_detects_sequence_gap() -> Result<()> {
        let tmp = TempDir::new()?;
        let config = AuditConfig {
            enabled: true,
            max_size_mb: 10,
            ..Default::default()
        };
        let logger = AuditLogger::new(config, tmp.path().to_path_buf())?;

        for i in 0..3 {
            let event = AuditEvent::new(AuditEventType::CommandExecution).with_action(
                format!("cmd-{}", i),
                "low".to_string(),
                false,
                true,
            );
            logger.log(&event)?;
        }

        // Simulate external tampering (bypass triggers to create a gap)
        let conn = Connection::open(logger.db_path())?;
        conn.execute_batch("DROP TRIGGER IF EXISTS audit_no_delete")?;
        conn.execute("DELETE FROM audit_events WHERE sequence = 1", [])?;

        let result = verify_chain(logger.db_path());
        assert!(result.is_err());
        let err_msg = result.unwrap_err().to_string();
        assert!(
            err_msg.contains("sequence gap"),
            "expected sequence gap, got: {}",
            err_msg
        );
        Ok(())
    }

    #[test]
    fn merkle_chain_recovery_continues_after_restart() -> Result<()> {
        let tmp = TempDir::new()?;

        // First logger writes 2 entries
        {
            let config = AuditConfig {
                enabled: true,
                max_size_mb: 10,
                ..Default::default()
            };
            let logger = AuditLogger::new(config, tmp.path().to_path_buf())?;
            for i in 0..2 {
                let event = AuditEvent::new(AuditEventType::CommandExecution).with_action(
                    format!("batch1-{}", i),
                    "low".to_string(),
                    false,
                    true,
                );
                logger.log(&event)?;
            }
        }

        // Second logger (simulating restart) continues the chain
        {
            let config = AuditConfig {
                enabled: true,
                max_size_mb: 10,
                ..Default::default()
            };
            let logger = AuditLogger::new(config, tmp.path().to_path_buf())?;
            for i in 0..2 {
                let event = AuditEvent::new(AuditEventType::CommandExecution).with_action(
                    format!("batch2-{}", i),
                    "low".to_string(),
                    false,
                    true,
                );
                logger.log(&event)?;
            }
        }

        // Full chain should verify (4 entries, sequences 0..3)
        let db_path = tmp.path().join("audit").join("audit.db");
        let count = verify_chain(&db_path)?;
        assert_eq!(count, 4);
        Ok(())
    }

    // ── HMAC signing tests ──────────────────────────────────────

    #[test]
    fn signature_present_when_sign_events_enabled() -> Result<()> {
        let _guard = ENV_MUTEX.lock().unwrap();
        let old_key = std::env::var("DAEMONCLAW_AUDIT_SIGNING_KEY").ok();
        defer! {
            if let Some(key) = old_key {
                // SAFETY: test-only, single-threaded test runner.
                unsafe { std::env::set_var("DAEMONCLAW_AUDIT_SIGNING_KEY", key) };
            } else {
                // SAFETY: test-only, single-threaded test runner.
                unsafe { std::env::remove_var("DAEMONCLAW_AUDIT_SIGNING_KEY") };
            }
        }

        let tmp = TempDir::new()?;
        let test_key = "a".repeat(64); // 64 hex chars = 32 bytes
        // SAFETY: test-only, single-threaded test runner.
        unsafe { std::env::set_var("DAEMONCLAW_AUDIT_SIGNING_KEY", &test_key) };

        let config = AuditConfig {
            enabled: true,
            sign_events: true,
            ..Default::default()
        };
        let logger = AuditLogger::new(config, tmp.path().to_path_buf())?;
        let event = AuditEvent::new(AuditEventType::CommandExecution);

        logger.log(&event)?;

        let conn = Connection::open(logger.db_path())?;
        let json: String = conn.query_row(
            "SELECT event_json FROM audit_events LIMIT 1",
            [],
            |r| r.get(0),
        )?;
        let parsed: AuditEvent = serde_json::from_str(&json)?;

        assert!(
            parsed.signature.is_some(),
            "signature must be present when sign_events=true"
        );
        let sig = parsed.signature.unwrap();
        assert_eq!(sig.len(), 64, "HMAC-SHA256 signature must be 64 hex chars");

        Ok(())
    }

    #[test]
    fn signature_absent_when_sign_events_disabled() -> Result<()> {
        let _guard = ENV_MUTEX.lock().unwrap();
        let tmp = TempDir::new()?;
        let config = AuditConfig {
            enabled: true,
            sign_events: false,
            ..Default::default()
        };
        let logger = AuditLogger::new(config, tmp.path().to_path_buf())?;
        let event = AuditEvent::new(AuditEventType::CommandExecution);

        logger.log(&event)?;

        let conn = Connection::open(logger.db_path())?;
        let json: String = conn.query_row(
            "SELECT event_json FROM audit_events LIMIT 1",
            [],
            |r| r.get(0),
        )?;
        let parsed: AuditEvent = serde_json::from_str(&json)?;

        assert!(
            parsed.signature.is_none(),
            "signature must be absent when sign_events=false"
        );
        Ok(())
    }

    #[test]
    fn signature_computed_over_entry_hash() -> Result<()> {
        let _guard = ENV_MUTEX.lock().unwrap();
        let old_key = std::env::var("DAEMONCLAW_AUDIT_SIGNING_KEY").ok();
        defer! {
            if let Some(key) = old_key {
                // SAFETY: test-only, single-threaded test runner.
                unsafe { std::env::set_var("DAEMONCLAW_AUDIT_SIGNING_KEY", key) };
            } else {
                // SAFETY: test-only, single-threaded test runner.
                unsafe { std::env::remove_var("DAEMONCLAW_AUDIT_SIGNING_KEY") };
            }
        }

        let tmp = TempDir::new()?;
        let test_key = "b".repeat(64);
        // SAFETY: test-only, single-threaded test runner.
        unsafe { std::env::set_var("DAEMONCLAW_AUDIT_SIGNING_KEY", &test_key) };

        let config = AuditConfig {
            enabled: true,
            sign_events: true,
            ..Default::default()
        };
        let logger = AuditLogger::new(config, tmp.path().to_path_buf())?;
        let event = AuditEvent::new(AuditEventType::CommandExecution);

        logger.log(&event)?;

        let conn = Connection::open(logger.db_path())?;
        let json: String = conn.query_row(
            "SELECT event_json FROM audit_events LIMIT 1",
            [],
            |r| r.get(0),
        )?;
        let parsed: AuditEvent = serde_json::from_str(&json)?;

        // Manually recompute HMAC to verify correctness
        use hmac::{Hmac, Mac};
        use sha2::Sha256;
        let key_bytes = hex::decode(&test_key)?;
        let mut mac = Hmac::<Sha256>::new_from_slice(&key_bytes).unwrap();
        mac.update(parsed.entry_hash.as_bytes());
        let expected_sig = hex::encode(mac.finalize().into_bytes());

        assert_eq!(parsed.signature, Some(expected_sig));

        Ok(())
    }

    #[test]
    fn constructor_fails_if_sign_events_but_no_key() -> Result<()> {
        let _guard = ENV_MUTEX.lock().unwrap();
        let old_key = std::env::var("DAEMONCLAW_AUDIT_SIGNING_KEY").ok();
        defer! {
            // Only restore if it was a valid 64-char key
            if let Some(key) = old_key.as_ref().filter(|k| k.len() == 64) {
                // SAFETY: test-only, single-threaded test runner.
                unsafe { std::env::set_var("DAEMONCLAW_AUDIT_SIGNING_KEY", key) };
            } else {
                // SAFETY: test-only, single-threaded test runner.
                unsafe { std::env::remove_var("DAEMONCLAW_AUDIT_SIGNING_KEY") };
            }
        }

        // SAFETY: test-only, single-threaded test runner.
        unsafe { std::env::remove_var("DAEMONCLAW_AUDIT_SIGNING_KEY") };

        let tmp = TempDir::new()?;
        let config = AuditConfig {
            enabled: true,
            sign_events: true,
            ..Default::default()
        };

        let result = AuditLogger::new(config, tmp.path().to_path_buf());
        assert!(result.is_err());
        if let Err(e) = result {
            let err_msg = e.to_string();
            assert!(
                err_msg.contains("DAEMONCLAW_AUDIT_SIGNING_KEY not set"),
                "error: {}",
                err_msg
            );
        }

        Ok(())
    }

    #[test]
    fn constructor_fails_if_signing_key_invalid_hex() -> Result<()> {
        let _guard = ENV_MUTEX.lock().unwrap();
        let old_key = std::env::var("DAEMONCLAW_AUDIT_SIGNING_KEY").ok();
        defer! {
            // Only restore if it was a valid 64-char key
            if let Some(key) = old_key.as_ref().filter(|k| k.len() == 64) {
                // SAFETY: test-only, single-threaded test runner.
                unsafe { std::env::set_var("DAEMONCLAW_AUDIT_SIGNING_KEY", key) };
            } else {
                // SAFETY: test-only, single-threaded test runner.
                unsafe { std::env::remove_var("DAEMONCLAW_AUDIT_SIGNING_KEY") };
            }
        }

        // SAFETY: test-only, single-threaded test runner.
        unsafe { std::env::set_var("DAEMONCLAW_AUDIT_SIGNING_KEY", "not-valid-hex") };

        let tmp = TempDir::new()?;
        let config = AuditConfig {
            enabled: true,
            sign_events: true,
            ..Default::default()
        };

        let result = AuditLogger::new(config, tmp.path().to_path_buf());
        assert!(result.is_err());
        if let Err(e) = result {
            let err_msg = e.to_string();
            assert!(
                err_msg.contains("must be hex-encoded"),
                "error: {}",
                err_msg
            );
        }

        Ok(())
    }

    #[test]
    fn constructor_fails_if_signing_key_wrong_length() -> Result<()> {
        let _guard = ENV_MUTEX.lock().unwrap();
        let old_key = std::env::var("DAEMONCLAW_AUDIT_SIGNING_KEY").ok();
        defer! {
            // Only restore if it was a valid 64-char key
            if let Some(key) = old_key.as_ref().filter(|k| k.len() == 64) {
                // SAFETY: test-only, single-threaded test runner.
                unsafe { std::env::set_var("DAEMONCLAW_AUDIT_SIGNING_KEY", key) };
            } else {
                // SAFETY: test-only, single-threaded test runner.
                unsafe { std::env::remove_var("DAEMONCLAW_AUDIT_SIGNING_KEY") };
            }
        }

        // 30 bytes = 60 hex chars (not 32 bytes)
        let short_key = "c".repeat(60);
        // SAFETY: test-only, single-threaded test runner.
        unsafe { std::env::set_var("DAEMONCLAW_AUDIT_SIGNING_KEY", &short_key) };
        let tmp = TempDir::new()?;
        let config = AuditConfig {
            enabled: true,
            sign_events: true,
            ..Default::default()
        };

        let result = AuditLogger::new(config, tmp.path().to_path_buf());
        assert!(result.is_err());
        if let Err(e) = result {
            let err_msg = e.to_string();
            assert!(err_msg.contains("must be 32 bytes"), "error: {}", err_msg);
        }

        Ok(())
    }

    #[test]
    fn different_keys_produce_different_signatures() -> Result<()> {
        let _guard = ENV_MUTEX.lock().unwrap();
        let old_key = std::env::var("DAEMONCLAW_AUDIT_SIGNING_KEY").ok();
        defer! {
            if let Some(key) = old_key {
                // SAFETY: test-only, single-threaded test runner.
                unsafe { std::env::set_var("DAEMONCLAW_AUDIT_SIGNING_KEY", key) };
            } else {
                // SAFETY: test-only, single-threaded test runner.
                unsafe { std::env::remove_var("DAEMONCLAW_AUDIT_SIGNING_KEY") };
            }
        }

        let _tmp = TempDir::new()?;

        // Compute HMAC manually with key1
        let key1 = "d".repeat(64);
        let key1_bytes = hex::decode(&key1)?;

        // Compute HMAC manually with key2
        let key2 = "e".repeat(64);
        let key2_bytes = hex::decode(&key2)?;

        // Use a fixed entry_hash for testing
        let test_entry_hash = "test_hash_value";

        use hmac::{Hmac, Mac};
        use sha2::Sha256;

        let mut mac1 = Hmac::<Sha256>::new_from_slice(&key1_bytes).unwrap();
        mac1.update(test_entry_hash.as_bytes());
        let sig1 = hex::encode(mac1.finalize().into_bytes());

        let mut mac2 = Hmac::<Sha256>::new_from_slice(&key2_bytes).unwrap();
        mac2.update(test_entry_hash.as_bytes());
        let sig2 = hex::encode(mac2.finalize().into_bytes());

        assert_ne!(
            sig1, sig2,
            "different keys must produce different signatures"
        );

        Ok(())
    }

    #[test]
    fn signature_deterministic_for_same_entry_hash() -> Result<()> {
        let _guard = ENV_MUTEX.lock().unwrap();
        let old_key = std::env::var("DAEMONCLAW_AUDIT_SIGNING_KEY").ok();
        defer! {
            if let Some(key) = old_key {
                // SAFETY: test-only, single-threaded test runner.
                unsafe { std::env::set_var("DAEMONCLAW_AUDIT_SIGNING_KEY", key) };
            } else {
                // SAFETY: test-only, single-threaded test runner.
                unsafe { std::env::remove_var("DAEMONCLAW_AUDIT_SIGNING_KEY") };
            }
        }

        let tmp = TempDir::new()?;
        let test_key = "f".repeat(64);
        // SAFETY: test-only, single-threaded test runner.
        unsafe { std::env::set_var("DAEMONCLAW_AUDIT_SIGNING_KEY", &test_key) };

        let config = AuditConfig {
            enabled: true,
            sign_events: true,
            ..Default::default()
        };
        let logger = AuditLogger::new(config, tmp.path().to_path_buf())?;

        // Log two events
        for _ in 0..2 {
            let event = AuditEvent::new(AuditEventType::CommandExecution).with_action(
                "cmd".to_string(),
                "low".to_string(),
                false,
                true,
            );
            logger.log(&event)?;
        }

        let conn = Connection::open(logger.db_path())?;
        let mut stmt = conn.prepare("SELECT event_json FROM audit_events ORDER BY id ASC")?;
        let jsons: Vec<String> = stmt
            .query_map([], |r| r.get(0))?
            .collect::<std::result::Result<_, _>>()?;
        let event1: AuditEvent = serde_json::from_str(&jsons[0])?;
        let event2: AuditEvent = serde_json::from_str(&jsons[1])?;

        // Different entry_hashes due to chaining, so signatures should differ
        assert_ne!(event1.entry_hash, event2.entry_hash);
        assert_ne!(event1.signature, event2.signature);

        // Manually verify determinism by recomputing signature for event1
        use hmac::{Hmac, Mac};
        use sha2::Sha256;
        let key_bytes = hex::decode(&test_key)?;
        let mut mac = Hmac::<Sha256>::new_from_slice(&key_bytes).unwrap();
        mac.update(event1.entry_hash.as_bytes());
        let expected_sig1 = hex::encode(mac.finalize().into_bytes());
        assert_eq!(event1.signature, Some(expected_sig1));

        Ok(())
    }

    #[test]
    fn verify_chain_accepts_mixed_signed_and_unsigned_records() -> Result<()> {
        let _guard = ENV_MUTEX.lock().unwrap();
        let old_key = std::env::var("DAEMONCLAW_AUDIT_SIGNING_KEY").ok();
        defer! {
            if let Some(key) = old_key.as_ref().filter(|k| k.len() == 64) {
                // SAFETY: test-only, single-threaded test runner.
                unsafe { std::env::set_var("DAEMONCLAW_AUDIT_SIGNING_KEY", key) };
            } else {
                // SAFETY: test-only, single-threaded test runner.
                unsafe { std::env::remove_var("DAEMONCLAW_AUDIT_SIGNING_KEY") };
            }
        }

        let tmp = TempDir::new()?;
        let test_key = "a1".repeat(32); // 64 hex chars = 32 bytes

        // First logger with sign_events=false (unsigned records)
        {
            // SAFETY: test-only, single-threaded test runner.
            unsafe { std::env::remove_var("DAEMONCLAW_AUDIT_SIGNING_KEY") };
            let config = AuditConfig {
                enabled: true,
                sign_events: false,
                ..Default::default()
            };
            let logger = AuditLogger::new(config, tmp.path().to_path_buf())?;
            for i in 0..2 {
                let event = AuditEvent::new(AuditEventType::CommandExecution).with_action(
                    format!("unsigned-{}", i),
                    "low".to_string(),
                    false,
                    true,
                );
                logger.log(&event)?;
            }
        }

        // Second logger with sign_events=true (signed records)
        {
            // SAFETY: test-only, single-threaded test runner.
            unsafe { std::env::set_var("DAEMONCLAW_AUDIT_SIGNING_KEY", &test_key) };
            let config = AuditConfig {
                enabled: true,
                sign_events: true,
                ..Default::default()
            };
            let logger = AuditLogger::new(config, tmp.path().to_path_buf())?;
            for i in 0..2 {
                let event = AuditEvent::new(AuditEventType::CommandExecution).with_action(
                    format!("signed-{}", i),
                    "low".to_string(),
                    false,
                    true,
                );
                logger.log(&event)?;
            }
        }

        // Verify the full chain (4 records: 2 unsigned + 2 signed)
        // Set the key in env so verify_chain can check signatures
        // SAFETY: test-only, single-threaded test runner.
        unsafe { std::env::set_var("DAEMONCLAW_AUDIT_SIGNING_KEY", &test_key) };
        let db_path = tmp.path().join("audit").join("audit.db");
        let count = verify_chain(&db_path)?;
        assert_eq!(count, 4, "should verify all 4 records");

        // Verify that first 2 records have no signature, last 2 have signatures
        let conn = Connection::open(&db_path)?;
        let mut stmt = conn.prepare("SELECT event_json FROM audit_events ORDER BY id ASC")?;
        let jsons: Vec<String> = stmt
            .query_map([], |r| r.get(0))?
            .collect::<std::result::Result<_, _>>()?;
        assert_eq!(jsons.len(), 4);

        let rec0: AuditEvent = serde_json::from_str(&jsons[0])?;
        let rec1: AuditEvent = serde_json::from_str(&jsons[1])?;
        let rec2: AuditEvent = serde_json::from_str(&jsons[2])?;
        let rec3: AuditEvent = serde_json::from_str(&jsons[3])?;

        assert!(rec0.signature.is_none(), "first unsigned record");
        assert!(rec1.signature.is_none(), "second unsigned record");
        assert!(rec2.signature.is_some(), "first signed record");
        assert!(rec3.signature.is_some(), "second signed record");

        Ok(())
    }

    // ── Concurrency tests ──────────────────────────────────────

    #[test]
    fn concurrent_writers_produce_single_valid_chain() -> Result<()> {
        let tmp = TempDir::new()?;
        let config = AuditConfig {
            enabled: true,
            max_size_mb: 10,
            ..Default::default()
        };
        let db_path = tmp.path().to_path_buf();

        // Two independent AuditLogger instances (simulates two processes
        // or two independently-constructed loggers sharing one audit.db).
        let logger_a = Arc::new(AuditLogger::new(config.clone(), db_path.clone())?);
        let logger_b = Arc::new(AuditLogger::new(config, db_path.clone())?);

        let events_per_writer = 50;
        let la = Arc::clone(&logger_a);
        let lb = Arc::clone(&logger_b);

        let handle_a = std::thread::spawn(move || -> Result<()> {
            for i in 0..events_per_writer {
                let event = AuditEvent::new(AuditEventType::CommandExecution)
                    .with_actor("writer-a".into(), None, None)
                    .with_action(format!("a-cmd-{i}"), "low".into(), false, true);
                la.log(&event)?;
            }
            Ok(())
        });

        let handle_b = std::thread::spawn(move || -> Result<()> {
            for i in 0..events_per_writer {
                let event = AuditEvent::new(AuditEventType::CommandExecution)
                    .with_actor("writer-b".into(), None, None)
                    .with_action(format!("b-cmd-{i}"), "low".into(), false, true);
                lb.log(&event)?;
            }
            Ok(())
        });

        handle_a.join().unwrap()?;
        handle_b.join().unwrap()?;

        let audit_db = tmp.path().join("audit").join("audit.db");

        // verify_chain walks the entire chain and checks:
        // - contiguous sequences (0..N)
        // - each prev_hash links to the preceding entry_hash
        // - each entry_hash is correctly computed
        // This FAILS if any fork occurred (duplicate sequence or mislinked prev_hash).
        let count = verify_chain(&audit_db)?;
        assert_eq!(count, events_per_writer * 2);

        // Double-check: all sequence numbers are unique
        let conn = Connection::open(&audit_db)?;
        let unique_seqs: i64 = conn.query_row(
            "SELECT COUNT(DISTINCT sequence) FROM audit_events",
            [],
            |r| r.get(0),
        )?;
        assert_eq!(unique_seqs, (events_per_writer * 2) as i64);

        Ok(())
    }

    #[test]
    fn concurrent_writers_interleave_from_both_sources() -> Result<()> {
        let tmp = TempDir::new()?;
        let config = AuditConfig {
            enabled: true,
            max_size_mb: 10,
            ..Default::default()
        };
        let db_path = tmp.path().to_path_buf();

        let logger_a = Arc::new(AuditLogger::new(config.clone(), db_path.clone())?);
        let logger_b = Arc::new(AuditLogger::new(config, db_path)?);

        let la = Arc::clone(&logger_a);
        let lb = Arc::clone(&logger_b);
        let events_per_writer = 20;

        let handle_a = std::thread::spawn(move || -> Result<()> {
            for i in 0..events_per_writer {
                let event = AuditEvent::new(AuditEventType::CommandExecution)
                    .with_actor("daemon".into(), None, None)
                    .with_action(format!("shell-{i}"), "standard".into(), true, true);
                la.log(&event)?;
            }
            Ok(())
        });

        let handle_b = std::thread::spawn(move || -> Result<()> {
            for i in 0..events_per_writer {
                let event = AuditEvent::new(AuditEventType::CommandExecution)
                    .with_actor("cli".into(), None, None)
                    .with_action(format!("echo-{i}"), "standard".into(), true, true);
                lb.log(&event)?;
            }
            Ok(())
        });

        handle_a.join().unwrap()?;
        handle_b.join().unwrap()?;

        let audit_db = tmp.path().join("audit").join("audit.db");
        let count = verify_chain(&audit_db)?;
        assert_eq!(count, events_per_writer * 2);

        // Verify both writers contributed (events are interleaved, not batched)
        let conn = Connection::open(&audit_db)?;
        let daemon_count: i64 = conn.query_row(
            "SELECT COUNT(*) FROM audit_events WHERE event_json LIKE '%writer-a%' OR event_json LIKE '%daemon%'",
            [],
            |r| r.get(0),
        )?;
        let cli_count: i64 = conn.query_row(
            "SELECT COUNT(*) FROM audit_events WHERE event_json LIKE '%writer-b%' OR event_json LIKE '%cli%'",
            [],
            |r| r.get(0),
        )?;
        assert_eq!(daemon_count, events_per_writer as i64);
        assert_eq!(cli_count, events_per_writer as i64);

        Ok(())
    }

    // ── Append-only enforcement tests ──────────────────────────

    #[test]
    fn direct_delete_is_rejected_by_trigger() -> Result<()> {
        let tmp = TempDir::new()?;
        let config = AuditConfig {
            enabled: true,
            max_size_mb: 10,
            ..Default::default()
        };
        let logger = AuditLogger::new(config, tmp.path().to_path_buf())?;

        let event = AuditEvent::new(AuditEventType::CommandExecution)
            .with_action("shell".into(), "low".into(), false, true);
        logger.log(&event)?;

        let conn = Connection::open(logger.db_path())?;
        let result = conn.execute("DELETE FROM audit_events WHERE sequence = 0", []);
        assert!(result.is_err());
        let err = result.unwrap_err().to_string();
        assert!(
            err.contains("append-only") && err.contains("DELETE prohibited"),
            "expected append-only DELETE error, got: {err}"
        );

        let count: i64 = conn.query_row(
            "SELECT COUNT(*) FROM audit_events", [], |r| r.get(0),
        )?;
        assert_eq!(count, 1, "row must survive the rejected DELETE");
        Ok(())
    }

    #[test]
    fn direct_update_is_rejected_by_trigger() -> Result<()> {
        let tmp = TempDir::new()?;
        let config = AuditConfig {
            enabled: true,
            max_size_mb: 10,
            ..Default::default()
        };
        let logger = AuditLogger::new(config, tmp.path().to_path_buf())?;

        let event = AuditEvent::new(AuditEventType::CommandExecution)
            .with_action("shell".into(), "low".into(), false, true);
        logger.log(&event)?;

        let conn = Connection::open(logger.db_path())?;
        let result = conn.execute(
            "UPDATE audit_events SET event_json = 'TAMPERED' WHERE sequence = 0",
            [],
        );
        assert!(result.is_err());
        let err = result.unwrap_err().to_string();
        assert!(
            err.contains("append-only") && err.contains("UPDATE prohibited"),
            "expected append-only UPDATE error, got: {err}"
        );

        let json: String = conn.query_row(
            "SELECT event_json FROM audit_events WHERE sequence = 0",
            [],
            |r| r.get(0),
        )?;
        assert!(
            !json.contains("TAMPERED"),
            "row content must be unchanged after rejected UPDATE"
        );
        Ok(())
    }

    #[test]
    fn log_still_works_with_triggers_installed() -> Result<()> {
        let tmp = TempDir::new()?;
        let config = AuditConfig {
            enabled: true,
            max_size_mb: 10,
            ..Default::default()
        };
        let logger = AuditLogger::new(config, tmp.path().to_path_buf())?;

        for i in 0..10 {
            let event = AuditEvent::new(AuditEventType::CommandExecution)
                .with_action(format!("cmd-{i}"), "low".into(), false, true);
            logger.log(&event)?;
        }

        let count = verify_chain(logger.db_path())?;
        assert_eq!(count, 10);
        Ok(())
    }

    #[test]
    fn record_chain_boundary_appends_event() -> Result<()> {
        let tmp = TempDir::new()?;
        let config = AuditConfig {
            enabled: true,
            max_size_mb: 10,
            ..Default::default()
        };
        let logger = AuditLogger::new(config, tmp.path().to_path_buf())?;

        let event = AuditEvent::new(AuditEventType::CommandExecution)
            .with_action("shell".into(), "low".into(), false, true);
        logger.log(&event)?;

        logger.record_chain_boundary(
            "fork cleanup: seqs 34-37, 79-80, 116 removed — 6 daemon + 1 CLI proof event"
        )?;

        let count = verify_chain(logger.db_path())?;
        assert_eq!(count, 2);

        let conn = Connection::open(logger.db_path())?;
        let event_type: String = conn.query_row(
            "SELECT event_type FROM audit_events WHERE sequence = 1",
            [],
            |r| r.get(0),
        )?;
        assert_eq!(event_type, "chain_boundary");
        Ok(())
    }

    // ── Track H: Anchor-chain anchoring tests ─────────────────────

    #[test]
    fn anchor_truncation_detection() -> Result<()> {
        // Populate chain (10+ events) -> anchor -> tail-truncate -> verify
        let tmp = TempDir::new()?;
        let config = AuditConfig {
            enabled: true,
            max_size_mb: 10,
            ..Default::default()
        };
        let logger = AuditLogger::new(config, tmp.path().to_path_buf())?;

        for i in 0..12 {
            let event = AuditEvent::new(AuditEventType::CommandExecution)
                .with_action(format!("cmd-{i}"), "low".into(), false, true);
            logger.log(&event)?;
        }

        // Anchor the chain tip
        let anchor_path = tmp.path().join("anchors.jsonl");
        let (seq, hash) = read_chain_tip(logger.db_path())?.unwrap();
        assert_eq!(seq, 11);

        let genesis_hash = read_genesis_hash(logger.db_path())?.unwrap();
        let chain_id = compute_chain_id(&genesis_hash);

        let record = AnchorRecord {
            sequence: seq,
            entry_hash: hash.clone(),
            ts: "2026-01-01T00:00:00Z".to_string(),
            chain_id,
            signature: None,
        };
        append_anchor(&anchor_path, &record)?;

        // Tail-truncate: drop trigger, DELETE last 3, reinstall
        let conn = Connection::open(logger.db_path())?;
        conn.execute_batch("DROP TRIGGER IF EXISTS audit_no_delete")?;
        conn.execute(
            "DELETE FROM audit_events WHERE sequence >= 9",
            [],
        )?;
        AuditLogger::install_append_only_triggers(&conn)?;

        // Internal verify_chain PASSES (remaining chain 0..8 is valid)
        let internal_count = verify_chain(logger.db_path())?;
        assert_eq!(internal_count, 9);

        // Anchored verification FAILS (anchor points to seq=11, which is gone)
        let result = verify_chain_anchored(logger.db_path(), &anchor_path)?;
        assert!(result.internal_ok);
        match result.anchor_status {
            AnchorStatus::AnchorMismatch { expected_seq, .. } => {
                assert_eq!(expected_seq, 11);
            }
            other => panic!("expected AnchorMismatch, got: {:?}", other),
        }

        Ok(())
    }

    #[test]
    fn anchor_store_permissions_failure() -> Result<()> {
        // append_anchor on a non-writable path fails
        let result = append_anchor(
            std::path::Path::new("/proc/nonexistent/anchors.jsonl"),
            &AnchorRecord {
                sequence: 0,
                entry_hash: "abc".into(),
                ts: "2026-01-01T00:00:00Z".into(),
                chain_id: "def".into(),
                signature: None,
            },
        );
        assert!(result.is_err());
        Ok(())
    }

    #[test]
    fn anchor_missing_file_returns_no_anchor() -> Result<()> {
        // Missing anchor file -> AnchorStatus::NoAnchor, not error
        let tmp = TempDir::new()?;
        let config = AuditConfig {
            enabled: true,
            ..Default::default()
        };
        let logger = AuditLogger::new(config, tmp.path().to_path_buf())?;

        let event = AuditEvent::new(AuditEventType::CommandExecution)
            .with_action("cmd".into(), "low".into(), false, true);
        logger.log(&event)?;

        let anchor_path = tmp.path().join("nonexistent-anchors.jsonl");
        let result = verify_chain_anchored(logger.db_path(), &anchor_path)?;

        assert!(result.internal_ok);
        assert!(matches!(result.anchor_status, AnchorStatus::NoAnchor));
        assert!(result.latest_anchor.is_none());
        Ok(())
    }

    #[test]
    fn retention_unreachable_via_log() -> Result<()> {
        // Write 1001+ events via log() -> all survive (run_retention is gated out)
        let tmp = TempDir::new()?;
        let config = AuditConfig {
            enabled: true,
            max_size_mb: 100,
            ..Default::default()
        };
        let logger = AuditLogger::new(config, tmp.path().to_path_buf())?;

        for i in 0..1005 {
            let event = AuditEvent::new(AuditEventType::CommandExecution)
                .with_action(format!("cmd-{i}"), "low".into(), false, true);
            logger.log(&event)?;
        }

        let conn = Connection::open(logger.db_path())?;
        let count: i64 = conn.query_row(
            "SELECT COUNT(*) FROM audit_events",
            [],
            |r| r.get(0),
        )?;
        assert_eq!(count, 1005, "all events must survive — retention is retired");

        let chain_count = verify_chain(logger.db_path())?;
        assert_eq!(chain_count, 1005);
        Ok(())
    }

    #[test]
    fn christening_and_anchor_verify() -> Result<()> {
        // 20 events -> record_chain_boundary -> anchor -> verify -> Verified
        let tmp = TempDir::new()?;
        let config = AuditConfig {
            enabled: true,
            ..Default::default()
        };
        let logger = AuditLogger::new(config, tmp.path().to_path_buf())?;

        for i in 0..20 {
            let event = AuditEvent::new(AuditEventType::CommandExecution)
                .with_action(format!("cmd-{i}"), "low".into(), false, true);
            logger.log(&event)?;
        }

        logger.record_chain_boundary("christening: initial chain boundary")?;

        // Now anchor
        let anchor_path = tmp.path().join("anchors.jsonl");
        let (seq, hash) = read_chain_tip(logger.db_path())?.unwrap();
        assert_eq!(seq, 20); // 20 events + 1 boundary = seq 20

        let genesis_hash = read_genesis_hash(logger.db_path())?.unwrap();
        let chain_id = compute_chain_id(&genesis_hash);

        let record = AnchorRecord {
            sequence: seq,
            entry_hash: hash,
            ts: "2026-01-01T00:00:00Z".to_string(),
            chain_id: chain_id.clone(),
            signature: None,
        };
        append_anchor(&anchor_path, &record)?;

        // Verify
        let result = verify_chain_anchored(logger.db_path(), &anchor_path)?;
        assert!(result.internal_ok);
        assert!(matches!(result.anchor_status, AnchorStatus::Verified));
        assert!(result.latest_anchor.is_some());

        let anchor = result.latest_anchor.unwrap();
        assert_eq!(anchor.chain_id, chain_id);
        Ok(())
    }

    #[test]
    fn anchor_skip_on_no_advance() -> Result<()> {
        // Anchor -> same chain state -> anchor file still 1 line
        let tmp = TempDir::new()?;
        let config = AuditConfig {
            enabled: true,
            ..Default::default()
        };
        let logger = AuditLogger::new(config, tmp.path().to_path_buf())?;

        for i in 0..5 {
            let event = AuditEvent::new(AuditEventType::CommandExecution)
                .with_action(format!("cmd-{i}"), "low".into(), false, true);
            logger.log(&event)?;
        }

        let anchor_path = tmp.path().join("anchors.jsonl");
        let (seq, hash) = read_chain_tip(logger.db_path())?.unwrap();

        let genesis_hash = read_genesis_hash(logger.db_path())?.unwrap();
        let chain_id = compute_chain_id(&genesis_hash);

        // First anchor
        let record = AnchorRecord {
            sequence: seq,
            entry_hash: hash.clone(),
            ts: "2026-01-01T00:00:00Z".to_string(),
            chain_id: chain_id.clone(),
            signature: None,
        };
        append_anchor(&anchor_path, &record)?;

        // Simulate the no-advance check (as witness-anchor does)
        let latest = read_latest_anchor(&anchor_path)?.unwrap();
        let (current_seq, _) = read_chain_tip(logger.db_path())?.unwrap();
        assert_eq!(latest.sequence, current_seq, "no advance");

        // If we were to write again (we should NOT in the real flow),
        // the file would have 2 lines. Verify it currently has exactly 1.
        let content = std::fs::read_to_string(&anchor_path)?;
        let lines: Vec<&str> = content.lines().filter(|l| !l.trim().is_empty()).collect();
        assert_eq!(lines.len(), 1, "anchor file should have exactly 1 record");

        Ok(())
    }

    #[test]
    fn read_chain_tip_empty_db() -> Result<()> {
        let tmp = TempDir::new()?;
        let config = AuditConfig {
            enabled: true,
            ..Default::default()
        };
        let _logger = AuditLogger::new(config, tmp.path().to_path_buf())?;

        // DB exists but has no events
        let tip = read_chain_tip(&tmp.path().join("audit").join("audit.db"))?;
        assert!(tip.is_none());
        Ok(())
    }

    #[test]
    fn compute_chain_id_deterministic() {
        let id1 = compute_chain_id("abc123");
        let id2 = compute_chain_id("abc123");
        assert_eq!(id1, id2);
        assert_eq!(id1.len(), 64); // SHA-256 hex = 64 chars

        let id3 = compute_chain_id("different");
        assert_ne!(id1, id3);
    }

    #[test]
    fn read_latest_anchor_parses_last_line() -> Result<()> {
        let tmp = TempDir::new()?;
        let path = tmp.path().join("anchors.jsonl");

        // Write two anchor records
        let r1 = AnchorRecord {
            sequence: 5,
            entry_hash: "hash5".into(),
            ts: "2026-01-01T00:00:00Z".into(),
            chain_id: "cid".into(),
            signature: None,
        };
        let r2 = AnchorRecord {
            sequence: 10,
            entry_hash: "hash10".into(),
            ts: "2026-01-01T01:00:00Z".into(),
            chain_id: "cid".into(),
            signature: None,
        };
        append_anchor(&path, &r1)?;
        append_anchor(&path, &r2)?;

        let latest = read_latest_anchor(&path)?.unwrap();
        assert_eq!(latest.sequence, 10);
        assert_eq!(latest.entry_hash, "hash10");
        Ok(())
    }
}
