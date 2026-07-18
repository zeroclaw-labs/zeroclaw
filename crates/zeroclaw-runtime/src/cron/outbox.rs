//! Durable, instance-scoped work for plugin host services.
//!
//! The outbox is host-owned and intentionally has no guest ABI. It stores
//! public payload data plus references to instance-local secret properties;
//! plaintext secret values must never enter an envelope. Delivery is
//! at-least-once, so consumers must propagate the persisted idempotency key to
//! any external system that supports deduplication.

use std::collections::HashSet;
use std::time::Duration;

use anyhow::{Context, Result, bail};
use chrono::{DateTime, Utc};
use rusqlite::{Connection, OptionalExtension, params};
use serde::{Deserialize, Serialize};
use serde_json::Value;
use uuid::Uuid;
use zeroclaw_api::plugin_key::SecretPropertyRef;
use zeroclaw_config::schema::Config;
use zeroclaw_plugins::instance::PluginInstanceId;

use super::store::with_initialized_connection;

const ENVELOPE_SCHEMA_VERSION: i64 = 1;
const MAX_ENVELOPE_BYTES: usize = 64 * 1024;
const MAX_IDEMPOTENCY_KEY_BYTES: usize = 256;
const MAX_LEASE_TOKEN_BYTES: usize = 128;
const MAX_ATTEMPTS: i64 = 8;
const LEASE_DURATION_MS: i64 = 5 * 60 * 1_000;
const BASE_RETRY_BACKOFF_MS: i64 = 1_000;
const MAX_RETRY_BACKOFF_MS: i64 = 15 * 60 * 1_000;
const MAX_RETRY_AFTER_MS: u64 = 24 * 60 * 60 * 1_000;
const SQLITE_BUSY_TIMEOUT: Duration = Duration::from_secs(5);

#[derive(Clone, Copy, Debug, Eq, PartialEq)]
#[repr(i64)]
enum StoredState {
    Pending = 0,
    Leased = 1,
    Completed = 2,
    Failed = 3,
}

/// Host-understood work classes. Plugin-specific action strings are not part
/// of the scheduler contract.
#[derive(Clone, Copy, Debug, Eq, PartialEq)]
pub enum PluginOutboxKind {
    /// Work that becomes eligible at a host-selected time.
    Scheduled,
    /// An outbound operation that may need durable retries.
    Delivery,
}

impl PluginOutboxKind {
    const fn as_db(self) -> i64 {
        match self {
            Self::Scheduled => 0,
            Self::Delivery => 1,
        }
    }

    fn from_db(value: i64) -> Result<Self> {
        match value {
            0 => Ok(Self::Scheduled),
            1 => Ok(Self::Delivery),
            _ => bail!("unsupported plugin outbox kind {value}"),
        }
    }
}

/// Data the caller attests is safe to persist without encryption.
///
/// This marker separates durable public data from secret references at the API
/// boundary. The host cannot infer whether an arbitrary JSON string is a
/// credential, so callers remain responsible for never constructing this type
/// from resolved secret values.
#[derive(Clone, Debug, PartialEq)]
pub struct PublicOutboxData(Value);

impl PublicOutboxData {
    #[must_use]
    pub(crate) fn new(value: Value) -> Self {
        Self(value)
    }

    #[must_use]
    pub fn value(&self) -> &Value {
        &self.0
    }
}

/// Schema-versioned payload stored under one canonical plugin instance.
#[derive(Clone, Debug, PartialEq)]
pub struct PluginOutboxEnvelope {
    public_data: PublicOutboxData,
    secret_references: Vec<SecretPropertyRef>,
}

impl PluginOutboxEnvelope {
    /// Build an envelope from public data and unresolved secret references.
    pub fn new(
        public_data: PublicOutboxData,
        secret_references: Vec<SecretPropertyRef>,
    ) -> Result<Self> {
        let mut unique = HashSet::with_capacity(secret_references.len());
        if secret_references
            .iter()
            .any(|reference| !unique.insert(reference))
        {
            bail!("plugin outbox secret references must be unique");
        }
        Ok(Self {
            public_data,
            secret_references,
        })
    }

    #[must_use]
    pub fn public_data(&self) -> &PublicOutboxData {
        &self.public_data
    }

    #[must_use]
    pub fn secret_references(&self) -> &[SecretPropertyRef] {
        &self.secret_references
    }
}

/// Typed outcome applied to a claimed job.
#[derive(Clone, Copy, Debug, Eq, PartialEq)]
pub enum PluginDeliveryOutcome {
    Success,
    PermanentFailure,
    RetryAfter(Duration),
    TransientFailure,
}

/// Result of idempotently inserting a job.
#[derive(Clone, Copy, Debug, Eq, PartialEq)]
pub enum EnqueueDisposition {
    Enqueued(PluginOutboxJobId),
    Existing(PluginOutboxJobId),
}

/// Durable row identifier. Idempotency is defined by instance plus caller key,
/// not by this process-local handle.
#[derive(Clone, Copy, Debug, Eq, Hash, PartialEq)]
pub struct PluginOutboxJobId(i64);

impl PluginOutboxJobId {
    #[must_use]
    pub fn get(self) -> i64 {
        self.0
    }
}

