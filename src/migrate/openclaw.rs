use crate::db::Registry;
use crate::migrate::report::{CreatedEntry, MigrationReport, SecretCollector};
use anyhow::{Context, Result};
use serde::Deserialize;
use serde_json::Value;
use std::collections::{HashMap, HashSet};
use std::path::{Path, PathBuf};

// ── OpenClaw JSON structures ────────────────────────────────────

#[derive(Deserialize)]
#[serde(rename_all = "camelCase")]
struct OpenClawConfig {
    agents: AgentsSection,
    #[serde(default)]
    bindings: Vec<Binding>,
    #[serde(default)]
    channels: Option<ChannelsSection>,
    #[serde(flatten)]
    extra: HashMap<String, Value>,
}

#[derive(Deserialize)]
#[serde(rename_all = "camelCase")]
struct AgentsSection {
    #[serde(default)]
    defaults: AgentDefaults,
    list: Vec<AgentEntry>,
}

#[derive(Deserialize, Default)]
#[serde(rename_all = "camelCase")]
struct AgentDefaults {
    #[serde(default)]
    model: Option<ModelRef>,
    #[serde(default)]
    workspace: Option<String>,
    #[serde(default)]
    heartbeat: Option<HeartbeatRef>,
    #[serde(flatten)]
    extra: HashMap<String, Value>,
}

#[derive(Deserialize, Clone)]
#[serde(rename_all = "camelCase")]
struct ModelRef {
    #[serde(default)]
    primary: Option<String>,
}

#[derive(Deserialize, Clone)]
#[serde(rename_all = "camelCase")]
struct HeartbeatRef {
    #[serde(default)]
    every: Option<String>,
}

#[derive(Deserialize)]
#[serde(rename_all = "camelCase")]
struct AgentEntry {
    id: String,
    #[serde(default)]
    name: Option<String>,
    #[serde(default)]
    workspace: Option<String>,
    #[serde(default)]
    agent_dir: Option<String>,
    #[serde(default)]
    model: Option<String>,
    #[serde(default)]
    heartbeat: Option<HeartbeatRef>,
    #[serde(flatten)]
    extra: HashMap<String, Value>,
}

#[derive(Deserialize)]
#[serde(rename_all = "camelCase")]
struct Binding {
    #[serde(default)]
    agent_id: Option<String>,
    #[serde(rename = "match")]
    #[serde(default)]
    match_: Option<BindingMatch>,
    #[serde(default)]
    account: Option<String>,
}

#[derive(Deserialize)]
#[serde(rename_all = "camelCase")]
struct BindingMatch {
    #[serde(default)]
    channel: Option<String>,
}

#[derive(Deserialize)]
#[serde(rename_all = "camelCase")]
struct ChannelsSection {
    #[serde(default)]
    telegram: Option<ChannelsTelegram>,
}

#[derive(Deserialize)]
#[serde(rename_all = "camelCase")]
struct ChannelsTelegram {
    #[serde(default)]
    accounts: HashMap<String, TelegramAccount>,
    #[serde(flatten)]
    extra: HashMap<String, Value>,
}

#[derive(Deserialize)]
#[serde(rename_all = "camelCase")]
struct TelegramAccount {
    #[serde(default)]
    bot_token: Option<String>,
    #[serde(default)]
    allow_from: Option<Vec<String>>,
}

// ── Per-agent models.json ───────────────────────────────────────

#[derive(Deserialize)]
#[serde(rename_all = "camelCase")]
struct ModelsJson {
    #[serde(default)]
    providers: HashMap<String, ProviderEntry>,
}

#[derive(Deserialize)]
#[serde(rename_all = "camelCase")]
struct ProviderEntry {
    #[serde(default, rename = "apiKey")]
    api_key: Option<String>,
}

// ── Resolution types ────────────────────────────────────────────

