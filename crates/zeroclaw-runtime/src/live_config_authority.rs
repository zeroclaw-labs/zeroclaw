use std::collections::HashMap;
use std::fs::{File, OpenOptions, TryLockError};
use std::future::Future;
use std::path::{Path, PathBuf};
use std::sync::Arc;

use anyhow::{Context, Result};
use zeroclaw_config::live::{LiveConfig, LiveConfigHandle};
use zeroclaw_config::schema::Config;

use crate::config_application::{
    ConfigApplicationLedger, ConfigApplicationStatus, ConfigApplicationTarget,
};

#[cfg(unix)]
use std::os::unix::fs::{MetadataExt, OpenOptionsExt};

/// The live configuration state shared by one supervised daemon generation.
///
/// The write lock is deliberately paired with the published-config storage
/// so every mutation path uses the same serialization witness as the live
/// state. The published pair (config plus its opaque revision) lives in
/// [`zeroclaw_config::live`]; this authority owns who may publish and when.
///
/// Readers receive [`LiveConfigHandle`] through [`Self::live_handle`]; the
/// writable storage is never exposed. Writers admit through
/// [`Self::begin_config_commit`], which serializes on the writer mutex and
/// admits a generation write lease as one unit.
#[derive(Clone)]
pub struct LiveConfigAuthority {
    live: LiveConfig,
    config_write_lock: Arc<tokio::sync::Mutex<()>>,
    agent_lifecycle: AgentLifecycleCoordinator,
    application_results: Arc<parking_lot::Mutex<ConfigApplicationLedger>>,
}

impl LiveConfigAuthority {
    /// Create the authority for one daemon generation. The initial config
    /// is the first publication of a fresh epoch.
    pub fn new(config: Config) -> Self {
        Self {
            live: LiveConfig::new(config),
            config_write_lock: zeroclaw_config::write_lock::shared_config_write_lock(),
            agent_lifecycle: AgentLifecycleCoordinator::default(),
            application_results: Arc::default(),
        }
    }

    /// Test-only authority whose config write lock is private to it.
    ///
    /// A unit-test binary is one process, so authorities built with
    /// [`Self::new`] share one lock across every test in it: a test holding
    /// its context's guard stalls every other test's config writers, and
    /// parallel runs queue timing-bounded tests behind unrelated ones.
    /// Clones of this authority still share its lock, as they do in
    /// production.
    #[cfg(test)]
    pub(crate) fn for_tests(config: Config) -> Self {
        Self {
            config_write_lock: Arc::new(tokio::sync::Mutex::new(())),
            ..Self::new(config)
        }
    }

    /// Test-only authority with its own published config and lifecycle that
    /// serializes on `writer`'s config write lock: another connection to the
    /// daemon `writer` belongs to, for a fixture that needs a different
    /// config than the first connection's.
    #[cfg(test)]
    pub(crate) fn for_tests_sharing_writer(config: Config, writer: &Self) -> Self {
        Self {
            config_write_lock: Arc::clone(&writer.config_write_lock),
            ..Self::new(config)
        }
    }

    /// Create an authority that exclusively owns this config across processes.
    pub fn new_owned(config: Config) -> Result<Self> {
        let ownership = ConfigOwnershipGuard::acquire(&config.data_dir)?;
        ownership.recover_abandoned_turns_or_log();
        Ok(Self::new_with_ownership(config, ownership))
    }

    /// Create an authority from a guard acquired by a caller that resolved the
    /// config identity before loading the executable config. The guard is
    /// transferred into the authority and shared by every derived capability.
    /// The loaded config becomes the initial publication of a fresh epoch: a
    /// full daemon reload is a new publication domain, never a continuation
    /// of the retired generation's sequence.
    pub fn new_with_ownership(config: Config, ownership: ConfigOwnershipGuard) -> Self {
        Self {
            live: LiveConfig::new(config),
            config_write_lock: zeroclaw_config::write_lock::shared_config_write_lock(),
            agent_lifecycle: AgentLifecycleCoordinator::with_ownership(ownership),
            application_results: Arc::default(),
        }
    }

    /// Return the read-only live-config handle shared by all consumers of
    /// this authority. The handle observes the config and its revision as
    /// one pair and exposes no write path.
    pub fn live_handle(&self) -> LiveConfigHandle {
        self.live.handle()
    }

    /// Clone the currently published config.
    pub fn snapshot_config(&self) -> Config {
        self.live.snapshot()
    }

    /// The currently published revision.
    pub fn published_revision(&self) -> zeroclaw_config::live::ConfigRevision {
        self.live.published_revision()
    }

    /// Application facts for the daemon, without exposing RPC sessions.
    pub fn application_status(&self) -> ConfigApplicationStatus {
        let results = self.application_results.lock();
        results.status(self.live.published_revision(), |target| {
            matches!(target, ConfigApplicationTarget::Daemon)
        })
    }

    /// The caller holds the live-session map through this synchronous read.
    /// Visibility filtering must not retire another caller's live targets.
    pub(crate) fn application_status_for_sessions(
        &self,
        current: impl Fn(&ConfigApplicationTarget) -> bool,
        visible: impl Fn(&ConfigApplicationTarget) -> bool,
    ) -> ConfigApplicationStatus {
        let mut results = self.application_results.lock();
        results.retire_targets(current);
        results.status(self.live.published_revision(), visible)
    }

    pub(crate) fn complete_application(
        &self,
        revision: zeroclaw_config::live::ConfigRevision,
        target: &ConfigApplicationTarget,
        paths: &[Vec<String>],
        applied: bool,
    ) {
        self.application_results
            .lock()
            .complete(revision, target, paths, applied);
    }

    /// The epoch of this authority's publication domain. Cloned
    /// authorities share it; a full replacement authority does not.
    pub fn config_epoch(&self) -> zeroclaw_config::live::ConfigEpoch {
        self.live.epoch()
    }

    /// Admit one serialized config write: acquire the process-wide writer
    /// mutex, then admit a generation write lease into the lifecycle
    /// generation. The returned [`ConfigCommit`] owns both for its whole
    /// lifetime, so a commit dispatched to a retained task keeps
    /// serialization and stays drain-visible even when its requester
    /// disappears.
    ///
    /// Admission fails closed once the generation is closing. The writer
    /// mutex is acquired *before* the lease: a waiter parked on the mutex
    /// holds no lifecycle state, so a closing generation never drains
    /// against a waiter that will simply be refused.
    pub async fn begin_config_commit(&self) -> Result<ConfigCommit, ConfigCommitError> {
        let guard = Arc::clone(&self.config_write_lock).lock_owned().await;
        let lease = self
            .agent_lifecycle
            .reserve_config_write()
            .map_err(|_| ConfigCommitError::GenerationClosing)?;
        Ok(ConfigCommit {
            guard,
            lease,
            live: self.live.clone(),
            application_results: Arc::clone(&self.application_results),
            application_attempts: Vec::new(),
        })
    }

    /// Hold this authority's config writer mutex without admitting a commit.
    /// Readers in the route-generation transaction (session construction,
    /// `session/configure`, rehydration and its reconciliation) take it so
    /// that no commit publishes while they build from config. It admits no
    /// write lease, so a closing generation never drains against a reader.
    pub async fn lock_config_writer(&self) -> tokio::sync::OwnedMutexGuard<()> {
        Arc::clone(&self.config_write_lock).lock_owned().await
    }

    /// [`Self::lock_config_writer`] without waiting: `None` while a commit or
    /// another reader holds the mutex.
    pub fn try_lock_config_writer(&self) -> Option<tokio::sync::OwnedMutexGuard<()>> {
        Arc::clone(&self.config_write_lock).try_lock_owned().ok()
    }

    /// Whether the daemon-wide config writer mutex is currently held.
    /// Test/diagnostic witness only: it proves a commit is in flight, not
    /// which one.
    #[cfg(any(test, feature = "test-util"))]
    pub fn config_write_lock_is_held(&self) -> bool {
        self.config_write_lock.try_lock().is_err()
    }

    /// Publish one config as the next revision WITHOUT the writer-mutex
    /// serialization. Fixture scaffolding only: production publication
    /// goes through `begin_config_commit` so every participating writer
    /// serializes before cloning current config through persistence and
    /// publication. Tests that exercise writer serialization hold real
    /// commits instead of using this.
    #[cfg(any(test, feature = "test-util"))]
    pub fn publish_for_test(&self, config: Config) -> zeroclaw_config::live::ConfigRevision {
        let revision = self.live.next_revision().expect("test revision available");
        self.live
            .publish(revision, config)
            .expect("test publication accepted")
    }

    /// Return the alias-scoped lifecycle authority shared by this daemon run.
    pub fn agent_lifecycle(&self) -> AgentLifecycleCoordinator {
        self.agent_lifecycle.clone()
    }

    /// Bind target execution admission to this authority's live config and
    /// alias lifecycle coordinator. Callers keep the returned capability and
    /// pass it into the target factory or detached task; it is not a registry
    /// and does not create another config owner.
    pub fn execution_capability(&self) -> AgentExecutionCapability {
        AgentExecutionCapability {
            config: self.live_handle(),
            agent_lifecycle: self.agent_lifecycle(),
        }
    }

    /// Close lifecycle admission for this daemon generation. New agent work
    /// and new config commits are both refused afterwards; already-admitted
    /// work runs to completion and is drained by the drain methods below.
    pub fn close_agent_lifecycle(&self) {
        self.agent_lifecycle.close_generation();
    }

    /// Drain a closed generation and release its process ownership.
    pub async fn drain_agent_lifecycle(&self) {
        self.close_agent_lifecycle();
        const DIAGNOSTIC_INTERVAL: std::time::Duration = std::time::Duration::from_secs(30);
        loop {
            if tokio::time::timeout(
                DIAGNOSTIC_INTERVAL,
                self.agent_lifecycle.drain_closed_generation(),
            )
            .await
            .is_ok()
            {
                return;
            }
            let aliases = self.agent_lifecycle.pending_work_aliases();
            let active_config_writes = self.agent_lifecycle.active_config_write_count();
            ::zeroclaw_log::record!(
                WARN,
                ::zeroclaw_log::Event::new(module_path!(), ::zeroclaw_log::Action::Note)
                    .with_category(::zeroclaw_log::EventCategory::Agent)
                    .with_outcome(::zeroclaw_log::EventOutcome::Unknown)
                    .with_attrs(::serde_json::json!({
                        "pending_aliases": aliases,
                        "active_config_writes": active_config_writes,
                        "waited_seconds": DIAGNOSTIC_INTERVAL.as_secs(),
                    })),
                "daemon generation remains fail-closed while admitted agent work is still running"
            );
        }
    }

    /// Drain a closed generation while RETAINING process ownership, so a
    /// daemon reload can transfer the guard into the next generation without
    /// an unlocked read/reacquire interval. Pair with
    /// [`Self::take_process_ownership`] once the drain completes.
    pub async fn drain_agent_lifecycle_retaining_ownership(&self) {
        self.close_agent_lifecycle();
        const DIAGNOSTIC_INTERVAL: std::time::Duration = std::time::Duration::from_secs(30);
        loop {
            if tokio::time::timeout(
                DIAGNOSTIC_INTERVAL,
                self.agent_lifecycle
                    .drain_closed_generation_retaining_ownership(),
            )
            .await
            .is_ok()
            {
                return;
            }
            let aliases = self.agent_lifecycle.pending_work_aliases();
            let active_config_writes = self.agent_lifecycle.active_config_write_count();
            ::zeroclaw_log::record!(
                WARN,
                ::zeroclaw_log::Event::new(module_path!(), ::zeroclaw_log::Action::Note)
                    .with_category(::zeroclaw_log::EventCategory::Agent)
                    .with_outcome(::zeroclaw_log::EventOutcome::Unknown)
                    .with_attrs(::serde_json::json!({
                        "pending_aliases": aliases,
                        "active_config_writes": active_config_writes,
                        "waited_seconds": DIAGNOSTIC_INTERVAL.as_secs(),
                    })),
                "daemon generation remains fail-closed while admitted agent work is still running"
            );
        }
    }

    /// Transfer the process-ownership guard out of this authority. Returns
    /// `None` when ownership was already released by a completed drain or
    /// never acquired.
    pub fn take_process_ownership(&self) -> Option<ConfigOwnershipGuard> {
        self.agent_lifecycle.take_ownership()
    }
}

#[derive(Debug, thiserror::Error)]
pub enum ConfigOwnershipError {
    #[error("config lifecycle is already owned at {path}")]
    AlreadyOwned { path: PathBuf },
    #[error(transparent)]
    Unavailable(#[from] anyhow::Error),
}

#[derive(Debug, PartialEq, Eq, thiserror::Error)]
pub enum AgentExecutionError {
    #[error(transparent)]
    Admission(#[from] AgentAdmissionError),
    #[error("agent `{alias}` is not configured in the live authority")]
    UnknownAlias { alias: String },
}

/// Explicit capability for admitting work against the authority-owned live
/// config. The capability is cheap to clone; each call to `admit` owns a
/// distinct alias lease, while the config snapshot is taken only after that
/// lease is acquired.
#[derive(Clone)]
pub struct AgentExecutionCapability {
    config: LiveConfigHandle,
    agent_lifecycle: AgentLifecycleCoordinator,
}

/// An admitted target snapshot and its owned lifecycle lease. Clones share
/// this one logical lease so detached continuations can outlive their caller
/// without releasing admission early.
#[derive(Clone)]
pub struct AgentExecutionAdmission {
    config: Arc<Config>,
    lease: Arc<AgentTurnLease>,
    capability: AgentExecutionCapability,
    alias: String,
    generation: u64,
}

/// Immutable generation witness for one selection of work whose target is
/// learned from storage. Capture before reading the payload, not when the
/// selected work is eventually polled. This is not a live config registry.
#[derive(Clone)]
pub struct AgentExecutionSelection {
    capability: AgentExecutionCapability,
    generations: HashMap<String, Result<u64, AgentAdmissionError>>,
    closing: bool,
}

impl AgentExecutionSelection {
    pub fn config_handle(&self) -> LiveConfigHandle {
        self.capability.config_handle()
    }

