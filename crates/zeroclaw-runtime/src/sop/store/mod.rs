//! Durable SOP run-state store (EPIC B) — the keystone contract.

pub mod model;
pub mod sqlite;

use std::collections::{HashMap, HashSet};
use std::path::{Path, PathBuf};
use std::sync::{Arc, Mutex};

use zeroclaw_config::schema::{SopConfig, SopRunStoreBackend};

pub use model::{
    ClaimToken, PersistedRun, ProposalKind, ProposalRecord, ProposalStatus, RetentionPolicy,
    SOP_STORE_VERSION, SopEventRecord,
};
pub use sqlite::SqliteRunStore;

pub trait SopRunStore: Send + Sync {
    // ── run state (persistence-resume, state-machine) ──
    /// Persist-before-mutate. Revision-guarded: a strictly-older revision is
    /// rejected as `StaleRevision`; an equal revision is accepted only as a
    /// byte-identical idempotent retry, else `RevisionConflict`; a newer
    /// revision wins.
    fn save_run(&self, run: &PersistedRun) -> Result<(), StoreError>;
    /// Move a run to terminal state (kept as a terminal record, not deleted) and
    /// release any live claim. Revision-guarded exactly like `save_run`, so a
    /// stale or divergent terminal write cannot clobber newer state or release a
    /// live claim.
    fn finish_run(&self, run_id: &str, terminal: &PersistedRun) -> Result<(), StoreError>;
    /// Boot-rehydrate source: every non-terminal run (latest revision per id).
    fn load_active_runs(&self) -> Result<Vec<PersistedRun>, StoreError>;
    /// Boot-rehydrate source for the display retention window: terminal runs,
    /// newest-first by `started_at`, truncated to `limit` (0 = unbounded). The
    /// engine seeds `finished_runs` from this so completed/failed/cancelled runs
    /// survive a restart in the Runs surface, matching `max_finished_runs`.
    fn load_terminal_runs(&self, limit: usize) -> Result<Vec<PersistedRun>, StoreError>;
    /// Single run by id (latest revision), terminal or not.
    fn load_run(&self, run_id: &str) -> Result<Option<PersistedRun>, StoreError>;
    /// `completed_at` of the most recently successful terminal run for `sop_name`,
    /// or `None` if that SOP has no completed run with a recorded completion.
    /// Drives the cooldown check off the shared store so every engine holder
    /// observes the same success marker (not just the engine that ran the SOP).
    fn last_terminal_completed_at(&self, sop_name: &str) -> Result<Option<String>, StoreError>;

    // ── CAS claim primitive (concurrency-control) ──
    fn try_claim_run(
        &self,
        run_id: &str,
        sop_name: &str,
        per_sop_cap: usize,
        global_cap: usize,
    ) -> Result<Option<ClaimToken>, StoreError>;
    fn renew_claim_for_restore(
        &self,
        run_id: &str,
        sop_name: &str,
    ) -> Result<ClaimToken, StoreError>;
    /// Live claim counts as `(for_sop, total)`, used by read-only admission
    /// checks so status surfaces observe the same concurrency source as CAS.
    fn claim_counts(&self, sop_name: &str) -> Result<(usize, usize), StoreError>;
    /// Renew a claim's lease (tick liveness). No-op if the claim is gone.
    fn heartbeat_claim(&self, token: &ClaimToken) -> Result<(), StoreError>;
    /// Release a claim (finish/cancel), freeing the slot for admission.
    fn release_claim(&self, token: &ClaimToken) -> Result<(), StoreError>;
    /// Reaper source: claims whose lease expired at/<= `now_iso`.
    fn expired_claims(&self, now_iso: &str) -> Result<Vec<ClaimToken>, StoreError>;

    // ── append-only event log (audit-trail, observability) ──
    /// Append-only, monotonic-seq, never-overwrite. Returns the assigned seq.
    fn append_event(&self, ev: &SopEventRecord) -> Result<u64, StoreError>;
    /// Ordered event history for a run.
    fn list_events(&self, run_id: &str) -> Result<Vec<SopEventRecord>, StoreError>;