/// Opaque lease proof required for every state transition after a claim.
#[derive(Clone, Debug, Eq, PartialEq)]
pub struct PluginOutboxLease {
    job_id: PluginOutboxJobId,
    token: String,
}

impl PluginOutboxLease {
    #[must_use]
    pub fn job_id(&self) -> PluginOutboxJobId {
        self.job_id
    }
}

/// Work returned only to a worker for the exact requested instance.
#[derive(Clone, Debug, PartialEq)]
pub struct ClaimedPluginOutboxJob {
    lease: PluginOutboxLease,
    idempotency_key: String,
    kind: PluginOutboxKind,
    envelope: PluginOutboxEnvelope,
    attempt: u32,
}

impl ClaimedPluginOutboxJob {
    #[must_use]
    pub fn lease(&self) -> &PluginOutboxLease {
        &self.lease
    }

    #[must_use]
    pub fn idempotency_key(&self) -> &str {
        &self.idempotency_key
    }

    #[must_use]
    pub fn kind(&self) -> PluginOutboxKind {
        self.kind
    }

    #[must_use]
    pub fn envelope(&self) -> &PluginOutboxEnvelope {
        &self.envelope
    }

    #[must_use]
    pub fn attempt(&self) -> u32 {
        self.attempt
    }
}

/// Typed result of applying a delivery outcome.
#[derive(Clone, Copy, Debug, Eq, PartialEq)]
pub enum CompletionDisposition {
    Completed,
    FailedPermanently,
    RetryScheduled { due_at: DateTime<Utc> },
    RetryExhausted,
    LeaseRejected,
}

/// Injected wall clock used for due and lease decisions.
pub trait OutboxClock: Send + Sync {
    fn now(&self) -> DateTime<Utc>;
}

#[derive(Clone, Copy, Debug, Default)]
pub struct SystemOutboxClock;

impl OutboxClock for SystemOutboxClock {
    fn now(&self) -> DateTime<Utc> {
        Utc::now()
    }
}

/// Injected lease-token entropy. Tokens fence stale workers after recovery.
pub trait LeaseTokenSource: Send + Sync {
    fn next_token(&self) -> String;
}

#[derive(Clone, Copy, Debug, Default)]
pub struct RandomLeaseTokenSource;

impl LeaseTokenSource for RandomLeaseTokenSource {
    fn next_token(&self) -> String {
        Uuid::new_v4().to_string()
    }
}

/// Stateless durable outbox coordinator. The database location is resolved
/// from the canonical `Config` on every operation rather than cached here.
pub struct PluginOutbox<C = SystemOutboxClock, T = RandomLeaseTokenSource> {
    clock: C,
    lease_tokens: T,
}

impl Default for PluginOutbox {
    fn default() -> Self {
        Self::new(SystemOutboxClock, RandomLeaseTokenSource)
    }
}

