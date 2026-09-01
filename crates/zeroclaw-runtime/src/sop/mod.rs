pub mod active_scope;
pub mod approval;
pub mod audit;
pub mod binding;
pub mod capability;
pub mod condition;
pub mod dispatch;
pub mod engine;
pub mod executor;
pub mod graph;
pub mod metrics;
pub mod procedural_memory;
pub mod route;
pub mod rundata;
pub mod schema;
pub mod scope;
pub mod step_contract;
pub mod store;
pub mod trigger_registry;
pub mod trigger_source;
pub mod types;
pub mod wire;

pub use approval::ApprovalDecision;
pub use audit::SopAuditLogger;
#[allow(unused_imports)]
pub use binding::{
    BindingContext, BindingRef, BindingScope, ExtractedBinding, extract_bindings, remap_step_refs,
    resolve_args,
};
pub use capability::{
    CapabilityContext, CapabilityInfo, CapabilityResult, SopCapability, SopCapabilityRegistry,
};
pub use engine::{
    CancelOutcome, MaintenanceSummary, SopEngine, err_is_cancellation_persistence_retained,
    err_is_resume_at_capacity, err_is_terminal_persistence_retained,
};
pub use executor::{drive_resumed_broker_action, spawn_headless_run_driver};
pub use graph::{
    FlowRole, GraphDiagnostic, GraphLayout, GraphLegend, GraphNode, GraphPin, GraphSeverity,
    GraphWire, LayoutGeometry, LegendEntry, NodeKind, NodePosition, NodeRunOverlay, NodeRunState,
    PinClass, RunOverlay, SopGraph, SopGraphExt, TRIGGER_NODE_BASE, TextGraphFormat, ToolSpecs,
    render_graph_text,
};
pub use metrics::SopMetricsCollector;
pub use scope::StepToolScope;
pub use step_contract::{StepFailure, StepRouting, SwitchRule};
pub use store::{
    ClaimToken, PersistedRun, ProposalKind, ProposalRecord, ProposalStatus, SopEventRecord,
    SopRunStore, SqliteRunStore, StoreError, build_run_store,
};
pub use trigger_registry::{
    BoundTriggerSource, ChannelAlias, ChannelTriggerKind, ConditionField, ConditionValueType,
    ConfiguredChannel, PayloadContract, TriggerField, TriggerFieldKind, TriggerSourceRegistry,
    build_registry, registry_from_config,
};
pub use types::{
    DeterministicRunState, DeterministicSavings, FilesystemEventKind, PlannedToolCall, Sop,
    SopEvent, SopExecutionMode, SopPriority, SopRun, SopRunAction, SopRunStatus, SopRunSummary,
    SopStep, SopStepKind, SopStepResult, SopStepStatus, SopTrigger, SopTriggerSource, StepSchema,
    StepToolCall,
};
pub use wire::{WireEdit, WireError, WireOp, apply_wire};

use anyhow::Result;
use std::path::{Path, PathBuf};
use std::sync::{Arc, Mutex};

use types::{SopManifest, SopMeta};
use zeroclaw_config::schema::SopConfig;
use zeroclaw_memory::traits::Memory;

/// Build the tool-spec map an SOP graph projection uses to type step pins.
/// Keys are tool names; values are the tool's declared `parameters` (input
/// pins) and `output` (output pin) schema. Derived once from the agent's
/// resolved security policy so the pins mirror the exact tools the step can
/// call, not a hand-authored list.
#[must_use]
pub fn tool_specs_from_config(
    config: &zeroclaw_config::schema::Config,
    agent_alias: &str,
) -> ToolSpecs {
    let security = Arc::new(
        zeroclaw_config::policy::SecurityPolicy::for_agent(config, agent_alias).unwrap_or_default(),
    );
    crate::tools::default_tools(security)
        .iter()
        .map(|tool| {
            let spec = tool.spec();
            (spec.name.clone(), spec)
        })
        .collect()
}

/// Injected side-effect adapters for [`build_sop_engine`]. Each is optional and
/// fail-closed when absent: the route falls back to the log-only no-op adapter,
/// and the `forge.comment` / `llm.generate` capabilities report a clear failure
/// instead of acting. The daemon injects real implementations; CLI / standalone
/// callers pass `SopEngineAdapters::default()`.
#[derive(Default)]
pub struct SopEngineAdapters {
    /// Delivers approval request / escalation notices to a channel.
    pub route: Option<Arc<dyn approval::ApprovalRouteAdapter>>,
    /// Posts a SOP step's comment to a git forge (`forge.comment`).
    pub forge: Option<Arc<dyn capability::ForgeCommentAdapter>>,
    /// Runs one bounded model call as a pipeline step (`llm.generate`).
    pub llm: Option<Arc<dyn capability::LlmGenerateAdapter>>,
}

/// Build a single shared SopEngine + SopAuditLogger pair.
/// This is the sole construction site for SOP state within a daemon.
/// Callers receive `Arc<Mutex<SopEngine>>` and `Arc<SopAuditLogger>`
/// handles — never call `SopEngine::new` or `SopAuditLogger::new`
/// directly outside this module.
///
/// The two directory arguments serve different roles and must not be conflated:
/// - `data_dir` is the daemon state dir. It anchors the durable run store, which
///   lands at `<data_dir>/sop/runs.db` unless `[sop] run_state_dir` overrides it.
/// - `install_root` is the install root (`config.install_root_dir()`, i.e.
///   `config_path`'s parent). It anchors SOP-*definition* loading, so a relative
///   `[sop] sops_dir` (documented `shared/sops`) resolves to `<install>/shared/sops`
///   — the same directory the web/RPC SOP author writes to. Passing `data_dir` for
///   both (the historical bug) made the engine load definitions from `<data_dir>/sops`,
///   which authored SOPs never populate, so every manual trigger reported "no
///   matching manual trigger".
pub fn build_sop_engine(
    config: SopConfig,
    data_dir: &Path,
    install_root: &Path,
    audit_memory: Arc<dyn Memory>,
    adapters: SopEngineAdapters,
) -> (Arc<Mutex<SopEngine>>, Arc<SopAuditLogger>) {
    let SopEngineAdapters {
        route: route_adapter,
        forge: forge_adapter,
        llm: llm_adapter,
    } = adapters;
    // Select the run-state backend from config (default: durable sqlite, so parked
    // HITL runs survive a restart). A backend-open failure must not crash daemon
    // startup, so fall back to in-memory with a loud log. The run store is anchored
    // at the daemon data dir, so a durable store lands at `<data_dir>/sop/runs.db`
    // unless `[sop] run_state_dir` overrides it.
    let store = store::build_run_store(&config, data_dir).unwrap_or_else(|e| {
        ::zeroclaw_log::record!(
            WARN,
            ::zeroclaw_log::Event::new(module_path!(), ::zeroclaw_log::Action::Note)
                .with_outcome(::zeroclaw_log::EventOutcome::Unknown)
                .with_attrs(::serde_json::json!({"error": e.to_string()})),
            "SOP: run-store init failed; falling back to in-memory"
        );
        Arc::new(store::InMemoryRunStore::new())
    });
    let (run_tx, _run_rx) = tokio::sync::broadcast::channel(256);
    // EPIC G: the approval broker (membership + quorum) resolves policies/groups
    // from the engine's live `[sop.approval]` at use-time. The route adapter
    // delivers approval request/escalation notices to a channel; the daemon injects
    // a real channel-delivering adapter, while CLI/standalone callers pass `None`
    // and fall back to the no-op (log-only) adapter - unchanged behavior there.
    let route: Arc<dyn approval::ApprovalRouteAdapter> =
        route_adapter.unwrap_or_else(|| Arc::new(approval::NoopRouteAdapter));
    let approval_broker = Arc::new(approval::ApprovalBroker::with_route(route));
    // Deterministic capability registry: builtins + the injected-adapter
    // capabilities (`forge.comment` write-back, `llm.generate` bounded model
    // call). The daemon injects real adapters; CLI/standalone callers pass
    // `SopEngineAdapters::default()`, leaving both fail-closed exactly like
    // `shell.exec`/`notify.channel`.
    let mut capabilities = capability::SopCapabilityRegistry::with_builtins();
    capabilities.register(capability::ForgeCommentCapability::new(forge_adapter));
    capabilities.register(capability::LlmGenerateCapability::new(llm_adapter));
    let mut engine = SopEngine::new(config)
        .with_store(store)
        .with_metrics(SopMetricsCollector::shared())
        .with_run_notifier(run_tx)
        .with_approval_broker(approval_broker)
        .with_capabilities(Arc::new(capabilities));
    engine.reload(install_root);
    engine.restore_runs();
    let engine = Arc::new(Mutex::new(engine));
    let audit = Arc::new(SopAuditLogger::new(audit_memory));
    (engine, audit)
}

/// Parse an execution mode string into `SopExecutionMode`, falling back to
/// `Supervised` for unknown values.
pub fn parse_execution_mode(s: &str) -> SopExecutionMode {
    match s.trim().to_lowercase().as_str() {
        "auto" => SopExecutionMode::Auto,
        "step_by_step" => SopExecutionMode::StepByStep,
        "priority_based" => SopExecutionMode::PriorityBased,
        "deterministic" => SopExecutionMode::Deterministic,
        // "supervised" and any unknown value
        _ => SopExecutionMode::Supervised,
    }
}

// ── SOP directory helpers ───────────────────────────────────────

/// Canonical fallback SOPs directory: `<install>/shared/sops`.
fn default_sops_dir(install_root: &Path) -> PathBuf {
    install_root.join("shared").join("sops")
}

/// Resolve the SOPs directory from config, falling back to the canonical
/// shared default.
///
/// A relative `config_dir` resolves against `install_root` (the install root,
/// `config_path`'s parent), matching the `skill-bundles` convention: the
/// documented `shared/sops` value yields `<install>/shared/sops`, the same
/// directory the web/RPC SOP author writes to and the CLI scans. An absolute
/// or `~`-prefixed value is used as-is (`Path::join` replaces the base entirely
/// when the joined path is itself absolute). Unset, empty, or whitespace-only
/// falls back to the canonical `<install>/shared/sops` — the same disabled
/// sentinel `SopConfig::runtime_enabled()` recognizes, so the CLI/RPC scan root
/// never diverges from whether the daemon built an engine.
pub fn resolve_sops_dir(install_root: &Path, config_dir: Option<&str>) -> PathBuf {
    match config_dir {
        Some(dir) if !dir.trim().is_empty() => {
            let expanded = shellexpand::tilde(dir);
            install_root.join(expanded.as_ref())
        }
        _ => default_sops_dir(install_root),
    }
}

/// Resolve `<sops_dir>/<name>`, accepting only a single normal path
/// component so caller-controlled names cannot escape the SOP root.
fn resolve_sop_dir(sops_dir: &Path, name: &str) -> Result<PathBuf> {
    let mut components = Path::new(name).components();
    let single_normal = matches!(
        (components.next(), components.next()),
        (Some(std::path::Component::Normal(_)), None)
    );
    if single_normal && !name.contains(['/', '\\', '\0']) {
        Ok(sops_dir.join(name))
    } else {
        anyhow::bail!(
            "invalid SOP name '{name}': must be a single path component (no separators, '.', '..', or absolute paths)"
        )
    }
}

// ── SOP loading ─────────────────────────────────────────────────

/// Load all SOPs from the configured directory, resolved against `install_root`.
pub fn load_sops(
    install_root: &Path,
    config_dir: Option<&str>,
    default_execution_mode: SopExecutionMode,
) -> Vec<Sop> {
    let dir = resolve_sops_dir(install_root, config_dir);
    load_sops_from_directory(&dir, default_execution_mode)
}

/// Load a single SOP by directory name from the SOPs root. Errors if the
/// directory or its `SOP.toml` is missing or malformed.
pub fn load_sop_by_name(
    sops_dir: &Path,
    name: &str,
    default_execution_mode: SopExecutionMode,
) -> Result<Sop> {
    load_sop(&resolve_sop_dir(sops_dir, name)?, default_execution_mode)
}

/// Delete an SOP's directory (manifest, steps, everything). Errors if no
/// SOP with that name exists.
pub fn delete_sop(sops_dir: &Path, name: &str) -> Result<()> {
    let _lock = lock_sops_dir(sops_dir)?;
    delete_sop_unlocked(sops_dir, name)
}

fn delete_sop_unlocked(sops_dir: &Path, name: &str) -> Result<()> {
    let dir = resolve_sop_dir(sops_dir, name)?;
    if !dir.exists() {
        anyhow::bail!("SOP '{name}' not found");
    }
    std::fs::remove_dir_all(&dir)?;
    Ok(())
}

/// Create a new SOP on disk, refusing to overwrite an existing one. Same
/// normalization and validation as `save_sop`.
pub fn create_sop(sops_dir: &Path, sop: &Sop) -> Result<()> {
    let _lock = lock_sops_dir(sops_dir)?;
    create_sop_unlocked(sops_dir, sop)
}

fn create_sop_unlocked(sops_dir: &Path, sop: &Sop) -> Result<()> {
    if resolve_sop_dir(sops_dir, &sop.name)?.exists() {
        anyhow::bail!("SOP '{}' already exists", sop.name);
    }
    save_sop_unlocked(sops_dir, sop)
}

/// Name of the advisory lock file that serializes authoring writes under a
/// SOP root. It is a plain file, so the directory scan that loads SOPs (which
/// only descends into directories holding a `SOP.toml`) never sees it.
const AUTHORING_LOCK_FILE: &str = ".sop-authoring.lock";

/// How long an authoring call waits for the SOP root before giving up. The
/// operations under the lock are a handful of filesystem calls, so real
/// contention clears in milliseconds; the bound exists so a stuck or killed
/// holder cannot wedge a request thread indefinitely.
const AUTHORING_LOCK_TIMEOUT: std::time::Duration = std::time::Duration::from_secs(10);

