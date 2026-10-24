//! The recovery contract every agent rename surface shares.
//!
//! Renaming an agent commits the config change first; the state kept under
//! the old alias follows afterwards: the default per-alias workspace, memory
//! attribution, cron jobs and their run history, ACP sessions and their saved
//! working directories, and session attribution. The CLI, the gateway, and the
//! daemon RPC all run the same sequence:
//!
//! 1. [`resolve`] validates both aliases and decides whether the rename is
//!    fresh or resumes an unfinished one.
//! 2. [`arm`] records the rename in the agent lifecycle recovery journal and
//!    holds the journal lock while the surface commits the config.
//! 3. The surface commits, then calls [`acknowledge_commit`], or [`abandon`]
//!    when the commit failed.
//! 4. [`converge`] moves every follower, re-checks all of them, and clears the
//!    record only once nothing is left under the old alias. Each move is
//!    durable before the record clears: the SQLite stores commit it with full
//!    sync, and each converge syncs every directory a workspace move or
//!    cleanup changes before it can clear the record, keeping the record
//!    open while any of them fails to sync.
//!
//! The record keeps the recovery durable across the window after the commit
//! and across processes. While it is open the old alias cannot be reused, and
//! re-running the same rename resumes it. A rename that cannot finish is
//! dropped with [`abandon_rename`], which moves nothing and reports what is
//! still kept under the old alias.

use std::fmt;
use std::path::{Component, Path, PathBuf};
use std::sync::Arc;
use std::time::Duration;

use zeroclaw_api::attribution::{Attributable as _, MemoryKind, Role};
use zeroclaw_api::memory_traits::Memory;
use zeroclaw_config::agent_recovery_journal::{
    self, AgentRecoveryJournal, JournalError, JournalGuard, RecoveryOperation, RecoveryPhase,
    RecoveryRecord,
};
use zeroclaw_config::schema::Config;
use zeroclaw_infra::acp_session_store::AcpSessionStore;
use zeroclaw_infra::session_backend::SessionBackend;
use zeroclaw_infra::session_sqlite::SqliteSessionBackend;
use zeroclaw_memory::MemoryBackendKind;

use crate::lifecycle_path::{
    PathPresence, inspect_lifecycle_path, same_existing_file, sync_directory,
};

/// How long [`arm`], and a rename discovered without a record, wait for the
/// journal lock. Callers hold their config write lock while arming, so a
/// contended journal fails fast rather than stalling every config write.
const ARM_LOCK_WAIT: Duration = Duration::from_millis(250);
/// How long a step that changes an existing record waits for the journal
/// lock: [`converge`] committing or clearing it, and [`abandon_rename`]
/// dropping it.
const RECORD_LOCK_WAIT: Duration = Duration::from_secs(5);
/// The store the journal's own read failures are reported under.
const JOURNAL_STORE: &str = "agent lifecycle recovery journal";

const KEY_ARMED: &str = "agents.rename_recovery.armed";
const KEY_RESUMED: &str = "agents.rename_recovery.resumed";
const KEY_CONVERGED: &str = "agents.rename_recovery.converged";
const KEY_INCOMPLETE: &str = "agents.rename_recovery.incomplete";
const KEY_REFUSED: &str = "agents.rename_recovery.refused";
const KEY_UNREADABLE: &str = "agents.rename_recovery.unreadable";
const KEY_RECORD_FAILED: &str = "agents.rename_recovery.record_failed";
const KEY_ABANDONED: &str = "agents.rename_recovery.abandoned";
const KEY_COMMIT_LANDED_ON_DISK: &str = "agents.rename_recovery.commit_landed_on_disk";
const KEY_ABANDON_KEPT_UNREADABLE_CONFIG: &str =
    "agents.rename_recovery.abandon_kept_unreadable_config";

/// Store handles a rename surface already holds. `None` means the surface
/// holds no handle for that store, not that the store is unconfigured: the
/// store is then resolved from the live config and what exists on disk.
#[derive(Clone, Copy)]
pub struct SurfaceStores<'a> {
    pub memory: Option<&'a Arc<dyn Memory>>,
    pub session_backend: Option<&'a Arc<dyn SessionBackend>>,
    pub acp: Option<&'a Arc<AcpSessionStore>>,
}

impl SurfaceStores<'_> {
    /// No handles: every store is resolved from the config and the disk.
    #[must_use]
    pub fn none() -> SurfaceStores<'static> {
        SurfaceStores {
            memory: None,
            session_backend: None,
            acp: None,
        }
    }
}

/// State that follows an agent's alias through a rename.
#[derive(Debug, Clone, Copy, PartialEq, Eq, serde::Serialize)]
#[serde(rename_all = "snake_case")]
pub enum FollowerKind {
    /// The default per-alias workspace directory.
    Workspace,
    /// Memory attribution.
    Memory,
    /// Cron jobs and their run-history ownership.
    Cron,
    /// ACP sessions and their saved working directories.
    Acp,
    /// Session attribution.
    Sessions,
}

impl FollowerKind {
    /// Every follower, in the order a rename moves them.
    const ALL: [Self; 5] = [
        Self::Workspace,
        Self::Memory,
        Self::Cron,
        Self::Acp,
        Self::Sessions,
    ];

    fn as_str(self) -> &'static str {
        match self {
            Self::Workspace => "workspace",
            Self::Memory => "memory",
            Self::Cron => "cron",
            Self::Acp => "acp",
            Self::Sessions => "sessions",
        }
    }
}

impl fmt::Display for FollowerKind {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        f.write_str(self.as_str())
    }
}

/// Why a follower has not converged.
#[derive(Debug, Clone, Copy, PartialEq, Eq, serde::Serialize)]
#[serde(rename_all = "snake_case")]
pub enum FollowerIssueKind {
    /// State is still attributed to the old alias.
    Lagging,
    /// The follower's store exists but could not be read, so whether state is
    /// still attributed to the old alias is unknown.
    Unreadable,
    /// The move needs an operator: the destination is already taken.
    Conflict,
}

/// A follower that has not converged. Its [`Display`](fmt::Display) is the
/// warning line a surface reports.
#[derive(Debug, Clone, serde::Serialize)]
pub struct FollowerIssue {
    pub follower: FollowerKind,
    pub kind: FollowerIssueKind,
    /// Why an unreadable follower could not be read; for a lagging or
    /// conflicting one, the whole warning line.
    pub detail: String,
}

impl FollowerIssue {
    fn lagging(follower: FollowerKind, detail: String) -> Self {
        Self {
            follower,
            kind: FollowerIssueKind::Lagging,
            detail,
        }
    }

    fn unreadable(follower: FollowerKind, detail: String) -> Self {
        Self {
            follower,
            kind: FollowerIssueKind::Unreadable,
            detail,
        }
    }

    fn conflict(follower: FollowerKind, detail: String) -> Self {
        Self {
            follower,
            kind: FollowerIssueKind::Conflict,
            detail,
        }
    }
}

impl fmt::Display for FollowerIssue {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        match self.kind {
            FollowerIssueKind::Unreadable => {
                write!(f, "{} could not be read: {}", self.follower, self.detail)
            }
            FollowerIssueKind::Lagging | FollowerIssueKind::Conflict => f.write_str(&self.detail),
        }
    }
}

/// What one [`converge`] moved.
#[derive(Debug, Default, Clone, serde::Serialize)]
pub struct ConvergeReport {
    pub workspace_moved: bool,
    pub memory_rows: usize,
    pub cron_jobs: usize,
    pub acp_sessions: usize,
    pub acp_workspaces: usize,
    pub sessions_repointed: usize,
}

/// How a [`converge`] ended.
#[derive(Debug)]
pub enum ConvergeOutcome {
    /// Nothing is left under the old alias and the recovery record is gone.
    Converged(ConvergeReport),
    /// Some follower still lags, could not be read, or conflicts. The record
    /// stays open, and re-running the same rename resumes it.
    Incomplete {
        report: ConvergeReport,
        outstanding: Vec<FollowerIssue>,
    },
}

impl ConvergeOutcome {
    #[must_use]
    pub fn report(&self) -> &ConvergeReport {
        match self {
            Self::Converged(report) | Self::Incomplete { report, .. } => report,
        }
    }

    #[must_use]
    pub fn is_converged(&self) -> bool {
        matches!(self, Self::Converged(_))
    }

    /// One warning line per outstanding follower; empty once converged.
    #[must_use]
    pub fn warnings(&self) -> Vec<String> {
        match self {
            Self::Converged(_) => Vec::new(),
            Self::Incomplete { outstanding, .. } => {
                outstanding.iter().map(ToString::to_string).collect()
            }
        }
    }
}

/// Why a rename recovery step refused or failed.
#[derive(Debug)]
pub enum RenameRecoveryError {
    /// An alias fails the alias rules, or both aliases are the same.
    InvalidAlias { alias: String, reason: String },
    /// An alias is the reserved `default` agent.
    ReservedAlias { alias: String },
    /// There is no agent to rename: the alias is not configured, no rename of
    /// it is recorded, and no state is left under it.
    NotConfigured { alias: String },
    /// An unfinished rename retired `alias`, the old alias of a rename to
    /// `pending_to`. Display leaves `pending_to` out, since surfaces render
    /// this error before they authorize the caller.
    AliasRetired { alias: String, pending_to: String },
    /// An unfinished rename of `from` is still converging into `to`. Display
    /// leaves `from` out, for the same reason.
    RecoveryPending { from: String, to: String },
    /// `from` is configured again while its rename to `to` is unfinished: an
    /// agent brought back around the create guards (a hand edit, say) would
    /// take over whatever the rename still owes `to`, so nothing moves until
    /// the operator removes it or abandons the rename. Display leaves `to`
    /// out: a request naming another target must not learn the pending one.
    SourceReconfigured { from: String, to: String },
    /// A store could not be read, so whether a rename is unfinished, or state
    /// is left under an alias, is unknown.
    Unreadable { store: String, detail: String },
    /// Another process holds the recovery journal lock.
    Busy { detail: String },
    /// The recovery journal could not be written.
    Persist { detail: String },
}

