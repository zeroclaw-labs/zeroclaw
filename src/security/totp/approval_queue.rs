// Approval queue for autonomous operations (D24, F20).
//
// When the agent wants to perform a destructive operation during
// autonomous context (Cron/SelfHeal), the command is queued as a
// proposal. The user approves or rejects on next login.
// Each proposal is HMAC-signed to prevent manipulation (F20).

use chrono::{Duration, Utc};
use hmac::{Hmac, Mac};
use rusqlite::{params, Connection};
use sha2::Sha256;

type HmacSha256 = Hmac<Sha256>;

pub struct ApprovalQueue {
    conn: Connection,
}

#[derive(Debug, Clone, serde::Serialize, serde::Deserialize)]
pub struct Proposal {
    pub id: i64,
    pub created_at: String,
    pub actor: String,
    pub command: String,
    pub reason: Option<String>,
    pub urgency: String,
    pub status: String,
    pub resolved_at: Option<String>,
    pub resolved_by: Option<String>,
    pub hmac: String,
    pub detail: Option<String>,
}

impl ApprovalQueue {
    pub fn open(path: &str) -> anyhow::Result<Self> {
        let conn = Connection::open(path)?;
        conn.execute_batch("PRAGMA journal_mode=WAL;")?;
        conn.execute_batch(
            "CREATE TABLE IF NOT EXISTS pending_approvals (
                id          INTEGER PRIMARY KEY AUTOINCREMENT,
                created_at  TEXT    NOT NULL,
                actor       TEXT    NOT NULL,
                command     TEXT    NOT NULL,
                reason      TEXT,
                urgency     TEXT    NOT NULL,
                status      TEXT    NOT NULL DEFAULT 'pending',
                resolved_at TEXT,
                resolved_by TEXT,
                hmac        TEXT    NOT NULL,
                detail      TEXT
            );",
        )?;
        Ok(Self { conn })
    }

    pub fn open_in_memory() -> anyhow::Result<Self> {
        let conn = Connection::open_in_memory()?;
        conn.execute_batch(
            "CREATE TABLE pending_approvals (
                id          INTEGER PRIMARY KEY AUTOINCREMENT,
                created_at  TEXT    NOT NULL,
                actor       TEXT    NOT NULL,
                command     TEXT    NOT NULL,
                reason      TEXT,
                urgency     TEXT    NOT NULL,
                status      TEXT    NOT NULL DEFAULT 'pending',
                resolved_at TEXT,
                resolved_by TEXT,
                hmac        TEXT    NOT NULL,
                detail      TEXT
            );",
        )?;
        Ok(Self { conn })
    }

    /// Queue a new proposal. Returns the proposal ID.
    pub fn queue(
        &self,
        actor: &str,
        command: &str,
        reason: Option<&str>,
        urgency: &str,
        signing_key: &[u8],
        detail: Option<&str>,
    ) -> anyhow::Result<i64> {
        let created_at = Utc::now().to_rfc3339();
        let hmac = compute_proposal_hmac(command, &created_at, actor, signing_key);

        self.conn.execute(
            "INSERT INTO pending_approvals (created_at, actor, command, reason, urgency, hmac, detail)
             VALUES (?1, ?2, ?3, ?4, ?5, ?6, ?7)",
            params![created_at, actor, command, reason, urgency, hmac, detail],
        )?;
        Ok(self.conn.last_insert_rowid())
    }

    /// Approve a proposal. Verifies HMAC to ensure it was not modified (F20).
    pub fn approve(
        &self,
        proposal_id: i64,
        approved_by: &str,
        signing_key: &[u8],
    ) -> anyhow::Result<ApprovalResult> {
        let proposal = self.get(proposal_id)?;
        let Some(proposal) = proposal else {
            return Ok(ApprovalResult::NotFound);
        };

        if proposal.status != "pending" {
            return Ok(ApprovalResult::AlreadyResolved);
        }

        // Verify HMAC (F20 — detect manipulation)
        let expected_hmac = compute_proposal_hmac(
            &proposal.command,
            &proposal.created_at,
            &proposal.actor,
            signing_key,
        );
        if expected_hmac != proposal.hmac {
            return Ok(ApprovalResult::TamperedProposal);
        }

        let now = Utc::now().to_rfc3339();
        self.conn.execute(
            "UPDATE pending_approvals SET status='approved', resolved_at=?1, resolved_by=?2 WHERE id=?3",
            params![now, approved_by, proposal_id],
        )?;

        Ok(ApprovalResult::Approved {
            command: proposal.command,
        })
    }

    /// Reject a proposal.
    pub fn reject(
        &self,
        proposal_id: i64,
        rejected_by: &str,
    ) -> anyhow::Result<bool> {
        let now = Utc::now().to_rfc3339();
        let rows = self.conn.execute(
            "UPDATE pending_approvals SET status='rejected', resolved_at=?1, resolved_by=?2 WHERE id=?3 AND status='pending'",
            params![now, rejected_by, proposal_id],
        )?;
        Ok(rows > 0)
    }

    /// Expire old proposals.
    pub fn expire_old(&self, max_age_hours: u64) -> anyhow::Result<usize> {
        let cutoff = (Utc::now() - Duration::hours(max_age_hours as i64)).to_rfc3339();
        let rows = self.conn.execute(
            "UPDATE pending_approvals SET status='expired' WHERE status='pending' AND created_at < ?1",
            params![cutoff],
        )?;
        Ok(rows)
    }

    /// List pending proposals.
    pub fn list_pending(&self) -> anyhow::Result<Vec<Proposal>> {
        let mut stmt = self.conn.prepare(
            "SELECT id, created_at, actor, command, reason, urgency, status, resolved_at, resolved_by, hmac, detail
             FROM pending_approvals WHERE status='pending' ORDER BY created_at ASC",
        )?;
        let rows = stmt.query_map([], |row| {
            Ok(Proposal {
                id: row.get(0)?,
                created_at: row.get(1)?,
                actor: row.get(2)?,
                command: row.get(3)?,
                reason: row.get(4)?,
                urgency: row.get(5)?,
                status: row.get(6)?,
                resolved_at: row.get(7)?,
                resolved_by: row.get(8)?,
                hmac: row.get(9)?,
                detail: row.get(10)?,
            })
        })?;
        rows.collect::<Result<Vec<_>, _>>().map_err(Into::into)
    }

    /// Get a single proposal by ID.
    pub fn get(&self, id: i64) -> anyhow::Result<Option<Proposal>> {
        let result = self.conn.query_row(
            "SELECT id, created_at, actor, command, reason, urgency, status, resolved_at, resolved_by, hmac, detail
             FROM pending_approvals WHERE id=?1",
            params![id],
            |row| {
                Ok(Proposal {
                    id: row.get(0)?,
                    created_at: row.get(1)?,
                    actor: row.get(2)?,
                    command: row.get(3)?,
                    reason: row.get(4)?,
                    urgency: row.get(5)?,
                    status: row.get(6)?,
                    resolved_at: row.get(7)?,
                    resolved_by: row.get(8)?,
                    hmac: row.get(9)?,
                    detail: row.get(10)?,
                })
            },
        );
        match result {
            Ok(p) => Ok(Some(p)),
            Err(rusqlite::Error::QueryReturnedNoRows) => Ok(None),
            Err(e) => Err(e.into()),
        }
    }
}

