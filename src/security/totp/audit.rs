// SQLite audit log with hash chain for tamper evidence (F3, D7, D8).
//
// Append-only: triggers block UPDATE and DELETE.
// Hash chain: each row includes SHA-256 of the previous row.
// Export: JSONL format for compliance reporting.

use chrono::Utc;
use rusqlite::{params, Connection};
use sha2::{Digest, Sha256};

use super::types::{AuditAction, ExecutionContext};

const GENESIS_HASH: &str = "0000000000000000000000000000000000000000000000000000000000000000";

pub struct AuditLog {
    conn: Connection,
}

#[derive(Debug, Clone, serde::Serialize)]
pub struct AuditEntry {
    pub id: i64,
    pub timestamp: String,
    pub user_id: String,
    pub action: String,
    pub command: Option<String>,
    pub decision: Option<String>,
    pub context: Option<String>,
    pub severity: String,
    pub detail: Option<String>,
    pub prev_hash: String,
    pub row_hash: String,
}

impl AuditLog {
    /// Open (or create) the audit log database.
    pub fn open(path: &str) -> anyhow::Result<Self> {
        let conn = Connection::open(path)?;
        conn.execute_batch("PRAGMA journal_mode=WAL; PRAGMA synchronous=NORMAL;")?;

        conn.execute_batch(
            "CREATE TABLE IF NOT EXISTS totp_audit (
                id          INTEGER PRIMARY KEY AUTOINCREMENT,
                timestamp   TEXT    NOT NULL,
                user_id     TEXT    NOT NULL,
                action      TEXT    NOT NULL,
                command     TEXT,
                decision    TEXT,
                context     TEXT,
                severity    TEXT    NOT NULL,
                detail      TEXT,
                prev_hash   TEXT    NOT NULL,
                row_hash    TEXT    NOT NULL
            );

            CREATE TRIGGER IF NOT EXISTS totp_audit_no_update
                BEFORE UPDATE ON totp_audit
                BEGIN SELECT RAISE(ABORT, 'totp_audit is append-only'); END;

            CREATE TRIGGER IF NOT EXISTS totp_audit_no_delete
                BEFORE DELETE ON totp_audit
                BEGIN SELECT RAISE(ABORT, 'totp_audit is append-only'); END;",
        )?;

        Ok(Self { conn })
    }

    /// Open an in-memory database (for testing).
    pub fn open_in_memory() -> anyhow::Result<Self> {
        let conn = Connection::open_in_memory()?;
        conn.execute_batch(
            "CREATE TABLE totp_audit (
                id          INTEGER PRIMARY KEY AUTOINCREMENT,
                timestamp   TEXT    NOT NULL,
                user_id     TEXT    NOT NULL,
                action      TEXT    NOT NULL,
                command     TEXT,
                decision    TEXT,
                context     TEXT,
                severity    TEXT    NOT NULL,
                detail      TEXT,
                prev_hash   TEXT    NOT NULL,
                row_hash    TEXT    NOT NULL
            );",
        )?;
        Ok(Self { conn })
    }

    /// Append an audit entry with hash chain.
    pub fn log(
        &self,
        user_id: &str,
        action: AuditAction,
        command: Option<&str>,
        decision: Option<&str>,
        context: Option<&ExecutionContext>,
        detail: Option<&str>,
    ) -> anyhow::Result<i64> {
        let timestamp = Utc::now().to_rfc3339();
        let action_str = format!("{:?}", action).to_lowercase();
        let severity = format!("{:?}", action.severity()).to_lowercase();
        let context_str = context.map(|c| format!("{:?}", c).to_lowercase());

        // Get previous hash
        let prev_hash: String = self
            .conn
            .query_row(
                "SELECT row_hash FROM totp_audit ORDER BY id DESC LIMIT 1",
                [],
                |row| row.get(0),
            )
            .unwrap_or_else(|_| GENESIS_HASH.to_string());

        // Compute this row's hash
        let row_hash = compute_row_hash(
            &timestamp,
            user_id,
            &action_str,
            command,
            decision,
            detail,
            &prev_hash,
        );

        self.conn.execute(
            "INSERT INTO totp_audit (timestamp, user_id, action, command, decision, context, severity, detail, prev_hash, row_hash)
             VALUES (?1, ?2, ?3, ?4, ?5, ?6, ?7, ?8, ?9, ?10)",
            params![
                timestamp,
                user_id,
                action_str,
                command,
                decision,
                context_str,
                severity,
                detail,
                prev_hash,
                row_hash,
            ],
        )?;

        Ok(self.conn.last_insert_rowid())
    }

    /// Verify the integrity of the hash chain.
    /// Returns (valid, total_rows, first_broken_row).
    pub fn verify_chain(&self) -> anyhow::Result<(bool, usize, Option<i64>)> {
        let mut stmt = self.conn.prepare(
            "SELECT id, timestamp, user_id, action, command, decision, detail, prev_hash, row_hash
             FROM totp_audit ORDER BY id ASC",
        )?;

        let rows = stmt.query_map([], |row| {
            Ok((
                row.get::<_, i64>(0)?,
                row.get::<_, String>(1)?,
                row.get::<_, String>(2)?,
                row.get::<_, String>(3)?,
                row.get::<_, Option<String>>(4)?,
                row.get::<_, Option<String>>(5)?,
                row.get::<_, Option<String>>(6)?,
                row.get::<_, String>(7)?,
                row.get::<_, String>(8)?,
            ))
        })?;

        let mut expected_prev = GENESIS_HASH.to_string();
        let mut total = 0usize;

        for row in rows {
            let (id, ts, uid, action, cmd, dec, detail, prev_hash, row_hash) = row?;
            total += 1;

            // Check prev_hash links to the previous row's row_hash
            if prev_hash != expected_prev {
                return Ok((false, total, Some(id)));
            }

            // Recompute and verify the row hash
            let computed = compute_row_hash(
                &ts,
                &uid,
                &action,
                cmd.as_deref(),
                dec.as_deref(),
                detail.as_deref(),
                &prev_hash,
            );
            if computed != row_hash {
                return Ok((false, total, Some(id)));
            }

            expected_prev = row_hash;
        }

        Ok((true, total, None))
    }

    /// Export entries as JSONL (one JSON object per line).
    pub fn export_jsonl(
        &self,
        from: Option<&str>,
        to: Option<&str>,
    ) -> anyhow::Result<Vec<String>> {
        let mut query = "SELECT id, timestamp, user_id, action, command, decision, context, severity, detail, prev_hash, row_hash FROM totp_audit WHERE 1=1".to_string();
        let mut bind_values: Vec<String> = Vec::new();

        if let Some(from) = from {
            query.push_str(" AND timestamp >= ?");
            bind_values.push(from.to_string());
        }
        if let Some(to) = to {
            query.push_str(" AND timestamp <= ?");
            bind_values.push(to.to_string());
        }
        query.push_str(" ORDER BY id ASC");

        let mut stmt = self.conn.prepare(&query)?;
        let mut lines = Vec::new();

        let mut rows = match bind_values.len() {
            0 => stmt.query([])?,
            1 => stmt.query(params![bind_values[0]])?,
            _ => stmt.query(params![bind_values[0], bind_values[1]])?,
        };

        while let Some(row) = rows.next()? {
            let entry: AuditEntry = Self::row_to_entry(row)?;
            lines.push(serde_json::to_string(&entry)?);
        }

        Ok(lines)
    }

    /// Count total entries.
    pub fn count(&self) -> anyhow::Result<usize> {
        let count: i64 = self
            .conn
            .query_row("SELECT COUNT(*) FROM totp_audit", [], |row| row.get(0))?;
        Ok(count as usize)
    }

    fn row_to_entry(row: &rusqlite::Row<'_>) -> rusqlite::Result<AuditEntry> {
        Ok(AuditEntry {
            id: row.get(0)?,
            timestamp: row.get(1)?,
            user_id: row.get(2)?,
            action: row.get(3)?,
            command: row.get(4)?,
            decision: row.get(5)?,
            context: row.get(6)?,
            severity: row.get(7)?,
            detail: row.get(8)?,
            prev_hash: row.get(9)?,
            row_hash: row.get(10)?,
        })
    }
}