struct ResolvedAgent {
    agent_id: String,
    source_path: String,
    provider: Option<String>,
    model: Option<String>,
    api_key: Option<String>,
    heartbeat_minutes: Option<i64>,
    workspace_path: Option<String>,
    workspace_canonical_or_raw: Option<PathBuf>,
    telegram: Option<ResolvedTelegram>,
    port: u16,
}

struct ResolvedTelegram {
    bot_token: String,
    allowed_users: Vec<String>,
}

// ── Staged instance (ready to commit) ───────────────────────────

pub struct StagedInstance {
    pub id: String,
    pub name: String,
    pub port: u16,
    pub config_toml: String,
    pub workspace_dir: Option<String>,
    pub instance_dir: PathBuf,
}

// ── Public entry point ──────────────────────────────────────────

pub fn run_openclaw_migration(
    config_path: &Path,
    dry_run: bool,
    cp_dir: &Path,
    registry: &Registry,
    instances_dir: &Path,
) -> Result<MigrationReport> {
    let source_path = config_path
        .to_str()
        .unwrap_or("(non-utf8)")
        .to_string();
    let openclaw_dir = config_path
        .parent()
        .context("Config path has no parent directory")?;

    let raw = std::fs::read_to_string(config_path)
        .with_context(|| format!("Failed to read {}", config_path.display()))?;
    let config: OpenClawConfig =
        serde_json::from_str(&raw).context("Failed to parse OpenClaw config JSON")?;

    let mut warnings = Vec::new();
    let mut errors = Vec::new();
    let mut secrets = SecretCollector::new();

    // Check for unsupported top-level fields
    collect_unsupported_warnings(&config.extra, "config", &mut warnings);
    collect_unsupported_warnings(&config.agents.defaults.extra, "agents.defaults", &mut warnings);
    if let Some(ref channels) = config.channels {
        if let Some(ref tg) = channels.telegram {
            collect_telegram_channel_warnings(&tg.extra, &mut warnings);
        }
    }

    // Check for duplicate agent IDs
    let mut seen_ids = HashSet::new();
    for a in &config.agents.list {
        if !seen_ids.insert(&a.id) {
            errors.push(format!("Duplicate agent ID '{}'", a.id));
        }
        collect_unsupported_warnings(&a.extra, &format!("agent '{}'", a.id), &mut warnings);
    }

    // Check name collisions (active + archived)
    for a in &config.agents.list {
        if let Some(ex) = registry.get_instance_by_name(&a.id)? {
            errors.push(format!(
                "Active instance '{}' exists (id: {})",
                a.id, ex.id
            ));
        }
        if let Some(arc) = registry.find_archived_instance_by_name(&a.id)? {
            errors.push(format!(
                "Archived instance '{}' exists (id: {}). \
                 Permanently delete the archived instance before migrating.",
                a.id, arc.id
            ));
        }
    }

    // Resolve each agent
    let mut resolved = Vec::new();
    let mut allocated_ports: Vec<u16> = Vec::new();

    for agent in &config.agents.list {
        match resolve_agent(
            agent,
            &config.agents.defaults,
            &config.bindings,
            &config.channels,
            openclaw_dir,
            &source_path,
            registry,
            &allocated_ports,
            &mut secrets,
            &mut warnings,
        ) {
            Ok(r) => {
                allocated_ports.push(r.port);
                resolved.push(r);
            }
            Err(e) => {
                errors.push(secrets.scrub(&e.to_string()));
            }
        }
    }

    // Check workspace overlap within batch
    let mut ws_owners: HashMap<PathBuf, String> = HashMap::new();
    for r in &resolved {
        if let Some(ref ws) = r.workspace_canonical_or_raw {
            if let Some(other) = ws_owners.get(ws) {
                errors.push(format!(
                    "Agents '{}' and '{}' share workspace '{}'",
                    other,
                    r.agent_id,
                    ws.display()
                ));
            } else {
                ws_owners.insert(ws.clone(), r.agent_id.clone());
            }
        }
    }

    // If any errors, report all and abort
    if !errors.is_empty() {
        return Ok(MigrationReport {
            dry_run,
            source_path,
            created: Vec::new(),
            warnings,
            errors,
        });
    }

    // Build staged instances
    let staged: Vec<StagedInstance> = resolved
        .iter()
        .map(|r| {
            let id = uuid::Uuid::new_v4().to_string();
            let instance_dir = instances_dir.join(&id);
            StagedInstance {
                id: id.clone(),
                name: r.agent_id.clone(),
                port: r.port,
                config_toml: build_config_toml(r),
                workspace_dir: r.workspace_path.clone(),
                instance_dir,
            }
        })
        .collect();

    // Build report entries
    let created: Vec<CreatedEntry> = staged
        .iter()
        .zip(resolved.iter())
        .map(|(s, r)| CreatedEntry {
            agent_id: r.agent_id.clone(),
            instance_id: s.id.clone(),
            instance_name: s.name.clone(),
            port: s.port,
            workspace_path: r
                .workspace_path
                .as_deref()
                .unwrap_or("(none)")
                .to_string(),
            channels: if r.telegram.is_some() {
                vec!["telegram".into()]
            } else {
                vec![]
            },
        })
        .collect();

    if dry_run {
        return Ok(MigrationReport {
            dry_run: true,
            source_path,
            created,
            warnings,
            errors: Vec::new(),
        });
    }

    // Commit phase
    let run_id = uuid::Uuid::new_v4().to_string();
    let pending = cp_dir.join(format!("migration-pending-{run_id}.json"));
    let done = cp_dir.join(format!("migration-done-{run_id}.json"));

    write_manifest(&pending, &run_id, &staged, instances_dir, &source_path)?;

    let mut committed: Vec<usize> = Vec::new();
    for (idx, s) in staged.iter().enumerate() {
        match commit_one(s, registry, &run_id) {
            Ok(()) => committed.push(idx),
            Err(e) => {
                // Clean failing item's FS artifacts
                let failing_cleaned = if s.instance_dir.exists() {
                    std::fs::remove_dir_all(&s.instance_dir).is_ok()
                } else {
                    true
                };

                let rb_errs = rollback_committed(&committed, &staged, registry);

                // Only remove manifest if ALL cleanup succeeded
                if rb_errs.is_empty() && failing_cleaned {
                    let _ = std::fs::remove_file(&pending);
                }

                let mut report_errors = vec![secrets.scrub(&e.to_string())];
                for rb_e in &rb_errs {
                    report_errors.push(format!("Rollback error: {rb_e}"));
                }

                return Ok(MigrationReport {
                    dry_run: false,
                    source_path,
                    created: Vec::new(),
                    warnings,
                    errors: report_errors,
                });
            }
        }
    }

    // All committed -> rename pending to done (atomic)
    std::fs::rename(&pending, &done)?;
    let _ = std::fs::remove_file(&done);

    Ok(MigrationReport {
        dry_run: false,
        source_path,
        created,
        warnings,
        errors: Vec::new(),
    })
}

