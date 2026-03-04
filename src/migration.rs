use crate::config::schema::{LinqConfig, WhatsAppConfig};
use crate::config::{
    ChannelsConfig, Config, DelegateAgentConfig, DiscordConfig, FeishuConfig, LarkConfig,
    MatrixConfig, NextcloudTalkConfig, SlackConfig, TelegramConfig,
};
use crate::memory::{self, Memory, MemoryCategory};
use anyhow::{bail, Context, Result};
use directories::UserDirs;
use rusqlite::{Connection, OpenFlags, OptionalExtension};
use serde::Serialize;
use serde_json::{Map as JsonMap, Value};
use std::collections::HashSet;
use std::fs;
use std::path::{Path, PathBuf};

#[derive(Debug, Clone)]
struct SourceEntry {
    key: String,
    content: String,
    category: MemoryCategory,
}

#[derive(Debug, Clone, Default, Serialize)]
pub(crate) struct MemoryMigrationStats {
    from_sqlite: usize,
    from_markdown: usize,
    candidates: usize,
    imported: usize,
    skipped_unchanged: usize,
    skipped_duplicate_content: usize,
    renamed_conflicts: usize,
}

#[derive(Debug, Clone, Default, Serialize)]
pub(crate) struct ConfigMigrationStats {
    source_loaded: bool,
    defaults_added: usize,
    defaults_preserved: usize,
    channels_added: usize,
    channels_merged: usize,
    agents_added: usize,
    agents_merged: usize,
    agent_tools_added: usize,
    merge_conflicts_preserved: usize,
    duplicate_items_skipped: usize,
}

#[derive(Debug, Clone, Serialize)]
pub(crate) struct OpenClawMigrationOptions {
    pub source_workspace: Option<PathBuf>,
    pub source_config: Option<PathBuf>,
    pub include_memory: bool,
    pub include_config: bool,
    pub dry_run: bool,
}

impl Default for OpenClawMigrationOptions {
    fn default() -> Self {
        Self {
            source_workspace: None,
            source_config: None,
            include_memory: true,
            include_config: true,
            dry_run: false,
        }
    }
}

#[derive(Debug, Clone, Default, Serialize)]
pub(crate) struct OpenClawMigrationReport {
    source_workspace: PathBuf,
    source_config: PathBuf,
    target_workspace: PathBuf,
    include_memory: bool,
    include_config: bool,
    dry_run: bool,
    memory: MemoryMigrationStats,
    config: ConfigMigrationStats,
    backups: Vec<PathBuf>,
    notes: Vec<String>,
}

#[derive(Debug, Default)]
struct JsonMergeStats {
    conflicts_preserved: usize,
    duplicate_items_skipped: usize,
}

pub async fn handle_command(command: crate::MigrateCommands, config: &Config) -> Result<()> {
    match command {
        crate::MigrateCommands::Openclaw {
            source,
            source_config,
            dry_run,
            no_memory,
            no_config,
        } => {
            let options = OpenClawMigrationOptions {
                source_workspace: source,
                source_config,
                include_memory: !no_memory,
                include_config: !no_config,
                dry_run,
            };
            let report = migrate_openclaw(config, options).await?;
            print_report(&report);
            Ok(())
        }
    }
}

pub(crate) async fn migrate_openclaw(
    config: &Config,
    options: OpenClawMigrationOptions,
) -> Result<OpenClawMigrationReport> {
    if !options.include_memory && !options.include_config {
        bail!("Nothing to migrate: both memory and config migration are disabled");
    }

    let source_workspace = resolve_openclaw_workspace(options.source_workspace.clone())?;
    let source_config = resolve_openclaw_config(options.source_config.clone())?;

    let mut report = OpenClawMigrationReport {
        source_workspace: source_workspace.clone(),
        source_config: source_config.clone(),
        target_workspace: config.workspace_dir.clone(),
        include_memory: options.include_memory,
        include_config: options.include_config,
        dry_run: options.dry_run,
        ..OpenClawMigrationReport::default()
    };

    if options.include_memory {
        if source_workspace.exists() {
            let (memory_stats, backup) =
                migrate_openclaw_memory(config, &source_workspace, options.dry_run).await?;
            report.memory = memory_stats;
            if let Some(path) = backup {
                report.backups.push(path);
            }
        } else if options.source_workspace.is_some() {
            bail!(
                "OpenClaw workspace not found at {}. Pass --source <path> if needed.",
                source_workspace.display()
            );
        } else {
            report.notes.push(format!(
                "OpenClaw workspace not found at {}; skipped memory migration",
                source_workspace.display()
            ));
        }
    }

    if options.include_config {
        if source_config.exists() {
            let (config_stats, backup, notes) =
                migrate_openclaw_config(config, &source_config, options.dry_run).await?;
            report.config = config_stats;
            if let Some(path) = backup {
                report.backups.push(path);
            }
            report.notes.extend(notes);
        } else if options.source_config.is_some() {
            bail!(
                "OpenClaw config not found at {}. Pass --source-config <path> if needed.",
                source_config.display()
            );
        } else {
            report.notes.push(format!(
                "OpenClaw config not found at {}; skipped config/agents migration",
                source_config.display()
            ));
        }
    }

    Ok(report)
}

async fn migrate_openclaw_memory(
    config: &Config,
    source_workspace: &Path,
    dry_run: bool,
) -> Result<(MemoryMigrationStats, Option<PathBuf>)> {
    let mut stats = MemoryMigrationStats::default();

    if !source_workspace.exists() {
        bail!(
            "OpenClaw workspace not found at {}. Pass --source <path> if needed.",
            source_workspace.display()
        );
    }

    if paths_equal(&source_workspace, &config.workspace_dir) {
        bail!("Source workspace matches current ZeroClaw workspace; refusing self-migration");
    }

    let entries = collect_source_entries(source_workspace, &mut stats)?;
    stats.candidates = entries.len();

    if entries.is_empty() {
        return Ok((stats, None));
    }

    if dry_run {
        return Ok((stats, None));
    }

    let memory_backup = backup_target_memory(&config.workspace_dir)?;

    let memory = target_memory_backend(config)?;
    let mut existing_content = existing_content_signatures(memory.as_ref()).await?;

    for (idx, entry) in entries.into_iter().enumerate() {
        let mut key = entry.key.trim().to_string();
        if key.is_empty() {
            key = format!("openclaw_{idx}");
        }

        if let Some(existing) = memory.get(&key).await? {
            if existing.content.trim() == entry.content.trim() {
                stats.skipped_unchanged += 1;
                continue;
            }

            let renamed = next_available_key(memory.as_ref(), &key).await?;
            key = renamed;
            stats.renamed_conflicts += 1;
        }

        let signature = content_signature(&entry.content, &entry.category);
        if existing_content.contains(&signature) {
            stats.skipped_duplicate_content += 1;
            continue;
        }

        memory
            .store(&key, &entry.content, entry.category, None)
            .await?;
        stats.imported += 1;
        existing_content.insert(signature);
    }

    Ok((stats, memory_backup))
}