impl fmt::Display for RenameRecoveryError {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        match self {
            Self::InvalidAlias { alias, reason } => {
                write!(f, "invalid agent alias `{alias}`: {reason}")
            }
            Self::ReservedAlias { alias } => {
                write!(f, "alias `{alias}` is reserved and cannot be renamed")
            }
            Self::NotConfigured { alias } => write!(f, "agents.{alias} is not configured"),
            Self::AliasRetired { alias, .. } => write!(
                f,
                "alias `{alias}` is retired by an unfinished agent rename and cannot be reused yet"
            ),
            Self::RecoveryPending { to, .. } => write!(
                f,
                "agent `{to}` is the target of an unfinished rename; re-run that rename first"
            ),
            Self::SourceReconfigured { from, .. } => write!(
                f,
                "agent `{from}` is configured again while an earlier rename of it is unfinished; remove `[agents.{from}]` from the config by hand, or abandon that rename, then retry"
            ),
            Self::Unreadable { store, detail } => write!(f, "{store} could not be read: {detail}"),
            Self::Busy { detail } => write!(
                f,
                "agent rename recovery is in progress elsewhere; retry shortly ({detail})"
            ),
            Self::Persist { detail } => {
                write!(f, "agent rename recovery could not be recorded: {detail}")
            }
        }
    }
}

impl std::error::Error for RenameRecoveryError {}

/// What [`abandon_rename`] dropped, and what it left behind.
#[derive(Debug)]
pub struct AbandonedRename {
    /// The record that was dropped.
    pub record: RecoveryRecord,
    /// State still kept under the old alias, which an agent created under it
    /// adopts, and every store that could not be read to tell.
    pub residue: Vec<FollowerIssue>,
}

/// What [`resolve`] decided a rename is.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum Disposition {
    /// The old alias is configured: arm, commit, then converge.
    Fresh,
    /// The config commit already landed and a record now covers the rename:
    /// converge without committing again.
    Resume,
}

/// A rename recorded before its config commit. It holds the journal lock
/// until [`acknowledge_commit`] or [`abandon`] consumes it; dropping it
/// instead releases the lock and leaves the prepared record, whose effect the
/// live config then decides.
#[derive(Debug)]
pub struct Armed {
    record: RecoveryRecord,
    guard: JournalGuard,
}

impl Armed {
    #[must_use]
    pub fn from(&self) -> &str {
        &self.record.from
    }

    #[must_use]
    pub fn to(&self) -> &str {
        &self.record.to
    }
}

/// Decide whether renaming `from` to `to` is a fresh rename or resumes an
/// unfinished one.
///
/// An open record decides first: this rename's own record resumes it, unless
/// `from` is configured again, and a record of another rename that retired
/// either alias, or still converges into one of them, refuses it. Otherwise a
/// configured `from` is fresh. A `from` that is gone while `to` is configured
/// may be a rename committed without a record (by an older build, say): the
/// followers are probed, and any state left under `from`, or a store that
/// cannot be read, records the rename as committed so it resumes and `from`
/// stays retired until it converges.
pub async fn resolve(
    config: &Config,
    from: &str,
    to: &str,
    stores: &SurfaceStores<'_>,
) -> Result<Disposition, RenameRecoveryError> {
    validate_rename(from, to)?;
    let journal = AgentRecoveryJournal::for_config(config);
    let records = load_records(config, &journal)
        .await
        .inspect_err(|e| log_refusal(from, to, e))?;
    validate_source(config, &records, from)?;
    match match_records(&records, config, from, to) {
        RecordMatch::Refused(error) => {
            log_refusal(from, to, &error);
            return Err(error);
        }
        RecordMatch::Pending(_) => {
            log_resumed(from, to, false);
            return Ok(Disposition::Resume);
        }
        RecordMatch::Clear => {}
    }
    drop(records);
    if config.agent(from).is_some() {
        return Ok(Disposition::Fresh);
    }
    if config.agent(to).is_none() {
        return Err(RenameRecoveryError::NotConfigured {
            alias: from.to_string(),
        });
    }
    discover(config, &journal, from, to, stores).await
}

/// Record the rename of `from` to `to` before its config commit, and hold the
/// journal lock so no other lifecycle operation writes the journal until the
/// surface has committed and called [`acknowledge_commit`] or [`abandon`].
///
/// `config_before_commit` is the config the commit starts from, and must not
/// configure `to` yet: a rename onto a configured alias is refused. The
/// record keeps `from`'s workspace for the move only when `from` sets no
/// `workspace.path` and so follows its alias; a custom path never moves.
pub async fn arm(
    config_before_commit: &Config,
    from: &str,
    to: &str,
) -> Result<Armed, RenameRecoveryError> {
    validate_rename(from, to)?;
    let journal = AgentRecoveryJournal::for_config(config_before_commit);
    // Checked before the lock, which creates the data directory and its lock
    // file, so a request that fails it leaves nothing behind.
    let records = load_records(config_before_commit, &journal)
        .await
        .inspect_err(|e| log_refusal(from, to, e))?;
    validate_source(config_before_commit, &records, from)?;
    drop(records);
    match prepare_record(journal, config_before_commit, from, to).await {
        Ok(armed) => {
            ::zeroclaw_log::record!(
                INFO,
                ::zeroclaw_log::Event::new(module_path!(), ::zeroclaw_log::Action::Note)
                    .with_attrs(::serde_json::json!({
                        "error_key": KEY_ARMED,
                        "from": from,
                        "to": to,
                        "moves_workspace": armed.record.source_workspace.is_some(),
                    })),
                "agent rename recovery armed before the config commit"
            );
            Ok(armed)
        }
        Err(error) => {
            log_refusal(from, to, &error);
            Err(error)
        }
    }
}

/// The surface's config commit landed: mark the record committed and release
/// the journal lock. Best effort: a failure is logged, and the prepared record
/// still covers the rename, since the live config shows the commit.
///
/// `config_after_commit` must show the commit, with `from` gone. Otherwise
/// the record is left prepared, and void while the live config still has
/// `from` and lacks `to`, rather than retiring an alias that is still
/// configured.
pub async fn acknowledge_commit(config_after_commit: &Config, armed: Armed) {
    let Armed { mut record, guard } = armed;
    let (from, to) = (record.from.clone(), record.to.clone());
    if !record.is_effective(config_after_commit) {
        log_record_failed(
            &from,
            &to,
            "the config does not show the rename committed; the record stays prepared",
        );
        return;
    }
    record.phase = RecoveryPhase::Committed;
    let journal = AgentRecoveryJournal::for_config(config_after_commit);
    let written = tokio::task::spawn_blocking(move || {
        let written = journal.upsert(&guard, record);
        drop(guard);
        written
    })
    .await;
    match written {
        Ok(Ok(())) => {}
        Ok(Err(e)) => log_record_failed(&from, &to, &e.to_string()),
        Err(e) => log_record_failed(&from, &to, &e.to_string()),
    }
}

/// The surface's config commit failed: drop the prepared record and release
/// the journal lock. Best effort: a failure is logged, and the record, whose
/// commit never landed, retires nothing while the config keeps `from` and
/// lacks `to`, and is dropped by the next [`arm`].
///
/// A `config` that does not prove the commit failed, one without `from` or
/// with both aliases, keeps the record, so the rename's recovery is not
/// lost.
///
/// `config` alone cannot prove it either when the surface saves the config
/// and then publishes it: a publication that fails after the save leaves the
/// live config without the rename while the config file already holds it. So
/// the file at `config.config_path` decides too. A file that no longer has
/// `from` but has `to` holds the commit: the record is kept and marked
/// committed, which no config voids, so `from` stays retired once the file
/// is loaded again. A file that cannot be read or parsed keeps the record as
/// it is. Only a file that still has `from`, or lacks `to`, drops it.
pub async fn abandon(config: &Config, armed: Armed) {
    let Armed { record, guard } = armed;
    let (from, to) = (record.from.clone(), record.to.clone());
    if record.is_effective(config) {
        log_record_failed(
            &from,
            &to,
            "the config shows the rename committed; the record is kept",
        );
        return;
    }
    let journal = AgentRecoveryJournal::for_config(config);
    let config_path = config.config_path.clone();
    // The file is read while the lock is still held, so no other lifecycle
    // operation can collect the record before it is settled.
    let settled = tokio::task::spawn_blocking(move || {
        let on_disk = commit_on_disk(&config_path, &record.from, &record.to);
        let written = match &on_disk {
            CommitOnDisk::Absent => journal
                .remove(&guard, RecoveryOperation::Rename, &record.from)
                .map(|_| ()),
            CommitOnDisk::Landed => journal.upsert(
                &guard,
                RecoveryRecord {
                    phase: RecoveryPhase::Committed,
                    ..record
                },
            ),
            CommitOnDisk::Unknown(_) => Ok(()),
        };
        drop(guard);
        (on_disk, written)
    })
    .await;
    let (on_disk, written) = match settled {
        Ok(settled) => settled,
        Err(e) => {
            log_record_failed(&from, &to, &e.to_string());
            return;
        }
    };
    match on_disk {
        CommitOnDisk::Absent => {}
        CommitOnDisk::Landed => {
            ::zeroclaw_log::record!(
                WARN,
                ::zeroclaw_log::Event::new(module_path!(), ::zeroclaw_log::Action::Note)
                    .with_outcome(::zeroclaw_log::EventOutcome::Failure)
                    .with_attrs(::serde_json::json!({
                        "error_key": KEY_COMMIT_LANDED_ON_DISK,
                        "from": from,
                        "to": to,
                        "config_path": config.config_path.display().to_string(),
                        "marked_committed": written.is_ok(),
                    })),
                "agent rename failed after its config save landed; its record is kept, so the old alias stays retired once the config is reloaded; re-run the same rename then to finish it"
            );
        }
        CommitOnDisk::Unknown(detail) => {
            ::zeroclaw_log::record!(
                WARN,
                ::zeroclaw_log::Event::new(module_path!(), ::zeroclaw_log::Action::Note)
                    .with_outcome(::zeroclaw_log::EventOutcome::Unknown)
                    .with_attrs(::serde_json::json!({
                        "error_key": KEY_ABANDON_KEPT_UNREADABLE_CONFIG,
                        "from": from,
                        "to": to,
                        "config_path": config.config_path.display().to_string(),
                        "error": detail,
                    })),
                "agent rename failed and the config file could not be read to tell whether its save landed; the prepared record is kept"
            );
        }
    }
    if let Err(e) = written {
        log_record_failed(&from, &to, &e.to_string());
    }
}