impl<C, T> PluginOutbox<C, T>
where
    C: OutboxClock,
    T: LeaseTokenSource,
{
    #[must_use]
    pub fn new(clock: C, lease_tokens: T) -> Self {
        Self {
            clock,
            lease_tokens,
        }
    }

    /// Insert one job or return the existing row for the same exact instance
    /// and idempotency key. Reusing a key for different work is rejected.
    pub fn enqueue(
        &self,
        config: &Config,
        instance: &PluginInstanceId,
        idempotency_key: &str,
        kind: PluginOutboxKind,
        envelope: &PluginOutboxEnvelope,
        due_at: DateTime<Utc>,
    ) -> Result<EnqueueDisposition> {
        validate_idempotency_key(idempotency_key)?;
        let payload = encode_envelope(envelope)?;
        let capability = capability_key(instance)?;
        let created_at = self.clock.now().timestamp_millis();
        let due_at = due_at.timestamp_millis();

        with_initialized_connection(config, |conn| {
            prepare_connection(conn)?;
            let changed = conn.execute(
                "INSERT INTO plugin_outbox (
                    package, capability, binding, idempotency_key, schema_version,
                    kind, payload, state, attempts, due_at, created_at
                 ) VALUES (?1, ?2, ?3, ?4, ?5, ?6, ?7, ?8, 0, ?9, ?10)
                 ON CONFLICT(package, capability, binding, idempotency_key) DO NOTHING",
                params![
                    instance.package(),
                    capability,
                    instance.binding(),
                    idempotency_key,
                    ENVELOPE_SCHEMA_VERSION,
                    kind.as_db(),
                    payload,
                    StoredState::Pending as i64,
                    due_at,
                    created_at,
                ],
            )?;

            if changed == 1 {
                return Ok(EnqueueDisposition::Enqueued(PluginOutboxJobId(
                    conn.last_insert_rowid(),
                )));
            }

            let existing = conn.query_row(
                "SELECT id, schema_version, kind, payload, due_at
                 FROM plugin_outbox
                 WHERE package = ?1 AND capability = ?2 AND binding = ?3
                   AND idempotency_key = ?4",
                params![
                    instance.package(),
                    capability,
                    instance.binding(),
                    idempotency_key,
                ],
                |row| {
                    Ok((
                        row.get::<_, i64>(0)?,
                        row.get::<_, i64>(1)?,
                        row.get::<_, i64>(2)?,
                        row.get::<_, Vec<u8>>(3)?,
                        row.get::<_, i64>(4)?,
                    ))
                },
            )?;
            if existing.1 != ENVELOPE_SCHEMA_VERSION
                || existing.2 != kind.as_db()
                || existing.3 != payload
                || existing.4 != due_at
            {
                bail!(
                    "plugin outbox idempotency key already names different work for this instance"
                );
            }
            Ok(EnqueueDisposition::Existing(PluginOutboxJobId(existing.0)))
        })
    }

    /// Atomically claim the earliest due job for one exact instance. Expired
    /// leases are recoverable after restart; their old tokens cannot commit.
    pub fn claim_due(
        &self,
        config: &Config,
        instance: &PluginInstanceId,
    ) -> Result<Option<ClaimedPluginOutboxJob>> {
        let now = self.clock.now().timestamp_millis();
        let lease_expires_at = now
            .checked_add(LEASE_DURATION_MS)
            .context("plugin outbox lease timestamp overflow")?;
        let lease_token = self.lease_tokens.next_token();
        if lease_token.is_empty()
            || lease_token.len() > MAX_LEASE_TOKEN_BYTES
            || lease_token.chars().any(char::is_control)
        {
            bail!("plugin outbox lease token source returned an invalid token");
        }
        let capability = capability_key(instance)?;

        with_initialized_connection(config, |conn| {
            prepare_connection(conn)?;

            conn.execute(
                "UPDATE plugin_outbox
                 SET state = ?1, lease_token = NULL, lease_expires_at = NULL,
                     completed_at = ?2
                 WHERE package = ?3 AND capability = ?4 AND binding = ?5
                   AND state = ?6 AND lease_expires_at <= ?2 AND attempts >= ?7",
                params![
                    StoredState::Failed as i64,
                    now,
                    instance.package(),
                    capability,
                    instance.binding(),
                    StoredState::Leased as i64,
                    MAX_ATTEMPTS,
                ],
            )?;

            let claimed = conn
                .query_row(
                    "UPDATE plugin_outbox
                     SET state = ?1, attempts = attempts + 1,
                         lease_token = ?2, lease_expires_at = ?3
                     WHERE id = (
                         SELECT id FROM plugin_outbox
                         WHERE package = ?4 AND capability = ?5 AND binding = ?6
                           AND attempts < ?7
                           AND (
                               (state = ?8 AND due_at <= ?9)
                               OR (state = ?1 AND lease_expires_at <= ?9)
                           )
                         ORDER BY due_at ASC, id ASC
                         LIMIT 1
                     )
                     AND attempts < ?7
                     AND package = ?4 AND capability = ?5 AND binding = ?6
                     AND (
                         (state = ?8 AND due_at <= ?9)
                         OR (state = ?1 AND lease_expires_at <= ?9)
                     )
                     RETURNING id, idempotency_key, schema_version, kind, payload, attempts",
                    params![
                        StoredState::Leased as i64,
                        lease_token,
                        lease_expires_at,
                        instance.package(),
                        capability,
                        instance.binding(),
                        MAX_ATTEMPTS,
                        StoredState::Pending as i64,
                        now,
                    ],
                    |row| {
                        Ok((
                            row.get::<_, i64>(0)?,
                            row.get::<_, String>(1)?,
                            row.get::<_, i64>(2)?,
                            row.get::<_, i64>(3)?,
                            row.get::<_, Vec<u8>>(4)?,
                            row.get::<_, i64>(5)?,
                        ))
                    },
                )
                .optional()?;

            let Some((id, idempotency_key, version, kind, payload, attempt)) = claimed else {
                return Ok(None);
            };
            if version != ENVELOPE_SCHEMA_VERSION {
                bail!("unsupported plugin outbox envelope schema version {version}");
            }
            let attempt = u32::try_from(attempt).context("invalid plugin outbox attempt count")?;
            Ok(Some(ClaimedPluginOutboxJob {
                lease: PluginOutboxLease {
                    job_id: PluginOutboxJobId(id),
                    token: lease_token,
                },
                idempotency_key,
                kind: PluginOutboxKind::from_db(kind)?,
                envelope: decode_envelope(&payload)?,
                attempt,
            }))
        })
    }

    /// Apply a typed delivery outcome if the exact instance still owns the
    /// unexpired lease. Wrong-instance and stale-token callers receive the same
    /// detail-free rejection.
    pub fn complete(
        &self,
        config: &Config,
        instance: &PluginInstanceId,
        lease: &PluginOutboxLease,
        outcome: PluginDeliveryOutcome,
    ) -> Result<CompletionDisposition> {
        let now = self.clock.now().timestamp_millis();
        let capability = capability_key(instance)?;

        with_initialized_connection(config, |conn| {
            prepare_connection(conn)?;
            let attempt = conn
                .query_row(
                    "SELECT attempts FROM plugin_outbox
                     WHERE id = ?1 AND package = ?2 AND capability = ?3 AND binding = ?4
                       AND state = ?5 AND lease_token = ?6 AND lease_expires_at > ?7",
                    params![
                        lease.job_id.0,
                        instance.package(),
                        capability,
                        instance.binding(),
                        StoredState::Leased as i64,
                        lease.token,
                        now,
                    ],
                    |row| row.get::<_, i64>(0),
                )
                .optional()?;
            let Some(attempt) = attempt else {
                return Ok(CompletionDisposition::LeaseRejected);
            };

            let (state, due_at, disposition) = match outcome {
                PluginDeliveryOutcome::Success => (
                    StoredState::Completed,
                    None,
                    CompletionDisposition::Completed,
                ),
                PluginDeliveryOutcome::PermanentFailure => (
                    StoredState::Failed,
                    None,
                    CompletionDisposition::FailedPermanently,
                ),
                PluginDeliveryOutcome::RetryAfter(delay) if attempt < MAX_ATTEMPTS => {
                    let delay_ms = delay.as_millis().min(u128::from(MAX_RETRY_AFTER_MS));
                    let delay_ms = i64::try_from(delay_ms)
                        .context("plugin outbox retry-after duration overflow")?;
                    let due_at = now
                        .checked_add(delay_ms)
                        .context("plugin outbox retry-after timestamp overflow")?;
                    (
                        StoredState::Pending,
                        Some(due_at),
                        CompletionDisposition::RetryScheduled {
                            due_at: datetime_from_millis(due_at)?,
                        },
                    )
                }
                PluginDeliveryOutcome::TransientFailure if attempt < MAX_ATTEMPTS => {
                    let due_at = now
                        .checked_add(retry_backoff_ms(attempt))
                        .context("plugin outbox retry timestamp overflow")?;
                    (
                        StoredState::Pending,
                        Some(due_at),
                        CompletionDisposition::RetryScheduled {
                            due_at: datetime_from_millis(due_at)?,
                        },
                    )
                }
                PluginDeliveryOutcome::RetryAfter(_) | PluginDeliveryOutcome::TransientFailure => (
                    StoredState::Failed,
                    None,
                    CompletionDisposition::RetryExhausted,
                ),
            };

            let changed = conn.execute(
                "UPDATE plugin_outbox
                 SET state = ?1, due_at = COALESCE(?2, due_at),
                     lease_token = NULL, lease_expires_at = NULL,
                     completed_at = CASE WHEN ?1 IN (?3, ?4) THEN ?5 ELSE NULL END
                 WHERE id = ?6 AND package = ?7 AND capability = ?8 AND binding = ?9
                   AND state = ?10 AND lease_token = ?11
                   AND lease_expires_at > ?5 AND attempts = ?12",
                params![
                    state as i64,
                    due_at,
                    StoredState::Completed as i64,
                    StoredState::Failed as i64,
                    now,
                    lease.job_id.0,
                    instance.package(),
                    capability,
                    instance.binding(),
                    StoredState::Leased as i64,
                    lease.token,
                    attempt,
                ],
            )?;
            if changed == 1 {
                Ok(disposition)
            } else {
                Ok(CompletionDisposition::LeaseRejected)
            }
        })
    }
}