fn target_memory_backend(config: &Config) -> Result<Box<dyn Memory>> {
    memory::create_memory_for_migration(&config.memory.backend, &config.workspace_dir)
}

fn collect_source_entries(
    source_workspace: &Path,
    stats: &mut MemoryMigrationStats,
) -> Result<Vec<SourceEntry>> {
    let mut entries = Vec::new();

    let sqlite_path = source_workspace.join("memory").join("brain.db");
    let sqlite_entries = read_openclaw_sqlite_entries(&sqlite_path)?;
    stats.from_sqlite = sqlite_entries.len();
    entries.extend(sqlite_entries);

    let markdown_entries = read_openclaw_markdown_entries(source_workspace)?;
    stats.from_markdown = markdown_entries.len();
    entries.extend(markdown_entries);

    // De-dup exact duplicates to make re-runs deterministic.
    let mut seen = HashSet::new();
    entries.retain(|entry| {
        let sig = format!("{}\u{0}{}\u{0}{}", entry.key, entry.content, entry.category);
        seen.insert(sig)
    });

    Ok(entries)
}

fn print_report(report: &OpenClawMigrationReport) {
    if report.dry_run {
        println!("🔎 Dry run: OpenClaw migration preview");
    } else {
        println!("✅ OpenClaw migration complete");
    }

    println!("  Source workspace: {}", report.source_workspace.display());
    println!("  Source config:    {}", report.source_config.display());
    println!("  Target workspace: {}", report.target_workspace.display());
    println!(
        "  Modules:          memory={} config={}",
        report.include_memory, report.include_config
    );

    if report.include_memory {
        println!("  [memory]");
        println!("    candidates:              {}", report.memory.candidates);
        println!("    from sqlite:             {}", report.memory.from_sqlite);
        println!(
            "    from markdown:           {}",
            report.memory.from_markdown
        );
        println!("    imported:                {}", report.memory.imported);
        println!(
            "    skipped unchanged keys:  {}",
            report.memory.skipped_unchanged
        );
        println!(
            "    skipped duplicate content: {}",
            report.memory.skipped_duplicate_content
        );
        println!(
            "    renamed key conflicts:   {}",
            report.memory.renamed_conflicts
        );
    }

    if report.include_config {
        println!("  [config]");
        println!(
            "    source loaded:           {}",
            report.config.source_loaded
        );
        println!(
            "    defaults merged:         {}",
            report.config.defaults_added
        );
        println!(
            "    defaults preserved:      {}",
            report.config.defaults_preserved
        );
        println!(
            "    channels added:          {}",
            report.config.channels_added
        );
        println!(
            "    channels merged:         {}",
            report.config.channels_merged
        );
        println!(
            "    agents added:            {}",
            report.config.agents_added
        );
        println!(
            "    agents merged:           {}",
            report.config.agents_merged
        );
        println!(
            "    agent tools appended:    {}",
            report.config.agent_tools_added
        );
        println!(
            "    merge conflicts preserved: {}",
            report.config.merge_conflicts_preserved
        );
        println!(
            "    duplicate source items:  {}",
            report.config.duplicate_items_skipped
        );
    }

    if !report.backups.is_empty() {
        println!("  Backups:");
        for path in &report.backups {
            println!("    - {}", path.display());
        }
    }

    if !report.notes.is_empty() {
        println!("  Notes:");
        for note in &report.notes {
            println!("    - {note}");
        }
    }
}

async fn migrate_openclaw_config(
    config: &Config,
    source_config_path: &Path,
    dry_run: bool,
) -> Result<(ConfigMigrationStats, Option<PathBuf>, Vec<String>)> {
    let mut stats = ConfigMigrationStats::default();
    let mut notes = Vec::new();

    if !source_config_path.exists() {
        notes.push(format!(
            "OpenClaw config not found at {}; skipping config migration",
            source_config_path.display()
        ));
        return Ok((stats, None, notes));
    }

    let raw = fs::read_to_string(source_config_path).with_context(|| {
        format!(
            "Failed to read OpenClaw config at {}",
            source_config_path.display()
        )
    })?;
    let source_config: Value = serde_json::from_str(&raw).with_context(|| {
        format!(
            "Failed to parse OpenClaw config JSON at {}",
            source_config_path.display()
        )
    })?;
    if !source_config.is_object() {
        bail!(
            "OpenClaw config at {} is not a JSON object",
            source_config_path.display()
        );
    }
    stats.source_loaded = true;

    let mut target_config = load_config_without_env(config)?;
    let mut changed = false;

    changed |= merge_openclaw_defaults(&mut target_config, &source_config, &mut stats);
    changed |= merge_openclaw_channels(
        &mut target_config.channels_config,
        &source_config,
        &mut stats,
        &mut notes,
    )?;
    changed |= merge_openclaw_agents(&mut target_config.agents, &source_config, &mut stats);

    if !changed || dry_run {
        return Ok((stats, None, notes));
    }

    let backup = backup_target_config(&target_config.config_path)?;
    target_config.save().await?;
    Ok((stats, backup, notes))
}

pub(crate) fn load_config_without_env(base: &Config) -> Result<Config> {
    let contents = fs::read_to_string(&base.config_path)
        .with_context(|| format!("Failed to read config file {}", base.config_path.display()))?;

    let mut parsed: Config = toml::from_str(&contents)
        .with_context(|| format!("Failed to parse config file {}", base.config_path.display()))?;
    parsed.config_path = base.config_path.clone();
    parsed.workspace_dir = base.workspace_dir.clone();
    Ok(parsed)
}

fn merge_openclaw_defaults(
    target: &mut Config,
    source: &Value,
    stats: &mut ConfigMigrationStats,
) -> bool {
    let (source_provider, source_model) = extract_source_provider_and_model(source);
    let source_temperature = extract_source_temperature(source);

    let mut changed = false;

    if let Some(provider) = source_provider {
        let has_value = target
            .default_provider
            .as_ref()
            .is_some_and(|value| !value.trim().is_empty());
        if !has_value {
            target.default_provider = Some(provider);
            stats.defaults_added += 1;
            changed = true;
        } else if target.default_provider.as_deref() != Some(provider.as_str()) {
            stats.defaults_preserved += 1;
            stats.merge_conflicts_preserved += 1;
        }
    }

    if let Some(model) = source_model {
        let has_value = target
            .default_model
            .as_ref()
            .is_some_and(|value| !value.trim().is_empty());
        if !has_value {
            target.default_model = Some(model);
            stats.defaults_added += 1;
            changed = true;
        } else if target.default_model.as_deref() != Some(model.as_str()) {
            stats.defaults_preserved += 1;
            stats.merge_conflicts_preserved += 1;
        }
    }

    if let Some(temp) = source_temperature {
        let default_temp = Config::default().default_temperature;
        if (target.default_temperature - default_temp).abs() < f64::EPSILON
            && (target.default_temperature - temp).abs() >= f64::EPSILON
        {
            target.default_temperature = temp;
            stats.defaults_added += 1;
            changed = true;
        } else if (target.default_temperature - temp).abs() >= f64::EPSILON {
            stats.defaults_preserved += 1;
            stats.merge_conflicts_preserved += 1;
        }
    }

    changed
}