/// Held for the duration of one authoring mutation. The kernel releases the
/// advisory lock when the file handle drops, including on process death, so a
/// crashed writer cannot leave the root permanently locked.
#[derive(Debug)]
struct AuthoringLock {
    _file: std::fs::File,
}

/// Serialize every write to a SOP root: create, save, delete, and rename.
///
/// Each of those is a multi-step read-modify-write over a SOP directory, and
/// the daemon runs them concurrently. Independent RPC connections each get
/// their own dispatcher, the gateway serves its authoring routes on the async
/// runtime, and the CLI is a separate process against the same root. Without a
/// shared boundary a create can claim a name between rename's collision check
/// and its move, a save can land between rename's snapshot and its commit and
/// then be overwritten by that stale snapshot, and a failed rename's rollback
/// can revert a save that succeeded in the meantime.
///
/// `File::lock` is advisory and scoped to the open file description, so a
/// fresh handle per acquisition serializes threads within one process and
/// processes against each other, on both Unix and Windows.
///
/// Readers are deliberately not covered. Every write under this lock lands
/// through an atomic file or directory rename, so a concurrent reader sees one
/// whole revision or the other, never a torn one.
fn lock_sops_dir(sops_dir: &Path) -> Result<AuthoringLock> {
    use anyhow::Context as _;

    std::fs::create_dir_all(sops_dir)
        .with_context(|| format!("creating SOP root {} before locking it", sops_dir.display()))?;
    let lock_path = sops_dir.join(AUTHORING_LOCK_FILE);
    let file = std::fs::OpenOptions::new()
        .read(true)
        .write(true)
        .create(true)
        .truncate(false)
        .open(&lock_path)
        .with_context(|| format!("opening SOP authoring lock {}", lock_path.display()))?;

    let deadline = std::time::Instant::now() + AUTHORING_LOCK_TIMEOUT;
    loop {
        match file.try_lock() {
            Ok(()) => return Ok(AuthoringLock { _file: file }),
            Err(std::fs::TryLockError::WouldBlock) => {
                if std::time::Instant::now() >= deadline {
                    anyhow::bail!(
                        "another SOP authoring operation is holding {}; try again",
                        lock_path.display()
                    );
                }
                std::thread::sleep(std::time::Duration::from_millis(20));
            }
            Err(std::fs::TryLockError::Error(e)) => {
                return Err(anyhow::Error::new(e).context(format!(
                    "locking SOP authoring root {}",
                    lock_path.display()
                )));
            }
        }
    }
}

/// Resolve `<sops_dir>/<name>` for a SOP that must already exist, refusing
/// anything that is not a real directory sitting directly in the root.
///
/// `resolve_sop_dir` is lexical: it proves the caller passed one path
/// component, not that the component is what it appears to be. A symlink
/// planted in the root passes that check and then silently redirects every
/// subsequent read and write at its target, so an operation that believes it
/// is editing an in-root SOP would rewrite a manifest outside the root
/// entirely. Both checks below are no-follow, and the canonical path is
/// confirmed to stay under the canonical root.
fn resolve_existing_sop_dir(
    sops_dir: &Path,
    name: &str,
) -> std::result::Result<PathBuf, SopAuthorError> {
    let dir = resolve_sop_dir(sops_dir, name).map_err(SopAuthorError::Other)?;
    let Ok(meta) = std::fs::symlink_metadata(&dir) else {
        return Err(SopAuthorError::NotFound(name.to_string()));
    };
    if !meta.is_dir() {
        return Err(SopAuthorError::Other(anyhow::Error::msg(format!(
            "SOP '{name}' is not a directory in the SOP root (a symlink or file cannot be \
             authored through)"
        ))));
    }
    let manifest = dir.join("SOP.toml");
    match std::fs::symlink_metadata(&manifest) {
        Ok(meta) if meta.is_file() => {}
        Ok(_) => {
            return Err(SopAuthorError::Other(anyhow::Error::msg(format!(
                "SOP '{name}' has a SOP.toml that is not a regular file"
            ))));
        }
        Err(e) => return Err(SopAuthorError::Io(e.into())),
    }
    let (Ok(canonical_dir), Ok(canonical_root)) =
        (std::fs::canonicalize(&dir), std::fs::canonicalize(sops_dir))
    else {
        return Err(SopAuthorError::Other(anyhow::Error::msg(format!(
            "SOP '{name}' could not be resolved inside the SOP root"
        ))));
    };
    if !canonical_dir.starts_with(&canonical_root) {
        return Err(SopAuthorError::Other(anyhow::Error::msg(format!(
            "SOP '{name}' resolves outside the SOP root"
        ))));
    }
    Ok(dir)
}

/// Typed classification of an authoring failure so transports map it to the
/// right status/RPC code without matching on stringified message substrings.
#[derive(Debug)]
pub enum SopAuthorError {
    AlreadyExists(String),
    NotFound(String),
    /// The request was fine; the filesystem was not. Transports map this to a
    /// server error, never to a client input error.
    Io(anyhow::Error),
    Other(anyhow::Error),
}

impl std::fmt::Display for SopAuthorError {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        match self {
            SopAuthorError::AlreadyExists(name) => write!(f, "SOP '{name}' already exists"),
            SopAuthorError::NotFound(name) => write!(f, "SOP '{name}' not found"),
            SopAuthorError::Io(e) | SopAuthorError::Other(e) => write!(f, "{e}"),
        }
    }
}

impl std::error::Error for SopAuthorError {}

pub fn create_sop_typed(sops_dir: &Path, sop: &Sop) -> std::result::Result<(), SopAuthorError> {
    let dir = resolve_sop_dir(sops_dir, &sop.name).map_err(SopAuthorError::Other)?;
    let _lock = lock_sops_dir(sops_dir).map_err(SopAuthorError::Io)?;
    // Re-checked under the lock: the bare check above is only a fast reject,
    // and the answer is not trustworthy until nothing else can be writing.
    if dir.exists() {
        return Err(SopAuthorError::AlreadyExists(sop.name.clone()));
    }
    save_sop_unlocked(sops_dir, sop).map_err(SopAuthorError::Other)
}

pub fn delete_sop_typed(sops_dir: &Path, name: &str) -> std::result::Result<(), SopAuthorError> {
    let dir = resolve_sop_dir(sops_dir, name).map_err(SopAuthorError::Other)?;
    let _lock = lock_sops_dir(sops_dir).map_err(SopAuthorError::Io)?;
    if !dir.exists() {
        return Err(SopAuthorError::NotFound(name.to_string()));
    }
    std::fs::remove_dir_all(&dir).map_err(|e| SopAuthorError::Io(e.into()))
}

/// Rename an existing SOP: move `<sops_dir>/<from>/` to `<sops_dir>/<to>/`
/// and update the manifest's `[sop] name` so the directory and the name the
/// runtime loads stay in agreement.
///
/// Rename is a pure identity change. Only the `[sop] name` value in
/// `SOP.toml` is rewritten, keeping that line's own comment and spacing;
/// steps, triggers, key order, and every other manifest key are left alone,
/// and `SOP.md` is never touched. That keeps a rename from quietly
/// materializing defaults (an execution mode the manifest deliberately left
/// unset, say) the way a load/save round trip would. The one cosmetic change
/// is quote style: a single-quoted name comes back double-quoted.
///
/// One writer at a time. A concurrent `save_sop`, `create_sop_typed`, or
/// second rename against the same SOP can interleave with the steps below;
/// the authoring surfaces are a single daemon, and nothing here adds
/// cross-process locking.
///
/// Ordering is what makes this safe against an interrupted rename. The
/// directory move is a single `rename(2)`, and it goes last:
///
/// 1. collision-check `to` and strict-validate the renamed SOP, before
///    anything on disk moves;
/// 2. rewrite `[sop] name` in place, via a temp file renamed over the
///    manifest, so `SOP.toml` is never half-written;
/// 3. move the directory - the commit point.
///
/// The SOP therefore lives in exactly one directory at every instant: there
/// is no window with two copies (a fork) or zero copies (a loss). An
/// interruption between steps 2 and 3 leaves one directory whose name lags
/// its manifest; re-running the same rename finishes the job. If step 3 fails
/// outright the manifest is rolled back, leaving the SOP exactly as it was
/// found.
///
/// Each step commits through a rename, so a reader and a process killed
/// mid-rename both see one whole revision. The directory holding each renamed
/// entry is flushed afterwards, so on Unix the two steps are durable across a
/// machine crash in the same order they were applied: an interrupted rename
/// leaves the SOP either wholly moved or wholly not, never both places or
/// neither. macOS honors the flush for ordering without draining the device
/// cache, and Windows has no directory-sync primitive, so on those platforms
/// that last step is the filesystem's to keep.
///
/// Errors: `NotFound` if `from` is not an SOP directory, `AlreadyExists` if
/// anything already occupies `to`, and `Other` for an invalid name (the same
/// single-path-component check every other authoring helper applies), a SOP
/// that strict validation rejects, or an I/O failure.
pub fn rename_sop_typed(
    sops_dir: &Path,
    from: &str,
    to: &str,
    default_execution_mode: SopExecutionMode,
) -> std::result::Result<(), SopAuthorError> {
    let to_dir = resolve_sop_dir(sops_dir, to).map_err(SopAuthorError::Other)?;
    if from == to {
        return Err(SopAuthorError::Other(anyhow::Error::msg(format!(
            "SOP '{from}' is already named '{to}'"
        ))));
    }
    // Everything from here to the move is one transaction. Without the lock a
    // create could claim `to` after the collision check, or a save could land
    // between the snapshot and the commit and then be reverted by it.
    let _lock = lock_sops_dir(sops_dir).map_err(SopAuthorError::Io)?;
    let from_dir = resolve_existing_sop_dir(sops_dir, from)?;
    // Collision check under the lock, so the answer still holds at the move.
    // `same_path` keeps a case-only rename ('Deploy' -> 'deploy') working on
    // case-insensitive filesystems, where the target reports as occupied by
    // the source itself.
    if path_occupied(&to_dir) && !same_path(&from_dir, &to_dir) {
        return Err(SopAuthorError::AlreadyExists(to.to_string()));
    }

    // Strict-save validation still applies: a rename cannot put a SOP back on
    // disk that `save_sop` would have refused to write in the first place.
    let mut renamed = load_sop(&from_dir, default_execution_mode).map_err(classify_author_error)?;
    renamed.name = to.to_string();
    let validation = validate_sop_strict(&renamed);
    if !validation.is_ok() {
        return Err(SopAuthorError::Other(anyhow::Error::msg(format!(
            "SOP rejected: {}",
            validation.blocking.join("; ")
        ))));
    }

    let manifest_path = from_dir.join("SOP.toml");
    let original =
        std::fs::read_to_string(&manifest_path).map_err(|e| SopAuthorError::Io(e.into()))?;
    let updated = manifest_with_name(&original, to).map_err(SopAuthorError::Other)?;

    write_file_atomic(&manifest_path, &updated).map_err(SopAuthorError::Io)?;
    if let Err(e) = std::fs::rename(&from_dir, &to_dir) {
        // The move is the commit point; it did not happen, so put the old
        // name back rather than leaving a directory that disagrees with its
        // manifest. If even that fails the SOP is left in the mismatched
        // state, so the caller has to hear about it.
        let mut msg = format!("failed to move SOP '{from}' to '{to}': {e}");
        if let Err(rollback) = write_file_atomic(&manifest_path, &original) {
            msg.push_str(&format!(
                "; the manifest could not be rolled back and still names '{to}' \
                 (re-run the rename to finish it): {rollback}"
            ));
        }
        return Err(SopAuthorError::Io(anyhow::Error::msg(msg)));
    }
    // The move already happened and is visible, so a failure to flush the SOP
    // root cannot be reported as a failed rename: the caller would retry an
    // operation that has in fact completed. Record it instead, because the
    // only thing lost is the power-loss guarantee.
    if let Err(e) = sync_dir(sops_dir) {
        ::zeroclaw_log::record!(
            WARN,
            ::zeroclaw_log::Event::new(module_path!(), ::zeroclaw_log::Action::Note)
                .with_outcome(::zeroclaw_log::EventOutcome::Unknown)
                .with_attrs(::serde_json::json!({
                    "error": format!("{e}"),
                    "from": from,
                    "to": to,
                })),
            "SOP rename committed but the SOP root could not be synchronized"
        );
    }
    Ok(())
}

/// Split a failure into "the filesystem broke" and "the SOP is wrong", so a
/// transport can answer a caller with a server error rather than telling them
/// to fix input that was never the problem. A malformed manifest is the
/// caller's; an unreadable one is ours.
fn classify_author_error(e: anyhow::Error) -> SopAuthorError {
    if e.chain().any(|cause| cause.is::<std::io::Error>()) {
        SopAuthorError::Io(e)
    } else {
        SopAuthorError::Other(e)
    }
}

/// Whether anything already occupies `path`, a broken symlink included -
/// `Path::exists` follows links and reports those as absent, which would let
/// a rename land on top of one.
fn path_occupied(path: &Path) -> bool {
    std::fs::symlink_metadata(path).is_ok()
}

/// Whether both paths resolve to the same directory on disk. Used to tell a
/// case-only rename on a case-insensitive filesystem apart from a genuine
/// collision with a different SOP; anything that fails to canonicalize is
/// treated as a collision.
fn same_path(a: &Path, b: &Path) -> bool {
    match (std::fs::canonicalize(a), std::fs::canonicalize(b)) {
        (Ok(a), Ok(b)) => a == b,
        _ => false,
    }
}

