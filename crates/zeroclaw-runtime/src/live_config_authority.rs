use std::collections::HashMap;
use std::fs::{File, OpenOptions, TryLockError};
use std::future::Future;
use std::path::{Path, PathBuf};
use std::sync::Arc;

use anyhow::{Context, Result};
use parking_lot::RwLock;
use zeroclaw_config::schema::Config;

#[cfg(unix)]
use std::os::unix::fs::{MetadataExt, OpenOptionsExt};

/// The live configuration state shared by one supervised daemon generation.
///
/// The write lock is deliberately paired with the config Arc so every
/// mutation path uses the same serialization witness as the live state.
#[derive(Clone)]
pub struct LiveConfigAuthority {
    config: Arc<RwLock<Config>>,
    config_write_lock: Arc<tokio::sync::Mutex<()>>,
    agent_lifecycle: AgentLifecycleCoordinator,
}

impl LiveConfigAuthority {
    /// Create the authority for one daemon generation.
    pub fn new(config: Config) -> Self {
        Self {
            config: Arc::new(RwLock::new(config)),
            config_write_lock: Arc::new(tokio::sync::Mutex::new(())),
            agent_lifecycle: AgentLifecycleCoordinator::default(),
        }
    }

    /// Create an authority that exclusively owns this config across processes.
    pub fn new_owned(config: Config) -> Result<Self> {
        let ownership = ConfigOwnershipGuard::acquire(&config.data_dir)?;
        Ok(Self::new_with_ownership(config, ownership))
    }

    /// Create an authority from a guard acquired by a caller that resolved the
    /// config identity before loading the executable config. The guard is
    /// transferred into the authority and shared by every derived capability.
    pub fn new_with_ownership(config: Config, ownership: ConfigOwnershipGuard) -> Self {
        let ownership = Arc::new(ownership);
        Self {
            config: Arc::new(RwLock::new(config)),
            config_write_lock: Arc::new(tokio::sync::Mutex::new(())),
            agent_lifecycle: AgentLifecycleCoordinator::with_ownership(ownership),
        }
    }

    /// Pair an existing live config handle with a local mutation witness.
    ///
    /// This preserves standalone callers that already own an `Arc<RwLock<Config>>`
    /// without claiming that their config participates in a supervised daemon's
    /// shared mutation domain.
    pub fn from_config(config: Arc<RwLock<Config>>) -> Self {
        Self {
            config,
            config_write_lock: Arc::new(tokio::sync::Mutex::new(())),
            agent_lifecycle: AgentLifecycleCoordinator::default(),
        }
    }

    /// Return the live config Arc shared by all consumers of this authority.
    pub fn config(&self) -> Arc<RwLock<Config>> {
        Arc::clone(&self.config)
    }

    /// Return the mutation witness shared by all consumers of this authority.
    pub fn config_write_lock(&self) -> Arc<tokio::sync::Mutex<()>> {
        Arc::clone(&self.config_write_lock)
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
            config: self.config(),
            agent_lifecycle: self.agent_lifecycle(),
        }
    }

    /// Close lifecycle admission for this daemon generation.
    pub fn close_agent_lifecycle(&self) {
        self.agent_lifecycle.close_generation();
    }

    /// Wait for post-commit agent cleanup admitted by this generation.
    pub async fn drain_agent_lifecycle(&self) {
        const DIAGNOSTIC_INTERVAL: std::time::Duration = std::time::Duration::from_secs(30);
        loop {
            if tokio::time::timeout(
                DIAGNOSTIC_INTERVAL,
                self.agent_lifecycle.drain_destructive_work(),
            )
            .await
            .is_ok()
            {
                return;
            }
            let aliases = self.agent_lifecycle.pending_destructive_aliases();
            ::zeroclaw_log::record!(
                WARN,
                ::zeroclaw_log::Event::new(module_path!(), ::zeroclaw_log::Action::Note)
                    .with_category(::zeroclaw_log::EventCategory::Agent)
                    .with_outcome(::zeroclaw_log::EventOutcome::Unknown)
                    .with_attrs(::serde_json::json!({
                        "pending_aliases": aliases,
                        "waited_seconds": DIAGNOSTIC_INTERVAL.as_secs(),
                    })),
                "daemon generation remains fail-closed while agent cleanup is still running"
            );
        }
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
    config: Arc<RwLock<Config>>,
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
    pub fn config_handle(&self) -> Arc<RwLock<Config>> {
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
        config: Arc<RwLock<Config>>,
        agent_lifecycle: AgentLifecycleCoordinator,
    ) -> Self {
        Self {
            config,
            agent_lifecycle,
        }
    }

    pub fn config_handle(&self) -> Arc<RwLock<Config>> {
        Arc::clone(&self.config)
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
}

#[cfg(unix)]
fn validate_lock_dir(path: &Path) -> Result<()> {
    let metadata = std::fs::metadata(path)
        .with_context(|| format!("inspecting config lifecycle directory {}", path.display()))?;
    let euid = unsafe { libc::geteuid() };
    anyhow::ensure!(
        metadata.uid() == euid,
        "config lifecycle directory {} is owned by uid {}, not the current user",
        path.display(),
        metadata.uid()
    );
    anyhow::ensure!(
        metadata.mode() & 0o022 == 0,
        "config lifecycle directory {} is writable by other users (mode {:o})",
        path.display(),
        metadata.mode() & 0o7777
    );
    Ok(())
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
        std::fs::create_dir_all(data_dir).with_context(|| {
            format!("creating config lifecycle directory {}", data_dir.display())
        })?;
        validate_lock_dir(data_dir)?;
        let path = data_dir.join("config-lifecycle.lock");
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
                Ok(Self { _file: file })
            }
            Err(TryLockError::WouldBlock) => Err(ConfigOwnershipError::AlreadyOwned { path }),
            Err(TryLockError::Error(error)) => Err(ConfigOwnershipError::Unavailable(
                anyhow::Error::new(error)
                    .context(format!("locking config lifecycle at {}", path.display())),
            )),
        }
    }
}