fn merge_openclaw_channels(
    target: &mut ChannelsConfig,
    source: &Value,
    stats: &mut ConfigMigrationStats,
    notes: &mut Vec<String>,
) -> Result<bool> {
    let mut changed = false;

    changed |= merge_channel_section::<TelegramConfig>(
        &mut target.telegram,
        openclaw_channel_value(source, &["telegram"]),
        "telegram",
        stats,
        notes,
    )?;
    changed |= merge_channel_section::<DiscordConfig>(
        &mut target.discord,
        openclaw_channel_value(source, &["discord"]),
        "discord",
        stats,
        notes,
    )?;
    changed |= merge_channel_section::<SlackConfig>(
        &mut target.slack,
        openclaw_channel_value(source, &["slack"]),
        "slack",
        stats,
        notes,
    )?;
    changed |= merge_channel_section::<MatrixConfig>(
        &mut target.matrix,
        openclaw_channel_value(source, &["matrix"]),
        "matrix",
        stats,
        notes,
    )?;
    changed |= merge_channel_section::<WhatsAppConfig>(
        &mut target.whatsapp,
        openclaw_channel_value(source, &["whatsapp"]),
        "whatsapp",
        stats,
        notes,
    )?;
    changed |= merge_channel_section::<LinqConfig>(
        &mut target.linq,
        openclaw_channel_value(source, &["linq"]),
        "linq",
        stats,
        notes,
    )?;
    changed |= merge_channel_section::<NextcloudTalkConfig>(
        &mut target.nextcloud_talk,
        openclaw_channel_value(source, &["nextcloud_talk", "nextcloud-talk"]),
        "nextcloud_talk",
        stats,
        notes,
    )?;
    changed |= merge_channel_section::<LarkConfig>(
        &mut target.lark,
        openclaw_channel_value(source, &["lark"]),
        "lark",
        stats,
        notes,
    )?;
    changed |= merge_channel_section::<FeishuConfig>(
        &mut target.feishu,
        openclaw_channel_value(source, &["feishu"]),
        "feishu",
        stats,
        notes,
    )?;

    Ok(changed)
}

fn merge_openclaw_agents(
    target_agents: &mut std::collections::HashMap<String, DelegateAgentConfig>,
    source: &Value,
    stats: &mut ConfigMigrationStats,
) -> bool {
    let mut changed = false;
    let source_agents = extract_source_agents(source);
    for (name, source_agent) in source_agents {
        if let Some(existing) = target_agents.get_mut(&name) {
            if merge_delegate_agent(existing, &source_agent, stats) {
                stats.agents_merged += 1;
                changed = true;
            }
            continue;
        }

        target_agents.insert(name, source_agent);
        stats.agents_added += 1;
        changed = true;
    }
    changed
}

fn merge_delegate_agent(
    target: &mut DelegateAgentConfig,
    source: &DelegateAgentConfig,
    stats: &mut ConfigMigrationStats,
) -> bool {
    let mut changed = false;

    if target.provider.trim().is_empty() && !source.provider.trim().is_empty() {
        target.provider = source.provider.clone();
        changed = true;
    } else if target.provider != source.provider {
        stats.merge_conflicts_preserved += 1;
    }

    if target.model.trim().is_empty() && !source.model.trim().is_empty() {
        target.model = source.model.clone();
        changed = true;
    } else if target.model != source.model {
        stats.merge_conflicts_preserved += 1;
    }

    match (&mut target.system_prompt, &source.system_prompt) {
        (None, Some(source_prompt)) => {
            target.system_prompt = Some(source_prompt.clone());
            changed = true;
        }
        (Some(target_prompt), Some(source_prompt))
            if target_prompt.trim().is_empty() && !source_prompt.trim().is_empty() =>
        {
            *target_prompt = source_prompt.clone();
            changed = true;
        }
        (Some(target_prompt), Some(source_prompt)) if target_prompt != source_prompt => {
            stats.merge_conflicts_preserved += 1;
        }
        _ => {}
    }

    match (&mut target.api_key, &source.api_key) {
        (None, Some(source_key)) => {
            target.api_key = Some(source_key.clone());
            changed = true;
        }
        (Some(target_key), Some(source_key))
            if target_key.trim().is_empty() && !source_key.trim().is_empty() =>
        {
            *target_key = source_key.clone();
            changed = true;
        }
        (Some(target_key), Some(source_key)) if target_key != source_key => {
            stats.merge_conflicts_preserved += 1;
        }
        _ => {}
    }

    match (target.temperature, source.temperature) {
        (None, Some(temp)) => {
            target.temperature = Some(temp);
            changed = true;
        }
        (Some(target_temp), Some(source_temp))
            if (target_temp - source_temp).abs() >= f64::EPSILON =>
        {
            stats.merge_conflicts_preserved += 1;
        }
        _ => {}
    }

    if target.max_depth != source.max_depth {
        stats.merge_conflicts_preserved += 1;
    }
    if target.agentic != source.agentic {
        stats.merge_conflicts_preserved += 1;
    }
    if target.max_iterations != source.max_iterations {
        stats.merge_conflicts_preserved += 1;
    }

    let mut seen = HashSet::new();
    for existing in &target.allowed_tools {
        let trimmed = existing.trim();
        if !trimmed.is_empty() {
            seen.insert(trimmed.to_string());
        }
    }
    for source_tool in &source.allowed_tools {
        let trimmed = source_tool.trim();
        if trimmed.is_empty() {
            continue;
        }
        if seen.insert(trimmed.to_string()) {
            target.allowed_tools.push(trimmed.to_string());
            stats.agent_tools_added += 1;
            changed = true;
        } else {
            stats.duplicate_items_skipped += 1;
        }
    }

    changed
}

fn openclaw_channel_value<'a>(source: &'a Value, aliases: &[&str]) -> Option<&'a Value> {
    let source_obj = source.as_object()?;
    for alias in aliases {
        if let Some(value) = source_obj.get(*alias) {
            return Some(value);
        }
    }
    let channels_obj = source_obj.get("channels")?.as_object()?;
    for alias in aliases {
        if let Some(value) = channels_obj.get(*alias) {
            return Some(value);
        }
    }
    None
}

