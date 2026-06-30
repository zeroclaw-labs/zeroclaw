//! Durable SQLite-backed [`SopRunStore`] (EPIC B).
//!
//! WAL-mode SQLite via the workspace `rusqlite` dep (already used by
//! `zeroclaw-memory`/`zeroclaw-runtime`). One `Arc<Mutex<Connection>>` serializes
//! access, which makes the CAS-claim admission and revision guards atomic without
//! explicit transactions. Tables:
//!
//! - `sop_runs(run_id PK, revision, terminal, last_progress_at, json)` - upsert-by-revision
//! - `sop_events(seq PK AUTOINCREMENT, run_id, ts, kind, actor, reason, payload)` - append-only
//! - `sop_claims(run_id PK, sop_name, lease_expires, json)` - single-winner admission
//! - `sop_proposals(id PK, status, json)` - procedural-memory namespace
//!
//! Selected by `build_run_store()` when `[sop] persist_runs = true` with backend
//! `"sqlite"`; `build_sop_engine` then injects it and calls `restore_runs()` at
//! startup. The in-memory backend remains the default when persistence is off.

use std::path::Path;
use std::sync::{Arc, Mutex};

use chrono::{Duration, Utc};
use rusqlite::{Connection, OptionalExtension, params};

use super::model::{
    ClaimToken, PersistedRun, ProposalRecord, ProposalStatus, RetentionPolicy, SopEventRecord,
};
use super::{SopRunStore, StoreError};

/// Default claim lease. The concurrency tick (EPIC A1) renews via `heartbeat_claim`;
/// the reaper reclaims claims past this without a heartbeat.
const DEFAULT_CLAIM_LEASE_SECS: i64 = 3600;

const SCHEMA: &str = "
PRAGMA journal_mode = WAL;
PRAGMA synchronous = NORMAL;
PRAGMA busy_timeout = 5000;
CREATE TABLE IF NOT EXISTS sop_runs (
    run_id           TEXT PRIMARY KEY,
    revision         INTEGER NOT NULL,
    terminal         INTEGER NOT NULL DEFAULT 0,
    last_progress_at TEXT,
    json             TEXT NOT NULL
);
CREATE INDEX IF NOT EXISTS idx_sop_runs_terminal ON sop_runs(terminal);
CREATE TABLE IF NOT EXISTS sop_events (
    seq     INTEGER PRIMARY KEY AUTOINCREMENT,
    run_id  TEXT NOT NULL,
    ts      TEXT NOT NULL,
    kind    TEXT NOT NULL,
    actor   TEXT,
    reason  TEXT,
    payload TEXT NOT NULL
);
CREATE INDEX IF NOT EXISTS idx_sop_events_run ON sop_events(run_id);
CREATE TABLE IF NOT EXISTS sop_claims (
    run_id        TEXT PRIMARY KEY,
    sop_name      TEXT NOT NULL,
    lease_expires TEXT NOT NULL,
    json          TEXT NOT NULL
);
CREATE INDEX IF NOT EXISTS idx_sop_claims_sop ON sop_claims(sop_name);
CREATE TABLE IF NOT EXISTS sop_proposals (
    id     TEXT PRIMARY KEY,
    status TEXT NOT NULL,
    json   TEXT NOT NULL
);
";

fn sql_err(e: rusqlite::Error) -> StoreError {
    StoreError::Backend(format!("sqlite: {e}"))
}

fn status_str(s: ProposalStatus) -> &'static str {
    match s {
        ProposalStatus::Pending => "pending",
        ProposalStatus::Applied => "applied",
        ProposalStatus::Rejected => "rejected",
        ProposalStatus::Quarantined => "quarantined",
        ProposalStatus::Stale => "stale",
    }
}

