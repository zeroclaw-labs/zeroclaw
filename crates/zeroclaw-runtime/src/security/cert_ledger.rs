//! Daemon-owned issued-certificate ledger.
//!
//! The single canonical record of which device holds which client certificate
//! (device id + SHA-256 fingerprint + validity + status), the join key back to
//! the gateway pairing `devices.db` (`token_hash`), and the choke point that
//! writes the append-only certificate audit trail. The daemon owns the CA, so it
//! owns this ledger; when the gateway is present it READS the ledger (one store,
//! two readers - no third device store, per AGENTS.md no-duplicate-state).
//!
//! Revocation is sourced here. The renew RPC (`cert/renew`) refuses a
//! revoked-but-unexpired cert immediately by consulting [`CertLedger::status_of`]
//! (threat A5). The WSS handshake-time refusal is wired separately against this
//! ledger via [`CertLedger::revoked_fingerprints`] / [`CertLedger::is_revoked`].

use anyhow::{Context, Result};
use parking_lot::Mutex;
use rusqlite::{Connection, OptionalExtension, params};
use std::path::Path;
use std::sync::Arc;

use super::audit::{AuditEvent, AuditEventType, AuditLogger};

/// Status of an issued certificate.
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

    fn from_db(s: &str) -> CertStatus {
        match s {
            "revoked" => CertStatus::Revoked,
            _ => CertStatus::Active,
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

/// The daemon's issued-certificate ledger over SQLite (`<data_dir>/tls/ledger.db`).
pub struct CertLedger {
    conn: Mutex<Connection>,
    audit: Option<Arc<AuditLogger>>,
    /// Where revocations are materialized for the WSS verifier to read
    /// (`<data_dir>/tls/revoked`). `None` for an in-memory ledger.
    revoked_path: Option<std::path::PathBuf>,
}

/// The revoked-fingerprint list the daemon's WSS mTLS verifier reads for
/// connect-time revocation refusal (A5). The ledger materializes it on revoke.
pub fn revoked_list_path(data_dir: &Path) -> std::path::PathBuf {
    data_dir.join("tls").join("revoked")
}

impl CertLedger {
    /// Open (creating if absent) the ledger at `<data_dir>/tls/ledger.db`. The CA
    /// already lives under `<data_dir>/tls/`, so the ledger sits beside it.
    pub fn open(data_dir: &Path, audit: Option<Arc<AuditLogger>>) -> Result<Self> {
        let tls_dir = data_dir.join("tls");
        std::fs::create_dir_all(&tls_dir)
            .with_context(|| format!("create tls dir {}", tls_dir.display()))?;
        let db_path = tls_dir.join("ledger.db");
        let conn = Connection::open(&db_path)
            .with_context(|| format!("open cert ledger DB: {}", db_path.display()))?;
        Self::init(conn, audit, Some(revoked_list_path(data_dir)))
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
        conn: Connection,
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
        conn.execute_batch(
            "CREATE TABLE IF NOT EXISTS issued_certs (
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
             CREATE INDEX IF NOT EXISTS idx_issued_certs_token  ON issued_certs(token_hash);",
        )
        .context("create cert-ledger schema")?;
        let ledger = Self {
            conn: Mutex::new(conn),
            audit,
            revoked_path,
        };
        // Refresh the materialized revocation list so it reflects the ledger at
        // startup (covers a missing/stale file).
        if let Err(e) = ledger.materialize_revocations() {
            ::zeroclaw_log::record!(
                WARN,
                ::zeroclaw_log::Event::new(module_path!(), ::zeroclaw_log::Action::Fail)
                    .with_outcome(::zeroclaw_log::EventOutcome::Failure)
                    .with_attrs(::serde_json::json!({ "error": format!("{e:#}") })),
                "cert ledger: failed to materialize the revocation list at open"
            );
        }
        Ok(ledger)
    }

    /// Rewrite the revoked-fingerprint file from the SQLite truth (atomic temp +
    /// rename). This is what makes a revoke take effect at the next handshake -
    /// the WSS verifier re-reads the file when its mtime changes. No-op for an
    /// in-memory ledger.
    fn materialize_revocations(&self) -> Result<()> {
        let Some(path) = &self.revoked_path else {
            return Ok(());
        };
        let revoked = self.revoked_fingerprints()?;
        let body = if revoked.is_empty() {
            String::new()
        } else {
            format!("{}\n", revoked.join("\n"))
        };
        if let Some(dir) = path.parent() {
            std::fs::create_dir_all(dir).with_context(|| format!("create {}", dir.display()))?;
        }
        let tmp = path.with_extension("tmp");
        std::fs::write(&tmp, body).with_context(|| format!("write {}", tmp.display()))?;
        std::fs::rename(&tmp, path).context("atomically replace the revocation list")?;
        Ok(())
    }

    /// Record a freshly issued (or renewed) certificate and write the matching
    /// append-only audit event. `renewal` selects `CertRenewed` vs `CertIssued`.
    pub fn record_issued(&self, entry: &LedgerEntry, renewal: bool) -> Result<()> {
        {
            let conn = self.conn.lock();
            conn.execute(
                "INSERT OR REPLACE INTO issued_certs
                    (fingerprint, device_id, token_hash, not_before, not_after, status, issued_at, actor)
                 VALUES (?1, ?2, ?3, ?4, ?5, ?6, ?7, ?8)",
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
            .context("insert issued cert")?;
        }
        let kind = if renewal {
            AuditEventType::CertRenewed
        } else {
            AuditEventType::CertIssued
        };
        self.audit_cert(kind, entry);
        Ok(())
    }

    /// The status of a cert by fingerprint, or `None` if unknown to the ledger.
    pub fn status_of(&self, fingerprint: &str) -> Result<Option<CertStatus>> {
        let conn = self.conn.lock();
        let s: Option<String> = conn
            .query_row(
                "SELECT status FROM issued_certs WHERE fingerprint = ?1",
                params![fingerprint],
                |r| r.get(0),
            )
            .optional()
            .context("query cert status")?;
        Ok(s.map(|s| CertStatus::from_db(&s)))
    }

    /// True iff the cert is known AND revoked. Unknown certs are not "revoked"
    /// here (the WSS verifier still rejects an unknown cert via CA-chain / pin).
    pub fn is_revoked(&self, fingerprint: &str) -> Result<bool> {
        Ok(matches!(
            self.status_of(fingerprint)?,
            Some(CertStatus::Revoked)
        ))
    }

    /// Look up the full ledger row for a fingerprint.
    pub fn lookup_by_fingerprint(&self, fingerprint: &str) -> Result<Option<LedgerEntry>> {
        let conn = self.conn.lock();
        conn.query_row(
            "SELECT fingerprint, device_id, token_hash, not_before, not_after, status, actor, issued_at
                 FROM issued_certs WHERE fingerprint = ?1",
            params![fingerprint],
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
    pub fn mark_revoked(&self, fingerprint: &str, actor: &str) -> Result<bool> {
        let changed = {
            let conn = self.conn.lock();
            conn.execute(
                "UPDATE issued_certs SET status = 'revoked'
                     WHERE fingerprint = ?1 AND status != 'revoked'",
                params![fingerprint],
            )
            .context("revoke cert")?
        };
        if changed > 0 {
            if let Some(mut entry) = self.lookup_by_fingerprint(fingerprint)? {
                entry.actor = actor.to_string();
                self.audit_cert(AuditEventType::CertRevoked, &entry);
            }
            // Materialize so the WSS verifier refuses this cert on the next
            // connect (drives the A5 refusal from the real revoke action).
            self.materialize_revocations()?;
        }
        Ok(changed > 0)
    }

    /// Revoke every active cert held by a device (e.g. a compromised device).
    /// Returns the number of certs revoked.
    pub fn revoke_device(&self, device_id: &str, actor: &str) -> Result<usize> {
        let fingerprints: Vec<String> = {
            let conn = self.conn.lock();
            let mut stmt = conn
                .prepare("SELECT fingerprint FROM issued_certs WHERE device_id = ?1 AND status = 'active'")
                .context("prepare device revoke")?;
            let rows = stmt
                .query_map(params![device_id], |r| r.get::<_, String>(0))
                .context("query device certs")?;
            rows.collect::<rusqlite::Result<Vec<String>>>()
                .context("collect device certs")?
        };
        let mut n = 0;
        for fp in fingerprints {
            if self.mark_revoked(&fp, actor)? {
                n += 1;
            }
        }
        Ok(n)
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
        let mut stmt = conn
            .prepare("SELECT fingerprint FROM issued_certs WHERE status = 'revoked'")
            .context("prepare revoked_fingerprints")?;
        let rows = stmt
            .query_map([], |r| r.get::<_, String>(0))
            .context("query revoked_fingerprints")?;
        rows.collect::<rusqlite::Result<Vec<String>>>()
            .context("collect revoked_fingerprints")
    }

    /// Emit a hash-chained audit event for a certificate lifecycle action. The
    /// security audit log is already separate from operational logs; cert facts
    /// (device id, fingerprint, validity, actor) are recorded in the existing
    /// actor/action fields so the entry is covered by the Merkle chain.
    fn audit_cert(&self, kind: AuditEventType, entry: &LedgerEntry) {
        let Some(audit) = &self.audit else {
            return;
        };
        let event = AuditEvent::new(kind)
            .with_actor(
                "cert".to_string(),
                Some(entry.device_id.clone()),
                Some(entry.actor.clone()),
            )
            .with_action(
                format!(
                    "fingerprint={} not_before={} not_after={}",
                    entry.fingerprint, entry.not_before, entry.not_after
                ),
                "cert".to_string(),
                true,
                true,
            )
            .with_result(true, None, 0, None);
        if let Err(e) = audit.log(&event) {
            ::zeroclaw_log::record!(
                ERROR,
                ::zeroclaw_log::Event::new(module_path!(), ::zeroclaw_log::Action::Fail)
                    .with_outcome(::zeroclaw_log::EventOutcome::Failure)
                    .with_attrs(::serde_json::json!({"fingerprint": entry.fingerprint})),
                "failed to write certificate audit event"
            );
            let _ = e;
        }
    }
}

fn row_to_entry(row: &rusqlite::Row<'_>) -> rusqlite::Result<LedgerEntry> {
    let status: String = row.get(5)?;
    Ok(LedgerEntry {
        fingerprint: row.get(0)?,
        device_id: row.get(1)?,
        token_hash: row.get(2)?,
        not_before: row.get(3)?,
        not_after: row.get(4)?,
        status: CertStatus::from_db(&status),
        actor: row.get(6)?,
        issued_at: row.get(7)?,
    })
}

#[cfg(test)]
mod tests {
    use super::*;

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
            issued_at: 1_000,
        }
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
        let set = zeroclaw_tls::load_revoked_fingerprints(&crl);
        assert!(set.contains("fpa")); // load normalizes to lowercase
        assert!(!set.contains("fpb"));
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