pub(super) fn initialize_schema(conn: &Connection) -> Result<()> {
    let schema = format!(
        "CREATE TABLE IF NOT EXISTS plugin_outbox (
            id               INTEGER PRIMARY KEY AUTOINCREMENT,
            package          TEXT NOT NULL,
            capability       TEXT NOT NULL,
            binding          TEXT NOT NULL,
            idempotency_key  TEXT NOT NULL,
            schema_version   INTEGER NOT NULL,
            kind             INTEGER NOT NULL
                                 CHECK(kind BETWEEN {kind_min} AND {kind_max}),
            payload          BLOB NOT NULL,
            state            INTEGER NOT NULL
                                 CHECK(state BETWEEN {state_min} AND {state_max}),
            attempts         INTEGER NOT NULL DEFAULT 0
                                 CHECK(attempts BETWEEN 0 AND {max_attempts}),
            due_at           INTEGER NOT NULL,
            lease_token      TEXT,
            lease_expires_at INTEGER,
            created_at       INTEGER NOT NULL,
            completed_at     INTEGER,
            UNIQUE(package, capability, binding, idempotency_key),
            CHECK(
                (state = {leased} AND lease_token IS NOT NULL AND lease_expires_at IS NOT NULL)
                OR
                (state != {leased} AND lease_token IS NULL AND lease_expires_at IS NULL)
            ),
            CHECK(
                (state IN ({completed}, {failed}) AND completed_at IS NOT NULL)
                OR
                (state NOT IN ({completed}, {failed}) AND completed_at IS NULL)
            )
        );
        CREATE INDEX IF NOT EXISTS idx_plugin_outbox_claim
            ON plugin_outbox(package, capability, binding, state, due_at, lease_expires_at, id);",
        kind_min = PluginOutboxKind::Scheduled.as_db(),
        kind_max = PluginOutboxKind::Delivery.as_db(),
        state_min = StoredState::Pending as i64,
        state_max = StoredState::Failed as i64,
        max_attempts = MAX_ATTEMPTS,
        leased = StoredState::Leased as i64,
        completed = StoredState::Completed as i64,
        failed = StoredState::Failed as i64,
    );
    conn.execute_batch(&schema)
        .context("Failed to initialize plugin outbox schema")?;
    Ok(())
}

fn prepare_connection(conn: &Connection) -> Result<()> {
    conn.busy_timeout(SQLITE_BUSY_TIMEOUT)
        .context("Failed to configure plugin outbox SQLite busy timeout")
}