    pub fn resolve_and_admit(
        &self,
        requested_alias: &str,
    ) -> Result<AgentExecutionAdmission, AgentExecutionError> {
        if self.closing {
            return Err(AgentAdmissionError::GenerationClosing.into());
        }
        let canonical = self
            .generations
            .keys()
            .find(|alias| alias.eq_ignore_ascii_case(requested_alias.trim()))
            .cloned()
            .unwrap_or_else(|| requested_alias.trim().to_string());
        let generation = self.generations.get(&canonical).cloned().ok_or_else(|| {
            AgentExecutionError::UnknownAlias {
                alias: canonical.clone(),
            }
        })??;
        self.capability.admit_at(&canonical, generation)
    }
}

impl AgentExecutionCapability {
    pub fn capture_selection(&self) -> AgentExecutionSelection {
        // Never nest the lifecycle mutex and config lock. A mutation between
        // these reads changes the generation and is rejected at admission.
        let (generations, closing) = {
            let state = self.agent_lifecycle.state.lock();
            let generations: HashMap<_, _> = state
                .aliases
                .iter()
                .map(|(alias, lifecycle)| {
                    let generation = if lifecycle.deleting {
                        Err(AgentAdmissionError::Deleting {
                            alias: alias.clone(),
                        })
                    } else {
                        Ok(lifecycle.generation)
                    };
                    (alias.clone(), generation)
                })
                .collect();
            (generations, state.closing)
        };
        let generations = self
            .config
            .read()
            .agents
            .keys()
            .map(|alias| {
                (
                    alias.clone(),
                    generations.get(alias).cloned().unwrap_or(Ok(0)),
                )
            })
            .collect();
        AgentExecutionSelection {
            capability: self.clone(),
            generations,
            closing,
        }
    }

    pub fn from_parts(
        config: LiveConfigHandle,
        agent_lifecycle: AgentLifecycleCoordinator,
    ) -> Self {
        Self {
            config,
            agent_lifecycle,
        }
    }

    pub fn config_handle(&self) -> LiveConfigHandle {
        self.config.clone()
    }

    pub fn agent_lifecycle_generation(&self, alias: &str) -> u64 {
        self.agent_lifecycle.alias_generation(alias)
    }

    /// Resolve a case-insensitive alias from the authoritative snapshot and
    /// admit the canonical alias before taking the usable target snapshot.
    pub fn resolve_and_admit(
        &self,
        requested_alias: &str,
    ) -> Result<AgentExecutionAdmission, AgentExecutionError> {
        let requested_alias = requested_alias.trim();
        let canonical = self
            .config
            .read()
            .agents
            .keys()
            .find(|alias| alias.eq_ignore_ascii_case(requested_alias))
            .cloned()
            .unwrap_or_else(|| requested_alias.to_string());
        self.admit(&canonical)
    }

    /// Admit the current generation for a target alias.
    pub fn admit(&self, alias: &str) -> Result<AgentExecutionAdmission, AgentExecutionError> {
        let generation = self.agent_lifecycle.alias_generation(alias);
        self.admit_at(alias, generation)
    }

    /// Admit a queued producer against the generation it carried when it was
    /// created. This deliberately does not mint a new generation for stale
    /// queued work.
    pub fn admit_at(
        &self,
        alias: &str,
        generation: u64,
    ) -> Result<AgentExecutionAdmission, AgentExecutionError> {
        let lease = self
            .agent_lifecycle
            .reserve_turn_at(alias.to_string(), generation)?;
        let snapshot = self.config.read().clone();
        if snapshot.agent(alias).is_none() {
            drop(lease);
            return Err(AgentExecutionError::UnknownAlias {
                alias: alias.to_string(),
            });
        }
        Ok(AgentExecutionAdmission {
            config: Arc::new(snapshot),
            lease: Arc::new(lease),
            capability: self.clone(),
            alias: alias.to_string(),
            generation,
        })
    }
}

impl AgentExecutionAdmission {
    pub fn config(&self) -> Arc<Config> {
        Arc::clone(&self.config)
    }

    pub fn capability(&self) -> AgentExecutionCapability {
        self.capability.clone()
    }

    pub fn alias(&self) -> &str {
        &self.alias
    }

    pub fn generation(&self) -> u64 {
        self.generation
    }

    pub fn lease(&self) -> Arc<AgentTurnLease> {
        Arc::clone(&self.lease)
    }

    /// Re-check the generation immediately before a queued admission begins
    /// target construction. Holding the turn lease keeps deletion out, but a
    /// daemon shutdown may close the generation while a detached worker is
    /// still queued; that worker must fail before provider, tool, or target
    /// persistence work starts.
    pub fn revalidate(&self) -> Result<(), AgentExecutionError> {
        self.capability
            .agent_lifecycle
            .validate_turn_at(&self.alias, self.generation)
            .map_err(AgentExecutionError::Admission)?;
        if self.capability.config.read().agent(&self.alias).is_none() {
            return Err(AgentExecutionError::UnknownAlias {
                alias: self.alias.clone(),
            });
        }
        Ok(())
    }
}

/// Cross-process witness for config and alias lifecycle ownership.
#[derive(Debug)]
pub struct ConfigOwnershipGuard {
    _file: File,
    /// The data directory this guard locked.
    data_dir: PathBuf,
}

/// The data-directory entry that holds the config lifecycle lock.
///
/// The data directory belongs to the operator: it is configurable and may be
/// shared with a group, so this guard accepts it as it is. (When the daemon's
/// local IPC endpoint lives there, the endpoint's `prepare_parent` then sets
/// it to 0700, best-effort.) The owner-only checks apply to this
/// subdirectory, which ZeroClaw creates for itself, and to the lock inside
/// it. They protect the lock against other users who cannot write the data
/// directory or its parents. A user who can write the data directory can
/// still rename `.lifecycle` aside or create it first, splitting or blocking
/// the lock; no check on this directory alone prevents that.
const LIFECYCLE_DIR: &str = ".lifecycle";
const LIFECYCLE_LOCK_FILE: &str = "config-lifecycle.lock";

#[cfg(unix)]
fn validate_lock_dir(path: &Path) -> Result<()> {
    let metadata = std::fs::symlink_metadata(path)
        .with_context(|| format!("inspecting config lifecycle directory {}", path.display()))?;
    let euid = unsafe { libc::geteuid() };
    check_lock_dir(path, &metadata, euid)
}

/// ZeroClaw creates the lifecycle directory as a real directory that only the
/// current user can write, so a failure here means it was replaced or changed
/// afterwards. Each refusal names the command that puts it right once the
/// operator has checked its contents. Like the checks on the lock file, this
/// protects the lock only against other users who cannot write the data
/// directory or its parents (see [`LIFECYCLE_DIR`]).
///
/// This is deliberately stricter than the IPC endpoint's
/// `require_trusted_lock_dir`, which must accept shared socket directories
/// such as `/tmp`: it trusts root-owned and sticky world-writable directories
/// and follows symlinks. None of those states is one ZeroClaw creates here.
/// A sticky shared `.lifecycle` would let any user create the lock first and
/// block startup, and a symlinked one would move the lock whenever the link
/// is retargeted, so two processes could lock different files.
#[cfg(unix)]
fn check_lock_dir(path: &Path, metadata: &std::fs::Metadata, euid: u32) -> Result<()> {
    let display = path.display().to_string();
    anyhow::ensure!(
        metadata.file_type().is_dir(),
        crate::i18n::get_required_cli_string_with_args(
            "cli-config-lifecycle-dir-not-directory",
            &[("path", &display), ("command", &remedy_command("rm", path))],
        )
    );
    let owner_uid = metadata.uid();
    if owner_uid != euid {
        anyhow::bail!(owner_refusal(path, owner_uid, euid, OwnedDir::Lifecycle));
    }
    let mode = metadata.mode() & 0o7777;
    anyhow::ensure!(
        mode & 0o022 == 0,
        crate::i18n::get_required_cli_string_with_args(
            "cli-config-lifecycle-dir-writable",
            &[
                ("path", &display),
                ("mode", &format!("{mode:o}")),
                ("command", &remedy_command("chmod 700", path)),
            ],
        )
    );
    Ok(())
}

/// The directory an ownership refusal is about.
#[cfg(unix)]
#[derive(Clone, Copy)]
enum OwnedDir {
    /// `.lifecycle`, which ZeroClaw creates and holds only the lock.
    Lifecycle,
    /// The operator's data directory, when `.lifecycle` cannot be created in
    /// it. It may belong to another account or a service on purpose.
    Data,
}

/// The refusal for `dir` at `path` when it belongs to `owner_uid` rather than
/// the effective user.
#[cfg(unix)]
fn owner_refusal(path: &Path, owner_uid: u32, euid: u32, dir: OwnedDir) -> String {
    let (running_as_root, root_owner, foreign_owner) = match dir {
        OwnedDir::Lifecycle => (
            "cli-config-lifecycle-dir-running-as-root",
            "cli-config-lifecycle-dir-root-owner",
            "cli-config-lifecycle-dir-foreign-owner",
        ),
        OwnedDir::Data => (
            "cli-config-data-dir-running-as-root",
            "cli-config-data-dir-root-owner",
            "cli-config-data-dir-foreign-owner",
        ),
    };
    let display = path.display().to_string();
    let owner = user_name(owner_uid).unwrap_or_else(|| format!("uid {owner_uid}"));
    if euid == 0 {
        // Handing the directory to root would only move the refusal to the
        // owner's next run without sudo.
        return crate::i18n::get_required_cli_string_with_args(
            running_as_root,
            &[("path", &display), ("owner", &owner)],
        );
    }
    let current_name = user_name(euid);
    let user = current_name
        .clone()
        .unwrap_or_else(|| format!("uid {euid}"));
    let chown_to = current_name.unwrap_or_else(|| euid.to_string());
    let command = remedy_command(
        &format!(
            "sudo chown -R {}",
            crate::util::shell_escape(std::ffi::OsStr::new(&chown_to))
        ),
        path,
    );
    let key = if owner_uid == 0 {
        root_owner
    } else {
        foreign_owner
    };
    crate::i18n::get_required_cli_string_with_args(
        key,
        &[
            ("path", &display),
            ("owner", &owner),
            ("user", &user),
            ("command", &command),
        ],
    )
}

/// Explains a refused `.lifecycle` creation when the data directory belongs
/// to someone else, typically after a first run with sudo. Any other failure
/// keeps the plain OS error.
///
/// This only explains a refusal the OS already made, so it follows a
/// symlinked data directory, as creating `.lifecycle` did, and names the
/// directory the link resolves to, which is the one a `chown` must change.
#[cfg(unix)]
fn lock_dir_creation_error(
    data_dir: &Path,
    lock_dir: &Path,
    error: std::io::Error,
) -> anyhow::Error {
    let euid = unsafe { libc::geteuid() };
    let refusal = if error.kind() == std::io::ErrorKind::PermissionDenied {
        std::fs::metadata(data_dir)
            .ok()
            .filter(|metadata| metadata.uid() != euid)
            .map(|metadata| {
                let resolved =
                    std::fs::canonicalize(data_dir).unwrap_or_else(|_| data_dir.to_path_buf());
                owner_refusal(&resolved, metadata.uid(), euid, OwnedDir::Data)
            })
    } else {
        None
    };
    let context = refusal
        .unwrap_or_else(|| format!("creating config lifecycle directory {}", lock_dir.display()));
    anyhow::Error::new(error).context(context)
}

#[cfg(not(unix))]
fn lock_dir_creation_error(
    _data_dir: &Path,
    lock_dir: &Path,
    error: std::io::Error,
) -> anyhow::Error {
    anyhow::Error::new(error).context(format!(
        "creating config lifecycle directory {}",
        lock_dir.display()
    ))
}

#[cfg(unix)]
fn remedy_command(command: &str, path: &Path) -> String {
    format!("{command} {}", crate::util::shell_escape(path.as_os_str()))
}

/// The account name for `uid`, when the user database has one.
#[cfg(unix)]
fn user_name(uid: u32) -> Option<String> {
    use std::ffi::CStr;

    let mut capacity = 1024usize;
    loop {
        let mut buffer = vec![0u8; capacity];
        // SAFETY: `passwd` is plain data that getpwuid_r fills in.
        let mut passwd = unsafe { std::mem::zeroed::<libc::passwd>() };
        let mut result = std::ptr::null_mut();
        // SAFETY: every pointer refers to a live local, and `buffer.len()` is
        // the size of the buffer getpwuid_r may write strings into.
        let status = unsafe {
            libc::getpwuid_r(
                uid,
                &mut passwd,
                buffer.as_mut_ptr().cast::<libc::c_char>(),
                buffer.len(),
                &mut result,
            )
        };
        if status == 0 && !result.is_null() && !passwd.pw_name.is_null() {
            // SAFETY: on success pw_name points at a NUL-terminated string
            // inside `buffer`, which is still alive here.
            let name = unsafe { CStr::from_ptr(passwd.pw_name) }.to_string_lossy();
            return (!name.is_empty()).then(|| name.into_owned());
        }
        if status != libc::ERANGE || capacity >= 1024 * 1024 {
            return None;
        }
        capacity *= 2;
    }
}

#[cfg(not(unix))]
fn validate_lock_dir(_path: &Path) -> Result<()> {
    Ok(())
}

#[cfg(unix)]
fn validate_lock_file(file: &File, path: &Path) -> Result<()> {
    let metadata = file
        .metadata()
        .with_context(|| format!("inspecting config lifecycle lock {}", path.display()))?;
    anyhow::ensure!(
        metadata.file_type().is_file(),
        "config lifecycle lock {} is not a regular file",
        path.display()
    );
    let euid = unsafe { libc::geteuid() };
    anyhow::ensure!(
        metadata.uid() == euid,
        "config lifecycle lock {} is owned by uid {}, not the current user",
        path.display(),
        metadata.uid()
    );
    anyhow::ensure!(
        metadata.mode() & 0o077 == 0,
        "config lifecycle lock {} is accessible to other users (mode {:o})",
        path.display(),
        metadata.mode() & 0o7777
    );
    anyhow::ensure!(
        metadata.nlink() > 0,
        "config lifecycle lock {} was unlinked while being opened",
        path.display()
    );
    Ok(())
}

#[cfg(not(unix))]
fn validate_lock_file(_file: &File, _path: &Path) -> Result<()> {
    Ok(())
}

impl ConfigOwnershipGuard {
    pub fn acquire(data_dir: &Path) -> std::result::Result<Self, ConfigOwnershipError> {
        zeroclaw_config::schema::create_owner_only_dir(data_dir)
            .with_context(|| format!("creating data directory {}", data_dir.display()))?;
        let lock_dir = data_dir.join(LIFECYCLE_DIR);
        match zeroclaw_config::schema::create_owner_only_dir(&lock_dir) {
            Ok(()) => {}
            // Something other than a directory is in the way; the validation
            // below names it and the remedy.
            Err(error) if error.kind() == std::io::ErrorKind::AlreadyExists => {}
            Err(error) => return Err(lock_dir_creation_error(data_dir, &lock_dir, error).into()),
        }
        validate_lock_dir(&lock_dir)?;
        let path = lock_dir.join(LIFECYCLE_LOCK_FILE);
        let mut options = OpenOptions::new();
        options.read(true).write(true).create(true);
        #[cfg(unix)]
        options.mode(0o600).custom_flags(libc::O_NOFOLLOW);
        let file = options
            .open(&path)
            .with_context(|| format!("opening config lifecycle lock {}", path.display()))?;
        validate_lock_file(&file, &path)?;
        match file.try_lock() {
            Ok(()) => {
                #[cfg(unix)]
                {
                    let locked = file.metadata().with_context(|| {
                        format!("inspecting locked config lifecycle file {}", path.display())
                    })?;
                    let current = std::fs::symlink_metadata(&path).with_context(|| {
                        format!("confirming config lifecycle lock {}", path.display())
                    })?;
                    if locked.dev() != current.dev() || locked.ino() != current.ino() {
                        ::zeroclaw_log::record!(
                            ERROR,
                            ::zeroclaw_log::Event::new(
                                module_path!(),
                                ::zeroclaw_log::Action::Note
                            )
                            .with_outcome(::zeroclaw_log::EventOutcome::Failure)
                            .with_attrs(
                                ::serde_json::json!({ "path": path.display().to_string() })
                            ),
                            "config lifecycle lock was replaced while being acquired"
                        );
                        return Err(ConfigOwnershipError::Unavailable(anyhow::Error::msg(
                            format!(
                                "config lifecycle lock {} was replaced while being acquired",
                                path.display()
                            ),
                        )));
                    }
                }
                Ok(Self {
                    _file: file,
                    data_dir: data_dir.to_path_buf(),
                })
            }
            Err(TryLockError::WouldBlock) => Err(ConfigOwnershipError::AlreadyOwned { path }),
            Err(TryLockError::Error(error)) => Err(ConfigOwnershipError::Unavailable(
                anyhow::Error::new(error)
                    .context(format!("locking config lifecycle at {}", path.display())),
            )),
        }
    }
}

impl ConfigOwnershipGuard {
    /// Settle the session turns a previous process left marked "running" in
    /// the data directory this guard locked. Holding the guard is what makes
    /// that safe; see [`zeroclaw_infra::recover_abandoned_session_turns`],
    /// which does the work and acts once per process. Call it before the
    /// process admits a turn of its own.
    pub fn recover_abandoned_turns(&self) -> std::io::Result<usize> {
        zeroclaw_infra::recover_abandoned_session_turns(&self.data_dir)
    }