/// Revision guard for the durable backend, mirroring the in-memory
/// `revision_guard`: a strictly-older revision is `StaleRevision`, a divergent
/// same-revision payload is `RevisionConflict`, and a byte-identical same-revision
/// retry is accepted. The stored and incoming envelopes are compared as their
/// serialized JSON. Called while the connection lock is held, so the read + write
/// it precedes is atomic within the process.
fn guard_revision(
    run_id: &str,
    stored_rev: u64,
    stored_json: &str,
    incoming_rev: u64,
    incoming_json: &str,
) -> Result<(), StoreError> {
    if incoming_rev < stored_rev {
        return Err(StoreError::StaleRevision {
            run_id: run_id.to_string(),
            have: incoming_rev,
            found: stored_rev,
        });
    }
    if incoming_rev == stored_rev && stored_json != incoming_json {
        return Err(StoreError::RevisionConflict {
            run_id: run_id.to_string(),
            revision: incoming_rev,
        });
    }
    Ok(())
}

/// Durable run store. Selected by `build_run_store` when `persist_runs = true`
/// with the default `"sqlite"` backend.
pub struct SqliteRunStore {
    conn: Arc<Mutex<Connection>>,
}

impl SqliteRunStore {
    /// Open (creating if absent) a database at `path`.
    pub fn open(path: &Path) -> Result<Self, StoreError> {
        let conn = Connection::open(path).map_err(sql_err)?;
        Self::init(conn)
    }

    /// In-memory database (tests + the no-durability fallback).
    pub fn open_in_memory() -> Result<Self, StoreError> {
        let conn = Connection::open_in_memory().map_err(sql_err)?;
        Self::init(conn)
    }

    fn init(conn: Connection) -> Result<Self, StoreError> {
        conn.execute_batch(SCHEMA).map_err(sql_err)?;
        Ok(Self {
            conn: Arc::new(Mutex::new(conn)),
        })
    }

    fn lock(&self) -> Result<std::sync::MutexGuard<'_, Connection>, StoreError> {
        self.conn
            .lock()
            .map_err(|_| StoreError::Backend("sqlite run store lock poisoned".into()))
    }
}

impl SopRunStore for SqliteRunStore {
    fn save_run(&self, run: &PersistedRun) -> Result<(), StoreError> {
        let g = self.lock()?;
        let id = run.run_id();
        let json = serde_json::to_string(run)?;
        let existing: Option<(i64, String)> = g
            .query_row(
                "SELECT revision, json FROM sop_runs WHERE run_id=?1",
                params![id],
                |r| Ok((r.get(0)?, r.get(1)?)),
            )
            .optional()
            .map_err(sql_err)?;
        if let Some((rev, existing_json)) = existing {
            guard_revision(id, rev as u64, &existing_json, run.revision, &json)?;
        }
        g.execute(
            "INSERT INTO sop_runs (run_id, revision, terminal, last_progress_at, json)
             VALUES (?1, ?2, 0, ?3, ?4)
             ON CONFLICT(run_id) DO UPDATE SET
                 revision=excluded.revision,
                 terminal=excluded.terminal,
                 last_progress_at=excluded.last_progress_at,
                 json=excluded.json",
            params![id, run.revision as i64, run.last_progress_at, json],
        )
        .map_err(sql_err)?;
        Ok(())
    }

    fn finish_run(&self, run_id: &str, terminal: &PersistedRun) -> Result<(), StoreError> {
        let g = self.lock()?;
        let json = serde_json::to_string(terminal)?;
        let existing: Option<(i64, String)> = g
            .query_row(
                "SELECT revision, json FROM sop_runs WHERE run_id=?1",
                params![run_id],
                |r| Ok((r.get(0)?, r.get(1)?)),
            )
            .optional()
            .map_err(sql_err)?;
        if let Some((rev, existing_json)) = existing {
            guard_revision(run_id, rev as u64, &existing_json, terminal.revision, &json)?;
        }
        g.execute(
            "INSERT INTO sop_runs (run_id, revision, terminal, last_progress_at, json)
             VALUES (?1, ?2, 1, ?3, ?4)
             ON CONFLICT(run_id) DO UPDATE SET
                 revision=excluded.revision,
                 terminal=1,
                 last_progress_at=excluded.last_progress_at,
                 json=excluded.json",
            params![
                run_id,
                terminal.revision as i64,
                terminal.last_progress_at,
                json
            ],
        )
        .map_err(sql_err)?;
        g.execute("DELETE FROM sop_claims WHERE run_id=?1", params![run_id])
            .map_err(sql_err)?;
        Ok(())
    }