    // ── proposal namespace (procedural-memory — strictly last consumer) ──
    fn save_proposal(&self, p: &ProposalRecord) -> Result<(), StoreError>;
    fn load_proposal(&self, id: &str) -> Result<Option<ProposalRecord>, StoreError>;
    fn list_proposals(
        &self,
        status: Option<ProposalStatus>,
    ) -> Result<Vec<ProposalRecord>, StoreError>;

    // ── maintenance ──
    /// Drop terminal runs beyond the retention policy. Returns the count dropped.
    fn prune(&self, policy: &RetentionPolicy) -> Result<usize, StoreError>;
    fn health_check(&self) -> bool;
    /// Backend name (for logs + the "never a silent no-op" guard).
    fn backend(&self) -> &'static str;
}

/// Errors a store may surface. Never swallowed by callers.
#[derive(Debug)]
pub enum StoreError {
    Io(std::io::Error),
    Serde(serde_json::Error),
    Backend(String),
    /// A save lost the revision race (a newer revision already persisted).
    StaleRevision {
        run_id: String,
        have: u64,
        found: u64,
    },
    /// A same-revision write whose content diverges from the stored run. The
    /// caller must bump `revision` to record new state; only a byte-identical
    /// retry at the same revision is accepted (idempotent).
    RevisionConflict {
        run_id: String,
        revision: u64,
    },
    /// A claim was lost to a concurrent winner (over-cap or already claimed).
    ClaimLost,
}

impl std::fmt::Display for StoreError {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        match self {
            Self::Io(e) => write!(f, "sop store io error: {e}"),
            Self::Serde(e) => write!(f, "sop store serde error: {e}"),
            Self::Backend(m) => write!(f, "sop store backend error: {m}"),
            Self::StaleRevision {
                run_id,
                have,
                found,
            } => write!(
                f,
                "sop store stale revision for run {run_id}: have {have}, found {found}"
            ),
            Self::RevisionConflict { run_id, revision } => write!(
                f,
                "sop store revision conflict for run {run_id}: divergent write at revision {revision}"
            ),
            Self::ClaimLost => write!(f, "sop store claim lost to a concurrent winner"),
        }
    }
}

impl std::error::Error for StoreError {}

impl From<std::io::Error> for StoreError {
    fn from(e: std::io::Error) -> Self {
        Self::Io(e)
    }
}

impl From<serde_json::Error> for StoreError {
    fn from(e: serde_json::Error) -> Self {
        Self::Serde(e)
    }
}

/// Build the configured run store.
///
/// - `persist_runs = true` (default), backend `"sqlite"` (default) -> [`SqliteRunStore`] at
///   `<run_state_dir | data_dir/sop>/runs.db` (dir created mode-0700), so in-flight runs -
///   including runs parked at a HITL approval - survive a restart.
/// - `persist_runs = false` -> ephemeral [`InMemoryRunStore`] (opt back into non-durable state).
/// - `persist_runs = true`, backend `"memory"` -> ephemeral [`InMemoryRunStore`] (degraded/tests).
///
/// The backend is the closed `SopRunStoreBackend` enum, so an out-of-set value is
/// rejected at config-deserialize time rather than here (no runtime unknown arm).
///
/// Called by `build_sop_engine`, which injects the result via `with_store` and
/// then calls `restore_runs()` to rehydrate in-flight runs at startup. A
/// backend-open failure is non-fatal there: the daemon logs and falls back to
/// the in-memory store rather than failing to boot.
pub fn build_run_store(
    cfg: &SopConfig,
    data_dir: &Path,
) -> Result<Arc<dyn SopRunStore>, StoreError> {
    if !cfg.persist_runs {
        return Ok(Arc::new(InMemoryRunStore::new()));
    }
    // Closed set: a serde enum, matched on variants. serde rejects unknown values
    // at deserialize time, so there is no runtime unknown-backend arm.
    match cfg.run_store_backend {
        SopRunStoreBackend::Sqlite => {
            let dir: PathBuf = match cfg.run_state_dir.as_deref() {
                Some(d) if !d.is_empty() => PathBuf::from(shellexpand::tilde(d).as_ref()),
                _ => data_dir.join("sop"),
            };
            std::fs::create_dir_all(&dir)?;
            #[cfg(unix)]
            {
                use std::os::unix::fs::PermissionsExt;
                // Best-effort tighten to 0700; ignore if the filesystem rejects it.
                let _ = std::fs::set_permissions(&dir, std::fs::Permissions::from_mode(0o700));
            }
            Ok(Arc::new(sqlite::SqliteRunStore::open(
                &dir.join("runs.db"),
            )?))
        }
        SopRunStoreBackend::Memory => Ok(Arc::new(InMemoryRunStore::new())),
    }
}