// ── Agent resolution ────────────────────────────────────────────

#[allow(clippy::too_many_arguments)]
fn resolve_agent(
    agent: &AgentEntry,
    defaults: &AgentDefaults,
    bindings: &[Binding],
    channels: &Option<ChannelsSection>,
    openclaw_dir: &Path,
    source_path: &str,
    registry: &Registry,
    already_allocated: &[u16],
    secrets: &mut SecretCollector,
    warnings: &mut Vec<String>,
) -> Result<ResolvedAgent> {
    // Model resolution
    let model_str = agent.model.as_deref().or_else(|| {
        defaults
            .model
            .as_ref()
            .and_then(|m| m.primary.as_deref())
    });

    let (provider, model) = if let Some(m) = model_str {
        if let Some(slash_pos) = m.find('/') {
            (
                Some(m[..slash_pos].to_string()),
                Some(m[slash_pos + 1..].to_string()),
            )
        } else {
            warnings.push(format!(
                "Agent '{}': model '{}' has no provider prefix (no '/')",
                agent.id, m
            ));
            (None, Some(m.to_string()))
        }
    } else {
        (None, None)
    };

    // Heartbeat resolution
    let heartbeat_minutes = agent
        .heartbeat
        .as_ref()
        .or(defaults.heartbeat.as_ref())
        .and_then(|hb| hb.every.as_deref())
        .and_then(|s| parse_duration_minutes(s));

    // Workspace resolution
    let (workspace_path, workspace_canonical_or_raw) = resolve_workspace(
        agent.workspace.as_deref().or(defaults.workspace.as_deref()),
        openclaw_dir,
        &agent.id,
        warnings,
    );

    // API key resolution
    let api_key = resolve_api_key(agent, openclaw_dir, provider.as_deref(), secrets, warnings);

    // Telegram binding resolution
    let telegram = resolve_telegram_binding(
        &agent.id,
        bindings,
        channels,
        secrets,
        warnings,
    );

    // Port allocation
    let port = registry
        .allocate_port_with_excludes(18801, 18999, already_allocated)?
        .with_context(|| format!("No available port for agent '{}'", agent.id))?;

    // Warn about non-telegram bindings
    for b in bindings {
        if b.agent_id.as_deref() == Some(&agent.id) {
            if let Some(ref m) = b.match_ {
                if let Some(ref ch) = m.channel {
                    if ch != "telegram" {
                        warnings.push(format!(
                            "Agent '{}': channel type '{}' not supported, skipping binding",
                            agent.id, ch
                        ));
                    }
                }
            }
        }
    }

    Ok(ResolvedAgent {
        agent_id: agent.id.clone(),
        source_path: source_path.to_string(),
        provider,
        model,
        api_key,
        heartbeat_minutes,
        workspace_path,
        workspace_canonical_or_raw,
        telegram,
        port,
    })
}