    fn load_active_runs(&self) -> Result<Vec<PersistedRun>, StoreError> {
        let g = self.lock()?;
        let mut stmt = g
            .prepare("SELECT json FROM sop_runs WHERE terminal=0")
            .map_err(sql_err)?;
        let rows = stmt
            .query_map([], |r| r.get::<_, String>(0))
            .map_err(sql_err)?;
        let mut out = Vec::new();
        for row in rows {
            out.push(serde_json::from_str(&row.map_err(sql_err)?)?);
        }
        Ok(out)
    }

    fn load_run(&self, run_id: &str) -> Result<Option<PersistedRun>, StoreError> {
        let g = self.lock()?;
        let json: Option<String> = g
            .query_row(
                "SELECT json FROM sop_runs WHERE run_id=?1",
                params![run_id],
                |r| r.get(0),
            )
            .optional()
            .map_err(sql_err)?;
        match json {
            Some(j) => Ok(Some(serde_json::from_str(&j)?)),
            None => Ok(None),
        }
    }

    fn try_claim_run(
        &self,
        run_id: &str,
        sop_name: &str,
        per_sop_cap: usize,
        global_cap: usize,
    ) -> Result<Option<ClaimToken>, StoreError> {
        let g = self.lock()?;
        // A terminal run is not re-claimable.
        let terminal: Option<i64> = g
            .query_row(
                "SELECT terminal FROM sop_runs WHERE run_id=?1",
                params![run_id],
                |r| r.get(0),
            )
            .optional()
            .map_err(sql_err)?;
        if terminal == Some(1) {
            return Ok(None);
        }
        // Already claimed?
        let claimed: Option<i64> = g
            .query_row(
                "SELECT 1 FROM sop_claims WHERE run_id=?1",
                params![run_id],
                |r| r.get(0),
            )
            .optional()
            .map_err(sql_err)?;
        if claimed.is_some() {
            return Ok(None);
        }
        // Both caps are enforced while the connection lock is held, so the
        // read-counts + insert are atomic within the process (mirrors the engine
        // `can_start`). A cap of 0 admits nothing.
        let per_sop: i64 = g
            .query_row(
                "SELECT COUNT(*) FROM sop_claims WHERE sop_name=?1",
                params![sop_name],
                |r| r.get(0),
            )
            .map_err(sql_err)?;
        if per_sop as usize >= per_sop_cap {
            return Ok(None);
        }
        let total: i64 = g
            .query_row("SELECT COUNT(*) FROM sop_claims", [], |r| r.get(0))
            .map_err(sql_err)?;
        if total as usize >= global_cap {
            return Ok(None);
        }
        let now = Utc::now();
        let token = ClaimToken {
            run_id: run_id.to_string(),
            sop_name: sop_name.to_string(),
            claimed_at: now.to_rfc3339(),
            lease_expires: (now + Duration::seconds(DEFAULT_CLAIM_LEASE_SECS)).to_rfc3339(),
            holder: format!("pid-{}", std::process::id()),
        };
        let json = serde_json::to_string(&token)?;
        g.execute(
            "INSERT INTO sop_claims (run_id, sop_name, lease_expires, json) VALUES (?1, ?2, ?3, ?4)",
            params![token.run_id, token.sop_name, token.lease_expires, json],
        )
        .map_err(sql_err)?;
        Ok(Some(token))
    }

    fn heartbeat_claim(&self, token: &ClaimToken) -> Result<(), StoreError> {
        let g = self.lock()?;
        let lease = (Utc::now() + Duration::seconds(DEFAULT_CLAIM_LEASE_SECS)).to_rfc3339();
        g.execute(
            "UPDATE sop_claims SET lease_expires=?1 WHERE run_id=?2",
            params![lease, token.run_id],
        )
        .map_err(sql_err)?;
        Ok(())
    }