fn merge_channel_section<T>(
    target: &mut Option<T>,
    source: Option<&Value>,
    channel_name: &str,
    stats: &mut ConfigMigrationStats,
    notes: &mut Vec<String>,
) -> Result<bool>
where
    T: Clone + serde::de::DeserializeOwned + serde::Serialize,
{
    let Some(source_value) = source else {
        return Ok(false);
    };

    if target.is_none() {
        let parsed = serde_json::from_value::<T>(source_value.clone());
        match parsed {
            Ok(parsed) => {
                *target = Some(parsed);
                stats.channels_added += 1;
                return Ok(true);
            }
            Err(error) => {
                notes.push(format!(
                    "Skipped channel '{channel_name}': source payload incompatible ({error})"
                ));
                return Ok(false);
            }
        }
    }

    let existing = target
        .as_ref()
        .context("channel target unexpectedly missing during merge")?;
    let original = serde_json::to_value(existing)?;
    let mut merged = original.clone();
    let mut merge_stats = JsonMergeStats::default();
    merge_json_preserving_target(&mut merged, source_value, &mut merge_stats);
    stats.merge_conflicts_preserved += merge_stats.conflicts_preserved;
    stats.duplicate_items_skipped += merge_stats.duplicate_items_skipped;

    if merged == original {
        return Ok(false);
    }

    let parsed = serde_json::from_value::<T>(merged);
    match parsed {
        Ok(parsed) => {
            *target = Some(parsed);
            stats.channels_merged += 1;
            Ok(true)
        }
        Err(error) => {
            notes.push(format!(
                "Skipped merged channel '{channel_name}': merged payload invalid ({error})"
            ));
            Ok(false)
        }
    }
}

fn merge_json_preserving_target(target: &mut Value, source: &Value, stats: &mut JsonMergeStats) {
    match target {
        Value::Object(target_obj) => {
            let Value::Object(source_obj) = source else {
                stats.conflicts_preserved += 1;
                return;
            };
            for (key, source_value) in source_obj {
                if let Some(target_value) = target_obj.get_mut(key) {
                    merge_json_preserving_target(target_value, source_value, stats);
                } else {
                    target_obj.insert(key.clone(), source_value.clone());
                }
            }
        }
        Value::Array(target_arr) => {
            let Value::Array(source_arr) = source else {
                stats.conflicts_preserved += 1;
                return;
            };
            for source_item in source_arr {
                if target_arr.iter().any(|existing| existing == source_item) {
                    stats.duplicate_items_skipped += 1;
                    continue;
                }
                target_arr.push(source_item.clone());
            }
        }
        Value::Null => {
            *target = source.clone();
        }
        target_value => {
            if target_value != source {
                stats.conflicts_preserved += 1;
            }
        }
    }
}

fn extract_source_agents(source: &Value) -> Vec<(String, DelegateAgentConfig)> {
    let Some(obj) = source.as_object() else {
        return Vec::new();
    };
    let Some(agents) = obj.get("agents").and_then(Value::as_object) else {
        return Vec::new();
    };

    let mut parsed = Vec::new();
    for (name, raw_agent) in agents {
        if name == "defaults" {
            continue;
        }
        if let Some(agent) = parse_source_agent(raw_agent) {
            parsed.push((name.clone(), agent));
        }
    }
    parsed
}

fn parse_source_agent(raw_agent: &Value) -> Option<DelegateAgentConfig> {
    let obj = raw_agent.as_object()?;
    let model_raw = find_string(obj, &["model"])?;
    let provider_hint = find_string(obj, &["provider"]);
    let (provider, model) = split_provider_and_model(&model_raw, provider_hint.as_deref());
    let model = model.or_else(|| {
        let trimmed = model_raw.trim();
        (!trimmed.is_empty()).then(|| trimmed.to_string())
    })?;

    let allowed_tools = obj
        .get("allowed_tools")
        .or_else(|| obj.get("tools"))
        .map(parse_tool_list)
        .unwrap_or_default();

    Some(DelegateAgentConfig {
        provider: provider.unwrap_or_else(|| "openrouter".to_string()),
        model,
        system_prompt: find_string(obj, &["system_prompt", "systemPrompt"]),
        api_key: find_string(obj, &["api_key", "apiKey"]),
        enabled: find_bool(obj, &["enabled"]).unwrap_or(true),
        capabilities: obj
            .get("capabilities")
            .or_else(|| obj.get("skills"))
            .map(parse_tool_list)
            .unwrap_or_default(),
        priority: find_i32(obj, &["priority"]).unwrap_or(0),
        temperature: find_f64(obj, &["temperature"]),
        max_depth: find_u32(obj, &["max_depth", "maxDepth"]).unwrap_or(3),
        agentic: obj.get("agentic").and_then(Value::as_bool).unwrap_or(false),
        allowed_tools,
        max_iterations: find_usize(obj, &["max_iterations", "maxIterations"]).unwrap_or(10),
    })
}

fn parse_tool_list(value: &Value) -> Vec<String> {
    let Some(arr) = value.as_array() else {
        return Vec::new();
    };

    let mut tools = Vec::new();
    let mut seen = HashSet::new();
    for item in arr {
        let Some(raw) = item.as_str() else {
            continue;
        };
        let tool = raw.trim();
        if tool.is_empty() || !seen.insert(tool.to_string()) {
            continue;
        }
        tools.push(tool.to_string());
    }
    tools
}

fn extract_source_provider_and_model(source: &Value) -> (Option<String>, Option<String>) {
    let Some(obj) = source.as_object() else {
        return (None, None);
    };

    let top_provider = find_string(obj, &["default_provider", "provider"]);
    let top_model = find_string(obj, &["default_model", "model"]);
    if let Some(top_model) = top_model {
        return split_provider_and_model(&top_model, top_provider.as_deref());
    }

    let Some(agent) = obj.get("agent").and_then(Value::as_object) else {
        return (top_provider.as_deref().map(normalize_provider_name), None);
    };
    let agent_provider = find_string(agent, &["provider"]).or(top_provider);
    let agent_model = find_string(agent, &["model"]);

    if let Some(agent_model) = agent_model {
        split_provider_and_model(&agent_model, agent_provider.as_deref())
    } else {
        (agent_provider.as_deref().map(normalize_provider_name), None)
    }
}

fn extract_source_temperature(source: &Value) -> Option<f64> {
    let obj = source.as_object()?;
    if let Some(value) = obj.get("default_temperature").and_then(Value::as_f64) {
        return Some(value);
    }

    obj.get("agent")
        .and_then(Value::as_object)
        .and_then(|agent| agent.get("temperature"))
        .and_then(Value::as_f64)
}