fn validate_idempotency_key(key: &str) -> Result<()> {
    if key.is_empty() {
        bail!("plugin outbox idempotency key must not be empty");
    }
    if key.len() > MAX_IDEMPOTENCY_KEY_BYTES {
        bail!("plugin outbox idempotency key exceeds {MAX_IDEMPOTENCY_KEY_BYTES} bytes");
    }
    if key.chars().any(char::is_control) {
        bail!("plugin outbox idempotency key must not contain control characters");
    }
    Ok(())
}

fn capability_key(instance: &PluginInstanceId) -> Result<String> {
    let value = serde_json::to_value(instance.capability())
        .context("Failed to encode plugin outbox capability")?;
    value
        .as_str()
        .map(ToOwned::to_owned)
        .context("Plugin capability did not encode as a string")
}

#[derive(Serialize)]
struct PersistedEnvelopeRef<'a> {
    public_data: &'a Value,
    #[serde(skip_serializing_if = "Vec::is_empty")]
    secret_references: Vec<&'a str>,
}

#[derive(Deserialize)]
struct PersistedEnvelope {
    public_data: Value,
    #[serde(default)]
    secret_references: Vec<String>,
}

fn encode_envelope(envelope: &PluginOutboxEnvelope) -> Result<Vec<u8>> {
    let persisted = PersistedEnvelopeRef {
        public_data: envelope.public_data().value(),
        secret_references: envelope
            .secret_references()
            .iter()
            .map(SecretPropertyRef::as_str)
            .collect(),
    };
    let encoded =
        serde_json::to_vec(&persisted).context("Failed to encode plugin outbox envelope")?;
    if encoded.len() > MAX_ENVELOPE_BYTES {
        bail!("plugin outbox envelope exceeds {MAX_ENVELOPE_BYTES} bytes");
    }
    Ok(encoded)
}

fn decode_envelope(encoded: &[u8]) -> Result<PluginOutboxEnvelope> {
    if encoded.len() > MAX_ENVELOPE_BYTES {
        bail!("persisted plugin outbox envelope exceeds {MAX_ENVELOPE_BYTES} bytes");
    }
    let envelope: PersistedEnvelope =
        serde_json::from_slice(encoded).context("Failed to decode plugin outbox envelope")?;
    let secret_references = envelope
        .secret_references
        .into_iter()
        .map(|property| SecretPropertyRef::parse(property).map_err(anyhow::Error::from))
        .collect::<Result<Vec<_>>>()?;
    PluginOutboxEnvelope::new(
        PublicOutboxData::new(envelope.public_data),
        secret_references,
    )
    .context("Persisted plugin outbox envelope violates structural invariants")
}

fn retry_backoff_ms(attempt: i64) -> i64 {
    let exponent = u32::try_from(attempt.saturating_sub(1)).unwrap_or(u32::MAX);
    BASE_RETRY_BACKOFF_MS
        .checked_shl(exponent.min(30))
        .unwrap_or(i64::MAX)
        .min(MAX_RETRY_BACKOFF_MS)
}

fn datetime_from_millis(value: i64) -> Result<DateTime<Utc>> {
    DateTime::from_timestamp_millis(value)
        .with_context(|| format!("invalid plugin outbox timestamp {value}"))
}

#[cfg(test)]
mod tests {
    use std::sync::{Arc, Barrier, Mutex};

    use chrono::TimeZone;
    use tempfile::TempDir;
    use zeroclaw_plugins::{PluginCapability, PluginManifest, instance::PluginInstanceScope};

    use super::*;

    #[derive(Clone)]
    struct TestClock(Arc<Mutex<DateTime<Utc>>>);

    impl TestClock {
        fn at(now: DateTime<Utc>) -> Self {
            Self(Arc::new(Mutex::new(now)))
        }

        fn advance(&self, duration: chrono::Duration) {
            let mut now = self.0.lock().unwrap_or_else(|error| error.into_inner());
            *now += duration;
        }
    }

    impl OutboxClock for TestClock {
        fn now(&self) -> DateTime<Utc> {
            *self.0.lock().unwrap_or_else(|error| error.into_inner())
        }
    }

    #[derive(Clone, Default)]
    struct TestLeaseTokens(Arc<std::sync::atomic::AtomicU64>);

    impl LeaseTokenSource for TestLeaseTokens {
        fn next_token(&self) -> String {
            let id = self.0.fetch_add(1, std::sync::atomic::Ordering::Relaxed);
            format!("lease-{id}")
        }
    }

    fn test_config(tmp: &TempDir) -> Config {
        Config {
            data_dir: tmp.path().join("data"),
            config_path: tmp.path().join("config.toml"),
            ..Config::default()
        }
    }

    fn instance(package: &str, binding: &str) -> PluginInstanceScope {
        let manifest = PluginManifest {
            name: package.to_string(),
            version: "0.0.0-test".to_string(),
            description: None,
            author: None,
            wasm_path: Some("plugin.wasm".to_string()),
            wasm_sha256: None,
            capabilities: vec![PluginCapability::Channel],
            permissions: Vec::new(),
            config_schema: None,
            signature: None,
            publisher_key: None,
        };
        PluginInstanceScope::from_manifest(
            &manifest,
            PluginCapability::Channel,
            binding,
            Vec::new(),
        )
        .expect("valid test instance")
    }