/// Rewrite only `[sop] name` in a SOP manifest, preserving every other key,
/// comment, and ordering decision in the document.
fn manifest_with_name(manifest_src: &str, new_name: &str) -> Result<String> {
    let mut doc: toml_edit::DocumentMut = manifest_src
        .parse()
        .map_err(|e| anyhow::Error::msg(format!("SOP.toml is not valid TOML: {e}")))?;
    let table = doc
        .get_mut("sop")
        .and_then(toml_edit::Item::as_table_like_mut)
        .ok_or_else(|| anyhow::Error::msg("SOP.toml has no [sop] table"))?;
    match table
        .get_mut("name")
        .and_then(toml_edit::Item::as_value_mut)
    {
        // Swap the string in place and put the original decor back, so a
        // trailing comment on the name line (`name = "x" # note`) and the
        // surrounding whitespace survive the edit.
        Some(existing) => {
            let decor = existing.decor().clone();
            *existing = toml_edit::Value::from(new_name);
            *existing.decor_mut() = decor;
        }
        None => {
            table.insert("name", toml_edit::value(new_name));
        }
    }
    Ok(doc.to_string())
}

/// Replace a file's contents atomically: stage a sibling temp file, flush it
/// to disk, then rename it over the target. Readers see either the old
/// contents or the new ones, never a half-written file.
///
/// The staging file is created fresh and exclusively under a name nothing can
/// predict. A fixed name like `.SOP.toml.tmp` would be wrong twice over: two
/// concurrent writers would share one staging path and could commit each
/// other's bytes, and a symlink planted at that name would be followed and its
/// target truncated. The target's permissions carry over, so replacing the
/// inode cannot widen a deliberately tight manifest mode.
fn write_file_atomic(path: &Path, contents: &str) -> Result<()> {
    use std::io::Write as _;

    let Some(dir) = path.parent() else {
        anyhow::bail!("cannot write '{}': no parent directory", path.display());
    };

    let mut tmp = tempfile::NamedTempFile::new_in(dir)?;
    tmp.write_all(contents.as_bytes())?;
    tmp.as_file().sync_all()?;
    if let Ok(existing) = std::fs::metadata(path) {
        tmp.as_file().set_permissions(existing.permissions())?;
    }
    tmp.persist(path).map_err(|e| anyhow::Error::new(e.error))?;
    // Flushing the file is only half of it: until the directory entry that
    // names it is on disk too, a power loss can take the rename back. This is
    // fatal here because nothing has been committed yet, so failing leaves the
    // SOP exactly as it was.
    sync_dir(dir)?;
    Ok(())
}

/// Flush a directory's entries so a rename into or out of it survives a
/// machine crash, not just a process crash.
///
/// Unix exposes this as `fsync` on a handle to the directory itself. macOS
/// honors it for ordering but does not force the device cache to drain the way
/// `F_FULLFSYNC` would, so the guarantee there is the filesystem's rather than
/// the hardware's. Windows has no equivalent: a directory handle needs backup
/// semantics even to open, and `FlushFileBuffers` defines no durability
/// contract for one, so ordering stays the filesystem's to keep.
#[cfg(unix)]
fn sync_dir(dir: &Path) -> Result<()> {
    use anyhow::Context as _;

    std::fs::File::open(dir)
        .and_then(|handle| handle.sync_all())
        .with_context(|| format!("synchronizing directory {}", dir.display()))
}

#[cfg(not(unix))]
fn sync_dir(_dir: &Path) -> Result<()> {
    Ok(())
}

/// Project the live run state for `run_id` onto `sop`'s graph. Errors if
/// the run is unknown or the engine lock is poisoned.
pub fn run_overlay_for(
    sop: &Sop,
    engine: &Arc<Mutex<SopEngine>>,
    run_id: &str,
) -> Result<RunOverlay> {
    let guard = engine
        .lock()
        .map_err(|_| anyhow::Error::msg("SOP engine lock poisoned"))?;
    let run = guard
        .get_run(run_id)
        .ok_or_else(|| anyhow::Error::msg(format!("run '{run_id}' not found")))?;
    let graph = SopGraph::from_sop(sop);
    Ok(RunOverlay::project(&graph, run))
}

/// Enumerate every run the engine holds (active + retained terminal),
/// newest first, optionally scoped to one SOP. Errors only if the engine
/// lock is poisoned. This is the Runs surface's data source.
pub fn run_summaries_for(
    engine: &Arc<Mutex<SopEngine>>,
    sop_name: Option<&str>,
) -> Result<Vec<SopRunSummary>> {
    let guard = engine
        .lock()
        .map_err(|_| anyhow::Error::msg("SOP engine lock poisoned"))?;
    Ok(guard.run_summaries(sop_name))
}

/// Renumber steps to a contiguous 1..=N sequence (positional order wins)
/// and remap every internal reference: `routing.next`, `depends_on`,
/// switch `goto` targets, and `on_failure: goto`. References to steps that
/// no longer exist are dropped (`goto` falls back to `Fail`). No-op when
/// step numbers are ambiguous (duplicates), since a remap would guess.
/// Runs automatically inside `save_sop`.
pub fn normalize_step_numbers(sop: &mut Sop) {
    let mut seen = std::collections::HashSet::new();
    if !sop.steps.iter().all(|s| seen.insert(s.number)) {
        return;
    }
    let remap: std::collections::HashMap<u32, u32> = sop
        .steps
        .iter()
        .enumerate()
        .map(|(i, s)| {
            (
                s.number,
                u32::try_from(i).unwrap_or(u32::MAX).saturating_add(1),
            )
        })
        .collect();
    for (i, step) in sop.steps.iter_mut().enumerate() {
        step.number = u32::try_from(i).unwrap_or(u32::MAX).saturating_add(1);
        step.routing.next = step.routing.next.and_then(|n| remap.get(&n).copied());
        step.routing.depends_on = step
            .routing
            .depends_on
            .iter()
            .filter_map(|d| remap.get(d).copied())
            .collect();
        for rule in &mut step.routing.switch {
            rule.goto = rule.goto.and_then(|g| remap.get(&g).copied());
        }
        if let StepFailure::Goto { step: target } = step.on_failure {
            step.on_failure = remap
                .get(&target)
                .map(|s| StepFailure::Goto { step: *s })
                .unwrap_or(StepFailure::Fail);
        }
        for call in &mut step.calls {
            binding::remap_step_refs(&mut call.args, &remap);
        }
    }
}

/// Load SOPs from a specific directory. Each subdirectory may contain
/// `SOP.toml` (metadata + triggers) and `SOP.md` (procedure steps).
pub fn load_sops_from_directory(
    sops_dir: &Path,
    default_execution_mode: SopExecutionMode,
) -> Vec<Sop> {
    if !sops_dir.exists() {
        return Vec::new();
    }

    let mut sops = Vec::new();

    let Ok(entries) = std::fs::read_dir(sops_dir) else {
        return sops;
    };

    for entry in entries.flatten() {
        let path = entry.path();
        if !path.is_dir() {
            continue;
        }

        let toml_path = path.join("SOP.toml");
        if !toml_path.exists() {
            continue;
        }

        match load_sop(&path, default_execution_mode) {
            Ok(sop) => sops.push(sop),
            Err(e) => {
                ::zeroclaw_log::record!(
                    WARN,
                    ::zeroclaw_log::Event::new(module_path!(), ::zeroclaw_log::Action::Note)
                        .with_outcome(::zeroclaw_log::EventOutcome::Unknown)
                        .with_attrs(::serde_json::json!({"error": format!("{}", e)})),
                    &format!("Failed to load SOP from {}", path.display().to_string())
                );
            }
        }
    }

    sops.sort_by(|a, b| a.name.cmp(&b.name));
    sops
}

/// Load a single SOP from a directory containing SOP.toml and optionally SOP.md.
fn load_sop(sop_dir: &Path, default_execution_mode: SopExecutionMode) -> Result<Sop> {
    let toml_path = sop_dir.join("SOP.toml");
    let toml_content = std::fs::read_to_string(&toml_path)?;
    let manifest: SopManifest = toml::from_str(&toml_content)?;

    let md_path = sop_dir.join("SOP.md");
    let mut steps = if md_path.exists() {
        let md_content = std::fs::read_to_string(&md_path)?;
        parse_steps(&md_content)
    } else if !manifest.steps.is_empty() {
        normalize_manifest_steps(manifest.steps)
    } else {
        Vec::new()
    };

    for pos in &manifest.positions {
        if let Some(step) = steps.iter_mut().find(|s| s.number == pos.step) {
            step.pos = Some(types::StepPos { x: pos.x, y: pos.y });
        }
    }
    let SopMeta {
        name,
        description,
        version,
        priority,
        execution_mode,
        cooldown_secs,
        max_concurrent,
        deterministic,
        admission_policy,
        max_pending_approvals,
        agent,
    } = manifest.sop;

    // When deterministic=true, override execution_mode to Deterministic
    let effective_mode = if deterministic {
        SopExecutionMode::Deterministic
    } else {
        execution_mode.unwrap_or(default_execution_mode)
    };

    let sop = Sop {
        name,
        description,
        version,
        priority,
        execution_mode: effective_mode,
        triggers: manifest.triggers,
        steps,
        cooldown_secs,
        max_concurrent,
        location: Some(sop_dir.to_path_buf()),
        deterministic,
        admission_policy,
        max_pending_approvals,
        agent,
    };
    capability::SopCapabilityRegistry::with_builtins().validate_sop(&sop)?;
    Ok(sop)
}

fn normalize_manifest_steps(mut steps: Vec<SopStep>) -> Vec<SopStep> {
    for (idx, step) in steps.iter_mut().enumerate() {
        if step.number == 0 {
            step.number = u32::try_from(idx).unwrap_or(u32::MAX).saturating_add(1);
        }
        if step.title.is_empty() {
            step.title = step
                .capability
                .clone()
                .unwrap_or_else(|| step.kind.to_string());
        }
    }
    steps
}

// ── Markdown step parser ────────────────────────────────────────

/// A parser behavior or `SOP.md` bullet understood by [`parse_steps`].
///
/// The catalog is the source for the generated syntax reference. Bullet
/// prefixes are also consumed by the parser below, so adding a supported
/// bullet requires updating one source-side entry rather than a separate
/// hand-maintained documentation list.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum SopStepSyntaxKey {
    StepsSection,
    NumberedItem,
    BoldTitle,
    Tools,
    AllowTools,
    DenyTools,
    RequiresConfirmation,
    Kind,
    Capability,
    With,
    Input,
    Output,
    When,
    Next,
    Terminal,
    DependsOn,
    Switch,
    OnFailure,
    Mode,
    Agent,
    Call,
    Prompt,
    Policy,
    Edit,
    ContinuationBody,
}

impl SopStepSyntaxKey {
    fn bullet_prefixes(self) -> &'static [&'static str] {
        match self {
            Self::Tools => &["tools:"],
            Self::AllowTools => &["allow-tools:", "allow_tools:"],
            Self::DenyTools => &["deny-tools:", "deny_tools:"],
            Self::RequiresConfirmation => &["requires_confirmation:"],
            Self::Kind => &["kind:"],
            Self::Capability => &["capability:"],
            Self::With => &["with:"],
            Self::Input => &["input:"],
            Self::Output => &["output:"],
            Self::When => &["when:"],
            Self::Next => &["next:"],
            Self::Terminal => &["terminal:"],
            Self::DependsOn => &["depends_on:", "depends-on:"],
            Self::Switch => &["switch:"],
            Self::OnFailure => &["on_failure:", "on-failure:"],
            Self::Mode => &["mode:"],
            Self::Agent => &["agent:"],
            Self::Call => &["call:"],
            Self::Prompt => &["prompt:"],
            Self::Policy => &["policy:"],
            Self::Edit => &["edit:"],
            Self::StepsSection | Self::NumberedItem | Self::BoldTitle | Self::ContinuationBody => {
                &[]
            }
        }
    }
}

/// One source-owned entry in the generated SOP syntax reference.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub struct SopStepSyntaxSpec {
    /// The parser behavior or bullet this entry documents.
    pub key: SopStepSyntaxKey,
    /// Human-readable explanation rendered into `docs/book/src/sop/syntax.md`.
    pub description: &'static str,
}