fn split_provider_and_model(
    model_raw: &str,
    provider_hint: Option<&str>,
) -> (Option<String>, Option<String>) {
    let model_raw = model_raw.trim();
    let provider_hint = provider_hint
        .map(str::trim)
        .filter(|provider| !provider.is_empty())
        .map(normalize_provider_name);

    if let Some((provider, model)) = model_raw.split_once('/') {
        let provider = normalize_provider_name(provider);
        let model = model.trim();
        let model = (!model.is_empty()).then(|| model.to_string());
        return (Some(provider), model);
    }

    let model = (!model_raw.is_empty()).then(|| model_raw.to_string());
    (provider_hint, model)
}

fn normalize_provider_name(provider: &str) -> String {
    match provider.trim().to_ascii_lowercase().as_str() {
        "google" => "gemini".to_string(),
        "together" => "together-ai".to_string(),
        other => other.to_string(),
    }
}

fn find_string(obj: &JsonMap<String, Value>, keys: &[&str]) -> Option<String> {
    keys.iter().find_map(|key| {
        obj.get(*key).and_then(Value::as_str).and_then(|raw| {
            let trimmed = raw.trim();
            (!trimmed.is_empty()).then(|| trimmed.to_string())
        })
    })
}

fn find_f64(obj: &JsonMap<String, Value>, keys: &[&str]) -> Option<f64> {
    keys.iter()
        .find_map(|key| obj.get(*key).and_then(Value::as_f64))
}

fn find_u32(obj: &JsonMap<String, Value>, keys: &[&str]) -> Option<u32> {
    keys.iter().find_map(|key| {
        obj.get(*key)
            .and_then(Value::as_u64)
            .and_then(|value| u32::try_from(value).ok())
    })
}

fn find_usize(obj: &JsonMap<String, Value>, keys: &[&str]) -> Option<usize> {
    keys.iter().find_map(|key| {
        obj.get(*key)
            .and_then(Value::as_u64)
            .and_then(|value| usize::try_from(value).ok())
    })
}

fn find_bool(obj: &JsonMap<String, Value>, keys: &[&str]) -> Option<bool> {
    keys.iter()
        .find_map(|key| obj.get(*key).and_then(Value::as_bool))
}

fn find_i32(obj: &JsonMap<String, Value>, keys: &[&str]) -> Option<i32> {
    keys.iter().find_map(|key| {
        obj.get(*key)
            .and_then(Value::as_i64)
            .and_then(|value| i32::try_from(value).ok())
    })
}

async fn existing_content_signatures(memory: &dyn Memory) -> Result<HashSet<String>> {
    let mut signatures = HashSet::new();
    for entry in memory.list(None, None).await? {
        signatures.insert(content_signature(&entry.content, &entry.category));
    }
    Ok(signatures)
}

fn content_signature(content: &str, category: &MemoryCategory) -> String {
    format!("{}\u{0}{}", content.trim(), category)
}

fn read_openclaw_sqlite_entries(db_path: &Path) -> Result<Vec<SourceEntry>> {
    if !db_path.exists() {
        return Ok(Vec::new());
    }

    let conn = Connection::open_with_flags(db_path, OpenFlags::SQLITE_OPEN_READ_ONLY)
        .with_context(|| format!("Failed to open source db {}", db_path.display()))?;

    let table_exists: Option<String> = conn
        .query_row(
            "SELECT name FROM sqlite_master WHERE type='table' AND name='memories' LIMIT 1",
            [],
            |row| row.get(0),
        )
        .optional()?;

    if table_exists.is_none() {
        return Ok(Vec::new());
    }

    let columns = table_columns(&conn, "memories")?;
    let key_expr = pick_column_expr(&columns, &["key", "id", "name"], "CAST(rowid AS TEXT)");
    let Some(content_expr) =
        pick_optional_column_expr(&columns, &["content", "value", "text", "memory"])
    else {
        bail!("OpenClaw memories table found but no content-like column was detected");
    };
    let category_expr = pick_column_expr(&columns, &["category", "kind", "type"], "'core'");

    let sql = format!(
        "SELECT {key_expr} AS key, {content_expr} AS content, {category_expr} AS category FROM memories"
    );

    let mut stmt = conn.prepare(&sql)?;
    let mut rows = stmt.query([])?;

    let mut entries = Vec::new();
    let mut idx = 0_usize;

    while let Some(row) = rows.next()? {
        let key: String = row
            .get(0)
            .unwrap_or_else(|_| format!("openclaw_sqlite_{idx}"));
        let content: String = row.get(1).unwrap_or_default();
        let category_raw: String = row.get(2).unwrap_or_else(|_| "core".to_string());

        if content.trim().is_empty() {
            continue;
        }

        entries.push(SourceEntry {
            key: normalize_key(&key, idx),
            content: content.trim().to_string(),
            category: parse_category(&category_raw),
        });

        idx += 1;
    }

    Ok(entries)
}

fn read_openclaw_markdown_entries(source_workspace: &Path) -> Result<Vec<SourceEntry>> {
    let mut all = Vec::new();

    let core_path = source_workspace.join("MEMORY.md");
    if core_path.exists() {
        let content = fs::read_to_string(&core_path)?;
        all.extend(parse_markdown_file(
            &content,
            MemoryCategory::Core,
            "openclaw_core",
        ));
    }

    let daily_dir = source_workspace.join("memory");
    if daily_dir.exists() {
        for file in fs::read_dir(&daily_dir)? {
            let file = file?;
            let path = file.path();
            if path.extension().and_then(|ext| ext.to_str()) != Some("md") {
                continue;
            }
            let content = fs::read_to_string(&path)?;
            let stem = path
                .file_stem()
                .and_then(|s| s.to_str())
                .unwrap_or("openclaw_daily");
            all.extend(parse_markdown_file(&content, MemoryCategory::Daily, stem));
        }
    }

    Ok(all)
}

fn parse_markdown_file(
    content: &str,
    default_category: MemoryCategory,
    stem: &str,
) -> Vec<SourceEntry> {
    let mut entries = Vec::new();

    for (idx, raw_line) in content.lines().enumerate() {
        let trimmed = raw_line.trim();
        if trimmed.is_empty() || trimmed.starts_with('#') {
            continue;
        }

        let line = trimmed.strip_prefix("- ").unwrap_or(trimmed);
        let (key, text) = match parse_structured_memory_line(line) {
            Some((k, v)) => (normalize_key(k, idx), v.trim().to_string()),
            None => (
                format!("openclaw_{stem}_{}", idx + 1),
                line.trim().to_string(),
            ),
        };

        if text.is_empty() {
            continue;
        }

        entries.push(SourceEntry {
            key,
            content: text,
            category: default_category.clone(),
        });
    }

    entries
}

fn parse_structured_memory_line(line: &str) -> Option<(&str, &str)> {
    if !line.starts_with("**") {
        return None;
    }

    let rest = line.strip_prefix("**")?;
    let key_end = rest.find("**:")?;
    let key = rest.get(..key_end)?.trim();
    let value = rest.get(key_end + 3..)?.trim();

    if key.is_empty() || value.is_empty() {
        return None;
    }

    Some((key, value))
}