    /// [`Self::recover_abandoned_turns`] for a startup path: a store that
    /// cannot be settled is no reason to refuse to start, so the failure is
    /// logged and startup goes on.
    pub fn recover_abandoned_turns_or_log(&self) {
        if let Err(error) = self.recover_abandoned_turns() {
            ::zeroclaw_log::record!(
                WARN,
                ::zeroclaw_log::Event::new(module_path!(), ::zeroclaw_log::Action::Note)
                    .with_outcome(::zeroclaw_log::EventOutcome::Failure)
                    .with_attrs(::serde_json::json!({ "error": error.to_string() })),
                "could not settle session turns left running by a previous process"
            );
        }
    }
}

/// Detach retained lifecycle work without exposing it to request
/// cancellation.
///
/// The owned future captures its destructive leases itself and releases them
/// only when it completes; dropping the returned handle does not cancel the
/// task, so request cancellation cannot release the leases while the
/// transaction or cleanup is still running. The future may commit its
/// reservations mid-flight (see
/// [`AgentDeleteLease::commit_destructive_mutation`]).
pub fn spawn_agent_lifecycle_job<F, T>(future: F) -> tokio::task::JoinHandle<T>
where
    F: Future<Output = T> + Send + 'static,
    T: Send + 'static,
{
    zeroclaw_spawn::spawn!(future)
}

#[derive(Default)]
struct AliasLifecycleState {
    generation: u64,
    reservations: usize,
    live_sessions: usize,
    active_turns: usize,
    deleting: bool,
}

impl AliasLifecycleState {
    fn is_idle(&self) -> bool {
        self.reservations == 0
            && self.live_sessions == 0
            && self.active_turns == 0
            && !self.deleting
    }
}

#[derive(Default)]
struct AgentLifecycleState {
    aliases: HashMap<String, AliasLifecycleState>,
    active_config_writes: usize,
    closing: bool,
    // Retained across ordinary drops, but released once a closed generation drains.
    ownership: Option<ConfigOwnershipGuard>,
}

/// Coordinates slow session admission with destructive alias mutations.
#[derive(Clone, Default)]
pub struct AgentLifecycleCoordinator {
    state: Arc<parking_lot::Mutex<AgentLifecycleState>>,
    idle: Arc<tokio::sync::Notify>,
}

#[derive(Debug, Clone, PartialEq, Eq)]
pub enum AgentAdmissionError {
    Deleting { alias: String },
    StaleGeneration { alias: String },
    GenerationClosing,
}

#[derive(Debug, Clone, PartialEq, Eq)]
pub enum AgentDeleteBlocker {
    Deleting { alias: String },
    Reservations { alias: String, count: usize },
    LiveSessions { alias: String, count: usize },
    ActiveTurns { alias: String, count: usize },
    GenerationClosing,
}

impl std::fmt::Display for AgentAdmissionError {
    fn fmt(&self, formatter: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        match self {
            Self::Deleting { alias } => write!(formatter, "agent `{alias}` is being deleted"),
            Self::StaleGeneration { alias } => {
                write!(formatter, "agent `{alias}` changed during admission")
            }
            Self::GenerationClosing => write!(formatter, "agent lifecycle generation is closing"),
        }
    }
}

impl std::error::Error for AgentAdmissionError {}

impl std::fmt::Display for AgentDeleteBlocker {
    fn fmt(&self, formatter: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        match self {
            Self::Deleting { alias } => write!(formatter, "agent `{alias}` is already changing"),
            Self::Reservations { alias, count } => write!(
                formatter,
                "agent `{alias}` has {count} in-flight session admission(s)"
            ),
            Self::LiveSessions { alias, count } => {
                write!(formatter, "agent `{alias}` has {count} live session(s)")
            }
            Self::ActiveTurns { alias, count } => {
                write!(formatter, "agent `{alias}` has {count} active turn(s)")
            }
            Self::GenerationClosing => write!(formatter, "agent lifecycle generation is closing"),
        }
    }
}

pub struct AgentAdmissionReservation {
    coordinator: AgentLifecycleCoordinator,
    alias: String,
    generation: u64,
    active: bool,
}

pub struct AgentSessionLease {
    coordinator: AgentLifecycleCoordinator,
    alias: String,
    active: bool,
}

pub struct AgentTurnLease {
    coordinator: AgentLifecycleCoordinator,
    alias: String,
    active: bool,
}

pub struct AgentDeleteLease {
    coordinator: AgentLifecycleCoordinator,
    alias: String,
    active: bool,
    committed: bool,
}

pub struct ConfigWriteLease {
    coordinator: AgentLifecycleCoordinator,
    active: bool,
}

impl AgentDeleteLease {
    /// Transition a reserved destructive mutation into committed destructive
    /// ownership: advance the alias generation exactly once under the
    /// coordinator lock. Call only after the config mutation is durably
    /// persisted, and retain the lease in a continuation that cannot be
    /// cancelled with the request until cleanup finishes. Dropping the lease
    /// without committing rolls the reservation back and leaves the alias
    /// generation (and every producer pinned to it) unchanged.
    pub fn commit_destructive_mutation(&mut self) {
        if !self.active || self.committed {
            return;
        }
        let mut state = self.coordinator.state.lock();
        // The alias entry created at reservation is never removed while the
        // lease is live, so the lookup cannot fail (same invariant as
        // `AgentAdmissionReservation::publish`).
        let lifecycle = state
            .aliases
            .get_mut(&self.alias)
            .expect("delete reservation must retain alias state");
        debug_assert!(lifecycle.deleting);
        lifecycle.generation = lifecycle.generation.wrapping_add(1);
        self.committed = true;
    }
}

impl AgentLifecycleCoordinator {
    fn with_ownership(ownership: ConfigOwnershipGuard) -> Self {
        Self {
            state: Arc::new(parking_lot::Mutex::new(AgentLifecycleState {
                ownership: Some(ownership),
                ..AgentLifecycleState::default()
            })),
            idle: Arc::new(tokio::sync::Notify::new()),
        }
    }

    fn delete_blocker_locked(
        state: &AgentLifecycleState,
        alias: &str,
    ) -> Option<AgentDeleteBlocker> {
        if state.closing {
            return Some(AgentDeleteBlocker::GenerationClosing);
        }
        let lifecycle = state.aliases.get(alias)?;
        if lifecycle.deleting {
            return Some(AgentDeleteBlocker::Deleting {
                alias: alias.to_string(),
            });
        }
        if lifecycle.reservations > 0 {
            return Some(AgentDeleteBlocker::Reservations {
                alias: alias.to_string(),
                count: lifecycle.reservations,
            });
        }
        if lifecycle.live_sessions > 0 {
            return Some(AgentDeleteBlocker::LiveSessions {
                alias: alias.to_string(),
                count: lifecycle.live_sessions,
            });
        }
        if lifecycle.active_turns > 0 {
            return Some(AgentDeleteBlocker::ActiveTurns {
                alias: alias.to_string(),
                count: lifecycle.active_turns,
            });
        }
        None
    }

    /// Reserve an alias while a config property mutation is prepared and
    /// committed. This prevents delete/rename from crossing an autovivifying
    /// write without treating an ordinary config edit as destructive work.
    pub fn reserve_config_mutation(
        &self,
        alias: impl Into<String>,
    ) -> Result<AgentAdmissionReservation, AgentAdmissionError> {
        self.reserve_admission(alias)
    }

    /// Retain one config write in this daemon generation. Unlike the
    /// alias-scoped reservations above, this lease also covers config writes
    /// that do not target an agent. Generation drain waits for every retained
    /// writer before transferring process ownership to its successor.
    pub fn reserve_config_write(&self) -> Result<ConfigWriteLease, AgentAdmissionError> {
        let mut state = self.state.lock();
        if state.closing {
            return Err(AgentAdmissionError::GenerationClosing);
        }
        state.active_config_writes += 1;
        Ok(ConfigWriteLease {
            coordinator: self.clone(),
            active: true,
        })
    }

    /// Reserve an alias generation before slow agent construction starts.
    pub fn reserve_admission(
        &self,
        alias: impl Into<String>,
    ) -> Result<AgentAdmissionReservation, AgentAdmissionError> {
        let alias = alias.into();
        let mut state = self.state.lock();
        if state.closing {
            return Err(AgentAdmissionError::GenerationClosing);
        }
        let lifecycle = state.aliases.entry(alias.clone()).or_default();
        if lifecycle.deleting {
            return Err(AgentAdmissionError::Deleting { alias });
        }
        lifecycle.reservations += 1;
        Ok(AgentAdmissionReservation {
            coordinator: self.clone(),
            alias,
            generation: lifecycle.generation,
            active: true,
        })
    }