fn resolve_workspace(
    workspace_str: Option<&str>,
    openclaw_dir: &Path,
    agent_id: &str,
    warnings: &mut Vec<String>,
) -> (Option<String>, Option<PathBuf>) {
    let ws = match workspace_str {
        Some(s) => s,
        None => return (None, None),
    };

    let resolved = if Path::new(ws).is_absolute() {
        PathBuf::from(ws)
    } else {
        openclaw_dir.join(ws)
    };

    match std::fs::canonicalize(&resolved) {
        Ok(canonical) => (
            Some(canonical.to_string_lossy().to_string()),
            Some(canonical),
        ),
        Err(_) => {
            warnings.push(format!(
                "Agent '{}': workspace '{}' could not be canonicalized (may not exist yet)",
                agent_id,
                resolved.display()
            ));
            (
                Some(resolved.to_string_lossy().to_string()),
                Some(resolved),
            )
        }
    }
}

fn resolve_api_key(
    agent: &AgentEntry,
    openclaw_dir: &Path,
    provider: Option<&str>,
    secrets: &mut SecretCollector,
    warnings: &mut Vec<String>,
) -> Option<String> {
    let models_path = if let Some(ref agent_dir) = agent.agent_dir {
        let dir = if Path::new(agent_dir).is_absolute() {
            PathBuf::from(agent_dir)
        } else {
            openclaw_dir.join(agent_dir)
        };
        dir.join("models.json")
    } else {
        openclaw_dir
            .join("agents")
            .join(&agent.id)
            .join("agent")
            .join("models.json")
    };

    let raw = match std::fs::read_to_string(&models_path) {
        Ok(s) => s,
        Err(_) => {
            warnings.push(format!(
                "Agent '{}': models.json not found at {}",
                agent.id,
                models_path.display()
            ));
            return None;
        }
    };

    let models: ModelsJson = match serde_json::from_str(&raw) {
        Ok(m) => m,
        Err(e) => {
            warnings.push(format!(
                "Agent '{}': failed to parse models.json: {e}",
                agent.id
            ));
            return None;
        }
    };

    let provider_name = provider.unwrap_or("default");
    if let Some(entry) = models.providers.get(provider_name) {
        if let Some(ref key) = entry.api_key {
            secrets.add(key);
            return Some(key.clone());
        }
    }

    // Try first available provider if specific not found
    for (_name, entry) in &models.providers {
        if let Some(ref key) = entry.api_key {
            secrets.add(key);
            return Some(key.clone());
        }
    }

    warnings.push(format!(
        "Agent '{}': no API key found in models.json",
        agent.id
    ));
    None
}

