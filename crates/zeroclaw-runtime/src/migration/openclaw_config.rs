//! Import an OpenClaw install's configuration (`openclaw.json`) into this
//! ZeroClaw install: model providers, agents, MCP servers, the WhatsApp
//! channel, heartbeat cadence, and each agent's workspace bootstrap files.
//!
//! The import is additive. An alias that already exists in the ZeroClaw
//! config is never overwritten, and a workspace file that already exists is
//! never replaced. The OpenClaw install is only read.
//!
//! Trust-boundary settings are imported closed: the WhatsApp channel lands
//! disabled, heartbeat is not switched on, and agents can only delegate to the
//! peers OpenClaw's `tools.agentToAgent.send` named. OpenClaw tool
//! permissions have no one-to-one ZeroClaw equivalent and are reported for
//! manual review instead of being translated into a wider risk profile.

use anyhow::{Context, Result, bail};
use directories::UserDirs;
use serde_json::{Map, Value, json};
use std::collections::BTreeSet;
use std::fs;
use std::path::{Path, PathBuf};
use zeroclaw_config::alias_refs::{create_map_key_checked, is_reserved_agent_alias};
use zeroclaw_config::presets::{recommended_runtime_preset, risk_preset};
use zeroclaw_config::providers::ModelProviders;
use zeroclaw_config::schema::Config;

use crate::agent::system_prompt::{BOOTSTRAP_FILES, CONDITIONAL_BOOTSTRAP_FILES};

const OPENCLAW_CONFIG_FILE: &str = "openclaw.json";
/// Risk preset imported agents start on: ZeroClaw's recommended daily-driver
/// profile (supervised, workspace-scoped, sandboxed). OpenClaw's own tool
/// permissions are never translated into a wider profile.
const RISK_PRESET: &str = "balanced";
const MCP_BUNDLE_ALIAS: &str = "openclaw";
const WHATSAPP_ALIAS: &str = "default";
const WHATSAPP_PEER_GROUP: &str = "whatsapp_default";
const MAX_ALIAS_LEN: usize = 63;
/// Workspace subdirectories copied next to the bootstrap files. `skills`
/// uses the same `<name>/SKILL.md` layout in both runtimes.
const WORKSPACE_DIRS: &[&str] = &["skills"];
/// Extra top-level workspace files OpenClaw agents rely on that ZeroClaw
/// reads on demand rather than injecting.
const EXTRA_WORKSPACE_FILES: &[&str] = &["HEARTBEAT.md"];

fn t(key: &str) -> String {
    crate::i18n::get_required_cli_string(key)
}

fn ta(key: &str, args: &[(&str, &str)]) -> String {
    crate::i18n::get_required_cli_string_with_args(key, args)
}

/// One file to copy into an agent workspace.
#[derive(Debug, Clone, PartialEq, Eq)]
struct CopyOp {
    src: PathBuf,
    dst: PathBuf,
}

/// One file to generate in an agent workspace when OpenClaw kept the
/// equivalent fact in `openclaw.json` rather than on disk.
#[derive(Debug, Clone, PartialEq, Eq)]
struct WriteOp {
    dst: PathBuf,
    content: String,
}

#[derive(Debug, Default)]
struct Report {
    imported: Vec<String>,
    skipped: Vec<String>,
    manual: Vec<String>,
}

/// Everything the import would change, computed without touching disk.
#[derive(Debug)]
struct ImportPlan {
    config: Config,
    copies: Vec<CopyOp>,
    writes: Vec<WriteOp>,
    report: Report,
}

impl ImportPlan {
    fn changes_config(&self) -> bool {
        !self.report.imported.is_empty()
    }
}

pub async fn migrate_openclaw_config(
    config: &Config,
    source: Option<PathBuf>,
    dry_run: bool,
) -> Result<()> {
    let home = resolve_openclaw_home(source)?;
    let json_path = home.join(OPENCLAW_CONFIG_FILE);
    if !json_path.is_file() {
        bail!(
            "{}",
            ta(
                "cli-migrate-openclaw-config-source-missing",
                &[("path", &json_path.display().to_string())]
            )
        );
    }
    let raw = fs::read_to_string(&json_path)
        .with_context(|| format!("Failed to read {}", json_path.display()))?;
    let openclaw: Value = serde_json::from_str(&raw).with_context(|| {
        ta(
            "cli-migrate-openclaw-config-parse-failed",
            &[("path", &json_path.display().to_string())],
        )
    })?;

    let plan = plan_import(&openclaw, &home, config)?;
    plan.config
        .validate()
        .context("Imported OpenClaw configuration does not validate")?;

    let pending_files = plan
        .copies
        .iter()
        .map(|op| &op.dst)
        .chain(plan.writes.iter().map(|op| &op.dst))
        .filter(|dst| !dst.exists())
        .count();

    if dry_run {
        println!("{}", t("cli-migrate-openclaw-config-dry-run-header"));
        print_report(&plan.report);
        println!(
            "{}",
            ta(
                "cli-migrate-openclaw-config-files-pending",
                &[("count", &pending_files.to_string())]
            )
        );
        println!("{}", t("cli-migrate-openclaw-config-dry-run-footer"));
        return Ok(());
    }

    // Files first: if a copy fails, the config is untouched and a rerun
    // repeats the whole import (existing files are kept). Agents that already
    // exist in the config are skipped on a rerun, so the reverse order could
    // strand a half-copied workspace.
    let (copied, kept) = apply_file_ops(&plan.copies, &plan.writes)?;

    if plan.changes_config() {
        if let Some(backup) = backup_config_file(&config.config_path)? {
            println!(
                "{}",
                ta(
                    "cli-migrate-openclaw-config-backup",
                    &[("path", &backup.display().to_string())]
                )
            );
        }
        plan.config.save().await?;
    }

    println!("{}", t("cli-migrate-openclaw-config-done-header"));
    print_report(&plan.report);
    println!(
        "{}",
        ta(
            "cli-migrate-openclaw-config-files-done",
            &[("copied", &copied.to_string()), ("kept", &kept.to_string())]
        )
    );
    Ok(())
}

fn print_report(report: &Report) {
    for (heading, lines) in [
        (
            "cli-migrate-openclaw-config-section-imported",
            &report.imported,
        ),
        (
            "cli-migrate-openclaw-config-section-skipped",
            &report.skipped,
        ),
        ("cli-migrate-openclaw-config-section-manual", &report.manual),
    ] {
        if lines.is_empty() {
            continue;
        }
        println!();
        println!("{}", t(heading));
        for line in lines {
            println!("  - {line}");
        }
    }
    println!();
}