// ── In-memory default backend ──────────────────────────────────────

#[derive(Default)]
struct Inner {
    runs: HashMap<String, PersistedRun>,
    terminal: HashSet<String>,
    events: HashMap<String, Vec<SopEventRecord>>,
    claims: HashMap<String, ClaimToken>,
    proposals: HashMap<String, ProposalRecord>,
    seq: u64,
}

/// Process-local, non-durable store. Mirrors today's in-memory run maps; lost on
/// restart. The compatibility default until `SqliteRunStore` lands.
pub struct InMemoryRunStore {
    inner: Mutex<Inner>,
}

impl Default for InMemoryRunStore {
    fn default() -> Self {
        Self::new()
    }
}

impl InMemoryRunStore {
    pub fn new() -> Self {
        Self {
            inner: Mutex::new(Inner::default()),
        }
    }

    fn lock(&self) -> Result<std::sync::MutexGuard<'_, Inner>, StoreError> {
        self.inner
            .lock()
            .map_err(|_| StoreError::Backend("in-memory store lock poisoned".into()))
    }
}

fn revision_guard(
    existing: Option<&PersistedRun>,
    incoming: &PersistedRun,
) -> Result<(), StoreError> {
    let Some(existing) = existing else {
        return Ok(());
    };
    if incoming.revision < existing.revision {
        return Err(StoreError::StaleRevision {
            run_id: incoming.run_id().to_string(),
            have: incoming.revision,
            found: existing.revision,
        });
    }
    if incoming.revision == existing.revision
        && serde_json::to_vec(existing)? != serde_json::to_vec(incoming)?
    {
        return Err(StoreError::RevisionConflict {
            run_id: incoming.run_id().to_string(),
            revision: incoming.revision,
        });
    }
    Ok(())
}

impl SopRunStore for InMemoryRunStore {
    fn save_run(&self, run: &PersistedRun) -> Result<(), StoreError> {
        let mut g = self.lock()?;
        revision_guard(g.runs.get(run.run_id()), run)?;
        g.runs.insert(run.run_id().to_string(), run.clone());
        Ok(())
    }

    fn finish_run(&self, run_id: &str, terminal: &PersistedRun) -> Result<(), StoreError> {
        let mut g = self.lock()?;
        revision_guard(g.runs.get(run_id), terminal)?;
        g.runs.insert(run_id.to_string(), terminal.clone());
        g.terminal.insert(run_id.to_string());
        g.claims.remove(run_id);
        Ok(())
    }

    fn load_active_runs(&self) -> Result<Vec<PersistedRun>, StoreError> {
        let g = self.lock()?;
        Ok(g.runs
            .values()
            .filter(|r| !g.terminal.contains(r.run_id()))
            .cloned()
            .collect())
    }

    fn load_terminal_runs(&self, limit: usize) -> Result<Vec<PersistedRun>, StoreError> {
        let g = self.lock()?;
        let mut out: Vec<PersistedRun> = g
            .runs
            .values()
            .filter(|r| g.terminal.contains(r.run_id()))
            .cloned()
            .collect();
        out.sort_by(|a, b| b.run.started_at.cmp(&a.run.started_at));
        if limit > 0 && out.len() > limit {
            out.truncate(limit);
        }
        Ok(out)
    }

    fn load_run(&self, run_id: &str) -> Result<Option<PersistedRun>, StoreError> {
        Ok(self.lock()?.runs.get(run_id).cloned())
    }

    fn last_terminal_completed_at(&self, sop_name: &str) -> Result<Option<String>, StoreError> {
        let g = self.lock()?;
        // Max `completed_at` over successful terminal runs for this SOP.
        // Timestamps are ISO-8601 UTC ("...Z"), which sort lexically in
        // completion order.
        Ok(g.terminal
            .iter()
            .filter_map(|id| g.runs.get(id))
            .filter(|r| r.run.sop_name == sop_name)
            .filter(|r| r.run.status == crate::sop::types::SopRunStatus::Completed)
            .filter_map(|r| r.run.completed_at.clone())
            .max())
    }

