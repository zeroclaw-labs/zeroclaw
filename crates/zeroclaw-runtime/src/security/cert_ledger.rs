//! Daemon-owned issued-certificate ledger.
//!
//! The single canonical record of which device holds which client certificate
//! (device id + SHA-256 fingerprint + validity + status), the join key back to
//! the gateway pairing `devices.db` (`token_hash`), and the choke point that
//! writes the append-only certificate audit trail. The daemon owns the CA, so it
//! owns this ledger; when the gateway is present it READS the ledger (one store,
//! two readers - no third device store, per AGENTS.md no-duplicate-state).
//!
//! Issuance is a two-phase commit across those two stores: the row lands in a
//! `pending` state that NO reader treats as a credential, and is promoted to
//! its final status only once the completion audit event is durable. A row this
//! ledger vouches for therefore always has a matching completion event, and
//! every failure mode - including a process that dies mid-issuance - leaves at
//! most a `pending` row, which the next open reconciles away
//! ([`CertLedger::record_issued`]).
//!
//! Promotion happens BEFORE the certificate reaches its keyholder, so *delivery*
//! is tracked as a second, explicitly reconciled dimension: `delivered_at`, set
//! by [`CertLedger::mark_delivered`] at the real delivery/publication boundary
//! and swept by `reconcile_undelivered_issuances` at the first issuance, ledger
//! open, or explicit [`CertLedger::sweep_undelivered_certificates`] after the
//! TTL elapses (not on a timer - see that function for the residual). See
//! [`CertLedger::record_issued`] for why promotion-before-delivery is the
//! deliberately chosen failure direction.
//!
//! Revocation is sourced here. The renew RPC (`cert/renew`) refuses a
//! revoked-but-unexpired cert immediately by consulting [`CertLedger::status_of`]
//! (threat A5). The WSS handshake-time refusal is wired separately against this
//! ledger via [`CertLedger::revoked_fingerprints`] / [`CertLedger::is_revoked`].

use anyhow::{Context, Result, bail};
use parking_lot::Mutex;
use rusqlite::{Connection, OptionalExtension, params};
use std::path::Path;
use std::sync::Arc;

use super::audit::{AuditEvent, AuditEventType, AuditLogger};

/// The `issued_certs.status` value for a row that is committed but whose
/// issuance has not been recorded as complete in the audit trail.
///
/// Deliberately outside [`CertStatus`]: a pending row is a certificate the
/// ledger does NOT vouch for, and no consumer may resolve it to a usable
/// status. Every read either filters it out or reports the fingerprint as
/// unknown; see [`CertLedger::record_issued`] for the state machine.
const PENDING: &str = "pending";

/// How old a `pending` row must be before open-time reconciliation treats it
/// as crash residue. The stage->promote flip happens inside one
/// `record_issued` call and completes in well under a second, but the ledger
/// is opened per request (renewal) and per process (CLI), so a concurrent
/// open CAN observe another connection's in-flight staging - reconciling that
/// row out from under it makes the promotion fail spuriously. Anything older
/// than this window cannot be in flight and is safe to resolve.
const PENDING_RECONCILE_GRACE_SECS: i64 = 60;

/// The `issued_certs` schema revision this build expects, stamped into
/// `PRAGMA user_version`.
///
/// * 0 - pre-versioned: `CHECK(status IN ('active','revoked'))`, no `pending`.
/// * 1 - `pending` admitted by the CHECK, so the issuance two-phase commit in
///   [`CertLedger::record_issued`] can stage a row.
/// * 2 - nullable `delivered_at`, so an active row records whether its
///   certificate actually reached its keyholder
///   ([`CertLedger::mark_delivered`]).
const SCHEMA_VERSION: i64 = 2;

/// The `issued_certs` column definitions at [`SCHEMA_VERSION`].
///
/// Shared by the fresh-create path and the migration rebuild so a migrated
/// table can never drift from a freshly created one - the classic migration
/// bug, and the one that would silently reintroduce the old CHECK.
const ISSUED_CERTS_COLUMNS: &str = "
     fingerprint TEXT PRIMARY KEY,
     device_id   TEXT NOT NULL,
     token_hash  TEXT NOT NULL DEFAULT '',
     not_before  INTEGER NOT NULL,
     not_after   INTEGER NOT NULL,
     -- 'pending' is the pre-completion state of the issuance two-phase
     -- commit, never a credential; see record_issued.
     status      TEXT NOT NULL DEFAULT 'active'
                     CHECK(status IN ('active','revoked','pending')),
     issued_at   INTEGER NOT NULL,
     actor       TEXT NOT NULL DEFAULT '',
     -- NULL until the certificate is confirmed handed to its keyholder; see
     -- mark_delivered and reconcile_undelivered_issuances. Deliberately
     -- separate from `status`: an active-but-undelivered row is a credential
     -- that MIGHT exist in the wild, so it must stay recorded (and revocable)
     -- rather than be hidden.
     delivered_at INTEGER
";

/// How long an ACTIVE row may sit with no recorded delivery before
/// `reconcile_undelivered_issuances` revokes it.
///
/// One hour. The upper bound on a real delivery is tiny by comparison - the
/// enrollment endpoint caps a whole connection at
/// `crate::enroll::CONN_TIMEOUT_SECS` (15s), renewal answers inside one RPC
/// call, and the operator CLI publishes its files in the same process - so an
/// hour cannot fire on a slow-but-live delivery even across a wildly loaded
/// host or a clock nudge. It is also ~700x shorter than the 30-day certificate
/// lifetime, so the window in which an undelivered credential could be used by
/// whoever *did* end up with it is bounded to something an operator can reason
/// about rather than a month.
const UNDELIVERED_CERT_TTL_SECS: i64 = 3_600;

/// The revocation `actor` recorded for a certificate swept by
/// `reconcile_undelivered_issuances`, so the audit trail
/// distinguishes it from an operator's deliberate revoke.
const UNDELIVERED_RECONCILE_ACTOR: &str = "reconcile:undelivered";

/// Disambiguates the scratch file each revocation-list materialization writes
/// before renaming it into place, so concurrent materializations cannot clobber
/// one another's temp file. See `CertLedger::materialize_on`.
static MATERIALIZE_SEQ: std::sync::atomic::AtomicU64 = std::sync::atomic::AtomicU64::new(0);

/// Every index the ledger relies on. Recreated verbatim after a rebuild,
/// because `DROP TABLE` takes the old table's indexes with it.
const ISSUED_CERTS_INDEXES: &str = "
     CREATE INDEX IF NOT EXISTS idx_issued_certs_device ON issued_certs(device_id);
     CREATE INDEX IF NOT EXISTS idx_issued_certs_status ON issued_certs(status);
     CREATE INDEX IF NOT EXISTS idx_issued_certs_token  ON issued_certs(token_hash);
";

/// The columns EVERY pre-v2 shape carries, named explicitly for the migration
/// copy so the rebuild is insensitive to column ORDER and fails loudly rather
/// than silently shifting values if the shape ever changes.
///
/// `delivered_at` is deliberately absent: it does not exist in a v0 or v1
/// table, so it cannot be selected from one. The rebuild supplies it, which is
/// where the backfill rule lives (see `migrate_schema`).
const ISSUED_CERTS_COLUMN_LIST: &str =
    "fingerprint, device_id, token_hash, not_before, not_after, status, issued_at, actor";

/// Scratch table the rebuild stages into before swapping it in.
const MIGRATION_TABLE: &str = "issued_certs_migrated";

/// Status of an issued certificate the ledger vouches for.
///
/// There is no variant for the transient `pending` state on purpose: readers
/// see a fingerprint this ledger vouches for, or they see nothing at all. See
/// [`CertLedger::record_issued`] for the state machine.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum CertStatus {
    Active,
    Revoked,
}

impl CertStatus {
    fn as_str(self) -> &'static str {
        match self {
            CertStatus::Active => "active",
            CertStatus::Revoked => "revoked",
        }
    }

    /// Parse a stored status, or `None` for one this ledger does not vouch for
    /// ([`PENDING`], or any value a future schema adds).
    ///
    /// The permissive `_ => Active` this replaced was the dangerous default: it
    /// would resolve a pending - that is, undelivered - certificate into an
    /// active credential for any reader that forgot to filter it out.
    fn from_db(s: &str) -> Option<CertStatus> {
        match s {
            "active" => Some(CertStatus::Active),
            "revoked" => Some(CertStatus::Revoked),
            _ => None,
        }
    }
}

/// How an issuance was authorized; controls the audit `actor` semantics so the
/// primary (self-service enrollment) path is never recorded with a blank actor.
#[derive(Debug, Clone)]
pub enum IssuanceActor {
    /// Self-service enrollment authorized by a pairing token. The audit actor is
    /// `enroll:<token-hash prefix>` so the evidence ties back to the pairing.
    Enrollment { token_hash: String },
    /// Operator-driven issuance via the `security issue-client-cert` CLI.
    Operator,
}

impl IssuanceActor {
    /// The `actor` string stored in the ledger row and the audit event.
    pub fn label(&self) -> String {
        match self {
            IssuanceActor::Enrollment { token_hash } => {
                let prefix: String = token_hash.chars().take(8).collect();
                format!("enroll:{prefix}")
            }
            IssuanceActor::Operator => "operator".to_string(),
        }
    }

    /// The `token_hash` join key to the pairing `devices.db`, when present.
    pub fn token_hash(&self) -> &str {
        match self {
            IssuanceActor::Enrollment { token_hash } => token_hash,
            IssuanceActor::Operator => "",
        }
    }
}

/// A row in the issued-cert ledger.
#[derive(Debug, Clone)]
pub struct LedgerEntry {
    /// Stable device id; equals the issued cert subject CN (the identity namespace).
    pub device_id: String,
    /// SHA-256 fingerprint (lowercase hex) of the issued cert DER. Primary key.
    pub fingerprint: String,
    /// `notBefore`, unix seconds.
    pub not_before: i64,
    /// `notAfter`, unix seconds.
    pub not_after: i64,
    /// Current status.
    pub status: CertStatus,
    /// Join key to the pairing `devices.db` (empty for operator CLI issuance).
    pub token_hash: String,
    /// Who authorized the issuance (`enroll:<prefix>` or `operator`).
    pub actor: String,
    /// When the cert was issued/recorded, unix seconds.
    pub issued_at: i64,
}

/// Marker carried in the error chain when an issuance is refused because its
/// precondition no longer holds - a renewal whose certificate was revoked while
/// the renewal was in flight.
///
/// A stable substring rather than a typed error because it has to survive the
/// `anyhow` chain and the JSON-RPC string that carries it to the client, which
/// is the only place the distinction is actionable: the client must re-enroll
/// rather than retry.
pub const ISSUANCE_PRECONDITION_FAILED: &str = "issuance precondition failed";

/// An extra condition a revocation's compare-and-set requires of the row.
#[derive(Debug, Clone, Copy)]
enum RevokePrecondition {
    /// Revoke any active row - an operator's deliberate revoke, which is
    /// correct whatever else is true of the certificate.
    Any,
    /// Revoke only while the row is STILL undelivered.
    ///
    /// The undelivered sweep scans for stale rows, releases the connection, and
    /// then revokes them one at a time. A delivery can commit in that gap: the
    /// client's response write succeeds and `mark_delivered` marks the row
    /// microseconds after the sweep decided it was abandoned. Without this
    /// condition the sweep would go on to revoke a certificate that had just
    /// been delivered, taking a working client offline for no reason - and
    /// unrecoverably, since revocation is permanent.
    ///
    /// Re-testing `delivered_at IS NULL` inside the UPDATE makes delivery win
    /// that race, which is the right winner: a delivered certificate is by
    /// definition not an undelivered one.
    StillUndelivered,
}

impl RevokePrecondition {
    fn sql_suffix(self) -> &'static str {
        match self {
            RevokePrecondition::Any => "",
            RevokePrecondition::StillUndelivered => " AND delivered_at IS NULL",
        }
    }
}

/// Interleavings a test needs to force at a point production code cannot be
/// paused from the outside.
///
/// Every race this ledger defends against lives between two statements that a
/// second connection can slip between. Reproducing one by racing real threads
/// is inherently flaky, so each defended window gets a hook the test fills with
/// "run the competing operation now" - making the interleaving deterministic
/// and the regression meaningful rather than probabilistic.
#[cfg(test)]
#[derive(Default)]
pub(crate) struct LedgerTestHooks {
    /// After `stage_pending` commits, before the completion audit and promote.
    pub after_stage: Mutex<Option<Box<dyn Fn() + Send + Sync>>>,
    /// After the undelivered sweep has collected its stale set, before it
    /// revokes any of them.
    pub after_stale_scan: Mutex<Option<Box<dyn Fn() + Send + Sync>>>,
    /// After the revocation snapshot has been read and staged, before it is
    /// renamed over the file the verifier reads.
    pub before_crl_rename: Mutex<Option<Box<dyn Fn() + Send + Sync>>>,
    /// After a device-wide revocation has read the device's active set, before
    /// it flips those rows.
    pub after_device_snapshot: Mutex<Option<Box<dyn Fn() + Send + Sync>>>,
}

/// The daemon's issued-certificate ledger over SQLite (`<data_dir>/tls/ledger.db`).
pub struct CertLedger {
    conn: Mutex<Connection>,
    audit: Option<Arc<AuditLogger>>,
    /// Where revocations are materialized for the WSS verifier to read
    /// (`<data_dir>/tls/revoked`). `None` for an in-memory ledger.
    revoked_path: Option<std::path::PathBuf>,
    #[cfg(test)]
    pub(crate) hooks: LedgerTestHooks,
}

/// The revoked-fingerprint list the daemon's WSS mTLS verifier reads for
/// connect-time revocation refusal (A5). The ledger materializes it on revoke.
pub fn revoked_list_path(data_dir: &Path) -> std::path::PathBuf {
    data_dir.join("tls").join("revoked")
}

/// The revoked-fingerprint list the WSS verifier will *actually* read: the
/// operator's `[wss.client_auth].crl_path` when set, otherwise the ledger
/// default under `<data_dir>/tls/revoked`.
///
/// Revocation must materialize to THIS path. Materializing to the default while
/// the verifier honours a configured override lets `revoke-client-cert` report
/// success while the next handshake still accepts the certificate — the
/// transport design of record requires revocation to fail closed.
pub fn effective_revoked_list_path(
    data_dir: &Path,
    configured_crl_path: Option<&str>,
) -> std::path::PathBuf {
    match configured_crl_path.map(str::trim).filter(|p| !p.is_empty()) {
        Some(p) => std::path::PathBuf::from(p),
        None => revoked_list_path(data_dir),
    }
}

impl CertLedger {
    /// Open (creating if absent) the ledger at `<data_dir>/tls/ledger.db`. The CA
    /// already lives under `<data_dir>/tls/`, so the ledger sits beside it.
    pub fn open(data_dir: &Path, audit: Option<Arc<AuditLogger>>) -> Result<Self> {
        Self::open_at(data_dir, audit, revoked_list_path(data_dir))
    }

    /// Open the ledger with an explicit materialization target, for callers that
    /// know the verifier reads a configured `crl_path` rather than the default.
    /// See [`effective_revoked_list_path`].
    pub fn open_at(
        data_dir: &Path,
        audit: Option<Arc<AuditLogger>>,
        revoked_path: std::path::PathBuf,
    ) -> Result<Self> {
        let tls_dir = data_dir.join("tls");
        std::fs::create_dir_all(&tls_dir)
            .with_context(|| format!("create tls dir {}", tls_dir.display()))?;
        let db_path = tls_dir.join("ledger.db");
        let conn = Connection::open(&db_path)
            .with_context(|| format!("open cert ledger DB: {}", db_path.display()))?;
        // Name the file: a migration or reconciliation error is only actionable
        // if the operator knows which ledger to look at.
        Self::init(conn, audit, Some(revoked_path))
            .with_context(|| format!("initialize cert ledger DB: {}", db_path.display()))
    }

    /// In-memory ledger for unit tests.
    pub fn open_in_memory(audit: Option<Arc<AuditLogger>>) -> Result<Self> {
        Self::init(
            Connection::open_in_memory().context("open in-memory cert ledger")?,
            audit,
            None,
        )
    }

    fn init(
        mut conn: Connection,
        audit: Option<Arc<AuditLogger>>,
        revoked_path: Option<std::path::PathBuf>,
    ) -> Result<Self> {
        conn.execute_batch(
            "PRAGMA journal_mode = WAL;
             PRAGMA synchronous = NORMAL;
             PRAGMA busy_timeout = 5000;
             PRAGMA temp_store = MEMORY;",
        )
        .context("set cert-ledger PRAGMAs")?;
        // Bring the schema up to date BEFORE anything reads or writes the
        // table, so a migrated ledger is then reconciled like any other.
        Self::migrate_schema(&mut conn)?;
        let ledger = Self {
            conn: Mutex::new(conn),
            audit,
            revoked_path,
            #[cfg(test)]
            hooks: LedgerTestHooks::default(),
        };
        // Resolve anything a previous process left mid-issuance BEFORE this
        // ledger answers a single query. Pending first: a pending row is not a
        // credential at all, so it is discarded outright, and the undelivered
        // sweep that follows then sees only rows this ledger vouches for.
        ledger.reconcile_pending_issuances()?;
        ledger.reconcile_undelivered_issuances()?;
        // Refresh the materialized revocation list so it reflects the ledger at
        // startup (covers a missing/stale file).
        ledger.materialize_revocations()?;
        Ok(ledger)
    }