/// Move every follower of the committed rename of `from` to `to`, re-check
/// them all, and clear the rename's record once nothing is left under `from`.
///
/// Every follower is attempted even when an earlier one fails, and each moves
/// only when its probe finds state under `from`, so a re-run repeats nothing.
/// The re-check decides the outcome: a follower whose move failed but that
/// holds nothing under `from` has converged. A converge without a record is
/// allowed, for a rename discovered or committed without one, but only once
/// `from` is no longer configured; a recorded rename whose `from` is
/// configured again moves nothing. A prepared record is marked committed
/// before anything moves. The journal lock is taken only to update the
/// record, never across follower I/O.
pub async fn converge(
    config: &Config,
    from: &str,
    to: &str,
    stores: &SurfaceStores<'_>,
) -> Result<ConvergeOutcome, RenameRecoveryError> {
    validate_rename(from, to)?;
    let journal = AgentRecoveryJournal::for_config(config);
    let (record, recorded) = {
        let records = load_records(config, &journal)
            .await
            .inspect_err(|e| log_refusal(from, to, e))?;
        validate_source(config, &records, from)?;
        let recorded = records.iter().any(|r| is_rename_record(r, from, to));
        match match_records(&records, config, from, to) {
            RecordMatch::Refused(error) => {
                log_refusal(from, to, &error);
                return Err(error);
            }
            RecordMatch::Pending(record) => (Some(record.clone()), recorded),
            RecordMatch::Clear => (None, recorded),
        }
    };
    if record.is_none() && config.agent(from).is_some() {
        return Err(RenameRecoveryError::InvalidAlias {
            alias: from.to_string(),
            reason: format!(
                "agents.{from} is still configured and no committed rename of it is recorded"
            ),
        });
    }
    // Whether a prepared record retires `from` hangs on the config. Once
    // state starts moving that must no longer be so, so the record is
    // committed first.
    let record = match record {
        Some(record) if record.phase == RecoveryPhase::Prepared => Some(
            promote_record(&journal, record)
                .await
                .inspect_err(|e| log_record_failed(from, to, &e.to_string()))?,
        ),
        record => record,
    };

    let plan = FollowerPlan::resolve(config, from, to, record.as_ref(), stores).await;
    let mut report = ConvergeReport::default();
    let attempted = plan.act_all(config, from, to, &mut report).await;
    let outstanding = plan.verify(config, from, to, attempted).await;

    if !outstanding.is_empty() {
        let warnings: Vec<String> = outstanding.iter().map(ToString::to_string).collect();
        ::zeroclaw_log::record!(
            WARN,
            ::zeroclaw_log::Event::new(module_path!(), ::zeroclaw_log::Action::Note)
                .with_outcome(::zeroclaw_log::EventOutcome::Failure)
                .with_attrs(::serde_json::json!({
                    "error_key": KEY_INCOMPLETE,
                    "from": from,
                    "to": to,
                    "report": &report,
                    "warnings": warnings,
                })),
            "agent rename has not converged; re-run the same rename to finish it"
        );
        return Ok(ConvergeOutcome::Incomplete {
            report,
            outstanding,
        });
    }
    if recorded {
        // Another process may have cleared the record since it was read; a
        // record that is already gone is not an error.
        remove_record(&journal, from, to).await?;
    }
    ::zeroclaw_log::record!(
        INFO,
        ::zeroclaw_log::Event::new(module_path!(), ::zeroclaw_log::Action::Note)
            .with_outcome(::zeroclaw_log::EventOutcome::Success)
            .with_attrs(::serde_json::json!({
                "error_key": KEY_CONVERGED,
                "from": from,
                "to": to,
                "report": &report,
            })),
        "agent rename converged; nothing is left under the old alias"
    );
    Ok(ConvergeOutcome::Converged(report))
}

/// Drop the recovery record of the unfinished rename of `from` to `to`
/// without moving anything: the operator's way out of a rename that cannot
/// finish. The followers are probed but never acted on, and neither the
/// config nor any store changes.
///
/// Once the record is gone `from` can be created again, and an agent created
/// under it adopts whatever state is still kept under it. `residue` lists
/// that state, and every store that could not be read, so the operator can
/// check it first. Only a record of exactly this rename is dropped, and it is
/// dropped even while `from` is configured again.
pub async fn abandon_rename(
    config: &Config,
    from: &str,
    to: &str,
) -> Result<AbandonedRename, RenameRecoveryError> {
    validate_rename(from, to)?;
    let journal = AgentRecoveryJournal::for_config(config);
    let records = load_records(config, &journal)
        .await
        .inspect_err(|e| log_refusal(from, to, e))?;
    validate_source(config, &records, from)?;
    let not_recorded = || RenameRecoveryError::NotConfigured {
        alias: from.to_string(),
    };
    if !records.iter().any(|r| is_rename_record(r, from, to)) {
        return Err(not_recorded());
    }
    drop(records);

    // Probed as if no record covered the rename: an agent created under
    // `from` adopts whatever is at its default workspace, wherever the record
    // said the workspace was.
    let plan = FollowerPlan::resolve(config, from, to, None, &SurfaceStores::none()).await;
    let residue = plan.residue(config, from, to).await;
    let record = remove_record(&journal, from, to)
        .await?
        .ok_or_else(not_recorded)?;
    let warnings: Vec<String> = residue.iter().map(ToString::to_string).collect();
    ::zeroclaw_log::record!(
        WARN,
        ::zeroclaw_log::Event::new(module_path!(), ::zeroclaw_log::Action::Note).with_attrs(
            ::serde_json::json!({
                "error_key": KEY_ABANDONED,
                "from": from,
                "to": to,
                "residue": warnings,
            })
        ),
        "agent rename abandoned; state still kept under the old alias stays there, and an agent created under it adopts that state"
    );
    Ok(AbandonedRename { record, residue })
}

/// Refuse an `alias` that an unfinished rename retired. Surfaces that bring
/// an agent alias into existence call this. It only compares `alias` against
/// the journal, read from its fixed path, so it applies no alias rules.
pub async fn ensure_alias_not_retired(
    config: &Config,
    alias: &str,
) -> Result<(), RenameRecoveryError> {
    match agent_recovery_journal::retired_alias(config, alias) {
        Ok(None) => Ok(()),
        Ok(Some(record)) => {
            let error = RenameRecoveryError::AliasRetired {
                alias: alias.to_string(),
                pending_to: record.to.clone(),
            };
            log_refusal(&record.from, &record.to, &error);
            Err(error)
        }
        Err(e) => {
            let error = journal_error(e);
            log_refusal(alias, "", &error);
            Err(error)
        }
    }
}

/// Refuse an `alias` that an unfinished rename is still converging into.
/// Surfaces that delete or rename an agent call this, so the pending rename's
/// state is not stranded under an alias that no longer exists.
pub async fn ensure_not_pending_target(
    config: &Config,
    alias: &str,
) -> Result<(), RenameRecoveryError> {
    match agent_recovery_journal::pending_target(config, alias) {
        Ok(None) => Ok(()),
        Ok(Some(record)) => {
            let error = RenameRecoveryError::RecoveryPending {
                from: record.from.clone(),
                to: record.to.clone(),
            };
            log_refusal(&record.from, &record.to, &error);
            Err(error)
        }
        Err(e) => {
            let error = journal_error(e);
            log_refusal("", alias, &error);
            Err(error)
        }
    }
}

/// The alias checks every entry point makes before it reads the journal or
/// touches the filesystem. `to` names a new directory under
/// `<install>/agents`, so it must satisfy the alias grammar, and `from` must
/// at least name a single directory there. Neither alias may be the reserved
/// one, and they must differ. Whether `from` must satisfy the grammar as well
/// depends on the journal: see [`validate_source`].
fn validate_rename(from: &str, to: &str) -> Result<(), RenameRecoveryError> {
    require_alias_grammar(to)?;
    require_single_component(from)?;
    for alias in [from, to] {
        if zeroclaw_config::alias_refs::is_reserved_agent_alias(alias) {
            return Err(RenameRecoveryError::ReservedAlias {
                alias: alias.to_string(),
            });
        }
    }
    if from == to {
        return Err(RenameRecoveryError::InvalidAlias {
            alias: to.to_string(),
            reason: "new alias must differ from the current name".to_string(),
        });
    }
    Ok(())
}

/// Hold `from` to the alias grammar unless this install already names it: a
/// configured alias, or the old alias of a record in `records`. Config keys
/// and journal records were written by this code or by a config older than
/// the grammar, and renaming such a legacy alias to a valid one is how an
/// operator migrates it. Only a `from` that nothing names comes straight from
/// the request into path derivation, so only that one must satisfy the
/// grammar; a legacy one still passed [`require_single_component`] in
/// [`validate_rename`]. The journal is read from its fixed path, which
/// derives nothing from `from`.
fn validate_source(
    config: &Config,
    records: &[RecoveryRecord],
    from: &str,
) -> Result<(), RenameRecoveryError> {
    if config.agent(from).is_some() || records.iter().any(|record| record.from == from) {
        return Ok(());
    }
    require_alias_grammar(from)
}

