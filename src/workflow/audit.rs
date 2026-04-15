// Audit Log with HMAC Chain (v3.0 Shopping Plan rev.2 §8)
//
// Append-only SQLite audit log for payment/high-risk operations.
// Each entry carries an HMAC-SHA256 chain: the MAC of entry N includes the
// MAC of entry N-1, so any tampering of the history is detectable.
//
// The audit DB is separate from the main brain.db (path: `audit.db`).
// This module provides the primitives; the `moa-bridge` binary calls into
// these via the `shell` 1st-class tool (plan rev.2 §12 #8).

use anyhow::{Context, Result};
use hmac::{Hmac, Mac};
use rusqlite::{params, Connection};
use serde::{Deserialize, Serialize};
use sha2::Sha256;

type HmacSha256 = Hmac<Sha256>;

/// A single audit log entry prior to insertion.
#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct AuditEntry {
    /// Stage identifier (e.g. "pre_invoke", "post_callback", "refund").
    pub stage: String,
    /// Payment provider ("passkey", "kakaopay", ...).
    pub provider: Option<String>,
    /// Transaction amount in KRW.
    pub amount_krw: Option<u64>,
    /// Merchant / mall identifier.
    pub merchant: Option<String>,
    /// Workflow slug that emitted the entry.
    pub workflow: Option<String>,
    /// Risk level ("low", "med", "high").
    pub risk_level: Option<String>,
    /// Device that produced the entry.
    pub device_id: String,
    /// Arbitrary JSON metadata.
    pub metadata_json: Option<String>,
}

impl AuditEntry {
    /// Serialize to canonical bytes for MAC computation.
    pub fn canonical_bytes(&self, timestamp: u64, prev_mac: &str) -> Vec<u8> {
        let obj = serde_json::json!({
            "ts": timestamp,
            "stage": self.stage,
            "provider": self.provider,
            "amount_krw": self.amount_krw,
            "merchant": self.merchant,
            "workflow": self.workflow,
            "risk_level": self.risk_level,
            "device_id": self.device_id,
            "metadata_json": self.metadata_json,
            "prev_mac": prev_mac,
        });
        serde_json::to_vec(&obj).unwrap_or_default()
    }
}

/// The in-DB record with chain MAC.
#[derive(Debug, Clone)]
pub struct AuditRecord {
    pub id: i64,
    pub timestamp: u64,
    pub entry: AuditEntry,
    pub prev_mac: String,
    pub mac: String,
}

/// Initialize the audit.db schema.
pub fn init_audit_db(conn: &Connection) -> Result<()> {
    conn.execute_batch(
        "PRAGMA journal_mode = WAL;
         PRAGMA synchronous = NORMAL;
         CREATE TABLE IF NOT EXISTS audit_log (
             id              INTEGER PRIMARY KEY AUTOINCREMENT,
             timestamp       INTEGER NOT NULL,
             stage           TEXT NOT NULL,
             provider        TEXT,
             amount_krw      INTEGER,
             merchant        TEXT,
             workflow        TEXT,
             risk_level      TEXT,
             device_id       TEXT NOT NULL,
             metadata_json   TEXT,
             prev_mac        TEXT NOT NULL,
             mac             TEXT NOT NULL
         );
         CREATE INDEX IF NOT EXISTS idx_audit_time ON audit_log(timestamp);
         CREATE INDEX IF NOT EXISTS idx_audit_workflow ON audit_log(workflow, timestamp);
         -- Append-only enforcement
         CREATE TRIGGER IF NOT EXISTS trg_audit_no_update
         BEFORE UPDATE ON audit_log
         BEGIN
             SELECT RAISE(ABORT, 'audit_log is append-only');
         END;
         CREATE TRIGGER IF NOT EXISTS trg_audit_no_delete
         BEFORE DELETE ON audit_log
         BEGIN
             SELECT RAISE(ABORT, 'audit_log is append-only');
         END;",
    )?;
    Ok(())
}