    fn try_claim_run(
        &self,
        run_id: &str,
        sop_name: &str,
        per_sop_cap: usize,
        global_cap: usize,
    ) -> Result<Option<ClaimToken>, StoreError> {
        let mut g = self.lock()?;
        if g.claims.contains_key(run_id) {
            return Ok(None);
        }
        // A run that has already reached a terminal state is not re-claimable.
        if g.terminal.contains(run_id) {
            return Ok(None);
        }
        // Both caps are enforced atomically under one lock, so neither limit can
        // be crossed by a concurrent winner. Counts are over LIVE claims, which
        // track admitted (active) runs 1:1 (mirrors the engine `can_start`).
        // A cap of 0 admits nothing (matches the engine `>= max_concurrent`).
        let active_for_sop = g.claims.values().filter(|c| c.sop_name == sop_name).count();
        if active_for_sop >= per_sop_cap {
            return Ok(None);
        }
        if g.claims.len() >= global_cap {
            return Ok(None);
        }
        // Timestamps are stamped by durable backends; the in-memory backend
        // leaves them empty (the reaper skips empty leases — see `expired_claims`).
        let token = ClaimToken {
            run_id: run_id.to_string(),
            sop_name: sop_name.to_string(),
            claimed_at: String::new(),
            lease_expires: String::new(),
            holder: "in-memory".to_string(),
        };
        g.claims.insert(run_id.to_string(), token.clone());
        Ok(Some(token))
    }

    fn renew_claim_for_restore(
        &self,
        run_id: &str,
        sop_name: &str,
    ) -> Result<ClaimToken, StoreError> {
        let mut g = self.lock()?;
        // No cap check: a restored run was already admitted before the restart.
        // Idempotent insert/overwrite (matches the in-memory empty-lease shape).
        let token = ClaimToken {
            run_id: run_id.to_string(),
            sop_name: sop_name.to_string(),
            claimed_at: String::new(),
            lease_expires: String::new(),
            holder: "in-memory".to_string(),
        };
        g.claims.insert(run_id.to_string(), token.clone());
        Ok(token)
    }

    fn claim_counts(&self, sop_name: &str) -> Result<(usize, usize), StoreError> {
        let g = self.lock()?;
        let per_sop = g.claims.values().filter(|c| c.sop_name == sop_name).count();
        Ok((per_sop, g.claims.len()))
    }

    fn heartbeat_claim(&self, _token: &ClaimToken) -> Result<(), StoreError> {
        Ok(())
    }

    fn release_claim(&self, token: &ClaimToken) -> Result<(), StoreError> {
        self.lock()?.claims.remove(&token.run_id);
        Ok(())
    }

    fn expired_claims(&self, now_iso: &str) -> Result<Vec<ClaimToken>, StoreError> {
        let g = self.lock()?;
        Ok(g.claims
            .values()
            .filter(|c| !c.lease_expires.is_empty() && c.lease_expires.as_str() <= now_iso)
            .cloned()
            .collect())
    }

    fn append_event(&self, ev: &SopEventRecord) -> Result<u64, StoreError> {
        let mut g = self.lock()?;
        g.seq += 1;
        let seq = g.seq;
        let mut rec = ev.clone();
        rec.seq = seq;
        g.events.entry(ev.run_id.clone()).or_default().push(rec);
        Ok(seq)
    }

    fn list_events(&self, run_id: &str) -> Result<Vec<SopEventRecord>, StoreError> {
        let mut v = self.lock()?.events.get(run_id).cloned().unwrap_or_default();
        v.sort_by_key(|e| e.seq);
        Ok(v)
    }

    fn save_proposal(&self, p: &ProposalRecord) -> Result<(), StoreError> {
        self.lock()?.proposals.insert(p.id.clone(), p.clone());
        Ok(())
    }

    fn load_proposal(&self, id: &str) -> Result<Option<ProposalRecord>, StoreError> {
        Ok(self.lock()?.proposals.get(id).cloned())
    }

    fn list_proposals(
        &self,
        status: Option<ProposalStatus>,
    ) -> Result<Vec<ProposalRecord>, StoreError> {
        let g = self.lock()?;
        Ok(g.proposals
            .values()
            .filter(|p| status.is_none_or(|s| p.status == s))
            .cloned()
            .collect())
    }