fn parse_category(raw: &str) -> MemoryCategory {
    match raw.trim().to_ascii_lowercase().as_str() {
        "core" | "" => MemoryCategory::Core,
        "daily" => MemoryCategory::Daily,
        "conversation" => MemoryCategory::Conversation,
        other => MemoryCategory::Custom(other.to_string()),
    }
}

fn normalize_key(key: &str, fallback_idx: usize) -> String {
    let trimmed = key.trim();
    if trimmed.is_empty() {
        return format!("openclaw_{fallback_idx}");
    }
    trimmed.to_string()
}

async fn next_available_key(memory: &dyn Memory, base: &str) -> Result<String> {
    for i in 1..=10_000 {
        let candidate = format!("{base}__openclaw_{i}");
        if memory.get(&candidate).await?.is_none() {
            return Ok(candidate);
        }
    }

    bail!("Unable to allocate non-conflicting key for '{base}'")
}

fn table_columns(conn: &Connection, table: &str) -> Result<Vec<String>> {
    let pragma = format!("PRAGMA table_info({table})");
    let mut stmt = conn.prepare(&pragma)?;
    let rows = stmt.query_map([], |row| row.get::<_, String>(1))?;

    let mut cols = Vec::new();
    for col in rows {
        cols.push(col?.to_ascii_lowercase());
    }

    Ok(cols)
}

fn pick_optional_column_expr(columns: &[String], candidates: &[&str]) -> Option<String> {
    candidates
        .iter()
        .find(|candidate| columns.iter().any(|c| c == *candidate))
        .map(std::string::ToString::to_string)
}

fn pick_column_expr(columns: &[String], candidates: &[&str], fallback: &str) -> String {
    pick_optional_column_expr(columns, candidates).unwrap_or_else(|| fallback.to_string())
}

pub(crate) fn resolve_openclaw_workspace(source: Option<PathBuf>) -> Result<PathBuf> {
    if let Some(src) = source {
        return Ok(src);
    }

    let home = UserDirs::new()
        .map(|u| u.home_dir().to_path_buf())
        .context("Could not find home directory")?;

    Ok(home.join(".openclaw").join("workspace"))
}

pub(crate) fn resolve_openclaw_config(source: Option<PathBuf>) -> Result<PathBuf> {
    if let Some(src) = source {
        return Ok(src);
    }

    let home = UserDirs::new()
        .map(|u| u.home_dir().to_path_buf())
        .context("Could not find home directory")?;

    Ok(home.join(".openclaw").join("openclaw.json"))
}

fn paths_equal(a: &Path, b: &Path) -> bool {
    match (fs::canonicalize(a), fs::canonicalize(b)) {
        (Ok(a), Ok(b)) => a == b,
        _ => a == b,
    }
}

fn backup_target_memory(workspace_dir: &Path) -> Result<Option<PathBuf>> {
    let timestamp = chrono::Local::now().format("%Y%m%d-%H%M%S").to_string();
    let backup_root = workspace_dir
        .join("memory")
        .join("migrations")
        .join(format!("openclaw-{timestamp}"));

    let mut copied_any = false;
    fs::create_dir_all(&backup_root)?;

    let files_to_copy = [
        workspace_dir.join("memory").join("brain.db"),
        workspace_dir.join("MEMORY.md"),
    ];

    for source in files_to_copy {
        if source.exists() {
            let Some(name) = source.file_name() else {
                continue;
            };
            fs::copy(&source, backup_root.join(name))?;
            copied_any = true;
        }
    }

    let daily_dir = workspace_dir.join("memory");
    if daily_dir.exists() {
        let daily_backup = backup_root.join("daily");
        for file in fs::read_dir(&daily_dir)? {
            let file = file?;
            let path = file.path();
            if path.extension().and_then(|ext| ext.to_str()) != Some("md") {
                continue;
            }
            fs::create_dir_all(&daily_backup)?;
            let Some(name) = path.file_name() else {
                continue;
            };
            fs::copy(&path, daily_backup.join(name))?;
            copied_any = true;
        }
    }

    if copied_any {
        Ok(Some(backup_root))
    } else {
        let _ = fs::remove_dir_all(&backup_root);
        Ok(None)
    }
}

