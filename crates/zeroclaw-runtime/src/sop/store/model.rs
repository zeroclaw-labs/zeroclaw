//! Durable run-state store — versioned wire shapes (EPIC B).

use serde::{Deserialize, Serialize};

use crate::sop::types::{SopRun, SopTriggerSource};

/// Bump when the persisted envelope layout changes incompatibly. Rehydrate
/// skips + logs unknown major versions rather than panicking on boot.
pub const SOP_STORE_VERSION: u32 = 1;

#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct PersistedRun {
    /// = [`SOP_STORE_VERSION`] at write time.
    pub version: u32,
    /// Monotonic per-run write counter; last-writer guard on rehydrate + CAS.
    pub revision: u64,
    /// The full run FSM state (every mode, not just deterministic).
    pub run: SopRun,
    /// ISO-8601; bumped on every transition. The reaper ages out stalled `Running`.
    pub last_progress_at: String,
    /// True once `step_results`/outputs have been run through the leak detector.
    pub redacted: bool,
    /// Origin of the run, for per-source caps + audit.
    pub trigger_source: SopTriggerSource,
}

impl PersistedRun {
    /// Wrap a run at the current store version with revision 0.
    pub fn new(run: SopRun, last_progress_at: String, trigger_source: SopTriggerSource) -> Self {
        Self {
            version: SOP_STORE_VERSION,
            revision: 0,
            run,
            last_progress_at,
            redacted: false,
            trigger_source,
        }
    }

    /// The run id this envelope is keyed by.
    pub fn run_id(&self) -> &str {
        &self.run.run_id
    }
}

/// CAS claim handle. Opaque to the engine; the store validates it. The single
/// admission primitive A1's concurrency tick and C's out-of-band approver share.
#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct ClaimToken {
    pub run_id: String,
    pub sop_name: String,
    pub claimed_at: String,
    /// `now + claim_lease_secs`; the reaper reclaims claims past this.
    pub lease_expires: String,
    /// Process/instance nonce so a CAS winner is provably this process.
    pub holder: String,
}

#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct SopEventRecord {
    pub run_id: String,
    /// Monotonic ordinal assigned by the store.
    pub seq: u64,
    pub ts: String,
    /// e.g. `run_started` | `step_completed` | `entered_waiting_approval` | …
    pub kind: String,
    /// Approver / actor identity (EPIC C threads this in).
    pub actor: Option<String>,
    pub reason: Option<String>,
    /// Redacted before append.
    pub payload: serde_json::Value,
}

/// Lifecycle of a procedural-memory proposal (EPIC F, strictly-last consumer).
#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum ProposalStatus {
    Pending,
    Applied,
    Rejected,
    Quarantined,
    Stale,
}

/// Whether a proposal creates a new SOP or updates an existing one.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum ProposalKind {
    Create,
    Update,
}

/// A captured-and-distilled SOP refinement awaiting approval + write-back.
#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct ProposalRecord {
    pub id: String,
    #[serde(default = "default_proposal_kind")]
    pub kind: ProposalKind,
    pub status: ProposalStatus,
    pub source_run_id: Option<String>,
    pub sop_name: String,
    /// For stale detection against the on-disk SOP.
    pub target_content_hash: Option<String>,
    /// Proposed `SOP.toml` bytes. Kept in the shared store so inspect/apply do
    /// not depend on transient files.
    #[serde(default)]
    pub manifest_toml: String,
    /// Proposed `SOP.md` bytes. Scanned before write-back and written atomically
    /// only after an explicit apply action.
    #[serde(default)]
    pub procedure_markdown: String,
    /// Distiller model, session, timestamp, …
    pub provenance: serde_json::Value,
    pub created_at: String,
    pub updated_at: String,
    #[serde(default)]
    pub status_reason: Option<String>,
    #[serde(default)]
    pub applied_at: Option<String>,
    #[serde(default)]
    pub applied_by: Option<String>,
    #[serde(default)]
    pub rollback_path: Option<String>,
}

fn default_proposal_kind() -> ProposalKind {
    ProposalKind::Update
}

/// Retention bound for terminal runs + their events.
#[derive(Debug, Clone)]
pub struct RetentionPolicy {
    /// Keep at most this many terminal runs (0 = unbounded — discouraged).
    pub max_terminal: usize,
    /// Drop terminal runs whose `completed_at` is older than this, if set.
    pub keep_secs: Option<u64>,
}

impl Default for RetentionPolicy {
    fn default() -> Self {
        // Mirrors today's in-memory `max_finished_runs` default of 100.
        Self {
            max_terminal: 100,
            keep_secs: None,
        }
    }
}