    /// Admit one ordinary message turn without pinning an idle connection.
    pub fn reserve_turn(
        &self,
        alias: impl Into<String>,
    ) -> Result<AgentTurnLease, AgentAdmissionError> {
        let alias = alias.into();
        let generation = self.alias_generation(&alias);
        self.reserve_turn_at(alias, generation)
    }

    /// Return the current generation token for a persistent turn producer.
    pub fn alias_generation(&self, alias: &str) -> u64 {
        self.state
            .lock()
            .aliases
            .get(alias)
            .map_or(0, |lifecycle| lifecycle.generation)
    }

    /// Admit a turn only when its persistent producer still targets the same
    /// alias generation it was constructed for.
    pub fn reserve_turn_at(
        &self,
        alias: impl Into<String>,
        generation: u64,
    ) -> Result<AgentTurnLease, AgentAdmissionError> {
        let alias = alias.into();
        let mut state = self.state.lock();
        if state.closing {
            return Err(AgentAdmissionError::GenerationClosing);
        }
        let lifecycle = state.aliases.entry(alias.clone()).or_default();
        if lifecycle.deleting {
            return Err(AgentAdmissionError::Deleting { alias });
        }
        if lifecycle.generation != generation {
            return Err(AgentAdmissionError::StaleGeneration { alias });
        }
        lifecycle.active_turns += 1;
        Ok(AgentTurnLease {
            coordinator: self.clone(),
            alias,
            active: true,
        })
    }

    fn validate_turn_at(&self, alias: &str, generation: u64) -> Result<(), AgentAdmissionError> {
        let state = self.state.lock();
        if state.closing {
            return Err(AgentAdmissionError::GenerationClosing);
        }
        state.aliases.get(alias).map_or(Ok(()), |lifecycle| {
            if lifecycle.deleting {
                Err(AgentAdmissionError::Deleting {
                    alias: alias.to_string(),
                })
            } else if lifecycle.generation != generation {
                Err(AgentAdmissionError::StaleGeneration {
                    alias: alias.to_string(),
                })
            } else {
                Ok(())
            }
        })
    }

    /// Reserve destructive work for one alias after proving no admission or
    /// published session is using it. The reservation blocks new admission by
    /// marking the alias changing, but does not advance the alias generation:
    /// a refused or failed mutation drops the lease and leaves existing
    /// producers usable. Only after the config mutation is durably committed
    /// does the caller call [`AgentDeleteLease::commit_destructive_mutation`],
    /// which advances the generation exactly once and hands destructive
    /// ownership to the retained continuation.
    pub fn begin_delete(
        &self,
        alias: impl Into<String>,
    ) -> Result<AgentDeleteLease, AgentDeleteBlocker> {
        let alias = alias.into();
        let mut state = self.state.lock();
        if let Some(blocker) = Self::delete_blocker_locked(&state, &alias) {
            return Err(blocker);
        }
        let lifecycle = state.aliases.entry(alias.clone()).or_default();
        lifecycle.deleting = true;
        Ok(AgentDeleteLease {
            coordinator: self.clone(),
            alias,
            active: true,
            committed: false,
        })
    }

    pub fn delete_blocker(&self, alias: &str) -> Option<AgentDeleteBlocker> {
        Self::delete_blocker_locked(&self.state.lock(), alias)
    }

    fn pending_work_aliases(&self) -> Vec<String> {
        let mut aliases: Vec<_> = self
            .state
            .lock()
            .aliases
            .iter()
            .filter(|(_, lifecycle)| !lifecycle.is_idle())
            .map(|(alias, _)| alias.clone())
            .collect();
        aliases.sort();
        aliases
    }

    pub(crate) fn active_config_write_count(&self) -> usize {
        self.state.lock().active_config_writes
    }

    pub fn live_session_count(&self, alias: &str) -> usize {
        self.state
            .lock()
            .aliases
            .get(alias)
            .map_or(0, |state| state.live_sessions)
    }

    pub fn active_turn_count(&self, alias: &str) -> usize {
        self.state
            .lock()
            .aliases
            .get(alias)
            .map_or(0, |state| state.active_turns)
    }

    /// Prevent this generation from admitting new sessions, turns, or
    /// destructive work before ingress shutdown begins.
    pub fn close_generation(&self) {
        self.state.lock().closing = true;
    }

    /// Closed capabilities cannot admit new work, so idle clones need not keep
    /// the process lock after the last admitted continuation has finished.
    async fn drain_closed_generation(&self) {
        loop {
            let notified = self.idle.notified();
            tokio::pin!(notified);
            notified.as_mut().enable();
            {
                let mut state = self.state.lock();
                if state.closing
                    && state.active_config_writes == 0
                    && state.aliases.values().all(AliasLifecycleState::is_idle)
                {
                    drop(state.ownership.take());
                    return;
                }
            }
            notified.await;
        }
    }

    /// Wait until the closed generation is idle WITHOUT releasing process
    /// ownership. Used by daemon reload, which transfers the guard into the
    /// next generation instead of releasing it between generations.
    async fn drain_closed_generation_retaining_ownership(&self) {
        loop {
            let notified = self.idle.notified();
            tokio::pin!(notified);
            notified.as_mut().enable();
            {
                let state = self.state.lock();
                if state.closing
                    && state.active_config_writes == 0
                    && state.aliases.values().all(AliasLifecycleState::is_idle)
                {
                    return;
                }
            }
            notified.await;
        }
    }

    /// Transfer the process-ownership guard out of this coordinator. Returns
    /// `None` when ownership was already released by a completed drain or
    /// never acquired.
    fn take_ownership(&self) -> Option<ConfigOwnershipGuard> {
        self.state.lock().ownership.take()
    }
}

impl AgentAdmissionReservation {
    /// Revalidate the reserved generation and publish one live session.
    pub fn publish(mut self) -> Result<AgentSessionLease, AgentAdmissionError> {
        let mut state = self.coordinator.state.lock();
        let closing = state.closing;
        let lifecycle = state
            .aliases
            .get_mut(&self.alias)
            .expect("admission reservation must retain alias state");
        lifecycle.reservations = lifecycle.reservations.saturating_sub(1);
        self.active = false;
        if closing {
            self.coordinator.idle.notify_waiters();
            return Err(AgentAdmissionError::GenerationClosing);
        }
        if lifecycle.deleting || lifecycle.generation != self.generation {
            self.coordinator.idle.notify_waiters();
            return Err(AgentAdmissionError::StaleGeneration {
                alias: self.alias.clone(),
            });
        }
        lifecycle.live_sessions += 1;
        Ok(AgentSessionLease {
            coordinator: self.coordinator.clone(),
            alias: self.alias.clone(),
            active: true,
        })
    }
}

impl Drop for AgentAdmissionReservation {
    fn drop(&mut self) {
        if !self.active {
            return;
        }
        if let Some(lifecycle) = self.coordinator.state.lock().aliases.get_mut(&self.alias) {
            lifecycle.reservations = lifecycle.reservations.saturating_sub(1);
        }
        self.coordinator.idle.notify_waiters();
    }
}

impl Drop for AgentSessionLease {
    fn drop(&mut self) {
        if !self.active {
            return;
        }
        if let Some(lifecycle) = self.coordinator.state.lock().aliases.get_mut(&self.alias) {
            lifecycle.live_sessions = lifecycle.live_sessions.saturating_sub(1);
        }
        self.coordinator.idle.notify_waiters();
    }
}

impl Drop for AgentTurnLease {
    fn drop(&mut self) {
        if !self.active {
            return;
        }
        if let Some(lifecycle) = self.coordinator.state.lock().aliases.get_mut(&self.alias) {
            lifecycle.active_turns = lifecycle.active_turns.saturating_sub(1);
        }
        self.coordinator.idle.notify_waiters();
    }
}

impl Drop for AgentDeleteLease {
    fn drop(&mut self) {
        if !self.active {
            return;
        }
        if let Some(lifecycle) = self.coordinator.state.lock().aliases.get_mut(&self.alias) {
            lifecycle.deleting = false;
        }
        self.coordinator.idle.notify_waiters();
    }
}

impl Drop for ConfigWriteLease {
    fn drop(&mut self) {
        if !self.active {
            return;
        }
        let mut state = self.coordinator.state.lock();
        state.active_config_writes = state.active_config_writes.saturating_sub(1);
        drop(state);
        self.coordinator.idle.notify_waiters();
    }
}

/// One admitted, serialized config write.
///
/// Owns the process-wide writer guard and the generation write lease
/// from admission until the commit completes (or the value drops before
/// dispatch, which abandons preparation and releases both). Created only
/// through [`LiveConfigAuthority::begin_config_commit`].
///
/// The intended writer shape is: `current_config()` → stage a mutated
/// clone → `next_revision()` (checked, *before* any irreversible I/O) →
/// run persistence → `publish()` the committed candidate under the
/// allocated revision. To make the irreversible phase uncancellable,
/// move this value into a detached task (see
/// [`spawn_agent_lifecycle_job`]) before the first persistence await;
/// the task then owns serialization and drain accounting until it
/// finishes, and a dropped requester cannot strand a committed disk
/// write without its publication.
pub struct ConfigCommit {
    guard: tokio::sync::OwnedMutexGuard<()>,
    lease: ConfigWriteLease,
    live: LiveConfig,
    application_results: Arc<parking_lot::Mutex<ConfigApplicationLedger>>,
    application_attempts: Vec<(ConfigApplicationTarget, Vec<Vec<String>>)>,
}

impl ConfigCommit {
    pub(crate) fn register_application(
        &mut self,
        target: ConfigApplicationTarget,
        paths: Vec<Vec<String>>,
    ) {
        self.application_attempts.push((target, paths));
    }

    /// Clone the currently published config. Read-for-modify under this
    /// commit's serialization: no other admitted writer can interleave.
    pub fn current_config(&self) -> Config {
        self.live.snapshot()
    }

    /// The revision currently published.
    pub fn published_revision(&self) -> zeroclaw_config::live::ConfigRevision {
        self.live.published_revision()
    }

    /// Allocate the next publication identity, checking
    /// representability. Call before any irreversible persistence: an
    /// exhausted epoch must refuse the commit while disk state is still
    /// unchanged.
    pub fn next_revision(
        &self,
    ) -> Result<zeroclaw_config::live::ConfigRevision, ConfigCommitError> {
        self.live.next_revision().map_err(ConfigCommitError::from)
    }

    /// Publish one committed candidate under its pre-allocated revision.
    /// Installs the config and revision as one pair; refuses a revision
    /// that is not the exact successor of the published one.
    pub fn publish(
        &self,
        revision: zeroclaw_config::live::ConfigRevision,
        config: Config,
    ) -> Result<(), ConfigCommitError> {
        let changed_paths = Config::semantic_changed_paths(&self.current_config(), &config).ok();
        let mut application_results = self.application_results.lock();
        self.live.publish(revision, config).map_err(|error| {
            ::zeroclaw_log::record!(
                ERROR,
                ::zeroclaw_log::Event::new(module_path!(), ::zeroclaw_log::Action::Reject)
                    .with_outcome(::zeroclaw_log::EventOutcome::Failure)
                    .with_attrs(::serde_json::json!({
                        "error": error.to_string(),
                    })),
                "config publication refused a non-successor revision; the published pair is unchanged"
            );
            ConfigCommitError::from(error)
        })?;
        application_results.published(
            revision,
            changed_paths.as_deref(),
            &self.application_attempts,
        );
        Ok(())
    }

    /// Read-only live handle over the storage this commit publishes to,
    /// for post-commit reads.
    pub fn live_handle(&self) -> LiveConfigHandle {
        self.live.handle()
    }