/// Detach post-commit lifecycle work while retaining alias exclusion.
///
/// Dropping the returned handle does not cancel the task, so request
/// cancellation cannot release the leases while cleanup is still running.
pub fn spawn_agent_lifecycle_job<F, T>(
    leases: Vec<AgentDeleteLease>,
    future: F,
) -> tokio::task::JoinHandle<T>
where
    F: Future<Output = T> + Send + 'static,
    T: Send + 'static,
{
    zeroclaw_spawn::spawn!(async move {
        let _leases = leases;
        future.await
    })
}

#[derive(Default)]
struct AliasLifecycleState {
    generation: u64,
    reservations: usize,
    live_sessions: usize,
    active_turns: usize,
    deleting: bool,
}

#[derive(Default)]
struct AgentLifecycleState {
    aliases: HashMap<String, AliasLifecycleState>,
    closing: bool,
}

/// Coordinates slow session admission with destructive alias mutations.
#[derive(Clone, Default)]
pub struct AgentLifecycleCoordinator {
    state: Arc<parking_lot::Mutex<AgentLifecycleState>>,
    destructive_idle: Arc<tokio::sync::Notify>,
    /// Every capability and lease retains this coordinator, keeping the
    /// process lock held until the last admitted continuation finishes.
    _ownership: Option<Arc<ConfigOwnershipGuard>>,
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
}

impl AgentLifecycleCoordinator {
    fn with_ownership(ownership: Arc<ConfigOwnershipGuard>) -> Self {
        Self {
            state: Arc::new(parking_lot::Mutex::new(AgentLifecycleState::default())),
            destructive_idle: Arc::new(tokio::sync::Notify::new()),
            _ownership: Some(ownership),
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
        let lifecycle = state.aliases.get(alias).map_or(Ok(()), |lifecycle| {
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
        });
        lifecycle
    }

    /// Enter destructive work for one alias after proving no admission or
    /// published session is using it. The returned lease keeps the alias
    /// unavailable until cleanup finishes.
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
        lifecycle.generation = lifecycle.generation.wrapping_add(1);
        Ok(AgentDeleteLease {
            coordinator: self.clone(),
            alias,
            active: true,
        })
    }

    pub fn delete_blocker(&self, alias: &str) -> Option<AgentDeleteBlocker> {
        Self::delete_blocker_locked(&self.state.lock(), alias)
    }