    fn prune(&self, policy: &RetentionPolicy) -> Result<usize, StoreError> {
        let mut g = self.lock()?;
        let mut dropped = 0usize;

        if let Some(keep) = policy.keep_secs {
            let cutoff = (chrono::Utc::now() - chrono::Duration::seconds(keep as i64))
                .format("%Y-%m-%dT%H:%M:%SZ")
                .to_string();
            let expired: Vec<String> = g
                .terminal
                .iter()
                .filter(|id| {
                    g.runs.get(*id).is_some_and(|r| {
                        let ts = r
                            .run
                            .completed_at
                            .as_deref()
                            .unwrap_or(r.run.started_at.as_str());
                        ts < cutoff.as_str()
                    })
                })
                .cloned()
                .collect();
            for id in expired {
                g.runs.remove(&id);
                g.events.remove(&id);
                g.terminal.remove(&id);
                dropped += 1;
            }
        }

        // Count bound (`max_terminal`, 0 = unbounded): keep at most N terminal
        // runs, evicting the OLDEST excess (by completion, then start time).
        // HashSet order is non-deterministic, so sort before truncating.
        if policy.max_terminal > 0 && g.terminal.len() > policy.max_terminal {
            let mut terminal: Vec<(String, String)> = g
                .terminal
                .iter()
                .map(|id| {
                    let ts = g
                        .runs
                        .get(id)
                        .map(|r| {
                            r.run
                                .completed_at
                                .clone()
                                .unwrap_or_else(|| r.run.started_at.clone())
                        })
                        .unwrap_or_default();
                    (id.clone(), ts)
                })
                .collect();
            terminal.sort_by(|a, b| a.1.cmp(&b.1));
            let drop_n = terminal.len() - policy.max_terminal;
            for (id, _) in terminal.into_iter().take(drop_n) {
                g.runs.remove(&id);
                g.events.remove(&id);
                g.terminal.remove(&id);
                dropped += 1;
            }
        }
        Ok(dropped)
    }

    fn health_check(&self) -> bool {
        self.inner.lock().is_ok()
    }

    fn backend(&self) -> &'static str {
        "in-memory"
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::sop::types::{SopEvent, SopRun, SopRunStatus, SopTriggerSource};
    use serde_json::json;

    /// Build a `PersistedRun` at a given revision and completion timestamp.
    fn run(id: &str, revision: u64, completed_at: Option<&str>) -> PersistedRun {
        let started_at = "2020-01-01T00:00:00Z".to_string();
        let sop_run = SopRun {
            run_id: id.to_string(),
            sop_name: "deploy".to_string(),
            trigger_event: SopEvent {
                source: SopTriggerSource::Manual,
                topic: None,
                payload: None,
                timestamp: started_at.clone(),
            },
            frame_marker_id: format!("marker-{id}"),
            status: SopRunStatus::Running,
            current_step: 0,
            total_steps: 1,
            started_at: started_at.clone(),
            completed_at: completed_at.map(|s| s.to_string()),
            step_results: Vec::new(),
            waiting_since: None,
            llm_calls_saved: 0,
        };
        PersistedRun {
            version: SOP_STORE_VERSION,
            revision,
            run: sop_run,
            last_progress_at: started_at,
            redacted: false,
            trigger_source: SopTriggerSource::Manual,
        }
    }

    fn ev(run: &str, kind: &str) -> SopEventRecord {
        SopEventRecord {
            run_id: run.to_string(),
            seq: 0,
            ts: "t".to_string(),
            kind: kind.to_string(),
            actor: None,
            reason: None,
            payload: json!({}),
        }
    }

    fn proposal(id: &str, status: ProposalStatus) -> ProposalRecord {
        ProposalRecord {
            id: id.to_string(),
            kind: ProposalKind::Update,
            status,
            source_run_id: None,
            sop_name: "deploy".to_string(),
            target_content_hash: None,
            manifest_toml: "[sop]\nname = \"deploy\"\ndescription = \"Deploy\"\n".to_string(),
            procedure_markdown: "## Steps\n\n1. **Deploy** - Do it.\n".to_string(),
            provenance: json!({}),
            created_at: "t".to_string(),
            updated_at: "t".to_string(),
            status_reason: None,
            applied_at: None,
            applied_by: None,
            rollback_path: None,
        }
    }