fn resolve_openclaw_home(source: Option<PathBuf>) -> Result<PathBuf> {
    if let Some(src) = source {
        // Accept either the OpenClaw home or the `openclaw.json` inside it.
        if src.file_name().is_some_and(|n| n == OPENCLAW_CONFIG_FILE)
            && let Some(parent) = src.parent()
        {
            return Ok(parent.to_path_buf());
        }
        return Ok(src);
    }
    let home = UserDirs::new()
        .map(|u| u.home_dir().to_path_buf())
        .context("Could not find home directory")?;
    Ok(home.join(".openclaw"))
}

fn backup_config_file(config_path: &Path) -> Result<Option<PathBuf>> {
    if !config_path.is_file() {
        return Ok(None);
    }
    let stamp = chrono::Local::now().format("%Y%m%d-%H%M%S");
    let mut name = config_path
        .file_name()
        .map(|n| n.to_os_string())
        .unwrap_or_default();
    name.push(format!(".bak-openclaw-{stamp}"));
    let backup = config_path.with_file_name(name);
    fs::copy(config_path, &backup)
        .with_context(|| format!("Failed to back up {}", config_path.display()))?;
    Ok(Some(backup))
}

/// Apply the planned file operations. Existing destinations are kept as-is.
/// Returns `(written, kept)`.
fn apply_file_ops(copies: &[CopyOp], writes: &[WriteOp]) -> Result<(usize, usize)> {
    let mut written = 0usize;
    let mut kept = 0usize;
    for op in copies {
        if op.dst.exists() {
            kept += 1;
            continue;
        }
        if let Some(parent) = op.dst.parent() {
            fs::create_dir_all(parent)
                .with_context(|| format!("Failed to create {}", parent.display()))?;
        }
        fs::copy(&op.src, &op.dst).with_context(|| {
            format!(
                "Failed to copy {} to {}",
                op.src.display(),
                op.dst.display()
            )
        })?;
        written += 1;
    }
    for op in writes {
        if op.dst.exists() {
            kept += 1;
            continue;
        }
        if let Some(parent) = op.dst.parent() {
            fs::create_dir_all(parent)
                .with_context(|| format!("Failed to create {}", parent.display()))?;
        }
        fs::write(&op.dst, &op.content)
            .with_context(|| format!("Failed to write {}", op.dst.display()))?;
        written += 1;
    }
    Ok((written, kept))
}

// ── Planning ─────────────────────────────────────────────────────

fn plan_import(openclaw: &Value, home: &Path, current: &Config) -> Result<ImportPlan> {
    let mut plan = ImportPlan {
        config: current.clone(),
        copies: Vec::new(),
        writes: Vec::new(),
        report: Report::default(),
    };

    let mcp_servers = import_mcp_servers(openclaw, &mut plan)?;
    let agents = import_agents(openclaw, home, &mcp_servers, &mut plan)?;
    import_delegates(openclaw, &agents, &mut plan)?;
    import_whatsapp(openclaw, &agents, &mut plan)?;
    import_heartbeat(openclaw, &agents, &mut plan);
    note_unmapped_sections(openclaw, &mut plan.report);

    Ok(plan)
}

/// An OpenClaw agent that was imported in this run.
#[derive(Debug, Clone)]
struct ImportedAgent {
    openclaw_id: String,
    alias: String,
}

fn openclaw_agent_entries(openclaw: &Value) -> Vec<(String, Value)> {
    let agents = &openclaw["agents"];
    if let Some(entries) = agents["entries"].as_object() {
        let mut out: Vec<(String, Value)> = entries
            .iter()
            .map(|(id, entry)| (id.clone(), entry.clone()))
            .collect();
        out.sort_by(|a, b| a.0.cmp(&b.0));
        return out;
    }
    // Older OpenClaw layout: `agents.list = [{ id, ... }]`.
    agents["list"]
        .as_array()
        .map(|list| {
            list.iter()
                .filter_map(|entry| {
                    entry["id"]
                        .as_str()
                        .map(|id| (id.to_string(), entry.clone()))
                })
                .collect()
        })
        .unwrap_or_default()
}

fn import_agents(
    openclaw: &Value,
    home: &Path,
    mcp_servers: &[String],
    plan: &mut ImportPlan,
) -> Result<Vec<ImportedAgent>> {
    struct Candidate {
        id: String,
        entry: Value,
        alias: String,
        primary: ModelTarget,
        fallbacks: Vec<ModelTarget>,
    }

    let defaults_model = &openclaw["agents"]["defaults"]["model"];
    let mut candidates = Vec::new();
    for (id, entry) in openclaw_agent_entries(openclaw) {
        let alias = slug(&id);
        if is_reserved_agent_alias(&alias) {
            plan.report.skipped.push(ta(
                "cli-migrate-openclaw-config-agent-reserved",
                &[("alias", &alias)],
            ));
            continue;
        }
        if plan.config.agents.contains_key(&alias)
            || candidates.iter().any(|c: &Candidate| c.alias == alias)
        {
            plan.report.skipped.push(ta(
                "cli-migrate-openclaw-config-agent-exists",
                &[("alias", &alias)],
            ));
            continue;
        }

        let model_cfg = if entry["model"].is_null() {
            defaults_model
        } else {
            &entry["model"]
        };
        let (primary, fallback_models) = model_selection(model_cfg);
        let Some(primary) = primary else {
            plan.report.skipped.push(ta(
                "cli-migrate-openclaw-config-agent-no-model",
                &[("alias", &alias)],
            ));
            continue;
        };
        let Some(primary) = resolve_model(openclaw, &primary) else {
            plan.report.skipped.push(ta(
                "cli-migrate-openclaw-config-agent-unknown-provider",
                &[("alias", &alias), ("model", &primary)],
            ));
            continue;
        };
        let mut fallbacks = Vec::new();
        for model_ref in &fallback_models {
            match resolve_model(openclaw, model_ref) {
                Some(target) => fallbacks.push(target),
                None => plan.report.skipped.push(ta(
                    "cli-migrate-openclaw-config-fallback-unknown-provider",
                    &[("alias", &alias), ("model", model_ref)],
                )),
            }
        }
        candidates.push(Candidate {
            id,
            entry,
            alias,
            primary,
            fallbacks,
        });
    }

    let mut auth = AuthNotes::default();

    // Pass 1: one provider entry per distinct primary model, carrying its
    // fallback chain. Primaries go first so a model that is one agent's
    // primary and another agent's fallback keeps a single shared entry.
    let mut provider_refs = Vec::with_capacity(candidates.len());
    for candidate in &candidates {
        let fallback_refs: Vec<String> = candidate
            .fallbacks
            .iter()
            .map(ModelTarget::leaf_ref)
            .collect();
        provider_refs.push(ensure_primary_provider(
            &candidate.primary,
            &fallback_refs,
            &candidate.alias,
            plan,
            &mut auth,
        )?);
    }

    // Pass 2: bare entries for models that only ever appear as a fallback.
    for target in candidates.iter().flat_map(|c| &c.fallbacks) {
        let provider_ref = target.leaf_ref();
        let inserted = insert_model_provider(
            &mut plan.config,
            &target.family,
            &slug(&target.model),
            target.entry.clone(),
        )?;
        if inserted {
            auth.observe(target, &provider_ref);
        }
        report_provider(plan, &provider_ref, &target.model, inserted);
    }

    let mut extras = AgentExtras::default();
    let mut imported = Vec::with_capacity(candidates.len());
    let profiles = if candidates.is_empty() {
        None
    } else {
        Some(ensure_profiles(plan)?)
    };
    for (candidate, provider_ref) in candidates.into_iter().zip(provider_refs) {
        let Candidate {
            id, entry, alias, ..
        } = candidate;

        let Some((risk_profile, runtime_profile)) = &profiles else {
            break;
        };
        // Same guarded create path the CLI, RPC, and Quickstart surfaces use.
        create_map_key_checked(&mut plan.config, "agents", &alias)
            .with_context(|| format!("Failed to create agent `{alias}`"))?;
        let agent = plan
            .config
            .agents
            .get_mut(&alias)
            .with_context(|| format!("Agent `{alias}` was not created"))?;
        agent.enabled = true;
        agent.model_provider = provider_ref.as_str().into();
        agent.risk_profile = risk_profile.as_str().into();
        agent.runtime_profile = runtime_profile.as_str().into();
        if !mcp_servers.is_empty() {
            agent.mcp_bundles = vec![MCP_BUNDLE_ALIAS.to_string()];
        }
        // Closed by default: only the peers OpenClaw's
        // `tools.agentToAgent.send` named become delegates.
        agent.delegate_same_risk_profile = false;

        let display = entry["identity"]["name"]
            .as_str()
            .or_else(|| entry["name"].as_str())
            .unwrap_or(&id);
        plan.report.imported.push(ta(
            "cli-migrate-openclaw-config-agent",
            &[
                ("alias", &alias),
                ("name", display),
                ("provider", &provider_ref),
            ],
        ));

        plan_workspace_files(&id, &entry, home, &alias, plan)?;
        extras.observe(&entry, &alias);

        imported.push(ImportedAgent {
            openclaw_id: id,
            alias,
        });
    }

    if let Some((risk_profile, _)) = &profiles {
        extras.flush(risk_profile, &mut plan.report);
    }
    auth.flush(&mut plan.report);
    Ok(imported)
}