/// Parser behavior and bullet catalog used by the SOP syntax reference.
pub const SOP_STEP_SYNTAX_CATALOG: &[SopStepSyntaxSpec] = &[
    SopStepSyntaxSpec {
        key: SopStepSyntaxKey::StepsSection,
        description: "The `## Steps` section is parsed until the next level-two heading.",
    },
    SopStepSyntaxSpec {
        key: SopStepSyntaxKey::NumberedItem,
        description: "Numbered items (`1.`, `2.`, ...) define step order.",
    },
    SopStepSyntaxSpec {
        key: SopStepSyntaxKey::BoldTitle,
        description: "Leading bold text (`**Title**`) becomes the step title; the remaining text becomes its body.",
    },
    SopStepSyntaxSpec {
        key: SopStepSyntaxKey::Tools,
        description: "`- tools:` maps to `suggested_tools` and provides advisory tool names for the step.",
    },
    SopStepSyntaxSpec {
        key: SopStepSyntaxKey::AllowTools,
        description: "`- allow-tools:` (or `- allow_tools:`) defines an explicit per-step tool allow-list.",
    },
    SopStepSyntaxSpec {
        key: SopStepSyntaxKey::DenyTools,
        description: "`- deny-tools:` (or `- deny_tools:`) defines an explicit per-step tool deny-list.",
    },
    SopStepSyntaxSpec {
        key: SopStepSyntaxKey::RequiresConfirmation,
        description: "`- requires_confirmation: true` enforces approval for that step.",
    },
    SopStepSyntaxSpec {
        key: SopStepSyntaxKey::Kind,
        description: "`- kind:` accepts `execute` (default), `checkpoint`/`approval`, or `capability`; a checkpoint pauses deterministic execution, while `requires_confirmation: true` requires approval in any execution mode.",
    },
    SopStepSyntaxSpec {
        key: SopStepSyntaxKey::Capability,
        description: "`- capability:` names the deterministic capability used by a `kind: capability` step.",
    },
    SopStepSyntaxSpec {
        key: SopStepSyntaxKey::With,
        description: "`- with:` supplies the structured input for a capability step.",
    },
    SopStepSyntaxSpec {
        key: SopStepSyntaxKey::Input,
        description: "`- input:` attaches a JSON Schema-like input contract to the step boundary.",
    },
    SopStepSyntaxSpec {
        key: SopStepSyntaxKey::Output,
        description: "`- output:` attaches a JSON Schema-like output contract to the step boundary.",
    },
    SopStepSyntaxSpec {
        key: SopStepSyntaxKey::When,
        description: "`- when:` is evaluated against accumulated completed-step outputs after the current step finishes. A false guard bypasses `switch` and explicit `next`, taking the linear successor or completing when the step is terminal or has no successor. With a true or absent guard, a non-empty `switch` takes precedence over `next`; without a switch, an explicit `next` is used before terminal or linear routing.",
    },
    SopStepSyntaxSpec {
        key: SopStepSyntaxKey::Next,
        description: "`- next:` routes to an explicit successor only when the top-level `when` allows routing and no `switch` ports are declared; ineligible routed steps are marked `skipped` and leave the run `pending` instead of dispatching.",
    },
    SopStepSyntaxSpec {
        key: SopStepSyntaxKey::Terminal,
        description: "`- terminal: true` completes the run instead of advancing to another step; the final step also completes when it has no linear successor.",
    },
    SopStepSyntaxSpec {
        key: SopStepSyntaxKey::DependsOn,
        description: "`- depends_on:` (or `- depends-on:`) lists prerequisite steps for a non-linear run.",
    },
    SopStepSyntaxSpec {
        key: SopStepSyntaxKey::Switch,
        description: "`- switch:` defines ordered `name>condition>step` ports for multi-branch routing. With a true or absent top-level `when`, the first matching port wins; an unmatched switch completes the run, and `next` plus the linear successor are ignored. A false top-level `when` bypasses switch evaluation.",
    },
    SopStepSyntaxSpec {
        key: SopStepSyntaxKey::OnFailure,
        description: "`- on_failure:` (or `- on-failure:`) accepts `fail`, `retry:<count>`, or `goto:<step>` and is enforced for reported step failures and output-schema failures.",
    },
    SopStepSyntaxSpec {
        key: SopStepSyntaxKey::Mode,
        description: "`- mode:` overrides the SOP execution mode for that step.",
    },
    SopStepSyntaxSpec {
        key: SopStepSyntaxKey::Agent,
        description: "`- agent:` overrides the parent agent alias for that step.",
    },
    SopStepSyntaxSpec {
        key: SopStepSyntaxKey::Call,
        description: "`- call:` adds a JSON planned tool call to the step when the value parses as a planned call.",
    },
    SopStepSyntaxSpec {
        key: SopStepSyntaxKey::Prompt,
        description: "`- prompt:` sets the approval-gate notice template.",
    },
    SopStepSyntaxSpec {
        key: SopStepSyntaxKey::Policy,
        description: "`- policy:` names an approval-broker policy in `[sop.approval].policies`; the policy gates approval through required-group membership and quorum. An absent policy fails closed rather than clearing on a single approval, while omission leaves the gate unpoliced.",
    },
    SopStepSyntaxSpec {
        key: SopStepSyntaxKey::Edit,
        description: "`- edit:` opts a checkpoint into editing the named field before resume.",
    },
    SopStepSyntaxSpec {
        key: SopStepSyntaxKey::ContinuationBody,
        description: "Unrecognized sub-bullets and other non-empty continuation lines are appended to the step body.",
    },
];

fn parse_step_bullet(bullet: &str) -> Option<(SopStepSyntaxKey, &str)> {
    SOP_STEP_SYNTAX_CATALOG.iter().find_map(|spec| {
        spec.key
            .bullet_prefixes()
            .iter()
            .find_map(|prefix| bullet.strip_prefix(prefix).map(|value| (spec.key, value)))
    })
}

/// Parse procedure steps from SOP.md content.
/// Expects a `## Steps` heading followed by numbered items (`1.`, `2.`, …).
/// Each item's first bold text (`**...**`) is the step title; the rest is body.
/// Sub-bullets parse execution hints and dark per-step contract metadata.
pub fn parse_steps(md: &str) -> Vec<SopStep> {
    let mut steps = Vec::new();
    let mut in_steps_section = false;
    let mut current = StepParseState::default();

    for line in md.lines() {
        let trimmed = line.trim();

        // Detect ## Steps heading
        if trimmed.starts_with("## ") {
            if trimmed.eq_ignore_ascii_case("## steps") || trimmed.eq_ignore_ascii_case("## Steps")
            {
                in_steps_section = true;
                continue;
            }
            // Any other ## heading ends the steps section
            if in_steps_section {
                // Flush pending step
                current.flush_into(&mut steps);
                in_steps_section = false;
            }
            continue;
        }

        if !in_steps_section {
            continue;
        }

        // Check for numbered item: `1.`, `2.`, etc.
        if let Some(rest) = parse_numbered_item(trimmed) {
            // Flush previous step
            current.flush_into(&mut steps);

            let step_num = u32::try_from(steps.len())
                .unwrap_or(u32::MAX)
                .saturating_add(1);
            current.reset_for_step(step_num);

            // Extract title from bold text: **title** — body
            if let Some((title, body)) = extract_bold_title(rest) {
                current.title = title;
                current.body = body;
            } else {
                current.title = rest.to_string();
            }
            continue;
        }

        // Sub-bullet parsing (only when inside a step)
        if current.number.is_some() && trimmed.starts_with("- ") {
            let bullet = trimmed.trim_start_matches("- ").trim();
            if let Some((key, val)) = parse_step_bullet(bullet) {
                match key {
                    SopStepSyntaxKey::Tools => {
                        current.tools = parse_csv_list(val);
                    }
                    SopStepSyntaxKey::AllowTools => {
                        ensure_scope(&mut current.scope).allow = Some(parse_csv_list(val));
                    }
                    SopStepSyntaxKey::DenyTools => {
                        ensure_scope(&mut current.scope).deny = parse_csv_list(val);
                    }
                    SopStepSyntaxKey::RequiresConfirmation => {
                        current.requires_confirmation = val.trim().eq_ignore_ascii_case("true");
                    }
                    SopStepSyntaxKey::Kind => {
                        current.kind = parse_step_kind(val);
                    }
                    SopStepSyntaxKey::Capability => {
                        current.capability = Some(val.trim().to_string());
                    }
                    SopStepSyntaxKey::With => {
                        current.capability_input = Some(parse_value_fragment(val.trim()));
                    }
                    SopStepSyntaxKey::Input => {
                        ensure_schema(&mut current.schema).input =
                            Some(parse_value_fragment(val.trim()));
                    }
                    SopStepSyntaxKey::Output => {
                        ensure_schema(&mut current.schema).output =
                            Some(parse_value_fragment(val.trim()));
                    }
                    SopStepSyntaxKey::When => {
                        let val = val.trim();
                        if !val.is_empty() {
                            current.routing.when = Some(val.to_string());
                        }
                    }
                    SopStepSyntaxKey::Next => {
                        current.routing.next = val.trim().parse::<u32>().ok();
                    }
                    SopStepSyntaxKey::Terminal => {
                        current.routing.terminal = val.trim().eq_ignore_ascii_case("true");
                    }
                    SopStepSyntaxKey::DependsOn => {
                        current.routing.depends_on = parse_u32_list(val);
                    }
                    SopStepSyntaxKey::Switch => {
                        current.routing.switch = parse_switch_rules(val);
                    }
                    SopStepSyntaxKey::OnFailure => {
                        current.on_failure = parse_step_failure(val);
                    }
                    SopStepSyntaxKey::Mode => {
                        current.mode = Some(parse_execution_mode(val));
                    }
                    SopStepSyntaxKey::Agent => {
                        let trimmed_val = val.trim();
                        current.agent = (!trimmed_val.is_empty()).then(|| trimmed_val.to_string());
                    }
                    SopStepSyntaxKey::Call => {
                        if let Ok(call) = serde_json::from_str::<PlannedToolCall>(val.trim()) {
                            current.calls.push(call);
                        }
                    }
                    SopStepSyntaxKey::Prompt => {
                        let val = val.trim();
                        if !val.is_empty() {
                            current.gate_prompt = Some(val.to_string());
                        }
                    }
                    SopStepSyntaxKey::Policy => {
                        let val = val.trim();
                        current.policy = if val.is_empty() {
                            None
                        } else {
                            Some(val.to_string())
                        };
                    }
                    SopStepSyntaxKey::Edit => {
                        // Editable-field opt-in for a checkpoint gate: the named field of
                        // the piped value an approver may amend before the run resumes.
                        let val = val.trim();
                        current.edit = if val.is_empty() {
                            None
                        } else {
                            Some(val.to_string())
                        };
                    }
                    SopStepSyntaxKey::StepsSection
                    | SopStepSyntaxKey::NumberedItem
                    | SopStepSyntaxKey::BoldTitle
                    | SopStepSyntaxKey::ContinuationBody => {
                        unreachable!("non-bullet SOP syntax key returned by parse_step_bullet")
                    }
                }
            } else {
                // Continuation body line
                if !current.body.is_empty() {
                    current.body.push('\n');
                }
                current.body.push_str(trimmed);
            }
            continue;
        }

        // Continuation line for step body
        if current.number.is_some() && !trimmed.is_empty() {
            if !current.body.is_empty() {
                current.body.push('\n');
            }
            current.body.push_str(trimmed);
        }
    }

    // Flush final step
    current.flush_into(&mut steps);

    steps
}

#[derive(Default)]
struct StepParseState {
    number: Option<u32>,
    title: String,
    body: String,
    tools: Vec<String>,
    requires_confirmation: bool,
    kind: SopStepKind,
    capability: Option<String>,
    capability_input: Option<serde_json::Value>,
    schema: Option<StepSchema>,
    scope: Option<StepToolScope>,
    routing: StepRouting,
    on_failure: StepFailure,
    mode: Option<SopExecutionMode>,
    calls: Vec<PlannedToolCall>,
    agent: Option<String>,
    policy: Option<String>,
    gate_prompt: Option<String>,
    edit: Option<String>,
}

impl StepParseState {
    fn reset_for_step(&mut self, number: u32) {
        *self = Self {
            number: Some(number),
            ..Self::default()
        };
    }

    fn flush_into(&mut self, steps: &mut Vec<SopStep>) {
        let Some(n) = self.number.take() else {
            return;
        };
        steps.push(SopStep {
            number: n,
            title: std::mem::take(&mut self.title),
            body: self.body.trim().to_string(),
            suggested_tools: std::mem::take(&mut self.tools),
            requires_confirmation: self.requires_confirmation,
            kind: self.kind,
            capability: self.capability.take(),
            capability_input: self.capability_input.take(),
            schema: self.schema.take(),
            scope: self.scope.take(),
            routing: std::mem::take(&mut self.routing),
            on_failure: std::mem::take(&mut self.on_failure),
            mode: self.mode.take(),
            calls: std::mem::take(&mut self.calls),
            pos: None,
            agent: self.agent.take(),
            policy: self.policy.take(),
            gate_prompt: self.gate_prompt.take(),
            edit: self.edit.take(),
        });
        *self = Self::default();
    }
}

fn parse_csv_list(value: &str) -> Vec<String> {
    value
        .split(',')
        .map(|item| item.trim().to_string())
        .filter(|item| !item.is_empty())
        .collect()
}

fn parse_u32_list(value: &str) -> Vec<u32> {
    value
        .split(',')
        .filter_map(|item| item.trim().parse::<u32>().ok())
        .collect()
}

fn parse_switch_rules(value: &str) -> Vec<SwitchRule> {
    value
        .split(';')
        .filter_map(|seg| {
            let mut parts = seg.splitn(3, '>');
            let name = parts.next().unwrap_or("").trim().to_string();
            if name.is_empty() {
                return None;
            }
            let when = parts.next().unwrap_or("").trim();
            let goto = parts.next().unwrap_or("").trim();
            Some(SwitchRule {
                name,
                when: (!when.is_empty()).then(|| when.to_string()),
                goto: goto.parse::<u32>().ok(),
            })
        })
        .collect()
}

fn parse_step_kind(value: &str) -> SopStepKind {
    match value.trim().to_ascii_lowercase().as_str() {
        "checkpoint" | "approval" => SopStepKind::Checkpoint,
        "capability" => SopStepKind::Capability,
        _ => SopStepKind::Execute,
    }
}

fn parse_value_fragment(value: &str) -> serde_json::Value {
    if let Ok(json) = serde_json::from_str(value) {
        return json;
    }
    let wrapped = format!("value = {value}");
    if let Ok(toml_value) = toml::from_str::<toml::Value>(&wrapped)
        && let Some(value) = toml_value.get("value")
        && let Ok(json) = serde_json::to_value(value)
    {
        return json;
    }
    serde_json::Value::String(value.into())
}

fn parse_step_failure(value: &str) -> StepFailure {
    let value = value.trim();
    if value.eq_ignore_ascii_case("fail") {
        return StepFailure::Fail;
    }
    if let Some(max) = value
        .strip_prefix("retry:")
        .or_else(|| value.strip_prefix("retry "))
        .and_then(|raw| raw.trim().parse::<u32>().ok())
    {
        return StepFailure::Retry { max };
    }
    if let Some(step) = value
        .strip_prefix("goto:")
        .or_else(|| value.strip_prefix("goto "))
        .and_then(|raw| raw.trim().parse::<u32>().ok())
    {
        return StepFailure::Goto { step };
    }
    StepFailure::Fail
}

fn ensure_schema(schema: &mut Option<StepSchema>) -> &mut StepSchema {
    schema.get_or_insert(StepSchema {
        input: None,
        output: None,
    })
}

fn ensure_scope(scope: &mut Option<StepToolScope>) -> &mut StepToolScope {
    scope.get_or_insert_with(StepToolScope::default)
}