    fn release_claim(&self, token: &ClaimToken) -> Result<(), StoreError> {
        self.lock()?
            .execute(
                "DELETE FROM sop_claims WHERE run_id=?1",
                params![token.run_id],
            )
            .map_err(sql_err)?;
        Ok(())
    }

    fn expired_claims(&self, now_iso: &str) -> Result<Vec<ClaimToken>, StoreError> {
        let g = self.lock()?;
        let mut stmt = g
            .prepare("SELECT json FROM sop_claims WHERE lease_expires <= ?1")
            .map_err(sql_err)?;
        let rows = stmt
            .query_map(params![now_iso], |r| r.get::<_, String>(0))
            .map_err(sql_err)?;
        let mut out = Vec::new();
        for row in rows {
            out.push(serde_json::from_str(&row.map_err(sql_err)?)?);
        }
        Ok(out)
    }

    fn append_event(&self, ev: &SopEventRecord) -> Result<u64, StoreError> {
        let g = self.lock()?;
        let payload = serde_json::to_string(&ev.payload)?;
        g.execute(
            "INSERT INTO sop_events (run_id, ts, kind, actor, reason, payload)
             VALUES (?1, ?2, ?3, ?4, ?5, ?6)",
            params![ev.run_id, ev.ts, ev.kind, ev.actor, ev.reason, payload],
        )
        .map_err(sql_err)?;
        Ok(g.last_insert_rowid() as u64)
    }

    fn list_events(&self, run_id: &str) -> Result<Vec<SopEventRecord>, StoreError> {
        let g = self.lock()?;
        let mut stmt = g
            .prepare(
                "SELECT seq, ts, kind, actor, reason, payload FROM sop_events
                 WHERE run_id=?1 ORDER BY seq",
            )
            .map_err(sql_err)?;
        let rows = stmt
            .query_map(params![run_id], |r| {
                Ok((
                    r.get::<_, i64>(0)?,
                    r.get::<_, String>(1)?,
                    r.get::<_, String>(2)?,
                    r.get::<_, Option<String>>(3)?,
                    r.get::<_, Option<String>>(4)?,
                    r.get::<_, String>(5)?,
                ))
            })
            .map_err(sql_err)?;
        let mut out = Vec::new();
        for row in rows {
            let (seq, ts, kind, actor, reason, payload_s) = row.map_err(sql_err)?;
            out.push(SopEventRecord {
                run_id: run_id.to_string(),
                seq: seq as u64,
                ts,
                kind,
                actor,
                reason,
                payload: serde_json::from_str(&payload_s)?,
            });
        }
        Ok(out)
    }

    fn save_proposal(&self, p: &ProposalRecord) -> Result<(), StoreError> {
        let g = self.lock()?;
        let json = serde_json::to_string(p)?;
        g.execute(
            "INSERT INTO sop_proposals (id, status, json) VALUES (?1, ?2, ?3)
             ON CONFLICT(id) DO UPDATE SET status=excluded.status, json=excluded.json",
            params![p.id, status_str(p.status), json],
        )
        .map_err(sql_err)?;
        Ok(())
    }

    fn load_proposal(&self, id: &str) -> Result<Option<ProposalRecord>, StoreError> {
        let g = self.lock()?;
        let json: Option<String> = g
            .query_row(
                "SELECT json FROM sop_proposals WHERE id=?1",
                params![id],
                |r| r.get(0),
            )
            .optional()
            .map_err(sql_err)?;
        match json {
            Some(j) => Ok(Some(serde_json::from_str(&j)?)),
            None => Ok(None),
        }
    }

