pub mod active_scope;
pub mod approval;
pub mod audit;
pub mod condition;
pub mod dispatch;
pub mod engine;
pub mod executor;
pub mod graph;
pub mod metrics;
pub mod route;
pub mod rundata;
pub mod schema;
pub mod scope;
pub mod step_contract;
pub mod store;
pub mod trigger_registry;
pub mod types;
pub mod wire;

pub use audit::SopAuditLogger;
pub use engine::{MaintenanceSummary, SopEngine};
pub use graph::{
    FlowRole, GraphDiagnostic, GraphLayout, GraphNode, GraphPin, GraphRender, GraphSeverity,
    GraphWire, NodeKind, NodePosition, NodeRunOverlay, NodeRunState, PinClass, RunOverlay,
    SopGraph, TRIGGER_NODE_BASE, TextGraphFormat, render_graph_text,
};
pub use metrics::SopMetricsCollector;
pub use scope::StepToolScope;
pub use step_contract::{StepFailure, StepRouting, SwitchRule};
pub use store::{
    ClaimToken, PersistedRun, ProposalRecord, ProposalStatus, SopEventRecord, SopRunStore,
    SqliteRunStore, StoreError, build_run_store,
};
pub use trigger_registry::{
    BoundTriggerSource, ChannelAlias, ChannelTriggerKind, ConfiguredChannel, TriggerSourceRegistry,
    build_registry,
};
#[allow(unused_imports)]
pub use types::{
    DeterministicRunState, DeterministicSavings, FilesystemEventKind, Sop, SopEvent,
    SopExecutionMode, SopPriority, SopRun, SopRunAction, SopRunStatus, SopStep, SopStepKind,
    SopStepResult, SopStepStatus, SopTrigger, SopTriggerSource, StepSchema,
};
pub use wire::{WireEdit, WireError, WireOp, apply_wire};

use anyhow::Result;
use std::path::{Path, PathBuf};
use std::sync::{Arc, Mutex};

use types::{SopManifest, SopMeta};
use zeroclaw_config::schema::SopConfig;
use zeroclaw_memory::traits::Memory;