fn resolve_telegram_binding(
    agent_id: &str,
    bindings: &[Binding],
    channels: &Option<ChannelsSection>,
    secrets: &mut SecretCollector,
    warnings: &mut Vec<String>,
) -> Option<ResolvedTelegram> {
    let binding = bindings.iter().find(|b| {
        b.agent_id.as_deref() == Some(agent_id)
            && b.match_
                .as_ref()
                .and_then(|m| m.channel.as_deref())
                == Some("telegram")
    })?;

    let account_name = binding.account.as_deref().unwrap_or("default");
    let channels_section = channels.as_ref()?;
    let telegram = channels_section.telegram.as_ref()?;
    let account = match telegram.accounts.get(account_name) {
        Some(a) => a,
        None => {
            warnings.push(format!(
                "Agent '{}': telegram account '{}' not found in channels config",
                agent_id, account_name
            ));
            return None;
        }
    };

    let bot_token = match &account.bot_token {
        Some(t) => {
            secrets.add(t);
            t.clone()
        }
        None => {
            warnings.push(format!(
                "Agent '{}': telegram account '{}' has no bot_token",
                agent_id, account_name
            ));
            return None;
        }
    };

    let allowed_users = account.allow_from.clone().unwrap_or_default();

    Some(ResolvedTelegram {
        bot_token,
        allowed_users,
    })
}

// ── Duration parsing ────────────────────────────────────────────

fn parse_duration_minutes(s: &str) -> Option<i64> {
    let s = s.trim();
    let mut total_minutes: i64 = 0;
    let mut current_num = String::new();

    for c in s.chars() {
        if c.is_ascii_digit() {
            current_num.push(c);
        } else {
            let n: i64 = current_num.parse().ok()?;
            current_num.clear();
            match c {
                'h' => total_minutes += n * 60,
                'm' => total_minutes += n,
                _ => return None,
            }
        }
    }

    if total_minutes > 0 {
        Some(total_minutes)
    } else {
        None
    }
}

// ── TOML builder ────────────────────────────────────────────────