    fn list_proposals(
        &self,
        status: Option<ProposalStatus>,
    ) -> Result<Vec<ProposalRecord>, StoreError> {
        let g = self.lock()?;
        let (sql, bind): (&str, Option<&'static str>) = match status {
            Some(s) => (
                "SELECT json FROM sop_proposals WHERE status=?1",
                Some(status_str(s)),
            ),
            None => ("SELECT json FROM sop_proposals", None),
        };
        let mut stmt = g.prepare(sql).map_err(sql_err)?;
        let mut out = Vec::new();
        if let Some(s) = bind {
            let rows = stmt
                .query_map(params![s], |r| r.get::<_, String>(0))
                .map_err(sql_err)?;
            for row in rows {
                out.push(serde_json::from_str(&row.map_err(sql_err)?)?);
            }
        } else {
            let rows = stmt
                .query_map([], |r| r.get::<_, String>(0))
                .map_err(sql_err)?;
            for row in rows {
                out.push(serde_json::from_str(&row.map_err(sql_err)?)?);
            }
        }
        Ok(out)
    }

    fn prune(&self, policy: &RetentionPolicy) -> Result<usize, StoreError> {
        let g = self.lock()?;
        let mut dropped = 0usize;
        if policy.max_terminal > 0 {
            let total: i64 = g
                .query_row("SELECT COUNT(*) FROM sop_runs WHERE terminal=1", [], |r| {
                    r.get(0)
                })
                .map_err(sql_err)?;
            if total as usize > policy.max_terminal {
                let drop_n = (total as usize - policy.max_terminal) as i64;
                // Oldest terminal runs first (by last_progress_at). Events before runs.
                g.execute(
                    "DELETE FROM sop_events WHERE run_id IN
                       (SELECT run_id FROM sop_runs WHERE terminal=1
                        ORDER BY last_progress_at ASC LIMIT ?1)",
                    params![drop_n],
                )
                .map_err(sql_err)?;
                dropped += g
                    .execute(
                        "DELETE FROM sop_runs WHERE run_id IN
                           (SELECT run_id FROM sop_runs WHERE terminal=1
                            ORDER BY last_progress_at ASC LIMIT ?1)",
                        params![drop_n],
                    )
                    .map_err(sql_err)?;
            }
        }
        if let Some(keep) = policy.keep_secs {
            let cutoff = (Utc::now() - Duration::seconds(keep as i64)).to_rfc3339();
            // `last_progress_at < cutoff` is false for a NULL timestamp, so a row
            // with no stamp is never age-evicted. That is safe here: terminal
            // writes always stamp `last_progress_at` (the engine passes
            // `now_iso8601()` via `PersistedRun::new`), so terminal rows are never
            // NULL in practice.
            g.execute(
                "DELETE FROM sop_events WHERE run_id IN
                   (SELECT run_id FROM sop_runs WHERE terminal=1 AND last_progress_at < ?1)",
                params![cutoff],
            )
            .map_err(sql_err)?;
            dropped += g
                .execute(
                    "DELETE FROM sop_runs WHERE terminal=1 AND last_progress_at < ?1",
                    params![cutoff],
                )
                .map_err(sql_err)?;
        }
        Ok(dropped)
    }

    fn health_check(&self) -> bool {
        match self.lock() {
            Ok(g) => g.query_row("SELECT 1", [], |r| r.get::<_, i64>(0)).is_ok(),
            Err(_) => false,
        }
    }

    fn backend(&self) -> &'static str {
        "sqlite"
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::sop::types::{SopEvent, SopRun, SopRunStatus, SopTriggerSource};
    use serde_json::json;

    fn run(id: &str, status: SopRunStatus, last_progress: &str) -> PersistedRun {
        let r = SopRun {
            run_id: id.to_string(),
            sop_name: "deploy".to_string(),
            trigger_event: SopEvent {
                source: SopTriggerSource::Manual,
                topic: None,
                payload: None,
                timestamp: "t".to_string(),
            },
            frame_marker_id: format!("marker-{id}"),
            status,
            current_step: 0,
            total_steps: 1,
            started_at: last_progress.to_string(),
            completed_at: None,
            step_results: vec![],
            waiting_since: None,
            llm_calls_saved: 0,
        };
        PersistedRun::new(r, last_progress.to_string(), SopTriggerSource::Manual)
    }

    fn ev(run_id: &str, kind: &str) -> SopEventRecord {
        SopEventRecord {
            run_id: run_id.to_string(),
            seq: 0,
            ts: "t".to_string(),
            kind: kind.to_string(),
            actor: None,
            reason: None,
            payload: json!({}),
        }
    }