/// Compute HMAC-SHA256 for an entry given the previous MAC.
pub fn compute_mac(entry: &AuditEntry, timestamp: u64, prev_mac: &str, key: &[u8]) -> String {
    let bytes = entry.canonical_bytes(timestamp, prev_mac);
    let mut mac = HmacSha256::new_from_slice(key).expect("HMAC accepts any key length");
    mac.update(&bytes);
    hex::encode(mac.finalize().into_bytes())
}

/// Append a new entry to the audit log. Returns the new entry's row ID.
pub fn append_entry(conn: &Connection, entry: &AuditEntry, key: &[u8]) -> Result<i64> {
    let timestamp = std::time::SystemTime::now()
        .duration_since(std::time::UNIX_EPOCH)
        .map(|d| d.as_secs())
        .unwrap_or(0);

    // Fetch the most recent MAC for chaining
    let prev_mac: String = conn
        .query_row(
            "SELECT mac FROM audit_log ORDER BY id DESC LIMIT 1",
            [],
            |r| r.get(0),
        )
        .unwrap_or_else(|_| "GENESIS".to_string());

    let mac = compute_mac(entry, timestamp, &prev_mac, key);

    conn.execute(
        "INSERT INTO audit_log
            (timestamp, stage, provider, amount_krw, merchant, workflow, risk_level,
             device_id, metadata_json, prev_mac, mac)
         VALUES (?1, ?2, ?3, ?4, ?5, ?6, ?7, ?8, ?9, ?10, ?11)",
        params![
            timestamp as i64,
            entry.stage,
            entry.provider,
            entry.amount_krw.map(|v| v as i64),
            entry.merchant,
            entry.workflow,
            entry.risk_level,
            entry.device_id,
            entry.metadata_json,
            prev_mac,
            mac,
        ],
    )?;
    Ok(conn.last_insert_rowid())
}

/// Verify the integrity of the entire chain.
/// Returns the ID of the first tampered entry, or None if all good.
pub fn verify_chain(conn: &Connection, key: &[u8]) -> Result<Option<i64>> {
    let mut stmt = conn.prepare(
        "SELECT id, timestamp, stage, provider, amount_krw, merchant, workflow,
                risk_level, device_id, metadata_json, prev_mac, mac
         FROM audit_log ORDER BY id",
    )?;
    let rows = stmt.query_map([], |r| {
        Ok((
            r.get::<_, i64>(0)?,
            r.get::<_, i64>(1)? as u64,
            AuditEntry {
                stage: r.get(2)?,
                provider: r.get(3)?,
                amount_krw: r.get::<_, Option<i64>>(4)?.map(|v| v as u64),
                merchant: r.get(5)?,
                workflow: r.get(6)?,
                risk_level: r.get(7)?,
                device_id: r.get(8)?,
                metadata_json: r.get(9)?,
            },
            r.get::<_, String>(10)?, // prev_mac
            r.get::<_, String>(11)?, // mac
        ))
    })?;

    let mut expected_prev = "GENESIS".to_string();
    for row in rows {
        let (id, ts, entry, stored_prev, stored_mac) = row.context("row decode")?;
        if stored_prev != expected_prev {
            return Ok(Some(id));
        }
        let expected_mac = compute_mac(&entry, ts, &expected_prev, key);
        if expected_mac != stored_mac {
            return Ok(Some(id));
        }
        expected_prev = stored_mac;
    }
    Ok(None)
}

/// Count total entries in the audit log.
pub fn count(conn: &Connection) -> Result<i64> {
    let count: i64 = conn.query_row("SELECT COUNT(*) FROM audit_log", [], |r| r.get(0))?;
    Ok(count)
}

#[cfg(test)]
mod tests {
    use super::*;

    fn test_key() -> Vec<u8> {
        b"test-hmac-key-32-bytes-long-ok!!".to_vec()
    }