/// Make sure the profiles imported agents bind to exist, using the same
/// preset tables Quickstart installs from. An existing block with the preset's
/// name is the operator's and wins. Returns `(risk_profile, runtime_profile)`.
fn ensure_profiles(plan: &mut ImportPlan) -> Result<(String, String)> {
    let risk = risk_preset(RISK_PRESET)
        .with_context(|| format!("Risk preset `{RISK_PRESET}` is not defined"))?;
    let runtime = recommended_runtime_preset(None).context("No runtime preset is defined")?;

    let config = &mut plan.config;
    let mut created = false;
    if !config.risk_profiles.contains_key(risk.preset_name) {
        config
            .risk_profiles
            .insert(risk.preset_name.to_string(), (risk.values)());
        created = true;
    }
    if !config.runtime_profiles.contains_key(runtime.preset_name) {
        config
            .runtime_profiles
            .insert(runtime.preset_name.to_string(), (runtime.values)());
        created = true;
    }
    if created {
        plan.report.imported.push(ta(
            "cli-migrate-openclaw-config-profiles",
            &[("risk", risk.preset_name), ("runtime", runtime.preset_name)],
        ));
    }
    Ok((
        risk.preset_name.to_string(),
        runtime.preset_name.to_string(),
    ))
}

/// `model` is either `"provider/model"` or `{ primary, fallbacks }`.
fn model_selection(model_cfg: &Value) -> (Option<String>, Vec<String>) {
    if let Some(primary) = model_cfg.as_str() {
        return (Some(primary.to_string()), Vec::new());
    }
    let primary = model_cfg["primary"].as_str().map(str::to_string);
    let fallbacks = model_cfg["fallbacks"]
        .as_array()
        .map(|list| {
            list.iter()
                .filter_map(|v| v.as_str().map(str::to_string))
                .collect()
        })
        .unwrap_or_default();
    (primary, fallbacks)
}

/// A resolved OpenClaw `provider/model` reference.
#[derive(Debug, Clone)]
struct ModelTarget {
    family: String,
    model: String,
    /// Provider entry fields, without `fallback`.
    entry: Map<String, Value>,
    /// The OpenClaw provider carried a non-literal `apiKey` (secret ref / env).
    indirect_key: bool,
}

impl ModelTarget {
    fn leaf_ref(&self) -> String {
        format!("{}.{}", self.family, slug(&self.model))
    }
}

fn resolve_model(openclaw: &Value, model_ref: &str) -> Option<ModelTarget> {
    let (provider, model) = model_ref.split_once('/')?;
    if provider.is_empty() || model.is_empty() {
        return None;
    }
    let def = &openclaw["models"]["providers"][provider];
    let known = ModelProviders::slot_names();

    let named = match provider {
        "openai-codex" => "openai".to_string(),
        "google" => "gemini".to_string(),
        other => other.replace('-', "_"),
    };
    let family = if def["api"].as_str() == Some("ollama") {
        "ollama".to_string()
    } else if known.contains(&named.as_str()) {
        named
    } else if def["baseUrl"].is_string() {
        // An OpenClaw custom provider: OpenAI-compatible endpoint by URL.
        "custom".to_string()
    } else {
        return None;
    };

    let mut entry = Map::new();
    entry.insert("model".into(), json!(model));
    if let Some(uri) = def["baseUrl"].as_str() {
        entry.insert("uri".into(), json!(uri));
    }
    let mut indirect_key = false;
    match &def["apiKey"] {
        Value::Null => {}
        // Ollama takes no credential; OpenClaw stores a placeholder there.
        Value::String(key) if family != "ollama" && !key.is_empty() && !key.contains("${") => {
            entry.insert("api_key".into(), json!(key));
        }
        Value::String(_) if family == "ollama" => {}
        _ => indirect_key = true,
    }
    if let Some(secs) = def["timeoutSeconds"].as_u64() {
        entry.insert("timeout_secs".into(), json!(secs));
    }

    let model_def = def["models"]
        .as_array()
        .and_then(|models| models.iter().find(|m| m["id"].as_str() == Some(model)))
        .cloned()
        .unwrap_or(Value::Null);
    if let Some(max) = model_def["maxTokens"]
        .as_u64()
        .or_else(|| def["maxTokens"].as_u64())
    {
        entry.insert("max_tokens".into(), json!(max));
    }
    if let Some(window) = model_def["contextWindow"]
        .as_u64()
        .or_else(|| model_def["contextTokens"].as_u64())
    {
        entry.insert("context_window".into(), json!(window));
    }
    if family == "ollama" {
        if let Some(num_ctx) = model_def["params"]["num_ctx"].as_u64() {
            entry.insert("num_ctx".into(), json!(num_ctx));
        }
        if let Some(think) = model_def["params"]["thinking"].as_bool() {
            entry.insert("think".into(), json!(think));
        }
    }

    // OpenClaw reaches OpenAI models through the Codex subscription login
    // when no API key is configured for the provider.
    if family == "openai"
        && !entry.contains_key("api_key")
        && !indirect_key
        && (provider == "openai-codex"
            || openclaw["plugins"]["entries"]["codex"]["enabled"].as_bool() == Some(true))
    {
        entry.insert("requires_openai_auth".into(), json!(true));
    }

    Some(ModelTarget {
        family,
        model: model.to_string(),
        entry,
        indirect_key,
    })
}

