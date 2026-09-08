//! Audit logging for security events
//! Each audit entry is chained via a Merkle hash: `entry_hash = SHA-256(prev_hash || canonical_json)`.
//! This makes the trail tamper-evident — modifying any entry invalidates all subsequent hashes.

use anyhow::{Result, bail};
use chrono::{DateTime, Utc};
use parking_lot::Mutex;
use serde::{Deserialize, Serialize};
use sha2::{Digest, Sha256};
use std::fs::OpenOptions;
use std::io::{BufRead, BufReader, Write};
use std::path::{Path, PathBuf};
use uuid::Uuid;
use zeroclaw_config::schema::AuditConfig;

/// Well-known seed for the genesis entry's `prev_hash`.
const GENESIS_PREV_HASH: &str = "0000000000000000000000000000000000000000000000000000000000000000";

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
    /// A client certificate issuance (or renewal) was attempted: the CSR is
    /// signed but the issued-cert ledger has not committed and no certificate
    /// has been delivered. Recorded before the ledger write, so it never claims
    /// a completed issuance; the matching `CertIssued`/`CertRenewed` follows
    /// once the row commits. An attempt with no completion is an interrupted,
    /// retryable issuance.
    CertIssuanceAttempted,
    /// A client mTLS certificate was issued from the daemon CA (enrollment or
    /// operator `issue-client-cert`) AND committed to the issued-cert ledger.
    CertIssued,
    /// A client certificate was renewed over an authenticated mTLS session and
    /// committed to the issued-cert ledger.
    CertRenewed,
    /// A client certificate was revoked (status flipped in the ledger).
    CertRevoked,
    /// A certificate issuance that committed a ledger row but never completed
    /// was reconciled away: the row is discarded and the certificate - if it
    /// ever reached a client at all - is not, and never was, in the ledger.
    /// This is what closes out an unmatched `CertIssuanceAttempted` left by a
    /// process that died mid-issuance, so the trail explains the gap instead of
    /// leaving it to inference.
    CertIssuanceAbandoned,
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
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub agent_alias: Option<String>,

    /// Monotonically increasing sequence number.
    #[serde(default)]
    pub sequence: u64,
    /// SHA-256 hash of the previous entry (genesis uses `GENESIS_PREV_HASH`).
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
            agent_alias: None,
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

    /// Set the owning agent's alias for multi-agent attribution.
    /// Builder method so existing AuditEvent construction sites can
    /// add the alias without an explicit field assignment. Pass the
    /// alias bound at agent-loop entry.
    #[must_use]
    pub fn with_agent_alias(mut self, agent_alias: impl Into<String>) -> Self {
        self.agent_alias = Some(agent_alias.into());
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

/// Internal chain state tracked across writes.
struct ChainState {
    prev_hash: String,
    sequence: u64,
}

/// Audit logger
pub struct AuditLogger {
    log_path: PathBuf,
    config: AuditConfig,
    chain: Mutex<ChainState>,
    /// Signing key (loaded once at construction time if sign_events enabled)
    signing_key: Option<Vec<u8>>,
    /// Remaining [`AuditLogger::log`] calls to honour before every further one
    /// fails. Test-only; see [`AuditLogger::fail_writes_after_for_test`].
    #[cfg(test)]
    write_budget: Mutex<Option<usize>>,
    /// Remaining durable appends to honour before every further one fails at
    /// the file-write step. Test-only; see
    /// [`AuditLogger::fail_durable_write_after_for_test`].
    #[cfg(test)]
    durable_write_budget: Mutex<Option<usize>>,
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
    /// Construct a logger over `<zeroclaw_dir>/<config.log_path>`.
    ///
    /// **One instance per file.** The Merkle chain's sequence and `prev_hash`
    /// live in this struct's mutex, so the mutex serializes only the writers
    /// that go through THIS instance. Two loggers over one file each recover
    /// the same tip at construction and then both claim it, producing
    /// duplicate sequence numbers and broken links that make `verify_chain`
    /// reject the file — see `two_loggers_on_one_file_duplicate_a_sequence`.
    ///
    /// Production code must therefore never construct a logger per request or
    /// per subsystem. Build one at daemon startup with
    /// [`AuditLogger::open_shared`] and inject the `Arc`; the certificate
    /// paths reach it through `RpcContext::cert_audit`. This constructor stays
    /// public for tests and for the single startup call behind `open_shared`.
    pub fn new(config: AuditConfig, zeroclaw_dir: PathBuf) -> Result<Self> {
        // Load and validate signing key if sign_events enabled
        let signing_key = if config.sign_events {
            let key_hex = std::env::var("ZEROCLAW_AUDIT_SIGNING_KEY").map_err(|e| {
                // Do not format the VarError: VarError::NotUnicode includes the
                // raw env-var value in its Display/Debug text, which would leak
                // signing-key material into logs and error output.
                let reason = match e {
                    std::env::VarError::NotPresent => "missing",
                    std::env::VarError::NotUnicode(_) => "not_valid_unicode",
                };
                ::zeroclaw_log::record!(
                    ERROR,
                    ::zeroclaw_log::Event::new(module_path!(), ::zeroclaw_log::Action::Fail)
                        .with_outcome(::zeroclaw_log::EventOutcome::Failure)
                        .with_attrs(::serde_json::json!({"reason": reason})),
                    "audit log: sign_events=true but ZEROCLAW_AUDIT_SIGNING_KEY env var is not usable"
                );
                match e {
                    std::env::VarError::NotPresent => anyhow::Error::msg(
                        "sign_events enabled but ZEROCLAW_AUDIT_SIGNING_KEY not set",
                    ),
                    std::env::VarError::NotUnicode(_) => anyhow::Error::msg(
                        "ZEROCLAW_AUDIT_SIGNING_KEY env var is not valid UTF-8",
                    ),
                }
            })?;

            let key_bytes = hex::decode(&key_hex).map_err(|e| {
                ::zeroclaw_log::record!(
                    ERROR,
                    ::zeroclaw_log::Event::new(module_path!(), ::zeroclaw_log::Action::Fail)
                        .with_outcome(::zeroclaw_log::EventOutcome::Failure)
                        .with_attrs(::serde_json::json!({"error": format!("{e}")})),
                    "audit log: ZEROCLAW_AUDIT_SIGNING_KEY env var must be hex-encoded"
                );
                anyhow::Error::msg(format!(
                    "ZEROCLAW_AUDIT_SIGNING_KEY must be hex-encoded: {e}"
                ))
            })?;

            if key_bytes.len() != 32 {
                bail!(
                    "ZEROCLAW_AUDIT_SIGNING_KEY must be 32 bytes (64 hex chars), got {}",
                    key_bytes.len()
                );
            }

            Some(key_bytes)
        } else {
            None
        };

        let log_path = zeroclaw_dir.join(&config.log_path);
        let chain_state = recover_chain_state(&log_path);
        Ok(Self {
            log_path,
            config,
            chain: Mutex::new(chain_state),
            signing_key,
            #[cfg(test)]
            write_budget: Mutex::new(None),
            #[cfg(test)]
            durable_write_budget: Mutex::new(None),
        })
    }

    /// Open THE audit logger for `zeroclaw_dir` — the single instance every
    /// certificate path in a daemon shares.
    ///
    /// Returns an `Arc` because sharing is the whole point: enrollment,
    /// renewal and the issued-cert ledger all append to one file, and the
    /// chain is only consistent while one instance owns it (see
    /// [`AuditLogger::new`] for what a second instance does). Call this once
    /// per daemon iteration, store it in `RpcContext::cert_audit`, and clone
    /// the `Arc` into every consumer.
    pub fn open_shared(config: AuditConfig, zeroclaw_dir: PathBuf) -> Result<std::sync::Arc<Self>> {
        Ok(std::sync::Arc::new(Self::new(config, zeroclaw_dir)?))
    }

    /// Fault injection: honour the next `successes` [`AuditLogger::log`] calls,
    /// then fail every one after them.
    ///
    /// Callers that write MORE THAN ONE event to describe a single durable
    /// action need a partial audit failure - the first event landing and a
    /// later one failing - which no external manipulation of the log file can
    /// produce, because the whole sequence happens inside one call. The
    /// certificate issuance attempt/completion pair
    /// (`security::cert_ledger::CertLedger::record_issued`) is that shape.
    ///
    /// The injected failure lands right after the rotation step and before
    /// the entry is sequenced, so it models a rotate/open failure. For the
    /// later window — entry sequenced and hashed, durable append then fails —
    /// use [`AuditLogger::fail_durable_write_after_for_test`]. Both leave the
    /// hash chain consistent with the file for the caller's retry, because
    /// [`AuditLogger::log`] commits the chain state only after `sync_all`.
    #[cfg(test)]
    pub(crate) fn fail_writes_after_for_test(&self, successes: usize) {
        *self.write_budget.lock() = Some(successes);
    }

    /// Undo [`AuditLogger::fail_writes_after_for_test`].
    #[cfg(test)]
    pub(crate) fn clear_write_failure_for_test(&self) {
        *self.write_budget.lock() = None;
    }

    /// Fault injection, second phase: honour the next `successes` durable
    /// appends, then fail every one after them AT THE FILE-WRITE STEP -
    /// after the candidate entry has been sequenced, hashed, signed and
    /// serialized.
    ///
    /// [`AuditLogger::fail_writes_after_for_test`] models a rotate/open
    /// failure that lands *before* the chain is touched, so it cannot reach
    /// the window this one covers: the gap between "this entry's sequence and
    /// hash have been computed" and "that entry is durable on disk". A
    /// failure there must leave the chain state exactly as it was, so the
    /// retry reuses the same sequence and `prev_hash` and the file stays
    /// verifiable. See `chain_state_survives_a_failed_durable_append`.
    #[cfg(test)]
    pub(crate) fn fail_durable_write_after_for_test(&self, successes: usize) {
        *self.durable_write_budget.lock() = Some(successes);
    }

    /// Undo [`AuditLogger::fail_durable_write_after_for_test`].
    #[cfg(test)]
    pub(crate) fn clear_durable_write_failure_for_test(&self) {
        *self.durable_write_budget.lock() = None;
    }

    /// Compute HMAC-SHA256 signature over entry_hash when sign_events enabled.
    fn compute_signature(&self, entry_hash: &str) -> Result<Option<String>> {
        if let Some(ref key_bytes) = self.signing_key {
            use hmac::{Hmac, Mac};
            use sha2::Sha256;

            let mut mac = Hmac::<Sha256>::new_from_slice(key_bytes).map_err(|e| {
                ::zeroclaw_log::record!(
                    ERROR,
                    ::zeroclaw_log::Event::new(module_path!(), ::zeroclaw_log::Action::Fail)
                        .with_outcome(::zeroclaw_log::EventOutcome::Failure)
                        .with_attrs(::serde_json::json!({"error": format!("{e}")})),
                    "audit log: HMAC-SHA256 init rejected key length"
                );
                anyhow::Error::msg(format!("Invalid HMAC key length: {e}"))
            })?;
            mac.update(entry_hash.as_bytes());

            Ok(Some(hex::encode(mac.finalize().into_bytes())))
        } else {
            Ok(None)
        }
    }

    /// Log an event.
    ///
    /// One event is one atomic step. The chain lock spans rotation,
    /// sequencing, hashing, signing, serialization AND the durable append,
    /// and the in-memory chain state is committed only after `sync_all`
    /// returns. Two properties the trail depends on follow from that order:
    ///
    /// * **Concurrent callers cannot interleave.** If the lock were released
    ///   before the append, two threads could take sequences 0 and 1 and then
    ///   write their lines in the opposite order — `verify_chain` rejects the
    ///   file even though each write was individually correct.
    /// * **A failed append changes nothing.** Rotation, serialization, open,
    ///   write and fsync all fail before the commit, so the state stays
    ///   exactly as it was and the caller's retry reuses the same sequence
    ///   and `prev_hash`. Advancing first left the in-memory chain ahead of
    ///   the file, and every later entry then linked to a hash that was never
    ///   written.
    ///
    /// Both properties hold only WITHIN one instance: one `AuditLogger` per
    /// file is a hard requirement, see [`AuditLogger::new`].
    pub fn log(&self, event: &AuditEvent) -> Result<()> {
        if !self.config.enabled {
            return Ok(());
        }

        // Held across the whole event, released on every exit path.
        let mut state = self.chain.lock();

        // Check log size and rotate if needed
        self.rotate_if_needed()?;

        #[cfg(test)]
        {
            let mut budget = self.write_budget.lock();
            if let Some(remaining) = budget.as_mut() {
                match remaining.checked_sub(1) {
                    Some(left) => *remaining = left,
                    None => bail!("injected audit write failure"),
                }
            }
        }

        // Build the candidate entry from the current tip. Nothing below
        // mutates `state` until the append is durable.
        let mut chained = event.clone();
        chained.sequence = state.sequence;
        chained.prev_hash = state.prev_hash.clone();
        chained.entry_hash = compute_entry_hash(&state.prev_hash, &chained);

        // Compute signature if sign_events enabled
        chained.signature = self.compute_signature(&chained.entry_hash)?;

        // Serialize
        let mut line = serde_json::to_string(&chained)?.into_bytes();
        line.push(b'\n');

        #[cfg(test)]
        {
            let mut budget = self.durable_write_budget.lock();
            if let Some(remaining) = budget.as_mut() {
                match remaining.checked_sub(1) {
                    Some(left) => *remaining = left,
                    None => bail!("injected durable audit write failure"),
                }
            }
        }

        // Write
        let mut file = OpenOptions::new()
            .create(true)
            .append(true)
            .open(&self.log_path)?;

        // One `write_all` of the complete line, so the append is a single
        // syscall rather than `writeln!`'s multi-write formatting path.
        file.write_all(&line)?;
        file.sync_all()?;

        // Durable — commit the new tip. This is the only mutation of `state`.
        state.prev_hash = chained.entry_hash;
        state.sequence += 1;

        Ok(())
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

    /// Rotate log if it exceeds max size
    fn rotate_if_needed(&self) -> Result<()> {
        if let Ok(metadata) = std::fs::metadata(&self.log_path) {
            let current_size_mb = metadata.len() / (1024 * 1024);
            if current_size_mb >= u64::from(self.config.max_size_mb) {
                self.rotate()?;
            }
        }
        Ok(())
    }

    /// Rotate the log file
    fn rotate(&self) -> Result<()> {
        for i in (1..10).rev() {
            let old_name = format!("{}.{}.log", self.log_path.display().to_string(), i);
            let new_name = format!("{}.{}.log", self.log_path.display().to_string(), i + 1);
            let _ = std::fs::rename(&old_name, &new_name);
        }

        let rotated = format!("{}.1.log", self.log_path.display().to_string());
        std::fs::rename(&self.log_path, &rotated)?;
        Ok(())
    }
}

/// Recover chain state from an existing log file.
/// Returns the genesis state if the file does not exist or is empty.
fn recover_chain_state(log_path: &Path) -> ChainState {
    let file = match std::fs::File::open(log_path) {
        Ok(f) => f,
        Err(_) => {
            return ChainState {
                prev_hash: GENESIS_PREV_HASH.to_string(),
                sequence: 0,
            };
        }
    };

    let reader = BufReader::new(file);
    let mut last_entry: Option<AuditEvent> = None;
    for l in reader.lines().map_while(Result::ok) {
        if let Ok(entry) = serde_json::from_str::<AuditEvent>(&l) {
            last_entry = Some(entry);
        }
    }

    match last_entry {
        Some(entry) => ChainState {
            prev_hash: entry.entry_hash,
            sequence: entry.sequence + 1,
        },
        None => ChainState {
            prev_hash: GENESIS_PREV_HASH.to_string(),
            sequence: 0,
        },
    }
}

pub fn verify_chain(log_path: &Path) -> Result<u64> {
    let file = std::fs::File::open(log_path)?;
    let reader = BufReader::new(file);

    let mut expected_prev_hash = GENESIS_PREV_HASH.to_string();
    let mut expected_sequence: u64 = 0;

    // Attempt to load signing key from environment (optional)
    let signing_key = std::env::var("ZEROCLAW_AUDIT_SIGNING_KEY")
        .ok()
        .and_then(|key_hex| hex::decode(&key_hex).ok())
        .filter(|key_bytes| key_bytes.len() == 32);

    for (line_idx, line) in reader.lines().enumerate() {
        let line = line?;
        if line.trim().is_empty() {
            continue;
        }
        let entry: AuditEvent = serde_json::from_str(&line)?;

        // Check sequence continuity
        if entry.sequence != expected_sequence {
            bail!(
                "sequence gap at line {}: expected {}, got {}",
                line_idx + 1,
                expected_sequence,
                entry.sequence
            );
        }

        // Check prev_hash linkage
        if entry.prev_hash != expected_prev_hash {
            bail!(
                "prev_hash mismatch at line {} (sequence {}): expected {}, got {}",
                line_idx + 1,
                entry.sequence,
                expected_prev_hash,
                entry.prev_hash
            );
        }

        // Recompute and verify entry_hash
        let recomputed = compute_entry_hash(&entry.prev_hash, &entry);
        if entry.entry_hash != recomputed {
            bail!(
                "entry_hash mismatch at line {} (sequence {}): expected {}, got {}",
                line_idx + 1,
                entry.sequence,
                recomputed,
                entry.entry_hash
            );
        }

        // Verify signature if present and key is available
        if let Some(ref signature) = entry.signature
            && let Some(ref key_bytes) = signing_key
        {
            use hmac::{Hmac, Mac};
            use sha2::Sha256;

            let mut mac = Hmac::<Sha256>::new_from_slice(key_bytes).map_err(|e| {
                ::zeroclaw_log::record!(
                    ERROR,
                    ::zeroclaw_log::Event::new(module_path!(), ::zeroclaw_log::Action::Fail)
                        .with_outcome(::zeroclaw_log::EventOutcome::Failure)
                        .with_attrs(::serde_json::json!({"error": format!("{e}")})),
                    "audit log: HMAC-SHA256 verify rejected key length"
                );
                anyhow::Error::msg(format!("Invalid HMAC key length during verification: {e}"))
            })?;
            mac.update(entry.entry_hash.as_bytes());
            let expected_sig = hex::encode(mac.finalize().into_bytes());

            if signature != &expected_sig {
                bail!(
                    "signature verification failed at line {} (sequence {}): signature mismatch",
                    line_idx + 1,
                    entry.sequence
                );
            }
        }
        // If signature present but key not available, skip verification (backward compat)

        expected_prev_hash = entry.entry_hash.clone();
        expected_sequence += 1;
    }

    Ok(expected_sequence)
}

#[cfg(test)]
mod tests {
    use super::*;
    use scopeguard::defer;
    use std::sync::Mutex;
    use tempfile::TempDir;

    /// Mutex to serialize tests that read/write ZEROCLAW_AUDIT_SIGNING_KEY env var.
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
            Some("@zeroclaw_user".to_string()),
        );

        assert!(event.actor.is_some());
        let actor = event.actor.as_ref().unwrap();
        assert_eq!(actor.channel, "telegram");
        assert_eq!(actor.user_id, Some("123".to_string()));
        assert_eq!(actor.username, Some("@zeroclaw_user".to_string()));
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
    fn audit_logger_disabled_does_not_create_file() -> Result<()> {
        let tmp = TempDir::new()?;
        let config = AuditConfig {
            enabled: false,
            ..Default::default()
        };
        let logger = AuditLogger::new(config, tmp.path().to_path_buf())?;
        let event = AuditEvent::new(AuditEventType::CommandExecution);

        logger.log(&event)?;

        // File should not exist since logging is disabled
        assert!(!tmp.path().join("audit.log").exists());
        Ok(())
    }

    // ── §8.1 Log rotation tests ─────────────────────────────

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

        let log_path = tmp.path().join("audit.log");
        assert!(log_path.exists(), "audit log file must be created");

        let content = tokio::fs::read_to_string(&log_path).await?;
        assert!(!content.is_empty(), "audit log must not be empty");

        let parsed: AuditEvent = serde_json::from_str(content.trim())?;
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

        let log_path = tmp.path().join("audit.log");
        let content = tokio::fs::read_to_string(&log_path).await?;
        let parsed: AuditEvent = serde_json::from_str(content.trim())?;

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
    fn audit_rotation_creates_numbered_backup() -> Result<()> {
        let tmp = TempDir::new()?;
        let config = AuditConfig {
            enabled: true,
            max_size_mb: 0, // Force rotation on first write
            ..Default::default()
        };
        let logger = AuditLogger::new(config, tmp.path().to_path_buf())?;

        // Write initial content that triggers rotation
        let log_path = tmp.path().join("audit.log");
        std::fs::write(&log_path, "initial content\n")?;

        let event = AuditEvent::new(AuditEventType::CommandExecution);
        logger.log(&event)?;

        let rotated = format!("{}.1.log", log_path.display().to_string());
        assert!(
            std::path::Path::new(&rotated).exists(),
            "rotation must create .1.log backup"
        );
        Ok(())
    }

    // ── Merkle hash-chain tests ─────────────────────────────

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

        let log_path = tmp.path().join("audit.log");
        let content = std::fs::read_to_string(&log_path)?;
        let parsed: AuditEvent = serde_json::from_str(content.trim())?;

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

        // Write several events
        for i in 0..5 {
            let event = AuditEvent::new(AuditEventType::CommandExecution).with_action(
                format!("cmd-{}", i),
                "low".to_string(),
                false,
                true,
            );
            logger.log(&event)?;
        }

        let log_path = tmp.path().join("audit.log");
        let count = verify_chain(&log_path)?;
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

        // Tamper with the second entry (change the command text)
        let log_path = tmp.path().join("audit.log");
        let content = std::fs::read_to_string(&log_path)?;
        let lines: Vec<&str> = content.lines().collect();
        assert_eq!(lines.len(), 3);

        let mut entry: serde_json::Value = serde_json::from_str(lines[1])?;
        entry["action"]["command"] = serde_json::Value::String("TAMPERED".to_string());
        let tampered_line = serde_json::to_string(&entry)?;

        let tampered_content = format!("{}\n{}\n{}\n", lines[0], tampered_line, lines[2]);
        std::fs::write(&log_path, tampered_content)?;

        // Verification must fail
        let result = verify_chain(&log_path);
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

        // Remove the second entry to create a sequence gap
        let log_path = tmp.path().join("audit.log");
        let content = std::fs::read_to_string(&log_path)?;
        let lines: Vec<&str> = content.lines().collect();
        let gapped_content = format!("{}\n{}\n", lines[0], lines[2]);
        std::fs::write(&log_path, gapped_content)?;

        let result = verify_chain(&log_path);
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
        let log_path = tmp.path().join("audit.log");

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
        let count = verify_chain(&log_path)?;
        assert_eq!(count, 4);
        Ok(())
    }

    // ── HMAC signing tests ──────────────────────────────────

    #[test]
    fn signature_present_when_sign_events_enabled() -> Result<()> {
        let _guard = ENV_MUTEX.lock().unwrap();
        let old_key = std::env::var("ZEROCLAW_AUDIT_SIGNING_KEY").ok();
        defer! {
            if let Some(key) = old_key {
                // SAFETY: test-only, single-threaded test runner.
                unsafe { std::env::set_var("ZEROCLAW_AUDIT_SIGNING_KEY", key) };
            } else {
                // SAFETY: test-only, single-threaded test runner.
                unsafe { std::env::remove_var("ZEROCLAW_AUDIT_SIGNING_KEY") };
            }
        }

        let tmp = TempDir::new()?;
        let test_key = "a".repeat(64); // 64 hex chars = 32 bytes
        // SAFETY: test-only, single-threaded test runner.
        unsafe { std::env::set_var("ZEROCLAW_AUDIT_SIGNING_KEY", &test_key) };

        let config = AuditConfig {
            enabled: true,
            sign_events: true,
            ..Default::default()
        };
        let logger = AuditLogger::new(config, tmp.path().to_path_buf())?;
        let event = AuditEvent::new(AuditEventType::CommandExecution);

        logger.log(&event)?;

        let log_path = tmp.path().join("audit.log");
        let content = std::fs::read_to_string(&log_path)?;
        let parsed: AuditEvent = serde_json::from_str(content.trim())?;

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

        let log_path = tmp.path().join("audit.log");
        let content = std::fs::read_to_string(&log_path)?;
        let parsed: AuditEvent = serde_json::from_str(content.trim())?;

        assert!(
            parsed.signature.is_none(),
            "signature must be absent when sign_events=false"
        );
        Ok(())
    }

    #[test]
    fn signature_computed_over_entry_hash() -> Result<()> {
        let _guard = ENV_MUTEX.lock().unwrap();
        let old_key = std::env::var("ZEROCLAW_AUDIT_SIGNING_KEY").ok();
        defer! {
            if let Some(key) = old_key {
                // SAFETY: test-only, single-threaded test runner.
                unsafe { std::env::set_var("ZEROCLAW_AUDIT_SIGNING_KEY", key) };
            } else {
                // SAFETY: test-only, single-threaded test runner.
                unsafe { std::env::remove_var("ZEROCLAW_AUDIT_SIGNING_KEY") };
            }
        }

        let tmp = TempDir::new()?;
        let test_key = "b".repeat(64);
        // SAFETY: test-only, single-threaded test runner.
        unsafe { std::env::set_var("ZEROCLAW_AUDIT_SIGNING_KEY", &test_key) };

        let config = AuditConfig {
            enabled: true,
            sign_events: true,
            ..Default::default()
        };
        let logger = AuditLogger::new(config, tmp.path().to_path_buf())?;
        let event = AuditEvent::new(AuditEventType::CommandExecution);

        logger.log(&event)?;

        let log_path = tmp.path().join("audit.log");
        let content = std::fs::read_to_string(&log_path)?;
        let parsed: AuditEvent = serde_json::from_str(content.trim())?;

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
        let old_key = std::env::var("ZEROCLAW_AUDIT_SIGNING_KEY").ok();
        defer! {
            // Only restore if it was a valid 64-char key
            if let Some(key) = old_key.as_ref().filter(|k| k.len() == 64) {
                // SAFETY: test-only, single-threaded test runner.
                unsafe { std::env::set_var("ZEROCLAW_AUDIT_SIGNING_KEY", key) };
            } else {
                // SAFETY: test-only, single-threaded test runner.
                unsafe { std::env::remove_var("ZEROCLAW_AUDIT_SIGNING_KEY") };
            }
        }

        // SAFETY: test-only, single-threaded test runner.
        unsafe { std::env::remove_var("ZEROCLAW_AUDIT_SIGNING_KEY") };

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
                err_msg.contains("ZEROCLAW_AUDIT_SIGNING_KEY not set"),
                "error: {}",
                err_msg
            );
        }

        Ok(())
    }

    #[test]
    fn constructor_fails_if_signing_key_invalid_hex() -> Result<()> {
        let _guard = ENV_MUTEX.lock().unwrap();
        let old_key = std::env::var("ZEROCLAW_AUDIT_SIGNING_KEY").ok();
        defer! {
            // Only restore if it was a valid 64-char key
            if let Some(key) = old_key.as_ref().filter(|k| k.len() == 64) {
                // SAFETY: test-only, single-threaded test runner.
                unsafe { std::env::set_var("ZEROCLAW_AUDIT_SIGNING_KEY", key) };
            } else {
                // SAFETY: test-only, single-threaded test runner.
                unsafe { std::env::remove_var("ZEROCLAW_AUDIT_SIGNING_KEY") };
            }
        }

        // SAFETY: test-only, single-threaded test runner.
        unsafe { std::env::set_var("ZEROCLAW_AUDIT_SIGNING_KEY", "not-valid-hex") };

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

    #[cfg(unix)]
    #[test]
    fn constructor_fails_if_signing_key_not_unicode_and_does_not_leak_value() -> Result<()> {
        use std::ffi::OsStr;
        use std::os::unix::ffi::OsStrExt;

        let _guard = ENV_MUTEX.lock().unwrap();
        let old_key = std::env::var_os("ZEROCLAW_AUDIT_SIGNING_KEY");
        defer! {
            if let Some(key) = old_key {
                // SAFETY: test-only, single-threaded test runner.
                unsafe { std::env::set_var("ZEROCLAW_AUDIT_SIGNING_KEY", key) };
            } else {
                // SAFETY: test-only, single-threaded test runner.
                unsafe { std::env::remove_var("ZEROCLAW_AUDIT_SIGNING_KEY") };
            }
        }

        let secret_bytes: &[u8] = b"ab\xFFc";
        let bad_value = OsStr::from_bytes(secret_bytes);
        // SAFETY: test-only, single-threaded test runner.
        unsafe { std::env::set_var("ZEROCLAW_AUDIT_SIGNING_KEY", bad_value) };

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
            assert!(err_msg.contains("not valid UTF-8"), "error: {}", err_msg);
            assert!(
                !err_msg.contains("ab"),
                "error message must not contain the raw signing-key value: {}",
                err_msg
            );
        }

        Ok(())
    }

    #[test]
    fn constructor_fails_if_signing_key_wrong_length() -> Result<()> {
        let _guard = ENV_MUTEX.lock().unwrap();
        let old_key = std::env::var("ZEROCLAW_AUDIT_SIGNING_KEY").ok();
        defer! {
            // Only restore if it was a valid 64-char key
            if let Some(key) = old_key.as_ref().filter(|k| k.len() == 64) {
                // SAFETY: test-only, single-threaded test runner.
                unsafe { std::env::set_var("ZEROCLAW_AUDIT_SIGNING_KEY", key) };
            } else {
                // SAFETY: test-only, single-threaded test runner.
                unsafe { std::env::remove_var("ZEROCLAW_AUDIT_SIGNING_KEY") };
            }
        }

        // 30 bytes = 60 hex chars (not 32 bytes)
        let short_key = "c".repeat(60);
        // SAFETY: test-only, single-threaded test runner.
        unsafe { std::env::set_var("ZEROCLAW_AUDIT_SIGNING_KEY", &short_key) };
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
        let old_key = std::env::var("ZEROCLAW_AUDIT_SIGNING_KEY").ok();
        defer! {
            if let Some(key) = old_key {
                // SAFETY: test-only, single-threaded test runner.
                unsafe { std::env::set_var("ZEROCLAW_AUDIT_SIGNING_KEY", key) };
            } else {
                // SAFETY: test-only, single-threaded test runner.
                unsafe { std::env::remove_var("ZEROCLAW_AUDIT_SIGNING_KEY") };
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
        let old_key = std::env::var("ZEROCLAW_AUDIT_SIGNING_KEY").ok();
        defer! {
            if let Some(key) = old_key {
                // SAFETY: test-only, single-threaded test runner.
                unsafe { std::env::set_var("ZEROCLAW_AUDIT_SIGNING_KEY", key) };
            } else {
                // SAFETY: test-only, single-threaded test runner.
                unsafe { std::env::remove_var("ZEROCLAW_AUDIT_SIGNING_KEY") };
            }
        }

        let tmp = TempDir::new()?;
        let test_key = "f".repeat(64);
        // SAFETY: test-only, single-threaded test runner.
        unsafe { std::env::set_var("ZEROCLAW_AUDIT_SIGNING_KEY", &test_key) };

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

        let log_path = tmp.path().join("audit.log");
        let content = std::fs::read_to_string(&log_path)?;
        let lines: Vec<&str> = content.lines().collect();
        let event1: AuditEvent = serde_json::from_str(lines[0])?;
        let event2: AuditEvent = serde_json::from_str(lines[1])?;

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
        let old_key = std::env::var("ZEROCLAW_AUDIT_SIGNING_KEY").ok();
        defer! {
            if let Some(key) = old_key.as_ref().filter(|k| k.len() == 64) {
                // SAFETY: test-only, single-threaded test runner.
                unsafe { std::env::set_var("ZEROCLAW_AUDIT_SIGNING_KEY", key) };
            } else {
                // SAFETY: test-only, single-threaded test runner.
                unsafe { std::env::remove_var("ZEROCLAW_AUDIT_SIGNING_KEY") };
            }
        }

        let tmp = TempDir::new()?;
        let log_path = tmp.path().join("audit.log");
        let test_key = "a1".repeat(32); // 64 hex chars = 32 bytes

        // First logger with sign_events=false (unsigned records)
        {
            // SAFETY: test-only, single-threaded test runner.
            unsafe { std::env::remove_var("ZEROCLAW_AUDIT_SIGNING_KEY") };
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
            unsafe { std::env::set_var("ZEROCLAW_AUDIT_SIGNING_KEY", &test_key) };
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
        unsafe { std::env::set_var("ZEROCLAW_AUDIT_SIGNING_KEY", &test_key) };
        let count = verify_chain(&log_path)?;
        assert_eq!(count, 4, "should verify all 4 records");

        // Verify that first 2 records have no signature, last 2 have signatures
        let content = std::fs::read_to_string(&log_path)?;
        let lines: Vec<&str> = content.lines().collect();
        assert_eq!(lines.len(), 4);

        let rec0: AuditEvent = serde_json::from_str(lines[0])?;
        let rec1: AuditEvent = serde_json::from_str(lines[1])?;
        let rec2: AuditEvent = serde_json::from_str(lines[2])?;
        let rec3: AuditEvent = serde_json::from_str(lines[3])?;

        assert!(rec0.signature.is_none(), "first unsigned record");
        assert!(rec1.signature.is_none(), "second unsigned record");
        assert!(rec2.signature.is_some(), "first signed record");
        assert!(rec3.signature.is_some(), "second signed record");

        Ok(())
    }

    // ── Durability: chain state must not outrun the file ────

    /// A write/sync failure AFTER the entry is sequenced and hashed must not
    /// advance the in-memory chain. If it does, the next event starts from a
    /// `prev_hash`/`sequence` that never reached the file and every later
    /// entry is unverifiable - the audit trail is destroyed by a transient
    /// disk error on a path that is on by default.
    #[test]
    fn chain_state_survives_a_failed_durable_append() -> Result<()> {
        let tmp = TempDir::new()?;
        let config = AuditConfig {
            enabled: true,
            max_size_mb: 10,
            ..Default::default()
        };
        let logger = AuditLogger::new(config, tmp.path().to_path_buf())?;
        let log_path = tmp.path().join("audit.log");

        let event = |tag: &str| {
            AuditEvent::new(AuditEventType::CertRenewed).with_action(
                tag.to_string(),
                "low".to_string(),
                false,
                true,
            )
        };

        logger.log(&event("before"))?;

        // Fail at the durable-append step: sequencing, hashing, signing and
        // serialization all succeed, then the file write fails.
        logger.fail_durable_write_after_for_test(0);
        let err = logger
            .log(&event("lost"))
            .expect_err("an injected durable-write failure must surface to the caller");
        assert!(
            err.to_string().contains("durable audit write"),
            "unexpected error: {err}"
        );
        logger.clear_durable_write_failure_for_test();

        // The next event must reuse the sequence and prev_hash the failed
        // append never committed.
        logger.log(&event("after"))?;

        let count = verify_chain(&log_path)?;
        assert_eq!(
            count, 2,
            "the failed append must leave no gap: exactly the two durable events"
        );

        let content = std::fs::read_to_string(&log_path)?;
        let seqs: Vec<u64> = content
            .lines()
            .filter(|l| !l.trim().is_empty())
            .map(|l| serde_json::from_str::<AuditEvent>(l).map(|e| e.sequence))
            .collect::<std::result::Result<_, _>>()?;
        assert_eq!(seqs, vec![0, 1], "sequences must stay consecutive");
        Ok(())
    }

    /// The daemon shares ONE logger across enrollment, renewal and every other
    /// certificate path, so concurrent callers meet inside a single instance.
    /// Sequencing, hashing and the durable append must be one atomic step, or
    /// two writers interleave their lines and the file no longer verifies.
    #[test]
    fn concurrent_writers_on_one_shared_logger_keep_the_chain_verifiable() -> Result<()> {
        const WRITERS: usize = 8;
        const PER_WRITER: usize = 8;

        let tmp = TempDir::new()?;
        let config = AuditConfig {
            enabled: true,
            max_size_mb: 10,
            ..Default::default()
        };
        let logger = std::sync::Arc::new(AuditLogger::new(config, tmp.path().to_path_buf())?);
        let log_path = tmp.path().join("audit.log");

        // A barrier so the writers actually overlap instead of queueing behind
        // each other's thread spawn.
        let barrier = std::sync::Arc::new(std::sync::Barrier::new(WRITERS));
        let mut handles = Vec::with_capacity(WRITERS);
        for writer in 0..WRITERS {
            let logger = std::sync::Arc::clone(&logger);
            let barrier = std::sync::Arc::clone(&barrier);
            handles.push(std::thread::spawn(move || {
                barrier.wait();
                for i in 0..PER_WRITER {
                    let event = AuditEvent::new(AuditEventType::CertRenewed).with_action(
                        format!("renew-{writer}-{i}"),
                        "low".to_string(),
                        false,
                        true,
                    );
                    logger.log(&event).expect("concurrent audit write");
                }
            }));
        }
        for handle in handles {
            handle.join().expect("writer thread panicked");
        }

        let total = (WRITERS * PER_WRITER) as u64;
        let count = verify_chain(&log_path)?;
        assert_eq!(count, total, "every concurrent event must be in the chain");

        let content = std::fs::read_to_string(&log_path)?;
        let seqs: Vec<u64> = content
            .lines()
            .filter(|l| !l.trim().is_empty())
            .map(|l| serde_json::from_str::<AuditEvent>(l).map(|e| e.sequence))
            .collect::<std::result::Result<_, _>>()?;
        assert_eq!(
            seqs,
            (0..total).collect::<Vec<u64>>(),
            "sequences must be strictly consecutive with no duplicates"
        );
        Ok(())
    }

    /// Why [`AuditLogger::new`] carries a one-instance-per-file invariant, in
    /// executable form: two loggers over the same file each recover the same
    /// chain tip and then both claim it. The mutex inside an instance cannot
    /// see across instances, so this corruption is unreachable only as long as
    /// production builds exactly one logger per audit file and shares it -
    /// which is what `RpcContext::cert_audit` is for.
    #[test]
    fn two_loggers_on_one_file_duplicate_a_sequence() -> Result<()> {
        let tmp = TempDir::new()?;
        let config = || AuditConfig {
            enabled: true,
            max_size_mb: 10,
            ..Default::default()
        };
        let log_path = tmp.path().join("audit.log");

        // Both recover the genesis tip before either has written.
        let first = AuditLogger::new(config(), tmp.path().to_path_buf())?;
        let second = AuditLogger::new(config(), tmp.path().to_path_buf())?;

        let event = |tag: &str| {
            AuditEvent::new(AuditEventType::CertIssued).with_action(
                tag.to_string(),
                "low".to_string(),
                false,
                true,
            )
        };
        first.log(&event("enrollment"))?;
        second.log(&event("renewal"))?;

        let err = verify_chain(&log_path)
            .expect_err("two writers over one audit file must corrupt the chain");
        assert!(
            err.to_string().contains("sequence gap"),
            "expected a duplicated sequence, got: {err}"
        );
        Ok(())
    }
}