fn require_alias_grammar(alias: &str) -> Result<(), RenameRecoveryError> {
    zeroclaw_config::helpers::validate_alias_key(alias).map_err(|reason| {
        RenameRecoveryError::InvalidAlias {
            alias: alias.to_string(),
            reason,
        }
    })
}

/// Refuse an alias that would not name exactly one directory under
/// `<install>/agents`. The alias grammar rules all of these out; a legacy
/// alias exempt from the grammar is still held to this, since every follower
/// path is derived from it.
fn require_single_component(alias: &str) -> Result<(), RenameRecoveryError> {
    let mut components = Path::new(alias).components();
    let reason = if alias.is_empty() {
        "alias must not be empty"
    } else if alias.contains(['/', '\\', '\0']) {
        "alias must not contain a path separator or NUL"
    } else if alias == "." || alias == ".." {
        "alias must not be `.` or `..`"
    } else if !matches!(
        (components.next(), components.next()),
        (Some(Component::Normal(_)), None)
    ) {
        "alias must name a single directory"
    } else {
        return Ok(());
    };
    Err(RenameRecoveryError::InvalidAlias {
        alias: alias.to_string(),
        reason: reason.to_string(),
    })
}

/// Every journal record, read off the async workers. A config without a data
/// directory has no journal.
async fn load_records(
    config: &Config,
    journal: &AgentRecoveryJournal,
) -> Result<Vec<RecoveryRecord>, RenameRecoveryError> {
    if config.data_dir.as_os_str().is_empty() {
        return Ok(Vec::new());
    }
    let journal = journal.clone();
    match tokio::task::spawn_blocking(move || journal.load()).await {
        Ok(loaded) => loaded.map_err(journal_error),
        Err(e) => Err(RenameRecoveryError::Unreadable {
            store: JOURNAL_STORE.to_string(),
            detail: e.to_string(),
        }),
    }
}

/// Run `op` on a blocking thread and wait for it. Journal writes are file
/// I/O, and a lock wait sleeps its thread, so neither runs on an async
/// worker.
async fn off_async_worker<T, F>(op: F) -> Result<T, RenameRecoveryError>
where
    T: Send + 'static,
    F: FnOnce() -> Result<T, RenameRecoveryError> + Send + 'static,
{
    match tokio::task::spawn_blocking(op).await {
        Ok(result) => result,
        Err(e) => Err(RenameRecoveryError::Persist {
            detail: e.to_string(),
        }),
    }
}

fn journal_error(error: JournalError) -> RenameRecoveryError {
    match error {
        JournalError::Busy { .. } => RenameRecoveryError::Busy {
            detail: error.to_string(),
        },
        JournalError::Write { .. } => RenameRecoveryError::Persist {
            detail: error.to_string(),
        },
        JournalError::Unreadable { .. } | JournalError::UnsupportedSchema { .. } => {
            RenameRecoveryError::Unreadable {
                store: JOURNAL_STORE.to_string(),
                detail: error.to_string(),
            }
        }
    }
}

fn is_rename_record(record: &RecoveryRecord, from: &str, to: &str) -> bool {
    record.operation == RecoveryOperation::Rename && record.from == from && record.to == to
}

/// How the effective journal records bear on renaming `from` to `to`.
enum RecordMatch<'r> {
    /// No effective record concerns either alias.
    Clear,
    /// The effective record of this very rename.
    Pending(&'r RecoveryRecord),
    /// An effective record of another rename forbids this one.
    Refused(RenameRecoveryError),
}

/// Match `records` against renaming `from` to `to`, in precedence order:
/// `from` configured again while a rename of it is unfinished; `to` retired
/// by another rename; `from` still receiving another rename's state; `from`
/// retired, by this rename (pending) or another; `to` still receiving another
/// rename's state. Only effective records count: a void prepared record,
/// whose `from` is still configured and whose `to` is not, retires nothing.
fn match_records<'r>(
    records: &'r [RecoveryRecord],
    config: &Config,
    from: &str,
    to: &str,
) -> RecordMatch<'r> {
    let effective = || records.iter().filter(|r| r.is_effective(config));
    // Resuming would move the rename's remaining state out from under an
    // agent that is live under `from` again.
    if config.agent(from).is_some()
        && let Some(record) = effective().find(|r| r.from == from)
    {
        return RecordMatch::Refused(RenameRecoveryError::SourceReconfigured {
            from: from.to_string(),
            to: record.to.clone(),
        });
    }
    if let Some(record) = effective().find(|r| r.from == to) {
        return RecordMatch::Refused(RenameRecoveryError::AliasRetired {
            alias: to.to_string(),
            pending_to: record.to.clone(),
        });
    }
    if let Some(record) = effective().find(|r| r.to == from && r.from != from) {
        return RecordMatch::Refused(RenameRecoveryError::RecoveryPending {
            from: record.from.clone(),
            to: record.to.clone(),
        });
    }
    if let Some(record) = effective().find(|r| r.from == from) {
        if is_rename_record(record, from, to) {
            return RecordMatch::Pending(record);
        }
        return RecordMatch::Refused(RenameRecoveryError::AliasRetired {
            alias: from.to_string(),
            pending_to: record.to.clone(),
        });
    }
    if let Some(record) = effective().find(|r| r.to == to) {
        return RecordMatch::Refused(RenameRecoveryError::RecoveryPending {
            from: record.from.clone(),
            to: record.to.clone(),
        });
    }
    RecordMatch::Clear
}

/// Take the journal lock and, holding it, drop void records, re-check `from`
/// and that no effective record forbids the rename against the journal as it
/// now stands (another process may have changed it since it was last read),
/// and record the rename as prepared. The lock stays held only once the
/// rename is armed.
async fn prepare_record(
    journal: AgentRecoveryJournal,
    config: &Config,
    from: &str,
    to: &str,
) -> Result<Armed, RenameRecoveryError> {
    let config = Box::new(config.clone());
    let (from, to) = (from.to_string(), to.to_string());
    off_async_worker(move || {
        let guard = journal.lock(ARM_LOCK_WAIT).map_err(journal_error)?;
        // Holding the lock, no other operation sits between its prepare and
        // its commit, so a prepared record whose `from` is still configured
        // and whose `to` is not never committed and is void. One whose `to`
        // has appeared as well is ambiguous and stays.
        journal
            .collect_void(&guard, &config)
            .map_err(journal_error)?;
        let records = journal.load().map_err(journal_error)?;
        validate_source(&config, &records, &from)?;
        match match_records(&records, &config, &from, &to) {
            RecordMatch::Clear => {}
            RecordMatch::Pending(record) => {
                return Err(RenameRecoveryError::RecoveryPending {
                    from: record.from.clone(),
                    to: record.to.clone(),
                });
            }
            RecordMatch::Refused(error) => return Err(error),
        }
        // A prepared record stands before its commit only because it is void
        // while `from` is configured and `to` is not. A rename onto an alias
        // that is already configured never commits, so it is refused rather
        // than recorded as an ambiguous record that would retire `from`.
        if config.agent(&to).is_some() {
            return Err(RenameRecoveryError::InvalidAlias {
                alias: to.clone(),
                reason: format!("agents.{to} is already configured"),
            });
        }
        let source_workspace = armed_source_workspace(&config, &from);
        let record = new_record(&from, &to, RecoveryPhase::Prepared, source_workspace);
        journal
            .upsert(&guard, record.clone())
            .map_err(journal_error)?;
        Ok(Armed { record, guard })
    })
    .await
}

/// The workspace a rename armed from `config` moves: `from`'s default
/// location, but only when `from` sets no `workspace.path` and so follows its
/// alias. An explicit path stays where the operator put it, even one naming
/// the default location.
fn armed_source_workspace(config: &Config, from: &str) -> Option<PathBuf> {
    let explicit = config
        .agent(from)
        .and_then(|agent| agent.workspace.path.as_ref())
        .is_some();
    let default = config.default_agent_workspace_dir(from);
    (!explicit && config.agent_workspace_dir(from) == default).then_some(default)
}

fn new_record(
    from: &str,
    to: &str,
    phase: RecoveryPhase,
    source_workspace: Option<PathBuf>,
) -> RecoveryRecord {
    RecoveryRecord {
        operation: RecoveryOperation::Rename,
        from: from.to_string(),
        to: to.to_string(),
        phase,
        source_workspace,
        armed_at: chrono::Utc::now().to_rfc3339(),
    }
}

/// Whether a rename's config commit is in the config file, as [`abandon`]
/// reads it.
#[derive(Debug, PartialEq, Eq)]
enum CommitOnDisk {
    /// The `agents` table no longer has `from` and has `to`: the save landed.
    Landed,
    /// The file still has `from`, or lacks `to`: the save never landed.
    Absent,
    /// The file could not be read or parsed, so whether the save landed is
    /// unknown. Carries why.
    Unknown(String),
}

/// Read whether the rename of `from` to `to` landed in the config file at
/// `path`. Only the TOML is parsed: no migration, environment override, or
/// secret handling runs, and the aliases are looked up among the parsed keys
/// of the `agents` table, so a quoted key matches the same as a bare one.
/// The read is blocking file I/O.
fn commit_on_disk(path: &Path, from: &str, to: &str) -> CommitOnDisk {
    let text = match std::fs::read_to_string(path) {
        Ok(text) => text,
        Err(e) => return CommitOnDisk::Unknown(e.to_string()),
    };
    let file = match text.parse::<toml::Table>() {
        Ok(file) => file,
        // The message alone: the full error quotes the offending line, which
        // may hold a secret.
        Err(e) => return CommitOnDisk::Unknown(e.message().to_string()),
    };
    let agents = match file.get("agents") {
        None => return CommitOnDisk::Absent,
        Some(toml::Value::Table(agents)) => agents,
        Some(_) => return CommitOnDisk::Unknown("`agents` is not a table".to_string()),
    };
    if !agents.contains_key(from) && agents.contains_key(to) {
        CommitOnDisk::Landed
    } else {
        CommitOnDisk::Absent
    }
}