fn build_config_toml(r: &ResolvedAgent) -> String {
    let mut doc = toml_edit::DocumentMut::new();
    doc.decor_mut().set_prefix(format!(
        "# Migrated from OpenClaw agent: {}\n# Source: {}\n# Original workspace: {}\n\n",
        r.agent_id,
        r.source_path,
        r.workspace_path.as_deref().unwrap_or("(none)")
    ));

    if let Some(ref k) = r.api_key {
        doc["api_key"] = toml_edit::value(k.as_str());
    }
    if let Some(ref p) = r.provider {
        doc["default_provider"] = toml_edit::value(p.as_str());
    }
    if let Some(ref m) = r.model {
        doc["default_model"] = toml_edit::value(m.as_str());
    }
    doc["default_temperature"] = toml_edit::value(0.7);
    if let Some(mins) = r.heartbeat_minutes {
        let mut hb = toml_edit::Table::new();
        hb["enabled"] = toml_edit::value(true);
        hb["interval_minutes"] = toml_edit::value(mins);
        doc["heartbeat"] = toml_edit::Item::Table(hb);
    }

    // Gateway always present
    let mut gw = toml_edit::Table::new();
    gw["port"] = toml_edit::value(i64::from(r.port));
    gw["host"] = toml_edit::value("127.0.0.1");
    gw["require_pairing"] = toml_edit::value(true);
    doc["gateway"] = toml_edit::Item::Table(gw);

    // Telegram only if binding exists
    if let Some(ref tg) = r.telegram {
        let mut t = toml_edit::Table::new();
        t["bot_token"] = toml_edit::value(tg.bot_token.as_str());
        let mut users = toml_edit::Array::new();
        for u in &tg.allowed_users {
            users.push(u.as_str());
        }
        t["allowed_users"] = toml_edit::value(users);
        let mut cc = toml_edit::Table::new();
        cc["cli"] = toml_edit::value(true);
        cc["telegram"] = toml_edit::Item::Table(t);
        doc["channels_config"] = toml_edit::Item::Table(cc);
    }

    doc.to_string()
}

// ── Unsupported field warnings ──────────────────────────────────

fn collect_unsupported_warnings(
    extra: &HashMap<String, Value>,
    context: &str,
    warnings: &mut Vec<String>,
) {
    let known_warnings: &[(&str, &str)] = &[
        ("tools", "Tool definitions are not migrated (ZeroClaw uses built-in tools)"),
        ("messages", "Message history is not migrated"),
        ("commands", "Custom commands are not migrated"),
        ("skills", "Skill definitions are not migrated"),
        ("plugins", "Plugin configurations are not migrated"),
        ("gateway", "Gateway config in OpenClaw format is not migrated (ZeroClaw uses its own gateway config)"),
        ("auth", "Auth configuration is not migrated"),
        ("contextPruning", "Context pruning settings are not migrated"),
        ("compaction", "Compaction settings are not migrated"),
        ("subagents", "Subagent definitions are not migrated"),
        ("maxConcurrent", "Concurrency limits are not migrated"),
        ("memorySearch", "Memory search settings are not migrated"),
    ];

    for (key, _value) in extra {
        if let Some((_, desc)) = known_warnings.iter().find(|(k, _)| k == key) {
            warnings.push(format!("{context}: {desc}"));
        } else {
            warnings.push(format!("{context}: unsupported field '{key}'"));
        }
    }
}

fn collect_telegram_channel_warnings(
    extra: &HashMap<String, Value>,
    warnings: &mut Vec<String>,
) {
    let known: &[(&str, &str)] = &[
        ("streamMode", "Telegram stream mode is not migrated"),
        ("dmPolicy", "Telegram DM policy is not migrated"),
        ("groupPolicy", "Telegram group policy is not migrated"),
    ];

    for (key, _value) in extra {
        if let Some((_, desc)) = known.iter().find(|(k, _)| k == key) {
            warnings.push(format!("channels.telegram: {desc}"));
        } else {
            warnings.push(format!("channels.telegram: unsupported field '{key}'"));
        }
    }
}

// ── Atomic file write (fsync + rename + dir-fsync) ──────────────

/// Write bytes atomically: temp file -> fsync -> rename -> dir-fsync.
/// Used for both config.toml (0600) and manifest JSON.
fn write_file_atomic(path: &Path, content: &[u8]) -> Result<()> {
    write_file_atomic_inner(path, content, None)
}

/// Write bytes atomically with explicit Unix permissions.
fn write_file_atomic_mode(path: &Path, content: &[u8], mode: u32) -> Result<()> {
    write_file_atomic_inner(path, content, Some(mode))
}