    /// Bring `conn` to [`SCHEMA_VERSION`], creating or rebuilding
    /// `issued_certs` as needed.
    ///
    /// `CREATE TABLE IF NOT EXISTS` does NOT touch a table that already exists,
    /// so widening the status CHECK in the schema literal alone left every
    /// ledger written by an earlier revision of this branch stuck on the
    /// two-value constraint. Such a daemon opened its ledger successfully and
    /// then failed EVERY enrollment and renewal at `CHECK constraint failed:
    /// status IN ('active','revoked')` the moment the issuance path staged a
    /// `pending` row. An existing table has to be REBUILT, not merely
    /// re-declared.
    ///
    /// The rebuild is the standard SQLite table rewrite inside ONE transaction:
    /// stage a table at the current schema, copy every column by name, drop the
    /// old table, rename, recreate the indexes `DROP TABLE` removed, and stamp
    /// `user_version`. SQLite gives both DDL and `user_version` full
    /// transactional semantics, so ANY failure rolls the whole thing back and
    /// leaves the original table and its rows exactly as they were - there is
    /// no half-migrated state to recover from.
    ///
    /// Two shapes have shipped on this branch (v0 and v1: identical columns and
    /// indexes, differing only in the status CHECK), and neither has
    /// `delivered_at`. Both therefore migrate to v2 in the SAME single pass -
    /// the copy selects only the columns they share, and the rebuild supplies
    /// `delivered_at` itself.
    ///
    /// **Backfill rule: a row that exists at migration time gets
    /// `delivered_at = issued_at`.** Those rows predate delivery tracking, so
    /// the ledger has no evidence either way - and the two possible defaults are
    /// not symmetric. Leaving them NULL would mark every certificate on the
    /// host as never-delivered, and the undelivered sweep would then revoke the
    /// operator's entire live fleet on the first upgraded start. Treating them
    /// as delivered preserves exactly the status quo the operator had before the
    /// upgrade: they remain active, revocable, and visible. Pinned by
    /// `migrating_a_v1_ledger_backfills_delivery_and_never_mass_revokes`.
    fn migrate_schema(conn: &mut Connection) -> Result<()> {
        let version: i64 = conn
            .query_row("PRAGMA user_version", [], |r| r.get(0))
            .context("read cert-ledger schema version")?;
        if version == SCHEMA_VERSION {
            return Ok(());
        }
        if version > SCHEMA_VERSION {
            // Fail closed: a ledger written by a newer build may use states
            // this binary would mis-read, and silently operating on it risks
            // publishing or discarding credentials on bad assumptions.
            bail!(
                "cert ledger is at schema v{version}, newer than the v{SCHEMA_VERSION} this build \
                 understands; upgrade ZeroClaw, or move the ledger aside to start fresh (issued \
                 certificates would then need re-enrollment)"
            );
        }

        let existing: i64 = conn
            .query_row(
                "SELECT COUNT(*) FROM sqlite_master WHERE type = 'table' AND name = 'issued_certs'",
                [],
                |r| r.get(0),
            )
            .context("probe for an existing issued_certs table")?;

        if existing == 0 {
            // Fresh ledger: create at the current schema and stamp it, so this
            // path never looks like a pre-versioned table on the next open.
            conn.execute_batch(&format!(
                "CREATE TABLE IF NOT EXISTS issued_certs ({ISSUED_CERTS_COLUMNS});
                 {ISSUED_CERTS_INDEXES}
                 PRAGMA user_version = {SCHEMA_VERSION};"
            ))
            .context("create cert-ledger schema")?;
            return Ok(());
        }

        let tx = conn
            .transaction()
            .context("begin cert-ledger schema migration")?;
        tx.execute_batch(&format!(
            "CREATE TABLE {MIGRATION_TABLE} ({ISSUED_CERTS_COLUMNS});
             INSERT INTO {MIGRATION_TABLE} ({ISSUED_CERTS_COLUMN_LIST}, delivered_at)
                 SELECT {ISSUED_CERTS_COLUMN_LIST}, issued_at FROM issued_certs;
             DROP TABLE issued_certs;
             ALTER TABLE {MIGRATION_TABLE} RENAME TO issued_certs;
             {ISSUED_CERTS_INDEXES}
             PRAGMA user_version = {SCHEMA_VERSION};"
        ))
        .with_context(|| {
            format!(
                "rebuild the cert-ledger issued_certs table from schema v{version} to \
                 v{SCHEMA_VERSION}; the existing ledger was rolled back and left unchanged, so \
                 no certificate records were lost"
            )
        })?;
        tx.commit().context("commit cert-ledger schema migration")
    }

    /// Resolve rows a previous process left in the `pending` state.
    ///
    /// The pending -> final flip happens inside the same
    /// [`CertLedger::record_issued`] call that committed the row, so ANY
    /// pending row still present when the ledger is opened belongs to a process
    /// that died - or a compensation that could not run - before the issuance
    /// completed. Such a row is an undelivered certificate by construction:
    /// discard it, and record WHY, so the unmatched `CertIssuanceAttempted` in
    /// the trail is closed out rather than left to inference.
    ///
    /// The audit event is written BEFORE the delete, matching the rest of this
    /// module: a failing audit logger leaves the row pending - invisible to
    /// every reader, and retried at the next open - instead of erasing it with
    /// no record.
    fn reconcile_pending_issuances(&self) -> Result<()> {
        let stale = {
            let conn = self.conn.lock();
            let mut stmt = conn
                .prepare(
                    "SELECT fingerprint, device_id, not_before, not_after, actor
                         FROM issued_certs WHERE status = ?1 AND issued_at < ?2",
                )
                .context("prepare pending-issuance reconciliation")?;
            let rows = stmt
                .query_map(
                    params![PENDING, now_unix() - PENDING_RECONCILE_GRACE_SECS],
                    |r| {
                        Ok((
                            r.get::<_, String>(0)?,
                            r.get::<_, String>(1)?,
                            r.get::<_, i64>(2)?,
                            r.get::<_, i64>(3)?,
                            r.get::<_, String>(4)?,
                        ))
                    },
                )
                .context("query pending issuances")?;
            rows.collect::<rusqlite::Result<Vec<_>>>()
                .context("collect pending issuances")?
        };
        for (fingerprint, device_id, not_before, not_after, actor) in stale {
            self.audit_cert_fields(
                CertFacts {
                    device_id: &device_id,
                    actor: &actor,
                    fingerprint: &fingerprint,
                    not_before,
                    not_after,
                },
                CertAuditStage::Abandoned,
            )?;
            self.conn
                .lock()
                .execute(
                    "DELETE FROM issued_certs WHERE fingerprint = ?1 AND status = ?2",
                    params![fingerprint, PENDING],
                )
                .context("discard a pending issuance")?;
        }
        Ok(())
    }

    /// Revoke every ACTIVE row whose certificate was never confirmed delivered
    /// and is older than `UNDELIVERED_CERT_TTL_SECS`.
    ///
    /// The other half of the issuance protocol. `record_issued` publishes the
    /// row BEFORE the caller can hand the certificate over (see there for why
    /// that direction is deliberate), so an active row is a claim that the
    /// certificate *may* exist in the wild, not that it does. Delivery closes
    /// that gap from the other side: the call site that owns the real
    /// delivery/publication boundary calls [`CertLedger::mark_delivered`] on
    /// success, and anything still unmarked once the TTL has passed is swept
    /// here.
    ///
    /// Revoking - rather than deleting - is what makes the sweep safe. The
    /// keyholder that never completed its enrollment cannot legitimately be
    /// depending on this certificate, but *something* may hold the bytes, so
    /// the fingerprint goes into the materialized revocation list the WSS
    /// verifier reads and stays a permanent, audited record. Deleting it would
    /// un-record a credential that might exist, which is the one thing this
    /// ledger must never do.
    ///
    /// # When this runs, precisely
    ///
    /// A stale row is revoked at **the first issuance, ledger open, or explicit
    /// sweep after the TTL elapses** - not on a timer. Three triggers:
    ///
    /// - every [`CertLedger::open`], which for the renew RPC is once per call
    ///   and for the daemon is once per start;
    /// - the start of every [`CertLedger::record_issued`], so any new issuance
    ///   or renewal cleans up what earlier ones stranded - this is what gives
    ///   the long-lived enrollment handle a bound at all, since it is opened
    ///   once for the daemon's whole lifetime;
    /// - [`CertLedger::sweep_undelivered_certificates`], for a caller that
    ///   knows it is at a good moment to reconcile.
    ///
    /// **Residual:** a daemon that goes completely idle on the certificate
    /// paths - no enrollment, no renewal, no restart - sweeps at its next such
    /// activity rather than exactly one hour in. The TTL bounds the age of a
    /// row that any of these triggers will tolerate, not wall-clock time since
    /// the failure. Concretely: on a daemon that does no certificate work, a
    /// ghost certificate stays out of the CRL, so it can still complete the WSS
    /// handshake - that plane authorizes by CA chain and revocation list and
    /// never consults this ledger. It cannot RENEW (the renew RPC opens a
    /// ledger, which sweeps before it reads status), and the next enrollment or
    /// renewal by anyone revokes it. A timer would close only the
    /// no-certificate-activity case; that is a deliberate tradeoff, not an
    /// oversight, and the operator-facing docs state the same bound.
    fn reconcile_undelivered_issuances(&self) -> Result<()> {
        let cutoff = now_unix() - UNDELIVERED_CERT_TTL_SECS;
        let stale: Vec<String> = {
            let conn = self.conn.lock();
            let mut stmt = conn
                .prepare(
                    "SELECT fingerprint FROM issued_certs
                         WHERE status = 'active' AND delivered_at IS NULL AND issued_at < ?1",
                )
                .context("prepare undelivered-issuance reconciliation")?;
            let rows = stmt
                .query_map(params![cutoff], |r| r.get::<_, String>(0))
                .context("query undelivered issuances")?;
            rows.collect::<rusqlite::Result<Vec<String>>>()
                .context("collect undelivered issuances")?
        };
        // The scan above released the connection lock, so between it and the
        // revocations below a delivery can commit. The revocation therefore
        // re-tests `delivered_at IS NULL` inside its own UPDATE rather than
        // trusting this list.
        #[cfg(test)]
        if let Some(hook) = self.hooks.after_stale_scan.lock().as_ref() {
            hook();
        }
        for fingerprint in stale {
            // revoke_matching is the whole revocation contract: it flips the
            // row, materializes the enforcement file from the in-transaction
            // view, and audits a CertRevoked naming this sweep as the actor. An
            // undelivered certificate is revoked by exactly the same path an
            // operator's revoke takes - no second, weaker mechanism.
            //
            // StillUndelivered is what makes a delivery that lands mid-sweep
            // win: the row it names is no longer undelivered, the UPDATE
            // matches nothing, and the sweep moves on without revoking a
            // certificate its client is at that moment starting to use.
            self.revoke_matching(
                &fingerprint,
                UNDELIVERED_RECONCILE_ACTOR,
                RevokePrecondition::StillUndelivered,
            )
            .with_context(|| format!("revoke undelivered certificate {fingerprint}"))?;
        }
        Ok(())
    }

    /// Run the undelivered sweep on this handle, for a caller holding a
    /// long-lived ledger that wants to reconcile without reopening.
    ///
    /// The enrollment endpoint is the case this exists for: it builds ONE
    /// ledger for the daemon's lifetime, so without a trigger like this its
    /// only sweep would be the one at startup. Best-effort by design - it
    /// reports whether the sweep ran, and a caller on an unrelated path should
    /// log rather than fail, since a sweep failure does not make the caller's
    /// own work unsafe and the rows stay eligible for the next attempt.
    pub fn sweep_undelivered_certificates(&self) -> Result<()> {
        self.reconcile_undelivered_issuances()
    }

    /// Record that the certificate behind `fingerprint` actually reached its
    /// keyholder. Returns true if this call is the one that marked it.
    ///
    /// Called by the site that owns the real delivery/publication boundary -
    /// the enrollment response write, the renewal response construction, the
    /// operator CLI's staged-file rename - and only on that boundary's success.
    /// Until then the row is active but undelivered, and
    /// `reconcile_undelivered_issuances` will revoke it.
    ///
    /// Idempotent: the `delivered_at IS NULL` guard means a repeat call reports
    /// false and leaves the FIRST delivery time standing, so the recorded
    /// instant is when the credential actually went out rather than whenever
    /// something last touched the row.
    ///
    /// Only an ACTIVE row is markable. Marking a `pending` row would claim
    /// delivery of a certificate the ledger has not vouched for; marking a
    /// revoked one would contradict the revocation.
    pub fn mark_delivered(&self, fingerprint: &str) -> Result<bool> {
        let conn = self.conn.lock();
        let changed = conn
            .execute(
                "UPDATE issued_certs SET delivered_at = ?2
                     WHERE fingerprint = ?1 AND status = 'active' AND delivered_at IS NULL",
                params![fingerprint, now_unix()],
            )
            .context("mark issued cert delivered")?;
        Ok(changed == 1)
    }

    /// Rewrite the revoked-fingerprint file from the SQLite truth (atomic temp +
    /// rename). This is what makes a revoke take effect at the next handshake -
    /// the WSS verifier re-reads the file when its mtime changes. No-op for an
    /// in-memory ledger.
    /// Rewrite the revocation file from the ledger's committed truth, under the
    /// SAME exclusive write lock a revocation takes.
    ///
    /// The lock is the point. Reading the revoked set, writing the scratch file
    /// and renaming it are three steps, and a revocation committing between the
    /// read and the rename would be published to the file and then immediately
    /// overwritten by this call's older snapshot - a committed revocation
    /// silently vanishing from the file the WSS verifier reads, which is the
    /// one direction revocation may never fail.
    ///
    /// `BEGIN IMMEDIATE` takes SQLite's write lock up front, and
    /// [`CertLedger::mark_revoked`] holds that same lock across its own flip,
    /// materialization and commit. So the two cannot interleave: one runs to
    /// completion before the other reads anything. That lock is held by SQLite
    /// itself, so it serializes across independent connections AND across
    /// processes sharing the data dir - which a `parking_lot` mutex on this
    /// handle would not, and this ledger is opened per request and per CLI
    /// invocation.
    ///
    /// A read-only `DEFERRED` transaction would NOT do: it takes only a read
    /// lock, which a concurrent writer is free to pass straight through.
    pub fn materialize_revocations(&self) -> Result<()> {
        let Some(path) = self.revoked_path.clone() else {
            return Ok(());
        };
        let mut conn = self.conn.lock();
        let tx = conn
            .transaction_with_behavior(rusqlite::TransactionBehavior::Immediate)
            .context("begin revocation materialization")?;
        self.materialize_on(&tx, Some(path.as_path()))?;
        // Commits no rows - it releases the write lock, and only once the file
        // on disk matches what this transaction read.
        tx.commit().context("commit revocation materialization")
    }

    /// Materialize the revocation file from `conn`'s current view. Taking the
    /// connection explicitly lets [`CertLedger::mark_revoked`] pass its open
    /// transaction, enforcing a pending revocation BEFORE committing it - a
    /// failed file write then rolls the status flip back instead of leaving a
    /// committed revoked row the verifier never sees.
    ///
    /// Callers MUST hold SQLite's write lock (an `IMMEDIATE` transaction, or a
    /// transaction that has already written). Every caller does; see
    /// [`CertLedger::materialize_revocations`] for why a read-lock caller would
    /// be able to publish a stale snapshot over a newer one.
    fn materialize_on(&self, conn: &Connection, revoked_path: Option<&Path>) -> Result<()> {
        let Some(path) = revoked_path else {
            return Ok(());
        };
        let revoked = Self::revoked_fingerprints_on(conn)?;
        let body = if revoked.is_empty() {
            String::new()
        } else {
            format!("{}\n", revoked.join("\n"))
        };
        if let Some(dir) = path.parent() {
            std::fs::create_dir_all(dir).with_context(|| format!("create {}", dir.display()))?;
        }
        // A temp name unique to this call, not the fixed `revoked.tmp` this
        // replaced. Every ledger open materializes, so N ledgers opened
        // concurrently against one data_dir - eight clients renewing at once,
        // each of which opens its own handle - all wrote the SAME temp path and
        // then renamed it. The first rename won and the rest failed with
        // ENOENT, so an ordinary burst of concurrent renewals turned into
        // "atomically replace the revocation list: No such file or directory"
        // and refused the renewals. Uniqueness by pid + counter covers both
        // threads in this process and a second process sharing the data dir.
        let unique = format!(
            "tmp.{}.{}",
            std::process::id(),
            MATERIALIZE_SEQ.fetch_add(1, std::sync::atomic::Ordering::Relaxed)
        );
        let tmp = path.with_extension(unique);
        std::fs::write(&tmp, body).with_context(|| format!("write {}", tmp.display()))?;
        // The window a competing revocation would have to slip through. It
        // cannot: every caller holds SQLite's write lock across this whole
        // function. The seam exists so a test can PROVE that rather than assert
        // it in a comment.
        #[cfg(test)]
        if let Some(hook) = self.hooks.before_crl_rename.lock().as_ref() {
            hook();
        }
        if let Err(e) = std::fs::rename(&tmp, path) {
            // Do not leave the scratch file behind for an operator to wonder at.
            let _ = std::fs::remove_file(&tmp);
            return Err(e).context("atomically replace the revocation list");
        }
        Ok(())
    }

    /// Record an issuance across both durable surfaces as a two-phase commit.
    /// `renewal` selects `CertRenewed` vs `CertIssued` for the completion
    /// event.
    ///
    /// SQLite and the append-only audit file cannot share one transaction, so
    /// the row carries the protocol instead: it is committed in the `pending`
    /// state - which no reader resolves to a credential - and promoted to
    /// `entry.status` only once the completion event is durable.
    ///
    /// 1. `CertIssuanceAttempted`, BEFORE any row exists. A failure here leaves
    ///    the ledger untouched.
    /// 2. the row, as `pending`. A failure here leaves an attempt with no
    ///    completion and nothing the ledger vouches for.
    /// 3. `CertIssued`/`CertRenewed`, AFTER the row commits, so a completion
    ///    event means the row exists.
    /// 4. the promotion to `entry.status`, which is what publishes the
    ///    certificate to every reader.
    ///
    /// The two invariants this buys, in the order they matter:
    ///
    /// - **A row this ledger vouches for was always audited as complete.**
    ///   Step 4 cannot run before step 3 succeeds, so no failure - of the
    ///   completion write's rotation, open, serialization, write or sync - can
    ///   publish a certificate the caller never delivered. That is the failure
    ///   that used to strand an ACTIVE row for an undelivered credential, and
    ///   because the client's retry carries a fresh CSR (a different
    ///   fingerprint) it left a SECOND active row rather than replacing the
    ///   first.
    /// - **No failure publishes anything.** Every step returns `Err` and
    ///   callers must not hand the certificate to the client unless this
    ///   returns `Ok`. When a failure follows step 2, the pending row this call
    ///   staged is removed on the same connection; if that compensation cannot
    ///   run (or the process dies first) the row stays `pending`, which is
    ///   still invisible to every reader, and `reconcile_pending_issuances`
    ///   discards it at the next open.
    ///
    /// The converse is deliberately NOT claimed: a completion event without a
    /// row is possible, when step 4 fails. The caller still gets `Err` and
    /// still delivers nothing, so the residue is a fingerprint that is audited
    /// but absent - which fails closed - rather than a live credential with no
    /// audit.
    ///
    /// Re-recording a fingerprint the ledger already holds is idempotent and
    /// never downgrades it: step 2 does not disturb an existing row, and step 4
    /// rewrites it. A failed completion for such a call therefore leaves the
    /// established row exactly as it was, with its old validity - correct,
    /// since the caller is returning an error rather than delivering the
    /// renewed certificate.
    ///
    /// # Why promotion happens before delivery
    ///
    /// Step 4 publishes the row while the caller still holds the certificate:
    /// the enrollment response has not been written, the renewal reply has not
    /// been serialized, the operator CLI has not renamed its staged files. That
    /// ordering is chosen, not incidental.
    ///
    /// The inverse - deliver first, promote after - fails in the strictly worse
    /// direction. A crash in *that* window hands a client a live, CA-signed
    /// certificate whose row is later reconciled away, so the ledger has no
    /// record of a credential that exists in the wild: nothing to list, nothing
    /// to revoke, and a certificate that keeps authenticating until it expires
    /// because the WSS verifier authorizes by CA chain, not by ledger
    /// membership. A ghost row is the opposite failure and a recoverable one -
    /// the ledger over-records a credential that may not exist, and an
    /// over-recorded credential can be revoked. **This ledger must never
    /// under-record a credential that might exist in the wild.**
    ///
    /// Delivery is therefore tracked as its own dimension rather than folded
    /// into `status`: `delivered_at` stays NULL until
    /// [`CertLedger::mark_delivered`] is called at the real
    /// delivery/publication boundary, and
    /// `reconcile_undelivered_issuances` revokes - never deletes -
    /// whatever is still unmarked once
    /// `UNDELIVERED_CERT_TTL_SECS` has passed. The ghost row ends up in the
    /// materialized revocation list, while a delivered certificate is never at
    /// risk of vanishing from the record.
    ///
    /// Every call sweeps stale undelivered rows before staging anything, which
    /// is what bounds a ghost row on a handle that is never reopened - the
    /// enrollment endpoint holds exactly one for the daemon's lifetime. The
    /// sweep is age-gated at the TTL, so it can never touch the row this call
    /// is about to create, nor any other issuance still in flight.
    pub fn record_issued(&self, entry: &LedgerEntry, renewal: bool) -> Result<()> {
        self.record_issued_requiring(entry, renewal, None)
    }