/// `config` no longer has `from` but has `to`, and no record covers the
/// rename: probe the followers without acting. State left under `from`, or a
/// store that cannot be read, records the rename as committed, so `from`
/// stays retired and the rename resumes; nothing at all means there was no
/// such agent.
async fn discover(
    config: &Config,
    journal: &AgentRecoveryJournal,
    from: &str,
    to: &str,
    stores: &SurfaceStores<'_>,
) -> Result<Disposition, RenameRecoveryError> {
    let plan = FollowerPlan::resolve(config, from, to, None, stores).await;
    let mut residue = 0usize;
    let mut unreadable = None;
    for follower in FollowerKind::ALL {
        match plan.probe(follower, config, from).await {
            Ok(count) => residue += count,
            Err(issue) => {
                unreadable.get_or_insert(issue);
            }
        }
    }
    if unreadable.is_none() && residue == 0 {
        return Err(RenameRecoveryError::NotConfigured {
            alias: from.to_string(),
        });
    }

    let disposition = record_discovered(config, journal, from, to)
        .await
        .inspect_err(|e| log_refusal(from, to, e))?;
    if let Some(issue) = unreadable {
        let error = RenameRecoveryError::Unreadable {
            store: issue.follower.to_string(),
            detail: issue.detail,
        };
        log_refusal(from, to, &error);
        return Err(error);
    }
    log_resumed(from, to, true);
    Ok(disposition)
}

/// Record a rename found committed without a record. Another process may
/// have recorded it, or a rename that forbids it, since [`resolve`] read the
/// journal; under the lock that record decides instead. `from` is re-checked
/// against the journal as it now stands, since a record that named it may
/// have been cleared meanwhile.
async fn record_discovered(
    config: &Config,
    journal: &AgentRecoveryJournal,
    from: &str,
    to: &str,
) -> Result<Disposition, RenameRecoveryError> {
    // Only a `to` at the alias-derived location takes the old workspace.
    let default_to = config.default_agent_workspace_dir(to);
    let source_workspace = (lexically_normalized(&config.agent_workspace_dir(to))
        == lexically_normalized(&default_to))
    .then(|| config.default_agent_workspace_dir(from));
    let journal = journal.clone();
    let config = Box::new(config.clone());
    let (from, to) = (from.to_string(), to.to_string());
    off_async_worker(move || {
        let guard = journal.lock(ARM_LOCK_WAIT).map_err(journal_error)?;
        let records = journal.load().map_err(journal_error)?;
        validate_source(&config, &records, &from)?;
        match match_records(&records, &config, &from, &to) {
            RecordMatch::Pending(_) => return Ok(Disposition::Resume),
            RecordMatch::Refused(error) => return Err(error),
            RecordMatch::Clear => {}
        }
        journal
            .upsert(
                &guard,
                new_record(&from, &to, RecoveryPhase::Committed, source_workspace),
            )
            .map_err(journal_error)?;
        Ok(Disposition::Resume)
    })
    .await
}

/// Mark the prepared record of this rename committed, holding the journal
/// lock. Another process may have committed, replaced, or cleared it since
/// it was read, so only a record that still names this rename and is still
/// prepared is written. Returns the record as the journal now holds it, or as
/// it was read when the journal no longer names this rename.
async fn promote_record(
    journal: &AgentRecoveryJournal,
    record: RecoveryRecord,
) -> Result<RecoveryRecord, RenameRecoveryError> {
    let journal = journal.clone();
    off_async_worker(move || {
        let guard = journal.lock(RECORD_LOCK_WAIT).map_err(journal_error)?;
        let current = journal
            .load()
            .map_err(journal_error)?
            .into_iter()
            .find(|r| is_rename_record(r, &record.from, &record.to));
        let Some(current) = current else {
            return Ok(record);
        };
        if current.phase == RecoveryPhase::Committed {
            return Ok(current);
        }
        let committed = RecoveryRecord {
            phase: RecoveryPhase::Committed,
            ..current
        };
        journal
            .upsert(&guard, committed.clone())
            .map_err(journal_error)?;
        Ok(committed)
    })
    .await
}

/// Drop the record of this rename, holding the journal lock, and return it.
/// Another process may have replaced or cleared it since it was read, so it
/// is removed only while it still names this rename; `None` when it no
/// longer does.
async fn remove_record(
    journal: &AgentRecoveryJournal,
    from: &str,
    to: &str,
) -> Result<Option<RecoveryRecord>, RenameRecoveryError> {
    let journal = journal.clone();
    let (from, to) = (from.to_string(), to.to_string());
    off_async_worker(move || {
        let guard = journal.lock(RECORD_LOCK_WAIT).map_err(journal_error)?;
        let current = journal
            .load()
            .map_err(journal_error)?
            .into_iter()
            .find(|r| is_rename_record(r, &from, &to));
        if current.is_some() {
            journal
                .remove(&guard, RecoveryOperation::Rename, &from)
                .map_err(journal_error)?;
        }
        Ok(current)
    })
    .await
}

/// A follower's store as resolved for one rename.
enum Slot<T> {
    /// The store does not exist or keeps no per-agent state: nothing moves.
    OutOfScope,
    /// The store exists but could not be inspected or opened. Carries why.
    Unreadable(String),
    /// The store, open.
    Open(T),
}

/// Where the old alias's workspace moves from and to.
struct WorkspaceMove {
    source: PathBuf,
    destination: PathBuf,
    /// The two aliases differ only in letter case, so on a case-insensitive
    /// filesystem `source` and `destination` are one directory.
    case_only: bool,
}

/// What the workspace follower does for one rename. It always watches the
/// old alias's default workspace, where an agent re-created under the old
/// alias would find its workspace: the record never clears while anything is
/// there, whatever the new alias's workspace configuration.
enum WorkspaceFollower {
    /// The workspace moves from the old alias's default location to the new
    /// alias's.
    Move(WorkspaceMove),
    /// Nothing moves: the new alias keeps a custom path, or the record kept
    /// no workspace because the old alias had one. An empty directory left
    /// at the old default location is removed; anything else there is the
    /// operator's to move.
    Leftover(PathBuf),
    /// The new alias's custom workspace overlaps the old default location: it
    /// is that location, lies beneath it, or contains it.
    TargetUsesOld,
    /// The recorded workspace is not the old default location under this
    /// config: the install moved, or the journal was edited.
    RecordMismatch(PathBuf),
    /// Whether the new alias's custom workspace overlaps the old default
    /// location could not be established. Carries why.
    Uninspectable(String),
}

impl WorkspaceFollower {
    /// 1 while the old default location holds anything, or the new alias's
    /// configuration still points at it, and 0 once neither is so. An error
    /// carries why that could not be established.
    async fn residue(&self) -> Result<usize, String> {
        match self {
            Self::Move(workspace) => match inspect_lifecycle_path(&workspace.source).await {
                PathPresence::Absent => Ok(0),
                PathPresence::Uninspectable(reason) => Err(reason),
                // A case-insensitive filesystem gives aliases that differ only
                // in case one directory, which is then already where it
                // belongs. Any other way of sharing it, a symlink say, is
                // still residue: the old alias would share it once re-created.
                PathPresence::Present if workspace.case_only => {
                    match same_place(&workspace.source, &workspace.destination).await {
                        Ok(same) => Ok(usize::from(!same)),
                        Err(e) => Err(format!(
                            "cannot inspect {}: {e}",
                            workspace.destination.display()
                        )),
                    }
                }
                PathPresence::Present => Ok(1),
            },
            Self::Leftover(old) => match inspect_lifecycle_path(old).await {
                PathPresence::Present => Ok(1),
                PathPresence::Absent => Ok(0),
                PathPresence::Uninspectable(reason) => Err(reason),
            },
            Self::TargetUsesOld | Self::RecordMismatch(_) => Ok(1),
            Self::Uninspectable(reason) => Err(reason.clone()),
        }
    }

    /// The warning line for a workspace follower that still holds residue
    /// when nothing more specific is known.
    fn residue_detail(&self, from: &str, to: &str) -> String {
        match self {
            Self::Move(WorkspaceMove { source: old, .. }) | Self::Leftover(old) => format!(
                "the old default workspace of `{from}` still exists at {}",
                old.display()
            ),
            Self::TargetUsesOld => format!(
                "the workspace.path of agent `{to}` is or overlaps the old default workspace of `{from}`"
            ),
            Self::RecordMismatch(recorded) => format!(
                "the recorded workspace {} is not the default workspace of `{from}` under this config",
                recorded.display()
            ),
            Self::Uninspectable(reason) => reason.clone(),
        }
    }
}

/// Where each follower's state lives for one rename, resolved once from the
/// live config, what exists on disk, and the genuine handles the surface
/// holds. Resolving opens only stores that already exist.
struct FollowerPlan {
    workspace: WorkspaceFollower,
    /// The spellings of the old default workspace that ACP rows are matched
    /// against: see [`old_workspace_spellings`].
    old_workspace_spellings: Vec<String>,
    memory: Slot<Arc<dyn Memory>>,
    acp: Slot<Arc<AcpSessionStore>>,
    sessions: Slot<Arc<dyn SessionBackend>>,
}