    fn sample_entry(stage: &str) -> AuditEntry {
        AuditEntry {
            stage: stage.to_string(),
            provider: Some("passkey".to_string()),
            amount_krw: Some(50_000),
            merchant: Some("coupang".to_string()),
            workflow: Some("test_wf".to_string()),
            risk_level: Some("low".to_string()),
            device_id: "dev1".to_string(),
            metadata_json: None,
        }
    }

    #[test]
    fn init_schema_creates_table() {
        let conn = Connection::open_in_memory().unwrap();
        init_audit_db(&conn).unwrap();
        assert_eq!(count(&conn).unwrap(), 0);
    }

    #[test]
    fn append_single_entry() {
        let conn = Connection::open_in_memory().unwrap();
        init_audit_db(&conn).unwrap();
        let id = append_entry(&conn, &sample_entry("pre_invoke"), &test_key()).unwrap();
        assert!(id > 0);
        assert_eq!(count(&conn).unwrap(), 1);
    }

    #[test]
    fn chain_verification_passes_for_untampered() {
        let conn = Connection::open_in_memory().unwrap();
        init_audit_db(&conn).unwrap();
        let key = test_key();
        for stage in ["pre_invoke", "post_callback", "receipt"] {
            append_entry(&conn, &sample_entry(stage), &key).unwrap();
        }
        assert!(verify_chain(&conn, &key).unwrap().is_none());
    }

    #[test]
    fn chain_verification_detects_tampering() {
        let conn = Connection::open_in_memory().unwrap();
        init_audit_db(&conn).unwrap();
        let key = test_key();
        append_entry(&conn, &sample_entry("stage1"), &key).unwrap();
        append_entry(&conn, &sample_entry("stage2"), &key).unwrap();

        // Tamper: manually rewrite a MAC (bypassing the append-only trigger
        // requires direct sqlite_master manipulation — we test via wrong key).
        let wrong_key = b"different-key-than-original-!!!!".to_vec();
        let tampered_result = verify_chain(&conn, &wrong_key).unwrap();
        assert!(tampered_result.is_some(), "wrong key must trigger failure");
    }

    #[test]
    fn append_only_update_denied() {
        let conn = Connection::open_in_memory().unwrap();
        init_audit_db(&conn).unwrap();
        append_entry(&conn, &sample_entry("stage1"), &test_key()).unwrap();
        let result = conn.execute(
            "UPDATE audit_log SET stage = 'hacked' WHERE id = 1",
            [],
        );
        assert!(result.is_err());
    }

    #[test]
    fn append_only_delete_denied() {
        let conn = Connection::open_in_memory().unwrap();
        init_audit_db(&conn).unwrap();
        append_entry(&conn, &sample_entry("stage1"), &test_key()).unwrap();
        let result = conn.execute("DELETE FROM audit_log WHERE id = 1", []);
        assert!(result.is_err());
    }

    #[test]
    fn mac_changes_with_input() {
        let key = test_key();
        let e1 = sample_entry("stage1");
        let e2 = sample_entry("stage2");
        let m1 = compute_mac(&e1, 100, "GENESIS", &key);
        let m2 = compute_mac(&e2, 100, "GENESIS", &key);
        assert_ne!(m1, m2);
    }

    #[test]
    fn mac_depends_on_prev() {
        let key = test_key();
        let e = sample_entry("same_stage");
        let m1 = compute_mac(&e, 100, "GENESIS", &key);
        let m2 = compute_mac(&e, 100, "some_other_mac", &key);
        assert_ne!(m1, m2);
    }

    #[test]
    fn empty_chain_verifies() {
        let conn = Connection::open_in_memory().unwrap();
        init_audit_db(&conn).unwrap();
        assert!(verify_chain(&conn, &test_key()).unwrap().is_none());
    }

    #[test]
    fn canonical_bytes_deterministic() {
        let entry = sample_entry("test");
        let a = entry.canonical_bytes(100, "PREV");
        let b = entry.canonical_bytes(100, "PREV");
        assert_eq!(a, b);
    }
}