    #[test]
    fn runs_save_load_finish_roundtrip() {
        let s = SqliteRunStore::open_in_memory().unwrap();
        s.save_run(&run("r1", SopRunStatus::Running, "1")).unwrap();
        assert_eq!(s.load_active_runs().unwrap().len(), 1);
        assert!(s.load_run("r1").unwrap().is_some());
        // Finishing is a state transition, so it carries a bumped revision.
        let mut terminal = run("r1", SopRunStatus::Completed, "2");
        terminal.revision = 1;
        s.finish_run("r1", &terminal).unwrap();
        assert_eq!(
            s.load_active_runs().unwrap().len(),
            0,
            "terminal excluded from active"
        );
        assert!(
            s.load_run("r1").unwrap().is_some(),
            "terminal still loadable"
        );
        assert_eq!(s.backend(), "sqlite");
        assert!(s.health_check());
    }

    #[test]
    fn save_run_rejects_stale_revision() {
        let s = SqliteRunStore::open_in_memory().unwrap();
        let mut newer = run("r1", SopRunStatus::Running, "1");
        newer.revision = 5;
        s.save_run(&newer).unwrap();
        let older = run("r1", SopRunStatus::Running, "1"); // revision 0
        assert!(matches!(
            s.save_run(&older),
            Err(StoreError::StaleRevision { .. })
        ));
    }

    #[test]
    fn claim_single_winner_cap_and_terminal() {
        let s = SqliteRunStore::open_in_memory().unwrap();
        assert!(s.try_claim_run("r1", "deploy", 2, 2).unwrap().is_some());
        assert!(
            s.try_claim_run("r1", "deploy", 2, 2).unwrap().is_none(),
            "dup refused"
        );
        assert!(s.try_claim_run("r2", "deploy", 2, 2).unwrap().is_some());
        assert!(
            s.try_claim_run("r3", "deploy", 2, 2).unwrap().is_none(),
            "cap reached"
        );
        // terminal run not re-claimable
        s.finish_run("rT", &run("rT", SopRunStatus::Completed, "1"))
            .unwrap();
        assert!(
            s.try_claim_run("rT", "deploy", 0, 0).unwrap().is_none(),
            "terminal not claimable"
        );
    }

    #[test]
    fn save_run_rejects_divergent_same_revision() {
        let s = SqliteRunStore::open_in_memory().unwrap();
        let mut base = run("r1", SopRunStatus::Running, "1");
        base.revision = 5;
        s.save_run(&base).unwrap();
        // A byte-identical same-revision write is an idempotent retry.
        s.save_run(&base).unwrap();
        // A divergent same-revision write is refused.
        let mut divergent = run("r1", SopRunStatus::Running, "2");
        divergent.revision = 5;
        assert!(matches!(
            s.save_run(&divergent),
            Err(StoreError::RevisionConflict { revision: 5, .. })
        ));
        // The stored run is unchanged (still revision 5, last_progress "1").
        let stored = s.load_run("r1").unwrap().unwrap();
        assert_eq!(stored.revision, 5);
        assert_eq!(stored.last_progress_at, "1");
    }

    #[test]
    fn finish_run_revision_guard_protects_state_and_claim() {
        let s = SqliteRunStore::open_in_memory().unwrap();
        let mut base = run("r1", SopRunStatus::Running, "1");
        base.revision = 5;
        s.save_run(&base).unwrap();
        s.try_claim_run("r1", "deploy", 4, 4).unwrap().unwrap();

        // A stale terminal write (older revision) is refused.
        let mut stale = run("r1", SopRunStatus::Completed, "2");
        stale.revision = 4;
        assert!(matches!(
            s.finish_run("r1", &stale),
            Err(StoreError::StaleRevision { .. })
        ));
        // A divergent same-revision terminal write is refused too.
        let mut divergent = run("r1", SopRunStatus::Completed, "2");
        divergent.revision = 5;
        assert!(matches!(
            s.finish_run("r1", &divergent),
            Err(StoreError::RevisionConflict { .. })
        ));
        // State survived: still active at revision 5, claim still held.
        assert_eq!(s.load_active_runs().unwrap().len(), 1);
        assert_eq!(s.load_run("r1").unwrap().unwrap().revision, 5);
        assert!(
            s.try_claim_run("r1", "deploy", 4, 4).unwrap().is_none(),
            "claim slot still held"
        );

        // A proper terminal write at a newer revision succeeds and releases.
        let mut done = run("r1", SopRunStatus::Completed, "2");
        done.revision = 6;
        s.finish_run("r1", &done).unwrap();
        assert_eq!(s.load_active_runs().unwrap().len(), 0);
        assert_eq!(s.load_run("r1").unwrap().unwrap().revision, 6);
    }