    fn envelope(label: &str) -> PluginOutboxEnvelope {
        PluginOutboxEnvelope::new(
            PublicOutboxData::new(serde_json::json!({"label": label})),
            vec![SecretPropertyRef::parse("credential").unwrap()],
        )
        .unwrap()
    }

    fn harness() -> (
        TempDir,
        Config,
        TestClock,
        PluginOutbox<TestClock, TestLeaseTokens>,
    ) {
        let tmp = TempDir::new().unwrap();
        let config = test_config(&tmp);
        let clock = TestClock::at(Utc.with_ymd_and_hms(2026, 7, 18, 12, 0, 0).unwrap());
        let outbox = PluginOutbox::new(clock.clone(), TestLeaseTokens::default());
        (tmp, config, clock, outbox)
    }

    fn stored_state(
        config: &Config,
        instance: &PluginInstanceId,
        idempotency_key: &str,
    ) -> StoredState {
        let capability = capability_key(instance).unwrap();
        let state = with_initialized_connection(config, |conn| {
            conn.query_row(
                "SELECT state FROM plugin_outbox
                 WHERE package = ?1 AND capability = ?2 AND binding = ?3
                   AND idempotency_key = ?4",
                params![
                    instance.package(),
                    capability,
                    instance.binding(),
                    idempotency_key,
                ],
                |row| row.get::<_, i64>(0),
            )
            .map_err(Into::into)
        })
        .unwrap();
        match state {
            value if value == StoredState::Pending as i64 => StoredState::Pending,
            value if value == StoredState::Leased as i64 => StoredState::Leased,
            value if value == StoredState::Completed as i64 => StoredState::Completed,
            value if value == StoredState::Failed as i64 => StoredState::Failed,
            _ => panic!("unexpected stored state {state}"),
        }
    }

    #[test]
    fn enqueue_is_idempotent_within_exact_instance() {
        let (_tmp, config, clock, outbox) = harness();
        let instance = instance("mail-sync", "primary");
        let due_at = clock.now();
        let first = outbox
            .enqueue(
                &config,
                instance.id(),
                "delivery-42",
                PluginOutboxKind::Delivery,
                &envelope("hello"),
                due_at,
            )
            .unwrap();
        clock.advance(chrono::Duration::seconds(1));
        let second = outbox
            .enqueue(
                &config,
                instance.id(),
                "delivery-42",
                PluginOutboxKind::Delivery,
                &envelope("hello"),
                due_at,
            )
            .unwrap();
        let EnqueueDisposition::Enqueued(first_id) = first else {
            panic!("first enqueue must insert")
        };
        assert_eq!(second, EnqueueDisposition::Existing(first_id));

        let conflict = outbox.enqueue(
            &config,
            instance.id(),
            "delivery-42",
            PluginOutboxKind::Delivery,
            &envelope("different"),
            clock.now(),
        );
        assert!(conflict.unwrap_err().to_string().contains("different work"));

        let kind_conflict = outbox.enqueue(
            &config,
            instance.id(),
            "delivery-42",
            PluginOutboxKind::Scheduled,
            &envelope("hello"),
            due_at,
        );
        assert!(
            kind_conflict
                .unwrap_err()
                .to_string()
                .contains("different work")
        );

        let due_conflict = outbox.enqueue(
            &config,
            instance.id(),
            "delivery-42",
            PluginOutboxKind::Delivery,
            &envelope("hello"),
            clock.now(),
        );
        assert!(
            due_conflict
                .unwrap_err()
                .to_string()
                .contains("different work")
        );
    }

    #[test]
    fn expired_lease_is_recovered_and_old_worker_is_fenced() {
        let (_tmp, config, clock, outbox) = harness();
        let instance = instance("calendar-sync", "primary");
        outbox
            .enqueue(
                &config,
                instance.id(),
                "renewal-1",
                PluginOutboxKind::Scheduled,
                &envelope("renew"),
                clock.now(),
            )
            .unwrap();
        let first = outbox.claim_due(&config, instance.id()).unwrap().unwrap();
        let lease_tokens = outbox.lease_tokens.clone();
        drop(outbox);
        clock.advance(chrono::Duration::milliseconds(LEASE_DURATION_MS + 1));
        let restarted = PluginOutbox::new(clock.clone(), lease_tokens);
        let recovered = restarted
            .claim_due(&config, instance.id())
            .unwrap()
            .unwrap();
        assert_eq!(recovered.attempt(), 2);
        assert_eq!(recovered.idempotency_key(), first.idempotency_key());
        assert_eq!(
            restarted
                .complete(
                    &config,
                    instance.id(),
                    first.lease(),
                    PluginDeliveryOutcome::Success,
                )
                .unwrap(),
            CompletionDisposition::LeaseRejected
        );
        assert_eq!(
            restarted
                .complete(
                    &config,
                    instance.id(),
                    recovered.lease(),
                    PluginDeliveryOutcome::Success,
                )
                .unwrap(),
            CompletionDisposition::Completed
        );
    }