    /// [`CertLedger::record_issued`] for a renewal, which may only publish
    /// while the certificate it renews is STILL active.
    ///
    /// Renewal reads the presenting certificate's status, resolves its device,
    /// signs a new certificate, and records it - four steps, on a connection
    /// the operator's `revoke-client-cert` does not share. An operator revoking
    /// that device in the middle of them would see their revocation succeed and
    /// then watch the renewal hand the same device a brand-new active
    /// certificate: revocation reported, device still connected. That is the
    /// worst outcome this ledger has, because the operator believes the device
    /// is off the network.
    ///
    /// `still_active` closes it by making the presenting fingerprint a
    /// precondition of the issuance commit itself, re-tested inside the
    /// transaction that publishes the new row. Revocation therefore always
    /// wins: either it lands before the check and the renewal is refused, or it
    /// lands after the new row is published and `revoke_device` - which
    /// revokes every active certificate the device holds - takes the new one
    /// too. There is no ordering in which the device keeps a usable
    /// certificate.
    ///
    /// A refused renewal returns [`ISSUANCE_PRECONDITION_FAILED`] in its error
    /// chain. It is retryable only in the sense that re-enrolling is the
    /// client's correct next move; retrying the renewal will keep failing,
    /// because the certificate it presents is revoked.
    pub fn record_issued_requiring(
        &self,
        entry: &LedgerEntry,
        renewal: bool,
        still_active: Option<&str>,
    ) -> Result<()> {
        // Best-effort, deliberately NOT `?`. This sweep is maintenance for
        // OTHER issuances; failing it would turn an unwritable CRL - or a
        // failing audit sink on some unrelated stale row - into a refusal to
        // issue for a client that has done nothing wrong. The stale rows stay
        // eligible for the next trigger, so nothing is lost by deferring. At
        // open the same failure IS fatal, because there is no in-flight
        // operation to protect and a broken ledger should surface loudly.
        if let Err(error) = self.reconcile_undelivered_issuances() {
            ::zeroclaw_log::record!(
                WARN,
                ::zeroclaw_log::Event::new(module_path!(), ::zeroclaw_log::Action::Note)
                    .with_outcome(::zeroclaw_log::EventOutcome::Failure)
                    .with_attrs(::serde_json::json!({ "error": format!("{error:#}") })),
                "cert ledger: could not sweep undelivered certificates before this issuance; \
                 they stay eligible for the next issuance, ledger open, or explicit sweep"
            );
        }
        // Fail fast, before the attempt event: a renewal whose certificate is
        // already revoked has nothing to record. This is an optimization and a
        // cleaner audit trail, NOT the guarantee - the authoritative re-test is
        // inside `promote_staged`'s transaction, because only a check in the
        // same transaction as the publish can exclude a revocation landing
        // between them.
        self.require_still_active(still_active)?;
        self.audit_cert(entry, CertAuditStage::Attempted { renewal })?;
        let staged = self.stage_pending(entry)?;
        #[cfg(test)]
        if let Some(hook) = self.hooks.after_stage.lock().as_ref() {
            hook();
        }
        if let Err(err) = self.audit_cert(entry, CertAuditStage::Completed { renewal }) {
            self.discard_staged(entry, staged);
            return Err(err);
        }
        if let Err(err) = self.promote_staged(entry, still_active) {
            self.discard_staged(entry, staged);
            return Err(err);
        }
        Ok(())
    }

    /// Refuse unless `fingerprint` (when given) is an active row right now.
    ///
    /// Advisory on its own - the row can be revoked the instant after this
    /// returns - so it is used only to fail early. The binding check is the
    /// same predicate evaluated inside the publishing transaction.
    fn require_still_active(&self, fingerprint: Option<&str>) -> Result<()> {
        let Some(fingerprint) = fingerprint else {
            return Ok(());
        };
        let conn = self.conn.lock();
        Self::assert_still_active(&conn, fingerprint)
    }

    /// The precondition itself, evaluated on whatever connection - or open
    /// transaction - the caller supplies.
    fn assert_still_active(conn: &Connection, fingerprint: &str) -> Result<()> {
        let active: Option<i64> = conn
            .query_row(
                "SELECT 1 FROM issued_certs WHERE fingerprint = ?1 AND status = 'active'",
                params![fingerprint],
                |r| r.get(0),
            )
            .optional()
            .context("check the presenting certificate is still active")?;
        if active.is_none() {
            bail!(
                "{ISSUANCE_PRECONDITION_FAILED}: the certificate being renewed ({fingerprint}) is \
                 no longer active in the ledger; it was revoked while this renewal was in \
                 flight, so no replacement was issued - re-enroll for a new certificate"
            );
        }
        Ok(())
    }

    /// Commit the row in the `pending` state, reporting whether THIS call is
    /// the one that staged it.
    ///
    /// `DO NOTHING` on conflict rather than `INSERT OR REPLACE`: an existing
    /// row is an already-completed issuance for that fingerprint, and briefly
    /// demoting it to `pending` would make a live credential vanish from every
    /// reader - including [`CertLedger::is_revoked`] - for the width of the
    /// completion write. Only the caller that created the row may compensate
    /// for it, which is what the returned flag carries.
    fn stage_pending(&self, entry: &LedgerEntry) -> Result<bool> {
        let conn = self.conn.lock();
        let inserted = conn
            .execute(
                "INSERT INTO issued_certs
                    (fingerprint, device_id, token_hash, not_before, not_after, status, issued_at, actor)
                 VALUES (?1, ?2, ?3, ?4, ?5, ?6, ?7, ?8)
                 ON CONFLICT(fingerprint) DO NOTHING",
                params![
                    entry.fingerprint,
                    entry.device_id,
                    entry.token_hash,
                    entry.not_before,
                    entry.not_after,
                    PENDING,
                    entry.issued_at,
                    entry.actor,
                ],
            )
            .context("insert issued cert")?;
        Ok(inserted == 1)
    }

    /// Publish the issuance: rewrite the row with `entry.status`. This is the
    /// single statement that makes the certificate visible to every reader.
    ///
    /// An UPDATE rather than the `INSERT OR REPLACE` this replaced, for one
    /// reason: `INSERT OR REPLACE` is a delete plus an insert, so it would
    /// silently reset `delivered_at` to NULL. Re-recording a fingerprint the
    /// ledger already holds (a renewal that produced the same certificate)
    /// cannot UN-deliver the credential its keyholder is already using, and
    /// resetting the mark would hand that established certificate to the
    /// undelivered sweep an hour later. Delivery is only ever set by
    /// [`CertLedger::mark_delivered`], never by an issuance write.
    ///
    /// `stage_pending` guarantees a row exists by the time this runs, so zero
    /// affected rows means one vanished underneath us (a concurrent
    /// compensation or an external deletion). That fails closed: the caller
    /// gets `Err` and delivers nothing, rather than publishing a certificate
    /// with no row behind it.
    ///
    /// `still_active` is the renewal precondition, and this is where it becomes
    /// a guarantee rather than a hope. The check and the publish run in ONE
    /// `IMMEDIATE` transaction, so a revocation committing on another
    /// connection lands entirely before it (and the publish is refused) or
    /// entirely after it (where `revoke_device` sweeps the new row too). A
    /// check outside this transaction could be overtaken between the two
    /// statements, which is exactly the race being closed.
    fn promote_staged(&self, entry: &LedgerEntry, still_active: Option<&str>) -> Result<()> {
        let mut conn = self.conn.lock();
        let tx = conn
            .transaction_with_behavior(rusqlite::TransactionBehavior::Immediate)
            .context("begin issuance publication")?;
        if let Some(fingerprint) = still_active {
            Self::assert_still_active(&tx, fingerprint)?;
        }
        let changed = tx
            .execute(
                "UPDATE issued_certs
                    SET device_id = ?2, token_hash = ?3, not_before = ?4, not_after = ?5,
                        status = ?6, issued_at = ?7, actor = ?8
                  WHERE fingerprint = ?1",
                params![
                    entry.fingerprint,
                    entry.device_id,
                    entry.token_hash,
                    entry.not_before,
                    entry.not_after,
                    entry.status.as_str(),
                    entry.issued_at,
                    entry.actor,
                ],
            )
            .context("activate issued cert")?;
        if changed == 0 {
            bail!(
                "activate issued cert: the staged row for {} disappeared before it could be \
                 published; nothing was delivered",
                entry.fingerprint
            );
        }
        tx.commit().context("commit issuance publication")
    }

    /// Compensate a failed issuance by removing the row this call staged.
    ///
    /// Best-effort by design, and it never masks the failure that triggered it:
    /// the caller returns that error either way. The `status = pending` guard
    /// makes the delete safe against a concurrent issuance of the same
    /// fingerprint that already promoted the row, and a delete that cannot run
    /// is not a leak - the row is still pending, so still not a credential, and
    /// the next open reconciles it away.
    fn discard_staged(&self, entry: &LedgerEntry, staged: bool) {
        if !staged {
            return;
        }
        let result = self.conn.lock().execute(
            "DELETE FROM issued_certs WHERE fingerprint = ?1 AND status = ?2",
            params![entry.fingerprint, PENDING],
        );
        if let Err(error) = result {
            ::zeroclaw_log::record!(
                WARN,
                ::zeroclaw_log::Event::new(module_path!(), ::zeroclaw_log::Action::Delete)
                    .with_outcome(::zeroclaw_log::EventOutcome::Failure)
                    .with_attrs(::serde_json::json!({
                        "error": format!("{error}"),
                        "fingerprint": entry.fingerprint,
                    })),
                "cert ledger: could not discard a pending issuance; it will be reconciled at the next open"
            );
        }
    }

    /// Fault injection for the issuance ordering contract: detach the table so
    /// the next [`CertLedger::record_issued`] fails at the ledger write, AFTER
    /// its audit event. Lets the callers' interrupted-issuance behaviour be
    /// tested without a production error hook.
    #[cfg(test)]
    pub(crate) fn detach_issued_certs_for_test(&self) -> Result<()> {
        self.conn
            .lock()
            .execute_batch("ALTER TABLE issued_certs RENAME TO issued_certs_detached")
            .context("detach issued_certs")
    }

    /// Undo [`CertLedger::detach_issued_certs_for_test`].
    #[cfg(test)]
    pub(crate) fn reattach_issued_certs_for_test(&self) -> Result<()> {
        self.conn
            .lock()
            .execute_batch("ALTER TABLE issued_certs_detached RENAME TO issued_certs")
            .context("reattach issued_certs")
    }

    /// The status of a cert by fingerprint, or `None` if unknown to the ledger.
    ///
    /// A `pending` row reads as `None`: its issuance has not been recorded as
    /// complete, so the ledger does not vouch for that certificate and callers
    /// must treat it exactly as they treat one they have never seen. For the
    /// renew RPC that means "re-enroll", which is the correct answer for a
    /// certificate that was never delivered.
    pub fn status_of(&self, fingerprint: &str) -> Result<Option<CertStatus>> {
        let conn = self.conn.lock();
        let s: Option<String> = conn
            .query_row(
                "SELECT status FROM issued_certs WHERE fingerprint = ?1 AND status != ?2",
                params![fingerprint, PENDING],
                |r| r.get(0),
            )
            .optional()
            .context("query cert status")?;
        match s {
            None => Ok(None),
            Some(s) => CertStatus::from_db(&s).map(Some).with_context(|| {
                format!("issued_certs.status {s:?} is not a status this ledger vouches for")
            }),
        }
    }

    /// True iff the cert is known to this ledger AND marked revoked.
    ///
    /// A cert this ledger has never seen is NOT revoked here, and that is not a
    /// gap the verifier closes by ledger membership: the WSS verifier's
    /// authority model is CA-based. It authorizes any certificate that chains
    /// to the configured client CA, subject to the optional leaf pins and this
    /// revocation list. Ledger membership is not required for normal RPC
    /// initialization - that is what makes the documented bring-your-own-CA
    /// path work, since certificates minted outside this daemon are legitimate
    /// and never appear in its issued-cert table.
    pub fn is_revoked(&self, fingerprint: &str) -> Result<bool> {
        Ok(matches!(
            self.status_of(fingerprint)?,
            Some(CertStatus::Revoked)
        ))
    }

    /// Look up the full ledger row for a fingerprint. A `pending` row reads as
    /// `None`, for the reason given on [`CertLedger::status_of`].
    pub fn lookup_by_fingerprint(&self, fingerprint: &str) -> Result<Option<LedgerEntry>> {
        let conn = self.conn.lock();
        conn.query_row(
            "SELECT fingerprint, device_id, token_hash, not_before, not_after, status, actor, issued_at
                 FROM issued_certs WHERE fingerprint = ?1 AND status != ?2",
            params![fingerprint, PENDING],
            row_to_entry,
        )
        .optional()
        .context("lookup cert by fingerprint")
    }

    /// The device id bound to a presenting cert (its subject CN, via the ledger).
    pub fn device_of(&self, fingerprint: &str) -> Result<Option<String>> {
        Ok(self
            .lookup_by_fingerprint(fingerprint)?
            .map(|e| e.device_id))
    }

    /// Mark a cert revoked by fingerprint. Returns true if a row changed.
    /// Writes a `CertRevoked` audit event when a row was actually flipped.
    ///
    /// Ordering guarantee: the status flip and the materialized enforcement
    /// file commit together or not at all. The file is rewritten from the
    /// in-transaction view BEFORE the SQLite commit, so
    /// - a materialization failure rolls the flip back: the ledger never
    ///   reports a revocation the WSS verifier is not enforcing;
    /// - a commit failure after the file write leaves the file over-enforcing
    ///   until the next materialization (fail-closed, never fail-open).
    pub fn mark_revoked(&self, fingerprint: &str, actor: &str) -> Result<bool> {
        self.revoke_matching(fingerprint, actor, RevokePrecondition::Any)
    }

    /// [`CertLedger::mark_revoked`] with an extra condition the row must still
    /// satisfy at the moment of the flip.
    ///
    /// The flip is a compare-and-set: the condition lives in the UPDATE's WHERE
    /// clause, so it is evaluated against the row inside the same statement
    /// that changes it. Checking it beforehand and flipping afterwards is
    /// exactly the bug this exists to prevent - see
    /// [`RevokePrecondition::StillUndelivered`].
    ///
    /// A condition that no longer holds means zero rows changed, and then
    /// NOTHING happens: no audit event, no materialization, no report of a
    /// revocation. That is the honest outcome, because nothing was revoked.
    fn revoke_matching(
        &self,
        fingerprint: &str,
        actor: &str,
        precondition: RevokePrecondition,
    ) -> Result<bool> {
        let audit_entry = {
            let mut conn = self.conn.lock();
            let tx = conn
                .transaction_with_behavior(rusqlite::TransactionBehavior::Immediate)
                .context("begin revocation transaction")?;
            // Only an ACTIVE row is revocable. Revoking a `pending` row would
            // publish - as revoked - a certificate the ledger has not vouched
            // for and whose issuance may still be compensated away; such a
            // fingerprint reads as unknown everywhere else, and reporting a
            // revocation for it here would contradict that.
            let changed = tx
                .execute(
                    &format!(
                        "UPDATE issued_certs SET status = 'revoked'
                             WHERE fingerprint = ?1 AND status = 'active'{}",
                        precondition.sql_suffix()
                    ),
                    params![fingerprint],
                )
                .context("revoke cert")?;
            if changed == 0 {
                return Ok(false);
            }
            let entry = tx
                .query_row(
                    "SELECT fingerprint, device_id, token_hash, not_before, not_after, status, actor, issued_at
                         FROM issued_certs WHERE fingerprint = ?1",
                    params![fingerprint],
                    row_to_entry,
                )
                .optional()
                .context("lookup revoked cert")?;
            // Enforce BEFORE the ledger reports it (drives the A5 refusal from
            // the real revoke action); failure here rolls the flip back.
            self.materialize_on(&tx, self.revoked_path.as_deref())?;
            tx.commit().context("commit revocation")?;
            entry.map(|mut e| {
                e.actor = actor.to_string();
                e
            })
        };
        if let Some(entry) = audit_entry {
            self.audit_cert(&entry, CertAuditStage::Revoked)?;
        }
        Ok(true)
    }