#[derive(Debug, Default)]
struct AuthNotes {
    anthropic: BTreeSet<String>,
    codex: bool,
    indirect: BTreeSet<String>,
}

impl AuthNotes {
    fn observe(&mut self, target: &ModelTarget, provider_ref: &str) {
        if target.indirect_key {
            self.indirect.insert(provider_ref.to_string());
        } else if target.family == "anthropic" && !target.entry.contains_key("api_key") {
            self.anthropic.insert(provider_ref.to_string());
        }
        if target.entry.get("requires_openai_auth") == Some(&json!(true)) {
            self.codex = true;
        }
    }

    fn flush(self, report: &mut Report) {
        for provider_ref in &self.anthropic {
            report.manual.push(ta(
                "cli-migrate-openclaw-config-note-anthropic-auth",
                &[("provider", provider_ref)],
            ));
        }
        if self.codex {
            report
                .manual
                .push(t("cli-migrate-openclaw-config-note-codex-auth"));
        }
        for provider_ref in &self.indirect {
            report.manual.push(ta(
                "cli-migrate-openclaw-config-note-secret-ref",
                &[("provider", provider_ref)],
            ));
        }
    }
}

/// Insert `[providers.models.<family>.<alias>]` unless it already exists.
/// Returns whether a new entry was written.
fn insert_model_provider(
    config: &mut Config,
    family: &str,
    alias: &str,
    entry: Map<String, Value>,
) -> Result<bool> {
    let provider_ref = format!("{family}.{alias}");
    if config
        .providers
        .models
        .find_by_name(&provider_ref)
        .is_some()
    {
        return Ok(false);
    }
    // Round-trip through the container's own serde shape so every provider
    // family is handled by its typed config without a per-family match here.
    let mut all = serde_json::to_value(&config.providers.models)
        .context("Failed to serialize model providers")?;
    let slots = all
        .as_object_mut()
        .context("Model providers did not serialize to a table")?;
    let slot = slots
        .entry(family.to_string())
        .or_insert_with(|| json!({}))
        .as_object_mut()
        .with_context(|| format!("providers.models.{family} is not a table"))?;
    slot.insert(alias.to_string(), Value::Object(entry));
    let rebuilt: ModelProviders = serde_json::from_value(all)
        .with_context(|| format!("Failed to build providers.models.{provider_ref}"))?;
    if rebuilt.find_by_name(&provider_ref).is_none() {
        bail!("providers.models.{provider_ref} was not accepted by the config schema");
    }
    config.providers.models = rebuilt;
    Ok(true)
}

fn report_provider(plan: &mut ImportPlan, provider_ref: &str, model: &str, inserted: bool) {
    if inserted {
        plan.report.imported.push(ta(
            "cli-migrate-openclaw-config-provider",
            &[("provider", provider_ref), ("model", model)],
        ));
    }
}

/// Resolve the provider entry an agent points at: `<family>.<model>` shared by
/// every agent with the same model and fallback chain, or
/// `<family>.<model>_<agent>` when that alias already carries another chain.
fn ensure_primary_provider(
    target: &ModelTarget,
    fallback_refs: &[String],
    agent_alias: &str,
    plan: &mut ImportPlan,
    auth: &mut AuthNotes,
) -> Result<String> {
    let shared_alias = slug(&target.model);
    let shared_ref = format!("{}.{shared_alias}", target.family);
    // A fallback pointing back at the entry itself would be a one-hop cycle.
    let chain: Vec<&String> = fallback_refs.iter().filter(|r| **r != shared_ref).collect();

    let existing_chain =
        plan.config
            .providers
            .models
            .find_by_name(&shared_ref)
            .map(|(_, _, cfg)| {
                cfg.fallback
                    .iter()
                    .map(|r| r.as_str().to_string())
                    .collect::<Vec<_>>()
            });

    let mut entry = target.entry.clone();
    if !chain.is_empty() {
        entry.insert("fallback".into(), json!(chain));
    }

    match existing_chain {
        Some(existing) if existing.iter().eq(chain.iter().copied()) => Ok(shared_ref),
        // The alias already carries a different chain, from another agent in
        // this run or from the operator; an agent-scoped entry keeps both.
        Some(_) => {
            let scoped_alias = slug(&format!("{shared_alias}_{agent_alias}"));
            let scoped_ref = format!("{}.{scoped_alias}", target.family);
            let inserted =
                insert_model_provider(&mut plan.config, &target.family, &scoped_alias, entry)?;
            if inserted {
                auth.observe(target, &scoped_ref);
            }
            report_provider(plan, &scoped_ref, &target.model, inserted);
            Ok(scoped_ref)
        }
        None => {
            let inserted =
                insert_model_provider(&mut plan.config, &target.family, &shared_alias, entry)?;
            if inserted {
                auth.observe(target, &shared_ref);
            }
            report_provider(plan, &shared_ref, &target.model, inserted);
            Ok(shared_ref)
        }
    }
}

fn openclaw_workspace_dir(id: &str, entry: &Value, home: &Path) -> PathBuf {
    if let Some(path) = entry["workspace"].as_str() {
        return PathBuf::from(path);
    }
    if id == "main" {
        home.join("workspace")
    } else {
        home.join(format!("workspace-{id}"))
    }
}