    #[test]
    fn claim_is_single_winner_and_cap_bounded() {
        let s = InMemoryRunStore::new();
        // First claim wins.
        assert!(s.try_claim_run("r1", "deploy", 2, 2).unwrap().is_some());
        // Duplicate claim on the same run is refused.
        assert!(s.try_claim_run("r1", "deploy", 2, 2).unwrap().is_none());
        // Second distinct run claims (under cap).
        assert!(s.try_claim_run("r2", "deploy", 2, 2).unwrap().is_some());
        // Third exceeds cap=2.
        assert!(s.try_claim_run("r3", "deploy", 2, 2).unwrap().is_none());
        // Releasing frees a slot.
        let tok = ClaimToken {
            run_id: "r1".to_string(),
            sop_name: "deploy".to_string(),
            claimed_at: String::new(),
            lease_expires: String::new(),
            holder: "in-memory".to_string(),
        };
        s.release_claim(&tok).unwrap();
        assert!(s.try_claim_run("r3", "deploy", 2, 2).unwrap().is_some());
    }

    #[test]
    fn claim_caps_isolate_per_sop_yet_share_global() {
        let s = InMemoryRunStore::new();
        // per_sop_cap = 1, global_cap = 3.
        // SOP "a" fills its single per-SOP slot.
        assert!(s.try_claim_run("a1", "a", 1, 3).unwrap().is_some());
        // A second "a" run is blocked by the per-SOP cap...
        assert!(s.try_claim_run("a2", "a", 1, 3).unwrap().is_none());
        // ...but a DIFFERENT SOP is not blocked by "a" being at its cap.
        assert!(s.try_claim_run("b1", "b", 1, 3).unwrap().is_some());
        assert!(s.try_claim_run("c1", "c", 1, 3).unwrap().is_some());
        // Global cap = 3 is now reached across all SOPs; a fourth distinct SOP
        // is refused even though its own per-SOP slot is free.
        assert!(s.try_claim_run("d1", "d", 1, 3).unwrap().is_none());
    }