    #[test]
    fn concurrent_workers_claim_once() {
        let (_tmp, config, clock, outbox) = harness();
        let instance = instance("relay", "primary");
        outbox
            .enqueue(
                &config,
                instance.id(),
                "message-1",
                PluginOutboxKind::Delivery,
                &envelope("send"),
                clock.now(),
            )
            .unwrap();

        let barrier = Arc::new(Barrier::new(3));
        let config = Arc::new(config);
        let instance_id = instance.id().clone();
        let outbox = Arc::new(outbox);
        let handles: Vec<_> = (0..2)
            .map(|_| {
                let barrier = Arc::clone(&barrier);
                let config = Arc::clone(&config);
                let instance = instance_id.clone();
                let outbox = Arc::clone(&outbox);
                std::thread::spawn(move || {
                    barrier.wait();
                    outbox.claim_due(&config, &instance).unwrap()
                })
            })
            .collect();
        barrier.wait();
        let claims = handles
            .into_iter()
            .filter_map(|handle| handle.join().unwrap())
            .count();
        assert_eq!(claims, 1);
    }

    #[test]
    fn claim_due_ignores_an_earlier_row_owned_by_another_instance() {
        let (_tmp, config, clock, outbox) = harness();
        let earlier = instance("relay", "earlier");
        let requested = instance("relay", "requested");
        outbox
            .enqueue(
                &config,
                earlier.id(),
                "other-work",
                PluginOutboxKind::Delivery,
                &envelope("other"),
                clock.now() - chrono::Duration::minutes(1),
            )
            .unwrap();
        outbox
            .enqueue(
                &config,
                requested.id(),
                "requested-work",
                PluginOutboxKind::Delivery,
                &envelope("requested"),
                clock.now(),
            )
            .unwrap();

        let requested_claim = outbox.claim_due(&config, requested.id()).unwrap().unwrap();
        assert_eq!(requested_claim.idempotency_key(), "requested-work");
        assert_eq!(requested_claim.envelope(), &envelope("requested"));

        let earlier_claim = outbox.claim_due(&config, earlier.id()).unwrap().unwrap();
        assert_eq!(earlier_claim.idempotency_key(), "other-work");
    }

    #[test]
    fn exact_instance_isolation_rejects_cross_instance_completion() {
        let (_tmp, config, clock, outbox) = harness();
        let first = instance("relay", "first");
        let second = instance("relay", "second");
        outbox
            .enqueue(
                &config,
                first.id(),
                "same-key",
                PluginOutboxKind::Delivery,
                &envelope("first"),
                clock.now(),
            )
            .unwrap();
        outbox
            .enqueue(
                &config,
                second.id(),
                "same-key",
                PluginOutboxKind::Delivery,
                &envelope("second"),
                clock.now(),
            )
            .unwrap();

        let first_claim = outbox.claim_due(&config, first.id()).unwrap().unwrap();
        assert_eq!(
            outbox
                .complete(
                    &config,
                    second.id(),
                    first_claim.lease(),
                    PluginDeliveryOutcome::Success,
                )
                .unwrap(),
            CompletionDisposition::LeaseRejected
        );
        let second_claim = outbox.claim_due(&config, second.id()).unwrap().unwrap();
        assert_eq!(second_claim.envelope(), &envelope("second"));
    }

    #[test]
    fn retry_after_uses_injected_clock_and_requested_delay() {
        let (_tmp, config, clock, outbox) = harness();
        let instance = instance("watcher", "primary");
        outbox
            .enqueue(
                &config,
                instance.id(),
                "watch-1",
                PluginOutboxKind::Scheduled,
                &envelope("renew"),
                clock.now(),
            )
            .unwrap();
        let claim = outbox.claim_due(&config, instance.id()).unwrap().unwrap();
        let expected = clock.now() + chrono::Duration::minutes(17);
        assert_eq!(
            outbox
                .complete(
                    &config,
                    instance.id(),
                    claim.lease(),
                    PluginDeliveryOutcome::RetryAfter(Duration::from_secs(17 * 60)),
                )
                .unwrap(),
            CompletionDisposition::RetryScheduled { due_at: expected }
        );
        assert!(outbox.claim_due(&config, instance.id()).unwrap().is_none());
        clock.advance(chrono::Duration::minutes(17));
        assert_eq!(
            outbox
                .claim_due(&config, instance.id())
                .unwrap()
                .unwrap()
                .attempt(),
            2
        );
    }

    #[test]
    fn retries_exhaust_and_permanent_failure_never_requeue() {
        let (_tmp, config, clock, outbox) = harness();
        let retrying = instance("retrying", "primary");
        outbox
            .enqueue(
                &config,
                retrying.id(),
                "retry",
                PluginOutboxKind::Delivery,
                &envelope("retry"),
                clock.now(),
            )
            .unwrap();
        for attempt in 1..=MAX_ATTEMPTS {
            let claim = outbox.claim_due(&config, retrying.id()).unwrap().unwrap();
            assert_eq!(i64::from(claim.attempt()), attempt);
            let disposition = outbox
                .complete(
                    &config,
                    retrying.id(),
                    claim.lease(),
                    PluginDeliveryOutcome::TransientFailure,
                )
                .unwrap();
            if attempt < MAX_ATTEMPTS {
                assert!(matches!(
                    disposition,
                    CompletionDisposition::RetryScheduled { .. }
                ));
                clock.advance(chrono::Duration::milliseconds(retry_backoff_ms(attempt)));
            } else {
                assert_eq!(disposition, CompletionDisposition::RetryExhausted);
            }
        }
        assert_eq!(
            stored_state(&config, retrying.id(), "retry"),
            StoredState::Failed
        );

        let permanent = instance("permanent", "primary");
        outbox
            .enqueue(
                &config,
                permanent.id(),
                "permanent",
                PluginOutboxKind::Delivery,
                &envelope("fail"),
                clock.now(),
            )
            .unwrap();
        let claim = outbox.claim_due(&config, permanent.id()).unwrap().unwrap();
        assert_eq!(
            outbox
                .complete(
                    &config,
                    permanent.id(),
                    claim.lease(),
                    PluginDeliveryOutcome::PermanentFailure,
                )
                .unwrap(),
            CompletionDisposition::FailedPermanently
        );
        assert!(outbox.claim_due(&config, permanent.id()).unwrap().is_none());
    }