/// Try to parse `N. rest` from a line, returning `rest` if successful.
fn parse_numbered_item(line: &str) -> Option<&str> {
    let dot_pos = line.find(". ")?;
    let prefix = &line[..dot_pos];
    if prefix.chars().all(|c| c.is_ascii_digit()) && !prefix.is_empty() {
        Some(line[dot_pos + 2..].trim())
    } else {
        None
    }
}

/// Extract `**title**` from the beginning of text, returning (title, rest).
pub fn extract_bold_title(text: &str) -> Option<(String, String)> {
    let start = text.find("**")?;
    let after_start = start + 2;
    let end = text[after_start..].find("**")?;
    let title = text[after_start..after_start + end].to_string();

    // Rest is everything after the closing ** and any separator (— or -)
    let rest_start = after_start + end + 2;
    let rest = text[rest_start..].trim();
    let rest = rest
        .strip_prefix("—")
        .or_else(|| rest.strip_prefix("–"))
        .or_else(|| rest.strip_prefix("-"))
        .unwrap_or(rest)
        .trim();

    Some((title, rest.to_string()))
}

fn render_step_failure(failure: &StepFailure) -> String {
    match failure {
        StepFailure::Fail => "fail".to_string(),
        StepFailure::Retry { max } => format!("retry: {max}"),
        StepFailure::Goto { step } => format!("goto: {step}"),
    }
}

fn render_step_bullets(step: &SopStep) -> Vec<String> {
    let mut bullets = Vec::new();

    if !step.suggested_tools.is_empty() {
        bullets.push(format!("tools: {}", step.suggested_tools.join(", ")));
    }
    if let Some(scope) = &step.scope {
        if let Some(allow) = &scope.allow {
            bullets.push(format!("allow-tools: {}", allow.join(", ")));
        }
        if !scope.deny.is_empty() {
            bullets.push(format!("deny-tools: {}", scope.deny.join(", ")));
        }
    }
    if step.requires_confirmation {
        bullets.push("requires_confirmation: true".to_string());
    }
    if step.kind == SopStepKind::Checkpoint {
        bullets.push("kind: checkpoint".to_string());
    }
    if let Some(schema) = &step.schema {
        if let Some(input) = &schema.input {
            bullets.push(format!("input: {input}"));
        }
        if let Some(output) = &schema.output {
            bullets.push(format!("output: {output}"));
        }
    }
    if let Some(when) = &step.routing.when {
        bullets.push(format!("when: {when}"));
    }
    if let Some(next) = step.routing.next {
        bullets.push(format!("next: {next}"));
    }
    if step.routing.terminal {
        bullets.push("terminal: true".to_string());
    }
    if !step.routing.depends_on.is_empty() {
        let csv = step
            .routing
            .depends_on
            .iter()
            .map(u32::to_string)
            .collect::<Vec<_>>()
            .join(", ");
        bullets.push(format!("depends_on: {csv}"));
    }
    if !step.routing.switch.is_empty() {
        let rendered = step
            .routing
            .switch
            .iter()
            .map(|rule| {
                let when = rule.when.as_deref().unwrap_or("");
                let goto = rule.goto.map(|g| g.to_string()).unwrap_or_default();
                format!("{}>{}>{}", rule.name, when, goto)
            })
            .collect::<Vec<_>>()
            .join("; ");
        bullets.push(format!("switch: {rendered}"));
    }
    if !step.on_failure.is_fail() {
        bullets.push(format!(
            "on_failure: {}",
            render_step_failure(&step.on_failure)
        ));
    }
    if let Some(mode) = step.mode {
        bullets.push(format!("mode: {mode}"));
    }
    if let Some(agent) = &step.agent {
        bullets.push(format!("agent: {agent}"));
    }
    for call in &step.calls {
        if let Ok(rendered) = serde_json::to_string(call) {
            bullets.push(format!("call: {rendered}"));
        }
    }

    bullets
}

/// Render steps back to `SOP.md` markdown, the inverse of `parse_steps`.
/// Every contract field (tools, scope, schema, routing, failure policy,
/// mode) becomes a sub-bullet, so render -> parse is lossless.
pub fn render_steps(steps: &[SopStep]) -> String {
    let mut out = String::from("## Steps\n\n");
    for step in steps {
        if step.body.is_empty() {
            out.push_str(&format!("{}. **{}**\n", step.number, step.title));
        } else {
            out.push_str(&format!(
                "{}. **{}** - {}\n",
                step.number, step.title, step.body
            ));
        }
        for bullet in render_step_bullets(step) {
            out.push_str(&format!("   - {bullet}\n"));
        }
    }
    out
}

/// Persist an SOP to `<sops_dir>/<name>/` as `SOP.toml` + `SOP.md`.
/// Normalizes step numbers first, then rejects the write entirely if
/// strict validation finds blocking problems; nothing touches disk on
/// failure.
pub fn save_sop(sops_dir: &Path, sop: &Sop) -> Result<()> {
    let _lock = lock_sops_dir(sops_dir)?;
    save_sop_unlocked(sops_dir, sop)
}

/// `save_sop`'s body without the root lock, for callers that already hold it.
fn save_sop_unlocked(sops_dir: &Path, sop: &Sop) -> Result<()> {
    let mut sop = sop.clone();
    normalize_step_numbers(&mut sop);
    let sop = &sop;
    let validation = validate_sop_strict(sop);
    if !validation.is_ok() {
        anyhow::bail!("SOP rejected: {}", validation.blocking.join("; "));
    }

    let sop_dir = resolve_sop_dir(sops_dir, &sop.name)?;
    std::fs::create_dir_all(&sop_dir)?;

    let manifest = SopManifest::from_sop(sop);
    let toml_content = toml::to_string_pretty(&manifest)?;
    std::fs::write(sop_dir.join("SOP.toml"), toml_content)?;
    std::fs::write(sop_dir.join("SOP.md"), render_steps(&sop.steps))?;

    Ok(())
}

// ── Validation ──────────────────────────────────────────────────

/// Validate a loaded SOP and return a list of warnings.
pub fn validate_sop(sop: &Sop) -> Vec<String> {
    let mut warnings = Vec::new();

    if sop.name.is_empty() {
        warnings.push("SOP name is empty".into());
    }
    if sop.description.is_empty() {
        warnings.push("SOP description is empty".into());
    }
    if sop.triggers.is_empty() {
        warnings.push("SOP has no triggers defined".into());
    }
    if sop.steps.is_empty() {
        warnings.push("SOP has no steps (missing or empty SOP.md)".into());
    }

    // Check step numbering continuity
    for (i, step) in sop.steps.iter().enumerate() {
        let expected = u32::try_from(i).unwrap_or(u32::MAX).saturating_add(1);
        if step.number != expected {
            warnings.push(format!(
                "Step numbering gap: expected {expected}, got {}",
                step.number
            ));
        }
        if step.title.is_empty() {
            warnings.push(format!("Step {} has an empty title", step.number));
        }
    }

    warnings
}

/// Validate planned-call binding references across the SOP. Blocking:
/// malformed binding syntax, `steps.N` naming an unknown step, a step
/// referencing itself or a later step, and `calls.K` at or past the
/// referencing call's own index. Warning: a `steps.N` reference to a step
/// that declares no output schema and no planned calls (nothing known to
/// bind against).
fn validate_planned_call_bindings(
    sop: &Sop,
    blocking: &mut Vec<String>,
    warnings: &mut Vec<String>,
) {
    let known: std::collections::HashMap<u32, &SopStep> =
        sop.steps.iter().map(|s| (s.number, s)).collect();
    for step in &sop.steps {
        for (call_idx, call) in step.calls.iter().enumerate() {
            let label = format!("Step {} call {call_idx} ({})", step.number, call.tool);
            for extracted in binding::extract_bindings(&call.args) {
                match extracted {
                    binding::ExtractedBinding::Malformed { raw, reason } => {
                        blocking.push(format!("{label}: malformed binding '{raw}': {reason}"));
                    }
                    binding::ExtractedBinding::Valid(bref) => match bref.scope {
                        binding::BindingScope::Step(n) => match known.get(&n) {
                            None => blocking.push(format!(
                                "{label}: binding '{}' references unknown step {n}",
                                bref.raw
                            )),
                            Some(_) if n >= step.number => blocking.push(format!(
                                "{label}: binding '{}' references step {n}, which does not run before step {}",
                                bref.raw, step.number
                            )),
                            Some(target)
                                if target.calls.is_empty()
                                    && target
                                        .schema
                                        .as_ref()
                                        .is_none_or(|s| s.output.is_none()) =>
                            {
                                warnings.push(format!(
                                    "{label}: binding '{}' targets step {n}, which declares no output schema or planned calls",
                                    bref.raw
                                ));
                            }
                            Some(_) => {}
                        },
                        binding::BindingScope::Call(k) => {
                            if k as usize >= call_idx {
                                blocking.push(format!(
                                    "{label}: binding '{}' references call {k}, which does not run before call {call_idx}",
                                    bref.raw
                                ));
                            }
                        }
                    },
                }
            }
        }
    }
}

/// Result of `validate_sop_strict`: `blocking` problems reject a save,
/// `warnings` surface in editors but do not block.
#[derive(Debug, Clone, Default, PartialEq, Eq)]
pub struct SopValidation {
    pub blocking: Vec<String>,
    pub warnings: Vec<String>,
}

impl SopValidation {
    pub fn is_ok(&self) -> bool {
        self.blocking.is_empty()
    }
}

/// Authoring-gate validation: empty name, empty step titles, and duplicate
/// step numbers block, as do graph projection errors (dangling `next` /
/// `depends_on` / switch / goto targets, unsatisfiable required inputs).
/// Graph warnings and the legacy `validate_sop` findings are advisory.
pub fn validate_sop_strict(sop: &Sop) -> SopValidation {
    let mut blocking = Vec::new();

    if sop.name.trim().is_empty() {
        blocking.push("SOP name is empty".into());
    }

    let mut seen = std::collections::HashSet::new();
    for step in &sop.steps {
        if step.title.trim().is_empty() {
            blocking.push(format!("Step {} has an empty title", step.number));
        }
        if !seen.insert(step.number) {
            blocking.push(format!("Duplicate step number {}", step.number));
        }
    }

    let mut warnings = Vec::new();
    validate_planned_call_bindings(sop, &mut blocking, &mut warnings);

    let graph = SopGraph::from_sop(sop);
    for diag in &graph.diagnostics {
        match diag.severity {
            GraphSeverity::Error => {
                blocking.push(format!("Step {}: {}", diag.step, diag.message));
            }
            GraphSeverity::Warning => {
                warnings.push(format!("Step {}: {}", diag.step, diag.message));
            }
        }
    }

    warnings.extend(validate_sop(sop));

    SopValidation { blocking, warnings }
}

#[cfg(test)]
mod tests {
    use serde_json::json;

    use super::*;

    #[test]
    fn resolve_sops_dir_joins_relative_config_value_to_install_root() {
        // The documented `shared/sops` must resolve to `<install>/shared/sops`,
        // not double the `shared` segment. Regression guard for a config that
        // carries `sops_dir = "shared/sops"`.
        let install_root = Path::new("/test/install");
        let resolved = resolve_sops_dir(install_root, Some("shared/sops"));
        assert_eq!(resolved, install_root.join("shared").join("sops"));
    }

    #[test]
    fn resolve_sops_dir_joins_bare_relative_value_under_install_root() {
        let install_root = Path::new("/test/install");
        let resolved = resolve_sops_dir(install_root, Some("custom-sops"));
        assert_eq!(resolved, install_root.join("custom-sops"));
    }

    #[test]
    fn resolve_sops_dir_keeps_absolute_config_value_as_is() {
        let install_root = Path::new("/test/install");
        let resolved = resolve_sops_dir(install_root, Some("/srv/shared/sops"));
        assert_eq!(resolved, Path::new("/srv/shared/sops"));
    }

    #[test]
    fn resolve_sops_dir_falls_back_to_shared_sops_when_unset() {
        let install_root = Path::new("/test/install");
        let canonical = install_root.join("shared").join("sops");
        assert_eq!(resolve_sops_dir(install_root, None), canonical);
        assert_eq!(resolve_sops_dir(install_root, Some("")), canonical);
        // Whitespace-only is the disabled sentinel `runtime_enabled()` also
        // rejects; the scan root must fall back, not join a garbage segment.
        assert_eq!(resolve_sops_dir(install_root, Some("   ")), canonical);
    }

    // Boundary regression: for the documented `sops_dir = "shared/sops"`, the
    // authoring write path (`create_sop_typed`, used by web/RPC), the runtime/CLI
    // load path (`load_sops`), and the delete path (`delete_sop_typed`) must all
    // resolve against the install root and converge on `<install>/shared/sops`.
    // This is the documented shared-workspace configuration; before the
    // install-root base it doubled to `<install>/shared/shared/sops` and authored
    // SOPs were invisible to loading.
    #[test]
    fn shared_sops_config_converges_across_author_load_and_delete() {
        let tmp = tempfile::tempdir().unwrap();
        let install_root = tmp.path();
        let config_dir = Some("shared/sops");
        let canonical = install_root.join("shared").join("sops");

        // The authoring surface resolves the write directory the same way the
        // loader does — one resolver, one root.
        let author_dir = resolve_sops_dir(install_root, config_dir);
        assert_eq!(
            author_dir, canonical,
            "author path must target <install>/shared/sops"
        );

        // Author a SOP (web/RPC `handle_sop_create` -> `create_sop_typed`).
        let sop = authoring_sop(vec![titled_step(1, "Do the thing")]);
        create_sop_typed(&author_dir, &sop).expect("author create should succeed");
        assert!(
            canonical.join("authoring").join("SOP.toml").exists(),
            "authored SOP.toml must land under <install>/shared/sops"
        );
        assert!(
            !install_root
                .join("shared")
                .join("shared")
                .join("sops")
                .exists(),
            "resolution must not double the shared segment"
        );

        // The runtime/CLI loader sees the authored SOP through the same base.
        let loaded = load_sops(install_root, config_dir, SopExecutionMode::Supervised);
        assert_eq!(loaded.len(), 1, "loader must see exactly the authored SOP");
        assert_eq!(loaded[0].name, "authoring");

        // Delete resolves to the same directory and removes it.
        delete_sop_typed(&author_dir, "authoring").expect("delete should succeed");
        assert!(
            !canonical.join("authoring").exists(),
            "delete must remove the SOP from <install>/shared/sops"
        );
        assert!(
            load_sops(install_root, config_dir, SopExecutionMode::Supervised).is_empty(),
            "loader must see the SOP gone after delete"
        );
    }

