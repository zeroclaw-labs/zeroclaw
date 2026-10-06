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
//!    record only once nothing is left under the old alias.
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

use crate::lifecycle_path::{PathPresence, inspect_lifecycle_path, same_existing_file};

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
/// `config_before_commit` is the config the commit starts from. The record
/// keeps `from`'s workspace for the move only when `from` sets no
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
/// `from`, rather than retiring an alias that is still configured.
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
/// commit never landed, retires nothing and is dropped by the next [`arm`].
///
/// A `config` that shows the commit after all keeps the record, so the
/// rename's recovery is not lost.
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
    let alias = from.clone();
    let removed = tokio::task::spawn_blocking(move || {
        let removed = journal.remove(&guard, RecoveryOperation::Rename, &alias);
        drop(guard);
        removed
    })
    .await;
    match removed {
        Ok(Ok(_)) => {}
        Ok(Err(e)) => log_record_failed(&from, &to, &e.to_string()),
        Err(e) => log_record_failed(&from, &to, &e.to_string()),
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
    // A prepared record retires `from` only while the config lacks it. Once
    // state starts moving that must no longer hang on the config, so the
    // record is committed first.
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
/// rename's state. Only effective records count: a prepared record whose
/// commit never landed retires nothing.
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
        // its commit, so every prepared record whose commit did not land is
        // void.
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
    /// Every spelling of `source` an ACP row may have recorded as its working
    /// directory: as configured, and with symlinks resolved.
    spellings: Vec<String>,
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
    /// The new alias's custom workspace is the old default location itself.
    TargetUsesOld,
    /// The recorded workspace is not the old default location under this
    /// config: the install moved, or the journal was edited.
    RecordMismatch(PathBuf),
    /// Whether the new alias's custom workspace is the old default location
    /// could not be established. Carries why.
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
                "agent `{to}` uses the old default workspace of `{from}` as its workspace.path"
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

    /// How much state `follower` still attributes to `from`.
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
            FollowerKind::Acp => match &self.acp {
                Slot::OutOfScope => Ok(0),
                Slot::Unreadable(reason) => Err(unreadable(reason.clone())),
                Slot::Open(store) => {
                    let mut residue = store
                        .count_sessions_by_agent(from)
                        .map_err(|e| unreadable(format!("{e:#}")))?;
                    for spelling in self.workspace_spellings() {
                        residue += store
                            .count_sessions_under_workspace(spelling)
                            .map_err(|e| unreadable(format!("{e:#}")))?;
                    }
                    Ok(residue)
                }
            },
            FollowerKind::Sessions => match &self.sessions {
                Slot::OutOfScope => Ok(0),
                Slot::Unreadable(reason) => Err(unreadable(reason.clone())),
                Slot::Open(backend) => backend
                    .count_agent_attribution(from)
                    .map_err(|e| unreadable(e.to_string())),
            },
        }
    }

    fn workspace_spellings(&self) -> &[String] {
        match &self.workspace {
            WorkspaceFollower::Move(workspace) => &workspace.spellings,
            _ => &[],
        }
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
        // ACP rows keep the old workspace's absolute path. They follow the
        // workspace only once it has left the old location (moved now, moved
        // earlier, or never there); while it stays there, so do they.
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
                        "agent `{to}` uses the old default workspace of `{from}` as its workspace.path; point workspace.path elsewhere, then re-run the rename"
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
                if let (WorkspaceFollower::Move(workspace), true) =
                    (&self.workspace, workspace_settled)
                {
                    let destination = workspace.destination.to_string_lossy();
                    for spelling in &workspace.spellings {
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

    /// Re-probe every follower. The re-probe is authoritative: a follower
    /// holding nothing under `from` has converged whatever its move reported,
    /// one still holding state reports why its move did not take it, or that
    /// it still lags, and one that cannot be read stays unreadable.
    async fn verify(
        &self,
        config: &Config,
        from: &str,
        to: &str,
        attempted: Vec<(FollowerKind, Option<FollowerIssue>)>,
    ) -> Vec<FollowerIssue> {
        let mut outstanding = Vec::new();
        for (follower, issue) in attempted {
            match self.probe(follower, config, from).await {
                Ok(0) => {}
                Ok(_) => outstanding.push(issue.unwrap_or_else(|| {
                    FollowerIssue::lagging(follower, self.residue_detail(follower, from, to))
                })),
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
            match self.probe(follower, config, from).await {
                Ok(0) => {}
                Ok(_) => residue.push(FollowerIssue::lagging(
                    follower,
                    self.residue_detail(follower, from, to),
                )),
                Err(unreadable) => residue.push(unreadable),
            }
        }
        residue
    }

    /// The warning line for `follower` still holding state under `from`.
    fn residue_detail(&self, follower: FollowerKind, from: &str, to: &str) -> String {
        match follower {
            FollowerKind::Workspace => self.workspace.residue_detail(from, to),
            _ => format!("{follower} still attributes state to `{from}`"),
        }
    }
}

/// Decide what the workspace follower does for renaming `from` to `to`,
/// covered by `record` when one exists.
///
/// A `to` at its own default location takes the old default workspace, as
/// long as the record kept that location for the move. A record that kept
/// none (the old alias had a `workspace.path`) moves nothing, and neither
/// does one naming another path. A `to` with a custom path takes nothing,
/// and that path must not be the old default location itself. Whether or not
/// anything moves, the old default location has to be empty or gone before
/// the record clears.
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
        return match same_place(&current, &old).await {
            Ok(true) => WorkspaceFollower::TargetUsesOld,
            Ok(false) => WorkspaceFollower::Leftover(old),
            Err(e) => WorkspaceFollower::Uninspectable(format!(
                "cannot tell whether {} is {}: {e}",
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
        _ => {
            let mut spellings = vec![old.to_string_lossy().into_owned()];
            if let Some(canonical) = canonical_spelling(&old).await {
                let canonical = canonical.to_string_lossy().into_owned();
                if !spellings.contains(&canonical) {
                    spellings.push(canonical);
                }
            }
            WorkspaceFollower::Move(WorkspaceMove {
                source: old,
                destination,
                spellings,
                case_only: from.to_lowercase() == to.to_lowercase(),
            })
        }
    }
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
/// else there is left for the operator.
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
/// the workspace.
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
mod tests {
    use super::*;
    use std::collections::HashMap;
    use std::sync::atomic::{AtomicUsize, Ordering};
    use tempfile::TempDir;
    use zeroclaw_api::memory_traits::{MemoryCategory, MemoryEntry};
    use zeroclaw_config::agent_recovery_journal::{pending_target, retired_alias};
    use zeroclaw_config::alias_refs::{self, AliasKind};
    use zeroclaw_config::schema::AliasedAgentConfig;
    use zeroclaw_memory::{NoneMemory, SqliteMemory};

    const FROM: &str = "scout";
    const TO: &str = "ranger";
    /// A configured alias older than the alias grammar, and the valid alias
    /// an operator migrates it to.
    const LEGACY: &str = "Legacy-Agent";
    const MIGRATED: &str = "legacy_agent";

    /// An install under `tmp` whose only agents are `aliases`, each able to
    /// own cron jobs. Nothing is created on disk.
    fn fixture(tmp: &TempDir, aliases: &[&str]) -> Config {
        let mut config = Config {
            config_path: tmp.path().join("config.toml"),
            data_dir: tmp.path().join("data"),
            ..Config::default()
        };
        config.agents.clear();
        config
            .risk_profiles
            .entry("default".to_string())
            .or_default();
        config
            .runtime_profiles
            .entry("default".to_string())
            .or_default();
        for alias in aliases {
            config.agents.insert((*alias).to_string(), agent());
        }
        config
    }

    fn agent() -> AliasedAgentConfig {
        AliasedAgentConfig {
            risk_profile: "default".into(),
            runtime_profile: "default".into(),
            ..AliasedAgentConfig::default()
        }
    }

    /// `config` with `from` renamed to `to` the way every surface commits it.
    fn renamed(config: &Config, from: &str, to: &str) -> Config {
        let mut after = config.clone();
        alias_refs::rename_with_cascade(&mut after, &AliasKind::Agent, from, to).unwrap();
        after
    }

    fn committed(config: &Config) -> Config {
        renamed(config, FROM, TO)
    }

    /// A surface's commit: arm, commit, acknowledge. Nothing has converged.
    async fn arm_and_commit(before: &Config) -> Config {
        let armed = arm(before, FROM, TO).await.unwrap();
        let after = committed(before);
        acknowledge_commit(&after, armed).await;
        after
    }

    fn records(config: &Config) -> Vec<RecoveryRecord> {
        AgentRecoveryJournal::for_config(config).load().unwrap()
    }

    fn outstanding(outcome: &ConvergeOutcome) -> &[FollowerIssue] {
        match outcome {
            ConvergeOutcome::Converged(_) => &[],
            ConvergeOutcome::Incomplete { outstanding, .. } => outstanding,
        }
    }

    fn lagging_followers(outcome: &ConvergeOutcome) -> Vec<FollowerKind> {
        outstanding(outcome)
            .iter()
            .map(|issue| issue.follower)
            .collect()
    }

    /// Put a file in `alias`'s alias-derived workspace, and return the
    /// workspace.
    fn seed_workspace(config: &Config, alias: &str) -> PathBuf {
        let workspace = config.default_agent_workspace_dir(alias);
        std::fs::create_dir_all(&workspace).unwrap();
        std::fs::write(workspace.join("MEMORY.md"), alias).unwrap();
        workspace
    }

    /// Put a file where `<install>/agents/<TO>` must be a directory.
    fn block_destination(config: &Config) -> PathBuf {
        let alias_dir = config
            .default_agent_workspace_dir(TO)
            .parent()
            .unwrap()
            .to_path_buf();
        std::fs::create_dir_all(alias_dir.parent().unwrap()).unwrap();
        std::fs::write(&alias_dir, "in the way").unwrap();
        alias_dir
    }

    fn sqlite_memory(config: &Config) -> SqliteMemory {
        SqliteMemory::new("sqlite", &config.data_dir).unwrap()
    }

    /// Give `alias` its memory identity and `rows` memories.
    async fn seed_memory(config: &Config, alias: &str, rows: usize) {
        let memory = sqlite_memory(config);
        let agent_id = memory.ensure_agent_uuid(alias).await.unwrap();
        for idx in 0..rows {
            memory
                .store_with_agent(
                    &format!("{alias}-{idx}"),
                    "remembered",
                    MemoryCategory::Core,
                    None,
                    None,
                    None,
                    Some(&agent_id),
                )
                .await
                .unwrap();
        }
    }

    async fn memory_identity(config: &Config, alias: &str) -> usize {
        sqlite_memory(config).count_agent(alias).await.unwrap()
    }

    async fn memory_rows(config: &Config, alias: &str) -> usize {
        sqlite_memory(config)
            .export_agent(alias)
            .await
            .unwrap()
            .len()
    }

    /// Add a cron job owned by `alias`. Its risk-profile check creates the
    /// owner's workspace, so that is pointed away from the one under test.
    fn seed_cron(config: &Config, alias: &str) {
        let mut scratch = config.clone();
        if let Some(agent) = scratch.agents.get_mut(alias) {
            agent.workspace.path = Some(config.install_root_dir().join("scratch").join(alias));
        }
        crate::cron::add_job(&scratch, alias, "*/5 * * * *", "echo hello").unwrap();
    }

    fn cron_residue(config: &Config, alias: &str) -> Option<usize> {
        crate::cron::agent_residue_count(config, alias).unwrap()
    }

    fn seed_acp(config: &Config, session: &str, alias: &str, workspace: &Path) {
        AcpSessionStore::new(&config.data_dir)
            .unwrap()
            .create_session(session, alias, &workspace.to_string_lossy(), None)
            .unwrap();
    }

    /// The owner and working directory of an ACP session.
    fn acp_row(config: &Config, session: &str) -> (String, PathBuf) {
        let row = AcpSessionStore::new(&config.data_dir)
            .unwrap()
            .load_session(session)
            .unwrap()
            .unwrap();
        (row.agent_alias, PathBuf::from(row.workspace_dir))
    }

    fn seed_session(config: &Config, session: &str, alias: &str) {
        SqliteSessionBackend::new(&config.data_dir)
            .unwrap()
            .set_session_agent_alias(session, alias)
            .unwrap();
    }

    fn session_owner(config: &Config, session: &str) -> Option<String> {
        SqliteSessionBackend::new(&config.data_dir)
            .unwrap()
            .get_session_agent_alias(session)
            .unwrap()
    }

    /// Seed every follower with state under `FROM`, and return the workspace.
    async fn seed_every_follower(config: &Config) -> PathBuf {
        let workspace = seed_workspace(config, FROM);
        seed_memory(config, FROM, 2).await;
        seed_cron(config, FROM);
        seed_acp(config, "acp-1", FROM, &workspace);
        seed_session(config, "chat-1", FROM);
        workspace
    }

    /// A memory backend that keeps only per-agent identity counts and counts
    /// the renames it is asked for. Like the SQL backends, it refuses to merge
    /// into an alias that already owns memory.
    #[derive(Default)]
    struct CountingMemory {
        agents: parking_lot::Mutex<HashMap<String, usize>>,
        renames: AtomicUsize,
    }

    impl zeroclaw_api::attribution::Attributable for CountingMemory {
        fn role(&self) -> Role {
            Role::Memory(MemoryKind::Sqlite)
        }

        fn alias(&self) -> &str {
            "counting"
        }
    }

    #[async_trait::async_trait]
    impl Memory for CountingMemory {
        fn name(&self) -> &str {
            "counting"
        }

        async fn store(
            &self,
            _key: &str,
            _content: &str,
            _category: MemoryCategory,
            _session_id: Option<&str>,
        ) -> anyhow::Result<()> {
            Ok(())
        }

        async fn recall(
            &self,
            _query: &str,
            _limit: usize,
            _session_id: Option<&str>,
            _since: Option<&str>,
            _until: Option<&str>,
        ) -> anyhow::Result<Vec<MemoryEntry>> {
            Ok(Vec::new())
        }

        async fn get(&self, _key: &str) -> anyhow::Result<Option<MemoryEntry>> {
            Ok(None)
        }

        async fn list(
            &self,
            _category: Option<&MemoryCategory>,
            _session_id: Option<&str>,
        ) -> anyhow::Result<Vec<MemoryEntry>> {
            Ok(Vec::new())
        }

        async fn forget(&self, _key: &str) -> anyhow::Result<bool> {
            Ok(false)
        }

        async fn forget_for_agent(&self, _key: &str, _agent_id: &str) -> anyhow::Result<bool> {
            Ok(false)
        }

        async fn count(&self) -> anyhow::Result<usize> {
            Ok(self.agents.lock().values().sum())
        }

        async fn health_check(&self) -> bool {
            true
        }

        async fn store_with_agent(
            &self,
            _key: &str,
            _content: &str,
            _category: MemoryCategory,
            _session_id: Option<&str>,
            _namespace: Option<&str>,
            _importance: Option<f64>,
            _agent_id: Option<&str>,
        ) -> anyhow::Result<()> {
            Ok(())
        }

        async fn recall_for_agents(
            &self,
            _allowed_agent_ids: &[&str],
            _query: &str,
            _limit: usize,
            _session_id: Option<&str>,
            _since: Option<&str>,
            _until: Option<&str>,
        ) -> anyhow::Result<Vec<MemoryEntry>> {
            Ok(Vec::new())
        }

        async fn rename_agent(&self, from: &str, to: &str) -> anyhow::Result<usize> {
            self.renames.fetch_add(1, Ordering::SeqCst);
            let mut agents = self.agents.lock();
            if agents.get(to).is_some_and(|count| *count > 0) {
                anyhow::bail!("cannot rename agent memory to `{to}`: refusing to merge");
            }
            let moved = agents.remove(from).unwrap_or(0);
            if moved > 0 {
                agents.insert(to.to_string(), moved);
            }
            Ok(usize::from(moved > 0))
        }

        async fn count_agent(&self, agent_alias: &str) -> anyhow::Result<usize> {
            Ok(self.agents.lock().get(agent_alias).copied().unwrap_or(0))
        }
    }

    #[tokio::test]
    async fn a_fresh_rename_moves_every_follower_and_clears_its_record() {
        let tmp = TempDir::new().unwrap();
        let before = fixture(&tmp, &[FROM]);
        let source = seed_every_follower(&before).await;
        let stores = SurfaceStores::none();

        assert_eq!(
            resolve(&before, FROM, TO, &stores).await.unwrap(),
            Disposition::Fresh
        );
        let armed = arm(&before, FROM, TO).await.unwrap();
        assert_eq!((armed.from(), armed.to()), (FROM, TO));
        assert_eq!(
            retired_alias(&before, FROM).unwrap(),
            None,
            "a prepared record retires nothing until its commit lands"
        );
        let after = committed(&before);
        acknowledge_commit(&after, armed).await;

        let recorded = records(&after);
        assert_eq!(recorded.len(), 1);
        assert_eq!(recorded[0].phase, RecoveryPhase::Committed);
        assert_eq!(
            recorded[0].source_workspace.as_deref(),
            Some(source.as_path())
        );
        assert!(retired_alias(&after, FROM).unwrap().is_some());
        assert!(pending_target(&after, TO).unwrap().is_some());

        let outcome = converge(&after, FROM, TO, &stores).await.unwrap();
        assert!(outcome.is_converged(), "{:?}", outcome.warnings());
        assert!(outcome.warnings().is_empty());
        let report = outcome.report();
        assert!(report.workspace_moved);
        assert_eq!(
            report.memory_rows, 1,
            "the identity row carries the memories"
        );
        assert_eq!(report.cron_jobs, 1);
        assert_eq!(report.acp_sessions, 1);
        assert_eq!(report.acp_workspaces, 1);
        assert_eq!(report.sessions_repointed, 1);

        let destination = after.default_agent_workspace_dir(TO);
        assert_eq!(
            std::fs::read_to_string(destination.join("MEMORY.md")).unwrap(),
            FROM
        );
        assert!(
            !source.parent().unwrap().exists(),
            "the emptied old alias directory is removed"
        );
        assert_eq!(memory_identity(&after, FROM).await, 0);
        assert_eq!(memory_rows(&after, TO).await, 2);
        assert_eq!(cron_residue(&after, FROM), Some(0));
        assert_eq!(cron_residue(&after, TO), Some(1));
        assert_eq!(acp_row(&after, "acp-1"), (TO.to_string(), destination));
        assert_eq!(session_owner(&after, "chat-1").as_deref(), Some(TO));

        assert!(!AgentRecoveryJournal::for_config(&after).path().exists());
        assert_eq!(retired_alias(&after, FROM).unwrap(), None);
        assert_eq!(pending_target(&after, TO).unwrap(), None);
    }

    #[tokio::test]
    async fn a_rename_interrupted_after_its_commit_resumes() {
        let tmp = TempDir::new().unwrap();
        let before = fixture(&tmp, &[FROM]);
        let source = seed_workspace(&before, FROM);
        seed_cron(&before, FROM);
        let stores = SurfaceStores::none();

        let after = arm_and_commit(&before).await;
        // The process stops here: nothing has converged.
        assert!(source.exists());
        assert_eq!(
            resolve(&after, FROM, TO, &stores).await.unwrap(),
            Disposition::Resume
        );

        let outcome = converge(&after, FROM, TO, &stores).await.unwrap();
        assert!(outcome.is_converged(), "{:?}", outcome.warnings());
        assert!(outcome.report().workspace_moved);
        assert_eq!(outcome.report().cron_jobs, 1);
        assert!(records(&after).is_empty());
    }

    #[tokio::test]
    async fn a_rename_interrupted_before_acknowledging_its_commit_resumes() {
        let tmp = TempDir::new().unwrap();
        let before = fixture(&tmp, &[FROM]);
        seed_workspace(&before, FROM);
        let stores = SurfaceStores::none();

        let armed = arm(&before, FROM, TO).await.unwrap();
        let after = committed(&before);
        // The process stops after the commit, before acknowledging it.
        drop(armed);
        assert_eq!(records(&after)[0].phase, RecoveryPhase::Prepared);
        assert!(
            retired_alias(&after, FROM).unwrap().is_some(),
            "the live config shows the commit, so the prepared record retires the old alias"
        );

        assert_eq!(
            resolve(&after, FROM, TO, &stores).await.unwrap(),
            Disposition::Resume
        );
        let outcome = converge(&after, FROM, TO, &stores).await.unwrap();
        assert!(outcome.is_converged(), "{:?}", outcome.warnings());
        assert!(records(&after).is_empty());
    }

    #[tokio::test]
    async fn a_rename_committed_without_a_record_is_discovered_and_recorded() {
        let tmp = TempDir::new().unwrap();
        let before = fixture(&tmp, &[FROM]);
        seed_cron(&before, FROM);
        // Committed with no record, as an older build did.
        let after = committed(&before);
        let stores = SurfaceStores::none();

        assert_eq!(
            resolve(&after, FROM, TO, &stores).await.unwrap(),
            Disposition::Resume
        );
        let recorded = records(&after);
        assert_eq!(recorded.len(), 1);
        assert_eq!(
            (
                recorded[0].from.as_str(),
                recorded[0].to.as_str(),
                recorded[0].phase
            ),
            (FROM, TO, RecoveryPhase::Committed)
        );
        assert_eq!(
            recorded[0].source_workspace,
            Some(after.default_agent_workspace_dir(FROM))
        );
        assert!(retired_alias(&after, FROM).unwrap().is_some());

        // Asking again resumes the same record rather than adding one.
        assert_eq!(
            resolve(&after, FROM, TO, &stores).await.unwrap(),
            Disposition::Resume
        );
        assert_eq!(records(&after).len(), 1);

        let outcome = converge(&after, FROM, TO, &stores).await.unwrap();
        assert!(outcome.is_converged(), "{:?}", outcome.warnings());
        assert_eq!(cron_residue(&after, TO), Some(1));
        assert!(records(&after).is_empty());
    }

    #[tokio::test]
    async fn discovery_that_cannot_read_a_store_still_retires_the_old_alias() {
        let tmp = TempDir::new().unwrap();
        let after = committed(&fixture(&tmp, &[FROM]));
        // A directory where the cron database belongs cannot be read.
        std::fs::create_dir_all(crate::cron::db_path(&after)).unwrap();

        let err = resolve(&after, FROM, TO, &SurfaceStores::none())
            .await
            .unwrap_err();
        assert!(
            matches!(&err, RenameRecoveryError::Unreadable { store, .. } if store == "cron"),
            "{err:?}"
        );
        assert!(
            err.to_string().starts_with("cron could not be read: "),
            "{err}"
        );
        let retired = retired_alias(&after, FROM)
            .unwrap()
            .expect("an unreadable follower may hold state, so the old alias stays retired");
        assert_eq!(retired.phase, RecoveryPhase::Committed);
        assert_eq!(retired.to, TO);
    }

    #[tokio::test]
    async fn a_rename_with_nothing_left_behind_is_not_configured_and_records_nothing() {
        let tmp = TempDir::new().unwrap();
        let after = committed(&fixture(&tmp, &[FROM]));
        let stores = SurfaceStores::none();

        let err = resolve(&after, FROM, TO, &stores).await.unwrap_err();
        assert!(
            matches!(&err, RenameRecoveryError::NotConfigured { alias } if alias == FROM),
            "{err:?}"
        );
        assert_eq!(err.to_string(), format!("agents.{FROM} is not configured"));

        // Neither alias configured is not configured either.
        let neither = fixture(&tmp, &[]);
        assert!(matches!(
            resolve(&neither, FROM, TO, &stores).await,
            Err(RenameRecoveryError::NotConfigured { .. })
        ));
        assert!(
            !after.data_dir.exists(),
            "resolving created no journal, lock, or store"
        );
    }

    #[tokio::test]
    async fn a_retried_rename_keeps_the_memory_identity_it_already_moved() {
        let tmp = TempDir::new().unwrap();
        let before = fixture(&tmp, &[FROM]);
        seed_workspace(&before, FROM);
        // An identity row with no memories under it.
        seed_memory(&before, FROM, 0).await;
        let after = arm_and_commit(&before).await;
        let blocker = block_destination(&after);
        let stores = SurfaceStores::none();

        let first = converge(&after, FROM, TO, &stores).await.unwrap();
        assert_eq!(lagging_followers(&first), vec![FollowerKind::Workspace]);
        assert_eq!(first.report().memory_rows, 1);
        assert_eq!(memory_identity(&after, TO).await, 1);

        std::fs::remove_file(&blocker).unwrap();
        let second = converge(&after, FROM, TO, &stores).await.unwrap();
        assert!(second.is_converged(), "{:?}", second.warnings());
        assert_eq!(
            second.report().memory_rows,
            0,
            "memory had nothing left to move"
        );
        assert_eq!(
            memory_identity(&after, TO).await,
            1,
            "a second rename would have dropped the moved identity as an orphan"
        );
        assert_eq!(memory_identity(&after, FROM).await, 0);
    }

    #[tokio::test]
    async fn a_retried_rename_does_not_merge_memory_it_already_moved() {
        let tmp = TempDir::new().unwrap();
        let before = fixture(&tmp, &[FROM]);
        seed_workspace(&before, FROM);
        seed_memory(&before, FROM, 3).await;
        let after = arm_and_commit(&before).await;
        let blocker = block_destination(&after);
        let stores = SurfaceStores::none();

        let first = converge(&after, FROM, TO, &stores).await.unwrap();
        assert_eq!(lagging_followers(&first), vec![FollowerKind::Workspace]);
        assert_eq!(memory_rows(&after, TO).await, 3);

        std::fs::remove_file(&blocker).unwrap();
        let second = converge(&after, FROM, TO, &stores).await.unwrap();
        assert!(second.is_converged(), "{:?}", second.warnings());
        assert_eq!(memory_rows(&after, TO).await, 3);
        assert_eq!(memory_rows(&after, FROM).await, 0);
    }

    #[tokio::test]
    async fn a_genuine_memory_handle_is_renamed_through_exactly_once() {
        let tmp = TempDir::new().unwrap();
        let before = fixture(&tmp, &[FROM]);
        seed_workspace(&before, FROM);
        let after = arm_and_commit(&before).await;
        let blocker = block_destination(&after);
        let counting = Arc::new(CountingMemory::default());
        counting.agents.lock().insert(FROM.to_string(), 1);
        let memory: Arc<dyn Memory> = counting.clone();
        let stores = SurfaceStores {
            memory: Some(&memory),
            ..SurfaceStores::none()
        };

        let first = converge(&after, FROM, TO, &stores).await.unwrap();
        assert_eq!(lagging_followers(&first), vec![FollowerKind::Workspace]);
        assert_eq!(first.report().memory_rows, 1);

        std::fs::remove_file(&blocker).unwrap();
        let second = converge(&after, FROM, TO, &stores).await.unwrap();
        assert!(second.is_converged(), "{:?}", second.warnings());
        assert_eq!(
            counting.renames.load(Ordering::SeqCst),
            1,
            "a follower with nothing under the old alias is not renamed again"
        );
        assert_eq!(counting.agents.lock().get(TO).copied(), Some(1));
    }

    #[tokio::test]
    async fn a_none_memory_placeholder_does_not_hide_the_configured_store() {
        let tmp = TempDir::new().unwrap();
        let before = fixture(&tmp, &[FROM]);
        seed_memory(&before, FROM, 2).await;
        let after = arm_and_commit(&before).await;
        // What a surface holds when its configured backend failed to build.
        let placeholder: Arc<dyn Memory> = Arc::new(NoneMemory::new("none"));
        let stores = SurfaceStores {
            memory: Some(&placeholder),
            ..SurfaceStores::none()
        };

        let outcome = converge(&after, FROM, TO, &stores).await.unwrap();
        assert!(outcome.is_converged(), "{:?}", outcome.warnings());
        assert_eq!(outcome.report().memory_rows, 1);
        assert_eq!(memory_rows(&after, TO).await, 2);
    }

    #[tokio::test]
    async fn run_history_left_under_the_old_alias_is_found_and_moved() {
        let tmp = TempDir::new().unwrap();
        let after = committed(&fixture(&tmp, &[FROM]));
        // A finished one-shot: its job row is gone, and its run row is still
        // cleanup-owned by the old alias.
        let now = chrono::Utc::now();
        crate::cron::record_run(
            &after,
            "finished-one-shot",
            now,
            now,
            "ok",
            crate::cron::RunOutcomes {
                execution: "ok",
                delivery: "not_required",
                persistence: "not_bound",
            },
            crate::cron::RunProvenance {
                principal: None,
                executing_agent: Some(FROM),
                job_source: Some("imperative"),
            },
            Some("done"),
            1,
        )
        .unwrap();
        let stores = SurfaceStores::none();

        assert_eq!(
            resolve(&after, FROM, TO, &stores).await.unwrap(),
            Disposition::Resume
        );
        let outcome = converge(&after, FROM, TO, &stores).await.unwrap();
        assert!(outcome.is_converged(), "{:?}", outcome.warnings());
        assert_eq!(outcome.report().cron_jobs, 0, "only run history moved");
        assert_eq!(cron_residue(&after, FROM), Some(0));
        assert_eq!(cron_residue(&after, TO), Some(1));
    }

    #[tokio::test]
    async fn invalid_or_reserved_aliases_are_refused_before_touching_disk() {
        let tmp = TempDir::new().unwrap();
        let config = fixture(&tmp, &[FROM]);
        let stores = SurfaceStores::none();

        for (from, to) in [
            ("../x", TO),
            (FROM, "../x"),
            ("default", TO),
            (FROM, "default"),
            (FROM, FROM),
        ] {
            let refusals = [
                resolve(&config, from, to, &stores).await.unwrap_err(),
                arm(&config, from, to).await.unwrap_err(),
                converge(&config, from, to, &stores).await.unwrap_err(),
            ];
            for refusal in refusals {
                let expected = if from == "default" || to == "default" {
                    matches!(refusal, RenameRecoveryError::ReservedAlias { .. })
                } else {
                    matches!(refusal, RenameRecoveryError::InvalidAlias { .. })
                };
                assert!(expected, "{from} -> {to}: {refusal:?}");
            }
        }
        assert!(!config.data_dir.exists(), "no journal or lock was created");
        let agents_dir = config
            .default_agent_workspace_dir(FROM)
            .parent()
            .and_then(Path::parent)
            .unwrap()
            .to_path_buf();
        assert!(
            !agents_dir.exists(),
            "nothing under the agents directory was created"
        );
    }

    #[tokio::test]
    async fn a_configured_legacy_alias_renames_end_to_end() {
        let tmp = TempDir::new().unwrap();
        let before = fixture(&tmp, &[LEGACY]);
        assert!(
            zeroclaw_config::helpers::validate_alias_key(LEGACY).is_err(),
            "the fixture alias must predate the alias grammar"
        );
        let source = seed_workspace(&before, LEGACY);
        seed_memory(&before, LEGACY, 1).await;
        seed_cron(&before, LEGACY);
        seed_acp(&before, "acp-legacy", LEGACY, &source);
        seed_session(&before, "chat-legacy", LEGACY);
        let stores = SurfaceStores::none();

        assert_eq!(
            resolve(&before, LEGACY, MIGRATED, &stores).await.unwrap(),
            Disposition::Fresh
        );
        let armed = arm(&before, LEGACY, MIGRATED).await.unwrap();
        let after = renamed(&before, LEGACY, MIGRATED);
        acknowledge_commit(&after, armed).await;
        assert!(retired_alias(&after, LEGACY).unwrap().is_some());

        let outcome = converge(&after, LEGACY, MIGRATED, &stores).await.unwrap();
        assert!(outcome.is_converged(), "{:?}", outcome.warnings());
        let report = outcome.report();
        assert!(report.workspace_moved);
        assert_eq!(
            (
                report.memory_rows,
                report.cron_jobs,
                report.acp_sessions,
                report.acp_workspaces,
                report.sessions_repointed,
            ),
            (1, 1, 1, 1, 1)
        );
        let destination = after.default_agent_workspace_dir(MIGRATED);
        assert_eq!(
            std::fs::read_to_string(destination.join("MEMORY.md")).unwrap(),
            LEGACY
        );
        assert!(!source.exists());
        assert_eq!(memory_rows(&after, MIGRATED).await, 1);
        assert_eq!(cron_residue(&after, MIGRATED), Some(1));
        assert_eq!(
            acp_row(&after, "acp-legacy"),
            (MIGRATED.to_string(), destination)
        );
        assert_eq!(
            session_owner(&after, "chat-legacy").as_deref(),
            Some(MIGRATED)
        );
        assert!(records(&after).is_empty());
    }

    #[tokio::test]
    async fn a_recorded_rename_of_a_legacy_alias_resumes_after_a_crash() {
        let tmp = TempDir::new().unwrap();
        let before = fixture(&tmp, &[LEGACY]);
        let source = seed_workspace(&before, LEGACY);
        seed_cron(&before, LEGACY);
        let armed = arm(&before, LEGACY, MIGRATED).await.unwrap();
        let after = renamed(&before, LEGACY, MIGRATED);
        acknowledge_commit(&after, armed).await;
        // The process stops here. The old alias is no longer configured, so
        // only the journal record still names it.
        assert!(after.agent(LEGACY).is_none());
        assert!(source.exists());
        let stores = SurfaceStores::none();

        assert_eq!(
            resolve(&after, LEGACY, MIGRATED, &stores).await.unwrap(),
            Disposition::Resume
        );
        let outcome = converge(&after, LEGACY, MIGRATED, &stores).await.unwrap();
        assert!(outcome.is_converged(), "{:?}", outcome.warnings());
        assert!(outcome.report().workspace_moved);
        assert_eq!(outcome.report().cron_jobs, 1);
        assert!(!source.exists());
        assert!(records(&after).is_empty());

        // Once the record is cleared nothing names the old alias, so a request
        // naming it again is held to the alias grammar.
        assert!(matches!(
            resolve(&after, LEGACY, MIGRATED, &stores).await,
            Err(RenameRecoveryError::InvalidAlias { alias, .. }) if alias == LEGACY
        ));
    }

    #[tokio::test]
    async fn a_custom_workspace_never_moves() {
        let tmp = TempDir::new().unwrap();
        let mut before = fixture(&tmp, &[FROM]);
        let custom = tmp.path().join("elsewhere");
        std::fs::create_dir_all(&custom).unwrap();
        std::fs::write(custom.join("MEMORY.md"), "custom").unwrap();
        before.agents.get_mut(FROM).unwrap().workspace.path = Some(custom.clone());
        let after = committed(&before);
        assert_eq!(after.agent_workspace_dir(TO), custom);
        let stores = SurfaceStores::none();

        // Nothing is at the old default location, so a rename committed
        // without a record left nothing behind.
        let err = resolve(&after, FROM, TO, &stores).await.unwrap_err();
        assert!(
            matches!(err, RenameRecoveryError::NotConfigured { .. }),
            "{err:?}"
        );
        assert!(!after.data_dir.exists());

        // A directory at the old default location is not this agent's
        // workspace, but an agent re-created under the old alias would adopt
        // it, so a recorded rename does not finish while it holds anything.
        let leftover = seed_workspace(&before, FROM);
        let armed = arm(&before, FROM, TO).await.unwrap();
        acknowledge_commit(&after, armed).await;
        assert_eq!(records(&after)[0].source_workspace, None);
        let outcome = converge(&after, FROM, TO, &stores).await.unwrap();
        let issues = outstanding(&outcome);
        assert_eq!(issues.len(), 1, "{:?}", outcome.warnings());
        assert_eq!(issues[0].kind, FollowerIssueKind::Conflict);
        assert_eq!(
            issues[0].to_string(),
            format!(
                "the old default workspace of `{FROM}` still exists at {}; move its contents by hand, then re-run the rename",
                leftover.display()
            )
        );
        assert!(!outcome.report().workspace_moved);
        assert!(leftover.join("MEMORY.md").is_file(), "nothing is deleted");
        assert!(retired_alias(&after, FROM).unwrap().is_some());

        // Emptied by hand, it is removed and the rename finishes; neither
        // workspace ever moved.
        std::fs::remove_file(leftover.join("MEMORY.md")).unwrap();
        let outcome = converge(&after, FROM, TO, &stores).await.unwrap();
        assert!(outcome.is_converged(), "{:?}", outcome.warnings());
        assert!(!outcome.report().workspace_moved);
        assert!(!leftover.parent().unwrap().exists());
        assert_eq!(
            std::fs::read_to_string(custom.join("MEMORY.md")).unwrap(),
            "custom"
        );
        assert!(!after.default_agent_workspace_dir(TO).exists());
        assert!(records(&after).is_empty());
    }

    #[tokio::test]
    async fn an_old_alias_pinned_to_its_default_workspace_never_shares_it() {
        let tmp = TempDir::new().unwrap();
        let mut before = fixture(&tmp, &[FROM]);
        let old = seed_workspace(&before, FROM);
        // `from` names its own default location as an explicit path, which
        // the rename hands to `to` unchanged.
        before.agents.get_mut(FROM).unwrap().workspace.path = Some(old.clone());
        let mut after = arm_and_commit(&before).await;
        assert_eq!(
            records(&after)[0].source_workspace,
            None,
            "an explicit path is recorded as custom"
        );
        assert_eq!(after.agent_workspace_dir(TO), old);
        let stores = SurfaceStores::none();

        let outcome = converge(&after, FROM, TO, &stores).await.unwrap();
        let issues = outstanding(&outcome);
        assert_eq!(issues.len(), 1, "{:?}", outcome.warnings());
        assert_eq!(issues[0].kind, FollowerIssueKind::Conflict);
        assert_eq!(
            issues[0].to_string(),
            format!(
                "agent `{TO}` uses the old default workspace of `{FROM}` as its workspace.path; point workspace.path elsewhere, then re-run the rename"
            )
        );
        assert!(old.join("MEMORY.md").is_file(), "nothing moved or deleted");
        assert!(retired_alias(&after, FROM).unwrap().is_some());
        // The same path spelled another way is the same directory.
        after.agents.get_mut(TO).unwrap().workspace.path = Some(old.join("..").join("workspace"));
        let spelled = converge(&after, FROM, TO, &stores).await.unwrap();
        assert_eq!(outstanding(&spelled)[0].kind, FollowerIssueKind::Conflict);

        // Pointed elsewhere, the old directory still holds files.
        let elsewhere = tmp.path().join("elsewhere");
        after.agents.get_mut(TO).unwrap().workspace.path = Some(elsewhere.clone());
        let outcome = converge(&after, FROM, TO, &stores).await.unwrap();
        let issues = outstanding(&outcome);
        assert_eq!(issues.len(), 1, "{:?}", outcome.warnings());
        assert_eq!(
            issues[0].to_string(),
            format!(
                "the old default workspace of `{FROM}` still exists at {}; move its contents by hand, then re-run the rename",
                old.display()
            )
        );

        // Once its files are moved by hand the empty directory is removed.
        std::fs::create_dir_all(&elsewhere).unwrap();
        std::fs::rename(old.join("MEMORY.md"), elsewhere.join("MEMORY.md")).unwrap();
        let outcome = converge(&after, FROM, TO, &stores).await.unwrap();
        assert!(outcome.is_converged(), "{:?}", outcome.warnings());
        assert!(!outcome.report().workspace_moved);
        assert!(!old.parent().unwrap().exists());
        assert!(elsewhere.join("MEMORY.md").is_file());
        assert!(records(&after).is_empty());
    }

    #[tokio::test]
    async fn a_custom_path_set_after_a_conflict_does_not_strand_the_old_workspace() {
        let tmp = TempDir::new().unwrap();
        let before = fixture(&tmp, &[FROM]);
        let old = seed_workspace(&before, FROM);
        let mut after = arm_and_commit(&before).await;
        let destination = after.default_agent_workspace_dir(TO);
        std::fs::create_dir_all(&destination).unwrap();
        std::fs::write(destination.join("keep.md"), "theirs").unwrap();
        let stores = SurfaceStores::none();
        let first = converge(&after, FROM, TO, &stores).await.unwrap();
        assert_eq!(outstanding(&first)[0].kind, FollowerIssueKind::Conflict);

        // Rather than clearing the destination, the operator gives the new
        // alias a custom workspace and re-runs the rename.
        let custom = tmp.path().join("custom");
        after.agents.get_mut(TO).unwrap().workspace.path = Some(custom.clone());
        let second = converge(&after, FROM, TO, &stores).await.unwrap();
        let issues = outstanding(&second);
        assert_eq!(issues.len(), 1, "{:?}", second.warnings());
        assert_eq!(issues[0].kind, FollowerIssueKind::Conflict);
        assert_eq!(
            issues[0].to_string(),
            format!(
                "the old default workspace of `{FROM}` still exists at {}; move its contents by hand, then re-run the rename",
                old.display()
            )
        );
        assert!(old.join("MEMORY.md").is_file());
        let mut recreated = after.clone();
        assert!(
            matches!(
                alias_refs::create_map_key_checked(&mut recreated, "agents", FROM),
                Err(alias_refs::CreateError::Retired { .. })
            ),
            "the old alias stays retired while its old workspace is there"
        );

        std::fs::create_dir_all(&custom).unwrap();
        std::fs::rename(old.join("MEMORY.md"), custom.join("MEMORY.md")).unwrap();
        let third = converge(&after, FROM, TO, &stores).await.unwrap();
        assert!(third.is_converged(), "{:?}", third.warnings());
        assert!(!old.exists());
        assert_eq!(
            std::fs::read_to_string(destination.join("keep.md")).unwrap(),
            "theirs"
        );
        assert!(records(&after).is_empty());
    }

    #[tokio::test]
    async fn a_recorded_workspace_outside_this_install_is_never_moved() {
        let tmp = TempDir::new().unwrap();
        let before = fixture(&tmp, &[FROM]);
        let old = seed_workspace(&before, FROM);
        let after = arm_and_commit(&before).await;
        // The journal names a workspace that is not the old alias's default
        // location under this config, as after the install moved.
        let elsewhere = tmp.path().join("elsewhere");
        std::fs::create_dir_all(&elsewhere).unwrap();
        std::fs::write(elsewhere.join("MEMORY.md"), "elsewhere").unwrap();
        let journal = AgentRecoveryJournal::for_config(&after);
        let mut record = records(&after).remove(0);
        record.source_workspace = Some(elsewhere.clone());
        let guard = journal.lock(Duration::ZERO).unwrap();
        journal.upsert(&guard, record).unwrap();
        drop(guard);

        let outcome = converge(&after, FROM, TO, &SurfaceStores::none())
            .await
            .unwrap();
        let issues = outstanding(&outcome);
        assert_eq!(issues.len(), 1, "{:?}", outcome.warnings());
        assert_eq!(issues[0].kind, FollowerIssueKind::Conflict);
        assert_eq!(
            issues[0].to_string(),
            format!(
                "the recorded workspace {} is not the default workspace of `{FROM}` under this config",
                elsewhere.display()
            )
        );
        assert!(!outcome.report().workspace_moved);
        assert!(elsewhere.join("MEMORY.md").is_file());
        assert!(old.join("MEMORY.md").is_file());
        assert!(!after.default_agent_workspace_dir(TO).exists());
        assert_eq!(records(&after).len(), 1);
    }

    #[tokio::test]
    async fn a_case_only_rename_converges_on_either_kind_of_filesystem() {
        const UPPER: &str = "Scout";
        let tmp = TempDir::new().unwrap();
        let before = fixture(&tmp, &[UPPER]);
        let source = seed_workspace(&before, UPPER);
        seed_acp(&before, "acp-case", UPPER, &source);
        let destination = before.default_agent_workspace_dir(FROM);
        // Whether the host folds case decides what converging means.
        let one_directory = same_existing_file(&source, &destination).unwrap();

        let armed = arm(&before, UPPER, FROM).await.unwrap();
        let after = renamed(&before, UPPER, FROM);
        acknowledge_commit(&after, armed).await;
        let outcome = converge(&after, UPPER, FROM, &SurfaceStores::none())
            .await
            .unwrap();
        assert!(outcome.is_converged(), "{:?}", outcome.warnings());
        assert_eq!(
            std::fs::read_to_string(destination.join("MEMORY.md")).unwrap(),
            UPPER
        );
        if one_directory {
            assert!(
                !outcome.report().workspace_moved,
                "one directory under both names is already in place"
            );
            assert!(source.join("MEMORY.md").is_file());
        } else {
            assert!(outcome.report().workspace_moved);
            assert!(!source.parent().unwrap().exists());
        }
        assert_eq!(acp_row(&after, "acp-case"), (FROM.to_string(), destination));
        assert!(records(&after).is_empty());
    }

    #[cfg(unix)]
    #[tokio::test]
    async fn a_workspace_shared_through_a_symlink_is_not_converged() {
        let tmp = TempDir::new().unwrap();
        let before = fixture(&tmp, &[FROM]);
        let old = seed_workspace(&before, FROM);
        let after = arm_and_commit(&before).await;
        // The new alias's directory is a link to the old one, so both default
        // workspaces are one directory without being a case-only rename.
        std::os::unix::fs::symlink(
            old.parent().unwrap(),
            after.default_agent_workspace_dir(TO).parent().unwrap(),
        )
        .unwrap();
        assert!(same_existing_file(&old, &after.default_agent_workspace_dir(TO)).unwrap());

        let outcome = converge(&after, FROM, TO, &SurfaceStores::none())
            .await
            .unwrap();
        assert_eq!(
            lagging_followers(&outcome),
            vec![FollowerKind::Workspace],
            "an agent re-created under the old alias would share it"
        );
        assert!(old.join("MEMORY.md").is_file());
        assert_eq!(records(&after).len(), 1);
    }

    #[tokio::test]
    async fn a_rename_whose_old_alias_is_configured_again_moves_nothing() {
        let tmp = TempDir::new().unwrap();
        let before = fixture(&tmp, &[FROM]);
        let source = seed_workspace(&before, FROM);
        seed_memory(&before, FROM, 2).await;
        let after = arm_and_commit(&before).await;
        // A hand edit brings the old alias back around the create guards.
        let mut readded = after.clone();
        readded.agents.insert(FROM.to_string(), agent());
        let stores = SurfaceStores::none();

        let refusals = [
            resolve(&readded, FROM, TO, &stores).await.unwrap_err(),
            arm(&readded, FROM, TO).await.unwrap_err(),
            converge(&readded, FROM, TO, &stores).await.unwrap_err(),
            resolve(&readded, FROM, "wolf", &stores).await.unwrap_err(),
            converge(&readded, FROM, "wolf", &stores).await.unwrap_err(),
        ];
        for refusal in &refusals {
            assert!(
                matches!(refusal, RenameRecoveryError::SourceReconfigured { from, to } if from == FROM && to == TO),
                "{refusal:?}"
            );
        }
        assert_eq!(
            refusals[0].to_string(),
            "agent `scout` is configured again while an earlier rename of it is unfinished; remove `[agents.scout]` from the config by hand, or abandon that rename, then retry"
        );
        assert!(
            !refusals[3].to_string().contains(TO),
            "a request naming another target must not learn the pending one: {}",
            refusals[3]
        );
        assert!(source.join("MEMORY.md").is_file(), "nothing moved");
        assert!(!after.default_agent_workspace_dir(TO).exists());
        assert_eq!(memory_identity(&after, FROM).await, 1);
        let recorded = records(&after);
        assert_eq!(recorded.len(), 1, "the record is untouched");
        assert_eq!(recorded[0].phase, RecoveryPhase::Committed);

        // With the hand-added entry removed again, the same rename finishes.
        let outcome = converge(&after, FROM, TO, &stores).await.unwrap();
        assert!(outcome.is_converged(), "{:?}", outcome.warnings());
        assert_eq!(memory_rows(&after, TO).await, 2);
    }

    #[tokio::test]
    async fn a_prepared_record_retires_the_old_alias_and_converge_commits_it() {
        let tmp = TempDir::new().unwrap();
        let before = fixture(&tmp, &[FROM]);
        seed_workspace(&before, FROM);
        let armed = arm(&before, FROM, TO).await.unwrap();
        let after = committed(&before);
        // The process stops before acknowledging the commit, and another door
        // then removes the target.
        drop(armed);
        let mut without_target = after.clone();
        without_target.agents.remove(TO);
        assert_eq!(records(&after)[0].phase, RecoveryPhase::Prepared);
        assert!(
            retired_alias(&without_target, FROM).unwrap().is_some(),
            "the old alias is gone, so it stays retired whatever became of the target"
        );
        assert!(matches!(
            ensure_alias_not_retired(&without_target, FROM).await,
            Err(RenameRecoveryError::AliasRetired { .. })
        ));

        // Converging commits the record before anything moves.
        let blocker = block_destination(&after);
        let stores = SurfaceStores::none();
        let outcome = converge(&after, FROM, TO, &stores).await.unwrap();
        assert!(!outcome.is_converged());
        assert_eq!(records(&after)[0].phase, RecoveryPhase::Committed);
        assert!(
            retired_alias(&before, FROM).unwrap().is_some(),
            "a committed record keeps the old alias retired even if it comes back"
        );

        std::fs::remove_file(&blocker).unwrap();
        let outcome = converge(&after, FROM, TO, &stores).await.unwrap();
        assert!(outcome.is_converged(), "{:?}", outcome.warnings());
        assert!(records(&after).is_empty());
    }

    #[tokio::test]
    async fn a_memory_merge_refusal_is_a_conflict() {
        let tmp = TempDir::new().unwrap();
        let before = fixture(&tmp, &[FROM]);
        seed_memory(&before, FROM, 2).await;
        // The new alias already owns memory of its own.
        seed_memory(&before, TO, 1).await;
        let after = arm_and_commit(&before).await;

        let outcome = converge(&after, FROM, TO, &SurfaceStores::none())
            .await
            .unwrap();
        let issues = outstanding(&outcome);
        assert_eq!(issues.len(), 1, "{:?}", outcome.warnings());
        assert_eq!(issues[0].follower, FollowerKind::Memory);
        assert_eq!(issues[0].kind, FollowerIssueKind::Conflict);
        assert_eq!(
            issues[0].to_string(),
            format!("memory for `{TO}` already exists; merge it by hand or abandon the rename")
        );
        assert_eq!(memory_rows(&after, FROM).await, 2);
        assert_eq!(memory_rows(&after, TO).await, 1);
        assert_eq!(records(&after).len(), 1);
    }

    #[tokio::test]
    async fn abandoning_a_rename_drops_only_its_record_and_reports_what_stays() {
        let tmp = TempDir::new().unwrap();
        let before = fixture(&tmp, &[FROM, "owl"]);
        let old = seed_workspace(&before, FROM);
        seed_memory(&before, FROM, 2).await;
        seed_cron(&before, FROM);
        let first = arm_and_commit(&before).await;
        let armed = arm(&first, "owl", "wolf").await.unwrap();
        let after = renamed(&first, "owl", "wolf");
        acknowledge_commit(&after, armed).await;

        // Only a record of exactly this rename is abandoned.
        for (from, to) in [(FROM, "wolf"), ("owl", TO), ("hawk", TO)] {
            assert!(
                matches!(
                    abandon_rename(&after, from, to).await,
                    Err(RenameRecoveryError::NotConfigured { alias }) if alias == from
                ),
                "{from} -> {to}"
            );
        }
        assert_eq!(records(&after).len(), 2);

        let abandoned = abandon_rename(&after, FROM, TO).await.unwrap();
        assert_eq!(
            (abandoned.record.from.as_str(), abandoned.record.to.as_str()),
            (FROM, TO)
        );
        let residue: Vec<(FollowerKind, String)> = abandoned
            .residue
            .iter()
            .map(|issue| (issue.follower, issue.to_string()))
            .collect();
        assert_eq!(
            residue,
            vec![
                (
                    FollowerKind::Workspace,
                    format!(
                        "the old default workspace of `{FROM}` still exists at {}",
                        old.display()
                    )
                ),
                (
                    FollowerKind::Memory,
                    format!("memory still attributes state to `{FROM}`")
                ),
                (
                    FollowerKind::Cron,
                    format!("cron still attributes state to `{FROM}`")
                ),
            ]
        );

        // The other rename's record is kept; nothing moved; no config was
        // written; and the old alias can be created again.
        let remaining = records(&after);
        assert_eq!(remaining.len(), 1);
        assert_eq!(
            (remaining[0].from.as_str(), remaining[0].to.as_str()),
            ("owl", "wolf")
        );
        assert!(old.join("MEMORY.md").is_file());
        assert_eq!(memory_identity(&after, FROM).await, 1);
        assert_eq!(cron_residue(&after, FROM), Some(1));
        assert!(!after.default_agent_workspace_dir(TO).exists());
        assert!(!after.config_path.exists());
        assert!(ensure_alias_not_retired(&after, FROM).await.is_ok());
        assert!(ensure_not_pending_target(&after, TO).await.is_ok());
        let mut recreated = after.clone();
        assert!(alias_refs::create_map_key_checked(&mut recreated, "agents", FROM).unwrap());

        assert!(matches!(
            abandon_rename(&after, FROM, TO).await,
            Err(RenameRecoveryError::NotConfigured { .. })
        ));
    }

    #[tokio::test]
    async fn a_rename_whose_old_alias_is_configured_again_can_be_abandoned() {
        let tmp = TempDir::new().unwrap();
        let before = fixture(&tmp, &[FROM]);
        let old = seed_workspace(&before, FROM);
        let after = arm_and_commit(&before).await;
        let mut readded = after.clone();
        readded.agents.insert(FROM.to_string(), agent());

        let abandoned = abandon_rename(&readded, FROM, TO).await.unwrap();
        assert_eq!(abandoned.residue.len(), 1, "{:?}", abandoned.residue);
        assert_eq!(abandoned.residue[0].follower, FollowerKind::Workspace);
        assert!(records(&after).is_empty());
        assert!(old.join("MEMORY.md").is_file());
        assert_eq!(
            resolve(&readded, FROM, TO, &SurfaceStores::none())
                .await
                .unwrap(),
            Disposition::Fresh,
            "the agent brought back is an ordinary agent again"
        );
    }

    #[tokio::test]
    async fn a_legacy_alias_that_names_more_than_one_directory_is_refused() {
        let tmp = TempDir::new().unwrap();
        let stores = SurfaceStores::none();
        for legacy in ["x/../../outside", "..", ".", "back\\slash", "nul\0byte", ""] {
            let config = fixture(&tmp, &[legacy]);
            let refusals = [
                resolve(&config, legacy, TO, &stores).await.unwrap_err(),
                arm(&config, legacy, TO).await.unwrap_err(),
                converge(&config, legacy, TO, &stores).await.unwrap_err(),
                abandon_rename(&config, legacy, TO).await.unwrap_err(),
            ];
            for refusal in refusals {
                assert!(
                    matches!(&refusal, RenameRecoveryError::InvalidAlias { alias, .. } if alias == legacy),
                    "{legacy:?}: {refusal:?}"
                );
            }
            assert!(!config.data_dir.exists(), "{legacy:?}: no journal or lock");
        }
        assert!(!tmp.path().join("agents").exists());
        // `<install>/agents/x/../../outside` is `<install>/outside`.
        assert!(!tmp.path().join("outside").exists());
    }

    #[tokio::test]
    async fn a_blocked_destination_keeps_the_record_until_the_workspace_settles() {
        let tmp = TempDir::new().unwrap();
        let before = fixture(&tmp, &[FROM]);
        let source = seed_workspace(&before, FROM);
        seed_cron(&before, FROM);
        seed_acp(&before, "acp-1", FROM, &source);
        let after = arm_and_commit(&before).await;
        let blocker = block_destination(&after);
        let stores = SurfaceStores::none();

        let outcome = converge(&after, FROM, TO, &stores).await.unwrap();
        assert_eq!(
            lagging_followers(&outcome),
            vec![FollowerKind::Workspace, FollowerKind::Acp]
        );
        assert_eq!(outstanding(&outcome)[0].kind, FollowerIssueKind::Unreadable);
        assert_eq!(
            outstanding(&outcome)[1].to_string(),
            format!("acp still attributes state to `{FROM}`")
        );
        assert_eq!(outcome.warnings().len(), 2);
        assert!(!outcome.report().workspace_moved);
        assert_eq!(outcome.report().cron_jobs, 1, "the other followers moved");
        assert_eq!(
            acp_row(&after, "acp-1"),
            (TO.to_string(), source.clone()),
            "the session keeps the directory the workspace is still in"
        );
        assert!(source.exists());
        assert_eq!(records(&after).len(), 1, "the record stays open");

        // Clearing the leftovers by hand does not clear the record.
        std::fs::remove_file(&blocker).unwrap();
        std::fs::remove_dir_all(&source).unwrap();
        assert!(retired_alias(&after, FROM).unwrap().is_some());

        let retried = converge(&after, FROM, TO, &stores).await.unwrap();
        assert!(retried.is_converged(), "{:?}", retried.warnings());
        assert_eq!(retried.report().acp_workspaces, 1);
        assert_eq!(
            acp_row(&after, "acp-1"),
            (TO.to_string(), after.default_agent_workspace_dir(TO))
        );
        assert!(records(&after).is_empty());
        assert_eq!(retired_alias(&after, FROM).unwrap(), None);
    }

    #[tokio::test]
    async fn a_non_empty_destination_conflicts_and_an_empty_one_is_replaced() {
        let tmp = TempDir::new().unwrap();
        let before = fixture(&tmp, &[FROM]);
        let source = seed_workspace(&before, FROM);
        let after = arm_and_commit(&before).await;
        let destination = after.default_agent_workspace_dir(TO);
        std::fs::create_dir_all(&destination).unwrap();
        std::fs::write(destination.join("keep.md"), "theirs").unwrap();
        let stores = SurfaceStores::none();

        let outcome = converge(&after, FROM, TO, &stores).await.unwrap();
        let issues = outstanding(&outcome);
        assert_eq!(issues.len(), 1, "{:?}", outcome.warnings());
        assert_eq!(issues[0].kind, FollowerIssueKind::Conflict);
        assert_eq!(
            issues[0].to_string(),
            format!(
                "workspace destination {} already exists and is not empty",
                destination.display()
            )
        );
        assert!(source.exists());
        assert_eq!(
            std::fs::read_to_string(destination.join("keep.md")).unwrap(),
            "theirs"
        );

        std::fs::remove_file(destination.join("keep.md")).unwrap();
        let retried = converge(&after, FROM, TO, &stores).await.unwrap();
        assert!(retried.is_converged(), "{:?}", retried.warnings());
        assert!(retried.report().workspace_moved);
        assert_eq!(
            std::fs::read_to_string(destination.join("MEMORY.md")).unwrap(),
            FROM
        );
        assert!(!source.exists());
    }

    #[tokio::test]
    async fn an_open_record_guards_both_of_its_aliases() {
        let tmp = TempDir::new().unwrap();
        let before = fixture(&tmp, &[FROM]);
        let after = arm_and_commit(&before).await;
        let stores = SurfaceStores::none();

        // Renaming the pending target away, or onto the retired alias, or the
        // retired alias anywhere else, is refused.
        let pending = resolve(&after, TO, "wolf", &stores).await.unwrap_err();
        assert!(
            matches!(&pending, RenameRecoveryError::RecoveryPending { from, to } if from == FROM && to == TO),
            "{pending:?}"
        );
        assert!(!pending.to_string().contains(FROM), "{pending}");
        let onto = resolve(&after, "owl", FROM, &stores).await.unwrap_err();
        assert!(
            matches!(&onto, RenameRecoveryError::AliasRetired { alias, pending_to } if alias == FROM && pending_to == TO),
            "{onto:?}"
        );
        assert!(!onto.to_string().contains(TO), "{onto}");
        let elsewhere = resolve(&after, FROM, "wolf", &stores).await.unwrap_err();
        assert!(
            matches!(&elsewhere, RenameRecoveryError::AliasRetired { alias, .. } if alias == FROM),
            "{elsewhere:?}"
        );

        // Renaming another agent into the pending target is refused as well.
        let mut with_owl = after.clone();
        with_owl.agents.insert("owl".to_string(), agent());
        assert!(matches!(
            resolve(&with_owl, "owl", TO, &stores).await,
            Err(RenameRecoveryError::RecoveryPending { .. })
        ));

        // The creation and deletion guards agree, and so do arm and converge.
        assert!(matches!(
            ensure_not_pending_target(&after, TO).await,
            Err(RenameRecoveryError::RecoveryPending { .. })
        ));
        assert!(matches!(
            ensure_alias_not_retired(&after, FROM).await,
            Err(RenameRecoveryError::AliasRetired { .. })
        ));
        assert!(ensure_alias_not_retired(&after, TO).await.is_ok());
        assert!(ensure_not_pending_target(&after, FROM).await.is_ok());
        assert!(ensure_alias_not_retired(&after, "owl").await.is_ok());
        // Arming from the config before the commit, where the retired alias is
        // still configured, is refused as a reconfigured old alias.
        assert!(matches!(
            arm(&before, FROM, "wolf").await,
            Err(RenameRecoveryError::SourceReconfigured { .. })
        ));
        assert!(matches!(
            converge(&after, FROM, "wolf", &stores).await,
            Err(RenameRecoveryError::AliasRetired { .. })
        ));
        assert_eq!(records(&after).len(), 1, "no refusal touched the record");
    }

    #[tokio::test]
    async fn an_abandoned_rename_leaves_no_record_and_moves_nothing() {
        let tmp = TempDir::new().unwrap();
        let before = fixture(&tmp, &[FROM]);
        let source = seed_workspace(&before, FROM);

        let armed = arm(&before, FROM, TO).await.unwrap();
        // While one surface is between its arm and its commit, no other can arm.
        assert!(matches!(
            arm(&before, FROM, "wolf").await,
            Err(RenameRecoveryError::Busy { .. })
        ));
        abandon(&before, armed).await;

        assert!(records(&before).is_empty());
        assert!(!AgentRecoveryJournal::for_config(&before).path().exists());
        assert!(source.exists());
        assert_eq!(retired_alias(&before, FROM).unwrap(), None);
        assert_eq!(
            resolve(&before, FROM, TO, &SurfaceStores::none())
                .await
                .unwrap(),
            Disposition::Fresh
        );
    }

    #[tokio::test]
    async fn acknowledge_and_abandon_defer_to_the_config_they_are_given() {
        let tmp = TempDir::new().unwrap();
        let before = fixture(&tmp, &[FROM]);

        // Acknowledged against a config still holding the old alias: the
        // record stays prepared and retires nothing.
        let armed = arm(&before, FROM, TO).await.unwrap();
        acknowledge_commit(&before, armed).await;
        assert_eq!(records(&before)[0].phase, RecoveryPhase::Prepared);
        assert_eq!(retired_alias(&before, FROM).unwrap(), None);

        // Abandoned against a config that shows the commit: the record stays,
        // so the rename can still be finished.
        let armed = arm(&before, FROM, TO).await.unwrap();
        let after = committed(&before);
        abandon(&after, armed).await;
        assert_eq!(records(&after).len(), 1);
        assert!(retired_alias(&after, FROM).unwrap().is_some());
    }

    #[tokio::test]
    async fn converge_refuses_a_rename_whose_commit_never_landed() {
        let tmp = TempDir::new().unwrap();
        let before = fixture(&tmp, &[FROM]);
        let source = seed_workspace(&before, FROM);
        let stores = SurfaceStores::none();

        assert!(matches!(
            converge(&before, FROM, TO, &stores).await,
            Err(RenameRecoveryError::InvalidAlias { .. })
        ));
        let armed = arm(&before, FROM, TO).await.unwrap();
        assert!(matches!(
            converge(&before, FROM, TO, &stores).await,
            Err(RenameRecoveryError::InvalidAlias { .. })
        ));
        abandon(&before, armed).await;
        assert!(source.exists());
    }

    #[tokio::test]
    async fn acp_rows_follow_every_spelling_of_the_moved_workspace() {
        let tmp = TempDir::new().unwrap();
        let real_install = tmp.path().join("install");
        std::fs::create_dir_all(&real_install).unwrap();
        // On unix the install root is reached through a symlink, so a row that
        // recorded the resolved path spells the workspace differently. Windows
        // resolves to a verbatim `\\?\` path, which differs as well.
        #[cfg(unix)]
        let install = {
            let link = tmp.path().join("install-link");
            std::os::unix::fs::symlink(&real_install, &link).unwrap();
            link
        };
        #[cfg(not(unix))]
        let install = real_install;
        let mut before = fixture(&tmp, &[FROM]);
        before.config_path = install.join("config.toml");
        let source = seed_workspace(&before, FROM);
        let canonical = std::fs::canonicalize(&source).unwrap();
        assert_ne!(canonical, source, "the workspace must have two spellings");
        let custom = tmp.path().join("elsewhere");
        seed_acp(&before, "raw", FROM, &source);
        seed_acp(&before, "canonical", FROM, &canonical);
        seed_acp(&before, "nested", "other", &source.join("proj"));
        seed_acp(&before, "custom", FROM, &custom);

        let after = arm_and_commit(&before).await;
        let outcome = converge(&after, FROM, TO, &SurfaceStores::none())
            .await
            .unwrap();
        assert!(outcome.is_converged(), "{:?}", outcome.warnings());
        assert_eq!(outcome.report().acp_sessions, 3);
        assert_eq!(outcome.report().acp_workspaces, 3);

        let destination = after.default_agent_workspace_dir(TO);
        assert_eq!(
            acp_row(&after, "raw"),
            (TO.to_string(), destination.clone())
        );
        assert_eq!(
            acp_row(&after, "canonical"),
            (TO.to_string(), destination.clone())
        );
        assert_eq!(
            acp_row(&after, "nested"),
            ("other".to_string(), destination.join("proj"))
        );
        assert_eq!(acp_row(&after, "custom"), (TO.to_string(), custom));
    }

    #[cfg(unix)]
    #[tokio::test]
    async fn acp_rows_follow_a_workspace_an_earlier_run_already_moved() {
        let tmp = TempDir::new().unwrap();
        let real_install = tmp.path().join("install");
        std::fs::create_dir_all(&real_install).unwrap();
        let install = tmp.path().join("install-link");
        std::os::unix::fs::symlink(&real_install, &install).unwrap();
        let mut before = fixture(&tmp, &[FROM]);
        before.config_path = install.join("config.toml");
        let source = seed_workspace(&before, FROM);
        let canonical = std::fs::canonicalize(&source).unwrap();
        seed_acp(&before, "canonical", FROM, &canonical.join("proj"));
        let after = arm_and_commit(&before).await;

        // An earlier run moved the workspace and removed the old alias
        // directory, then stopped before the ACP rows followed.
        let destination = after.default_agent_workspace_dir(TO);
        std::fs::create_dir_all(destination.parent().unwrap()).unwrap();
        std::fs::rename(&source, &destination).unwrap();
        std::fs::remove_dir(source.parent().unwrap()).unwrap();

        let outcome = converge(&after, FROM, TO, &SurfaceStores::none())
            .await
            .unwrap();
        assert!(outcome.is_converged(), "{:?}", outcome.warnings());
        assert!(!outcome.report().workspace_moved);
        assert_eq!(
            acp_row(&after, "canonical"),
            (TO.to_string(), destination.join("proj"))
        );
    }

    #[tokio::test]
    async fn converging_a_converged_rename_again_changes_nothing() {
        let tmp = TempDir::new().unwrap();
        let before = fixture(&tmp, &[FROM]);
        seed_every_follower(&before).await;
        let after = arm_and_commit(&before).await;
        let stores = SurfaceStores::none();
        let first = converge(&after, FROM, TO, &stores).await.unwrap();
        assert!(first.is_converged(), "{:?}", first.warnings());
        let destination = after.default_agent_workspace_dir(TO);

        let again = converge(&after, FROM, TO, &stores).await.unwrap();
        assert!(again.is_converged(), "{:?}", again.warnings());
        let report = again.report();
        assert!(!report.workspace_moved);
        assert_eq!(
            (
                report.memory_rows,
                report.cron_jobs,
                report.acp_sessions,
                report.acp_workspaces,
                report.sessions_repointed,
            ),
            (0, 0, 0, 0, 0)
        );
        assert_eq!(
            std::fs::read_to_string(destination.join("MEMORY.md")).unwrap(),
            FROM
        );
        assert_eq!(memory_rows(&after, TO).await, 2);
        assert_eq!(cron_residue(&after, TO), Some(1));
        assert_eq!(acp_row(&after, "acp-1"), (TO.to_string(), destination));
        assert_eq!(session_owner(&after, "chat-1").as_deref(), Some(TO));
        assert!(records(&after).is_empty());
    }

    #[tokio::test]
    async fn a_corrupt_journal_fails_every_entry_point_closed() {
        let tmp = TempDir::new().unwrap();
        let config = fixture(&tmp, &[FROM]);
        let journal = AgentRecoveryJournal::for_config(&config);
        std::fs::create_dir_all(&config.data_dir).unwrap();
        std::fs::write(journal.path(), "{not json").unwrap();
        let stores = SurfaceStores::none();

        let err = resolve(&config, FROM, TO, &stores).await.unwrap_err();
        assert!(
            matches!(&err, RenameRecoveryError::Unreadable { store, .. } if store == JOURNAL_STORE),
            "{err:?}"
        );
        assert!(matches!(
            arm(&config, FROM, TO).await,
            Err(RenameRecoveryError::Unreadable { .. })
        ));
        assert!(matches!(
            converge(&config, FROM, TO, &stores).await,
            Err(RenameRecoveryError::Unreadable { .. })
        ));
        assert!(matches!(
            ensure_alias_not_retired(&config, FROM).await,
            Err(RenameRecoveryError::Unreadable { .. })
        ));
        assert!(matches!(
            ensure_not_pending_target(&config, TO).await,
            Err(RenameRecoveryError::Unreadable { .. })
        ));
        assert_eq!(
            std::fs::read_to_string(journal.path()).unwrap(),
            "{not json"
        );
    }

    #[test]
    fn warning_lines_and_errors_keep_their_shapes() {
        let names: Vec<String> = FollowerKind::ALL.iter().map(ToString::to_string).collect();
        assert_eq!(names, ["workspace", "memory", "cron", "acp", "sessions"]);

        let unreadable = FollowerIssue::unreadable(FollowerKind::Cron, "not a database".into());
        assert_eq!(
            unreadable.to_string(),
            "cron could not be read: not a database"
        );
        assert_eq!(
            serde_json::to_value(&unreadable).unwrap(),
            serde_json::json!({
                "follower": "cron",
                "kind": "unreadable",
                "detail": "not a database",
            })
        );
        let lagging = FollowerIssue::lagging(FollowerKind::Memory, "memory rename: boom".into());
        assert_eq!(lagging.to_string(), "memory rename: boom");

        let outcome = ConvergeOutcome::Incomplete {
            report: ConvergeReport::default(),
            outstanding: vec![unreadable, lagging],
        };
        assert!(!outcome.is_converged());
        assert_eq!(
            outcome.warnings(),
            [
                "cron could not be read: not a database",
                "memory rename: boom"
            ]
        );

        let error = |e: RenameRecoveryError| e.to_string();
        assert_eq!(
            error(RenameRecoveryError::NotConfigured { alias: FROM.into() }),
            "agents.scout is not configured"
        );
        assert_eq!(
            error(RenameRecoveryError::AliasRetired {
                alias: FROM.into(),
                pending_to: TO.into(),
            }),
            "alias `scout` is retired by an unfinished agent rename and cannot be reused yet"
        );
        assert_eq!(
            error(RenameRecoveryError::RecoveryPending {
                from: FROM.into(),
                to: TO.into(),
            }),
            "agent `ranger` is the target of an unfinished rename; re-run that rename first"
        );
        assert_eq!(
            error(RenameRecoveryError::SourceReconfigured {
                from: FROM.into(),
                to: TO.into(),
            }),
            "agent `scout` is configured again while an earlier rename of it is unfinished; remove `[agents.scout]` from the config by hand, or abandon that rename, then retry"
        );
        let conflict = FollowerIssue::conflict(FollowerKind::Memory, "taken".into());
        assert_eq!(conflict.to_string(), "taken");
        assert_eq!(
            serde_json::to_value(&conflict).unwrap()["kind"],
            serde_json::json!("conflict")
        );
        assert_eq!(
            error(RenameRecoveryError::Unreadable {
                store: "acp".into(),
                detail: "locked".into(),
            }),
            "acp could not be read: locked"
        );
        assert_eq!(
            error(RenameRecoveryError::Busy {
                detail: "held".into()
            }),
            "agent rename recovery is in progress elsewhere; retry shortly (held)"
        );
        assert_eq!(
            error(RenameRecoveryError::Persist {
                detail: "full".into()
            }),
            "agent rename recovery could not be recorded: full"
        );
    }

    /// Surfaces await these inside handler futures with a 16 KiB budget, so
    /// each must be `Send` and leave most of that budget to the handler.
    #[test]
    fn entry_points_are_send_and_their_futures_stay_small() {
        fn send<T: Send>(value: T) -> T {
            value
        }
        send(Option::<Armed>::None);
        let config = Config::default();
        let stores = SurfaceStores::none();
        let sizes = [
            (
                "resolve",
                std::mem::size_of_val(&send(resolve(&config, FROM, TO, &stores))),
            ),
            ("arm", std::mem::size_of_val(&send(arm(&config, FROM, TO)))),
            (
                "converge",
                std::mem::size_of_val(&send(converge(&config, FROM, TO, &stores))),
            ),
            (
                "ensure_alias_not_retired",
                std::mem::size_of_val(&send(ensure_alias_not_retired(&config, FROM))),
            ),
            (
                "ensure_not_pending_target",
                std::mem::size_of_val(&send(ensure_not_pending_target(&config, TO))),
            ),
            (
                "abandon_rename",
                std::mem::size_of_val(&send(abandon_rename(&config, FROM, TO))),
            ),
        ];
        for (name, size) in sizes {
            assert!(size <= 4 * 1024, "the {name} future is {size} bytes");
        }
    }
}