fn write_file_atomic_inner(path: &Path, content: &[u8], _mode: Option<u32>) -> Result<()> {
    use std::io::Write;
    #[cfg(unix)]
    use std::os::unix::fs::OpenOptionsExt;

    let dir = path.parent().context("File path has no parent")?;
    let temp = dir.join(format!(".tmp-{}", uuid::Uuid::new_v4()));

    #[cfg(unix)]
    let mut f = {
        let mut opts = std::fs::OpenOptions::new();
        opts.write(true).create_new(true);
        if let Some(m) = _mode {
            opts.mode(m);
        }
        opts.open(&temp)?
    };
    #[cfg(not(unix))]
    let mut f = std::fs::OpenOptions::new()
        .write(true)
        .create_new(true)
        .open(&temp)?;

    f.write_all(content)?;
    f.sync_all()?;

    std::fs::rename(&temp, path)?;

    // fsync directory for rename durability
    let dir_fd = std::fs::File::open(dir)?;
    dir_fd.sync_all()?;
    Ok(())
}

// ── Manifest ────────────────────────────────────────────────────

#[derive(serde::Serialize, serde::Deserialize)]
pub struct Manifest {
    pub run_id: String,
    pub pid: u32,
    pub started_at: String,
    pub source_path: String,
    pub instances_dir: String,
    pub instance_ids: Vec<String>,
}

fn write_manifest(
    path: &Path,
    run_id: &str,
    staged: &[StagedInstance],
    instances_dir: &Path,
    source_path: &str,
) -> Result<()> {
    let manifest = Manifest {
        run_id: run_id.to_string(),
        pid: std::process::id(),
        started_at: chrono::Utc::now().to_rfc3339(),
        source_path: source_path.to_string(),
        instances_dir: instances_dir.to_string_lossy().to_string(),
        instance_ids: staged.iter().map(|s| s.id.clone()).collect(),
    };
    let json = serde_json::to_string_pretty(&manifest)?;
    write_file_atomic(path, json.as_bytes())?;
    Ok(())
}

// ── Commit / Rollback ───────────────────────────────────────────

fn commit_one(s: &StagedInstance, registry: &Registry, run_id: &str) -> Result<()> {
    // Create instance directory
    std::fs::create_dir_all(&s.instance_dir).with_context(|| {
        format!(
            "Failed to create instance dir: {}",
            s.instance_dir.display()
        )
    })?;

    // Write .migration-created marker (fsync file + dir)
    write_marker_file(&s.instance_dir)?;

    // Write config.toml atomically with 0600 perms
    let config_path = s.instance_dir.join("config.toml");
    write_file_atomic_mode(&config_path, s.config_toml.as_bytes(), 0o600)?;

    // Register in DB
    registry.create_instance(
        &s.id,
        &s.name,
        s.port,
        &config_path.to_string_lossy(),
        s.workspace_dir.as_deref(),
        Some(run_id),
    )?;

    Ok(())
}

fn write_marker_file(instance_dir: &Path) -> Result<()> {
    use std::io::Write;
    #[cfg(unix)]
    use std::os::unix::fs::OpenOptionsExt;

    let marker = instance_dir.join(".migration-created");
    #[cfg(unix)]
    let mut f = std::fs::OpenOptions::new()
        .write(true)
        .create_new(true)
        .mode(0o600)
        .open(&marker)?;
    #[cfg(not(unix))]
    let mut f = std::fs::OpenOptions::new()
        .write(true)
        .create_new(true)
        .open(&marker)?;
    f.write_all(b"")?;
    f.sync_all()?;

    // fsync directory
    let dir_fd = std::fs::File::open(instance_dir)?;
    dir_fd.sync_all()?;
    Ok(())
}