/// Build a single shared SopEngine + SopAuditLogger pair.
///
/// This is the sole construction site for SOP state within a daemon.
/// Callers receive `Arc<Mutex<SopEngine>>` and `Arc<SopAuditLogger>`
/// handles — never call `SopEngine::new` or `SopAuditLogger::new`
/// directly outside this module.
pub fn build_sop_engine(
    config: SopConfig,
    workspace_dir: &Path,
    audit_memory: Arc<dyn Memory>,
) -> (Arc<Mutex<SopEngine>>, Arc<SopAuditLogger>) {
    // Select the run-state backend from config (default: ephemeral in-memory,
    // unchanged behavior). A backend-open failure must not crash daemon startup,
    // so fall back to in-memory with a loud log. `workspace_dir` here is the
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
    let mut engine = SopEngine::new(config)
        .with_store(store)
        .with_metrics(SopMetricsCollector::shared());
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
pub fn resolve_sops_dir(workspace_dir: &Path, config_dir: Option<&str>) -> PathBuf {
    match config_dir {
        Some(dir) if !dir.is_empty() => {
            let expanded = shellexpand::tilde(dir);
            PathBuf::from(expanded.as_ref())
        }
        _ => sops_dir(workspace_dir),
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

/// Load a single SOP by name from the configured directory. Lenient: returns
/// the SOP with any diagnostics resolvable via `validate_sop_strict`.
pub fn load_sop_by_name(
    sops_dir: &Path,
    name: &str,
    default_execution_mode: SopExecutionMode,
) -> Result<Sop> {
    load_sop(&sops_dir.join(name), default_execution_mode)
}

/// Delete a SOP directory by name. Errors if it does not exist.
pub fn delete_sop(sops_dir: &Path, name: &str) -> Result<()> {
    let dir = sops_dir.join(name);
    if !dir.exists() {
        anyhow::bail!("SOP '{name}' not found");
    }
    std::fs::remove_dir_all(&dir)?;
    Ok(())
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
    let steps = if md_path.exists() {
        let md_content = std::fs::read_to_string(&md_path)?;
        parse_steps(&md_content)
    } else {
        Vec::new()
    };

    let SopMeta {
        name,
        description,
        version,
        priority,
        execution_mode,
        cooldown_secs,
        max_concurrent,
        deterministic,
    } = manifest.sop;

    // When deterministic=true, override execution_mode to Deterministic
    let effective_mode = if deterministic {
        SopExecutionMode::Deterministic
    } else {
        execution_mode.unwrap_or(default_execution_mode)
    };

    Ok(Sop {
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
    })
}

// ── Markdown step parser ────────────────────────────────────────

/// Parse procedure steps from SOP.md content.
///
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
                    let val = val.trim();
                    if val.eq_ignore_ascii_case("checkpoint") {
                        current.kind = SopStepKind::Checkpoint;
                    } else {
                        current.kind = SopStepKind::Execute;
                    }
                }
            } else if let Some(val) = bullet.strip_prefix("input:") {
                ensure_schema(&mut current.schema).input = Some(parse_schema_fragment(val.trim()));
            } else if let Some(val) = bullet.strip_prefix("output:") {
                ensure_schema(&mut current.schema).output = Some(parse_schema_fragment(val.trim()));
            } else if let Some(val) = bullet.strip_prefix("when:") {
                let val = val.trim();
                if !val.is_empty() {
                    current.routing.when = Some(val.to_string());
                }
            } else if let Some(val) = bullet.strip_prefix("next:") {
                current.routing.next = val.trim().parse::<u32>().ok();
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
    schema: Option<StepSchema>,
    scope: Option<StepToolScope>,
    routing: StepRouting,
    on_failure: StepFailure,
    mode: Option<SopExecutionMode>,
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
            schema: self.schema.take(),
            scope: self.scope.take(),
            routing: std::mem::take(&mut self.routing),
            on_failure: std::mem::take(&mut self.on_failure),
            mode: self.mode.take(),
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

/// Parse a `name>when>goto; name>when>goto` switch bullet. Each segment needs
/// a non-empty name; an empty `when` field is the catch-all. Segments without
/// a usable name are dropped.
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

fn parse_schema_fragment(value: &str) -> serde_json::Value {
    serde_json::from_str(value).unwrap_or_else(|_| serde_json::Value::String(value.into()))
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

// ── Markdown step renderer (inverse of parse_steps) ─────────────

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

    bullets
}

/// Render procedure steps to SOP.md, the exact inverse of `parse_steps`:
/// a `## Steps` heading, numbered items with a bold title, optional body,
/// and the per-step sub-bullets in a stable order.
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

// ── SOP save (round-trip back to disk) ──────────────────────────

/// Serialize a `Sop` to its on-disk pair (`SOP.toml`, `SOP.md`) under
/// `<sops_dir>/<name>/`. Reuses the manifest serde and the `render_steps`
/// Markdown projection. No parallel format.
///
/// STRICT: validates first and writes NOTHING if any blocking violation is
/// present. Warnings do not block. Returns the blocking list on rejection.
pub fn save_sop(sops_dir: &Path, sop: &Sop) -> Result<()> {
    let validation = validate_sop_strict(sop);
    if !validation.is_ok() {
        anyhow::bail!("SOP rejected: {}", validation.blocking.join("; "));
    }

    let sop_dir = sops_dir.join(&sop.name);
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

// ── Validation engine (one rule set, two severities) ────────────

/// The outcome of validating a `Sop` against the one rule set. Blocking
/// violations reject a save; warnings never block.
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

/// Validate a `Sop` with both severities. Folds three sources:
/// 1. save-only hard constraints (blocking): non-empty name, non-empty step
///    titles, unique step number (the on-disk routing key).
/// 2. `SopGraph` structural diagnostics by severity (Error -> blocking,
///    Warning -> advisory).
/// 3. `validate_sop` advisory checks (warnings).
///
/// NOTE: dup detection keys on step NUMBER, the current on-disk routing key.
/// Keying by a title-slug would require a model change and is out of scope.
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

    let graph = SopGraph::from_sop(sop);
    let mut warnings = Vec::new();
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
    fn switch_survives_render_then_parse() {
        let original = vec![SopStep {
            number: 1,
            title: "gate".into(),
            body: "route on event".into(),
            routing: StepRouting {
                switch: vec![
                    SwitchRule {
                        name: "pull_request".into(),
                        when: Some("$.event == \"pr\"".into()),
                        goto: Some(2),
                    },
                    SwitchRule {
                        name: "unknown".into(),
                        when: None,
                        goto: Some(3),
                    },
                ],
                ..Default::default()
            },
            ..Default::default()
        }];
        let md = render_steps(&original);
        assert!(md.contains("switch: pull_request>$.event == \"pr\">2; unknown>>3"));
        let parsed = parse_steps(&md);
        assert_eq!(parsed[0].routing.switch, original[0].routing.switch);
    }

    #[test]
    fn save_sop_round_trips_through_loader() {
        let tmp = tempfile::tempdir().unwrap();
        let sops_dir = tmp.path();

        let steps = parse_steps(
            r#"
## Steps
1. **First** - do the first thing
   - tools: shell, file_read
   - requires_confirmation: true
   - next: 2
2. **Second** - then the second
   - allow-tools: shell
   - deny-tools: browser
   - kind: checkpoint
   - depends_on: 1
   - on_failure: retry: 3
   - mode: auto
"#,
        );

        let original = Sop {
            name: "roundtrip".to_string(),
            description: "exercises every field path".to_string(),
            version: "2.3.4".to_string(),
            priority: SopPriority::High,
            execution_mode: SopExecutionMode::StepByStep,
            triggers: vec![
                SopTrigger::Manual,
                SopTrigger::Cron {
                    expression: "0 9 * * *".to_string(),
                },
            ],
            steps,
            cooldown_secs: 30,
            max_concurrent: 2,
            location: None,
            deterministic: false,
        };

        save_sop(sops_dir, &original).expect("save");
        let reloaded =
            load_sop(&sops_dir.join("roundtrip"), SopExecutionMode::Supervised).expect("load");

        assert_eq!(reloaded.name, original.name);
        assert_eq!(reloaded.description, original.description);
        assert_eq!(reloaded.version, original.version);
        assert_eq!(reloaded.priority, original.priority);
        assert_eq!(reloaded.execution_mode, original.execution_mode);
        assert_eq!(reloaded.triggers, original.triggers);
        assert_eq!(reloaded.cooldown_secs, original.cooldown_secs);
        assert_eq!(reloaded.max_concurrent, original.max_concurrent);
        assert_eq!(reloaded.deterministic, original.deterministic);
        assert_eq!(reloaded.steps, original.steps);
    }

    #[test]
    fn save_sop_rejects_empty_name() {
        let tmp = tempfile::tempdir().unwrap();
        let mut sop = Sop {
            name: String::new(),
            description: "d".to_string(),
            version: "1.0.0".to_string(),
            priority: SopPriority::Normal,
            execution_mode: SopExecutionMode::Supervised,
            triggers: Vec::new(),
            steps: Vec::new(),
            cooldown_secs: 0,
            max_concurrent: 1,
            location: None,
            deterministic: false,
        };
        assert!(save_sop(tmp.path(), &sop).is_err());
        sop.name = "ok".to_string();
        assert!(save_sop(tmp.path(), &sop).is_ok());
    }

    fn minimal_sop(name: &str, steps: Vec<SopStep>) -> Sop {
        Sop {
            name: name.to_string(),
            description: "d".to_string(),
            version: "1.0.0".to_string(),
            priority: SopPriority::Normal,
            execution_mode: SopExecutionMode::Supervised,
            triggers: Vec::new(),
            steps,
            cooldown_secs: 0,
            max_concurrent: 1,
            location: None,
            deterministic: false,
        }
    }

    #[test]
    fn strict_save_rejects_and_writes_nothing() {
        let tmp = tempfile::tempdir().unwrap();
        // Two steps sharing number 1 → duplicate (blocking).
        let dup = minimal_sop(
            "dupe",
            vec![
                SopStep {
                    number: 1,
                    title: "a".to_string(),
                    ..SopStep::default()
                },
                SopStep {
                    number: 1,
                    title: "b".to_string(),
                    ..SopStep::default()
                },
            ],
        );
        let result = save_sop(tmp.path(), &dup);
        assert!(result.is_err());
        assert!(
            !tmp.path().join("dupe").exists(),
            "strict save must write nothing on rejection"
        );
    }

    #[test]
    fn strict_save_reports_structured_blocking() {
        let v = validate_sop_strict(&minimal_sop(
            "",
            vec![SopStep {
                number: 1,
                title: String::new(),
                ..SopStep::default()
            }],
        ));
        assert!(!v.is_ok());
        assert!(v.blocking.iter().any(|b| b.contains("name is empty")));
        assert!(v.blocking.iter().any(|b| b.contains("empty title")));
    }

    #[test]
    fn warnings_do_not_block_save() {
        let tmp = tempfile::tempdir().unwrap();
        // No triggers, no steps → only warnings from validate_sop. Name set.
        let warn_only = minimal_sop("warns", Vec::new());
        let v = validate_sop_strict(&warn_only);
        assert!(v.is_ok(), "warnings alone must not block");
        assert!(!v.warnings.is_empty());
        assert!(save_sop(tmp.path(), &warn_only).is_ok());
        assert!(tmp.path().join("warns").exists());
    }

    #[test]
    fn load_is_lenient_for_imperfect_sop() {
        let tmp = tempfile::tempdir().unwrap();
        // Write a SOP with a dangling next ref directly, bypassing strict save.
        let dir = tmp.path().join("lenient");
        std::fs::create_dir_all(&dir).unwrap();
        std::fs::write(
            dir.join("SOP.toml"),
            "[sop]\nname = \"lenient\"\ndescription = \"d\"\n",
        )
        .unwrap();
        std::fs::write(
            dir.join("SOP.md"),
            "## Steps\n1. **only step**\n   - next: 99\n",
        )
        .unwrap();
        let loaded = load_sop(&dir, SopExecutionMode::Supervised);
        assert!(loaded.is_ok(), "load must be lenient and not reject");
        let v = validate_sop_strict(&loaded.unwrap());
        assert!(
            v.blocking.iter().any(|b| b.contains("does not exist")),
            "the dangling ref surfaces as a blocking diagnostic for repair"
        );
    }

    #[test]
    fn graph_error_folds_to_blocking_warning_folds_to_advisory() {
        // mismatch (warning) + required unsatisfied (error) in one SOP.
        let to_frag = |t: &str| serde_json::json!({"type": t});
        let producer = SopStep {
            number: 1,
            title: "p".to_string(),
            schema: Some(StepSchema {
                input: None,
                output: Some(to_frag("number")),
            }),
            ..SopStep::default()
        };
        let consumer = SopStep {
            number: 2,
            title: "c".to_string(),
            schema: Some(StepSchema {
                input: Some(to_frag("string")),
                output: None,
            }),
            ..SopStep::default()
        };
        let v = validate_sop_strict(&minimal_sop("folds", vec![producer, consumer]));
        assert!(v.blocking.iter().any(|b| b.contains("required input")));
        assert!(v.warnings.iter().any(|w| w.contains("mismatch")));
    }
}