fn plan_workspace_files(
    id: &str,
    entry: &Value,
    home: &Path,
    alias: &str,
    plan: &mut ImportPlan,
) -> Result<()> {
    let src_root = openclaw_workspace_dir(id, entry, home);
    let dst_root = plan.config.agent_workspace_dir(alias);

    let top_level = BOOTSTRAP_FILES
        .iter()
        .chain(CONDITIONAL_BOOTSTRAP_FILES)
        .chain(EXTRA_WORKSPACE_FILES);
    let mut has_identity = false;
    for name in top_level {
        let src = src_root.join(name);
        if is_regular_file(&src) {
            has_identity |= *name == "IDENTITY.md";
            plan.copies.push(CopyOp {
                src,
                dst: dst_root.join(name),
            });
        }
    }
    for dir in WORKSPACE_DIRS {
        collect_tree(&src_root.join(dir), &dst_root.join(dir), &mut plan.copies)?;
    }

    // OpenClaw can keep an agent's identity only in `openclaw.json`; ZeroClaw
    // reads it from the workspace, so materialize it there once.
    if !has_identity && let Some(content) = identity_markdown(&entry["identity"]) {
        plan.writes.push(WriteOp {
            dst: dst_root.join("IDENTITY.md"),
            content,
        });
    }
    Ok(())
}

fn identity_markdown(identity: &Value) -> Option<String> {
    let name = identity["name"].as_str()?;
    let mut out = format!("# IDENTITY.md\n\n- **Name:** {name}\n");
    if let Some(emoji) = identity["emoji"].as_str() {
        out.push_str(&format!("- **Emoji:** {emoji}\n"));
    }
    if let Some(theme) = identity["theme"].as_str() {
        out.push_str(&format!("- **Vibe:** {theme}\n"));
    }
    Some(out)
}

fn is_regular_file(path: &Path) -> bool {
    fs::symlink_metadata(path).is_ok_and(|meta| meta.file_type().is_file())
}

/// Collect regular files under `src` recursively. Symlinks and dot-entries
/// (`.git`, `.DS_Store`, …) are not followed or copied.
fn collect_tree(src: &Path, dst: &Path, out: &mut Vec<CopyOp>) -> Result<()> {
    let Ok(meta) = fs::symlink_metadata(src) else {
        return Ok(());
    };
    if !meta.file_type().is_dir() {
        return Ok(());
    }
    let mut entries: Vec<_> = fs::read_dir(src)
        .with_context(|| format!("Failed to read {}", src.display()))?
        .collect::<std::io::Result<Vec<_>>>()
        .with_context(|| format!("Failed to read {}", src.display()))?;
    entries.sort_by_key(|e| e.file_name());
    for entry in entries {
        let name = entry.file_name();
        if name.to_string_lossy().starts_with('.') {
            continue;
        }
        let file_type = entry
            .file_type()
            .with_context(|| format!("Failed to stat {}", entry.path().display()))?;
        if file_type.is_dir() {
            collect_tree(&entry.path(), &dst.join(&name), out)?;
        } else if file_type.is_file() {
            out.push(CopyOp {
                src: entry.path(),
                dst: dst.join(&name),
            });
        }
    }
    Ok(())
}

/// OpenClaw per-agent settings with no ZeroClaw equivalent, grouped so the
/// report names each kind once instead of once per agent.
#[derive(Debug, Default)]
struct AgentExtras {
    tools: Vec<String>,
    model_params: Vec<String>,
    memory_paths: Vec<String>,
}

impl AgentExtras {
    fn observe(&mut self, entry: &Value, alias: &str) {
        if entry["tools"].is_object() {
            self.tools.push(alias.to_string());
        }
        if entry["models"].is_object() {
            self.model_params.push(alias.to_string());
        }
        if entry["memory"]["search"]["extraPaths"].is_array() {
            self.memory_paths.push(alias.to_string());
        }
    }

    fn flush(self, risk_profile: &str, report: &mut Report) {
        for (key, aliases) in [
            ("cli-migrate-openclaw-config-note-agent-tools", self.tools),
            (
                "cli-migrate-openclaw-config-note-agent-model-params",
                self.model_params,
            ),
            (
                "cli-migrate-openclaw-config-note-agent-memory-paths",
                self.memory_paths,
            ),
        ] {
            if !aliases.is_empty() {
                report.manual.push(ta(
                    key,
                    &[("aliases", &aliases.join(", ")), ("profile", risk_profile)],
                ));
            }
        }
    }
}

fn import_delegates(
    openclaw: &Value,
    agents: &[ImportedAgent],
    plan: &mut ImportPlan,
) -> Result<()> {
    let entries = openclaw_agent_entries(openclaw);
    for agent in agents {
        let Some((_, entry)) = entries.iter().find(|(id, _)| *id == agent.openclaw_id) else {
            continue;
        };
        let Some(send) = entry["tools"]["agentToAgent"]["send"].as_array() else {
            continue;
        };
        let mut targets: Vec<String> = Vec::new();
        for peer in send.iter().filter_map(Value::as_str) {
            let peer_alias = slug(peer);
            if peer_alias == agent.alias || targets.contains(&peer_alias) {
                continue;
            }
            if plan.config.agents.contains_key(&peer_alias) {
                targets.push(peer_alias);
            } else {
                plan.report.skipped.push(ta(
                    "cli-migrate-openclaw-config-delegate-missing",
                    &[("alias", &agent.alias), ("peer", peer)],
                ));
            }
        }
        if targets.is_empty() {
            continue;
        }
        let delegates = serde_json::from_value(json!(targets))
            .with_context(|| format!("Failed to build delegates for `{}`", agent.alias))?;
        if let Some(cfg) = plan.config.agents.get_mut(&agent.alias) {
            cfg.delegates = delegates;
            plan.report.imported.push(ta(
                "cli-migrate-openclaw-config-delegates",
                &[("alias", &agent.alias), ("peers", &targets.join(", "))],
            ));
        }
    }
    Ok(())
}