fn rollback_committed(
    committed_indices: &[usize],
    staged: &[StagedInstance],
    registry: &Registry,
) -> Vec<String> {
    let mut errors = Vec::new();
    // Reverse order
    for &idx in committed_indices.iter().rev() {
        let s = &staged[idx];
        // Delete from DB (best-effort)
        if let Err(e) = registry
            .conn()
            .execute("DELETE FROM instances WHERE id = ?1", rusqlite::params![s.id])
        {
            errors.push(format!("DB rollback failed for {}: {e}", s.id));
        }
        // Delete from FS (best-effort)
        if s.instance_dir.exists() {
            if let Err(e) = std::fs::remove_dir_all(&s.instance_dir) {
                errors.push(format!("FS rollback failed for {}: {e}", s.id));
            }
        }
    }
    errors
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn parse_duration_simple_minutes() {
        assert_eq!(parse_duration_minutes("30m"), Some(30));
    }

    #[test]
    fn parse_duration_hours() {
        assert_eq!(parse_duration_minutes("1h"), Some(60));
    }

    #[test]
    fn parse_duration_combined() {
        assert_eq!(parse_duration_minutes("2h30m"), Some(150));
    }

    #[test]
    fn parse_duration_invalid() {
        assert_eq!(parse_duration_minutes("abc"), None);
    }

    #[test]
    fn build_config_toml_basic() {
        let r = ResolvedAgent {
            agent_id: "test".into(),
            source_path: "/test/openclaw.json".into(),
            provider: Some("anthropic".into()),
            model: Some("claude-sonnet-4-20250514".into()),
            api_key: Some("sk-ant-test".into()),
            heartbeat_minutes: Some(30),
            workspace_path: Some("/home/user/projects".into()),
            workspace_canonical_or_raw: Some(PathBuf::from("/home/user/projects")),
            telegram: Some(ResolvedTelegram {
                bot_token: "123:ABC".into(),
                allowed_users: vec!["user1".into()],
            }),
            port: 18801,
        };

        let toml_str = build_config_toml(&r);
        assert!(toml_str.contains("api_key = \"sk-ant-test\""));
        assert!(toml_str.contains("default_provider = \"anthropic\""));
        assert!(toml_str.contains("default_model = \"claude-sonnet-4-20250514\""));
        assert!(toml_str.contains("port = 18801"));
        assert!(toml_str.contains("require_pairing = true"));
        assert!(toml_str.contains("bot_token = \"123:ABC\""));
        assert!(toml_str.contains("[heartbeat]"));
        assert!(toml_str.contains("interval_minutes = 30"));
    }

    #[test]
    fn build_config_toml_minimal() {
        let r = ResolvedAgent {
            agent_id: "bare".into(),
            source_path: "/test/oc.json".into(),
            provider: None,
            model: None,
            api_key: None,
            heartbeat_minutes: None,
            workspace_path: None,
            workspace_canonical_or_raw: None,
            telegram: None,
            port: 18805,
        };

        let toml_str = build_config_toml(&r);
        assert!(!toml_str.contains("api_key"));
        assert!(!toml_str.contains("default_provider"));
        assert!(!toml_str.contains("[heartbeat]"));
        assert!(!toml_str.contains("[channels_config"));
        assert!(toml_str.contains("port = 18805"));
        assert!(toml_str.contains("[gateway]"));
    }

    #[test]
    fn unsupported_field_warnings_known_keys() {
        let mut extra = HashMap::new();
        extra.insert("tools".to_string(), Value::Null);
        extra.insert("plugins".to_string(), Value::Null);

        let mut warnings = Vec::new();
        collect_unsupported_warnings(&extra, "config", &mut warnings);

        assert_eq!(warnings.len(), 2);
        assert!(warnings.iter().any(|w| w.contains("Tool definitions")));
        assert!(warnings.iter().any(|w| w.contains("Plugin configurations")));
    }

    #[test]
    fn unsupported_field_warnings_unknown_keys() {
        let mut extra = HashMap::new();
        extra.insert("fooBar".to_string(), Value::Null);

        let mut warnings = Vec::new();
        collect_unsupported_warnings(&extra, "config", &mut warnings);

        assert_eq!(warnings.len(), 1);
        assert!(warnings[0].contains("unsupported field 'fooBar'"));
    }
}