fn backup_target_config(config_path: &Path) -> Result<Option<PathBuf>> {
    if !config_path.exists() {
        return Ok(None);
    }

    let timestamp = chrono::Local::now().format("%Y%m%d-%H%M%S").to_string();
    let Some(parent) = config_path.parent() else {
        return Ok(None);
    };
    let backup_root = parent
        .join("migrations")
        .join(format!("openclaw-{timestamp}"));
    fs::create_dir_all(&backup_root)?;
    let backup_path = backup_root.join("config.toml");
    fs::copy(config_path, &backup_path)?;
    Ok(Some(backup_path))
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::config::{
        Config, DelegateAgentConfig, MemoryConfig, ProgressMode, StreamMode, TelegramConfig,
    };
    use crate::memory::{Memory, SqliteMemory};
    use rusqlite::params;
    use serde_json::json;
    use tempfile::TempDir;

    fn test_config(workspace: &Path) -> Config {
        Config {
            workspace_dir: workspace.to_path_buf(),
            config_path: workspace.join("config.toml"),
            memory: MemoryConfig {
                backend: "sqlite".to_string(),
                ..MemoryConfig::default()
            },
            ..Config::default()
        }
    }

    #[test]
    fn parse_structured_markdown_line() {
        let line = "**user_pref**: likes Rust";
        let parsed = parse_structured_memory_line(line).unwrap();
        assert_eq!(parsed.0, "user_pref");
        assert_eq!(parsed.1, "likes Rust");
    }

    #[test]
    fn parse_unstructured_markdown_generates_key() {
        let entries = parse_markdown_file("- plain note", MemoryCategory::Core, "core");
        assert_eq!(entries.len(), 1);
        assert!(entries[0].key.starts_with("openclaw_core_"));
        assert_eq!(entries[0].content, "plain note");
    }

    #[test]
    fn sqlite_reader_supports_legacy_value_column() {
        let dir = TempDir::new().unwrap();
        let db_path = dir.path().join("brain.db");
        let conn = Connection::open(&db_path).unwrap();

        conn.execute_batch("CREATE TABLE memories (key TEXT, value TEXT, type TEXT);")
            .unwrap();
        conn.execute(
            "INSERT INTO memories (key, value, type) VALUES (?1, ?2, ?3)",
            params!["legacy_key", "legacy_value", "daily"],
        )
        .unwrap();

        let rows = read_openclaw_sqlite_entries(&db_path).unwrap();
        assert_eq!(rows.len(), 1);
        assert_eq!(rows[0].key, "legacy_key");
        assert_eq!(rows[0].content, "legacy_value");
        assert_eq!(rows[0].category, MemoryCategory::Daily);
    }

    #[tokio::test]
    async fn migration_renames_conflicting_key() {
        let source = TempDir::new().unwrap();
        let target = TempDir::new().unwrap();

        // Existing target memory
        let target_mem = SqliteMemory::new(target.path()).unwrap();
        target_mem
            .store("k", "new value", MemoryCategory::Core, None)
            .await
            .unwrap();

        // Source sqlite with conflicting key + different content
        let source_db_dir = source.path().join("memory");
        fs::create_dir_all(&source_db_dir).unwrap();
        let source_db = source_db_dir.join("brain.db");
        let conn = Connection::open(&source_db).unwrap();
        conn.execute_batch("CREATE TABLE memories (key TEXT, content TEXT, category TEXT);")
            .unwrap();
        conn.execute(
            "INSERT INTO memories (key, content, category) VALUES (?1, ?2, ?3)",
            params!["k", "old value", "core"],
        )
        .unwrap();

        let config = test_config(target.path());
        migrate_openclaw_memory(&config, source.path(), false)
            .await
            .unwrap();

        let all = target_mem.list(None, None).await.unwrap();
        assert!(all.iter().any(|e| e.key == "k" && e.content == "new value"));
        assert!(all
            .iter()
            .any(|e| e.key.starts_with("k__openclaw_") && e.content == "old value"));
    }

    #[tokio::test]
    async fn dry_run_does_not_write() {
        let source = TempDir::new().unwrap();
        let target = TempDir::new().unwrap();
        let source_db_dir = source.path().join("memory");
        fs::create_dir_all(&source_db_dir).unwrap();

        let source_db = source_db_dir.join("brain.db");
        let conn = Connection::open(&source_db).unwrap();
        conn.execute_batch("CREATE TABLE memories (key TEXT, content TEXT, category TEXT);")
            .unwrap();
        conn.execute(
            "INSERT INTO memories (key, content, category) VALUES (?1, ?2, ?3)",
            params!["dry", "run", "core"],
        )
        .unwrap();

        let config = test_config(target.path());
        migrate_openclaw_memory(&config, source.path(), true)
            .await
            .unwrap();

        let target_mem = SqliteMemory::new(target.path()).unwrap();
        assert_eq!(target_mem.count().await.unwrap(), 0);
    }

    #[tokio::test]
    async fn migration_skips_duplicate_content_across_different_keys() {
        let source = TempDir::new().unwrap();
        let target = TempDir::new().unwrap();

        let target_mem = SqliteMemory::new(target.path()).unwrap();
        target_mem
            .store("existing", "same content", MemoryCategory::Core, None)
            .await
            .unwrap();

        let source_db_dir = source.path().join("memory");
        fs::create_dir_all(&source_db_dir).unwrap();
        let source_db = source_db_dir.join("brain.db");
        let conn = Connection::open(&source_db).unwrap();
        conn.execute_batch("CREATE TABLE memories (key TEXT, content TEXT, category TEXT);")
            .unwrap();
        conn.execute(
            "INSERT INTO memories (key, content, category) VALUES (?1, ?2, ?3)",
            params!["incoming", "same content", "core"],
        )
        .unwrap();

        let config = test_config(target.path());
        let (stats, _) = migrate_openclaw_memory(&config, source.path(), false)
            .await
            .unwrap();

        assert_eq!(stats.skipped_duplicate_content, 1);
        assert_eq!(target_mem.count().await.unwrap(), 1);
    }

    #[tokio::test]
    async fn config_migration_merges_agents_and_channels_without_overwrite() {
        let source = TempDir::new().unwrap();
        let target = TempDir::new().unwrap();

        let mut config = test_config(target.path());
        config.default_provider = Some("openrouter".to_string());
        config.default_model = Some("existing-model".to_string());
        config.channels_config.telegram = Some(TelegramConfig {
            bot_token: "target-token".to_string(),
            allowed_users: vec!["u1".to_string()],
            stream_mode: StreamMode::default(),
            draft_update_interval_ms: 1_500,
            interrupt_on_new_message: false,
            mention_only: false,
            progress_mode: ProgressMode::default(),
            ack_enabled: true,
            group_reply: None,
            base_url: None,
        });
        config.agents.insert(
            "researcher".to_string(),
            DelegateAgentConfig {
                provider: "openrouter".to_string(),
                model: "existing-model".to_string(),
                system_prompt: Some("existing prompt".to_string()),
                api_key: None,
                enabled: true,
                capabilities: Vec::new(),
                priority: 0,
                temperature: None,
                max_depth: 3,
                agentic: false,
                allowed_tools: vec!["shell".to_string()],
                max_iterations: 10,
            },
        );
        config.save().await.unwrap();
        let baseline = load_config_without_env(&config).unwrap();
        let baseline_telegram_token = baseline
            .channels_config
            .telegram
            .as_ref()
            .expect("baseline telegram config")
            .bot_token
            .clone();

        let source_config_path = source.path().join("openclaw.json");
        fs::write(
            &source_config_path,
            serde_json::to_string_pretty(&json!({
                "agent": {
                    "model": "anthropic/claude-sonnet-4-6",
                    "temperature": 0.2
                },
                "telegram": {
                    "bot_token": "source-token",
                    "allowed_users": ["u1", "u2"]
                },
                "agents": {
                    "researcher": {
                        "model": "openai/gpt-4o",
                        "tools": ["shell", "file_read"],
                        "agentic": true
                    },
                    "helper": {
                        "model": "openai/gpt-4o-mini",
                        "tools": ["web_search"],
                        "agentic": true
                    }
                }
            }))
            .unwrap(),
        )
        .unwrap();

        let (stats, _backup, notes) = migrate_openclaw_config(&config, &source_config_path, false)
            .await
            .unwrap();
        assert!(notes.is_empty(), "unexpected migration notes: {notes:?}");

        let merged = load_config_without_env(&config).unwrap();
        assert_eq!(
            merged.default_provider.as_deref(),
            Some("openrouter"),
            "existing provider must be preserved"
        );
        assert_eq!(
            merged.default_model.as_deref(),
            Some("existing-model"),
            "existing model must be preserved"
        );

        let telegram = merged.channels_config.telegram.unwrap();
        assert_eq!(
            telegram.bot_token, baseline_telegram_token,
            "existing channel credentials must be preserved"
        );
        assert_eq!(telegram.allowed_users.len(), 2);
        assert!(telegram.allowed_users.contains(&"u1".to_string()));
        assert!(telegram.allowed_users.contains(&"u2".to_string()));

        let researcher = merged.agents.get("researcher").unwrap();
        assert_eq!(researcher.model, "existing-model");
        assert!(researcher.allowed_tools.contains(&"shell".to_string()));
        assert!(researcher.allowed_tools.contains(&"file_read".to_string()));
        assert!(merged.agents.contains_key("helper"));

        assert_eq!(stats.agents_added, 1);
        assert_eq!(stats.agents_merged, 1);
        assert_eq!(stats.agent_tools_added, 1);
        assert!(
            stats.merge_conflicts_preserved > 0,
            "merge conflicts should be recorded for overlapping fields that are preserved"
        );
    }

    #[tokio::test]
    async fn migrate_openclaw_rejects_when_both_modules_disabled() {
        let target = TempDir::new().unwrap();
        let config = test_config(target.path());

        let err = migrate_openclaw(
            &config,
            OpenClawMigrationOptions {
                include_memory: false,
                include_config: false,
                ..OpenClawMigrationOptions::default()
            },
        )
        .await
        .expect_err("both modules disabled must error");

        assert!(
            err.to_string().contains("Nothing to migrate"),
            "unexpected error: {err}"
        );
    }

    #[tokio::test]
    async fn migrate_openclaw_errors_on_explicit_missing_workspace() {
        let target = TempDir::new().unwrap();
        let config = test_config(target.path());
        let missing_source = target.path().join("missing-openclaw-workspace");

        let err = migrate_openclaw(
            &config,
            OpenClawMigrationOptions {
                source_workspace: Some(missing_source.clone()),
                include_memory: true,
                include_config: false,
                dry_run: true,
                ..OpenClawMigrationOptions::default()
            },
        )
        .await
        .expect_err("explicit missing workspace must error");

        assert!(
            err.to_string().contains("workspace not found"),
            "unexpected error for {}: {err}",
            missing_source.display()
        );
    }

    #[tokio::test]
    async fn migrate_openclaw_errors_on_explicit_missing_config() {
        let source = TempDir::new().unwrap();
        let target = TempDir::new().unwrap();
        let config = test_config(target.path());
        let missing_config = target.path().join("missing-openclaw.json");

        // Ensure memory path exists so the error comes from explicit config resolution.
        std::fs::create_dir_all(source.path().join("memory")).unwrap();

        let err = migrate_openclaw(
            &config,
            OpenClawMigrationOptions {
                source_workspace: Some(source.path().to_path_buf()),
                source_config: Some(missing_config.clone()),
                include_memory: false,
                include_config: true,
                dry_run: true,
            },
        )
        .await
        .expect_err("explicit missing config must error");

        assert!(
            err.to_string().contains("config not found"),
            "unexpected error for {}: {err}",
            missing_config.display()
        );
    }

    #[tokio::test]
    async fn migrate_openclaw_config_missing_source_returns_note() {
        let target = TempDir::new().unwrap();
        let config = test_config(target.path());
        let missing_source = target.path().join("missing-openclaw.json");

        let (stats, backup, notes) = migrate_openclaw_config(&config, &missing_source, true)
            .await
            .expect("missing config should return note");

        assert!(!stats.source_loaded);
        assert!(backup.is_none());
        assert_eq!(notes.len(), 1);
        assert!(
            notes[0].contains("skipping config migration"),
            "unexpected note: {}",
            notes[0]
        );
    }

    #[test]
    fn migration_target_rejects_none_backend() {
        let target = TempDir::new().unwrap();
        let mut config = test_config(target.path());
        config.memory.backend = "none".to_string();

        let err = target_memory_backend(&config)
            .err()
            .expect("backend=none should be rejected for migration target");
        assert!(err.to_string().contains("disables persistence"));
    }

    // ── §7.1 / §7.2 Config backward compatibility & migration tests ──

    #[test]
    fn parse_category_handles_all_variants() {
        assert_eq!(parse_category("core"), MemoryCategory::Core);
        assert_eq!(parse_category("daily"), MemoryCategory::Daily);
        assert_eq!(parse_category("conversation"), MemoryCategory::Conversation);
        assert_eq!(parse_category(""), MemoryCategory::Core);
        assert_eq!(
            parse_category("custom_type"),
            MemoryCategory::Custom("custom_type".to_string())
        );
    }

    #[test]
    fn parse_category_case_insensitive() {
        assert_eq!(parse_category("CORE"), MemoryCategory::Core);
        assert_eq!(parse_category("Daily"), MemoryCategory::Daily);
        assert_eq!(parse_category("CONVERSATION"), MemoryCategory::Conversation);
    }

    #[test]
    fn normalize_key_handles_empty_string() {
        let key = normalize_key("", 42);
        assert_eq!(key, "openclaw_42");
    }

    #[test]
    fn normalize_key_trims_whitespace() {
        let key = normalize_key("  my_key  ", 0);
        assert_eq!(key, "my_key");
    }

    #[test]
    fn parse_structured_markdown_rejects_empty_key() {
        assert!(parse_structured_memory_line("****:value").is_none());
    }

    #[test]
    fn parse_structured_markdown_rejects_empty_value() {
        assert!(parse_structured_memory_line("**key**:").is_none());
    }

    #[test]
    fn parse_structured_markdown_rejects_no_stars() {
        assert!(parse_structured_memory_line("key: value").is_none());
    }

    #[tokio::test]
    async fn migration_skips_empty_content() {
        let dir = TempDir::new().unwrap();
        let db_path = dir.path().join("brain.db");
        let conn = Connection::open(&db_path).unwrap();

        conn.execute_batch("CREATE TABLE memories (key TEXT, content TEXT, category TEXT);")
            .unwrap();
        conn.execute(
            "INSERT INTO memories (key, content, category) VALUES (?1, ?2, ?3)",
            params!["empty_key", "   ", "core"],
        )
        .unwrap();

        let rows = read_openclaw_sqlite_entries(&db_path).unwrap();
        assert_eq!(
            rows.len(),
            0,
            "entries with empty/whitespace content must be skipped"
        );
    }

    #[test]
    fn backup_creates_timestamped_directory() {
        let tmp = TempDir::new().unwrap();
        let mem_dir = tmp.path().join("memory");
        std::fs::create_dir_all(&mem_dir).unwrap();

        // Create a brain.db to back up
        let db_path = mem_dir.join("brain.db");
        std::fs::write(&db_path, "fake db content").unwrap();

        let result = backup_target_memory(tmp.path()).unwrap();
        assert!(
            result.is_some(),
            "backup should be created when files exist"
        );

        let backup_dir = result.unwrap();
        assert!(backup_dir.exists());
        assert!(
            backup_dir.to_string_lossy().contains("openclaw-"),
            "backup dir must contain openclaw- prefix"
        );
    }

    #[test]
    fn backup_returns_none_when_no_files() {
        let tmp = TempDir::new().unwrap();
        let result = backup_target_memory(tmp.path()).unwrap();
        assert!(
            result.is_none(),
            "backup should return None when no files to backup"
        );
    }
}