    /// Revoke every active cert held by a device (e.g. a compromised device),
    /// as ONE atomic transaction. Returns the number of certs revoked.
    ///
    /// # Why this is one transaction
    ///
    /// This is the operator's "get that device off the network" command, so its
    /// promise is total: when it returns, the device holds no usable
    /// certificate. A snapshot followed by per-fingerprint revocations - which
    /// is what this replaced - cannot keep that promise, because a renewal can
    /// publish a NEW active row for the same device in the gap between the
    /// snapshot and the updates. The command then revokes only the stale set,
    /// reports success, and leaves the device holding a certificate it was
    /// handed moments earlier.
    ///
    /// Reading and flipping inside one `IMMEDIATE` transaction closes it, and
    /// [`CertLedger::record_issued_requiring`]'s promote step is likewise an
    /// `IMMEDIATE` transaction, so SQLite's write lock serializes the two.
    /// Only two orderings exist, and neither leaves the device usable:
    ///
    /// 1. **Revocation commits first.** The renewal's promote step then
    ///    re-tests its presenting fingerprint, finds it revoked, and refuses -
    ///    no new row is ever published. (The presenting certificate always
    ///    belongs to this device: renewal resolves the device id FROM it.)
    /// 2. **The renewal commits first.** This transaction's `UPDATE` then runs
    ///    against committed state that already includes the new row, and
    ///    `WHERE device_id = ? AND status = 'active'` sweeps it along with the
    ///    rest.
    ///
    /// There is no third ordering in which the two overlap, which is precisely
    /// what the write lock buys and what a stale snapshot gave away. No
    /// revocation epoch or device generation is needed for the same reason.
    pub fn revoke_device(&self, device_id: &str, actor: &str) -> Result<usize> {
        let revoked = {
            let mut conn = self.conn.lock();
            let tx = conn
                .transaction_with_behavior(rusqlite::TransactionBehavior::Immediate)
                .context("begin device revocation transaction")?;

            // Read the set INSIDE the write-locked transaction. The snapshot
            // and the flip below are therefore one indivisible step; the
            // version this replaced read the set with no transaction at all,
            // released it, and then revoked each fingerprint in a transaction
            // of its own.
            let entries: Vec<LedgerEntry> = {
                let mut stmt = tx
                    .prepare(
                        "SELECT fingerprint, device_id, token_hash, not_before, not_after, status, actor, issued_at
                             FROM issued_certs WHERE device_id = ?1 AND status = 'active'",
                    )
                    .context("prepare device revoke")?;
                let rows = stmt
                    .query_map(params![device_id], row_to_entry)
                    .context("query device certs")?;
                rows.collect::<rusqlite::Result<Vec<LedgerEntry>>>()
                    .context("collect device certs")?
            };

            #[cfg(test)]
            if let Some(hook) = self.hooks.after_device_snapshot.lock().as_ref() {
                hook();
            }

            let changed = tx
                .execute(
                    "UPDATE issued_certs SET status = 'revoked'
                         WHERE device_id = ?1 AND status = 'active'",
                    params![device_id],
                )
                .context("revoke device certs")?;
            // Holding the write lock means no row can have been added to or
            // removed from the set between the SELECT and the UPDATE. If that
            // ever stops being true, fail rather than under-report a
            // revocation the operator was told had happened.
            if changed != entries.len() {
                bail!(
                    "device revocation for {device_id} matched {changed} rows but audited \
                     {}; refusing to report a revocation whose scope is uncertain",
                    entries.len()
                );
            }
            if changed == 0 {
                return Ok(0);
            }
            // Enforce before the ledger reports it, from the in-transaction
            // view, exactly as a single-fingerprint revoke does: a failed file
            // write rolls the whole batch back rather than leaving the ledger
            // claiming revocations the verifier does not enforce.
            self.materialize_on(&tx, self.revoked_path.as_deref())?;
            tx.commit().context("commit device revocation")?;
            entries
        };

        // Audited after the commit, per fingerprint, mirroring `mark_revoked`.
        //
        // The module's audit-before-mutate rule exists for ISSUANCE, where an
        // unaudited publish would put a credential in the world with no record.
        // Revocation is the inverted risk: it fails closed, so the danger is an
        // event claiming a revocation that never committed. Committing first
        // means every event describes something that actually happened.
        //
        // A failing audit sink does not silently swallow the revocation: every
        // remaining fingerprint is still attempted, and the first error is
        // returned, so the operator learns the trail is incomplete for a
        // revocation that IS in force.
        let mut first_error = None;
        for entry in &revoked {
            let mut entry = entry.clone();
            entry.actor = actor.to_string();
            if let Err(error) = self.audit_cert(&entry, CertAuditStage::Revoked)
                && first_error.is_none()
            {
                first_error = Some(error);
            }
        }
        match first_error {
            Some(error) => Err(error).with_context(|| {
                format!(
                    "device {device_id}: {} certificate(s) were revoked and are enforced, but the \
                     audit trail could not be written",
                    revoked.len()
                )
            }),
            None => Ok(revoked.len()),
        }
    }

    /// All currently-active ledger rows.
    pub fn list_active(&self) -> Result<Vec<LedgerEntry>> {
        let conn = self.conn.lock();
        let mut stmt = conn
            .prepare(
                "SELECT fingerprint, device_id, token_hash, not_before, not_after, status, actor, issued_at
                     FROM issued_certs WHERE status = 'active'",
            )
            .context("prepare list_active")?;
        let rows = stmt
            .query_map([], row_to_entry)
            .context("query list_active")?;
        rows.collect::<rusqlite::Result<Vec<LedgerEntry>>>()
            .context("collect list_active")
    }

    /// The set of currently-revoked fingerprints - the source the WSS verifier
    /// revocation check (and CRL materialization) consume.
    pub fn revoked_fingerprints(&self) -> Result<Vec<String>> {
        let conn = self.conn.lock();
        Self::revoked_fingerprints_on(&conn)
    }

    /// The revoked set as seen by `conn` - which may be an open transaction,
    /// so [`CertLedger::mark_revoked`] can materialize a pending flip.
    fn revoked_fingerprints_on(conn: &Connection) -> Result<Vec<String>> {
        let mut stmt = conn
            .prepare("SELECT fingerprint FROM issued_certs WHERE status = 'revoked'")
            .context("prepare revoked_fingerprints")?;
        let rows = stmt
            .query_map([], |r| r.get::<_, String>(0))
            .context("query revoked_fingerprints")?;
        rows.collect::<rusqlite::Result<Vec<String>>>()
            .context("collect revoked_fingerprints")
    }

    /// Emit a hash-chained audit event for a certificate lifecycle action.
    fn audit_cert(&self, entry: &LedgerEntry, stage: CertAuditStage) -> Result<()> {
        self.audit_cert_fields(
            CertFacts {
                device_id: &entry.device_id,
                actor: &entry.actor,
                fingerprint: &entry.fingerprint,
                not_before: entry.not_before,
                not_after: entry.not_after,
            },
            stage,
        )
    }

    /// Emit a hash-chained audit event from loose facts, for the reconciliation
    /// path - which describes a row that is by definition not a [`LedgerEntry`]
    /// this ledger vouches for, so it has no [`CertStatus`] to carry.
    ///
    /// The security audit log is already separate from operational logs; cert
    /// facts (device id, fingerprint, validity, actor) are recorded in the
    /// existing actor/action fields so the entry is covered by the Merkle
    /// chain.
    ///
    /// Only a stage the durable stores have settled carries an
    /// `ExecutionResult`: a pre-commit attempt has no outcome to report, and
    /// recording one would make the append-only trail claim an issuance the
    /// ledger may still reject.
    fn audit_cert_fields(&self, facts: CertFacts<'_>, stage: CertAuditStage) -> Result<()> {
        let Some(audit) = &self.audit else {
            return Ok(());
        };
        let mut event = AuditEvent::new(stage.event_type())
            .with_actor(
                "cert".to_string(),
                Some(facts.device_id.to_string()),
                Some(facts.actor.to_string()),
            )
            .with_action(
                format!(
                    "{}fingerprint={} not_before={} not_after={}",
                    stage.action_prefix(),
                    facts.fingerprint,
                    facts.not_before,
                    facts.not_after
                ),
                "cert".to_string(),
                true,
                true,
            );
        if let Some(outcome) = stage.outcome() {
            event = event.with_result(outcome.success, None, 0, outcome.error);
        }
        audit
            .log(&event)
            .with_context(|| format!("write certificate audit event for {}", facts.fingerprint))
    }
}

/// Wall-clock unix seconds, the unit every ledger timestamp column uses.
///
/// A clock that cannot resolve reads as 0, which makes a row look ancient and
/// so undelivered-sweepable rather than permanently unsweepable - the
/// fail-closed direction for a credential this ledger cannot vouch for.
fn now_unix() -> i64 {
    std::time::SystemTime::now()
        .duration_since(std::time::UNIX_EPOCH)
        .map(|d| d.as_secs() as i64)
        .unwrap_or(0)
}

/// The certificate facts every cert audit event carries.
struct CertFacts<'a> {
    device_id: &'a str,
    actor: &'a str,
    fingerprint: &'a str,
    not_before: i64,
    not_after: i64,
}

/// The `ExecutionResult` a settled [`CertAuditStage`] claims.
struct StageOutcome {
    success: bool,
    error: Option<String>,
}

/// Which certificate lifecycle fact an audit event records.
///
/// Issuance spans two events because SQLite and the append-only audit file
/// cannot share one transaction; see [`CertLedger::record_issued`] for the
/// ordering argument and what an unmatched attempt means.
#[derive(Debug, Clone, Copy)]
enum CertAuditStage {
    /// Issuance attempted: the CSR is signed, the ledger has not committed and
    /// the caller has not delivered the certificate.
    Attempted { renewal: bool },
    /// Issuance completed: the ledger row is committed and about to be
    /// promoted out of `pending`.
    Completed { renewal: bool },
    /// An issuance that committed a row but never completed was reconciled
    /// away at open; the row is about to be discarded.
    Abandoned,
    /// Revocation committed together with its enforcement file.
    Revoked,
}

impl CertAuditStage {
    fn event_type(self) -> AuditEventType {
        match self {
            CertAuditStage::Attempted { .. } => AuditEventType::CertIssuanceAttempted,
            CertAuditStage::Completed { renewal: true } => AuditEventType::CertRenewed,
            CertAuditStage::Completed { renewal: false } => AuditEventType::CertIssued,
            CertAuditStage::Abandoned => AuditEventType::CertIssuanceAbandoned,
            CertAuditStage::Revoked => AuditEventType::CertRevoked,
        }
    }

    /// What the event claims, or `None` for a stage whose outcome is not
    /// settled yet.
    fn outcome(self) -> Option<StageOutcome> {
        match self {
            CertAuditStage::Attempted { .. } => None,
            CertAuditStage::Abandoned => Some(StageOutcome {
                success: false,
                error: Some("issuance never completed; the ledger row was discarded".to_string()),
            }),
            CertAuditStage::Completed { .. } | CertAuditStage::Revoked => Some(StageOutcome {
                success: true,
                error: None,
            }),
        }
    }

    /// Names the completion an attempt is waiting on, so an unmatched attempt
    /// reads as an interrupted renewal or an interrupted first issuance, and
    /// marks the reconciliation that closes such an attempt out.
    fn action_prefix(self) -> &'static str {
        match self {
            CertAuditStage::Attempted { renewal: true } => "attempt=renew ",
            CertAuditStage::Attempted { renewal: false } => "attempt=issue ",
            CertAuditStage::Abandoned => "abandoned=reconcile ",
            CertAuditStage::Completed { .. } | CertAuditStage::Revoked => "",
        }
    }
}

/// Map a row this ledger vouches for. A `pending` (or otherwise unrecognized)
/// status is an ERROR here rather than a silent widening to `Active`: every
/// query feeding this mapper filters pending out, and a reader that ever forgot
/// to must fail loudly instead of reporting an undelivered certificate as a
/// usable one.
fn row_to_entry(row: &rusqlite::Row<'_>) -> rusqlite::Result<LedgerEntry> {
    let status: String = row.get(5)?;
    let status = CertStatus::from_db(&status).ok_or_else(|| {
        rusqlite::Error::FromSqlConversionFailure(
            5,
            rusqlite::types::Type::Text,
            format!("issued_certs.status {status:?} is not a status this ledger vouches for")
                .into(),
        )
    })?;
    Ok(LedgerEntry {
        fingerprint: row.get(0)?,
        device_id: row.get(1)?,
        token_hash: row.get(2)?,
        not_before: row.get(3)?,
        not_after: row.get(4)?,
        status,
        actor: row.get(6)?,
        issued_at: row.get(7)?,
    })
}

#[cfg(test)]
mod tests {
    use super::*;
    use zeroclaw_config::schema::AuditConfig;

    fn entry(fp: &str, device: &str) -> LedgerEntry {
        LedgerEntry {
            device_id: device.to_string(),
            fingerprint: fp.to_string(),
            not_before: 1_000,
            not_after: 1_000 + 30 * 86_400,
            status: CertStatus::Active,
            token_hash: "abcdef0123456789".to_string(),
            actor: IssuanceActor::Enrollment {
                token_hash: "abcdef0123456789".to_string(),
            }
            .label(),
            // Issued NOW, as every production caller records it. A 1970
            // timestamp would make every fixture row instantly eligible for the
            // undelivered sweep, so tests would be asserting against a state no
            // real issuance passes through. Staleness is opt-in, via
            // `backdate_issuance`.
            issued_at: now_unix(),
        }
    }

    /// An audit logger writing to `<dir>/audit.log`, with that path.
    fn audit_logger(dir: &Path) -> (Arc<AuditLogger>, std::path::PathBuf) {
        let logger = AuditLogger::new(
            AuditConfig {
                enabled: true,
                log_path: "audit.log".to_string(),
                max_size_mb: 100,
                sign_events: false,
            },
            dir.to_path_buf(),
        )
        .unwrap();
        (Arc::new(logger), dir.join("audit.log"))
    }

    fn audit_events(log_path: &Path) -> Vec<AuditEvent> {
        std::fs::read_to_string(log_path)
            .unwrap_or_default()
            .lines()
            .filter(|l| !l.trim().is_empty())
            .map(|l| serde_json::from_str::<AuditEvent>(l).expect("audit line is a valid event"))
            .collect()
    }

    /// The serialized `event_type`: the readable surface an audit reader sees.
    fn type_name(event: &AuditEvent) -> String {
        serde_json::to_value(&event.event_type)
            .unwrap()
            .as_str()
            .expect("event_type serializes as a string")
            .to_string()
    }

    fn command_of(event: &AuditEvent) -> String {
        event
            .action
            .as_ref()
            .and_then(|a| a.command.clone())
            .unwrap_or_default()
    }

    /// Every (fingerprint, status) pair actually stored, read through a
    /// SEPARATE connection. The ledger's own readers deliberately hide
    /// `pending`, so asserting a row is *gone* rather than merely invisible has
    /// to go around them.
    fn stored_rows(dir: &Path) -> Vec<(String, String)> {
        let conn = Connection::open(dir.join("tls").join("ledger.db")).unwrap();
        let mut stmt = conn
            .prepare("SELECT fingerprint, status FROM issued_certs ORDER BY fingerprint")
            .unwrap();
        stmt.query_map([], |r| Ok((r.get(0)?, r.get(1)?)))
            .unwrap()
            .collect::<rusqlite::Result<Vec<(String, String)>>>()
            .unwrap()
    }

    /// Every (fingerprint, delivered_at) pair actually stored, read through a
    /// SEPARATE connection. `delivered_at` has no reader on [`CertLedger`] by
    /// design - nothing in production needs to ask - so a test that asserts on
    /// it has to go to the table.
    fn delivery_marks(dir: &Path) -> Vec<(String, Option<i64>)> {
        let conn = Connection::open(dir.join("tls").join("ledger.db")).unwrap();
        let mut stmt = conn
            .prepare("SELECT fingerprint, delivered_at FROM issued_certs ORDER BY fingerprint")
            .unwrap();
        stmt.query_map([], |r| Ok((r.get(0)?, r.get(1)?)))
            .unwrap()
            .collect::<rusqlite::Result<Vec<(String, Option<i64>)>>>()
            .unwrap()
    }

    /// Age a row by `secs` so the undelivered sweep sees it as stale - exactly
    /// what `secs` of wall clock would do, without the test sleeping for an
    /// hour. Backdating the data rather than shrinking the TTL keeps the
    /// production constant under test instead of substituting a test-only one.
    fn backdate_issuance(dir: &Path, fingerprint: &str, secs: i64) {
        let conn = Connection::open(dir.join("tls").join("ledger.db")).unwrap();
        let changed = conn
            .execute(
                "UPDATE issued_certs SET issued_at = ?2 WHERE fingerprint = ?1",
                params![fingerprint, now_unix() - secs],
            )
            .unwrap();
        assert_eq!(changed, 1, "backdate fixture must hit exactly one row");
    }

    /// Comfortably past [`UNDELIVERED_CERT_TTL_SECS`], for tests that want a row
    /// the sweep must act on.
    const PAST_TTL_SECS: i64 = UNDELIVERED_CERT_TTL_SECS + 600;

    /// Commit a row in the `pending` state through a separate connection:
    /// exactly what a process that died between the ledger commit and the
    /// completion audit event leaves behind. Building the fixture outside the
    /// ledger API keeps the crash-window test independent of the code under
    /// test, and proves the schema CHECK actually admits the state.
    fn stage_pending_row(dir: &Path, fingerprint: &str, device_id: &str) {
        let conn = Connection::open(dir.join("tls").join("ledger.db")).unwrap();
        conn.execute(
            "INSERT INTO issued_certs
                (fingerprint, device_id, token_hash, not_before, not_after, status, issued_at, actor)
             VALUES (?1, ?2, 'abcdef0123456789', 1000, 2592000, 'pending', 1000, 'enroll:abcdef01')",
            params![fingerprint, device_id],
        )
        .unwrap();
    }

    /// An in-flight staging must survive a concurrent open. The ledger is
    /// opened per request, so open-time reconciliation runs while another
    /// connection may be between stage and promote; only rows older than the
    /// grace window may be swept. Without the window this deleted the fresh
    /// row and the concurrent promotion failed with "staged row disappeared".
    #[test]
    fn a_concurrent_open_leaves_a_fresh_pending_row_alone() {
        let dir = tempfile::tempdir().unwrap();
        let (audit, _log) = audit_logger(dir.path());
        drop(CertLedger::open(dir.path(), Some(audit.clone())).unwrap());
        // A pending row staged moments ago, exactly as another connection's
        // in-flight record_issued would leave it mid-call.
        let conn = Connection::open(dir.path().join("tls").join("ledger.db")).unwrap();
        conn.execute(
            "INSERT INTO issued_certs
                (fingerprint, device_id, token_hash, not_before, not_after, status, issued_at, actor)
             VALUES ('fpLive', 'devL', 'abcdef0123456789', 1000, 2592000, 'pending', ?1, 'enroll:abcdef01')",
            params![now_unix()],
        )
        .unwrap();
        drop(conn);
        drop(CertLedger::open(dir.path(), Some(audit)).unwrap());
        assert!(
            stored_rows(dir.path()).contains(&("fpLive".to_string(), "pending".to_string())),
            "a fresh in-flight pending row must survive a concurrent open"
        );
    }