    #[test]
    fn absolute_sops_dir_converges_across_author_and_load() {
        let tmp = tempfile::tempdir().unwrap();
        let install_root = tmp.path().join("install");
        let abs_sops = tmp.path().join("elsewhere").join("sops");
        std::fs::create_dir_all(&install_root).unwrap();
        let config_dir = Some(abs_sops.to_string_lossy());
        let config_dir = config_dir.as_deref();

        // An absolute value ignores the install root entirely.
        assert_eq!(resolve_sops_dir(&install_root, config_dir), abs_sops);

        let sop = authoring_sop(vec![titled_step(1, "Do the thing")]);
        create_sop_typed(&abs_sops, &sop).expect("author create should succeed");
        let loaded = load_sops(&install_root, config_dir, SopExecutionMode::Supervised);
        assert_eq!(loaded.len(), 1, "absolute-path SOP must load");
        assert_eq!(loaded[0].name, "authoring");
    }

    fn authoring_sop(steps: Vec<SopStep>) -> Sop {
        Sop {
            name: "authoring".into(),
            description: "test".into(),
            version: "0.1.0".into(),
            priority: SopPriority::Normal,
            execution_mode: SopExecutionMode::Auto,
            triggers: vec![SopTrigger::Manual],
            steps,
            cooldown_secs: 0,
            max_concurrent: 1,
            location: None,
            deterministic: false,
            admission_policy: Default::default(),
            max_pending_approvals: 0,
            agent: None,
        }
    }

    fn titled_step(number: u32, title: &str) -> SopStep {
        SopStep {
            number,
            title: title.to_string(),
            ..SopStep::default()
        }
    }

    #[test]
    fn normalize_step_numbers_remaps_all_references() {
        let mut s3 = titled_step(30, "c");
        s3.routing.next = Some(10);
        s3.routing.depends_on = vec![20, 99];
        s3.routing.switch = vec![SwitchRule {
            name: "port".into(),
            when: None,
            goto: Some(20),
        }];
        s3.on_failure = StepFailure::Goto { step: 10 };
        let mut sop = authoring_sop(vec![titled_step(10, "a"), titled_step(20, "b"), s3]);

        normalize_step_numbers(&mut sop);

        let numbers: Vec<u32> = sop.steps.iter().map(|s| s.number).collect();
        assert_eq!(numbers, vec![1, 2, 3]);
        assert_eq!(sop.steps[2].routing.next, Some(1));
        assert_eq!(
            sop.steps[2].routing.depends_on,
            vec![2],
            "dangling ref 99 dropped"
        );
        assert_eq!(sop.steps[2].routing.switch[0].goto, Some(2));
        assert_eq!(sop.steps[2].on_failure, StepFailure::Goto { step: 1 });
    }

    #[test]
    fn normalize_step_numbers_refuses_duplicate_numbers() {
        let mut sop = authoring_sop(vec![titled_step(1, "a"), titled_step(1, "b")]);
        let before = sop.steps.clone();
        normalize_step_numbers(&mut sop);
        assert_eq!(
            sop.steps, before,
            "ambiguous numbering must not be remapped"
        );
    }

    #[test]
    fn normalize_dangling_failure_goto_falls_back_to_fail() {
        let mut s1 = titled_step(1, "a");
        s1.on_failure = StepFailure::Goto { step: 99 };
        let mut sop = authoring_sop(vec![s1]);
        normalize_step_numbers(&mut sop);
        assert_eq!(sop.steps[0].on_failure, StepFailure::Fail);
    }

    #[test]
    fn render_parse_roundtrip_preserves_full_step_contract() {
        let mut step = titled_step(1, "Collect");
        step.body = "Gather context.".into();
        step.suggested_tools = vec!["read_file".into(), "shell".into()];
        step.requires_confirmation = true;
        step.kind = SopStepKind::Checkpoint;
        step.schema = Some(StepSchema {
            input: Some(json!({"type": "object", "required": ["ticket"]})),
            output: Some(json!({"type": "boolean"})),
        });
        step.scope = Some(crate::sop::scope::StepToolScope {
            allow: Some(vec!["fs".into()]),
            deny: vec!["shell".into()],
        });
        step.routing = StepRouting {
            when: Some("$.steps.1.ok == true".into()),
            next: Some(2),
            terminal: false,
            depends_on: vec![2],
            switch: vec![
                SwitchRule {
                    name: "pr".into(),
                    when: Some("$.event".into()),
                    goto: Some(2),
                },
                SwitchRule {
                    name: "catch_all".into(),
                    when: None,
                    goto: None,
                },
            ],
        };
        step.on_failure = StepFailure::Retry { max: 2 };
        step.mode = Some(SopExecutionMode::Auto);

        let mut terminal = titled_step(2, "Done");
        terminal.routing.terminal = true;

        let rendered = render_steps(&[step.clone(), terminal.clone()]);
        let parsed = parse_steps(&rendered);

        assert_eq!(parsed, vec![step, terminal]);
    }

    #[test]
    fn save_sop_rejects_blocking_validation_and_writes_nothing() {
        let dir = tempfile::tempdir().unwrap();
        let sop = authoring_sop(vec![titled_step(1, "")]);
        let err = save_sop(dir.path(), &sop).unwrap_err();
        assert!(err.to_string().contains("SOP rejected"));
        assert!(!dir.path().join("authoring").exists());
    }

    #[test]
    fn save_then_load_roundtrips_via_disk() {
        let dir = tempfile::tempdir().unwrap();
        let mut s1 = titled_step(1, "First");
        s1.body = "Do the thing.".into();
        s1.routing.next = Some(2);
        let sop = authoring_sop(vec![s1, titled_step(2, "Second")]);

        save_sop(dir.path(), &sop).unwrap();

        let loaded =
            load_sop_by_name(dir.path(), "authoring", SopExecutionMode::Supervised).unwrap();
        assert_eq!(loaded.name, sop.name);
        assert_eq!(loaded.execution_mode, SopExecutionMode::Auto);
        assert_eq!(loaded.triggers, sop.triggers);
        assert_eq!(loaded.steps, sop.steps);

        delete_sop(dir.path(), "authoring").unwrap();
        assert!(load_sop_by_name(dir.path(), "authoring", SopExecutionMode::Supervised).is_err());
    }

    #[test]
    fn step_pos_roundtrips_via_toml_and_stays_out_of_markdown() {
        let dir = tempfile::tempdir().unwrap();
        let mut s1 = titled_step(1, "First");
        s1.body = "Do the thing.".into();
        s1.pos = Some(types::StepPos { x: 320.5, y: -48.0 });
        let sop = authoring_sop(vec![s1, titled_step(2, "Second")]);

        save_sop(dir.path(), &sop).unwrap();

        let toml = std::fs::read_to_string(dir.path().join("authoring/SOP.toml")).unwrap();
        assert!(
            toml.contains("[[positions]]"),
            "positions block in TOML: {toml}"
        );
        let md = std::fs::read_to_string(dir.path().join("authoring/SOP.md")).unwrap();
        assert!(
            !md.contains("320.5"),
            "coordinate must not leak into SOP.md: {md}"
        );

        let loaded =
            load_sop_by_name(dir.path(), "authoring", SopExecutionMode::Supervised).unwrap();
        assert_eq!(
            loaded.steps[0].pos,
            Some(types::StepPos { x: 320.5, y: -48.0 })
        );
        assert_eq!(loaded.steps[1].pos, None);
    }

    #[test]
    fn sop_name_path_traversal_is_rejected_across_all_helpers() {
        let dir = tempfile::tempdir().unwrap();
        let hostile = [
            "../escape",
            "..",
            ".",
            "/etc/shadow",
            "a/b",
            "a\\b",
            "../../etc/cron.d/evil",
            "",
        ];
        for name in hostile {
            assert!(
                load_sop_by_name(dir.path(), name, SopExecutionMode::Supervised).is_err(),
                "load must reject {name:?}"
            );
            assert!(
                delete_sop(dir.path(), name).is_err(),
                "delete must reject {name:?}"
            );
            let mut sop = authoring_sop(vec![titled_step(1, "First")]);
            sop.name = name.into();
            assert!(
                save_sop(dir.path(), &sop).is_err(),
                "save must reject {name:?}"
            );
            assert!(
                create_sop(dir.path(), &sop).is_err(),
                "create must reject {name:?}"
            );
        }
        let escape = dir.path().parent().unwrap().join("escape");
        assert!(!escape.exists(), "no write may land outside the SOP root");
    }

    /// Every SOP directory under `sops_dir` must be internally consistent: its
    /// manifest identity matches its directory name, and the steps recorded in
    /// `SOP.toml` match the ones in `SOP.md`. A directory whose identity
    /// disagrees with its manifest, or a pair of files from two different
    /// revisions, is precisely the damage unsynchronized authoring causes.
    /// Returns the SOP names found, sorted.
    fn assert_root_consistent(sops_dir: &Path) -> Vec<String> {
        let mut names = Vec::new();
        for entry in std::fs::read_dir(sops_dir).unwrap() {
            let entry = entry.unwrap();
            let path = entry.path();
            if !path.is_dir() {
                continue;
            }
            let dir_name = entry.file_name().to_string_lossy().into_owned();
            let raw = std::fs::read_to_string(path.join("SOP.toml"))
                .unwrap_or_else(|e| panic!("{dir_name}: manifest unreadable: {e}"));
            let manifest: types::SopManifest = toml::from_str(&raw)
                .unwrap_or_else(|e| panic!("{dir_name}: manifest unparsable: {e}"));
            assert_eq!(
                manifest.sop.name, dir_name,
                "directory name and manifest identity must agree"
            );
            if !manifest.steps.is_empty() {
                let md = std::fs::read_to_string(path.join("SOP.md"))
                    .unwrap_or_else(|e| panic!("{dir_name}: SOP.md unreadable: {e}"));
                let md_titles: Vec<String> =
                    parse_steps(&md).into_iter().map(|s| s.title).collect();
                let manifest_titles: Vec<String> =
                    manifest.steps.iter().map(|s| s.title.clone()).collect();
                assert_eq!(
                    md_titles, manifest_titles,
                    "{dir_name}: SOP.toml and SOP.md are from different revisions"
                );
            }
            names.push(dir_name);
        }
        names.sort();
        names
    }

    fn named_sop(name: &str, step_title: &str) -> Sop {
        let mut sop = authoring_sop(vec![titled_step(1, step_title)]);
        sop.name = name.to_string();
        sop
    }

    #[cfg(unix)]
    #[test]
    fn sync_dir_flushes_a_real_directory_and_reports_a_missing_one() {
        // The rename's durability rests on this, so a silent no-op would be
        // worse than a failure: it would leave the docs claiming a guarantee
        // nothing delivers.
        let dir = tempfile::tempdir().unwrap();
        sync_dir(dir.path()).expect("an existing directory must synchronize");

        let missing = dir.path().join("not-here");
        let err = sync_dir(&missing).expect_err("a missing directory must not report success");
        assert!(err.to_string().contains("synchronizing directory"), "{err}");
    }

    #[test]
    fn write_file_atomic_survives_a_read_only_parent_by_failing_not_lying() {
        // `write_file_atomic` now flushes the parent after persisting. The
        // staging file lands in the same directory, so a directory that cannot
        // be written fails before anything is replaced.
        let dir = tempfile::tempdir().unwrap();
        let target = dir.path().join("nested").join("SOP.toml");
        let err = write_file_atomic(&target, "x = 1\n")
            .expect_err("writing into a directory that does not exist must fail");
        assert!(!target.exists(), "{err}");
    }

    #[cfg(unix)]
    #[test]
    fn rename_sop_refuses_a_symlinked_source_and_leaves_the_target_untouched() {
        // A symlink planted in the SOP root passes the lexical single-component
        // check, and every read and write would then follow it out of the root
        // while the final move only relocates the link. Rename must refuse the
        // source outright, before it touches the external manifest.
        let root = tempfile::tempdir().unwrap();
        let outside = tempfile::tempdir().unwrap();
        save_sop(outside.path(), &named_sop("external", "Outside step")).unwrap();
        let external_dir = outside.path().join("external");
        let external_manifest = external_dir.join("SOP.toml");
        let before = std::fs::read(&external_manifest).unwrap();

        std::os::unix::fs::symlink(&external_dir, root.path().join("linked")).unwrap();

        let err = rename_sop_typed(
            root.path(),
            "linked",
            "captured",
            SopExecutionMode::Supervised,
        )
        .expect_err("a symlinked source SOP must be refused");
        assert!(
            err.to_string().contains("not a directory in the SOP root"),
            "{err}"
        );

        assert_eq!(
            std::fs::read(&external_manifest).unwrap(),
            before,
            "the external manifest must be byte-for-byte untouched"
        );
        assert!(
            std::fs::symlink_metadata(root.path().join("linked"))
                .unwrap()
                .file_type()
                .is_symlink(),
            "the rejected source must be left exactly where it was"
        );
        assert!(!root.path().join("captured").exists());
    }