fn import_mcp_servers(openclaw: &Value, plan: &mut ImportPlan) -> Result<Vec<String>> {
    let Some(servers) = openclaw["mcp"]["servers"].as_object() else {
        return Ok(Vec::new());
    };
    let mut names: Vec<&String> = servers.keys().collect();
    names.sort();

    let mut granted = Vec::new();
    for name in names {
        let server = &servers[name];
        if server["enabled"].as_bool() == Some(false) {
            plan.report.skipped.push(ta(
                "cli-migrate-openclaw-config-mcp-disabled",
                &[("name", name)],
            ));
            continue;
        }
        if plan.config.mcp.servers.iter().any(|s| &s.name == name) {
            plan.report.skipped.push(ta(
                "cli-migrate-openclaw-config-mcp-exists",
                &[("name", name)],
            ));
            granted.push(name.clone());
            continue;
        }

        let mut entry = Map::new();
        entry.insert("name".into(), json!(name));
        if let Some(command) = server["command"].as_str() {
            entry.insert("transport".into(), json!("stdio"));
            entry.insert("command".into(), json!(command));
            if server["args"].is_array() {
                entry.insert("args".into(), server["args"].clone());
            }
            if server["env"].is_object() {
                entry.insert("env".into(), server["env"].clone());
            }
        } else if let Some(url) = server["url"].as_str() {
            let transport = match server["transport"].as_str() {
                Some("sse") => "sse",
                _ => "http",
            };
            entry.insert("transport".into(), json!(transport));
            entry.insert("url".into(), json!(url));
            if server["headers"].is_object() {
                entry.insert("headers".into(), server["headers"].clone());
            }
        } else {
            plan.report.skipped.push(ta(
                "cli-migrate-openclaw-config-mcp-unsupported",
                &[("name", name)],
            ));
            continue;
        }
        if let Some(ms) = server["requestTimeoutMs"].as_u64() {
            entry.insert("tool_timeout_secs".into(), json!(ms.div_ceil(1000).max(1)));
        }

        let parsed = serde_json::from_value(Value::Object(entry))
            .with_context(|| format!("Failed to build MCP server `{name}`"))?;
        plan.config.mcp.servers.push(parsed);
        plan.report
            .imported
            .push(ta("cli-migrate-openclaw-config-mcp", &[("name", name)]));
        if server["cwd"].is_string() {
            plan.report.manual.push(ta(
                "cli-migrate-openclaw-config-note-mcp-cwd",
                &[("name", name)],
            ));
        }
        granted.push(name.clone());
    }

    if granted.is_empty() {
        return Ok(granted);
    }
    // OpenClaw exposes its MCP servers to every agent. ZeroClaw grants them
    // per agent through a bundle; an existing `openclaw` bundle is left as the
    // operator configured it.
    if !plan.config.mcp_bundles.contains_key(MCP_BUNDLE_ALIAS) {
        let bundle = serde_json::from_value(json!({ "servers": granted }))
            .context("Failed to build MCP bundle")?;
        plan.config
            .mcp_bundles
            .insert(MCP_BUNDLE_ALIAS.to_string(), bundle);
        plan.report.imported.push(ta(
            "cli-migrate-openclaw-config-mcp-bundle",
            &[
                ("bundle", MCP_BUNDLE_ALIAS),
                ("servers", &granted.join(", ")),
            ],
        ));
    }
    Ok(granted)
}

fn whatsapp_policy(raw: Option<&str>) -> &'static str {
    match raw {
        Some("open") => "all",
        Some("disabled") => "ignore",
        // `allowlist`, `pairing`, and anything unrecognized stay closed.
        _ => "allowlist",
    }
}

fn routing_agent(openclaw: &Value, agents: &[ImportedAgent]) -> Option<String> {
    let preferred = [
        openclaw["talk"]["agentId"].as_str(),
        openclaw["agents"]["defaults"]["systemAgent"]["agentId"].as_str(),
        Some("main"),
    ];
    preferred
        .into_iter()
        .flatten()
        .find_map(|id| agents.iter().find(|a| a.openclaw_id == id))
        .or_else(|| agents.first())
        .map(|a| a.alias.clone())
}

fn import_whatsapp(
    openclaw: &Value,
    agents: &[ImportedAgent],
    plan: &mut ImportPlan,
) -> Result<()> {
    let wa = &openclaw["channels"]["whatsapp"];
    if !wa.is_object() {
        return Ok(());
    }
    if plan.config.channels.whatsapp.contains_key(WHATSAPP_ALIAS) {
        plan.report.skipped.push(ta(
            "cli-migrate-openclaw-config-whatsapp-exists",
            &[("alias", WHATSAPP_ALIAS)],
        ));
        return Ok(());
    }
    let Some(agent_alias) = routing_agent(openclaw, agents) else {
        plan.report
            .skipped
            .push(t("cli-migrate-openclaw-config-whatsapp-no-agent"));
        return Ok(());
    };

    let session_path =
        crate::quickstart::default_whatsapp_web_session_path(&plan.config, WHATSAPP_ALIAS);
    let self_chat = wa["selfChatMode"].as_bool() == Some(true);
    let channel = serde_json::from_value(json!({
        // The OpenClaw linked-device session cannot be reused, and two
        // runtimes answering the same number would double-reply. The channel
        // is imported disabled; enabling it starts a fresh pairing.
        "enabled": false,
        "session_path": session_path,
        "mode": if self_chat { "personal" } else { "business" },
        "dm_policy": whatsapp_policy(wa["dmPolicy"].as_str()),
        "group_policy": whatsapp_policy(wa["groupPolicy"].as_str()),
        "self_chat_mode": self_chat,
    }))
    .context("Failed to build WhatsApp channel")?;
    plan.config
        .channels
        .whatsapp
        .insert(WHATSAPP_ALIAS.to_string(), channel);

    let channel_ref = format!("whatsapp.{WHATSAPP_ALIAS}");
    if let Some(agent) = plan.config.agents.get_mut(&agent_alias) {
        agent.channels.push(channel_ref.as_str().into());
    }
    plan.report.imported.push(ta(
        "cli-migrate-openclaw-config-whatsapp",
        &[("channel", &channel_ref), ("agent", &agent_alias)],
    ));
    plan.report.manual.push(ta(
        "cli-migrate-openclaw-config-note-whatsapp-enable",
        &[("channel", &channel_ref)],
    ));

    let mut peers: Vec<String> = Vec::new();
    for key in ["allowFrom", "groupAllowFrom"] {
        for peer in wa[key]
            .as_array()
            .into_iter()
            .flatten()
            .filter_map(Value::as_str)
        {
            // `*` is OpenClaw's allow-everyone wildcard; the policy fields
            // above already carry that decision.
            if peer != "*" && !peers.iter().any(|p| p == peer) {
                peers.push(peer.to_string());
            }
        }
    }
    if !peers.is_empty() && !plan.config.peer_groups.contains_key(WHATSAPP_PEER_GROUP) {
        let group = serde_json::from_value(json!({
            "channel": channel_ref,
            "agents": [agent_alias],
            "external_peers": peers,
        }))
        .context("Failed to build WhatsApp peer group")?;
        plan.config
            .peer_groups
            .insert(WHATSAPP_PEER_GROUP.to_string(), group);
        plan.report.imported.push(ta(
            "cli-migrate-openclaw-config-whatsapp-peers",
            &[
                ("group", WHATSAPP_PEER_GROUP),
                ("count", &peers.len().to_string()),
            ],
        ));
    }
    Ok(())
}

/// Parse an OpenClaw duration (`"30m"`, `"1h"`, `"90s"`) into whole minutes.
fn duration_minutes(raw: &str) -> Option<u32> {
    let raw = raw.trim();
    let split = raw.find(|c: char| !c.is_ascii_digit())?;
    let value: u64 = raw[..split].parse().ok()?;
    let minutes = match raw[split..].trim() {
        "s" => value.div_ceil(60),
        "m" => value,
        "h" => value.checked_mul(60)?,
        "d" => value.checked_mul(60 * 24)?,
        _ => return None,
    };
    u32::try_from(minutes.max(1)).ok()
}