    #[test]
    fn events_are_append_only_with_monotonic_seq() {
        let s = InMemoryRunStore::new();
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
    fn proposals_round_trip_and_filter_by_status() {
        let s = InMemoryRunStore::new();
        s.save_proposal(&proposal("p1", ProposalStatus::Pending))
            .unwrap();
        s.save_proposal(&proposal("p2", ProposalStatus::Applied))
            .unwrap();
        assert_eq!(
            s.load_proposal("p1").unwrap().unwrap().status,
            ProposalStatus::Pending
        );
        assert_eq!(s.list_proposals(None).unwrap().len(), 2);
        assert_eq!(
            s.list_proposals(Some(ProposalStatus::Applied))
                .unwrap()
                .len(),
            1
        );
        assert_eq!(s.backend(), "in-memory");
    }

    #[test]
    fn save_run_rejects_stale_revision_but_accepts_same_or_newer() {
        let s = InMemoryRunStore::new();
        s.save_run(&run("r1", 5, None)).unwrap();
        // A lower revision loses the race.
        let err = s.save_run(&run("r1", 4, None)).unwrap_err();
        assert!(matches!(
            err,
            StoreError::StaleRevision {
                have: 4,
                found: 5,
                ..
            }
        ));
        // The stored revision is unchanged after the rejected write.
        assert_eq!(s.load_run("r1").unwrap().unwrap().revision, 5);
        // A byte-identical same-revision write is an idempotent retry.
        s.save_run(&run("r1", 5, None)).unwrap();
        // A DIVERGENT same-revision write is refused, not silently applied.
        let err = s
            .save_run(&run("r1", 5, Some("2020-06-06T00:00:00Z")))
            .unwrap_err();
        assert!(matches!(
            err,
            StoreError::RevisionConflict { revision: 5, .. }
        ));
        // The divergent payload did not land: completed_at is still None.
        assert!(
            s.load_run("r1")
                .unwrap()
                .unwrap()
                .run
                .completed_at
                .is_none()
        );
        // A newer revision wins.
        s.save_run(&run("r1", 6, None)).unwrap();
        assert_eq!(s.load_run("r1").unwrap().unwrap().revision, 6);
    }

    #[test]
    fn finish_run_marks_terminal_and_releases_claim() {
        let s = InMemoryRunStore::new();
        s.save_run(&run("r1", 0, None)).unwrap();
        let tok = s.try_claim_run("r1", "deploy", 4, 4).unwrap().unwrap();
        assert!(
            s.load_active_runs()
                .unwrap()
                .iter()
                .any(|r| r.run_id() == "r1")
        );

        s.finish_run("r1", &run("r1", 1, Some("2020-01-02T00:00:00Z")))
            .unwrap();

        // No longer reported as active...
        assert!(s.load_active_runs().unwrap().is_empty());
        // ...but still loadable as a terminal record.
        assert_eq!(s.load_run("r1").unwrap().unwrap().revision, 1);
        // ...and its claim slot was freed.
        assert!(s.try_claim_run("r1b", "deploy", 1, 4).unwrap().is_some());
        // A terminal run is not re-claimable even after its claim was released.
        assert!(s.try_claim_run("r1", "deploy", 4, 4).unwrap().is_none());
        // release_claim on the old token is a no-op (slot already freed).
        s.release_claim(&tok).unwrap();
    }

    #[test]
    fn finish_run_revision_guard_protects_live_state_and_claim() {
        let s = InMemoryRunStore::new();
        s.save_run(&run("r1", 5, None)).unwrap();
        let _tok = s.try_claim_run("r1", "deploy", 4, 4).unwrap().unwrap();

        // A stale terminal write (older revision) is refused: it must not
        // clobber newer state nor release the live claim.
        let err = s
            .finish_run("r1", &run("r1", 4, Some("2020-01-02T00:00:00Z")))
            .unwrap_err();
        assert!(matches!(err, StoreError::StaleRevision { .. }));
        // A divergent same-revision terminal write is refused too.
        let err = s
            .finish_run("r1", &run("r1", 5, Some("2020-01-02T00:00:00Z")))
            .unwrap_err();
        assert!(matches!(err, StoreError::RevisionConflict { .. }));

        // State survived: still active, still revision 5, claim still held
        // (a fresh claim on the same run is refused because the slot is taken).
        assert!(
            s.load_active_runs()
                .unwrap()
                .iter()
                .any(|r| r.run_id() == "r1")
        );
        assert_eq!(s.load_run("r1").unwrap().unwrap().revision, 5);
        assert!(s.try_claim_run("r1", "deploy", 4, 4).unwrap().is_none());

        // A proper terminal write at a newer revision succeeds and releases.
        s.finish_run("r1", &run("r1", 6, Some("2020-01-02T00:00:00Z")))
            .unwrap();
        assert!(s.load_active_runs().unwrap().is_empty());
        assert_eq!(s.load_run("r1").unwrap().unwrap().revision, 6);
    }

    #[test]
    fn prune_evicts_oldest_terminal_runs_first() {
        let s = InMemoryRunStore::new();
        // Three terminal runs with ascending completion timestamps.
        for (id, ts) in [
            ("old", "2020-01-01T00:00:00Z"),
            ("mid", "2020-02-01T00:00:00Z"),
            ("new", "2020-03-01T00:00:00Z"),
        ] {
            s.finish_run(id, &run(id, 1, Some(ts))).unwrap();
        }
        let dropped = s
            .prune(&RetentionPolicy {
                max_terminal: 2,
                keep_secs: None,
            })
            .unwrap();
        assert_eq!(dropped, 1);
        // The oldest was evicted; the two newest survive.
        assert!(s.load_run("old").unwrap().is_none());
        assert!(s.load_run("mid").unwrap().is_some());
        assert!(s.load_run("new").unwrap().is_some());
        // A no-op prune (under cap) drops nothing.
        assert_eq!(
            s.prune(&RetentionPolicy {
                max_terminal: 100,
                keep_secs: None,
            })
            .unwrap(),
            0
        );
    }

    #[test]
    fn prune_keep_secs_drops_aged_runs_even_under_max_terminal() {
        let s = InMemoryRunStore::new();
        // Two terminal runs; the count (2) stays well under max_terminal, so only
        // the age bound can evict. One is ancient, one is far-future.
        s.finish_run("old", &run("old", 1, Some("2000-01-01T00:00:00Z")))
            .unwrap();
        s.finish_run("fresh", &run("fresh", 1, Some("2099-01-01T00:00:00Z")))
            .unwrap();
        let dropped = s
            .prune(&RetentionPolicy {
                max_terminal: 100,
                keep_secs: Some(1),
            })
            .unwrap();
        assert_eq!(
            dropped, 1,
            "aged terminal run must be pruned by keep_secs despite count under cap"
        );
        assert!(s.load_run("old").unwrap().is_none(), "ancient run evicted");
        assert!(
            s.load_run("fresh").unwrap().is_some(),
            "future-dated run retained"
        );
        // keep_secs also applies when max_terminal is unbounded (0).
        s.finish_run("old2", &run("old2", 1, Some("2001-01-01T00:00:00Z")))
            .unwrap();
        assert_eq!(
            s.prune(&RetentionPolicy {
                max_terminal: 0,
                keep_secs: Some(1),
            })
            .unwrap(),
            1,
            "keep_secs prunes even with unbounded max_terminal"
        );
    }

    #[test]
    fn expired_claims_skips_empty_leases_and_matches_past_due() {
        let s = InMemoryRunStore::new();
        // The in-memory backend stamps empty leases — those are never expired.
        s.try_claim_run("r1", "deploy", 4, 4).unwrap().unwrap();
        assert!(s.expired_claims("2999-01-01T00:00:00Z").unwrap().is_empty());
    }

    fn cfg() -> SopConfig {
        serde_json::from_str("{}").expect("default SopConfig from empty object")
    }

    #[test]
    fn factory_defaults_to_durable_sqlite() {
        // persist_runs now defaults on (HITL admission durability) -> the durable
        // sqlite backend is selected so a run parked at an approval survives restart.
        let dir = std::env::temp_dir().join(format!("zc-sop-default-{}", std::process::id()));
        let _ = std::fs::remove_dir_all(&dir);
        let mut c = cfg();
        c.run_state_dir = Some(dir.to_string_lossy().into_owned());
        let s = build_run_store(&c, Path::new("/unused")).unwrap();
        assert_eq!(s.backend(), "sqlite");
        let _ = std::fs::remove_dir_all(&dir);
    }

    #[test]
    fn factory_in_memory_when_persist_disabled() {
        // Opt back out: persist_runs = false -> ephemeral in-memory, data_dir untouched.
        let mut c = cfg();
        c.persist_runs = false;
        let s = build_run_store(&c, Path::new("/nonexistent")).unwrap();
        assert_eq!(s.backend(), "in-memory");
    }

    #[test]
    fn factory_backend_selection_memory() {
        let mut c = cfg();
        c.persist_runs = true;
        c.run_store_backend = SopRunStoreBackend::Memory;
        assert_eq!(
            build_run_store(&c, Path::new("/nonexistent"))
                .unwrap()
                .backend(),
            "in-memory"
        );
    }

    #[test]
    fn unknown_backend_is_rejected_at_deserialize() {
        // The backend is a closed serde enum, so an out-of-set value fails at parse
        // time rather than at first use; there is no runtime unknown-backend arm.
        let r: Result<SopConfig, _> = serde_json::from_str(r#"{"run_store_backend":"bogus"}"#);
        assert!(
            r.is_err(),
            "an unknown run_store_backend must fail to deserialize"
        );
        // A known value still deserializes (back-compat with existing configs).
        let ok: SopConfig = serde_json::from_str(r#"{"run_store_backend":"sqlite"}"#)
            .expect("known backend deserializes");
        assert_eq!(ok.run_store_backend, SopRunStoreBackend::Sqlite);
    }

    #[test]
    fn factory_builds_sqlite_in_configured_dir() {
        let dir = std::env::temp_dir().join(format!("zc-sop-factory-{}", std::process::id()));
        let _ = std::fs::remove_dir_all(&dir);
        let mut c = cfg();
        c.persist_runs = true;
        c.run_store_backend = SopRunStoreBackend::Sqlite;
        c.run_state_dir = Some(dir.to_string_lossy().into_owned());
        let s = build_run_store(&c, Path::new("/unused")).unwrap();
        assert_eq!(s.backend(), "sqlite");
        assert!(dir.join("runs.db").exists());
        let _ = std::fs::remove_dir_all(&dir);
    }
}