    #[test]
    fn concurrent_create_and_rename_cannot_merge_two_sops() {
        // Without a shared boundary the create can claim the target between
        // rename's collision check and its move, after which the move replaces
        // the empty directory and create writes its files over the moved SOP.
        for round in 0..12 {
            let dir = tempfile::tempdir().unwrap();
            save_sop(dir.path(), &named_sop("alpha", "Alpha step")).unwrap();
            let root_a = dir.path().to_path_buf();
            let root_b = dir.path().to_path_buf();

            let renamer = std::thread::spawn(move || {
                rename_sop_typed(&root_a, "alpha", "beta", SopExecutionMode::Supervised).is_ok()
            });
            let creator = std::thread::spawn(move || {
                create_sop_typed(&root_b, &named_sop("beta", "Beta step")).is_ok()
            });
            let renamed = renamer.join().unwrap();
            let created = creator.join().unwrap();

            let names = assert_root_consistent(dir.path());
            assert!(
                names.contains(&"beta".to_string()),
                "round {round}: beta must exist whoever won, got {names:?}"
            );
            let beta = load_sop_by_name(dir.path(), "beta", SopExecutionMode::Supervised).unwrap();
            assert!(
                beta.steps[0].title == "Alpha step" || beta.steps[0].title == "Beta step",
                "round {round}: beta must be exactly one definition, got {:?}",
                beta.steps[0].title
            );
            if renamed && created {
                // Both winning means the rename moved alpha to beta and the
                // create then had to lose, or vice versa. They cannot both
                // have written beta.
                panic!("round {round}: create and rename must not both claim 'beta'");
            }
            assert!(
                renamed || created,
                "round {round}: one of the two operations must succeed"
            );
        }
    }

    #[test]
    fn concurrent_save_and_rename_cannot_tear_a_revision() {
        // Rename snapshots the manifest, then writes it back. A save landing in
        // between would be reverted by that stale snapshot, leaving the
        // manifest and SOP.md describing different revisions.
        for round in 0..12 {
            let dir = tempfile::tempdir().unwrap();
            save_sop(dir.path(), &named_sop("alpha", "V1")).unwrap();
            let root_a = dir.path().to_path_buf();
            let root_b = dir.path().to_path_buf();

            let renamer = std::thread::spawn(move || {
                rename_sop_typed(&root_a, "alpha", "beta", SopExecutionMode::Supervised).is_ok()
            });
            let saver =
                std::thread::spawn(move || save_sop(&root_b, &named_sop("alpha", "V2")).is_ok());
            let renamed = renamer.join().unwrap();
            saver.join().unwrap();

            let names = assert_root_consistent(dir.path());
            assert!(
                renamed,
                "round {round}: a save of a different SOP name must not block the rename"
            );
            assert!(
                names.contains(&"beta".to_string()),
                "round {round}: got {names:?}"
            );
            for name in &names {
                let sop = load_sop_by_name(dir.path(), name, SopExecutionMode::Supervised).unwrap();
                assert!(
                    sop.steps[0].title == "V1" || sop.steps[0].title == "V2",
                    "round {round}: {name} must hold one whole revision, got {:?}",
                    sop.steps[0].title
                );
            }
        }
    }

    #[test]
    fn concurrent_renames_of_one_sop_leave_a_single_definition() {
        for round in 0..12 {
            let dir = tempfile::tempdir().unwrap();
            save_sop(dir.path(), &named_sop("alpha", "Only step")).unwrap();
            let root_a = dir.path().to_path_buf();
            let root_b = dir.path().to_path_buf();

            let to_beta = std::thread::spawn(move || {
                rename_sop_typed(&root_a, "alpha", "beta", SopExecutionMode::Supervised).is_ok()
            });
            let to_gamma = std::thread::spawn(move || {
                rename_sop_typed(&root_b, "alpha", "gamma", SopExecutionMode::Supervised).is_ok()
            });
            let beta_won = to_beta.join().unwrap();
            let gamma_won = to_gamma.join().unwrap();

            let names = assert_root_consistent(dir.path());
            assert_eq!(
                names.len(),
                1,
                "round {round}: two renames of one SOP must not fork it, got {names:?}"
            );
            assert_ne!(
                beta_won, gamma_won,
                "round {round}: exactly one rename may succeed"
            );
            assert_eq!(
                names[0],
                if beta_won { "beta" } else { "gamma" },
                "round {round}: the surviving name must be the winner's"
            );
        }
    }

    #[test]
    fn rename_sop_moves_the_directory_and_rewrites_the_manifest_name() {
        let dir = tempfile::tempdir().unwrap();
        let mut sop = authoring_sop(vec![titled_step(1, "First")]);
        sop.steps[0].body = "Do the thing.".into();
        save_sop(dir.path(), &sop).unwrap();

        rename_sop_typed(
            dir.path(),
            "authoring",
            "renamed",
            SopExecutionMode::Supervised,
        )
        .unwrap();

        assert!(
            !dir.path().join("authoring").exists(),
            "the directory the SOP was renamed away from must be gone"
        );
        let loaded = load_sop_by_name(dir.path(), "renamed", SopExecutionMode::Supervised).unwrap();
        assert_eq!(loaded.name, "renamed");
        assert_eq!(loaded.steps, sop.steps);

        // The whole point of the move/delete ordering: one copy, always.
        let dirs: Vec<String> = std::fs::read_dir(dir.path())
            .unwrap()
            .map(|e| e.unwrap())
            .filter(|e| e.path().is_dir())
            .map(|e| e.file_name().to_string_lossy().into_owned())
            .collect();
        assert_eq!(
            dirs,
            vec!["renamed".to_string()],
            "a rename may leave neither a fork nor a hole"
        );
        // The lock file shares the root with the SOPs, so the loader has to
        // keep ignoring anything that is not a SOP directory.
        assert_eq!(
            load_sops_from_directory(dir.path(), SopExecutionMode::Supervised)
                .into_iter()
                .map(|s| s.name)
                .collect::<Vec<_>>(),
            vec!["renamed".to_string()],
            "the authoring lock file must never be loaded as a SOP"
        );
    }

    #[test]
    fn rename_sop_changes_only_the_manifest_name() {
        let dir = tempfile::tempdir().unwrap();
        let sop = authoring_sop(vec![titled_step(1, "First")]);
        save_sop(dir.path(), &sop).unwrap();

        // Make the manifest look like a checked-in one: a comment, and no
        // `execution_mode` so the SOP inherits the runtime default. A rename
        // that round-tripped the SOP through load/save would drop the comment
        // and bake the default in.
        let manifest_path = dir.path().join("authoring").join("SOP.toml");
        let hand_authored: String = std::iter::once("# hand-authored, keep me\n".to_string())
            .chain(
                std::fs::read_to_string(&manifest_path)
                    .unwrap()
                    .lines()
                    .filter(|line| !line.starts_with("execution_mode"))
                    .map(|line| format!("{line}\n")),
            )
            .collect();
        std::fs::write(&manifest_path, &hand_authored).unwrap();
        let md_before =
            std::fs::read_to_string(dir.path().join("authoring").join("SOP.md")).unwrap();

        rename_sop_typed(
            dir.path(),
            "authoring",
            "renamed",
            SopExecutionMode::Supervised,
        )
        .unwrap();

        let after = std::fs::read_to_string(dir.path().join("renamed").join("SOP.toml")).unwrap();
        assert_eq!(
            after,
            hand_authored.replace("name = \"authoring\"", "name = \"renamed\""),
            "the name value is the only byte a rename is allowed to change"
        );
        assert_eq!(
            std::fs::read_to_string(dir.path().join("renamed").join("SOP.md")).unwrap(),
            md_before,
            "SOP.md carries no identity, so a rename must not rewrite it"
        );
    }

    #[test]
    fn rename_sop_rejects_a_collision_and_touches_neither_sop() {
        let dir = tempfile::tempdir().unwrap();
        let mut alpha = authoring_sop(vec![titled_step(1, "First")]);
        alpha.name = "alpha".into();
        save_sop(dir.path(), &alpha).unwrap();
        let mut beta = authoring_sop(vec![titled_step(1, "Other")]);
        beta.name = "beta".into();
        save_sop(dir.path(), &beta).unwrap();

        let alpha_before =
            std::fs::read_to_string(dir.path().join("alpha").join("SOP.toml")).unwrap();
        let beta_before =
            std::fs::read_to_string(dir.path().join("beta").join("SOP.toml")).unwrap();

        let err = rename_sop_typed(dir.path(), "alpha", "beta", SopExecutionMode::Supervised)
            .expect_err("renaming onto an existing SOP must be refused, not merged");
        assert!(
            matches!(&err, SopAuthorError::AlreadyExists(name) if name == "beta"),
            "{err:?}"
        );

        assert_eq!(
            std::fs::read_to_string(dir.path().join("alpha").join("SOP.toml")).unwrap(),
            alpha_before
        );
        assert_eq!(
            std::fs::read_to_string(dir.path().join("beta").join("SOP.toml")).unwrap(),
            beta_before
        );
        assert_eq!(
            load_sop_by_name(dir.path(), "beta", SopExecutionMode::Supervised)
                .unwrap()
                .steps[0]
                .title,
            "Other",
            "the SOP that owns the name keeps its own steps"
        );
    }

    #[test]
    fn rename_sop_rejects_path_traversal_in_either_name() {
        let dir = tempfile::tempdir().unwrap();
        let sop = authoring_sop(vec![titled_step(1, "First")]);
        save_sop(dir.path(), &sop).unwrap();

        let hostile = [
            "../escape",
            "..",
            ".",
            "/etc/shadow",
            "a/b",
            "a\\b",
            "../../etc/cron.d/evil",
            "",
        ];
        for name in hostile {
            assert!(
                rename_sop_typed(dir.path(), "authoring", name, SopExecutionMode::Supervised)
                    .is_err(),
                "rename target must reject {name:?}"
            );
            assert!(
                rename_sop_typed(dir.path(), name, "authoring", SopExecutionMode::Supervised)
                    .is_err(),
                "rename source must reject {name:?}"
            );
        }
        let escape = dir.path().parent().unwrap().join("escape");
        assert!(!escape.exists(), "no rename may land outside the SOP root");
        assert!(
            load_sop_by_name(dir.path(), "authoring", SopExecutionMode::Supervised).is_ok(),
            "a rejected rename leaves the SOP exactly where it was"
        );
    }

    #[test]
    fn rename_sop_rejects_an_unknown_source() {
        let dir = tempfile::tempdir().unwrap();
        let err = rename_sop_typed(
            dir.path(),
            "missing",
            "renamed",
            SopExecutionMode::Supervised,
        )
        .expect_err("renaming a SOP that does not exist must be refused");
        assert!(
            matches!(&err, SopAuthorError::NotFound(name) if name == "missing"),
            "{err:?}"
        );
        assert!(!dir.path().join("renamed").exists());
    }

    #[test]
    fn rename_sop_rejects_renaming_to_the_same_name() {
        let dir = tempfile::tempdir().unwrap();
        let sop = authoring_sop(vec![titled_step(1, "First")]);
        save_sop(dir.path(), &sop).unwrap();

        let err = rename_sop_typed(
            dir.path(),
            "authoring",
            "authoring",
            SopExecutionMode::Supervised,
        )
        .expect_err("a no-op rename is a caller mistake, not a silent success");
        assert!(err.to_string().contains("already named"), "{err}");
        assert!(load_sop_by_name(dir.path(), "authoring", SopExecutionMode::Supervised).is_ok());
    }

    #[test]
    fn rename_sop_applies_strict_save_validation() {
        let dir = tempfile::tempdir().unwrap();
        let mut first = titled_step(1, "First");
        first.routing.next = Some(2);
        let sop = authoring_sop(vec![first, titled_step(2, "Second")]);
        save_sop(dir.path(), &sop).unwrap();

        // Route step 1 at a step that does not exist: the same blocking
        // problem that stops `save_sop` writing a SOP in the first place.
        let md_path = dir.path().join("authoring").join("SOP.md");
        let broken = std::fs::read_to_string(&md_path)
            .unwrap()
            .replace("next: 2", "next: 99");
        std::fs::write(&md_path, broken).unwrap();

        let err = rename_sop_typed(
            dir.path(),
            "authoring",
            "renamed",
            SopExecutionMode::Supervised,
        )
        .expect_err("a rename must not smuggle a SOP strict save would reject onto disk");
        assert!(err.to_string().contains("SOP rejected"), "{err}");
        assert!(
            !dir.path().join("renamed").exists(),
            "nothing moves when validation fails"
        );
        assert!(dir.path().join("authoring").exists());
    }

    #[test]
    fn rename_sop_keeps_a_trailing_comment_on_the_name_line() {
        // The name line is the one line a rename edits, so it is the line most
        // at risk of losing its decoration. Swapping the whole TOML item would
        // drop the comment; only the string may change.
        let dir = tempfile::tempdir().unwrap();
        let sop = authoring_sop(vec![titled_step(1, "First")]);
        save_sop(dir.path(), &sop).unwrap();

        let manifest_path = dir.path().join("authoring").join("SOP.toml");
        let annotated = std::fs::read_to_string(&manifest_path).unwrap().replace(
            "name = \"authoring\"",
            "name = \"authoring\" # operator note, keep me",
        );
        std::fs::write(&manifest_path, &annotated).unwrap();

        rename_sop_typed(
            dir.path(),
            "authoring",
            "renamed",
            SopExecutionMode::Supervised,
        )
        .unwrap();

        let after = std::fs::read_to_string(dir.path().join("renamed").join("SOP.toml")).unwrap();
        assert!(
            after.contains("name = \"renamed\" # operator note, keep me"),
            "the comment on the renamed line must survive: {after}"
        );
    }

    #[cfg(unix)]
    #[test]
    fn rename_sop_preserves_the_manifest_file_mode() {
        // The manifest is replaced by renaming a fresh file over it, which
        // would otherwise hand it whatever mode the umask dictates and widen a
        // deliberately tight one.
        use std::os::unix::fs::PermissionsExt;

        let dir = tempfile::tempdir().unwrap();
        let sop = authoring_sop(vec![titled_step(1, "First")]);
        save_sop(dir.path(), &sop).unwrap();

        let manifest_path = dir.path().join("authoring").join("SOP.toml");
        std::fs::set_permissions(&manifest_path, std::fs::Permissions::from_mode(0o600)).unwrap();

        rename_sop_typed(
            dir.path(),
            "authoring",
            "renamed",
            SopExecutionMode::Supervised,
        )
        .unwrap();

        let mode = std::fs::metadata(dir.path().join("renamed").join("SOP.toml"))
            .unwrap()
            .permissions()
            .mode()
            & 0o777;
        assert_eq!(mode, 0o600, "a rename must not widen the manifest's mode");
    }