    /// The `issued_certs` schema EXACTLY as every pre-versioned revision of
    /// this branch created it: same columns, same indexes, the old two-value
    /// CHECK, and `user_version` left at 0.
    ///
    /// Verified against the branch history - commits a762ced3c through
    /// 0ec59c70e all created this identical shape - so one fixture covers every
    /// ledger an early adopter can be holding.
    const V0_SCHEMA: &str = "CREATE TABLE IF NOT EXISTS issued_certs (
             fingerprint TEXT PRIMARY KEY,
             device_id   TEXT NOT NULL,
             token_hash  TEXT NOT NULL DEFAULT '',
             not_before  INTEGER NOT NULL,
             not_after   INTEGER NOT NULL,
             status      TEXT NOT NULL DEFAULT 'active'
                             CHECK(status IN ('active','revoked')),
             issued_at   INTEGER NOT NULL,
             actor       TEXT NOT NULL DEFAULT ''
         );
         CREATE INDEX IF NOT EXISTS idx_issued_certs_device ON issued_certs(device_id);
         CREATE INDEX IF NOT EXISTS idx_issued_certs_status ON issued_certs(status);
         CREATE INDEX IF NOT EXISTS idx_issued_certs_token  ON issued_certs(token_hash);";

    /// The `issued_certs` schema as the revision that widened the status CHECK
    /// created it: v0's columns, the three-value CHECK, and still NO
    /// `delivered_at`.
    ///
    /// Spelled out rather than derived from [`ISSUED_CERTS_COLUMNS`], because
    /// that const now describes v2 - reusing it would quietly turn every
    /// "migrate from the old shape" test into a no-op the day the shape
    /// changed, which is the exact class of bug the migration tests exist to
    /// catch.
    const V1_SCHEMA: &str = "CREATE TABLE IF NOT EXISTS issued_certs (
             fingerprint TEXT PRIMARY KEY,
             device_id   TEXT NOT NULL,
             token_hash  TEXT NOT NULL DEFAULT '',
             not_before  INTEGER NOT NULL,
             not_after   INTEGER NOT NULL,
             status      TEXT NOT NULL DEFAULT 'active'
                             CHECK(status IN ('active','revoked','pending')),
             issued_at   INTEGER NOT NULL,
             actor       TEXT NOT NULL DEFAULT ''
         );
         CREATE INDEX IF NOT EXISTS idx_issued_certs_device ON issued_certs(device_id);
         CREATE INDEX IF NOT EXISTS idx_issued_certs_status ON issued_certs(status);
         CREATE INDEX IF NOT EXISTS idx_issued_certs_token  ON issued_certs(token_hash);";

    /// Lay down a v1-shaped ledger stamped `version`, holding certificates
    /// issued long ago - the state a host that has been running this branch is
    /// actually in when it upgrades.
    fn create_v1_ledger(dir: &Path, version: i64) {
        std::fs::create_dir_all(dir.join("tls")).unwrap();
        let conn = Connection::open(dir.join("tls").join("ledger.db")).unwrap();
        conn.execute_batch(V1_SCHEMA).unwrap();
        conn.execute_batch(
            "INSERT INTO issued_certs
                (fingerprint, device_id, token_hash, not_before, not_after, status, issued_at, actor)
             VALUES
                ('fpLiveA','devA','tokA',100,200,'active',150,'operator'),
                ('fpLiveB','devB','tokB',300,400,'active',350,'enroll:deadbeef'),
                ('fpDead','devC','tokC',500,600,'revoked',550,'operator');",
        )
        .unwrap();
        conn.execute_batch(&format!("PRAGMA user_version = {version};"))
            .unwrap();
    }

    /// Lay down a pre-versioned ledger holding one active and one revoked cert.
    fn create_v0_ledger(dir: &Path) {
        std::fs::create_dir_all(dir.join("tls")).unwrap();
        let conn = Connection::open(dir.join("tls").join("ledger.db")).unwrap();
        conn.execute_batch(V0_SCHEMA).unwrap();
        conn.execute_batch(
            "INSERT INTO issued_certs
                (fingerprint, device_id, token_hash, not_before, not_after, status, issued_at, actor)
             VALUES
                ('fpOldActive','devOld','tokOld',100,200,'active',150,'operator'),
                ('fpOldRevoked','devOld2','tokOld2',300,400,'revoked',350,'enroll:deadbeef');",
        )
        .unwrap();
    }

    fn user_version(dir: &Path) -> i64 {
        let conn = Connection::open(dir.join("tls").join("ledger.db")).unwrap();
        conn.query_row("PRAGMA user_version", [], |r| r.get(0))
            .unwrap()
    }

    fn index_names(dir: &Path) -> Vec<String> {
        let conn = Connection::open(dir.join("tls").join("ledger.db")).unwrap();
        let mut stmt = conn
            .prepare(
                "SELECT name FROM sqlite_master
                     WHERE type = 'index' AND tbl_name = 'issued_certs' AND name NOT LIKE 'sqlite_%'
                     ORDER BY name",
            )
            .unwrap();
        stmt.query_map([], |r| r.get(0))
            .unwrap()
            .collect::<rusqlite::Result<Vec<String>>>()
            .unwrap()
    }

    /// The stored CREATE TABLE text, so a test can assert WHICH constraint the
    /// table actually carries rather than inferring it from behaviour.
    fn table_sql(dir: &Path) -> String {
        let conn = Connection::open(dir.join("tls").join("ledger.db")).unwrap();
        conn.query_row(
            "SELECT sql FROM sqlite_master WHERE type = 'table' AND name = 'issued_certs'",
            [],
            |r| r.get(0),
        )
        .unwrap()
    }

    #[test]
    fn open_migrates_a_pre_versioned_ledger_and_preserves_every_row() {
        // Blocking regression: CREATE TABLE IF NOT EXISTS does NOT widen the
        // CHECK on a table that already exists, so a daemon that ran an earlier
        // revision of this branch opened its ledger fine and then failed EVERY
        // enrollment and renewal at `CHECK constraint failed: status IN
        // ('active','revoked')` the moment the issuance path inserted 'pending'.
        let dir = tempfile::tempdir().unwrap();
        create_v0_ledger(dir.path());
        assert_eq!(user_version(dir.path()), 0, "fixture must be pre-versioned");
        assert!(
            table_sql(dir.path()).contains("'active','revoked')"),
            "fixture must carry the OLD two-value CHECK"
        );

        let led = CertLedger::open(dir.path(), None).unwrap();

        // Every column of every pre-existing row survives the rebuild intact.
        let active = led
            .lookup_by_fingerprint("fpOldActive")
            .unwrap()
            .expect("the pre-existing active cert must survive migration");
        assert_eq!(active.device_id, "devOld");
        assert_eq!(active.token_hash, "tokOld");
        assert_eq!(active.not_before, 100);
        assert_eq!(active.not_after, 200);
        assert_eq!(active.issued_at, 150);
        assert_eq!(active.actor, "operator");
        assert_eq!(active.status, CertStatus::Active);
        let revoked = led
            .lookup_by_fingerprint("fpOldRevoked")
            .unwrap()
            .expect("the pre-existing revoked cert must survive migration");
        assert_eq!(revoked.device_id, "devOld2");
        assert_eq!(revoked.token_hash, "tokOld2");
        assert_eq!(revoked.not_before, 300);
        assert_eq!(revoked.not_after, 400);
        assert_eq!(revoked.issued_at, 350);
        assert_eq!(revoked.actor, "enroll:deadbeef");
        assert_eq!(revoked.status, CertStatus::Revoked);
        // Revocation state survives too - it drives the WSS refusal.
        assert!(led.is_revoked("fpOldRevoked").unwrap());
        assert_eq!(
            led.revoked_fingerprints().unwrap(),
            vec!["fpOldRevoked".to_string()]
        );

        // THE POINT: the pending -> active issuance path now runs on this DB.
        // This is the line that reproduced the reviewer's IntegrityError.
        led.record_issued(&entry("fpNew", "devNew"), false)
            .expect("issuance must succeed against a migrated ledger");
        assert_eq!(led.status_of("fpNew").unwrap(), Some(CertStatus::Active));

        // ... and the new credential coexists with the preserved ones.
        let mut active_fps: Vec<String> = led
            .list_active()
            .unwrap()
            .into_iter()
            .map(|e| e.fingerprint)
            .collect();
        active_fps.sort();
        assert_eq!(active_fps, ["fpNew", "fpOldActive"]);
        assert!(led.is_revoked("fpOldRevoked").unwrap());

        // The schema is stamped, widened, and still carries its indexes.
        assert_eq!(user_version(dir.path()), SCHEMA_VERSION);
        assert!(
            table_sql(dir.path()).contains("'active','revoked','pending')"),
            "migrated table must carry the widened CHECK, got: {}",
            table_sql(dir.path())
        );
        assert_eq!(
            index_names(dir.path()),
            [
                "idx_issued_certs_device",
                "idx_issued_certs_status",
                "idx_issued_certs_token"
            ],
            "the rebuild must recreate every index the old table had"
        );
    }

    /// Occupy the scratch table name the rebuild stages into, so the migration
    /// fails at its very first statement.
    fn obstruct_migration(dir: &Path) {
        let conn = Connection::open(dir.join("tls").join("ledger.db")).unwrap();
        conn.execute_batch("CREATE TABLE issued_certs_migrated (occupied INTEGER);")
            .unwrap();
    }

    #[test]
    fn migration_handles_an_unversioned_ledger_that_already_has_the_wide_check() {
        // The other early-adopter shape: a ledger written by the revision that
        // widened the CHECK but predated versioning. It is v1-shaped yet still
        // stamped 0, so the rebuild runs over it. That must be lossless - and
        // it must carry `pending` rows through the copy, where reconciliation
        // then resolves them exactly as it would on any other open.
        let dir = tempfile::tempdir().unwrap();
        std::fs::create_dir_all(dir.path().join("tls")).unwrap();
        {
            let conn = Connection::open(dir.path().join("tls").join("ledger.db")).unwrap();
            conn.execute_batch(V1_SCHEMA).unwrap();
            conn.execute_batch(
                "INSERT INTO issued_certs
                    (fingerprint, device_id, token_hash, not_before, not_after, status, issued_at, actor)
                 VALUES
                    ('fpKeep','devK','tokK',100,200,'active',150,'operator'),
                    ('fpGone','devG','tokG',300,400,'pending',350,'enroll:cafe');",
            )
            .unwrap();
        }
        assert_eq!(user_version(dir.path()), 0);

        let led = CertLedger::open(dir.path(), None).unwrap();

        assert_eq!(user_version(dir.path()), SCHEMA_VERSION);
        let kept = led.lookup_by_fingerprint("fpKeep").unwrap().unwrap();
        assert_eq!(kept.actor, "operator");
        assert_eq!(kept.not_after, 200);
        // The pending row survived the rebuild and was then reconciled away.
        assert_eq!(
            stored_rows(dir.path()),
            vec![("fpKeep".to_string(), "active".to_string())],
            "a pending row must migrate, then be reconciled - never block the rebuild"
        );
        led.record_issued(&entry("fpNew", "devNew"), false).unwrap();
        assert_eq!(led.list_active().unwrap().len(), 2);
    }

    #[test]
    fn a_failed_migration_rolls_back_and_leaves_the_old_ledger_intact() {
        // Failure-safety: a migration that dies part-way must leave the ledger
        // exactly as it found it. A half-migrated certificate ledger is worse
        // than an un-migrated one - it can lose or duplicate credentials.
        let dir = tempfile::tempdir().unwrap();
        create_v0_ledger(dir.path());
        obstruct_migration(dir.path());

        let err = CertLedger::open(dir.path(), None)
            .map(|_| ())
            .expect_err("an obstructed migration must fail the open");
        let chain = format!("{err:#}");
        assert!(
            chain.contains("rebuild the cert-ledger issued_certs table"),
            "got: {chain}"
        );
        assert!(
            chain.contains("left unchanged"),
            "the error must tell the operator nothing was lost: {chain}"
        );
        assert!(
            chain.contains("ledger.db"),
            "the error must name the ledger file: {chain}"
        );

        // Everything about the original table survives the rollback.
        assert_eq!(
            user_version(dir.path()),
            0,
            "a failed migration must not stamp the version"
        );
        assert!(
            table_sql(dir.path()).contains("'active','revoked')"),
            "the original CHECK must be intact"
        );
        assert_eq!(
            index_names(dir.path()),
            [
                "idx_issued_certs_device",
                "idx_issued_certs_status",
                "idx_issued_certs_token"
            ]
        );
        assert_eq!(
            stored_rows(dir.path()),
            vec![
                ("fpOldActive".to_string(), "active".to_string()),
                ("fpOldRevoked".to_string(), "revoked".to_string()),
            ],
            "every pre-existing row must survive a failed migration"
        );

        // Clearing the obstruction lets the very same ledger migrate cleanly.
        {
            let conn = Connection::open(dir.path().join("tls").join("ledger.db")).unwrap();
            conn.execute_batch("DROP TABLE issued_certs_migrated;")
                .unwrap();
        }
        let led = CertLedger::open(dir.path(), None).unwrap();
        assert_eq!(user_version(dir.path()), SCHEMA_VERSION);
        led.record_issued(&entry("fpNew", "devNew"), false).unwrap();
        assert_eq!(led.list_active().unwrap().len(), 2);
        assert!(led.is_revoked("fpOldRevoked").unwrap());
    }

    #[test]
    fn reopening_an_already_migrated_ledger_does_not_rebuild() {
        let dir = tempfile::tempdir().unwrap();
        create_v0_ledger(dir.path());
        {
            let led = CertLedger::open(dir.path(), None).unwrap();
            led.record_issued(&entry("fpNew", "devNew"), false).unwrap();
            // A real issuance is delivered to its client; without the mark the
            // reopen below would (correctly) sweep it as undelivered, which is
            // a different test.
            assert!(led.mark_delivered("fpNew").unwrap());
        }
        assert_eq!(user_version(dir.path()), SCHEMA_VERSION);

        // Occupy the scratch table name. If the version check did NOT
        // short-circuit, the rebuild would run and fail on it - so a CLEAN open
        // here is positive proof the migration path was skipped entirely.
        obstruct_migration(dir.path());

        let led = CertLedger::open(dir.path(), None).unwrap();
        assert_eq!(user_version(dir.path()), SCHEMA_VERSION);
        let mut fps: Vec<String> = led
            .list_active()
            .unwrap()
            .into_iter()
            .map(|e| e.fingerprint)
            .collect();
        fps.sort();
        assert_eq!(fps, ["fpNew", "fpOldActive"]);
        assert!(led.is_revoked("fpOldRevoked").unwrap());
    }

    #[test]
    fn a_ledger_from_a_newer_build_is_refused_rather_than_guessed_at() {
        // Fail closed on a forward version: a newer build may use states this
        // one would mis-read, and a certificate ledger is the wrong place to
        // guess.
        let dir = tempfile::tempdir().unwrap();
        create_v0_ledger(dir.path());
        {
            let conn = Connection::open(dir.path().join("tls").join("ledger.db")).unwrap();
            conn.execute_batch(&format!("PRAGMA user_version = {};", SCHEMA_VERSION + 1))
                .unwrap();
        }

        let err = format!(
            "{:#}",
            CertLedger::open(dir.path(), None)
                .map(|_| ())
                .expect_err("a forward schema version must be refused")
        );
        assert!(err.contains("newer than"), "got: {err}");
        assert!(
            err.contains("re-enrollment"),
            "the refusal must tell the operator what their options are: {err}"
        );
        // Refusing touched nothing.
        assert_eq!(stored_rows(dir.path()).len(), 2);
    }

    #[test]
    fn a_fresh_ledger_is_created_already_stamped() {
        let dir = tempfile::tempdir().unwrap();
        let led = CertLedger::open(dir.path(), None).unwrap();
        assert_eq!(
            user_version(dir.path()),
            SCHEMA_VERSION,
            "a fresh ledger must not look pre-versioned on the next open"
        );
        assert!(table_sql(dir.path()).contains("'active','revoked','pending')"));
        assert_eq!(
            index_names(dir.path()),
            [
                "idx_issued_certs_device",
                "idx_issued_certs_status",
                "idx_issued_certs_token"
            ]
        );
        // And a fresh ledger reopens without rebuilding.
        drop(led);
        obstruct_migration(dir.path());
        let led = CertLedger::open(dir.path(), None).unwrap();
        led.record_issued(&entry("fp1", "dev1"), false).unwrap();
        assert_eq!(led.list_active().unwrap().len(), 1);
    }

    #[test]
    fn record_lookup_and_status() {
        let led = CertLedger::open_in_memory(None).unwrap();
        led.record_issued(&entry("fp1", "dev1"), false).unwrap();
        assert_eq!(led.status_of("fp1").unwrap(), Some(CertStatus::Active));
        assert_eq!(led.status_of("missing").unwrap(), None);
        let got = led.lookup_by_fingerprint("fp1").unwrap().unwrap();
        assert_eq!(got.device_id, "dev1");
        assert_eq!(got.actor, "enroll:abcdef01");
        assert_eq!(led.device_of("fp1").unwrap().as_deref(), Some("dev1"));
    }

    #[test]
    fn revoke_flips_status_and_lists() {
        let led = CertLedger::open_in_memory(None).unwrap();
        led.record_issued(&entry("fp1", "dev1"), false).unwrap();
        assert!(!led.is_revoked("fp1").unwrap());
        assert!(led.mark_revoked("fp1", "operator").unwrap());
        assert!(led.is_revoked("fp1").unwrap());
        assert_eq!(led.status_of("fp1").unwrap(), Some(CertStatus::Revoked));
        assert_eq!(led.revoked_fingerprints().unwrap(), vec!["fp1".to_string()]);
        assert!(led.list_active().unwrap().is_empty());
        // Idempotent: revoking again reports no change.
        assert!(!led.mark_revoked("fp1", "operator").unwrap());
        // Revoking an unknown fingerprint is a no-op.
        assert!(!led.mark_revoked("nope", "operator").unwrap());
    }

    #[test]
    fn mark_revoked_rolls_back_when_materialization_fails() {
        // Fault injection for the revocation atomicity contract: if the
        // enforcement file cannot be written, the status flip must NOT commit.
        // A revoked-in-SQLite row with a stale enforcement file would keep
        // authenticating at the WSS handshake while `security list-client-certs`
        // reports it revoked - exactly the split this ordering forbids.
        let dir = tempfile::tempdir().unwrap();
        let led = CertLedger::open(dir.path(), None).unwrap();
        led.record_issued(&entry("fpA", "dev1"), false).unwrap();

        // Obstruct materialization: replace the enforcement file with a
        // directory so the atomic rename over it fails.
        let crl = revoked_list_path(dir.path());
        std::fs::remove_file(&crl).unwrap();
        std::fs::create_dir(&crl).unwrap();

        let err = led.mark_revoked("fpA", "operator").unwrap_err().to_string();
        assert!(err.contains("revocation list"), "got: {err}");
        // Rolled back: the ledger does not report a revocation it could not
        // enforce, and the revoked set stays empty.
        assert_eq!(led.status_of("fpA").unwrap(), Some(CertStatus::Active));
        assert!(!led.is_revoked("fpA").unwrap());
        assert!(led.revoked_fingerprints().unwrap().is_empty());

        // Clear the fault: the same revoke now commits AND enforces.
        std::fs::remove_dir(&crl).unwrap();
        assert!(led.mark_revoked("fpA", "operator").unwrap());
        assert!(led.is_revoked("fpA").unwrap());
        let body = std::fs::read_to_string(&crl).unwrap();
        assert!(body.contains("fpA"), "enforcement file must carry the fp");
    }

    #[test]
    fn revoke_materializes_the_crl_file_for_the_wss_verifier() {
        // P1 contract: revoking in the ledger writes <data_dir>/tls/revoked so the
        // WSS verifier refuses that cert on the next connect (A5).
        let dir = tempfile::tempdir().unwrap();
        let led = CertLedger::open(dir.path(), None).unwrap();
        led.record_issued(&entry("fpA", "dev1"), false).unwrap();
        led.record_issued(&entry("fpB", "dev2"), false).unwrap();

        let crl = revoked_list_path(dir.path());
        // Nothing revoked yet -> the file exists (materialized at open) but is empty.
        let before = std::fs::read_to_string(&crl).unwrap_or_default();
        assert!(before.trim().is_empty());

        led.mark_revoked("fpA", "operator").unwrap();
        let after = std::fs::read_to_string(&crl).unwrap();
        let revoked: Vec<&str> = after
            .lines()
            .map(str::trim)
            .filter(|l| !l.is_empty())
            .collect();
        assert_eq!(
            revoked,
            vec!["fpA"],
            "the revoked fingerprint is materialized"
        );
        // The verifier sees it as revoked, the other does not.
        let set = zeroclaw_tls::load_revoked_fingerprints(&crl).unwrap();
        assert!(set.contains("fpa")); // load normalizes to lowercase
        assert!(!set.contains("fpb"));
    }

    #[test]
    fn revoke_materializes_to_a_configured_crl_path_not_the_default() {
        // Regression: with `[wss.client_auth].crl_path` set, the WSS verifier
        // reads THAT file. Materializing to the default instead let
        // `revoke-client-cert` report success while the next handshake still
        // accepted the certificate. Revocation must fail closed.
        let dir = tempfile::tempdir().unwrap();
        let custom = dir.path().join("operator-managed.crl");

        let effective = effective_revoked_list_path(dir.path(), Some(custom.to_str().unwrap()));
        assert_eq!(effective, custom, "a configured path wins over the default");

        let led = CertLedger::open_at(dir.path(), None, effective.clone()).unwrap();
        led.record_issued(&entry("fpA", "dev1"), false).unwrap();
        assert!(led.mark_revoked("fpA", "operator").unwrap());

        // The configured file - the one the verifier reads - carries the revocation.
        let set = zeroclaw_tls::load_revoked_fingerprints(&custom).unwrap();
        assert!(
            set.contains("fpa"),
            "revocation must land in the configured CRL path"
        );

        // And it did not silently go only to the default path.
        let default_body =
            std::fs::read_to_string(revoked_list_path(dir.path())).unwrap_or_default();
        assert!(
            !default_body.contains("fpA"),
            "revocation must not be written only to the unused default path"
        );
    }

    #[test]
    fn effective_revoked_list_path_falls_back_when_unset_or_blank() {
        let dir = tempfile::tempdir().unwrap();
        let default_path = revoked_list_path(dir.path());
        assert_eq!(effective_revoked_list_path(dir.path(), None), default_path);
        assert_eq!(
            effective_revoked_list_path(dir.path(), Some("")),
            default_path
        );
        assert_eq!(
            effective_revoked_list_path(dir.path(), Some("   ")),
            default_path,
            "a whitespace-only crl_path is not a real override"
        );
    }

    /// A padded spelling resolves to the same trimmed path everywhere. The WSS
    /// startup resolver, operator CLI, and ledger all call THIS function, so
    /// the verifier and the revocation writer cannot be split onto different
    /// files by a configuration spelling.
    #[test]
    fn effective_revoked_list_path_trims_padded_overrides_to_one_path() {
        let dir = tempfile::tempdir().unwrap();
        let padded = effective_revoked_list_path(dir.path(), Some("  /tmp/crl.pem  "));
        let exact = effective_revoked_list_path(dir.path(), Some("/tmp/crl.pem"));
        assert_eq!(
            padded, exact,
            "padded and exact spellings must resolve to one CRL path"
        );
    }

    #[test]
    fn open_refreshes_stale_materialized_revocations_from_ledger() {
        let dir = tempfile::tempdir().unwrap();
        let crl = revoked_list_path(dir.path());
        {
            let led = CertLedger::open(dir.path(), None).unwrap();
            led.record_issued(&entry("fpA", "dev1"), false).unwrap();
            led.mark_revoked("fpA", "operator").unwrap();
        }

        std::fs::write(&crl, "# stale\n").unwrap();
        let reopened = CertLedger::open(dir.path(), None).unwrap();
        assert!(reopened.is_revoked("fpA").unwrap());
        let refreshed = std::fs::read_to_string(&crl).unwrap();
        assert_eq!(refreshed.trim(), "fpA");

        std::fs::remove_file(&crl).unwrap();
        let reopened = CertLedger::open(dir.path(), None).unwrap();
        assert!(reopened.is_revoked("fpA").unwrap());
        let refreshed = std::fs::read_to_string(&crl).unwrap();
        assert_eq!(refreshed.trim(), "fpA");
    }

    #[test]
    fn record_issued_propagates_certificate_audit_write_failure() {
        let dir = tempfile::tempdir().unwrap();
        let audit = AuditLogger::new(
            AuditConfig {
                enabled: true,
                log_path: "missing/audit.log".to_string(),
                max_size_mb: 100,
                sign_events: false,
            },
            dir.path().to_path_buf(),
        )
        .unwrap();
        let led = CertLedger::open_in_memory(Some(Arc::new(audit))).unwrap();

        let err = led
            .record_issued(&entry("fp1", "dev1"), false)
            .unwrap_err()
            .to_string();
        assert!(err.contains("certificate audit event"), "got: {err}");
    }

    #[test]
    fn record_issued_audits_an_attempt_then_a_completion() {
        let dir = tempfile::tempdir().unwrap();
        let (audit, log) = audit_logger(dir.path());
        let led = CertLedger::open_in_memory(Some(audit)).unwrap();

        led.record_issued(&entry("fp1", "dev1"), false).unwrap();
        led.record_issued(&entry("fp2", "dev1"), true).unwrap();

        let events = audit_events(&log);
        let names: Vec<String> = events.iter().map(type_name).collect();
        assert_eq!(
            names,
            [
                "cert_issuance_attempted",
                "cert_issued",
                "cert_issuance_attempted",
                "cert_renewed",
            ],
            "each issuance is an attempt followed by its completion"
        );
        // The attempt claims no outcome and names the completion it is waiting
        // on; only the post-commit event records success.
        assert!(events[0].result.is_none(), "an attempt records no outcome");
        assert!(events[2].result.is_none(), "an attempt records no outcome");
        assert!(command_of(&events[0]).starts_with("attempt=issue fingerprint=fp1 "));
        assert!(command_of(&events[2]).starts_with("attempt=renew fingerprint=fp2 "));
        assert!(events[1].result.as_ref().unwrap().success);
        assert!(events[3].result.as_ref().unwrap().success);
        assert!(command_of(&events[1]).starts_with("fingerprint=fp1 "));
        assert!(command_of(&events[3]).starts_with("fingerprint=fp2 "));
        // The chain sequence is the append order: no completion precedes its attempt.
        assert!(events[0].sequence < events[1].sequence);
        assert!(events[2].sequence < events[3].sequence);
    }

    #[test]
    fn record_issued_ledger_failure_leaves_an_attempt_with_no_completion() {
        // Forced SQLite failure AFTER the audit write. Renaming the INSERT
        // target away is the least invasive stand-in for any commit-time
        // failure (full disk, corruption, lock timeout) and needs no production
        // injection hook. The pre-commit event must not have claimed issuance.
        let dir = tempfile::tempdir().unwrap();
        let (audit, log) = audit_logger(dir.path());
        let led = CertLedger::open_in_memory(Some(audit)).unwrap();
        led.detach_issued_certs_for_test().unwrap();

        let err = led
            .record_issued(&entry("fp1", "dev1"), false)
            .unwrap_err()
            .to_string();
        assert!(err.contains("insert issued cert"), "got: {err}");

        let events = audit_events(&log);
        let names: Vec<String> = events.iter().map(type_name).collect();
        assert_eq!(
            names,
            ["cert_issuance_attempted"],
            "a failed ledger write must leave an attempt, never a completion"
        );
        assert!(events[0].result.is_none(), "an attempt records no outcome");

        // Nothing usable was delivered: the caller got the retryable error, so
        // it never hands the certificate over, and no row backs that fingerprint.
        led.reattach_issued_certs_for_test().unwrap();
        assert_eq!(led.status_of("fp1").unwrap(), None);
        assert!(led.list_active().unwrap().is_empty());

        // The retry completes, and the completion event is what marks it so.
        led.record_issued(&entry("fp1", "dev1"), false).unwrap();
        let names: Vec<String> = audit_events(&log).iter().map(type_name).collect();
        assert_eq!(
            names,
            [
                "cert_issuance_attempted",
                "cert_issuance_attempted",
                "cert_issued",
            ]
        );
        assert_eq!(led.status_of("fp1").unwrap(), Some(CertStatus::Active));
    }

    #[test]
    fn completion_audit_failure_publishes_no_certificate_and_no_duplicate_on_retry() {
        // The blocking case. The ATTEMPT event lands, the row commits, and the
        // COMPLETION event fails. Before the pending -> active protocol this
        // returned Err with an ACTIVE row already committed for a certificate
        // the caller never delivered; because the client's retry carries a
        // fresh CSR - and so a different fingerprint - the retry then added a
        // SECOND active credential instead of replacing the first.
        let dir = tempfile::tempdir().unwrap();
        let (audit, log) = audit_logger(dir.path());
        let led = CertLedger::open(dir.path(), Some(audit.clone())).unwrap();

        // Let the attempt write land; fail every write after it. This is the
        // one fault an external manipulation of the log file cannot produce:
        // both writes happen inside a single record_issued call.
        audit.fail_writes_after_for_test(1);
        let err = led
            .record_issued(&entry("fp1", "dev1"), false)
            .unwrap_err()
            .to_string();
        assert!(err.contains("certificate audit event"), "got: {err}");

        // The trail holds the attempt and no completion.
        let names: Vec<String> = audit_events(&log).iter().map(type_name).collect();
        assert_eq!(
            names,
            ["cert_issuance_attempted"],
            "a failed completion write must not leave a completion event"
        );

        // Nothing was published: no status, no row to look up, no active list
        // entry, and no revocation state for a fingerprint nobody holds.
        assert_eq!(
            led.status_of("fp1").unwrap(),
            None,
            "an undelivered certificate must not be Active - or known at all"
        );
        assert!(led.lookup_by_fingerprint("fp1").unwrap().is_none());
        assert!(!led.is_revoked("fp1").unwrap());
        assert!(led.list_active().unwrap().is_empty());
        // The compensating delete removed the row; it did not merely hide it.
        assert!(
            stored_rows(dir.path()).is_empty(),
            "the staged row must be compensated away, got: {:?}",
            stored_rows(dir.path())
        );

        // The retry - a fresh CSR, so a fresh fingerprint, as the real client
        // does - completes, and leaves exactly ONE active credential.
        audit.clear_write_failure_for_test();
        led.record_issued(&entry("fp2", "dev1"), false).unwrap();
        let active = led.list_active().unwrap();
        assert_eq!(active.len(), 1, "the retry must not multiply active rows");
        assert_eq!(active[0].fingerprint, "fp2");
        assert_eq!(led.status_of("fp1").unwrap(), None);
        let names: Vec<String> = audit_events(&log).iter().map(type_name).collect();
        assert_eq!(
            names,
            [
                "cert_issuance_attempted",
                "cert_issuance_attempted",
                "cert_issued",
            ],
            "the interrupted attempt stays unmatched; only the retry completes"
        );
    }

    #[test]
    fn completion_audit_failure_leaves_an_established_certificate_untouched() {
        // Re-recording a fingerprint the ledger already holds must not use the
        // established row as the compensation target: the certificate behind it
        // WAS delivered. A failed completion leaves it active with its original
        // validity, which is right - the caller is returning an error rather
        // than handing over the renewed certificate.
        let dir = tempfile::tempdir().unwrap();
        let (audit, log) = audit_logger(dir.path());
        let led = CertLedger::open(dir.path(), Some(audit.clone())).unwrap();
        led.record_issued(&entry("fp1", "dev1"), false).unwrap();

        let mut renewed = entry("fp1", "dev1");
        renewed.not_after = 9_999_999;
        audit.fail_writes_after_for_test(1);
        assert!(led.record_issued(&renewed, true).is_err());

        let held = led
            .lookup_by_fingerprint("fp1")
            .unwrap()
            .expect("the established certificate must survive");
        assert_eq!(held.status, CertStatus::Active);
        assert_eq!(
            held.not_after,
            1_000 + 30 * 86_400,
            "an undelivered renewal must not extend the established validity"
        );
        assert_eq!(led.list_active().unwrap().len(), 1);
        let names: Vec<String> = audit_events(&log).iter().map(type_name).collect();
        assert_eq!(
            names,
            [
                "cert_issuance_attempted",
                "cert_issued",
                "cert_issuance_attempted",
            ]
        );
    }

    #[test]
    fn open_reconciles_a_pending_row_left_by_a_crash() {
        // The crash window the compensating delete cannot cover: the process
        // dies between the ledger commit and the completion event. The flip out
        // of `pending` happens inside the same record_issued call, so a pending
        // row older than the reconciliation grace window is crash residue - an
        // undelivered certificate that must be resolved durably, not left to
        // linger. (The fixture's 1970 issued_at is far past the window.)
        let dir = tempfile::tempdir().unwrap();
        let (audit, log) = audit_logger(dir.path());
        {
            let led = CertLedger::open(dir.path(), Some(audit.clone())).unwrap();
            led.record_issued(&entry("fpDone", "dev1"), false).unwrap();
            // fpDone stands for a completed, delivered issuance; the undelivered
            // sweep is exercised on its own below.
            assert!(led.mark_delivered("fpDone").unwrap());
        }
        stage_pending_row(dir.path(), "fpCrash", "dev2");
        assert!(
            stored_rows(dir.path()).contains(&("fpCrash".to_string(), "pending".to_string())),
            "fixture must actually commit a pending row"
        );

        let reopened = CertLedger::open(dir.path(), Some(audit)).unwrap();

        // Resolved durably, and it never surfaces as a credential on the way.
        assert_eq!(reopened.status_of("fpCrash").unwrap(), None);
        assert!(reopened.lookup_by_fingerprint("fpCrash").unwrap().is_none());
        assert!(!reopened.is_revoked("fpCrash").unwrap());
        assert_eq!(
            stored_rows(dir.path()),
            vec![("fpDone".to_string(), "active".to_string())],
            "the stranded row is gone; the completed issuance is untouched"
        );
        let active: Vec<String> = reopened
            .list_active()
            .unwrap()
            .into_iter()
            .map(|e| e.fingerprint)
            .collect();
        assert_eq!(active, ["fpDone"]);

        // And the trail explains the stranded attempt rather than leaving the
        // reader to infer it from a missing completion.
        let events = audit_events(&log);
        let names: Vec<String> = events.iter().map(type_name).collect();
        assert_eq!(
            names,
            [
                "cert_issuance_attempted",
                "cert_issued",
                "cert_issuance_abandoned",
            ]
        );
        let abandoned = events.last().unwrap();
        assert!(
            command_of(abandoned).starts_with("abandoned=reconcile fingerprint=fpCrash "),
            "got: {}",
            command_of(abandoned)
        );
        let result = abandoned
            .result
            .as_ref()
            .expect("a reconciliation has a settled outcome");
        assert!(
            !result.success,
            "the reconciliation must record a FAILED issuance, not a completed one"
        );

        // Reconciliation is not a one-shot: a second open finds nothing left.
        let again = CertLedger::open(dir.path(), None).unwrap();
        assert_eq!(again.list_active().unwrap().len(), 1);
        assert_eq!(audit_events(&log).len(), 3);
    }

    #[test]
    fn a_pending_row_is_never_a_credential_for_any_reader() {
        // The consumer audit, executable: every reader that means "usable
        // credential" must refuse a pending row. Inserted after open, so it is
        // a live pending row rather than one reconciliation already removed.
        let dir = tempfile::tempdir().unwrap();
        let led = CertLedger::open(dir.path(), None).unwrap();
        stage_pending_row(dir.path(), "fpPending", "devP");

        assert_eq!(led.status_of("fpPending").unwrap(), None);
        assert!(led.lookup_by_fingerprint("fpPending").unwrap().is_none());
        assert!(led.device_of("fpPending").unwrap().is_none());
        assert!(!led.is_revoked("fpPending").unwrap());
        assert!(led.list_active().unwrap().is_empty());
        assert!(led.revoked_fingerprints().unwrap().is_empty());
        assert!(
            !led.mark_revoked("fpPending", "operator").unwrap(),
            "a pending row is not revocable: it is not a credential to revoke"
        );
        assert_eq!(led.revoke_device("devP", "operator").unwrap(), 0);
        // Refusing to revoke it did not quietly flip it either.
        assert_eq!(
            stored_rows(dir.path()),
            vec![("fpPending".to_string(), "pending".to_string())]
        );
        // And the enforcement file the WSS verifier reads stays empty.
        let crl = std::fs::read_to_string(revoked_list_path(dir.path())).unwrap_or_default();
        assert!(crl.trim().is_empty());
    }

    #[test]
    fn revoke_device_revokes_all_its_active_certs() {
        let led = CertLedger::open_in_memory(None).unwrap();
        led.record_issued(&entry("fp1", "dev1"), false).unwrap();
        led.record_issued(&entry("fp2", "dev1"), true).unwrap();
        led.record_issued(&entry("fp3", "dev2"), false).unwrap();
        assert_eq!(led.revoke_device("dev1", "operator").unwrap(), 2);
        assert!(led.is_revoked("fp1").unwrap());
        assert!(led.is_revoked("fp2").unwrap());
        assert!(!led.is_revoked("fp3").unwrap());
    }

    #[test]
    fn renewal_replaces_row_on_same_fingerprint() {
        let led = CertLedger::open_in_memory(None).unwrap();
        let mut e = entry("fp1", "dev1");
        led.record_issued(&e, false).unwrap();
        // A renewal that produces the same fingerprint just updates validity.
        e.not_after = 9_999_999;
        led.record_issued(&e, true).unwrap();
        let got = led.lookup_by_fingerprint("fp1").unwrap().unwrap();
        assert_eq!(got.not_after, 9_999_999);
        // Only one row exists for that fingerprint.
        assert_eq!(led.list_active().unwrap().len(), 1);
    }

    #[test]
    fn concurrent_ledger_opens_do_not_clobber_each_others_materialization() {
        // Regression: every open materializes the revocation list, and the
        // scratch file used to be a fixed `revoked.tmp`. Several ledgers opened
        // at once against one data_dir therefore wrote the same temp path and
        // raced the rename - the first won, the rest failed ENOENT and took the
        // whole open down with them. Eight clients renewing simultaneously is
        // ordinary traffic, and each renewal opens its own ledger handle, so
        // this failed a routine burst with an error about the revocation list.
        let dir = tempfile::tempdir().unwrap();
        {
            let led = CertLedger::open(dir.path(), None).unwrap();
            led.record_issued(&entry("fpA", "dev1"), false).unwrap();
            assert!(led.mark_delivered("fpA").unwrap());
            led.record_issued(&entry("fpB", "dev2"), false).unwrap();
            assert!(led.mark_delivered("fpB").unwrap());
            assert!(led.mark_revoked("fpB", "operator").unwrap());
        }

        let failures: Vec<String> = std::thread::scope(|scope| {
            let handles: Vec<_> = (0..8)
                .map(|_| scope.spawn(|| CertLedger::open(dir.path(), None).map(|_| ())))
                .collect();
            handles
                .into_iter()
                .filter_map(|h| h.join().unwrap().err())
                .map(|e| format!("{e:#}"))
                .collect()
        });
        assert!(
            failures.is_empty(),
            "concurrent opens must all succeed, got: {failures:?}"
        );

        // The list is intact - not truncated or half-written by the race - and
        // no scratch files were left lying around the tls dir.
        let crl = revoked_list_path(dir.path());
        assert_eq!(std::fs::read_to_string(&crl).unwrap().trim(), "fpB");
        let strays: Vec<String> = std::fs::read_dir(dir.path().join("tls"))
            .unwrap()
            .map(|e| e.unwrap().file_name().to_string_lossy().into_owned())
            .filter(|n| n.contains("revoked.tmp"))
            .collect();
        assert!(strays.is_empty(), "left scratch files behind: {strays:?}");
    }

    #[test]
    fn an_undelivered_active_row_is_revoked_once_the_ttl_has_passed() {
        // The window the pending -> active protocol alone could not close: the
        // row is promoted BEFORE the caller can hand the certificate over, so a
        // crash or a dead connection in between leaves an ACTIVE row for a
        // certificate nobody received. The sweep bounds that to the TTL.
        let dir = tempfile::tempdir().unwrap();
        let (audit, log) = audit_logger(dir.path());
        {
            let led = CertLedger::open(dir.path(), Some(audit.clone())).unwrap();
            led.record_issued(&entry("fpGhost", "devGhost"), false)
                .unwrap();
            // No mark_delivered: the delivery boundary never succeeded.
            assert_eq!(led.status_of("fpGhost").unwrap(), Some(CertStatus::Active));
        }
        backdate_issuance(dir.path(), "fpGhost", PAST_TTL_SECS);

        let reopened = CertLedger::open(dir.path(), Some(audit)).unwrap();

        // Revoked - not deleted. The bytes may exist in the wild, so the ledger
        // keeps vouching for the fingerprint as a REVOKED credential.
        assert_eq!(
            reopened.status_of("fpGhost").unwrap(),
            Some(CertStatus::Revoked),
            "an undelivered certificate past the TTL must be revoked"
        );
        assert!(reopened.is_revoked("fpGhost").unwrap());
        assert!(reopened.list_active().unwrap().is_empty());
        assert_eq!(
            stored_rows(dir.path()),
            vec![("fpGhost".to_string(), "revoked".to_string())],
            "the row must survive as a revocation, never be erased"
        );

        // It reached the file the WSS verifier actually reads.
        let crl = std::fs::read_to_string(revoked_list_path(dir.path())).unwrap();
        assert_eq!(crl.trim(), "fpGhost");
        let set = zeroclaw_tls::load_revoked_fingerprints(&revoked_list_path(dir.path())).unwrap();
        assert!(
            set.contains("fpghost"),
            "the verifier must see the revocation"
        );

        // And the trail says WHO revoked it, so an operator reading the log can
        // tell a reconciliation from a deliberate revoke.
        let events = audit_events(&log);
        let names: Vec<String> = events.iter().map(type_name).collect();
        assert_eq!(
            names,
            ["cert_issuance_attempted", "cert_issued", "cert_revoked"],
        );
        let revoked_event = events.last().unwrap();
        assert_eq!(
            revoked_event
                .actor
                .as_ref()
                .and_then(|a| a.username.clone())
                .unwrap_or_default(),
            UNDELIVERED_RECONCILE_ACTOR,
            "the revocation must name the undelivered sweep as its actor"
        );
        assert_eq!(
            revoked_event
                .actor
                .as_ref()
                .and_then(|a| a.user_id.clone())
                .unwrap_or_default(),
            "devGhost",
            "and the device whose credential it was"
        );

        // Idempotent: a second open finds nothing left to sweep.
        drop(reopened);
        let again = CertLedger::open(dir.path(), None).unwrap();
        assert!(again.is_revoked("fpGhost").unwrap());
        assert_eq!(audit_events(&log).len(), 3, "the sweep must not re-revoke");
    }

    #[test]
    fn a_new_issuance_sweeps_stale_undelivered_rows_on_the_same_handle() {
        // Liveness. The enrollment endpoint builds ONE ledger for the daemon's
        // whole lifetime, so an open-time-only sweep meant a failed enrollment
        // could leave an active row and an unchanged CRL until the daemon
        // restarted - the "one hour" bound was not a bound at all on that path.
        // Every issuance now sweeps first, so ordinary enrollment traffic is
        // what enforces it. Deliberately NO reopen anywhere in this test.
        let dir = tempfile::tempdir().unwrap();
        let (audit, log) = audit_logger(dir.path());
        let led = CertLedger::open(dir.path(), Some(audit)).unwrap();

        // A: issued, never delivered - the response write failed.
        led.record_issued(&entry("fpGhost", "devGhost"), false)
            .unwrap();
        backdate_issuance(dir.path(), "fpGhost", PAST_TTL_SECS);
        assert_eq!(
            led.status_of("fpGhost").unwrap(),
            Some(CertStatus::Active),
            "still active until something sweeps"
        );

        // B: an unrelated later enrollment, through the SAME handle.
        led.record_issued(&entry("fpNew", "devNew"), false).unwrap();

        // A is gone from service, without anyone reopening the ledger.
        assert_eq!(
            led.status_of("fpGhost").unwrap(),
            Some(CertStatus::Revoked),
            "a new issuance must sweep stale undelivered rows on a live handle"
        );
        let crl = std::fs::read_to_string(revoked_list_path(dir.path())).unwrap();
        assert_eq!(
            crl.trim(),
            "fpGhost",
            "the sweep must reach the CRL the verifier reads, with no reopen"
        );

        // B is untouched: young, and undelivered only because it has not been
        // marked yet. Sweeping it would break every enrollment in flight.
        assert_eq!(
            led.status_of("fpNew").unwrap(),
            Some(CertStatus::Active),
            "the issuance that triggered the sweep must not sweep itself"
        );
        assert!(led.mark_delivered("fpNew").unwrap());

        // The trail shows the revocation landing between the two issuances.
        let names: Vec<String> = audit_events(&log).iter().map(type_name).collect();
        assert_eq!(
            names,
            [
                "cert_issuance_attempted",
                "cert_issued",
                "cert_revoked",
                "cert_issuance_attempted",
                "cert_issued",
            ],
        );
    }

    /// Install a one-shot hook: it runs the closure the first time the seam is
    /// reached and never again, so a hook that itself performs ledger work
    /// cannot recurse into its own seam.
    fn once_hook(
        slot: &Mutex<Option<Box<dyn Fn() + Send + Sync>>>,
        action: impl Fn() + Send + Sync + 'static,
    ) {
        let fired = std::sync::atomic::AtomicBool::new(false);
        *slot.lock() = Some(Box::new(move || {
            if !fired.swap(true, std::sync::atomic::Ordering::SeqCst) {
                action();
            }
        }));
    }

    #[test]
    fn a_committed_revocation_cannot_be_overwritten_by_an_older_snapshot() {
        // Materialization is read-set, write-scratch, rename. A revocation
        // committing between the read and the rename would publish itself to
        // the file and then be silently overwritten by this older snapshot -
        // SQLite says revoked, the file the WSS verifier reads does not, and
        // the certificate keeps authenticating. Revocation may never fail in
        // that direction.
        //
        // The fix is that materialization holds SQLite's WRITE lock across all
        // three steps, so a revocation on another connection cannot interleave
        // - it waits. This forces exactly that interleaving through the seam
        // and proves the revocation survives.
        let dir = tempfile::tempdir().unwrap();
        let publisher = Arc::new(CertLedger::open(dir.path(), None).unwrap());
        publisher
            .record_issued(&entry("fpDoomed", "devD"), false)
            .unwrap();
        assert!(publisher.mark_delivered("fpDoomed").unwrap());

        // A SECOND handle - its own SQLite connection, exactly like a second
        // process running `security revoke-client-cert`.
        let revoker = Arc::new(CertLedger::open(dir.path(), None).unwrap());
        let revoked_ok = Arc::new(std::sync::atomic::AtomicBool::new(false));

        let revoker_thread = Arc::clone(&revoker);
        let flag = Arc::clone(&revoked_ok);
        once_hook(&publisher.hooks.before_crl_rename, move || {
            // Start the competing revocation and give it every chance to get
            // in. Under the fix it blocks on the write lock this thread holds;
            // the bounded wait is what keeps the test from hanging on that.
            let inner = Arc::clone(&revoker_thread);
            let inner_flag = Arc::clone(&flag);
            let handle = std::thread::spawn(move || {
                if inner.mark_revoked("fpDoomed", "operator").unwrap_or(false) {
                    inner_flag.store(true, std::sync::atomic::Ordering::SeqCst);
                }
            });
            std::thread::sleep(std::time::Duration::from_millis(300));
            // Deliberately NOT joined here: joining would deadlock against the
            // very lock this test is proving we hold.
            std::mem::drop(handle);
        });

        // Publishes an EMPTY snapshot - taken before the revocation exists.
        publisher.materialize_revocations().unwrap();

        // Let the revocation finish now that the lock is free.
        let deadline = std::time::Instant::now() + std::time::Duration::from_secs(10);
        while !revoked_ok.load(std::sync::atomic::Ordering::SeqCst)
            && std::time::Instant::now() < deadline
        {
            std::thread::sleep(std::time::Duration::from_millis(20));
        }
        assert!(
            revoked_ok.load(std::sync::atomic::Ordering::SeqCst),
            "the competing revocation must eventually commit, not fail"
        );

        // THE POINT: the committed revocation is still in the file the verifier
        // reads. It was not overwritten by the older, empty snapshot.
        let crl = revoked_list_path(dir.path());
        let body = std::fs::read_to_string(&crl).unwrap();
        assert_eq!(
            body.trim(),
            "fpDoomed",
            "a committed revocation must never be overwritten by an older snapshot"
        );
        let set = zeroclaw_tls::load_revoked_fingerprints(&crl).unwrap();
        assert!(set.contains("fpdoomed"), "the verifier must enforce it");
        assert!(revoker.is_revoked("fpDoomed").unwrap());
        // SQLite and the file agree, which is the invariant that was at risk.
        assert_eq!(
            publisher.revoked_fingerprints().unwrap(),
            vec!["fpDoomed".to_string()]
        );
    }

    #[test]
    fn a_delivery_that_lands_mid_sweep_beats_the_revocation() {
        // The sweep scans for stale undelivered rows, releases the connection,
        // and then revokes them one by one. A delivery can commit in that gap -
        // the client's response write finally succeeds and marks the row
        // delivered microseconds after the sweep listed it as abandoned.
        //
        // The flip must therefore re-test `delivered_at IS NULL` inside its own
        // UPDATE. Without that, the sweep revokes a certificate that was just
        // delivered, and permanently: the client is cut off with no recourse
        // but re-enrolment, for a delivery that actually succeeded.
        let dir = tempfile::tempdir().unwrap();
        let (audit, log) = audit_logger(dir.path());
        let led = Arc::new(CertLedger::open(dir.path(), Some(audit)).unwrap());
        led.record_issued(&entry("fpRaced", "devRaced"), false)
            .unwrap();
        backdate_issuance(dir.path(), "fpRaced", PAST_TTL_SECS);

        // The delivery lands after the scan has already selected the row.
        let deliverer = Arc::clone(&led);
        once_hook(&led.hooks.after_stale_scan, move || {
            assert!(
                deliverer.mark_delivered("fpRaced").unwrap(),
                "the delivery must win the row while the sweep is mid-flight"
            );
        });

        led.sweep_undelivered_certificates().unwrap();

        // Delivery wins: the certificate its client is now using stays usable.
        assert_eq!(
            led.status_of("fpRaced").unwrap(),
            Some(CertStatus::Active),
            "a certificate delivered mid-sweep must not be revoked"
        );
        assert!(
            led.revoked_fingerprints().unwrap().is_empty(),
            "and it must not reach the verifier's revocation list"
        );
        assert!(
            std::fs::read_to_string(revoked_list_path(dir.path()))
                .unwrap()
                .trim()
                .is_empty()
        );

        // A compare-and-set miss is a no-op, not a silent half-revocation: no
        // CertRevoked event claims something that did not happen.
        let names: Vec<String> = audit_events(&log).iter().map(type_name).collect();
        assert_eq!(
            names,
            ["cert_issuance_attempted", "cert_issued"],
            "a skipped revocation must not be audited as one"
        );

        // And the row is genuinely settled - a later sweep leaves it alone too.
        *led.hooks.after_stale_scan.lock() = None;
        led.sweep_undelivered_certificates().unwrap();
        assert_eq!(led.status_of("fpRaced").unwrap(), Some(CertStatus::Active));
    }

    #[test]
    fn a_revocation_that_lands_mid_renewal_stops_the_new_certificate() {
        // Renewal reads the presenting certificate's status, resolves its
        // device, signs, and only then records - on a connection the operator's
        // revoke does not share. An operator revoking that device in between
        // would see their revocation succeed and the renewal still hand the
        // device a fresh ACTIVE certificate: the operator believes the device
        // is off the network while it holds a brand-new credential.
        //
        // The presenting fingerprint is therefore a precondition of the
        // publishing transaction. Here the revocation lands at the worst
        // possible moment - after the new row is already staged.
        let dir = tempfile::tempdir().unwrap();
        let (audit, log) = audit_logger(dir.path());
        let led = Arc::new(CertLedger::open(dir.path(), Some(audit)).unwrap());
        led.record_issued(&entry("fpOld", "devRenew"), false)
            .unwrap();
        assert!(led.mark_delivered("fpOld").unwrap());

        let revoker = Arc::clone(&led);
        once_hook(&led.hooks.after_stage, move || {
            assert!(
                revoker.mark_revoked("fpOld", "operator").unwrap(),
                "the operator's revocation must land while the renewal is staged"
            );
        });

        let err = format!(
            "{:#}",
            led.record_issued_requiring(&entry("fpNew", "devRenew"), true, Some("fpOld"))
                .expect_err("a renewal must not publish after its certificate is revoked")
        );
        assert!(
            err.contains(ISSUANCE_PRECONDITION_FAILED),
            "the refusal must be distinguishable from an internal fault: {err}"
        );
        assert!(
            err.contains("re-enroll"),
            "and must tell the client what to do instead: {err}"
        );

        // Revocation wins, completely: no new active certificate exists for the
        // device, and the staged row was compensated away rather than left.
        assert_eq!(led.status_of("fpNew").unwrap(), None);
        assert_eq!(led.status_of("fpOld").unwrap(), Some(CertStatus::Revoked));
        assert!(
            led.list_active().unwrap().is_empty(),
            "the revoked device must hold no active certificate, got: {:?}",
            led.list_active().unwrap()
        );
        assert_eq!(
            stored_rows(dir.path()),
            vec![("fpOld".to_string(), "revoked".to_string())],
            "the refused renewal must leave no row behind"
        );

        // The trail records the renewal as attempted and even completed - the
        // completion event is written before the publishing transaction, and
        // the revocation landed after it. That is the residue `record_issued`
        // already documents and deliberately accepts: an audited fingerprint
        // with no row fails CLOSED (nothing to authenticate with), whereas the
        // opposite - a live credential with no audit - does not. What must
        // never appear is a published row, and there is none.
        let names: Vec<String> = audit_events(&log).iter().map(type_name).collect();
        assert_eq!(
            names,
            [
                "cert_issuance_attempted",
                "cert_issued",
                "cert_issuance_attempted",
                "cert_revoked",
                "cert_renewed",
            ],
            "the renewal may be audited, but must publish nothing"
        );
        assert_eq!(
            led.status_of("fpNew").unwrap(),
            None,
            "an audited-but-unpublished renewal is unusable, which is the point"
        );
    }

    /// Every still-active fingerprint held by a device.
    fn active_for_device(led: &CertLedger, device_id: &str) -> Vec<String> {
        led.list_active()
            .unwrap()
            .into_iter()
            .filter(|e| e.device_id == device_id)
            .map(|e| e.fingerprint)
            .collect()
    }

    #[test]
    fn device_revocation_catches_a_renewal_racing_its_snapshot() {
        // The operator's "get that device off the network" command must be
        // total. It used to take a snapshot of the device's active
        // fingerprints with no transaction, release it, and then revoke each
        // one separately - so a renewal publishing a NEW active row for the
        // same device in that gap survived. The command reported success and
        // the device kept a certificate it had been handed moments earlier.
        //
        // The seam fires at exactly the snapshot moment and publishes a renewal
        // from a SECOND connection, which is the interleaving that defeated the
        // old implementation.
        let dir = tempfile::tempdir().unwrap();
        let (audit, _log) = audit_logger(dir.path());
        let revoker = Arc::new(CertLedger::open(dir.path(), Some(audit)).unwrap());
        revoker
            .record_issued(&entry("fpOld", "devRaced"), false)
            .unwrap();
        assert!(revoker.mark_delivered("fpOld").unwrap());

        // A second handle: its own SQLite connection, as a renewal RPC has.
        let renewer = Arc::new(CertLedger::open(dir.path(), None).unwrap());
        let renewal_done = Arc::new(std::sync::atomic::AtomicBool::new(false));

        let renewer_thread = Arc::clone(&renewer);
        let done = Arc::clone(&renewal_done);
        once_hook(&revoker.hooks.after_device_snapshot, move || {
            let inner = Arc::clone(&renewer_thread);
            let inner_done = Arc::clone(&done);
            std::thread::spawn(move || {
                // Publishes a second active certificate for the SAME device.
                let _ =
                    inner.record_issued_requiring(&entry("fpNew", "devRaced"), true, Some("fpOld"));
                inner_done.store(true, std::sync::atomic::Ordering::SeqCst);
            });
            // Give the renewal every chance to get in. Under the fix it blocks
            // on the write lock this transaction holds; joining here would
            // deadlock against that lock, so the wait is bounded instead.
            std::thread::sleep(std::time::Duration::from_millis(300));
        });

        let n = revoker.revoke_device("devRaced", "operator").unwrap();
        assert!(n >= 1, "the device's certificate must be revoked");

        let deadline = std::time::Instant::now() + std::time::Duration::from_secs(10);
        while !renewal_done.load(std::sync::atomic::Ordering::SeqCst)
            && std::time::Instant::now() < deadline
        {
            std::thread::sleep(std::time::Duration::from_millis(20));
        }
        assert!(
            renewal_done.load(std::sync::atomic::Ordering::SeqCst),
            "the racing renewal must finish one way or the other"
        );

        // THE INVARIANT: whatever order the two landed in, the device is off
        // the network. Either the renewal was refused (its presenting
        // certificate was revoked first) or it published and was swept - never
        // "published and survived".
        let reader = CertLedger::open(dir.path(), None).unwrap();
        assert_eq!(
            active_for_device(&reader, "devRaced"),
            Vec::<String>::new(),
            "a revoked device must hold NO active certificate"
        );
        assert_eq!(
            reader.status_of("fpOld").unwrap(),
            Some(CertStatus::Revoked)
        );

        // If the renewal did publish, it must be revoked in SQLite AND in the
        // file the verifier reads - not merely absent from list_active.
        let crl = revoked_list_path(dir.path());
        let enforced = zeroclaw_tls::load_revoked_fingerprints(&crl).unwrap();
        assert!(enforced.contains("fpold"), "the old cert must be enforced");
        if let Some(status) = reader.status_of("fpNew").unwrap() {
            assert_eq!(
                status,
                CertStatus::Revoked,
                "a renewal that published during a device revoke must be revoked"
            );
            assert!(
                enforced.contains("fpnew"),
                "and it must reach the verifier's CRL, got: {enforced:?}"
            );
        }
    }

    #[test]
    fn device_revocation_sweeps_a_certificate_published_before_it_runs() {
        // Ordering 2 from the doc comment, deterministically: the renewal
        // commits first, so the device holds TWO active certificates when the
        // revoke runs. Both must go, and both must reach the CRL - the command
        // operates on committed state, not on any earlier view.
        let dir = tempfile::tempdir().unwrap();
        let (audit, log) = audit_logger(dir.path());
        let led = CertLedger::open(dir.path(), Some(audit)).unwrap();
        led.record_issued(&entry("fpOld", "devTwo"), false).unwrap();
        assert!(led.mark_delivered("fpOld").unwrap());
        led.record_issued_requiring(&entry("fpNew", "devTwo"), true, Some("fpOld"))
            .expect("the renewal publishes while the old certificate is active");
        assert!(led.mark_delivered("fpNew").unwrap());
        let mut before = active_for_device(&led, "devTwo");
        before.sort();
        assert_eq!(before, ["fpNew", "fpOld"], "both must be live to start");

        assert_eq!(led.revoke_device("devTwo", "operator").unwrap(), 2);

        assert_eq!(active_for_device(&led, "devTwo"), Vec::<String>::new());
        assert_eq!(led.status_of("fpOld").unwrap(), Some(CertStatus::Revoked));
        assert_eq!(led.status_of("fpNew").unwrap(), Some(CertStatus::Revoked));
        let enforced =
            zeroclaw_tls::load_revoked_fingerprints(&revoked_list_path(dir.path())).unwrap();
        assert!(enforced.contains("fpold") && enforced.contains("fpnew"));

        // Every revoked certificate is audited individually, with the operator
        // as the actor - a batched flip must not collapse into one event.
        let revocations: Vec<AuditEvent> = audit_events(&log)
            .into_iter()
            .filter(|e| type_name(e) == "cert_revoked")
            .collect();
        assert_eq!(revocations.len(), 2, "one audit event per revoked cert");
        for event in &revocations {
            assert_eq!(
                event.actor.as_ref().and_then(|a| a.username.clone()),
                Some("operator".to_string())
            );
        }
    }

    #[test]
    fn a_device_revocation_that_wins_refuses_the_later_renewal() {
        // Ordering 1 from the doc comment: the revoke commits first, so the
        // renewal's presenting certificate is already revoked and its publish
        // is refused. Pinned device-scoped, since `revoke_device` is the
        // operator command that has to make this promise.
        let dir = tempfile::tempdir().unwrap();
        let led = CertLedger::open(dir.path(), None).unwrap();
        led.record_issued(&entry("fpOld", "devLate"), false)
            .unwrap();
        assert!(led.mark_delivered("fpOld").unwrap());
        assert_eq!(led.revoke_device("devLate", "operator").unwrap(), 1);

        let err = format!(
            "{:#}",
            led.record_issued_requiring(&entry("fpNew", "devLate"), true, Some("fpOld"))
                .expect_err("a revoked device must not renew")
        );
        assert!(err.contains(ISSUANCE_PRECONDITION_FAILED), "got: {err}");
        assert_eq!(active_for_device(&led, "devLate"), Vec::<String>::new());
        assert_eq!(led.status_of("fpNew").unwrap(), None);
    }

    #[test]
    fn revoking_a_device_leaves_other_devices_alone() {
        // The batched UPDATE is scoped by device_id; a wider WHERE clause would
        // take the whole fleet off the network in one command.
        let dir = tempfile::tempdir().unwrap();
        let led = CertLedger::open(dir.path(), None).unwrap();
        led.record_issued(&entry("fpA", "devA"), false).unwrap();
        led.record_issued(&entry("fpB", "devB"), false).unwrap();
        assert!(led.mark_delivered("fpA").unwrap());
        assert!(led.mark_delivered("fpB").unwrap());

        assert_eq!(led.revoke_device("devA", "operator").unwrap(), 1);

        assert_eq!(led.status_of("fpA").unwrap(), Some(CertStatus::Revoked));
        assert_eq!(led.status_of("fpB").unwrap(), Some(CertStatus::Active));
        assert_eq!(active_for_device(&led, "devB"), ["fpB"]);
        // Revoking a device with nothing active is a no-op that reports zero.
        assert_eq!(led.revoke_device("devA", "operator").unwrap(), 0);
        assert_eq!(led.revoke_device("nobody", "operator").unwrap(), 0);
    }

    #[test]
    fn a_renewal_of_an_already_revoked_certificate_is_refused_up_front() {
        // The same guarantee without any interleaving: revocation already won,
        // so the renewal is refused before it writes anything at all.
        let dir = tempfile::tempdir().unwrap();
        let (audit, log) = audit_logger(dir.path());
        let led = CertLedger::open(dir.path(), Some(audit)).unwrap();
        led.record_issued(&entry("fpOld", "devRenew"), false)
            .unwrap();
        assert!(led.mark_delivered("fpOld").unwrap());
        assert!(led.mark_revoked("fpOld", "operator").unwrap());

        let err = format!(
            "{:#}",
            led.record_issued_requiring(&entry("fpNew", "devRenew"), true, Some("fpOld"))
                .expect_err("a revoked certificate must not renew")
        );
        assert!(err.contains(ISSUANCE_PRECONDITION_FAILED), "got: {err}");
        assert_eq!(led.status_of("fpNew").unwrap(), None);
        assert!(led.list_active().unwrap().is_empty());

        // Refused before the attempt event: nothing was tried, so nothing is
        // recorded as tried.
        let names: Vec<String> = audit_events(&log).iter().map(type_name).collect();
        assert_eq!(
            names,
            ["cert_issuance_attempted", "cert_issued", "cert_revoked"]
        );
    }

    #[test]
    fn a_first_enrollment_carries_no_renewal_precondition() {
        // The precondition is renewal-only. A first enrollment has no
        // presenting certificate, and must not be gated on one.
        let led = CertLedger::open_in_memory(None).unwrap();
        led.record_issued_requiring(&entry("fp1", "dev1"), false, None)
            .expect("a first issuance has nothing to precondition on");
        assert_eq!(led.status_of("fp1").unwrap(), Some(CertStatus::Active));
    }

    #[test]
    fn an_explicit_sweep_reconciles_without_reopening() {
        // The public trigger the long-lived enrollment handle uses when it is
        // not issuing anything - a client connects, fails to authenticate, and
        // that activity is still enough to reconcile earlier residue.
        let dir = tempfile::tempdir().unwrap();
        let led = CertLedger::open(dir.path(), None).unwrap();
        led.record_issued(&entry("fpGhost", "devGhost"), false)
            .unwrap();
        backdate_issuance(dir.path(), "fpGhost", PAST_TTL_SECS);

        led.sweep_undelivered_certificates().unwrap();

        assert_eq!(
            led.status_of("fpGhost").unwrap(),
            Some(CertStatus::Revoked),
            "an explicit sweep must reconcile without a reopen"
        );
        // Idempotent: sweeping again is a no-op, not a second revocation.
        led.sweep_undelivered_certificates().unwrap();
        assert_eq!(
            led.revoked_fingerprints().unwrap(),
            vec!["fpGhost".to_string()]
        );
    }

    #[test]
    fn a_delivered_row_survives_the_undelivered_sweep() {
        // The other half of the contract, and the one that keeps the sweep from
        // being a slow-motion outage: a certificate whose delivery boundary
        // succeeded is never touched, however old it gets.
        let dir = tempfile::tempdir().unwrap();
        {
            let led = CertLedger::open(dir.path(), None).unwrap();
            led.record_issued(&entry("fpReal", "devReal"), false)
                .unwrap();
            assert!(
                led.mark_delivered("fpReal").unwrap(),
                "the delivery boundary marks the row"
            );
        }
        assert!(
            delivery_marks(dir.path())[0].1.is_some(),
            "delivery must be recorded durably, not in memory"
        );
        backdate_issuance(dir.path(), "fpReal", PAST_TTL_SECS * 24);

        let reopened = CertLedger::open(dir.path(), None).unwrap();

        assert_eq!(
            reopened.status_of("fpReal").unwrap(),
            Some(CertStatus::Active),
            "a delivered certificate must never be swept"
        );
        assert!(reopened.revoked_fingerprints().unwrap().is_empty());
    }

    #[test]
    fn an_undelivered_row_inside_the_ttl_is_left_alone() {
        // The sweep is a deadline, not a mode. A just-issued row is undelivered
        // for the width of the response write; revoking THAT would break every
        // enrollment instead of only the failed ones.
        let dir = tempfile::tempdir().unwrap();
        {
            let led = CertLedger::open(dir.path(), None).unwrap();
            let mut fresh = entry("fpFresh", "devFresh");
            fresh.issued_at = now_unix();
            led.record_issued(&fresh, false).unwrap();
        }

        let reopened = CertLedger::open(dir.path(), None).unwrap();

        assert_eq!(
            reopened.status_of("fpFresh").unwrap(),
            Some(CertStatus::Active),
            "an in-flight issuance must survive a ledger open"
        );
        assert!(reopened.revoked_fingerprints().unwrap().is_empty());
    }

    #[test]
    fn mark_delivered_is_idempotent_and_only_marks_a_live_row() {
        let dir = tempfile::tempdir().unwrap();
        let led = CertLedger::open(dir.path(), None).unwrap();
        led.record_issued(&entry("fp1", "dev1"), false).unwrap();

        assert!(led.mark_delivered("fp1").unwrap(), "first mark takes");
        let first = delivery_marks(dir.path())[0].1.expect("marked");
        assert!(
            !led.mark_delivered("fp1").unwrap(),
            "a repeat mark reports no change rather than failing"
        );
        assert_eq!(
            delivery_marks(dir.path())[0].1,
            Some(first),
            "a repeat mark must keep the FIRST delivery instant"
        );

        // A fingerprint the ledger has never seen is not markable.
        assert!(!led.mark_delivered("nope").unwrap());

        // Neither is a pending row: the ledger does not vouch for it, so it has
        // no delivery to claim.
        stage_pending_row(dir.path(), "fpPending", "devP");
        assert!(!led.mark_delivered("fpPending").unwrap());
        assert_eq!(
            delivery_marks(dir.path())
                .into_iter()
                .find(|(fp, _)| fp == "fpPending")
                .unwrap()
                .1,
            None,
        );

        // Nor a revoked one - marking it would contradict the revocation.
        led.record_issued(&entry("fp2", "dev2"), false).unwrap();
        assert!(led.mark_revoked("fp2", "operator").unwrap());
        assert!(!led.mark_delivered("fp2").unwrap());
    }

    #[test]
    fn re_recording_a_delivered_fingerprint_keeps_its_delivery_mark() {
        // Guards the promote_staged UPDATE. The INSERT OR REPLACE it replaced
        // was a delete plus an insert, so re-recording an established
        // fingerprint reset delivered_at to NULL - and the sweep would then
        // revoke a certificate its keyholder is actively using, an hour after a
        // renewal that produced the same certificate.
        let dir = tempfile::tempdir().unwrap();
        {
            let led = CertLedger::open(dir.path(), None).unwrap();
            let mut e = entry("fp1", "dev1");
            led.record_issued(&e, false).unwrap();
            assert!(led.mark_delivered("fp1").unwrap());
            let delivered = delivery_marks(dir.path())[0].1.expect("marked");

            // A renewal that lands on the same fingerprint updates validity.
            e.not_after = 9_999_999;
            led.record_issued(&e, true).unwrap();
            assert_eq!(
                led.lookup_by_fingerprint("fp1").unwrap().unwrap().not_after,
                9_999_999,
                "the re-record must still update the row"
            );
            assert_eq!(
                delivery_marks(dir.path())[0].1,
                Some(delivered),
                "an issuance write must never clear a delivery mark"
            );
        }

        backdate_issuance(dir.path(), "fp1", PAST_TTL_SECS);
        let reopened = CertLedger::open(dir.path(), None).unwrap();
        assert_eq!(
            reopened.status_of("fp1").unwrap(),
            Some(CertStatus::Active),
            "the established credential must survive the sweep after a re-record"
        );
    }

    #[test]
    fn migrating_a_v1_ledger_backfills_delivery_and_never_mass_revokes() {
        // THE mass-revocation guard. Rows written before delivery tracking
        // existed carry no evidence of delivery, and the undelivered sweep
        // revokes anything active it finds unmarked. Migrate them as NULL and
        // the first upgraded start would revoke every live certificate on the
        // host - every device locked out at once, by an upgrade. The backfill
        // (delivered_at = issued_at) is what makes an upgrade a no-op for the
        // fleet, and it is load-bearing enough to pin explicitly.
        let dir = tempfile::tempdir().unwrap();
        create_v1_ledger(dir.path(), 1);
        assert_eq!(user_version(dir.path()), 1, "fixture must be a v1 ledger");
        assert!(
            !table_sql(dir.path()).contains("delivered_at"),
            "fixture must predate the delivery column"
        );

        let led = CertLedger::open(dir.path(), None).unwrap();

        // Migrated to v2, and every row's delivery stamped from its issuance.
        assert_eq!(user_version(dir.path()), SCHEMA_VERSION);
        assert_eq!(
            delivery_marks(dir.path()),
            vec![
                ("fpDead".to_string(), Some(550)),
                ("fpLiveA".to_string(), Some(150)),
                ("fpLiveB".to_string(), Some(350)),
            ],
            "every pre-existing row must be backfilled with delivered_at = issued_at"
        );

        // The point: these certificates are ANCIENT (issued_at 150/350, i.e.
        // 1970), so they are far past the undelivered TTL. The sweep ran during
        // that open and left them alone.
        let mut active: Vec<String> = led
            .list_active()
            .unwrap()
            .into_iter()
            .map(|e| e.fingerprint)
            .collect();
        active.sort();
        assert_eq!(
            active,
            ["fpLiveA", "fpLiveB"],
            "an upgrade must not revoke the operator's live certificates"
        );
        assert!(led.revoked_fingerprints().unwrap() == vec!["fpDead".to_string()]);

        // And it stays a no-op across further opens - the backfill is durable,
        // not a per-open suppression.
        drop(led);
        let again = CertLedger::open(dir.path(), None).unwrap();
        assert_eq!(again.list_active().unwrap().len(), 2);
    }

    #[test]
    fn migrating_a_v0_ledger_reaches_v2_in_one_pass() {
        // The other early-adopter shape. v0 (narrow CHECK) and v1 (wide CHECK)
        // differ only in a constraint and neither has delivered_at, so ONE
        // rebuild has to carry either of them all the way to v2 - there is no
        // v0 -> v1 -> v2 staircase to fall back on.
        let dir = tempfile::tempdir().unwrap();
        create_v0_ledger(dir.path());
        assert_eq!(user_version(dir.path()), 0);

        let led = CertLedger::open(dir.path(), None).unwrap();

        assert_eq!(
            user_version(dir.path()),
            SCHEMA_VERSION,
            "a v0 ledger must land on v2 in one pass, not on an intermediate"
        );
        assert!(
            table_sql(dir.path()).contains("'active','revoked','pending')"),
            "the v1 widening must still be applied on the way through"
        );
        assert!(table_sql(dir.path()).contains("delivered_at"));
        assert_eq!(
            delivery_marks(dir.path()),
            vec![
                ("fpOldActive".to_string(), Some(150)),
                ("fpOldRevoked".to_string(), Some(350)),
            ],
        );
        // Same guard as v1: the ancient active row survives the sweep.
        assert_eq!(
            led.status_of("fpOldActive").unwrap(),
            Some(CertStatus::Active)
        );
    }

    #[test]
    fn issuance_actor_labels() {
        assert_eq!(IssuanceActor::Operator.label(), "operator");
        assert_eq!(
            IssuanceActor::Enrollment {
                token_hash: "0123456789abcdef".to_string()
            }
            .label(),
            "enroll:01234567"
        );
    }
}