fn compute_row_hash(
    timestamp: &str,
    user_id: &str,
    action: &str,
    command: Option<&str>,
    decision: Option<&str>,
    detail: Option<&str>,
    prev_hash: &str,
) -> String {
    let mut hasher = Sha256::new();
    hasher.update(timestamp.as_bytes());
    hasher.update(b"|");
    hasher.update(user_id.as_bytes());
    hasher.update(b"|");
    hasher.update(action.as_bytes());
    hasher.update(b"|");
    hasher.update(command.unwrap_or("").as_bytes());
    hasher.update(b"|");
    hasher.update(decision.unwrap_or("").as_bytes());
    hasher.update(b"|");
    hasher.update(detail.unwrap_or("").as_bytes());
    hasher.update(b"|");
    hasher.update(prev_hash.as_bytes());

    let result = hasher.finalize();
    result.iter().map(|b| format!("{:02x}", b)).collect()
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn hash_chain_integrity() {
        let log = AuditLog::open_in_memory().unwrap();
        log.log("alice", AuditAction::Setup, None, None, None, None).unwrap();
        log.log("alice", AuditAction::VerifyOk, Some("ls"), Some("allowed"), None, None).unwrap();
        log.log("bob", AuditAction::VerifyFail, Some("rm -rf"), Some("totp_required"), None, None).unwrap();

        let (valid, total, broken) = log.verify_chain().unwrap();
        assert!(valid);
        assert_eq!(total, 3);
        assert!(broken.is_none());
    }

    #[test]
    fn export_produces_jsonl() {
        let log = AuditLog::open_in_memory().unwrap();
        log.log("alice", AuditAction::Setup, None, None, None, None).unwrap();
        log.log("alice", AuditAction::VerifyOk, Some("ls"), Some("allowed"), None, None).unwrap();

        let lines = log.export_jsonl(None, None).unwrap();
        assert_eq!(lines.len(), 2);
        for line in &lines {
            let _: serde_json::Value = serde_json::from_str(line).unwrap();
        }
    }

    #[test]
    fn count_entries() {
        let log = AuditLog::open_in_memory().unwrap();
        assert_eq!(log.count().unwrap(), 0);
        log.log("alice", AuditAction::Setup, None, None, None, None).unwrap();
        assert_eq!(log.count().unwrap(), 1);
    }

    #[test]
    fn severity_classification_logged() {
        let log = AuditLog::open_in_memory().unwrap();
        log.log("admin", AuditAction::BreakGlass, None, None, None, Some("{\"reason\":\"locked out\"}")).unwrap();

        let lines = log.export_jsonl(None, None).unwrap();
        assert!(lines[0].contains("\"critical\""));
    }
}