    pub fn pending_destructive_aliases(&self) -> Vec<String> {
        let mut aliases: Vec<String> = self
            .state
            .lock()
            .aliases
            .iter()
            .filter(|(_, lifecycle)| lifecycle.deleting)
            .map(|(alias, _)| alias.clone())
            .collect();
        aliases.sort();
        aliases
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

    /// Wait until every destructive lease admitted before generation close has
    /// left its detached post-commit cleanup task.
    pub async fn drain_destructive_work(&self) {
        loop {
            let notified = self.destructive_idle.notified();
            if self
                .state
                .lock()
                .aliases
                .values()
                .all(|lifecycle| !lifecycle.deleting)
            {
                return;
            }
            notified.await;
        }
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
            return Err(AgentAdmissionError::GenerationClosing);
        }
        if lifecycle.deleting || lifecycle.generation != self.generation {
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
        self.coordinator.destructive_idle.notify_waiters();
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn cloned_authority_preserves_config_and_write_lock_identity() {
        let authority = LiveConfigAuthority::new(Config::default());
        let cloned = authority.clone();

        assert!(Arc::ptr_eq(&authority.config(), &cloned.config()));
        assert!(Arc::ptr_eq(
            &authority.config_write_lock(),
            &cloned.config_write_lock()
        ));
        assert!(Arc::ptr_eq(
            &authority.agent_lifecycle().state,
            &cloned.agent_lifecycle().state
        ));
    }

    #[test]
    fn from_config_preserves_config_and_allocates_local_write_lock() {
        let config = Arc::new(RwLock::new(Config::default()));
        let authority = LiveConfigAuthority::from_config(Arc::clone(&config));
        let other = LiveConfigAuthority::from_config(config.clone());

        assert!(Arc::ptr_eq(&config, &authority.config()));
        assert!(!Arc::ptr_eq(
            &authority.config_write_lock(),
            &other.config_write_lock()
        ));
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

    #[test]
    fn config_ownership_is_exclusive_and_released_on_drop() {
        let temp = tempfile::TempDir::new().unwrap();
        let first = ConfigOwnershipGuard::acquire(temp.path()).unwrap();
        assert!(matches!(
            ConfigOwnershipGuard::acquire(temp.path()),
            Err(ConfigOwnershipError::AlreadyOwned { .. })
        ));
        drop(first);
        ConfigOwnershipGuard::acquire(temp.path()).unwrap();
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
        let delete = authority.agent_lifecycle().begin_delete("alpha").unwrap();
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

    #[test]
    fn selection_during_delete_cannot_be_used_after_recreation() {
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
        authority
            .config()
            .write()
            .agents
            .insert("new".into(), Default::default());
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
        let handle = spawn_agent_lifecycle_job(vec![lease], async move {
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
        let delete = lifecycle.begin_delete("alpha").unwrap();
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
    fn pending_destructive_aliases_are_stable_for_diagnostics() {
        let lifecycle = AgentLifecycleCoordinator::default();
        let _zeta = lifecycle.begin_delete("zeta").unwrap();
        let _alpha = lifecycle.begin_delete("alpha").unwrap();

        assert_eq!(
            lifecycle.pending_destructive_aliases(),
            ["alpha".to_string(), "zeta".to_string()]
        );
    }

    #[tokio::test]
    async fn closing_generation_rejects_admission_and_drains_detached_cleanup() {
        let authority = LiveConfigAuthority::new(Config::default());
        let lifecycle = authority.agent_lifecycle();
        let lease = lifecycle.begin_delete("alpha").unwrap();
        let release = Arc::new(tokio::sync::Notify::new());
        let task_release = Arc::clone(&release);
        let handle = spawn_agent_lifecycle_job(vec![lease], async move {
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
        tokio::time::timeout(std::time::Duration::from_secs(1), drain)
            .await
            .expect("generation drain completes after detached cleanup");
    }

    #[tokio::test]
    async fn lifecycle_job_panic_releases_generation_drain() {
        let authority = LiveConfigAuthority::new(Config::default());
        let lease = authority.agent_lifecycle().begin_delete("alpha").unwrap();
        let handle = spawn_agent_lifecycle_job(vec![lease], async move {
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
        let temp = tempfile::TempDir::new().unwrap();
        let config = Config {
            data_dir: temp.path().to_path_buf(),
            ..Config::default()
        };
        let authority = LiveConfigAuthority::new_owned(config).unwrap();
        let lease = authority.agent_lifecycle().begin_delete("alpha").unwrap();
        let release = Arc::new(tokio::sync::Semaphore::new(0));
        let task_release = Arc::clone(&release);
        let cleanup = spawn_agent_lifecycle_job(vec![lease], async move {
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
        assert!(matches!(
            ConfigOwnershipGuard::acquire(temp.path()),
            Err(ConfigOwnershipError::AlreadyOwned { .. })
        ));

        drop(authority);
        ConfigOwnershipGuard::acquire(temp.path()).unwrap();
    }

    #[test]
    fn execution_capability_retains_process_ownership_after_authority_drop() {
        let temp = tempfile::TempDir::new().unwrap();
        let config = Config {
            data_dir: temp.path().to_path_buf(),
            ..Config::default()
        };
        let authority = LiveConfigAuthority::new_owned(config).unwrap();
        let capability =
            AgentExecutionCapability::from_parts(authority.config(), authority.agent_lifecycle());
        drop(authority);

        assert!(matches!(
            ConfigOwnershipGuard::acquire(temp.path()),
            Err(ConfigOwnershipError::AlreadyOwned { .. })
        ));
        drop(capability);
        assert!(ConfigOwnershipGuard::acquire(temp.path()).is_ok());
    }
}