    /// Release serialization explicitly — the writer guard and the
    /// generation write lease drop here — while the caller continues with
    /// slow, non-config side effects. The retained destructive
    /// transactions call this after their required config work, exactly
    /// where they previously dropped the raw writer guard before
    /// workspace cleanup.
    pub fn release_serialization(self) {
        drop(self.guard);
        drop(self.lease);
    }
}

/// Why a config commit could not be admitted, allocated, or published.
#[derive(Debug, thiserror::Error)]
pub enum ConfigCommitError {
    /// The generation is closing (reload or shutdown); new config writes
    /// are refused, including through old cloned handles.
    #[error("config lifecycle generation is closing; refusing new config commits")]
    GenerationClosing,
    /// The epoch's sequence space is exhausted; refused before any
    /// irreversible persistence.
    #[error("config revision sequence is exhausted for this authority epoch")]
    RevisionExhausted,
    /// A publication attempted to install a revision that is not the
    /// successor of the published one. The published pair is unchanged.
    #[error("config publication refused a non-successor revision")]
    NotSuccessor,
}

impl From<zeroclaw_config::live::LiveConfigError> for ConfigCommitError {
    fn from(error: zeroclaw_config::live::LiveConfigError) -> Self {
        match error {
            zeroclaw_config::live::LiveConfigError::RevisionExhausted => Self::RevisionExhausted,
            zeroclaw_config::live::LiveConfigError::NotSuccessor { .. } => Self::NotSuccessor,
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn cloned_authority_preserves_storage_epoch_and_write_lock_identity() {
        let authority = LiveConfigAuthority::new(Config::default());
        let cloned = authority.clone();

        assert!(authority.live_handle().same_storage(&cloned.live_handle()));
        assert_eq!(authority.config_epoch(), cloned.config_epoch());
        assert_eq!(authority.published_revision(), cloned.published_revision());
        assert!(Arc::ptr_eq(
            &authority.config_write_lock,
            &cloned.config_write_lock
        ));
        assert!(Arc::ptr_eq(
            &authority.config_write_lock,
            &zeroclaw_config::write_lock::shared_config_write_lock()
        ));
        assert!(Arc::ptr_eq(
            &authority.agent_lifecycle().state,
            &cloned.agent_lifecycle().state
        ));
    }

    #[tokio::test]
    async fn shared_writer_waiter_is_not_admitted_and_refuses_closed_generation() {
        let authority = LiveConfigAuthority::new(Config::default());
        let held = zeroclaw_config::write_lock::shared_config_write_lock()
            .lock_owned()
            .await;
        let mut pending = std::pin::pin!(authority.begin_config_commit());
        assert!(
            tokio::time::timeout(std::time::Duration::from_millis(20), &mut pending)
                .await
                .is_err()
        );
        assert_eq!(authority.agent_lifecycle().active_config_write_count(), 0);
        authority.close_agent_lifecycle();
        tokio::time::timeout(
            std::time::Duration::from_secs(1),
            authority.drain_agent_lifecycle_retaining_ownership(),
        )
        .await
        .expect("a waiting writer must not delay generation drain");
        drop(held);
        assert!(matches!(
            tokio::time::timeout(std::time::Duration::from_secs(1), pending)
                .await
                .unwrap(),
            Err(ConfigCommitError::GenerationClosing)
        ));
    }

    #[tokio::test]
    async fn full_replacement_authority_gets_a_fresh_epoch_and_lifecycle() {
        let first = LiveConfigAuthority::new(Config::default());
        let first_commit = first.begin_config_commit().await.unwrap();
        let revision = first_commit.next_revision().unwrap();
        first_commit.publish(revision, Config::default()).unwrap();
        let first_published = first.published_revision();

        // A full replacement (daemon reload) constructs a fresh authority:
        // fresh epoch, fresh lifecycle, sequence restarts at zero, and the
        // retired generation's revision is never equal to the new one even
        // though the new initial sequence matches an earlier one.
        let second = LiveConfigAuthority::new(Config::default());
        assert_ne!(first.config_epoch(), second.config_epoch());
        assert!(!second.published_revision().same_epoch(&first_published));
        assert!(
            !second
                .published_revision()
                .succeeds_within_epoch(&first_published)
        );
        assert!(second.agent_lifecycle().reserve_turn("any").is_ok());
    }

    #[tokio::test]
    async fn config_commit_admission_fails_closed_after_generation_close() {
        let authority = LiveConfigAuthority::new(Config::default());
        let old_handle = authority.clone();
        authority.close_agent_lifecycle();

        // Old handles (clones from before the close) cannot admit new
        // writes, and neither can the original.
        assert!(matches!(
            authority.begin_config_commit().await,
            Err(ConfigCommitError::GenerationClosing)
        ));
        assert!(matches!(
            old_handle.begin_config_commit().await,
            Err(ConfigCommitError::GenerationClosing)
        ));
    }

    #[tokio::test]
    async fn dispatched_config_commit_publishes_despite_requester_cancellation() {
        let tmp = tempfile::TempDir::new().unwrap();
        let config = Config {
            config_path: tmp.path().join("config.toml"),
            data_dir: tmp.path().join("data"),
            ..Config::default()
        };
        config.save().await.unwrap();
        let config_path = config.config_path.clone();
        let authority = LiveConfigAuthority::new(config);

        let gate = zeroclaw_config::schema::test_post_replace_pause_gate::arm(config_path);
        let commit = authority.begin_config_commit().await.unwrap();
        let mut working = commit.current_config();
        working
            .set_prop_persistent("gateway.host", "0.0.0.0")
            .unwrap();
        working.mark_dirty("gateway.host");
        let revision = commit.next_revision().unwrap();
        let prior_revision = commit.published_revision();
        assert!(revision.succeeds_within_epoch(&prior_revision));

        // The irreversible save+publish phase runs retained; the requester
        // only awaits the join handle. Simulate the requester disappearing
        // exactly while the save is paused inside the post-rename window.
        let (completed_tx, completed_rx) = tokio::sync::oneshot::channel();
        let job = spawn_agent_lifecycle_job(Box::pin(async move {
            // `commit` owns the writer guard and the generation write lease for
            // the whole body; dropping this future is what releases them.
            let mut config = working;
            config.save_dirty().await?;
            commit.publish(revision, config)?;
            drop(commit);
            let _ = completed_tx.send(());
            Ok::<(), anyhow::Error>(())
        }));
        let requester = zeroclaw_spawn::spawn!(async move {
            let _ = job.await;
        });
        tokio::time::timeout(std::time::Duration::from_secs(5), gate.wait_paused())
            .await
            .expect("retained commit reaches the post-rename window");
        requester.abort();
        assert!(requester.await.unwrap_err().is_cancelled());
        gate.release();

        // Observe this retained task after its guard and lease are released;
        // another authority may immediately acquire the shared writer mutex.
        tokio::time::timeout(std::time::Duration::from_secs(5), completed_rx)
            .await
            .expect("cancelled requester must not abandon the dispatched commit")
            .expect("retained commit must signal completion");
        assert_eq!(authority.published_revision(), revision);
        assert_eq!(authority.live_handle().read().gateway.host, "0.0.0.0");
        assert_eq!(authority.agent_lifecycle().active_config_write_count(), 0);
    }

    #[tokio::test]
    async fn reload_drain_waits_for_admitted_non_agent_config_commit() {
        let authority = LiveConfigAuthority::new(Config::default());
        // An ordinary (non-agent, non-destructive) config commit is
        // admitted and held mid-flight while the generation closes.
        let commit = authority.begin_config_commit().await.unwrap();
        authority.close_agent_lifecycle();

        let mut drain = std::pin::pin!(authority.drain_agent_lifecycle_retaining_ownership());
        assert!(
            tokio::time::timeout(std::time::Duration::from_millis(20), &mut drain)
                .await
                .is_err(),
            "drain must wait for the admitted config commit"
        );

        // The admitted commit still publishes: closing suppresses new
        // admission, not an already-admitted publication.
        let revision = commit.next_revision().unwrap();
        commit.publish(revision, Config::default()).unwrap();
        drop(commit);
        tokio::time::timeout(std::time::Duration::from_secs(1), drain)
            .await
            .expect("drain completes once the admitted commit finishes");
    }

    #[test]
    fn test_authorities_get_a_private_write_lock_unless_told_to_share_one() {
        let process = zeroclaw_config::write_lock::shared_config_write_lock();
        let isolated = LiveConfigAuthority::for_tests(Config::default());
        let other = LiveConfigAuthority::for_tests(Config::default());
        assert!(!Arc::ptr_eq(&isolated.config_write_lock, &process));
        assert!(!Arc::ptr_eq(
            &isolated.config_write_lock,
            &other.config_write_lock
        ));
        assert!(Arc::ptr_eq(
            &isolated.config_write_lock,
            &isolated.clone().config_write_lock
        ));

        let sharing = LiveConfigAuthority::for_tests_sharing_writer(Config::default(), &isolated);
        assert!(Arc::ptr_eq(
            &sharing.config_write_lock,
            &isolated.config_write_lock
        ));
        assert!(!sharing.live_handle().same_storage(&isolated.live_handle()));
    }

    #[tokio::test]
    async fn config_writer_gate_and_commits_serialize_on_one_mutex() {
        let authority = LiveConfigAuthority::for_tests(Config::default());
        let gate = authority.lock_config_writer().await;
        assert!(authority.try_lock_config_writer().is_none());
        let mut commit = std::pin::pin!(authority.begin_config_commit());
        assert!(
            tokio::time::timeout(std::time::Duration::from_millis(20), &mut commit)
                .await
                .is_err(),
            "a commit must wait while a reader holds the writer gate"
        );

        drop(gate);
        let commit = tokio::time::timeout(std::time::Duration::from_secs(5), commit)
            .await
            .expect("the commit proceeds once the reader releases the gate")
            .expect("an open generation admits the commit");
        assert!(authority.try_lock_config_writer().is_none());
        drop(commit);
        assert!(authority.try_lock_config_writer().is_some());
    }

    #[test]
    fn delete_refuses_reserved_and_live_aliases() {
        let lifecycle = AgentLifecycleCoordinator::default();
        let reservation = lifecycle.reserve_admission("alpha").unwrap();
        assert_eq!(
            lifecycle.begin_delete("alpha").err(),
            Some(AgentDeleteBlocker::Reservations {
                alias: "alpha".to_string(),
                count: 1,
            })
        );

        let session = reservation.publish().unwrap();
        assert_eq!(lifecycle.live_session_count("alpha"), 1);
        assert_eq!(
            lifecycle.begin_delete("alpha").err(),
            Some(AgentDeleteBlocker::LiveSessions {
                alias: "alpha".to_string(),
                count: 1,
            })
        );

        drop(session);
        assert_eq!(lifecycle.live_session_count("alpha"), 0);
        assert!(lifecycle.begin_delete("alpha").is_ok());
    }

    #[test]
    fn delete_lease_blocks_recreation_until_cleanup_finishes() {
        let lifecycle = AgentLifecycleCoordinator::default();
        let delete = lifecycle.begin_delete("alpha").unwrap();
        assert_eq!(
            lifecycle.reserve_admission("alpha").err(),
            Some(AgentAdmissionError::Deleting {
                alias: "alpha".to_string(),
            })
        );
        assert!(lifecycle.reserve_admission("beta").is_ok());

        drop(delete);
        assert!(lifecycle.reserve_admission("alpha").is_ok());
    }

    /// Spawning a process copies every open descriptor into the child until
    /// it execs, another test's config lock included. A test that re-acquires
    /// the lock right after releasing it could then find it still held, so
    /// those tests hold this for reading, and a test that spawns holds it for
    /// writing while its child runs.
    static SPAWN_VS_LOCK_REACQUIRE: tokio::sync::RwLock<()> = tokio::sync::RwLock::const_new(());

    #[test]
    fn config_ownership_is_exclusive_and_released_on_drop() {
        let _no_spawn = SPAWN_VS_LOCK_REACQUIRE.blocking_read();
        let temp = tempfile::TempDir::new().unwrap();
        let first = ConfigOwnershipGuard::acquire(temp.path()).unwrap();
        assert!(matches!(
            ConfigOwnershipGuard::acquire(temp.path()),
            Err(ConfigOwnershipError::AlreadyOwned { .. })
        ));
        drop(first);
        ConfigOwnershipGuard::acquire(temp.path()).unwrap();
    }

    #[cfg(unix)]
    fn mode_of(path: &Path) -> u32 {
        use std::os::unix::fs::PermissionsExt;

        std::fs::symlink_metadata(path)
            .unwrap()
            .permissions()
            .mode()
            & 0o7777
    }

    #[cfg(unix)]
    #[test]
    fn fresh_install_under_umask_0002_is_owner_only_and_acquired() {
        const CHILD_ENV: &str = "ZEROCLAW_LIFECYCLE_UMASK_TEST_CHILD";
        const TEST_NAME: &str = "live_config_authority::tests::fresh_install_under_umask_0002_is_owner_only_and_acquired";

        // The umask is process-global. Run the behavioral part in a dedicated
        // test subprocess so unrelated tests in the parent harness cannot
        // observe it.
        if std::env::var_os(CHILD_ENV).is_none() {
            let _spawning = SPAWN_VS_LOCK_REACQUIRE.blocking_write();
            let output = std::process::Command::new(
                std::env::current_exe().expect("test executable must be available"),
            )
            .args(["--exact", TEST_NAME, "--nocapture"])
            .env(CHILD_ENV, "1")
            .output()
            .expect("spawn umask test subprocess");
            let stdout = String::from_utf8_lossy(&output.stdout);
            assert!(
                output.status.success() && stdout.contains("1 passed"),
                "umask test subprocess failed with status {}:\n{stdout}\n{}",
                output.status,
                String::from_utf8_lossy(&output.stderr)
            );
            return;
        }

        let temp = tempfile::TempDir::new().unwrap();
        let install = temp.path().join("install");
        let data_dir = install.join("data");
        // SAFETY: umask has no memory-safety preconditions, and this
        // subprocess runs only the current test.
        unsafe { libc::umask(0o002) };

        let guard = ConfigOwnershipGuard::acquire(&data_dir)
            .expect("ZeroClaw must accept the directories it creates itself");

        assert_eq!(mode_of(&data_dir), 0o700);
        assert_eq!(mode_of(&data_dir.join(LIFECYCLE_DIR)), 0o700);
        assert_eq!(
            mode_of(&data_dir.join(LIFECYCLE_DIR).join(LIFECYCLE_LOCK_FILE)),
            0o600
        );
        // Missing parents keep the default mode.
        assert_eq!(mode_of(&install), 0o775);
        drop(guard);
    }

    #[cfg(unix)]
    #[test]
    fn existing_shared_data_dir_is_accepted_unchanged() {
        use std::os::unix::fs::PermissionsExt;

        // The data directory is the operator's, as in v0.8.5 and on Windows:
        // only the `.lifecycle` subdirectory is held to owner-only.
        for mode in [0o775, 0o777] {
            let temp = tempfile::TempDir::new().unwrap();
            let data_dir = temp.path().join("data");
            std::fs::create_dir(&data_dir).unwrap();
            std::fs::set_permissions(&data_dir, std::fs::Permissions::from_mode(mode)).unwrap();

            let guard = ConfigOwnershipGuard::acquire(&data_dir)
                .unwrap_or_else(|error| panic!("mode {mode:o} data dir refused: {error}"));

            assert_eq!(mode_of(&data_dir), mode, "the data dir must not be changed");
            assert_eq!(mode_of(&data_dir.join(LIFECYCLE_DIR)) & 0o022, 0);
            drop(guard);
        }
    }

    #[cfg(unix)]
    #[test]
    fn writable_lifecycle_dir_is_refused_unchanged_with_chmod_remedy() {
        use std::os::unix::fs::PermissionsExt;

        for mode in [0o775, 0o757] {
            let temp = tempfile::TempDir::new().unwrap();
            // The space proves the remedy quotes the path.
            let data_dir = temp.path().join("data dir");
            let lock_dir = data_dir.join(LIFECYCLE_DIR);
            std::fs::create_dir_all(&lock_dir).unwrap();
            std::fs::set_permissions(&lock_dir, std::fs::Permissions::from_mode(mode)).unwrap();

            let error = ConfigOwnershipGuard::acquire(&data_dir)
                .expect_err("a lifecycle dir other users can write must be refused")
                .to_string();

            let quoted = crate::util::shell_escape(lock_dir.as_os_str());
            assert!(
                quoted.starts_with('\''),
                "the path with a space must be quoted"
            );
            let command = format!("chmod 700 {quoted}");
            assert!(error.contains(&command), "missing `{command}`: {error}");
            assert_copyable(&error);
            assert!(
                error.contains(&format!("{mode:o}")),
                "missing mode: {error}"
            );
            assert!(
                error.contains("writable by its group or by other users"),
                "{error}"
            );
            assert!(
                error.contains("If chmod has no effect")
                    && error.contains("does not keep Unix permissions"),
                "missing the filesystem note: {error}"
            );
            assert_eq!(mode_of(&lock_dir), mode, "the refusal must not repair it");
            assert!(!lock_dir.join(LIFECYCLE_LOCK_FILE).exists());
        }
    }

    #[cfg(unix)]
    #[test]
    fn symlinked_lifecycle_dir_is_refused_with_remove_remedy() {
        let temp = tempfile::TempDir::new().unwrap();
        let data_dir = temp.path().join("data");
        std::fs::create_dir(&data_dir).unwrap();
        // An owner-only directory of the current user would pass every other
        // check if the link were followed. TempDir follows the umask, so pin
        // its mode.
        let elsewhere = tempfile::TempDir::new().unwrap();
        std::fs::set_permissions(
            elsewhere.path(),
            std::os::unix::fs::PermissionsExt::from_mode(0o700),
        )
        .unwrap();
        let lock_dir = data_dir.join(LIFECYCLE_DIR);
        std::os::unix::fs::symlink(elsewhere.path(), &lock_dir).unwrap();

        let error = ConfigOwnershipGuard::acquire(&data_dir)
            .expect_err("a symlinked lifecycle dir must be refused")
            .to_string();

        let command = format!("rm {}", crate::util::shell_escape(lock_dir.as_os_str()));
        assert!(error.contains(&command), "missing `{command}`: {error}");
        assert_copyable(&error);
        assert!(!elsewhere.path().join(LIFECYCLE_LOCK_FILE).exists());

        // A dangling link gets the same answer, not a bare "File exists".
        std::fs::remove_file(&lock_dir).unwrap();
        std::os::unix::fs::symlink(temp.path().join("missing"), &lock_dir).unwrap();
        let error = ConfigOwnershipGuard::acquire(&data_dir)
            .expect_err("a dangling lifecycle link must be refused")
            .to_string();
        assert!(error.contains(&command), "missing `{command}`: {error}");
    }

    /// Remedy commands are pasted into a shell, so the rendered text must not
    /// carry Fluent's invisible bidi isolation marks around placeables.
    #[cfg(unix)]
    fn assert_copyable(error: &str) {
        assert!(
            !error.contains(['\u{2068}', '\u{2069}']),
            "bidi isolation marks in: {error:?}"
        );
    }

    /// Metadata of a real directory that belongs to a regular user. Under
    /// root, the directory is handed to uid 65534 first.
    #[cfg(unix)]
    fn regular_user_dir(temp: &tempfile::TempDir) -> std::fs::Metadata {
        let dir = temp.path().join("owned");
        std::fs::create_dir(&dir).unwrap();
        // SAFETY: geteuid has no preconditions.
        if unsafe { libc::geteuid() } == 0 {
            std::os::unix::fs::chown(&dir, Some(65534), None).unwrap();
        }
        std::fs::symlink_metadata(&dir).unwrap()
    }

    #[cfg(unix)]
    #[test]
    fn foreign_owned_lifecycle_dir_puts_the_chown_remedy_first() {
        use std::os::unix::fs::MetadataExt;

        // Changing a real directory's owner needs root, so feed the check the
        // metadata of a directory and a different effective uid.
        let temp = tempfile::TempDir::new().unwrap();
        let lock_dir = temp.path().join("data dir").join(LIFECYCLE_DIR);
        let owned = regular_user_dir(&temp);
        let owner_uid = owned.uid();
        let euid = owner_uid + 1;
        let quoted = crate::util::shell_escape(lock_dir.as_os_str());

        let error = check_lock_dir(&lock_dir, &owned, euid)
            .expect_err("a lifecycle dir owned by another user must be refused")
            .to_string();

        let owner = user_name(owner_uid).unwrap_or_else(|| format!("uid {owner_uid}"));
        let chown_to = user_name(euid).unwrap_or_else(|| euid.to_string());
        let chown = format!(
            "sudo chown -R {} {quoted}",
            crate::util::shell_escape(std::ffi::OsStr::new(&chown_to))
        );
        let run_as = format!("Otherwise run ZeroClaw as {owner}.");
        let chown_at = error
            .find(&chown)
            .unwrap_or_else(|| panic!("missing `{chown}`: {error}"));
        let run_as_at = error
            .find(&run_as)
            .unwrap_or_else(|| panic!("missing `{run_as}`: {error}"));
        assert!(
            chown_at < run_as_at,
            "the chown remedy must come first: {error}"
        );
        assert!(
            !error.contains("not with sudo"),
            "no sudo was used: {error}"
        );
        assert!(
            error.contains("only config-lifecycle.lock, with a link count of 1"),
            "missing the contents check: {error}"
        );
        assert!(
            error.contains("If chown has no effect")
                && error.contains("does not keep Unix permissions"),
            "missing the filesystem note: {error}"
        );
        assert_copyable(&error);
    }

    #[cfg(unix)]
    #[test]
    fn user_owned_lifecycle_dir_under_sudo_says_run_without_sudo() {
        use std::os::unix::fs::MetadataExt;

        // ZeroClaw run with sudo finds the user's own .lifecycle. Handing it
        // to root would only break the user's next run without sudo.
        let temp = tempfile::TempDir::new().unwrap();
        let lock_dir = temp.path().join("data").join(LIFECYCLE_DIR);
        let owned = regular_user_dir(&temp);
        let owner_uid = owned.uid();
        let owner = user_name(owner_uid).unwrap_or_else(|| format!("uid {owner_uid}"));

        let error = check_lock_dir(&lock_dir, &owned, 0)
            .expect_err("a run as root must refuse a user's lifecycle dir")
            .to_string();

        assert!(error.contains("but ZeroClaw is running as root"), "{error}");
        assert!(
            error.contains(&format!("Run ZeroClaw as {owner}, without sudo.")),
            "{error}"
        );
        assert!(
            !error.contains("chown"),
            "root must not take it over: {error}"
        );
        assert_copyable(&error);
    }

    #[cfg(unix)]
    #[test]
    fn root_owned_lifecycle_dir_is_refused_through_the_effective_uid() {
        use std::os::unix::fs::MetadataExt;

        // validate_lock_dir reads the effective uid itself. A root-owned
        // directory tests that wiring, and which owner message it picks,
        // without changing any directory's owner.
        // SAFETY: geteuid has no preconditions.
        let euid = unsafe { libc::geteuid() };
        let root = Path::new("/");
        if euid == 0 || std::fs::symlink_metadata(root).unwrap().uid() != 0 {
            eprintln!("skipped: needs a non-root user and a root-owned /");
            return;
        }

        let error = validate_lock_dir(root)
            .expect_err("a root-owned lifecycle dir must be refused")
            .to_string();

        assert!(
            error.contains("belongs to root, usually because ZeroClaw was run with sudo"),
            "{error}"
        );
        assert!(
            !error.to_lowercase().contains("run zeroclaw as root"),
            "wrong advice: {error}"
        );
        let user = user_name(euid).unwrap_or_else(|| euid.to_string());
        let chown = format!(
            "sudo chown -R {} /",
            crate::util::shell_escape(std::ffi::OsStr::new(&user))
        );
        assert!(error.contains(&chown), "missing `{chown}`: {error}");
        assert_sentence_follows(&error, &chown);
        assert!(
            error.contains("If chown has no effect")
                && error.contains("does not keep Unix permissions"),
            "missing the filesystem note: {error}"
        );
        assert_copyable(&error);
    }

    #[cfg(unix)]
    #[test]
    fn root_run_refuses_a_lifecycle_dir_owned_by_another_user() {
        // Only root can give a directory away. CI containers often run as
        // root; elsewhere this test has nothing to check.
        // SAFETY: geteuid has no preconditions.
        if unsafe { libc::geteuid() } != 0 {
            eprintln!("skipped: needs root");
            return;
        }
        let temp = tempfile::TempDir::new().unwrap();
        let data_dir = temp.path().join("data");
        let lock_dir = data_dir.join(LIFECYCLE_DIR);
        std::fs::create_dir_all(&lock_dir).unwrap();
        std::os::unix::fs::chown(&lock_dir, Some(65534), Some(65534)).unwrap();
        let owner = user_name(65534).unwrap_or_else(|| "uid 65534".to_string());

        let error = ConfigOwnershipGuard::acquire(&data_dir)
            .expect_err("a run as root must refuse another user's lifecycle dir")
            .to_string();

        assert!(error.contains("but ZeroClaw is running as root"), "{error}");
        assert!(
            error.contains(&format!("Run ZeroClaw as {owner}, without sudo.")),
            "{error}"
        );
        assert!(
            !error.contains("chown"),
            "root must not take it over: {error}"
        );
        assert!(!lock_dir.join(LIFECYCLE_LOCK_FILE).exists());
    }

    /// A cause printed after the message (as `{:#}` does) must never land on
    /// the command line, so a sentence has to follow the command.
    #[cfg(unix)]
    fn assert_sentence_follows(error: &str, command: &str) {
        let last = error.trim_end().lines().last().unwrap_or_default();
        assert!(
            !last.contains(command),
            "the command must not end the message: {error}"
        );
    }

    /// An existing root-owned directory that the current, non-root user
    /// cannot write, standing in for a data directory left by a run with
    /// sudo. `/` is read-only on macOS, where mkdir reports EROFS instead.
    #[cfg(unix)]
    fn unwritable_root_owned_dir() -> Option<&'static Path> {
        use std::os::unix::ffi::OsStrExt;
        use std::os::unix::fs::MetadataExt;

        let denied = |path: &Path| {
            let path = std::ffi::CString::new(path.as_os_str().as_bytes()).unwrap();
            // SAFETY: `path` is a NUL-terminated string that outlives the call.
            let status = unsafe { libc::access(path.as_ptr(), libc::W_OK) };
            status != 0 && std::io::Error::last_os_error().raw_os_error() == Some(libc::EACCES)
        };
        ["/", "/root", "/private/var/root"]
            .into_iter()
            .map(Path::new)
            .find(|path| {
                std::fs::symlink_metadata(path)
                    .is_ok_and(|metadata| metadata.is_dir() && metadata.uid() == 0)
                    && denied(path)
            })
    }

    /// The refusal for a data directory the user cannot create `.lifecycle`
    /// in, after checking that the OS error is still its cause.
    #[cfg(unix)]
    fn data_dir_refusal(data_dir: &Path) -> String {
        let error = match ConfigOwnershipGuard::acquire(data_dir) {
            Err(ConfigOwnershipError::Unavailable(error)) => error,
            other => panic!("expected a refusal, got {other:?}"),
        };
        assert_eq!(
            error
                .root_cause()
                .downcast_ref::<std::io::Error>()
                .map(std::io::Error::kind),
            Some(std::io::ErrorKind::PermissionDenied),
            "the OS error must stay the cause: {error:#}"
        );
        error.to_string()
    }

    #[cfg(unix)]
    #[test]
    fn data_dir_owned_by_root_explains_the_refused_lifecycle_dir() {
        use std::os::unix::fs::PermissionsExt;

        // SAFETY: geteuid has no preconditions.
        let euid = unsafe { libc::geteuid() };
        if euid == 0 {
            eprintln!("skipped: root can create .lifecycle anywhere");
            return;
        }

        // A data directory the user owns but cannot write keeps the OS error.
        let temp = tempfile::TempDir::new().unwrap();
        let data_dir = temp.path().join("data");
        std::fs::create_dir(&data_dir).unwrap();
        std::fs::set_permissions(&data_dir, std::fs::Permissions::from_mode(0o500)).unwrap();
        let error = ConfigOwnershipGuard::acquire(&data_dir)
            .expect_err("an unwritable data dir must be refused")
            .to_string();
        std::fs::set_permissions(&data_dir, std::fs::Permissions::from_mode(0o700)).unwrap();
        assert!(
            error.starts_with("creating config lifecycle directory"),
            "{error}"
        );

        // After a first run with sudo, data/ belongs to root and the user
        // cannot create .lifecycle in it.
        let Some(root_dir) = unwritable_root_owned_dir() else {
            eprintln!("skipped: no root-owned directory this user cannot write");
            return;
        };

        let error = data_dir_refusal(root_dir);

        assert!(
            error.starts_with(&format!(
                "Data directory {} belongs to root, so ZeroClaw cannot create its config lock directory there.",
                root_dir.display()
            )),
            "{error}"
        );
        assert!(
            !error.contains("Config lifecycle directory"),
            "the data directory is not the lifecycle directory: {error}"
        );
        let user = user_name(euid).unwrap_or_else(|| euid.to_string());
        let chown = format!(
            "sudo chown -R {} {}",
            crate::util::shell_escape(std::ffi::OsStr::new(&user)),
            crate::util::shell_escape(root_dir.as_os_str())
        );
        assert!(error.contains(&chown), "missing `{chown}`: {error}");
        assert_sentence_follows(&error, &chown);
        assert!(
            error.contains(
                "If it belongs to root on purpose, run ZeroClaw as its owner or point data_dir elsewhere."
            ),
            "{error}"
        );
        assert_copyable(&error);
    }

    #[cfg(unix)]
    #[test]
    fn symlinked_data_dir_is_explained_at_the_directory_it_resolves_to() {
        // SAFETY: geteuid has no preconditions.
        let euid = unsafe { libc::geteuid() };
        if euid == 0 {
            eprintln!("skipped: root can create .lifecycle anywhere");
            return;
        }
        let Some(root_dir) = unwritable_root_owned_dir() else {
            eprintln!("skipped: no root-owned directory this user cannot write");
            return;
        };
        // The link itself belongs to the user; the directory it resolves to,
        // where creating .lifecycle failed, does not.
        let temp = tempfile::TempDir::new().unwrap();
        let link = temp.path().join("data");
        std::os::unix::fs::symlink(root_dir, &link).unwrap();
        let resolved = std::fs::canonicalize(root_dir).unwrap();

        let error = data_dir_refusal(&link);

        assert!(
            error.starts_with(&format!(
                "Data directory {} belongs to root",
                resolved.display()
            )),
            "{error}"
        );
        // `chown -R` does not follow a symlink operand, so the command must
        // name the resolved directory, never the link.
        let user = user_name(euid).unwrap_or_else(|| euid.to_string());
        let chown = format!(
            "sudo chown -R {} {}",
            crate::util::shell_escape(std::ffi::OsStr::new(&user)),
            crate::util::shell_escape(resolved.as_os_str())
        );
        assert!(error.contains(&chown), "missing `{chown}`: {error}");
        assert_copyable(&error);
    }

    #[cfg(unix)]
    #[test]
    fn foreign_owned_data_dir_offers_the_chown_only_if_it_is_yours() {
        use std::os::unix::fs::MetadataExt;

        // A data directory another regular user owns needs root to set up, so
        // render the refusal from real metadata and a different effective uid.
        let temp = tempfile::TempDir::new().unwrap();
        let data_dir = temp.path().join("data dir");
        let owner_uid = regular_user_dir(&temp).uid();
        let euid = owner_uid + 1;

        let error = owner_refusal(&data_dir, owner_uid, euid, OwnedDir::Data);

        let owner = user_name(owner_uid).unwrap_or_else(|| format!("uid {owner_uid}"));
        let user = user_name(euid).unwrap_or_else(|| format!("uid {euid}"));
        let chown_to = user_name(euid).unwrap_or_else(|| euid.to_string());
        assert!(
            error.starts_with(&format!(
                "Data directory {} belongs to {owner}, so ZeroClaw, running as {user}, cannot create its config lock directory there.",
                data_dir.display()
            )),
            "{error}"
        );
        let chown = format!(
            "sudo chown -R {} {}",
            crate::util::shell_escape(std::ffi::OsStr::new(&chown_to)),
            crate::util::shell_escape(data_dir.as_os_str())
        );
        let position = |needle: &str| {
            error
                .find(needle)
                .unwrap_or_else(|| panic!("missing `{needle}`: {error}"))
        };
        let run_as_at = position(&format!(
            "Run ZeroClaw as {owner}, or point data_dir at a directory of your own."
        ));
        let yours_at = position("If the directory really is yours");
        let chown_at = position(&chown);
        assert!(
            run_as_at < yours_at && yours_at < chown_at,
            "running as the owner comes first, the chown only if it is yours: {error}"
        );
        assert!(
            !error.contains("config-lifecycle.lock"),
            "the lifecycle contents do not describe a data directory: {error}"
        );
        assert_sentence_follows(&error, &chown);
        assert!(error.contains("If chown has no effect"), "{error}");
        assert_copyable(&error);
    }

    #[cfg(unix)]
    #[test]
    fn user_owned_data_dir_under_sudo_says_run_without_sudo() {
        use std::os::unix::fs::MetadataExt;

        let temp = tempfile::TempDir::new().unwrap();
        let data_dir = temp.path().join("data");
        let owner_uid = regular_user_dir(&temp).uid();
        let owner = user_name(owner_uid).unwrap_or_else(|| format!("uid {owner_uid}"));

        let error = owner_refusal(&data_dir, owner_uid, 0, OwnedDir::Data);

        assert_eq!(
            error,
            format!(
                "Data directory {} belongs to {owner}, but ZeroClaw is running as root. Run ZeroClaw as {owner}, without sudo.",
                data_dir.display()
            )
        );
        assert_copyable(&error);
    }

    #[test]
    fn execution_admission_blocks_delete_until_final_work_drops() {
        let mut config = Config::default();
        config.agents.insert(
            "alpha".to_string(),
            zeroclaw_config::schema::AliasedAgentConfig::default(),
        );
        let authority = LiveConfigAuthority::new(config);
        let capability = authority.execution_capability();
        let admission = capability.admit("alpha").unwrap();

        assert_eq!(admission.alias(), "alpha");
        assert_eq!(authority.agent_lifecycle().active_turn_count("alpha"), 1);
        assert!(matches!(
            authority.agent_lifecycle().begin_delete("alpha"),
            Err(AgentDeleteBlocker::ActiveTurns { .. })
        ));

        drop(admission);
        assert!(authority.agent_lifecycle().begin_delete("alpha").is_ok());
    }

    #[test]
    fn stale_execution_generation_is_rejected_before_target_admission() {
        let mut config = Config::default();
        config.agents.insert(
            "alpha".to_string(),
            zeroclaw_config::schema::AliasedAgentConfig::default(),
        );
        let authority = LiveConfigAuthority::new(config);
        let capability = authority.execution_capability();
        let generation = capability.agent_lifecycle_generation("alpha");
        let mut delete = authority.agent_lifecycle().begin_delete("alpha").unwrap();
        // A committed deletion advances the generation; a reservation alone
        // does not (see the refusal rollback regression below).
        delete.commit_destructive_mutation();
        drop(delete);

        assert_eq!(
            capability.admit_at("alpha", generation).err(),
            Some(AgentExecutionError::Admission(
                AgentAdmissionError::StaleGeneration {
                    alias: "alpha".to_string(),
                }
            ))
        );
    }

    #[tokio::test]
    async fn selection_during_delete_cannot_be_used_after_recreation() {
        let mut config = Config::default();
        config.agents.insert("alpha".into(), Default::default());
        let authority = LiveConfigAuthority::new(config);
        let deletion = authority.agent_lifecycle().begin_delete("alpha").unwrap();
        let selection = authority.execution_capability().capture_selection();
        drop(deletion);
        assert!(matches!(
            selection.resolve_and_admit("alpha"),
            Err(AgentExecutionError::Admission(
                AgentAdmissionError::Deleting { .. }
            ))
        ));
        // A config publication adds a new alias; the previously captured
        // selection was taken against the old publication and must not
        // admit the new alias.
        let commit = authority.begin_config_commit().await.unwrap();
        let mut published = commit.current_config();
        published.agents.insert("new".into(), Default::default());
        let revision = commit.next_revision().unwrap();
        commit.publish(revision, published).unwrap();
        assert!(matches!(
            selection.resolve_and_admit("new"),
            Err(AgentExecutionError::UnknownAlias { .. })
        ));
    }

    #[test]
    fn pre_admitted_execution_rejects_closed_generation_before_construction() {
        let mut config = Config::default();
        config.agents.insert(
            "alpha".to_string(),
            zeroclaw_config::schema::AliasedAgentConfig::default(),
        );
        let authority = LiveConfigAuthority::new(config);
        let admission = authority.execution_capability().admit("alpha").unwrap();

        authority.close_agent_lifecycle();

        assert_eq!(
            admission.revalidate().err(),
            Some(AgentExecutionError::Admission(
                AgentAdmissionError::GenerationClosing
            ))
        );
    }

    #[tokio::test]
    async fn detached_lifecycle_job_retains_alias_exclusion() {
        let lifecycle = AgentLifecycleCoordinator::default();
        let lease = lifecycle.begin_delete("alpha").unwrap();
        let release = Arc::new(tokio::sync::Notify::new());
        let task_release = Arc::clone(&release);
        let handle = spawn_agent_lifecycle_job(async move {
            let _lease = lease;
            task_release.notified().await;
        });
        drop(handle);

        assert!(lifecycle.reserve_admission("alpha").is_err());
        release.notify_one();
        tokio::time::timeout(std::time::Duration::from_secs(1), async {
            loop {
                if lifecycle.reserve_admission("alpha").is_ok() {
                    break;
                }
                tokio::task::yield_now().await;
            }
        })
        .await
        .expect("detached lifecycle job releases its lease after completion");
    }

    #[test]
    fn active_turn_blocks_same_alias_delete_without_blocking_other_aliases() {
        let lifecycle = AgentLifecycleCoordinator::default();
        let generation = lifecycle.alias_generation("alpha");
        let turn = lifecycle.reserve_turn("alpha").unwrap();

        assert_eq!(lifecycle.active_turn_count("alpha"), 1);
        assert_eq!(
            lifecycle.begin_delete("alpha").err(),
            Some(AgentDeleteBlocker::ActiveTurns {
                alias: "alpha".to_string(),
                count: 1,
            })
        );
        assert!(lifecycle.begin_delete("beta").is_ok());

        drop(turn);
        assert_eq!(lifecycle.active_turn_count("alpha"), 0);
        // Dropping the reservation without committing is a rollback: the old
        // generation and its producers stay usable.
        let delete = lifecycle.begin_delete("alpha").unwrap();
        drop(delete);
        assert!(lifecycle.reserve_turn_at("alpha", generation).is_ok());
        // Committing advances the generation exactly once; producers pinned to
        // the old generation are rejected afterwards.
        let mut delete = lifecycle.begin_delete("alpha").unwrap();
        delete.commit_destructive_mutation();
        drop(delete);
        assert_eq!(
            lifecycle.reserve_turn_at("alpha", generation).err(),
            Some(AgentAdmissionError::StaleGeneration {
                alias: "alpha".to_string(),
            })
        );
        assert!(lifecycle.reserve_turn("alpha").is_ok());
    }

    #[test]
    fn delete_preview_blocker_matches_authoritative_delete_check() {
        let lifecycle = AgentLifecycleCoordinator::default();
        let _reservation = lifecycle.reserve_admission("busy").unwrap();
        let expected = AgentDeleteBlocker::Reservations {
            alias: "busy".to_string(),
            count: 1,
        };

        assert_eq!(lifecycle.delete_blocker("busy"), Some(expected.clone()));
        assert_eq!(lifecycle.begin_delete("busy").err(), Some(expected));
    }

    #[test]
    fn pending_work_aliases_are_stable_for_diagnostics() {
        let lifecycle = AgentLifecycleCoordinator::default();
        let _zeta = lifecycle.begin_delete("zeta").unwrap();
        let _alpha = lifecycle.begin_delete("alpha").unwrap();
        let _beta = lifecycle.reserve_turn("beta").unwrap();

        assert_eq!(
            lifecycle.pending_work_aliases(),
            ["alpha".to_string(), "beta".to_string(), "zeta".to_string()]
        );
    }

    #[tokio::test]
    async fn closing_generation_rejects_admission_and_drains_detached_cleanup() {
        let authority = LiveConfigAuthority::new(Config::default());
        let lifecycle = authority.agent_lifecycle();
        let lease = lifecycle.begin_delete("alpha").unwrap();
        let reservation = lifecycle.reserve_admission("pending").unwrap();
        let session = lifecycle
            .reserve_admission("session")
            .unwrap()
            .publish()
            .unwrap();
        let turn = lifecycle.reserve_turn("turn").unwrap();
        let release = Arc::new(tokio::sync::Notify::new());
        let task_release = Arc::clone(&release);
        let handle = spawn_agent_lifecycle_job(async move {
            let _lease = lease;
            task_release.notified().await;
        });
        drop(handle);

        authority.close_agent_lifecycle();
        assert_eq!(
            lifecycle.reserve_admission("beta").err(),
            Some(AgentAdmissionError::GenerationClosing)
        );
        assert_eq!(
            lifecycle.reserve_turn("beta").err(),
            Some(AgentAdmissionError::GenerationClosing)
        );
        assert_eq!(
            lifecycle.begin_delete("beta").err(),
            Some(AgentDeleteBlocker::GenerationClosing)
        );

        let mut drain = std::pin::pin!(authority.drain_agent_lifecycle());
        assert!(
            tokio::time::timeout(std::time::Duration::from_millis(20), &mut drain)
                .await
                .is_err()
        );
        release.notify_waiters();
        assert!(
            tokio::time::timeout(std::time::Duration::from_millis(20), &mut drain)
                .await
                .is_err()
        );
        drop(turn);
        assert!(
            tokio::time::timeout(std::time::Duration::from_millis(20), &mut drain)
                .await
                .is_err()
        );
        drop(session);
        assert!(
            tokio::time::timeout(std::time::Duration::from_millis(20), &mut drain)
                .await
                .is_err()
        );
        assert_eq!(
            reservation.publish().err(),
            Some(AgentAdmissionError::GenerationClosing)
        );
        tokio::time::timeout(std::time::Duration::from_secs(1), drain)
            .await
            .expect("generation drain completes after detached cleanup");
    }

    #[tokio::test]
    async fn lifecycle_job_panic_releases_generation_drain() {
        let authority = LiveConfigAuthority::new(Config::default());
        let lease = authority.agent_lifecycle().begin_delete("alpha").unwrap();
        let handle = spawn_agent_lifecycle_job(async move {
            let _lease = lease;
            panic!("test cleanup panic");
        });
        authority.close_agent_lifecycle();
        assert!(handle.await.is_err());
        tokio::time::timeout(
            std::time::Duration::from_secs(1),
            authority.drain_agent_lifecycle(),
        )
        .await
        .expect("panic drops destructive leases and unblocks generation drain");
    }

    #[tokio::test]
    async fn generation_drain_retains_process_ownership_until_cleanup_finishes() {
        let _no_spawn = SPAWN_VS_LOCK_REACQUIRE.read().await;
        let temp = tempfile::TempDir::new().unwrap();
        let config = Config {
            data_dir: temp.path().to_path_buf(),
            ..Config::default()
        };
        let authority = LiveConfigAuthority::new_owned(config).unwrap();
        let lease = authority.agent_lifecycle().begin_delete("alpha").unwrap();
        let release = Arc::new(tokio::sync::Semaphore::new(0));
        let task_release = Arc::clone(&release);
        let cleanup = spawn_agent_lifecycle_job(async move {
            let _lease = lease;
            let _permit = task_release
                .acquire_owned()
                .await
                .expect("test release semaphore remains open");
        });
        authority.close_agent_lifecycle();

        assert!(matches!(
            ConfigOwnershipGuard::acquire(temp.path()),
            Err(ConfigOwnershipError::AlreadyOwned { .. })
        ));
        release.add_permits(1);
        cleanup.await.unwrap();
        authority.drain_agent_lifecycle().await;
        assert_eq!(
            authority.agent_lifecycle().reserve_turn("alpha").err(),
            Some(AgentAdmissionError::GenerationClosing)
        );
        ConfigOwnershipGuard::acquire(temp.path()).unwrap();
    }

    #[test]
    fn execution_capability_retains_process_ownership_after_authority_drop() {
        let _no_spawn = SPAWN_VS_LOCK_REACQUIRE.blocking_read();
        let temp = tempfile::TempDir::new().unwrap();
        let config = Config {
            data_dir: temp.path().to_path_buf(),
            ..Config::default()
        };
        let authority = LiveConfigAuthority::new_owned(config).unwrap();
        let capability = AgentExecutionCapability::from_parts(
            authority.live_handle(),
            authority.agent_lifecycle(),
        );
        drop(authority);

        assert!(matches!(
            ConfigOwnershipGuard::acquire(temp.path()),
            Err(ConfigOwnershipError::AlreadyOwned { .. })
        ));
        drop(capability);
        assert!(ConfigOwnershipGuard::acquire(temp.path()).is_ok());
    }

    #[test]
    fn refused_destructive_mutation_preserves_generation_and_producers() {
        let lifecycle = AgentLifecycleCoordinator::default();
        let generation = lifecycle.alias_generation("alpha");

        // A reserved-but-uncommitted delete (hard-reference refusal, config
        // save failure, or request cancellation before commit) rolls back
        // cleanly: admission is blocked only while reserved.
        let lease = lifecycle.begin_delete("alpha").unwrap();
        assert!(lifecycle.reserve_turn("alpha").is_err());
        drop(lease);
        assert_eq!(lifecycle.alias_generation("alpha"), generation);
        let producer = lifecycle.reserve_turn_at("alpha", generation).unwrap();
        drop(producer);

        // The equivalent rename reservation pair rolls back both aliases.
        let from = lifecycle.begin_delete("from").unwrap();
        let to = lifecycle.begin_delete("to").unwrap();
        drop(from);
        drop(to);
        assert_eq!(lifecycle.alias_generation("from"), 0);
        assert_eq!(lifecycle.alias_generation("to"), 0);
    }

    #[test]
    fn committed_destructive_mutation_advances_generation_exactly_once() {
        let lifecycle = AgentLifecycleCoordinator::default();
        let generation = lifecycle.alias_generation("alpha");
        let mut lease = lifecycle.begin_delete("alpha").unwrap();
        lease.commit_destructive_mutation();
        lease.commit_destructive_mutation();
        drop(lease);

        assert_eq!(
            lifecycle.alias_generation("alpha"),
            generation.wrapping_add(1)
        );
        assert_eq!(
            lifecycle.reserve_turn_at("alpha", generation).err(),
            Some(AgentAdmissionError::StaleGeneration {
                alias: "alpha".to_string(),
            })
        );
        assert!(lifecycle.reserve_turn("alpha").is_ok());
    }

    #[test]
    fn rename_reservation_pair_commits_together() {
        let lifecycle = AgentLifecycleCoordinator::default();
        let from_generation = lifecycle.alias_generation("from");
        let to_generation = lifecycle.alias_generation("to");
        let mut leases = vec![
            lifecycle.begin_delete("from").unwrap(),
            lifecycle.begin_delete("to").unwrap(),
        ];
        for lease in &mut leases {
            lease.commit_destructive_mutation();
        }
        drop(leases);

        assert_eq!(
            lifecycle.alias_generation("from"),
            from_generation.wrapping_add(1)
        );
        assert_eq!(
            lifecycle.alias_generation("to"),
            to_generation.wrapping_add(1)
        );
    }

    #[tokio::test]
    async fn reload_transfers_process_ownership_without_release() {
        let _no_spawn = SPAWN_VS_LOCK_REACQUIRE.read().await;
        let temp = tempfile::TempDir::new().unwrap();
        let config = Config {
            data_dir: temp.path().to_path_buf(),
            ..Config::default()
        };
        let authority = LiveConfigAuthority::new_owned(config).unwrap();

        authority.close_agent_lifecycle();
        authority.drain_agent_lifecycle_retaining_ownership().await;
        let guard = authority
            .take_process_ownership()
            .expect("reload drain retains ownership for transfer");

        // Ownership never left this process across the reload boundary: a
        // competing acquirer is refused for the whole interval.
        assert!(matches!(
            ConfigOwnershipGuard::acquire(temp.path()),
            Err(ConfigOwnershipError::AlreadyOwned { .. })
        ));

        // The next generation adopts the transferred guard together with a
        // freshly loaded protected snapshot and admits new work.
        let mut next_config = Config {
            data_dir: temp.path().to_path_buf(),
            ..Config::default()
        };
        next_config
            .agents
            .insert("alpha".into(), Default::default());
        let next = LiveConfigAuthority::new_with_ownership(next_config, guard);
        assert!(next.agent_lifecycle().reserve_turn("alpha").is_ok());
        drop(next);
        assert!(ConfigOwnershipGuard::acquire(temp.path()).is_ok());
    }

    fn store_with_running_turn(
        data_dir: &Path,
    ) -> Arc<dyn zeroclaw_infra::session_backend::SessionBackend> {
        use zeroclaw_api::model_provider::ChatMessage;
        let backend = zeroclaw_infra::make_session_backend(data_dir, "sqlite").unwrap();
        backend.append("s1", &ChatMessage::user("hello")).unwrap();
        backend
            .set_session_state("s1", "running", Some("turn-1"))
            .unwrap();
        backend
    }

    fn owned_config(data_dir: &Path) -> Config {
        Config {
            data_dir: data_dir.to_path_buf(),
            ..Config::default()
        }
    }

    #[test]
    fn fresh_owner_settles_turns_a_killed_process_left_running() {
        let temp = tempfile::TempDir::new().unwrap();
        // The previous process: it marked a turn running and never came back.
        drop(store_with_running_turn(temp.path()));

        let authority = LiveConfigAuthority::new_owned(owned_config(temp.path())).unwrap();

        let store = zeroclaw_infra::make_session_backend(temp.path(), "sqlite").unwrap();
        assert!(store.list_running_sessions().is_empty());
        let state = store.get_session_state("s1").unwrap().unwrap();
        assert_eq!(state.state, "error");
        assert_eq!(state.turn_id.as_deref(), Some("turn-1"));
        assert_eq!(store.load("s1").len(), 1, "the session's history is kept");
        drop(authority);
    }

    #[test]
    fn process_refused_ownership_settles_nothing() {
        let temp = tempfile::TempDir::new().unwrap();
        let owner = LiveConfigAuthority::new_owned(owned_config(temp.path())).unwrap();
        // The owner is running this turn right now.
        let store = store_with_running_turn(temp.path());

        assert!(LiveConfigAuthority::new_owned(owned_config(temp.path())).is_err());

        assert_eq!(store.list_running_sessions().len(), 1);
        assert_eq!(
            store.get_session_state("s1").unwrap().unwrap().state,
            "running"
        );
        drop(owner);
    }

    #[tokio::test]
    async fn reload_leaves_live_turns_running() {
        let temp = tempfile::TempDir::new().unwrap();
        let authority = LiveConfigAuthority::new_owned(owned_config(temp.path())).unwrap();
        // A turn this process started after it took ownership, still
        // draining while the daemon reloads.
        let store = store_with_running_turn(temp.path());

        authority.close_agent_lifecycle();
        authority.drain_agent_lifecycle_retaining_ownership().await;
        let guard = authority
            .take_process_ownership()
            .expect("reload drain retains ownership for transfer");
        // The next generation starts up exactly as the first one did.
        assert_eq!(guard.recover_abandoned_turns().unwrap(), 0);
        let next = LiveConfigAuthority::new_with_ownership(owned_config(temp.path()), guard);

        assert_eq!(
            store.get_session_state("s1").unwrap().unwrap().state,
            "running"
        );
        drop(next);
    }

    #[test]
    fn taking_ownership_again_in_the_same_process_settles_nothing() {
        let temp = tempfile::TempDir::new().unwrap();
        let first = LiveConfigAuthority::new_owned(owned_config(temp.path())).unwrap();
        // A turn started under the first acquisition and detached from it:
        // it is still running when the surface that admitted it is torn
        // down and started again.
        let store = store_with_running_turn(temp.path());
        drop(first);

        let second = LiveConfigAuthority::new_owned(owned_config(temp.path())).unwrap();

        assert_eq!(
            store.get_session_state("s1").unwrap().unwrap().state,
            "running"
        );
        drop(second);
    }

    /// Run state lives in the SQLite store whichever backend is configured:
    /// a process started on JSONL can reload onto SQLite later, and must not
    /// find turns from before it started still marked running.
    #[test]
    fn fresh_owner_settles_the_sqlite_store_when_jsonl_is_configured() {
        let temp = tempfile::TempDir::new().unwrap();
        drop(store_with_running_turn(temp.path()));
        let mut config = owned_config(temp.path());
        config.channels.session_backend = "jsonl".to_string();

        let authority = LiveConfigAuthority::new_owned(config).unwrap();

        let store = zeroclaw_infra::make_session_backend(temp.path(), "sqlite").unwrap();
        assert!(store.list_running_sessions().is_empty());
        drop(authority);
    }

    #[test]
    fn unreadable_store_does_not_stop_startup_and_the_error_is_returned() {
        let temp = tempfile::TempDir::new().unwrap();
        let db_path = zeroclaw_infra::session_sqlite::SqliteSessionBackend::db_path(temp.path());
        std::fs::create_dir_all(db_path.parent().unwrap()).unwrap();
        std::fs::write(&db_path, b"not a database").unwrap();

        let guard = ConfigOwnershipGuard::acquire(temp.path()).unwrap();
        assert!(guard.recover_abandoned_turns().is_err());
        drop(guard);

        let other = tempfile::TempDir::new().unwrap();
        let db_path = zeroclaw_infra::session_sqlite::SqliteSessionBackend::db_path(other.path());
        std::fs::create_dir_all(db_path.parent().unwrap()).unwrap();
        std::fs::write(&db_path, b"not a database").unwrap();
        assert!(LiveConfigAuthority::new_owned(owned_config(other.path())).is_ok());
    }
}