fn import_heartbeat(openclaw: &Value, agents: &[ImportedAgent], plan: &mut ImportPlan) {
    let hb = &openclaw["agents"]["defaults"]["heartbeat"];
    // An already-configured ZeroClaw heartbeat is the operator's own.
    if !hb.is_object() || plan.config.heartbeat.enabled || !plan.config.heartbeat.agent.is_empty() {
        return;
    }
    let Some(agent_alias) = hb["agentId"]
        .as_str()
        .and_then(|id| agents.iter().find(|a| a.openclaw_id == id))
        .map(|a| a.alias.clone())
        .or_else(|| routing_agent(openclaw, agents))
    else {
        return;
    };
    plan.config.heartbeat.agent = agent_alias.clone();
    if let Some(minutes) = hb["every"].as_str().and_then(duration_minutes) {
        plan.config.heartbeat.interval_minutes = minutes;
    }
    plan.report.imported.push(ta(
        "cli-migrate-openclaw-config-heartbeat",
        &[
            ("agent", &agent_alias),
            (
                "minutes",
                &plan.config.heartbeat.interval_minutes.to_string(),
            ),
        ],
    ));
    // OpenClaw's `activeHours` window has no ZeroClaw equivalent, so turning
    // the heartbeat on would also run it overnight. Leave that to the operator.
    plan.report
        .manual
        .push(t("cli-migrate-openclaw-config-note-heartbeat-enable"));
}

fn note_unmapped_sections(openclaw: &Value, report: &mut Report) {
    let notes = [
        ("ui", "cli-migrate-openclaw-config-note-ui"),
        ("gateway", "cli-migrate-openclaw-config-note-gateway"),
        ("skills", "cli-migrate-openclaw-config-note-skills"),
        ("plugins", "cli-migrate-openclaw-config-note-plugins"),
    ];
    for (section, key) in notes {
        if openclaw[section].is_object() {
            report.manual.push(t(key));
        }
    }
    report
        .manual
        .push(t("cli-migrate-openclaw-config-note-state"));
}