impl FollowerPlan {
    async fn resolve(
        config: &Config,
        from: &str,
        to: &str,
        record: Option<&RecoveryRecord>,
        stores: &SurfaceStores<'_>,
    ) -> Self {
        Self {
            workspace: workspace_follower(config, from, to, record).await,
            old_workspace_spellings: old_workspace_spellings(config, from).await,
            memory: memory_slot(config, stores).await,
            acp: match stores.acp {
                Some(store) => Slot::Open(Arc::clone(store)),
                None => {
                    open_existing(&AcpSessionStore::db_path(&config.data_dir), || {
                        AcpSessionStore::new(&config.data_dir).map(Arc::new)
                    })
                    .await
                }
            },
            sessions: match stores.session_backend {
                Some(backend) => Slot::Open(Arc::clone(backend)),
                None => {
                    open_existing(&SqliteSessionBackend::db_path(&config.data_dir), || {
                        SqliteSessionBackend::new(&config.data_dir)
                            .map(|backend| Arc::new(backend) as Arc<dyn SessionBackend>)
                    })
                    .await
                }
            },
        }
    }

    /// How much state `follower` still keeps under `from`.
    async fn probe(
        &self,
        follower: FollowerKind,
        config: &Config,
        from: &str,
    ) -> Result<usize, FollowerIssue> {
        let unreadable = |detail: String| FollowerIssue::unreadable(follower, detail);
        match follower {
            FollowerKind::Workspace => self.workspace.residue().await.map_err(unreadable),
            FollowerKind::Memory => match &self.memory {
                Slot::OutOfScope => Ok(0),
                Slot::Unreadable(reason) => Err(unreadable(reason.clone())),
                Slot::Open(memory) => memory
                    .count_agent(from)
                    .await
                    .map_err(|e| unreadable(format!("{e:#}"))),
            },
            FollowerKind::Cron => match inspect_lifecycle_path(&crate::cron::db_path(config)).await
            {
                PathPresence::Absent => Ok(0),
                PathPresence::Uninspectable(reason) => Err(unreadable(reason)),
                PathPresence::Present => crate::cron::agent_residue_count(config, from)
                    .map(Option::unwrap_or_default)
                    .map_err(|e| unreadable(format!("{e:#}"))),
            },
            FollowerKind::Acp => self.acp_residue(from).map(|acp| acp.total()),
            FollowerKind::Sessions => match &self.sessions {
                Slot::OutOfScope => Ok(0),
                Slot::Unreadable(reason) => Err(unreadable(reason.clone())),
                Slot::Open(backend) => backend
                    .count_agent_attribution(from)
                    .map_err(|e| unreadable(e.to_string())),
            },
        }
    }

    /// The ACP sessions `from` still owns, and those saved with a working
    /// directory in its old default workspace, whatever the workspace
    /// follower does: either kind keeps the record open.
    fn acp_residue(&self, from: &str) -> Result<AcpResidue, FollowerIssue> {
        let unreadable =
            |e: anyhow::Error| FollowerIssue::unreadable(FollowerKind::Acp, format!("{e:#}"));
        let store = match &self.acp {
            Slot::OutOfScope => return Ok(AcpResidue::default()),
            Slot::Unreadable(reason) => {
                return Err(FollowerIssue::unreadable(FollowerKind::Acp, reason.clone()));
            }
            Slot::Open(store) => store,
        };
        let owned = store.count_sessions_by_agent(from).map_err(unreadable)?;
        let mut in_old_workspace = 0;
        for spelling in &self.old_workspace_spellings {
            in_old_workspace += store
                .count_sessions_under_workspace(spelling)
                .map_err(unreadable)?;
        }
        Ok(AcpResidue {
            owned,
            in_old_workspace,
        })
    }

    /// Probe every follower in order and move the ones holding state under
    /// `from`. Returns each follower with the issue its probe or move hit.
    async fn act_all(
        &self,
        config: &Config,
        from: &str,
        to: &str,
        report: &mut ConvergeReport,
    ) -> Vec<(FollowerKind, Option<FollowerIssue>)> {
        let mut attempted = Vec::with_capacity(FollowerKind::ALL.len());
        // Whether the workspace follower left nothing at the old default
        // location, which decides whether ACP rows saved there follow a
        // moving workspace: see `acp_destination`.
        let mut workspace_settled = true;
        for follower in FollowerKind::ALL {
            let issue = match self.probe(follower, config, from).await {
                Err(issue) => Some(issue),
                Ok(0) => None,
                Ok(_) => {
                    self.act(follower, config, from, to, workspace_settled, report)
                        .await
                }
            };
            if follower == FollowerKind::Workspace {
                workspace_settled = issue.is_none();
            }
            attempted.push((follower, issue));
        }
        attempted
    }

    async fn act(
        &self,
        follower: FollowerKind,
        config: &Config,
        from: &str,
        to: &str,
        workspace_settled: bool,
        report: &mut ConvergeReport,
    ) -> Option<FollowerIssue> {
        match follower {
            FollowerKind::Workspace => match &self.workspace {
                WorkspaceFollower::Move(workspace) => {
                    let moved = move_workspace(workspace).await;
                    if moved.is_ok() {
                        report.workspace_moved = true;
                        remove_alias_dir(&workspace.source).await;
                    }
                    moved.err()
                }
                WorkspaceFollower::Leftover(old) => remove_leftover(old, from).await.err(),
                WorkspaceFollower::TargetUsesOld => Some(FollowerIssue::conflict(
                    follower,
                    format!(
                        "the workspace.path of agent `{to}` is or overlaps the old default workspace of `{from}`; point workspace.path elsewhere, then re-run the rename"
                    ),
                )),
                WorkspaceFollower::RecordMismatch(_) => Some(FollowerIssue::conflict(
                    follower,
                    self.workspace.residue_detail(from, to),
                )),
                // Its probe reports it unreadable, so it is never acted on.
                WorkspaceFollower::Uninspectable(reason) => {
                    Some(FollowerIssue::unreadable(follower, reason.clone()))
                }
            },
            FollowerKind::Memory => {
                let Slot::Open(memory) = &self.memory else {
                    return None;
                };
                match memory.rename_agent(from, to).await {
                    Ok(rows) => {
                        report.memory_rows = rows;
                        None
                    }
                    Err(e) => Some(memory_rename_failed(memory.as_ref(), to, &e).await),
                }
            }
            FollowerKind::Cron => match crate::cron::rename_jobs_by_agent(config, from, to) {
                Ok(jobs) => {
                    report.cron_jobs = jobs;
                    None
                }
                Err(e) => Some(FollowerIssue::lagging(
                    follower,
                    format!("cron rename: {e:#}"),
                )),
            },
            FollowerKind::Acp => {
                let Slot::Open(store) = &self.acp else {
                    return None;
                };
                let mut failure = None;
                match store.rename_sessions_by_agent(from, to) {
                    Ok(rows) => report.acp_sessions = rows,
                    Err(e) => failure = Some(format!("acp rename: {e:#}")),
                }
                if let Some(destination) = self
                    .acp_destination(config, from, to, workspace_settled)
                    .await
                {
                    let destination = destination.to_string_lossy();
                    for spelling in &self.old_workspace_spellings {
                        match store.relocate_session_workspaces(spelling, &destination) {
                            Ok(rows) => report.acp_workspaces += rows,
                            Err(e) => {
                                failure.get_or_insert_with(|| format!("acp rename: {e:#}"));
                            }
                        }
                    }
                }
                failure.map(|detail| FollowerIssue::lagging(follower, detail))
            }
            FollowerKind::Sessions => {
                let Slot::Open(backend) = &self.sessions else {
                    return None;
                };
                match backend.rename_agent_attribution(from, to) {
                    Ok(rows) => {
                        report.sessions_repointed = rows;
                        None
                    }
                    Err(e) => Some(FollowerIssue::lagging(
                        follower,
                        format!("session attribution rename: {e}"),
                    )),
                }
            }
        }
    }

    /// Where ACP rows saved with a working directory in the old default
    /// workspace of `from` are re-pointed, or `None` while they stay put.
    ///
    /// A workspace that moves takes them to the new alias's default location
    /// once the workspace follower has left nothing at the old one in this
    /// run. When nothing moves, they follow to the new alias's current
    /// workspace once the old location is gone, as long as that workspace
    /// does not overlap it. While the old files are still there, or whether
    /// they are cannot be told, a resumed session belongs with them, and an
    /// overlapping workspace is the workspace follower's conflict to report.
    /// Rows left in place keep the record open until a later run can
    /// re-point them.
    async fn acp_destination(
        &self,
        config: &Config,
        from: &str,
        to: &str,
        workspace_settled: bool,
    ) -> Option<PathBuf> {
        if let WorkspaceFollower::Move(workspace) = &self.workspace {
            return workspace_settled.then(|| workspace.destination.clone());
        }
        let old = config.default_agent_workspace_dir(from);
        if !matches!(inspect_lifecycle_path(&old).await, PathPresence::Absent) {
            return None;
        }
        let current = config.agent_workspace_dir(to);
        matches!(workspaces_overlap(&current, &old).await, Ok(false)).then_some(current)
    }

    /// Re-probe every follower. The re-probe is authoritative: a follower
    /// holding nothing under `from` has converged whatever its move reported,
    /// one still holding state reports why its move did not take it, or that
    /// it still lags, and one that cannot be read stays unreadable. A
    /// workspace holding nothing under `from` has converged only once
    /// [`sync_workspace_directories`] has synced the directories its moves
    /// and cleanups change, which it does on every run, whether or not this
    /// one moved anything.
    async fn verify(
        &self,
        config: &Config,
        from: &str,
        to: &str,
        attempted: Vec<(FollowerKind, Option<FollowerIssue>)>,
    ) -> Vec<FollowerIssue> {
        let mut outstanding = Vec::new();
        for (follower, issue) in attempted {
            match self.probe_detail(follower, config, from, to).await {
                Ok(None) if follower == FollowerKind::Workspace => {
                    outstanding.extend(sync_workspace_directories(config, from, to).await);
                }
                Ok(None) => {}
                Ok(Some(detail)) => outstanding
                    .push(issue.unwrap_or_else(|| FollowerIssue::lagging(follower, detail))),
                Err(unreadable) => outstanding.push(unreadable),
            }
        }
        outstanding
    }