    #[cfg(unix)]
    #[test]
    fn rename_sop_does_not_write_through_a_planted_staging_symlink() {
        // A predictable staging name (`.SOP.toml.tmp`) would be followed and
        // its target truncated. Staging under an unpredictable, exclusively
        // created name means a planted link is simply never opened.
        let dir = tempfile::tempdir().unwrap();
        let sop = authoring_sop(vec![titled_step(1, "First")]);
        save_sop(dir.path(), &sop).unwrap();

        let outside = dir.path().join("outside-victim");
        std::fs::write(&outside, "do not truncate me").unwrap();
        std::os::unix::fs::symlink(&outside, dir.path().join("authoring/.SOP.toml.tmp")).unwrap();

        rename_sop_typed(
            dir.path(),
            "authoring",
            "renamed",
            SopExecutionMode::Supervised,
        )
        .unwrap();

        assert_eq!(
            std::fs::read_to_string(&outside).unwrap(),
            "do not truncate me",
            "the staging write must not follow a planted symlink"
        );
        let after = std::fs::read_to_string(dir.path().join("renamed").join("SOP.toml")).unwrap();
        assert!(after.contains("name = \"renamed\""), "{after}");
    }

    #[cfg(unix)]
    #[test]
    fn rename_sop_rolls_back_the_manifest_when_the_move_fails() {
        use std::os::unix::fs::PermissionsExt;

        let dir = tempfile::tempdir().unwrap();
        let sop = authoring_sop(vec![titled_step(1, "First")]);
        save_sop(dir.path(), &sop).unwrap();
        let before =
            std::fs::read_to_string(dir.path().join("authoring").join("SOP.toml")).unwrap();

        // Deny the move (renaming a directory needs write on its parent) while
        // leaving the SOP's own directory writable, so the manifest rewrite
        // lands and only the commit step fails.
        let root_perms = std::fs::metadata(dir.path()).unwrap().permissions();
        std::fs::set_permissions(dir.path(), std::fs::Permissions::from_mode(0o500)).unwrap();
        let probe = dir.path().join(".write-probe");
        if std::fs::File::create(&probe).is_ok() {
            // Running as root, where the mode bits above are advisory. The
            // rollback path is unreachable here; leave it to a non-root run.
            let _ = std::fs::remove_file(&probe);
            std::fs::set_permissions(dir.path(), root_perms).unwrap();
            return;
        }
        let err = rename_sop_typed(
            dir.path(),
            "authoring",
            "renamed",
            SopExecutionMode::Supervised,
        )
        .expect_err("a move that cannot happen must not report success");
        std::fs::set_permissions(dir.path(), root_perms).unwrap();

        assert!(
            matches!(err, SopAuthorError::Io(_)),
            "a failed move is a filesystem failure, not a bad request: {err:?}"
        );
        assert!(err.to_string().contains("failed to move"), "{err}");
        assert_eq!(
            std::fs::read_to_string(dir.path().join("authoring").join("SOP.toml")).unwrap(),
            before,
            "the manifest goes back to the name the SOP still answers to on disk"
        );
        assert!(!dir.path().join("renamed").exists());
    }

    #[test]
    fn validate_sop_strict_blocks_graph_errors_and_duplicates() {
        let mut s1 = titled_step(1, "a");
        s1.routing.next = Some(99);
        let validation = validate_sop_strict(&authoring_sop(vec![s1, titled_step(1, "b")]));
        assert!(!validation.is_ok());
        assert!(
            validation
                .blocking
                .iter()
                .any(|b| b.contains("Duplicate step number 1"))
        );
        assert!(validation.blocking.iter().any(|b| b.contains("step 99")));

        let ok = validate_sop_strict(&authoring_sop(vec![titled_step(1, "a")]));
        assert!(ok.is_ok());
    }

    #[test]
    fn parse_steps_keeps_legacy_tools_hint() {
        let steps = parse_steps(
            r#"
## Steps
1. **Collect** - Gather context.
   - tools: read_file, shell
"#,
        );

        assert_eq!(steps.len(), 1);
        assert_eq!(steps[0].suggested_tools, vec!["read_file", "shell"]);
        assert!(steps[0].scope.is_none());
        assert_eq!(
            steps[0]
                .effective_tool_scope()
                .as_ref()
                .and_then(|scope| scope.allow.clone()),
            Some(vec!["read_file".to_string(), "shell".to_string()])
        );
        assert!(steps[0].routing.when.is_none());
        assert_eq!(steps[0].on_failure, StepFailure::Fail);
    }

    #[test]
    fn parse_steps_populates_contract_bullets() {
        let steps = parse_steps(
            r#"
## Steps
1. **Collect** - Gather context.
   - input: {"type":"object","required":["ticket"]}
   - output: {"type":"object","properties":{"ok":{"type":"boolean"}}}
   - allow-tools: fs
   - deny-tools: shell
   - when: $.steps.1.ok == true
   - next: 3
   - depends_on: 1, 2
   - switch: pull_request>$.event>3; catch_all>>2
   - on_failure: retry:2
   - mode: auto
"#,
        );

        let step = &steps[0];
        assert_eq!(
            step.schema.as_ref().and_then(|schema| schema.input.clone()),
            Some(json!({"type":"object","required":["ticket"]}))
        );
        assert_eq!(
            step.schema
                .as_ref()
                .and_then(|schema| schema.output.clone()),
            Some(json!({"type":"object","properties":{"ok":{"type":"boolean"}}}))
        );
        assert_eq!(
            step.scope.as_ref().and_then(|scope| scope.allow.clone()),
            Some(vec!["fs".to_string()])
        );
        assert_eq!(
            step.scope.as_ref().map(|scope| scope.deny.clone()),
            Some(vec!["shell".to_string()])
        );
        assert_eq!(step.routing.when.as_deref(), Some("$.steps.1.ok == true"));
        assert_eq!(step.routing.next, Some(3));
        assert_eq!(step.routing.depends_on, vec![1, 2]);
        assert_eq!(
            step.routing.switch,
            vec![
                SwitchRule {
                    name: "pull_request".into(),
                    when: Some("$.event".into()),
                    goto: Some(3),
                },
                SwitchRule {
                    name: "catch_all".into(),
                    when: None,
                    goto: Some(2),
                },
            ]
        );
        assert_eq!(step.on_failure, StepFailure::Retry { max: 2 });
        assert_eq!(step.mode, Some(SopExecutionMode::Auto));
    }

    #[test]
    fn step_agent_override_roundtrips_through_render_and_parse() {
        let mut step = titled_step(1, "notify");
        step.agent = Some("pr_bot".into());
        let parsed = parse_steps(&render_steps(&[step.clone()]));
        assert_eq!(parsed[0].agent.as_deref(), Some("pr_bot"));

        let mut plain = titled_step(2, "wait");
        plain.agent = None;
        let parsed = parse_steps(&render_steps(&[plain]));
        assert!(parsed[0].agent.is_none(), "no agent bullet when unset");
    }

    #[test]
    fn effective_agent_prefers_step_override_then_parent() {
        let mut step = titled_step(1, "s");
        assert_eq!(step.effective_agent(Some("parent")), Some("parent"));
        assert_eq!(step.effective_agent(None), None);
        step.agent = Some("override".into());
        assert_eq!(step.effective_agent(Some("parent")), Some("override"));
    }

    fn planned(tool: &str, args: serde_json::Value) -> PlannedToolCall {
        PlannedToolCall {
            tool: tool.into(),
            args,
            pinned: None,
        }
    }

    #[test]
    fn planned_calls_roundtrip_through_render_and_parse() {
        let mut step = titled_step(1, "fetch");
        step.calls = vec![
            planned("http_request", json!({"url": "https://example.com"})),
            PlannedToolCall {
                tool: "calculator".into(),
                args: json!({"function": "add", "values": "{{calls.0.status}}"}),
                pinned: Some(json!({"result": 3.0})),
            },
        ];
        let rendered = render_steps(std::slice::from_ref(&step));
        let parsed = parse_steps(&rendered);
        assert_eq!(parsed.len(), 1);
        assert_eq!(parsed[0].calls, step.calls);
    }

    #[test]
    fn strict_save_blocks_forward_step_binding() {
        let mut s1 = titled_step(1, "a");
        s1.calls = vec![planned("shell", json!({"command": "{{steps.2.out}}"}))];
        let sop = authoring_sop(vec![s1, titled_step(2, "b")]);
        let v = validate_sop_strict(&sop);
        assert!(
            v.blocking
                .iter()
                .any(|b| b.contains("does not run before step 1")),
            "got: {:?}",
            v.blocking
        );
    }

    #[test]
    fn strict_save_blocks_unknown_step_and_self_call_bindings() {
        let mut s2 = titled_step(2, "b");
        s2.calls = vec![
            planned("shell", json!({"command": "{{steps.9.out}}"})),
            planned("shell", json!({"command": "{{calls.1.out}}"})),
        ];
        let sop = authoring_sop(vec![titled_step(1, "a"), s2]);
        let v = validate_sop_strict(&sop);
        assert!(
            v.blocking.iter().any(|b| b.contains("unknown step 9")),
            "got: {:?}",
            v.blocking
        );
        assert!(
            v.blocking
                .iter()
                .any(|b| b.contains("does not run before call 1")),
            "got: {:?}",
            v.blocking
        );
    }

    #[test]
    fn strict_save_blocks_malformed_binding() {
        let mut s1 = titled_step(1, "a");
        s1.calls = vec![planned("shell", json!({"command": "{{bogus.thing}}"}))];
        let sop = authoring_sop(vec![s1]);
        let v = validate_sop_strict(&sop);
        assert!(
            v.blocking.iter().any(|b| b.contains("malformed binding")),
            "got: {:?}",
            v.blocking
        );
    }

    #[test]
    fn strict_save_accepts_valid_bindings_and_warns_on_schemaless_target() {
        let mut s1 = titled_step(1, "a");
        s1.schema = Some(StepSchema {
            input: None,
            output: Some(json!({"type": "object"})),
        });
        let mut s2 = titled_step(2, "b");
        s2.calls = vec![
            planned("http_request", json!({"url": "{{steps.1.url}}"})),
            planned("shell", json!({"command": "echo {{calls.0.status}}"})),
        ];
        let mut s3 = titled_step(3, "c");
        s3.calls = vec![planned("shell", json!({"command": "{{steps.2.out}}"}))];
        let sop = authoring_sop(vec![s1, s2, s3]);
        let v = validate_sop_strict(&sop);
        assert!(v.is_ok(), "blocking: {:?}", v.blocking);

        let mut s4 = titled_step(1, "bare");
        let mut s5 = titled_step(2, "binder");
        s5.calls = vec![planned("shell", json!({"command": "{{steps.1.out}}"}))];
        s4.calls = Vec::new();
        let sop = authoring_sop(vec![s4, s5]);
        let v = validate_sop_strict(&sop);
        assert!(v.is_ok());
        assert!(
            v.warnings
                .iter()
                .any(|w| w.contains("no output schema or planned calls")),
            "got: {:?}",
            v.warnings
        );
    }

    #[test]
    fn normalize_step_numbers_rewrites_call_bindings() {
        let mut s3 = titled_step(30, "c");
        s3.calls = vec![planned(
            "shell",
            json!({"command": "{{steps.10.out}} then {{steps.20.ok}}"}),
        )];
        let mut sop = authoring_sop(vec![titled_step(10, "a"), titled_step(20, "b"), s3]);
        normalize_step_numbers(&mut sop);
        assert_eq!(
            sop.steps[2].calls[0].args,
            json!({"command": "{{steps.1.out}} then {{steps.2.ok}}"})
        );
    }

    #[test]
    fn parse_steps_reads_policy_bullet() {
        let steps = parse_steps(
            r#"
## Steps
1. **Gate** - Requires the release group.
   - policy: prod
2. **Go** - Unpoliced.
"#,
        );
        assert_eq!(steps[0].policy.as_deref(), Some("prod"));
        assert_eq!(
            steps[1].policy, None,
            "a step with no policy bullet stays None"
        );
    }

    #[test]
    fn parse_steps_populates_capability_bullets() {
        let steps = parse_steps(
            r#"
## Steps
1. **Status** - Check the repository.
   - kind: capability
   - capability: git.status
   - with: { require_clean = true }
"#,
        );

        let step = &steps[0];
        assert_eq!(step.kind, SopStepKind::Capability);
        assert_eq!(step.capability.as_deref(), Some("git.status"));
        assert_eq!(
            step.capability_input.clone(),
            Some(json!({"require_clean": true}))
        );
    }

    #[test]
    fn load_sop_reads_admission_policy_and_pending_cap() {
        // A2: admission_policy + max_pending_approvals are user-facing SOP.toml knobs;
        // prove they survive the SOP.toml -> runtime Sop load path.
        let tmp = tempfile::tempdir().unwrap();
        let dir = tmp.path().join("s");
        std::fs::create_dir_all(&dir).unwrap();
        std::fs::write(
            dir.join("SOP.toml"),
            "[sop]\nname = \"s\"\ndescription = \"d\"\nadmission_policy = \"drop\"\nmax_pending_approvals = 1\n",
        )
        .unwrap();
        let sop = load_sop(&dir, SopExecutionMode::Supervised).expect("load ok");
        assert_eq!(
            sop.admission_policy,
            crate::sop::types::SopAdmissionPolicy::Drop
        );
        assert_eq!(sop.max_pending_approvals, 1);
    }
}