/// Turn an OpenClaw id or model name into a ZeroClaw alias: lowercase
/// letters, digits, and single underscores, starting and ending alphanumeric.
fn slug(raw: &str) -> String {
    let mut out = String::with_capacity(raw.len());
    for ch in raw.chars() {
        let ch = ch.to_ascii_lowercase();
        if ch.is_ascii_lowercase() || ch.is_ascii_digit() {
            out.push(ch);
        } else if !out.is_empty() && !out.ends_with('_') {
            out.push('_');
        }
    }
    out.truncate(MAX_ALIAS_LEN);
    let trimmed = out.trim_end_matches('_');
    if trimmed.is_empty() {
        "openclaw".to_string()
    } else {
        trimmed.to_string()
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use tempfile::TempDir;

    fn test_config(root: &Path) -> Config {
        Config {
            config_path: root.join("zeroclaw").join("config.toml"),
            data_dir: root.join("zeroclaw").join("data"),
            ..Config::default()
        }
    }

    fn sample_openclaw(home: &Path) -> Value {
        json!({
            "agents": {
                "defaults": {
                    "model": { "primary": "openai/gpt-5.6-luna", "fallbacks": ["ollama/qwen3.5:9b"] },
                    "heartbeat": { "agentId": "main", "every": "45m" }
                },
                "entries": {
                    "main": {
                        "identity": { "name": "Lead", "emoji": "✨" },
                        "workspace": home.join("workspace"),
                        "tools": { "agentToAgent": { "send": ["aegis", "ghost"] } }
                    },
                    "aegis": {
                        "model": {
                            "primary": "anthropic/claude-sonnet-5-5",
                            "fallbacks": ["openai/gpt-5.6-luna", "ollama/qwen3.5:9b"]
                        },
                        "workspace": home.join("workspace-aegis")
                    },
                    "Local Bot": { "model": "litellm-local/local" }
                }
            },
            "models": { "providers": {
                "ollama": {
                    "api": "ollama", "apiKey": "ollama-local",
                    "baseUrl": "http://127.0.0.1:11434", "timeoutSeconds": 300,
                    "models": [{ "id": "qwen3.5:9b", "params": { "num_ctx": 32768, "thinking": false } }]
                },
                "litellm-local": {
                    "apiKey": "sk-local", "baseUrl": "http://127.0.0.1:4000/v1",
                    "models": [{ "id": "local", "contextWindow": 32768, "maxTokens": 4096 }]
                }
            }},
            "plugins": { "entries": { "codex": { "enabled": true } } },
            "mcp": { "servers": {
                "freo": { "command": "/bin/freo", "args": ["--mcp"], "env": { "A": "b" }, "requestTimeoutMs": 120_000 },
                "off": { "command": "/bin/off", "enabled": false }
            }},
            "channels": { "whatsapp": {
                "enabled": true, "dmPolicy": "allowlist", "groupPolicy": "disabled",
                "selfChatMode": true, "allowFrom": ["+431234"], "groupAllowFrom": ["+431234"]
            }},
            "ui": { "prefs": { "themeMode": "light" } }
        })
    }

    fn write(path: &Path, body: &str) {
        fs::create_dir_all(path.parent().unwrap()).unwrap();
        fs::write(path, body).unwrap();
    }

    #[test]
    fn slug_produces_valid_aliases() {
        for raw in [
            "main",
            "Local Bot",
            "gpt-5.6-luna",
            "qwen3.5:9b",
            "__x__",
            "---",
        ] {
            let alias = slug(raw);
            zeroclaw_config::helpers::validate_alias_key(&alias)
                .unwrap_or_else(|e| panic!("{raw:?} -> {alias:?}: {e}"));
        }
        assert_eq!(slug("gpt-5.6-luna"), "gpt_5_6_luna");
        assert_eq!(slug("Local Bot"), "local_bot");
    }

    #[test]
    fn duration_minutes_parses_openclaw_durations() {
        assert_eq!(duration_minutes("30m"), Some(30));
        assert_eq!(duration_minutes("2h"), Some(120));
        assert_eq!(duration_minutes("20s"), Some(1));
        assert_eq!(duration_minutes("soon"), None);
    }

    #[test]
    fn plan_imports_agents_providers_and_validates() {
        let tmp = TempDir::new().unwrap();
        let home = tmp.path().join("openclaw");
        let config = test_config(tmp.path());
        let plan = plan_import(&sample_openclaw(&home), &home, &config).unwrap();
        let cfg = &plan.config;

        cfg.validate().expect("imported config must validate");

        assert_eq!(cfg.agents.len(), 3);
        let main = &cfg.agents["main"];
        assert_eq!(main.model_provider.as_str(), "openai.gpt_5_6_luna");
        assert_eq!(main.risk_profile.as_str(), RISK_PRESET);
        assert_eq!(main.runtime_profile.as_str(), "unbounded");
        assert!(!main.delegate_same_risk_profile);
        assert!(cfg.risk_profiles.contains_key(RISK_PRESET));
        assert!(cfg.runtime_profiles[main.runtime_profile.as_str()].agentic);

        let models = &cfg.providers.models;
        let (_, _, luna) = models.find_by_name("openai.gpt_5_6_luna").unwrap();
        assert_eq!(luna.model.as_deref(), Some("gpt-5.6-luna"));
        assert!(luna.requires_openai_auth);
        assert_eq!(luna.fallback.len(), 1);
        assert_eq!(luna.fallback[0].as_str(), "ollama.qwen3_5_9b");

        let (_, _, sonnet) = models.find_by_name("anthropic.claude_sonnet_5_5").unwrap();
        assert_eq!(
            sonnet
                .fallback
                .iter()
                .map(|r| r.as_str())
                .collect::<Vec<_>>(),
            ["openai.gpt_5_6_luna", "ollama.qwen3_5_9b"]
        );
        assert!(sonnet.api_key.is_none());

        let ollama = &models.ollama["qwen3_5_9b"];
        assert_eq!(ollama.base.uri.as_deref(), Some("http://127.0.0.1:11434"));
        assert!(ollama.base.api_key.is_none(), "placeholder key is dropped");
        assert_eq!(ollama.num_ctx, Some(32768));
        assert_eq!(ollama.base.think, Some(false));

        let (family, _, local) = models.find_by_name("custom.local").unwrap();
        assert_eq!(family, "custom");
        assert_eq!(local.api_key.as_deref(), Some("sk-local"));
        assert_eq!(
            cfg.agents["local_bot"].model_provider.as_str(),
            "custom.local"
        );
    }

    #[test]
    fn plan_keeps_delegation_and_channels_closed() {
        let tmp = TempDir::new().unwrap();
        let home = tmp.path().join("openclaw");
        let config = test_config(tmp.path());
        let plan = plan_import(&sample_openclaw(&home), &home, &config).unwrap();
        let cfg = &plan.config;

        let main = &cfg.agents["main"];
        let delegates: Vec<&str> = main.delegates.iter().map(|d| d.agent.as_str()).collect();
        assert_eq!(delegates, ["aegis"], "unknown peers are not invented");
        assert!(cfg.agents["aegis"].delegates.is_empty());

        let wa = &cfg.channels.whatsapp[WHATSAPP_ALIAS];
        assert!(!wa.enabled, "channel is imported disabled");
        assert!(wa.self_chat_mode);
        assert_eq!(main.channels.len(), 1);
        assert_eq!(main.channels[0].as_str(), "whatsapp.default");
        let group = &cfg.peer_groups[WHATSAPP_PEER_GROUP];
        assert_eq!(group.external_peers.len(), 1, "peers are de-duplicated");

        assert!(!cfg.heartbeat.enabled, "heartbeat is not switched on");
        assert_eq!(cfg.heartbeat.agent, "main");
        assert_eq!(cfg.heartbeat.interval_minutes, 45);

        assert_eq!(cfg.mcp.servers.len(), 1, "disabled server is skipped");
        assert_eq!(cfg.mcp.servers[0].name, "freo");
        assert_eq!(cfg.mcp.servers[0].tool_timeout_secs, Some(120));
        assert_eq!(cfg.mcp_bundles[MCP_BUNDLE_ALIAS].servers, ["freo"]);
        assert_eq!(main.mcp_bundles, [MCP_BUNDLE_ALIAS]);
    }

    #[test]
    fn reserved_agent_alias_is_skipped_without_side_effects() {
        let tmp = TempDir::new().unwrap();
        let home = tmp.path().join("openclaw");
        let config = test_config(tmp.path());
        let openclaw = json!({
            "agents": { "entries": { "default": { "model": "openai/gpt-5.6-luna" } } }
        });
        let plan = plan_import(&openclaw, &home, &config).unwrap();

        assert!(plan.config.agents.is_empty());
        assert!(plan.config.providers.models.openai.is_empty());
        assert!(plan.config.risk_profiles.is_empty());
        assert!(!plan.changes_config());
        assert_eq!(plan.report.skipped.len(), 1);
    }

    #[test]
    fn plan_never_overwrites_existing_entries() {
        let tmp = TempDir::new().unwrap();
        let home = tmp.path().join("openclaw");
        let config = test_config(tmp.path());
        let openclaw = sample_openclaw(&home);
        let first = plan_import(&openclaw, &home, &config).unwrap();

        let mut edited = first.config.clone();
        edited.agents.get_mut("main").unwrap().enabled = false;
        let second = plan_import(&openclaw, &home, &edited).unwrap();

        assert!(!second.config.agents["main"].enabled);
        assert!(!second.changes_config(), "{:?}", second.report.imported);
        assert_eq!(second.config.mcp.servers.len(), 1);
    }

    #[test]
    fn workspace_files_are_copied_without_overwriting() {
        let tmp = TempDir::new().unwrap();
        let home = tmp.path().join("openclaw");
        write(&home.join("workspace/SOUL.md"), "soul");
        write(&home.join("workspace/notes.blend"), "not a bootstrap file");
        write(&home.join("workspace/skills/demo/SKILL.md"), "skill");
        write(&home.join("workspace/skills/.git/config"), "ignored");
        write(&home.join("workspace-aegis/IDENTITY.md"), "aegis identity");

        let config = test_config(tmp.path());
        let plan = plan_import(&sample_openclaw(&home), &home, &config).unwrap();
        let main_ws = plan.config.agent_workspace_dir("main");
        let aegis_ws = plan.config.agent_workspace_dir("aegis");
        write(&main_ws.join("SOUL.md"), "existing soul");

        let (written, kept) = apply_file_ops(&plan.copies, &plan.writes).unwrap();
        assert_eq!(kept, 1);
        // skill + aegis identity + generated main identity
        assert_eq!(written, 3);
        assert_eq!(
            fs::read_to_string(main_ws.join("SOUL.md")).unwrap(),
            "existing soul"
        );
        assert!(main_ws.join("skills/demo/SKILL.md").is_file());
        assert!(!main_ws.join("skills/.git").exists());
        assert!(!main_ws.join("notes.blend").exists());
        assert!(
            fs::read_to_string(main_ws.join("IDENTITY.md"))
                .unwrap()
                .contains("Lead")
        );
        assert_eq!(
            fs::read_to_string(aegis_ws.join("IDENTITY.md")).unwrap(),
            "aegis identity"
        );
    }
}