    /// Probe every follower without acting and list what is still kept under
    /// `from`, and every store that could not be read.
    async fn residue(&self, config: &Config, from: &str, to: &str) -> Vec<FollowerIssue> {
        let mut residue = Vec::new();
        for follower in FollowerKind::ALL {
            match self.probe_detail(follower, config, from, to).await {
                Ok(None) => {}
                Ok(Some(detail)) => residue.push(FollowerIssue::lagging(follower, detail)),
                Err(unreadable) => residue.push(unreadable),
            }
        }
        residue
    }

    /// Probe `follower` as [`Self::probe`] does and, while it still holds
    /// state under `from`, return the warning line saying what; `None` once
    /// it holds nothing.
    async fn probe_detail(
        &self,
        follower: FollowerKind,
        config: &Config,
        from: &str,
        to: &str,
    ) -> Result<Option<String>, FollowerIssue> {
        if follower == FollowerKind::Acp {
            return Ok(self.acp_residue(from)?.detail(from));
        }
        let held = self.probe(follower, config, from).await?;
        Ok((held > 0).then(|| match follower {
            FollowerKind::Workspace => self.workspace.residue_detail(from, to),
            _ => attributed_detail(follower, from),
        }))
    }
}

/// The ACP sessions a rename still has to re-point.
#[derive(Default)]
struct AcpResidue {
    /// Sessions the old alias still owns.
    owned: usize,
    /// Sessions, whoever owns them, saved with a working directory that is
    /// the old default workspace or lies beneath it. A resumed session runs
    /// there, in whatever an agent re-created under the old alias keeps.
    in_old_workspace: usize,
}

impl AcpResidue {
    fn total(&self) -> usize {
        self.owned + self.in_old_workspace
    }

    /// The warning line for the sessions still to re-point, or `None` once
    /// there are none.
    fn detail(&self, from: &str) -> Option<String> {
        let owned = (self.owned > 0).then(|| attributed_detail(FollowerKind::Acp, from));
        let in_old_workspace = (self.in_old_workspace > 0).then(|| {
            format!(
                "saved session working directories still point at the old default workspace of `{from}`"
            )
        });
        match (owned, in_old_workspace) {
            (Some(owned), Some(in_old_workspace)) => Some(format!("{owned}; {in_old_workspace}")),
            (owned, in_old_workspace) => owned.or(in_old_workspace),
        }
    }
}

/// The warning line for `follower` still attributing state to `from`.
fn attributed_detail(follower: FollowerKind, from: &str) -> String {
    format!("{follower} still attributes state to `{from}`")
}

/// Decide what the workspace follower does for renaming `from` to `to`,
/// covered by `record` when one exists.
///
/// A `to` at its own default location takes the old default workspace, as
/// long as the record kept that location for the move. A record that kept
/// none (the old alias had a `workspace.path`) moves nothing, and neither
/// does one naming another path. A `to` with a custom path takes nothing,
/// and that path must not overlap the old default location: an agent
/// re-created under `from` would share it. Whether or not anything moves,
/// the old default location has to be empty or gone before the record
/// clears.
async fn workspace_follower(
    config: &Config,
    from: &str,
    to: &str,
    record: Option<&RecoveryRecord>,
) -> WorkspaceFollower {
    let old = config.default_agent_workspace_dir(from);
    let destination = config.default_agent_workspace_dir(to);
    let current = config.agent_workspace_dir(to);
    if lexically_normalized(&current) != lexically_normalized(&destination) {
        return match workspaces_overlap(&current, &old).await {
            Ok(true) => WorkspaceFollower::TargetUsesOld,
            Ok(false) => WorkspaceFollower::Leftover(old),
            Err(e) => WorkspaceFollower::Uninspectable(format!(
                "cannot tell whether {} overlaps {}: {e}",
                current.display(),
                old.display()
            )),
        };
    }
    match record.map(|record| record.source_workspace.as_ref()) {
        Some(None) => WorkspaceFollower::Leftover(old),
        Some(Some(recorded)) if lexically_normalized(recorded) != lexically_normalized(&old) => {
            WorkspaceFollower::RecordMismatch(recorded.clone())
        }
        _ => WorkspaceFollower::Move(WorkspaceMove {
            source: old,
            destination,
            case_only: from.to_lowercase() == to.to_lowercase(),
        }),
    }
}

/// Every spelling of the old default workspace of `from` that an ACP row may
/// have recorded as its working directory: as configured, and with symlinks
/// resolved. A row keeps pointing there whatever the workspace follower does,
/// and after the directory is gone, which [`canonical_spelling`] resolves
/// through its nearest existing ancestor.
async fn old_workspace_spellings(config: &Config, from: &str) -> Vec<String> {
    let old = config.default_agent_workspace_dir(from);
    let mut spellings = vec![old.to_string_lossy().into_owned()];
    if let Some(canonical) = canonical_spelling(&old).await {
        let canonical = canonical.to_string_lossy().into_owned();
        if !spellings.contains(&canonical) {
            spellings.push(canonical);
        }
    }
    spellings
}

/// `path` with `.` components dropped and each `..` folded into the
/// component before it, without consulting the filesystem.
fn lexically_normalized(path: &Path) -> PathBuf {
    let mut normalized = PathBuf::new();
    for component in path.components() {
        match component {
            Component::CurDir => {}
            Component::ParentDir => match normalized.components().next_back() {
                Some(Component::Normal(_)) => {
                    normalized.pop();
                }
                // Nothing lies above the root.
                Some(Component::RootDir) => {}
                _ => normalized.push(component),
            },
            other => normalized.push(other),
        }
    }
    normalized
}

/// Whether `a` and `b` name the same place: the same path once `.` and `..`
/// are folded, or one existing file or directory reached both ways.
async fn same_place(a: &Path, b: &Path) -> std::io::Result<bool> {
    if lexically_normalized(a) == lexically_normalized(b) {
        return Ok(true);
    }
    let (a, b) = (a.to_path_buf(), b.to_path_buf());
    match tokio::task::spawn_blocking(move || same_existing_file(&a, &b)).await {
        Ok(same) => same,
        Err(e) => Err(std::io::Error::other(e)),
    }
}

/// Whether the workspaces at `a` and `b` overlap: one is the other, or lies
/// beneath it. Paths compare component by component once `.` and `..` are
/// folded, so nothing needs to exist yet, and `workspace2` does not lie
/// beneath `workspace`. When both exist, one directory reached both ways,
/// or one beneath the other once symlinks are resolved, overlaps as well.
async fn workspaces_overlap(a: &Path, b: &Path) -> std::io::Result<bool> {
    let (lexical_a, lexical_b) = (lexically_normalized(a), lexically_normalized(b));
    if lexical_a.starts_with(&lexical_b) || lexical_b.starts_with(&lexical_a) {
        return Ok(true);
    }
    let (a, b) = (a.to_path_buf(), b.to_path_buf());
    match tokio::task::spawn_blocking(move || existing_paths_overlap(&a, &b)).await {
        Ok(overlap) => overlap,
        Err(e) => Err(std::io::Error::other(e)),
    }
}

/// Whether `a` and `b` both exist and are one file or directory, or one lies
/// beneath the other once both are canonicalized. Either missing is no
/// overlap; a path that cannot be inspected for any other reason is an
/// error.
fn existing_paths_overlap(a: &Path, b: &Path) -> std::io::Result<bool> {
    if same_existing_file(a, b)? {
        return Ok(true);
    }
    let (Some(a), Some(b)) = (existing_canonical(a)?, existing_canonical(b)?) else {
        return Ok(false);
    };
    Ok(a.starts_with(&b) || b.starts_with(&a))
}

/// `path` with every symlink resolved, or `None` when nothing exists there:
/// the path is missing, or one of its ancestors is not a directory.
fn existing_canonical(path: &Path) -> std::io::Result<Option<PathBuf>> {
    match std::fs::canonicalize(path) {
        Ok(canonical) => Ok(Some(canonical)),
        Err(e)
            if matches!(
                e.kind(),
                std::io::ErrorKind::NotFound | std::io::ErrorKind::NotADirectory
            ) =>
        {
            Ok(None)
        }
        Err(e) => Err(e),
    }
}

/// `path` with the symlinks in its existing part resolved. A path that no
/// longer exists (a workspace an earlier run already moved) resolves its
/// nearest existing ancestor and keeps the rest, so rows recorded under the
/// resolved spelling are still found.
async fn canonical_spelling(path: &Path) -> Option<PathBuf> {
    let mut missing = Vec::new();
    let mut current = path;
    loop {
        if let Ok(mut canonical) = tokio::fs::canonicalize(current).await {
            canonical.extend(missing.iter().rev());
            return Some(canonical);
        }
        missing.push(current.file_name()?);
        current = current.parent().filter(|p| !p.as_os_str().is_empty())?;
    }
}