    #[test]
    fn successful_completion_is_terminal() {
        let (_tmp, config, clock, outbox) = harness();
        let instance = instance("delivery", "primary");
        outbox
            .enqueue(
                &config,
                instance.id(),
                "success",
                PluginOutboxKind::Delivery,
                &envelope("ok"),
                clock.now(),
            )
            .unwrap();
        let claim = outbox.claim_due(&config, instance.id()).unwrap().unwrap();
        assert_eq!(
            outbox
                .complete(
                    &config,
                    instance.id(),
                    claim.lease(),
                    PluginDeliveryOutcome::Success,
                )
                .unwrap(),
            CompletionDisposition::Completed
        );
        assert_eq!(
            stored_state(&config, instance.id(), "success"),
            StoredState::Completed
        );
        assert!(outbox.claim_due(&config, instance.id()).unwrap().is_none());
    }

    #[test]
    fn schema_migration_is_idempotent_in_existing_cron_database() {
        let (tmp, config, _clock, _outbox) = harness();
        std::fs::create_dir_all(config.data_dir.join("cron")).unwrap();
        let db_path = config.data_dir.join("cron").join("jobs.db");
        let conn = Connection::open(&db_path).unwrap();
        conn.execute_batch(
            "CREATE TABLE cron_jobs (
                id TEXT PRIMARY KEY,
                expression TEXT NOT NULL,
                command TEXT NOT NULL,
                created_at TEXT NOT NULL,
                next_run TEXT NOT NULL
            );",
        )
        .unwrap();
        initialize_schema(&conn).unwrap();
        initialize_schema(&conn).unwrap();
        let table_count: i64 = conn
            .query_row(
                "SELECT COUNT(*) FROM sqlite_master WHERE type = 'table' AND name = 'plugin_outbox'",
                [],
                |row| row.get(0),
            )
            .unwrap();
        assert_eq!(table_count, 1);
        drop(conn);
        drop(tmp);
    }

    #[test]
    fn schema_constraints_reject_impossible_lifecycle_rows() {
        let (_tmp, config, clock, outbox) = harness();
        let instance = instance("constrained", "primary");
        let EnqueueDisposition::Enqueued(job_id) = outbox
            .enqueue(
                &config,
                instance.id(),
                "constrained",
                PluginOutboxKind::Delivery,
                &envelope("valid"),
                clock.now(),
            )
            .unwrap()
        else {
            panic!("fresh test database must insert the job")
        };

        with_initialized_connection(&config, |conn| {
            assert!(
                conn.execute(
                    "UPDATE plugin_outbox SET state = ?1 WHERE id = ?2",
                    params![StoredState::Leased as i64, job_id.0],
                )
                .is_err(),
                "leased state requires a token and expiry"
            );
            assert!(
                conn.execute(
                    "UPDATE plugin_outbox SET attempts = ?1 WHERE id = ?2",
                    params![MAX_ATTEMPTS + 1, job_id.0],
                )
                .is_err(),
                "attempt count must remain bounded"
            );
            assert!(
                conn.execute(
                    "UPDATE plugin_outbox SET kind = ?1 WHERE id = ?2",
                    params![PluginOutboxKind::Delivery.as_db() + 1, job_id.0],
                )
                .is_err(),
                "job kind must remain typed"
            );
            Ok(())
        })
        .unwrap();
    }

    #[test]
    fn oversized_payload_and_duplicate_secret_references_are_rejected() {
        assert!(SecretPropertyRef::parse("client_key.pem").is_ok());
        for invalid in ["../key", "key/value", "key:value", "plugin://other/key"] {
            assert!(SecretPropertyRef::parse(invalid).is_err(), "{invalid:?}");
        }

        assert!(
            PluginOutboxEnvelope::new(
                PublicOutboxData::new(Value::Null),
                vec![
                    SecretPropertyRef::parse("token").unwrap(),
                    SecretPropertyRef::parse("token").unwrap(),
                ],
            )
            .is_err()
        );

        let (_tmp, config, clock, outbox) = harness();
        let instance = instance("bounded", "primary");
        let oversized = PluginOutboxEnvelope::new(
            PublicOutboxData::new(Value::String("x".repeat(MAX_ENVELOPE_BYTES))),
            Vec::new(),
        )
        .unwrap();
        assert!(
            outbox
                .enqueue(
                    &config,
                    instance.id(),
                    "large",
                    PluginOutboxKind::Delivery,
                    &oversized,
                    clock.now(),
                )
                .unwrap_err()
                .to_string()
                .contains("exceeds")
        );
    }
}