    #[test]
    fn claim_caps_isolate_per_sop_yet_share_global() {
        let s = SqliteRunStore::open_in_memory().unwrap();
        // per_sop_cap = 1, global_cap = 3.
        assert!(s.try_claim_run("a1", "a", 1, 3).unwrap().is_some());
        // A second "a" run is blocked by the per-SOP cap...
        assert!(s.try_claim_run("a2", "a", 1, 3).unwrap().is_none());
        // ...but a different SOP is not blocked by "a" being at its cap.
        assert!(s.try_claim_run("b1", "b", 1, 3).unwrap().is_some());
        assert!(s.try_claim_run("c1", "c", 1, 3).unwrap().is_some());
        // Global cap = 3 is reached across all SOPs; a fourth distinct SOP
        // is refused even though its own per-SOP slot is free.
        assert!(s.try_claim_run("d1", "d", 1, 3).unwrap().is_none());
    }

    #[test]
    fn events_append_only_monotonic_and_ordered() {
        let s = SqliteRunStore::open_in_memory().unwrap();
        assert_eq!(s.append_event(&ev("r1", "run_started")).unwrap(), 1);
        assert_eq!(s.append_event(&ev("r1", "step_completed")).unwrap(), 2);
        assert_eq!(s.append_event(&ev("r2", "run_started")).unwrap(), 3);
        let r1 = s.list_events("r1").unwrap();
        assert_eq!(r1.len(), 2);
        assert_eq!(r1[0].seq, 1);
        assert_eq!(r1[1].kind, "step_completed");
        assert_eq!(s.list_events("r2").unwrap().len(), 1);
    }

    #[test]
    fn prune_evicts_oldest_terminal_first() {
        let s = SqliteRunStore::open_in_memory().unwrap();
        for (id, ts) in [("a", "1"), ("b", "2"), ("c", "3")] {
            s.finish_run(id, &run(id, SopRunStatus::Completed, ts))
                .unwrap();
        }
        let dropped = s
            .prune(&RetentionPolicy {
                max_terminal: 1,
                keep_secs: None,
            })
            .unwrap();
        assert_eq!(dropped, 2);
        assert!(s.load_run("c").unwrap().is_some(), "newest kept");
        assert!(s.load_run("a").unwrap().is_none(), "oldest dropped");
        assert!(s.load_run("b").unwrap().is_none());
    }

    #[test]
    fn runs_survive_reopen() {
        // Durability: a run written by one instance is visible to a fresh
        // instance opening the same file (the restart-resume guarantee).
        let path = std::env::temp_dir().join(format!("zc-sop-durable-{}.db", std::process::id()));
        let _ = std::fs::remove_file(&path);
        {
            let a = SqliteRunStore::open(&path).unwrap();
            a.save_run(&run("r1", SopRunStatus::WaitingApproval, "1"))
                .unwrap();
        } // drop `a` - simulates daemon shutdown
        let b = SqliteRunStore::open(&path).unwrap();
        let active = b.load_active_runs().unwrap();
        assert_eq!(active.len(), 1);
        assert_eq!(active[0].run.run_id, "r1");
        assert_eq!(active[0].run.status, SopRunStatus::WaitingApproval);
        let _ = std::fs::remove_file(&path);
    }
}