/// Move the old workspace to the new alias-derived location. An absent
/// destination is created, and an empty directory there is replaced; anything
/// else there is left for the operator. The move is visible once this
/// returns; [`sync_workspace_directories`] makes it durable before the record
/// can clear.
async fn move_workspace(workspace: &WorkspaceMove) -> Result<(), FollowerIssue> {
    let WorkspaceMove {
        source,
        destination,
        ..
    } = workspace;
    let follower = FollowerKind::Workspace;
    let move_failed = |e: &dyn fmt::Display| {
        FollowerIssue::lagging(
            follower,
            format!(
                "workspace move {} -> {} failed: {e}",
                source.display(),
                destination.display()
            ),
        )
    };
    match inspect_lifecycle_path(destination).await {
        PathPresence::Uninspectable(reason) => {
            return Err(FollowerIssue::unreadable(follower, reason));
        }
        PathPresence::Absent => {
            if let Some(parent) = destination.parent() {
                tokio::fs::create_dir_all(parent)
                    .await
                    .map_err(|e| move_failed(&e))?;
            }
        }
        PathPresence::Present => match is_empty_directory(destination).await {
            Ok(true) => tokio::fs::remove_dir(destination)
                .await
                .map_err(|e| move_failed(&e))?,
            Ok(false) => {
                return Err(FollowerIssue::conflict(
                    follower,
                    format!(
                        "workspace destination {} already exists and is not empty",
                        destination.display()
                    ),
                ));
            }
            Err(e) => {
                return Err(FollowerIssue::unreadable(
                    follower,
                    format!("cannot inspect {}: {e}", destination.display()),
                ));
            }
        },
    }
    tokio::fs::rename(source, destination)
        .await
        .map_err(|e| move_failed(&e))
}

/// Sync every directory whose entries a workspace move or cleanup of this
/// rename changes, so the change survives a power loss before the record
/// clears. In order: `<install>/agents/<to>`, which gains the moved
/// workspace; `<install>/agents`, which gains `<to>` when the move creates
/// it and loses `<from>` when the emptied alias directory is removed; and
/// `<install>/agents/<from>`, which loses the workspace.
///
/// Every converge that finds the old default workspace gone syncs them, not
/// only one that moved it: a run that stopped between a move and its sync
/// leaves nothing to move when it resumes. A directory that does not exist
/// has nothing to sync. Each directory that cannot be inspected or synced is
/// an unreadable workspace issue, which keeps the record open so the next
/// converge retries it.
async fn sync_workspace_directories(config: &Config, from: &str, to: &str) -> Vec<FollowerIssue> {
    let new_workspace = config.default_agent_workspace_dir(to);
    let old_workspace = config.default_agent_workspace_dir(from);
    let new_alias_dir = new_workspace.parent();
    let candidates = [
        new_alias_dir,
        new_alias_dir.and_then(Path::parent),
        old_workspace.parent(),
    ];
    let mut dirs: Vec<&Path> = Vec::with_capacity(candidates.len());
    for dir in candidates.into_iter().flatten() {
        if !dirs.contains(&dir) {
            dirs.push(dir);
        }
    }

    let follower = FollowerKind::Workspace;
    let mut issues = Vec::new();
    for dir in dirs {
        match inspect_lifecycle_path(dir).await {
            PathPresence::Absent => {}
            PathPresence::Uninspectable(reason) => {
                issues.push(FollowerIssue::unreadable(follower, reason));
            }
            PathPresence::Present => {
                if let Err(e) = sync_directory(dir).await {
                    issues.push(FollowerIssue::unreadable(
                        follower,
                        format!("could not sync {}: {e}", dir.display()),
                    ));
                }
            }
        }
    }
    issues
}

/// Whether `path` is a real directory with nothing in it. A symlink or a file
/// is not.
async fn is_empty_directory(path: &Path) -> std::io::Result<bool> {
    if !tokio::fs::symlink_metadata(path).await?.is_dir() {
        return Ok(false);
    }
    Ok(tokio::fs::read_dir(path)
        .await?
        .next_entry()
        .await?
        .is_none())
}

/// Best effort: remove `<install>/agents/<from>` once the old default
/// workspace below it is gone. It stays when anything else was kept beside
/// the workspace. [`sync_workspace_directories`] makes the removal durable
/// before the record can clear.
async fn remove_alias_dir(old_workspace: &Path) {
    if let Some(alias_dir) = old_workspace.parent() {
        let _ = tokio::fs::remove_dir(alias_dir).await;
    }
}

/// Remove the old default workspace of `from`, which this rename does not
/// move, when it is an empty directory. Anything else there is not this
/// rename's to take, and an agent re-created under `from` would adopt it, so
/// it is left for the operator to move.
async fn remove_leftover(old: &Path, from: &str) -> Result<(), FollowerIssue> {
    let follower = FollowerKind::Workspace;
    match is_empty_directory(old).await {
        Ok(true) => {}
        Ok(false) => {
            return Err(FollowerIssue::conflict(
                follower,
                format!(
                    "the old default workspace of `{from}` still exists at {}; move its contents by hand, then re-run the rename",
                    old.display()
                ),
            ));
        }
        Err(e) => {
            return Err(FollowerIssue::unreadable(
                follower,
                format!("cannot inspect {}: {e}", old.display()),
            ));
        }
    }
    tokio::fs::remove_dir(old).await.map_err(|e| {
        FollowerIssue::lagging(
            follower,
            format!(
                "cannot remove the empty old default workspace {}: {e}",
                old.display()
            ),
        )
    })?;
    remove_alias_dir(old).await;
    Ok(())
}

/// Why renaming memory from the old alias to `to` failed. The backends that
/// key memory by an agent identity refuse to merge into an alias that already
/// owns memory, which only an operator can settle; that is told apart by
/// reading whether `to` owns memory, not by the error text. Anything else
/// lags, and re-running the rename retries it.
async fn memory_rename_failed(
    memory: &dyn Memory,
    to: &str,
    error: &anyhow::Error,
) -> FollowerIssue {
    let follower = FollowerKind::Memory;
    match memory.export_agent(to).await {
        Ok(owned) if !owned.is_empty() => FollowerIssue::conflict(
            follower,
            format!("memory for `{to}` already exists; merge it by hand or abandon the rename"),
        ),
        _ => FollowerIssue::lagging(follower, format!("memory rename: {error:#}")),
    }
}

/// The memory store holding `from`'s attribution. A surface handle is used
/// unless it is the `NoneMemory` placeholder a surface falls back to when its
/// configured backend could not be built (or it booted without agents): that
/// placeholder's empty answers say nothing about the configured store.
async fn memory_slot(config: &Config, stores: &SurfaceStores<'_>) -> Slot<Arc<dyn Memory>> {
    let backend = zeroclaw_memory::classify_memory_backend(
        &zeroclaw_memory::backend_kind_from_dotted(&config.memory.backend),
    );
    if let Some(handle) = stores.memory {
        let placeholder = matches!(handle.role(), Role::Memory(MemoryKind::None))
            && backend != MemoryBackendKind::None;
        if !placeholder {
            return Slot::Open(Arc::clone(handle));
        }
    }
    let open = || -> anyhow::Result<Arc<dyn Memory>> {
        Ok(Arc::from(zeroclaw_memory::create_memory_from_config(
            config, None,
        )?))
    };
    match backend {
        // No per-agent rows: an unknown backend name builds markdown memory.
        MemoryBackendKind::None | MemoryBackendKind::Markdown | MemoryBackendKind::Unknown => {
            Slot::OutOfScope
        }
        MemoryBackendKind::Sqlite | MemoryBackendKind::Lucid => {
            match zeroclaw_memory::sqlite_db_path_for_config(config) {
                Some(path) => open_existing(&path, open).await,
                None => Slot::OutOfScope,
            }
        }
        MemoryBackendKind::Postgres | MemoryBackendKind::Qdrant => opened(open()),
    }
}

/// Open the store whose file is `path` only if that file exists, so resolving
/// a follower never creates its store.
async fn open_existing<T>(path: &Path, open: impl FnOnce() -> anyhow::Result<T>) -> Slot<T> {
    match inspect_lifecycle_path(path).await {
        PathPresence::Absent => Slot::OutOfScope,
        PathPresence::Uninspectable(reason) => Slot::Unreadable(reason),
        PathPresence::Present => opened(open()),
    }
}

fn opened<T>(store: anyhow::Result<T>) -> Slot<T> {
    match store {
        Ok(store) => Slot::Open(store),
        Err(e) => Slot::Unreadable(format!("{e:#}")),
    }
}

fn log_resumed(from: &str, to: &str, discovered: bool) {
    ::zeroclaw_log::record!(
        INFO,
        ::zeroclaw_log::Event::new(module_path!(), ::zeroclaw_log::Action::Note).with_attrs(
            ::serde_json::json!({
                "error_key": KEY_RESUMED,
                "from": from,
                "to": to,
                "discovered": discovered,
            })
        ),
        "agent rename resumes an unfinished rename"
    );
}

/// Log a refusal the recovery journal or an unreadable store decided. Plain
/// input errors are the caller's to report.
fn log_refusal(from: &str, to: &str, error: &RenameRecoveryError) {
    let (error_key, message) = match error {
        RenameRecoveryError::AliasRetired { .. }
        | RenameRecoveryError::RecoveryPending { .. }
        | RenameRecoveryError::SourceReconfigured { .. } => (
            KEY_REFUSED,
            "agent lifecycle operation refused by an unfinished agent rename",
        ),
        RenameRecoveryError::Unreadable { .. } => (
            KEY_UNREADABLE,
            "agent rename recovery could not read a store it must check",
        ),
        _ => return,
    };
    ::zeroclaw_log::record!(
        WARN,
        ::zeroclaw_log::Event::new(module_path!(), ::zeroclaw_log::Action::Note)
            .with_outcome(::zeroclaw_log::EventOutcome::Failure)
            .with_attrs(::serde_json::json!({
                "error_key": error_key,
                "from": from,
                "to": to,
                "error": error.to_string(),
            })),
        message
    );
}

fn log_record_failed(from: &str, to: &str, detail: &str) {
    ::zeroclaw_log::record!(
        WARN,
        ::zeroclaw_log::Event::new(module_path!(), ::zeroclaw_log::Action::Note)
            .with_outcome(::zeroclaw_log::EventOutcome::Failure)
            .with_attrs(::serde_json::json!({
                "error_key": KEY_RECORD_FAILED,
                "from": from,
                "to": to,
                "error": detail,
            })),
        "agent rename recovery record was not updated"
    );
}

#[cfg(test)]
mod tests;
