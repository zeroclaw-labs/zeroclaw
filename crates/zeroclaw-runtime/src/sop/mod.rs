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
pub use engine::{MaintenanceSummary, SopEngine, err_is_resume_at_capacity};
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
pub fn build_sop_engine(
    config: SopConfig,
    workspace_dir: &Path,
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
    // startup, so fall back to in-memory with a loud log. `workspace_dir` here is the
    // daemon data dir (every caller passes `config.data_dir`), so a durable store
    // lands at `<data_dir>/sop/runs.db` unless `[sop] run_state_dir` overrides it.
    let store = store::build_run_store(&config, workspace_dir).unwrap_or_else(|e| {
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
    engine.reload(workspace_dir);
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

/// Return the default SOPs directory: `<workspace>/sops`.
fn sops_dir(workspace_dir: &Path) -> PathBuf {
    workspace_dir.join("sops")
}

/// Resolve the SOPs directory from config, falling back to workspace default.
///
/// A relative `config_dir` (the common case in the documented
/// `<workspace>/sops` layout) resolves against `workspace_dir`; an
/// absolute or `~`-prefixed value is used as-is (`Path::join` replaces
/// the base entirely when the joined path is itself absolute).
pub fn resolve_sops_dir(workspace_dir: &Path, config_dir: Option<&str>) -> PathBuf {
    match config_dir {
        Some(dir) if !dir.is_empty() => {
            let expanded = shellexpand::tilde(dir);
            workspace_dir.join(expanded.as_ref())
        }
        _ => sops_dir(workspace_dir),
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

/// Load all SOPs from the configured directory.
pub fn load_sops(
    workspace_dir: &Path,
    config_dir: Option<&str>,
    default_execution_mode: SopExecutionMode,
) -> Vec<Sop> {
    let dir = resolve_sops_dir(workspace_dir, config_dir);
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
    if resolve_sop_dir(sops_dir, &sop.name)?.exists() {
        anyhow::bail!("SOP '{}' already exists", sop.name);
    }
    save_sop(sops_dir, sop)
}

/// Typed classification of an authoring failure so transports map it to the
/// right status/RPC code without matching on stringified message substrings.
#[derive(Debug)]
pub enum SopAuthorError {
    AlreadyExists(String),
    NotFound(String),
    Other(anyhow::Error),
}

impl std::fmt::Display for SopAuthorError {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        match self {
            SopAuthorError::AlreadyExists(name) => write!(f, "SOP '{name}' already exists"),
            SopAuthorError::NotFound(name) => write!(f, "SOP '{name}' not found"),
            SopAuthorError::Other(e) => write!(f, "{e}"),
        }
    }
}

impl std::error::Error for SopAuthorError {}

pub fn create_sop_typed(sops_dir: &Path, sop: &Sop) -> std::result::Result<(), SopAuthorError> {
    let dir = resolve_sop_dir(sops_dir, &sop.name).map_err(SopAuthorError::Other)?;
    if dir.exists() {
        return Err(SopAuthorError::AlreadyExists(sop.name.clone()));
    }
    save_sop(sops_dir, sop).map_err(SopAuthorError::Other)
}

pub fn delete_sop_typed(sops_dir: &Path, name: &str) -> std::result::Result<(), SopAuthorError> {
    let dir = resolve_sop_dir(sops_dir, name).map_err(SopAuthorError::Other)?;
    if !dir.exists() {
        return Err(SopAuthorError::NotFound(name.to_string()));
    }
    std::fs::remove_dir_all(&dir).map_err(|e| SopAuthorError::Other(e.into()))
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
            if let Some(tools_str) = bullet.strip_prefix("tools:") {
                current.tools = parse_csv_list(tools_str);
            } else if let Some(tools_str) = bullet
                .strip_prefix("allow-tools:")
                .or_else(|| bullet.strip_prefix("allow_tools:"))
            {
                ensure_scope(&mut current.scope).allow = Some(parse_csv_list(tools_str));
            } else if let Some(tools_str) = bullet
                .strip_prefix("deny-tools:")
                .or_else(|| bullet.strip_prefix("deny_tools:"))
            {
                ensure_scope(&mut current.scope).deny = parse_csv_list(tools_str);
            } else if bullet.starts_with("requires_confirmation:") {
                if let Some(val) = bullet.strip_prefix("requires_confirmation:") {
                    current.requires_confirmation = val.trim().eq_ignore_ascii_case("true");
                }
            } else if bullet.starts_with("kind:") {
                if let Some(val) = bullet.strip_prefix("kind:") {
                    current.kind = parse_step_kind(val);
                }
            } else if let Some(val) = bullet.strip_prefix("capability:") {
                current.capability = Some(val.trim().to_string());
            } else if let Some(val) = bullet.strip_prefix("with:") {
                current.capability_input = Some(parse_value_fragment(val.trim()));
            } else if let Some(val) = bullet.strip_prefix("input:") {
                ensure_schema(&mut current.schema).input = Some(parse_value_fragment(val.trim()));
            } else if let Some(val) = bullet.strip_prefix("output:") {
                ensure_schema(&mut current.schema).output = Some(parse_value_fragment(val.trim()));
            } else if let Some(val) = bullet.strip_prefix("when:") {
                let val = val.trim();
                if !val.is_empty() {
                    current.routing.when = Some(val.to_string());
                }
            } else if let Some(val) = bullet.strip_prefix("next:") {
                current.routing.next = val.trim().parse::<u32>().ok();
            } else if let Some(val) = bullet.strip_prefix("terminal:") {
                current.routing.terminal = val.trim().eq_ignore_ascii_case("true");
            } else if let Some(val) = bullet
                .strip_prefix("depends_on:")
                .or_else(|| bullet.strip_prefix("depends-on:"))
            {
                current.routing.depends_on = parse_u32_list(val);
            } else if let Some(val) = bullet.strip_prefix("switch:") {
                current.routing.switch = parse_switch_rules(val);
            } else if let Some(val) = bullet
                .strip_prefix("on_failure:")
                .or_else(|| bullet.strip_prefix("on-failure:"))
            {
                current.on_failure = parse_step_failure(val);
            } else if let Some(val) = bullet.strip_prefix("mode:") {
                current.mode = Some(parse_execution_mode(val));
            } else if let Some(val) = bullet.strip_prefix("agent:") {
                let trimmed_val = val.trim();
                current.agent = (!trimmed_val.is_empty()).then(|| trimmed_val.to_string());
            } else if let Some(val) = bullet.strip_prefix("call:") {
                if let Ok(call) = serde_json::from_str::<PlannedToolCall>(val.trim()) {
                    current.calls.push(call);
                }
            } else if let Some(val) = bullet.strip_prefix("prompt:") {
                let val = val.trim();
                if !val.is_empty() {
                    current.gate_prompt = Some(val.to_string());
                }
            } else if let Some(val) = bullet.strip_prefix("policy:") {
                let val = val.trim();
                current.policy = if val.is_empty() {
                    None
                } else {
                    Some(val.to_string())
                };
            } else if let Some(val) = bullet.strip_prefix("edit:") {
                // Editable-field opt-in for a checkpoint gate: the named field of
                // the piped value an approver may amend before the run resumes.
                let val = val.trim();
                current.edit = if val.is_empty() {
                    None
                } else {
                    Some(val.to_string())
                };
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
    fn resolve_sops_dir_joins_relative_config_value_to_workspace() {
        let workspace = Path::new("/home/user/.zoder/data");
        let resolved = resolve_sops_dir(workspace, Some("shared/sops"));
        assert_eq!(resolved, workspace.join("shared/sops"));
    }

    #[test]
    fn resolve_sops_dir_keeps_absolute_config_value_as_is() {
        let workspace = Path::new("/home/user/.zoder/data");
        let resolved = resolve_sops_dir(workspace, Some("/srv/shared/sops"));
        assert_eq!(resolved, Path::new("/srv/shared/sops"));
    }

    #[test]
    fn resolve_sops_dir_falls_back_to_workspace_sops_when_unset() {
        let workspace = Path::new("/home/user/.zoder/data");
        assert_eq!(resolve_sops_dir(workspace, None), workspace.join("sops"));
        assert_eq!(
            resolve_sops_dir(workspace, Some("")),
            workspace.join("sops")
        );
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