#[derive(Debug)]
pub enum ApprovalResult {
    Approved { command: String },
    NotFound,
    AlreadyResolved,
    TamperedProposal,
}

fn compute_proposal_hmac(command: &str, created_at: &str, actor: &str, key: &[u8]) -> String {
    let mut mac = HmacSha256::new_from_slice(key).expect("HMAC accepts any key length");
    mac.update(command.as_bytes());
    mac.update(b"|");
    mac.update(created_at.as_bytes());
    mac.update(b"|");
    mac.update(actor.as_bytes());
    let result = mac.finalize().into_bytes();
    result.iter().map(|b| format!("{:02x}", b)).collect()
}

#[cfg(test)]
mod tests {
    use super::*;

    const TEST_KEY: &[u8] = b"test-signing-key-for-proposals!!";

    #[test]
    fn queue_and_approve() {
        let q = ApprovalQueue::open_in_memory().unwrap();
        let id = q
            .queue("cron/backup", "db.delete old_rows", Some("Cleanup"), "low", TEST_KEY, None)
            .unwrap();
        assert!(id > 0);

        let pending = q.list_pending().unwrap();
        assert_eq!(pending.len(), 1);

        let result = q.approve(id, "admin", TEST_KEY).unwrap();
        assert!(matches!(result, ApprovalResult::Approved { .. }));

        let pending = q.list_pending().unwrap();
        assert_eq!(pending.len(), 0);
    }

    #[test]
    fn tampered_proposal_detected() {
        let q = ApprovalQueue::open_in_memory().unwrap();
        let id = q
            .queue("cron/job", "db.delete safe_rows", Some("test"), "low", TEST_KEY, None)
            .unwrap();

        // Tamper with the command directly in the DB
        q.conn
            .execute(
                "UPDATE pending_approvals SET command='db.drop everything' WHERE id=?1",
                params![id],
            )
            .unwrap();

        let result = q.approve(id, "admin", TEST_KEY).unwrap();
        assert!(matches!(result, ApprovalResult::TamperedProposal));
    }

    #[test]
    fn reject_proposal() {
        let q = ApprovalQueue::open_in_memory().unwrap();
        let id = q
            .queue("cron/job", "db.delete", None, "low", TEST_KEY, None)
            .unwrap();
        assert!(q.reject(id, "admin").unwrap());

        let p = q.get(id).unwrap().unwrap();
        assert_eq!(p.status, "rejected");
    }

    #[test]
    fn double_approve_fails() {
        let q = ApprovalQueue::open_in_memory().unwrap();
        let id = q
            .queue("cron/job", "db.delete", None, "low", TEST_KEY, None)
            .unwrap();

        let r1 = q.approve(id, "admin", TEST_KEY).unwrap();
        assert!(matches!(r1, ApprovalResult::Approved { .. }));

        let r2 = q.approve(id, "admin", TEST_KEY).unwrap();
        assert!(matches!(r2, ApprovalResult::AlreadyResolved));
    }

    #[test]
    fn not_found_proposal() {
        let q = ApprovalQueue::open_in_memory().unwrap();
        let result = q.approve(999, "admin", TEST_KEY).unwrap();
        assert!(matches!(result, ApprovalResult::NotFound));
    }
}
