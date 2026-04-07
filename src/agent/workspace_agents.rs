//! Workspace-based agent definitions with hot-reload via file watcher.
//!
//! Agents are defined as folders under `workspace/agents/<name>/` containing a
//! `config.toml` (parsed as [`WorkspaceAgentConfig`]), an optional `IDENTITY.md`
//! system prompt, optional `TOOLS.md` tool guidance, and an optional `skills/`
//! subdirectory.  A sibling `common/` folder provides shared `.md` context and
//! skills inherited by all workspace agents.
//!
//! The [`WorkspaceAgentManager`] loads these on startup and optionally watches
//! for file-system changes to hot-reload agent definitions at runtime.

use std::collections::{HashMap, HashSet};
use std::path::{Path, PathBuf};
use std::sync::Arc;

use anyhow::{Context, Result};
use notify::{EventKind, RecommendedWatcher, RecursiveMode, Watcher};
use parking_lot::RwLock;
use tracing::{debug, error, info, warn};

use crate::config::schema::{DelegateAgentConfig, WorkspaceAgentConfig};

/// Manages workspace-folder-based agent definitions with optional hot-reload.
pub struct WorkspaceAgentManager {
    /// Shared mutable agent registry (config agents + workspace agents + ad-hoc ephemeral).
    agents: Arc<RwLock<HashMap<String, DelegateAgentConfig>>>,
    /// Workspace root path (parent of `agents/`).
    workspace_dir: PathBuf,
    /// Global config defaults.
    default_provider: String,
    default_model: String,
    /// Names of agents defined in the static TOML config (protected from overwrite).
    config_agent_names: HashSet<String>,
    /// File watcher handle (kept alive so watcher doesn't drop).
    watcher_handle: Option<RecommendedWatcher>,
}

impl WorkspaceAgentManager {
    /// Create a new manager, seeding the registry with `config_agents` and then
    /// scanning `workspace_dir/agents/` for folder-based definitions.
    ///
    /// Config-defined agents take priority: a workspace agent whose name
    /// collides with a config agent is silently skipped.
    pub fn new(
        workspace_dir: PathBuf,
        config_agents: HashMap<String, DelegateAgentConfig>,
        default_provider: String,
        default_model: String,
    ) -> Self {
        let config_agent_names: HashSet<String> = config_agents.keys().cloned().collect();
        let mut agents = config_agents;

        let agents_dir = workspace_dir.join("agents");
        let common_dir = agents_dir.join("common");

        // Ensure workspace agent directories exist on boot.
        for dir in [&agents_dir, &common_dir] {
            if !dir.is_dir() {
                if let Err(e) = std::fs::create_dir_all(dir) {
                    warn!(path = %dir.display(), error = %e, "failed to create workspace agent directory");
                } else {
                    info!(path = %dir.display(), "created workspace agent directory");
                }
            }
        }

        let mut workspace_count = 0u32;

        if agents_dir.is_dir() {
            if let Ok(entries) = std::fs::read_dir(&agents_dir) {
                for entry in entries.flatten() {
                    let path = entry.path();
                    if !path.is_dir() {
                        continue;
                    }
                    let name = match path.file_name().and_then(|n| n.to_str()) {
                        Some(n) => n.to_owned(),
                        None => continue,
                    };
                    // Skip the `common` directory — it holds shared context, not an agent.
                    if name == "common" {
                        continue;
                    }
                    // Config agents take priority.
                    if config_agent_names.contains(&name) {
                        debug!(
                            agent = %name,
                            "workspace agent skipped: config-defined agent has priority"
                        );
                        continue;
                    }
                    if !path.join("config.toml").exists() {
                        continue;
                    }
                    match load_workspace_agent(
                        &path,
                        &common_dir,
                        &default_provider,
                        &default_model,
                    ) {
                        Ok(delegate) => {
                            info!(agent = %name, "loaded workspace agent");
                            agents.insert(name, delegate);
                            workspace_count += 1;
                        }
                        Err(e) => {
                            warn!(agent = %name, error = %e, "failed to load workspace agent");
                        }
                    }
                }
            }
        }

        info!(count = workspace_count, "workspace agents loaded");

        Self {
            agents: Arc::new(RwLock::new(agents)),
            workspace_dir,
            default_provider,
            default_model,
            config_agent_names,
            watcher_handle: None,
        }
    }

    /// Start a recursive file watcher on `workspace_dir/agents/`.
    ///
    /// On `Create`/`Modify` events for `config.toml` or `.md` files the
    /// affected agent is reloaded.  On `Remove` events the agent is removed
    /// from the registry (only if it was workspace-loaded, never config-defined).
    pub fn start_watcher(&mut self) -> Result<()> {
        let agents_dir = self.workspace_dir.join("agents");
        if !agents_dir.is_dir() {
            std::fs::create_dir_all(&agents_dir)
                .with_context(|| format!("creating agents directory: {}", agents_dir.display()))?;
        }

        let (tx, mut rx) = tokio::sync::mpsc::channel::<notify::Event>(100);

        let mut watcher = RecommendedWatcher::new(
            move |res: Result<notify::Event, notify::Error>| {
                if let Ok(event) = res {
                    let _ = tx.blocking_send(event);
                }
            },
            notify::Config::default(),
        )
        .context("creating file watcher")?;

        watcher
            .watch(agents_dir.as_ref(), RecursiveMode::Recursive)
            .with_context(|| format!("watching directory: {}", agents_dir.display()))?;

        let agents_clone = Arc::clone(&self.agents);
        let config_names = self.config_agent_names.clone();
        let common_dir = agents_dir.join("common");
        let agents_root = agents_dir.clone();
        let default_provider = self.default_provider.clone();
        let default_model = self.default_model.clone();

        // Guard against missing Tokio runtime (e.g. in non-async tests).
        let Ok(handle) = tokio::runtime::Handle::try_current() else {
            warn!("no Tokio runtime available — workspace agent watcher disabled");
            self.watcher_handle = Some(watcher);
            return Ok(());
        };
        handle.spawn(async move {
            while let Some(event) = rx.recv().await {
                let dominated_by_relevant_path = event.paths.iter().any(|p| {
                    if let Some(ext) = p.extension().and_then(|e| e.to_str()) {
                        ext == "toml" || ext == "md"
                    } else {
                        false
                    }
                });
                if !dominated_by_relevant_path {
                    continue;
                }

                match event.kind {
                    EventKind::Create(_) | EventKind::Modify(_) => {
                        for path in &event.paths {
                            let agent_name = match extract_agent_name_from_path(path, &agents_root)
                            {
                                Some(n) => n,
                                None => continue,
                            };
                            if agent_name == "common" {
                                // Common dir changed — reload all workspace agents.
                                debug!("common directory changed, reloading all workspace agents");
                                reload_all_workspace_agents(
                                    &agents_clone,
                                    &agents_root,
                                    &common_dir,
                                    &config_names,
                                    &default_provider,
                                    &default_model,
                                );
                                break;
                            }
                            if config_names.contains(&agent_name) {
                                debug!(
                                    agent = %agent_name,
                                    "ignoring watcher event for config-defined agent"
                                );
                                continue;
                            }
                            let agent_dir = agents_root.join(&agent_name);
                            if !agent_dir.join("config.toml").exists() {
                                continue;
                            }
                            match load_workspace_agent(
                                &agent_dir,
                                &common_dir,
                                &default_provider,
                                &default_model,
                            ) {
                                Ok(delegate) => {
                                    info!(agent = %agent_name, "reloaded workspace agent");
                                    agents_clone.write().insert(agent_name, delegate);
                                }
                                Err(e) => {
                                    warn!(
                                        agent = %agent_name,
                                        error = %e,
                                        "failed to reload workspace agent"
                                    );
                                }
                            }
                        }
                    }
                    EventKind::Remove(_) => {
                        for path in &event.paths {
                            let agent_name = match extract_agent_name_from_path(path, &agents_root)
                            {
                                Some(n) => n,
                                None => continue,
                            };
                            if config_names.contains(&agent_name) {
                                continue;
                            }
                            let agent_dir = agents_root.join(&agent_name);
                            if !agent_dir.join("config.toml").exists() {
                                info!(agent = %agent_name, "removed workspace agent");
                                agents_clone.write().remove(&agent_name);
                            }
                        }
                    }
                    _ => {}
                }
            }
        });

        self.watcher_handle = Some(watcher);
        info!(
            path = %agents_dir.display(),
            "workspace agent file watcher started"
        );
        Ok(())
    }

    /// Returns a clone of the shared agent registry handle.
    pub fn agents(&self) -> Arc<RwLock<HashMap<String, DelegateAgentConfig>>> {
        Arc::clone(&self.agents)
    }

    /// Register an ephemeral (ad-hoc) agent that is not persisted to disk.
    pub fn register_ephemeral(&self, name: String, config: DelegateAgentConfig) {
        info!(agent = %name, "registered ephemeral agent");
        self.agents.write().insert(name, config);
    }

    /// Persist an agent from the registry to `workspace_dir/agents/<name>/`.
    ///
    /// Writes `config.toml` from the delegate config fields and `IDENTITY.md`
    /// from the system prompt.  The file watcher will pick up the new files
    /// automatically.
    pub async fn save_agent(&self, name: &str) -> Result<()> {
        let delegate = {
            let lock = self.agents.read();
            lock.get(name)
                .cloned()
                .with_context(|| format!("agent '{name}' not found in registry"))?
        };

        let agent_dir = self.workspace_dir.join("agents").join(name);
        tokio::fs::create_dir_all(&agent_dir)
            .await
            .with_context(|| format!("creating agent directory: {}", agent_dir.display()))?;

        // Build a WorkspaceAgentConfig for serialization.
        let ws_config = WorkspaceAgentConfig {
            provider: Some(delegate.provider.clone()),
            model: Some(delegate.model.clone()),
            temperature: delegate.temperature,
            agentic: delegate.agentic,
            allowed_tools: delegate.allowed_tools.clone(),
            max_iterations: delegate.max_iterations,
            max_depth: delegate.max_depth,
            timeout_secs: delegate.timeout_secs,
            agentic_timeout_secs: delegate.agentic_timeout_secs,
            memory_namespace: delegate.memory_namespace.clone(),
            max_context_tokens: delegate.max_context_tokens,
            max_tool_result_chars: delegate.max_tool_result_chars,
        };

        let toml_str =
            toml::to_string_pretty(&ws_config).context("serializing workspace agent config")?;
        let config_path = agent_dir.join("config.toml");
        tokio::fs::write(&config_path, toml_str)
            .await
            .with_context(|| format!("writing config: {}", config_path.display()))?;

        if let Some(prompt) = &delegate.system_prompt {
            let identity_path = agent_dir.join("IDENTITY.md");
            tokio::fs::write(&identity_path, prompt)
                .await
                .with_context(|| format!("writing identity: {}", identity_path.display()))?;
        }

        info!(agent = %name, path = %agent_dir.display(), "saved workspace agent to disk");
        Ok(())
    }
}

// ── Helpers ─────────────────────────────────────────────────────────

/// Load a single workspace agent from its folder.
///
/// Reads `config.toml`, optional `IDENTITY.md`, optional `TOOLS.md`, and
/// shared `.md` files from the `common_dir`, then assembles them into a
/// [`DelegateAgentConfig`].
fn load_workspace_agent(
    agent_dir: &Path,
    common_dir: &Path,
    default_provider: &str,
    default_model: &str,
) -> Result<DelegateAgentConfig> {
    let agent_name = agent_dir
        .file_name()
        .and_then(|n| n.to_str())
        .unwrap_or("unknown");

    // Parse config.toml
    let config_path = agent_dir.join("config.toml");
    let config_text = std::fs::read_to_string(&config_path)
        .with_context(|| format!("reading {}", config_path.display()))?;
    let workspace_config: WorkspaceAgentConfig = toml::from_str(&config_text)
        .with_context(|| format!("parsing {}", config_path.display()))?;

    // Read optional markdown files
    let identity = read_optional_file(&agent_dir.join("IDENTITY.md"));
    let tools_guidance = read_optional_file(&agent_dir.join("TOOLS.md"));

    // Read shared context from common_dir (top-level .md files only)
    let shared_context = read_common_md_files(common_dir);

    // Assemble system prompt: shared + identity + tools
    let mut prompt_parts: Vec<String> = Vec::new();
    if !shared_context.is_empty() {
        prompt_parts.push(shared_context);
    }
    if let Some(id) = identity {
        prompt_parts.push(id);
    }
    if let Some(tools) = tools_guidance {
        prompt_parts.push(tools);
    }

    let system_prompt = if prompt_parts.is_empty() {
        None
    } else {
        Some(prompt_parts.join("\n\n"))
    };

    let skills_directory = format!("agents/{agent_name}/skills");

    Ok(workspace_config.into_delegate_config(
        agent_name,
        system_prompt,
        Some(skills_directory),
        default_provider,
        default_model,
    ))
}

/// Read a file and return its contents, or `None` if the file doesn't exist
/// or can't be read.
fn read_optional_file(path: &Path) -> Option<String> {
    match std::fs::read_to_string(path) {
        Ok(s) if !s.trim().is_empty() => Some(s),
        _ => None,
    }
}

/// Read all top-level `.md` files from a directory and concatenate them
/// (sorted by filename for deterministic ordering).
fn read_common_md_files(common_dir: &Path) -> String {
    let mut parts: Vec<(String, String)> = Vec::new();
    let entries = match std::fs::read_dir(common_dir) {
        Ok(e) => e,
        Err(_) => return String::new(),
    };

    for entry in entries.flatten() {
        let path = entry.path();
        if !path.is_file() {
            continue;
        }
        let ext = path.extension().and_then(|e| e.to_str());
        if ext != Some("md") {
            continue;
        }
        let name = path
            .file_name()
            .unwrap_or_default()
            .to_string_lossy()
            .to_string();
        if let Ok(content) = std::fs::read_to_string(&path) {
            if !content.trim().is_empty() {
                parts.push((name, content));
            }
        }
    }

    // Sort by filename for deterministic ordering.
    parts.sort_by(|a, b| a.0.cmp(&b.0));
    parts
        .into_iter()
        .map(|(_, content)| content)
        .collect::<Vec<_>>()
        .join("\n\n")
}

/// Extract the agent name from a filesystem event path.
///
/// Given `agents_root = /workspace/agents` and
/// `path = /workspace/agents/researcher/config.toml`, returns `Some("researcher")`.
fn extract_agent_name_from_path(path: &Path, agents_root: &Path) -> Option<String> {
    let relative = path.strip_prefix(agents_root).ok()?;
    let first_component = relative.components().next()?;
    match first_component {
        std::path::Component::Normal(name) => Some(name.to_string_lossy().to_string()),
        _ => None,
    }
}

/// Reload all workspace agents from disk (used when the `common/` directory changes).
fn reload_all_workspace_agents(
    agents: &Arc<RwLock<HashMap<String, DelegateAgentConfig>>>,
    agents_root: &Path,
    common_dir: &Path,
    config_names: &HashSet<String>,
    default_provider: &str,
    default_model: &str,
) {
    let entries = match std::fs::read_dir(agents_root) {
        Ok(e) => e,
        Err(e) => {
            error!(error = %e, "failed to read agents directory during reload");
            return;
        }
    };

    let mut lock = agents.write();
    for entry in entries.flatten() {
        let path = entry.path();
        if !path.is_dir() {
            continue;
        }
        let name = match path.file_name().and_then(|n| n.to_str()) {
            Some(n) => n.to_owned(),
            None => continue,
        };
        if name == "common" || config_names.contains(&name) {
            continue;
        }
        if !path.join("config.toml").exists() {
            continue;
        }
        match load_workspace_agent(&path, common_dir, default_provider, default_model) {
            Ok(delegate) => {
                info!(agent = %name, "reloaded workspace agent (common changed)");
                lock.insert(name, delegate);
            }
            Err(e) => {
                warn!(agent = %name, error = %e, "failed to reload workspace agent");
            }
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use tempfile::TempDir;

    /// Helper: write a minimal workspace agent folder.
    fn write_agent(base: &Path, name: &str, toml_content: &str, identity: Option<&str>) {
        let dir = base.join("agents").join(name);
        std::fs::create_dir_all(&dir).unwrap();
        std::fs::write(dir.join("config.toml"), toml_content).unwrap();
        if let Some(id) = identity {
            std::fs::write(dir.join("IDENTITY.md"), id).unwrap();
        }
    }

    fn write_common_md(base: &Path, filename: &str, content: &str) {
        let dir = base.join("agents").join("common");
        std::fs::create_dir_all(&dir).unwrap();
        std::fs::write(dir.join(filename), content).unwrap();
    }

    #[test]
    fn loads_workspace_agents_on_new() {
        let tmp = TempDir::new().unwrap();
        let base = tmp.path();
        write_agent(
            base,
            "researcher",
            "agentic = true\n",
            Some("You are a researcher."),
        );

        let mgr = WorkspaceAgentManager::new(
            base.to_path_buf(),
            HashMap::new(),
            "openai".into(),
            "gpt-4".into(),
        );

        let agents = mgr.agents.read();
        assert!(agents.contains_key("researcher"));
        let cfg = &agents["researcher"];
        assert!(cfg.agentic);
        assert_eq!(cfg.provider, "openai");
        assert!(cfg.system_prompt.as_deref().unwrap().contains("researcher"));
    }

    #[test]
    fn config_agents_take_priority() {
        let tmp = TempDir::new().unwrap();
        let base = tmp.path();
        write_agent(base, "coder", "agentic = false\n", None);

        let mut config_agents = HashMap::new();
        config_agents.insert(
            "coder".into(),
            DelegateAgentConfig {
                provider: "anthropic".into(),
                model: "claude-3".into(),
                system_prompt: Some("config-defined".into()),
                api_key: None,
                temperature: None,
                max_depth: 3,
                agentic: true,
                allowed_tools: vec![],
                max_iterations: 10,
                timeout_secs: None,
                agentic_timeout_secs: None,
                skills_directory: None,
                memory_namespace: None,
                max_context_tokens: None,
                max_tool_result_chars: None,
            },
        );

        let mgr = WorkspaceAgentManager::new(
            base.to_path_buf(),
            config_agents,
            "openai".into(),
            "gpt-4".into(),
        );

        let agents = mgr.agents.read();
        let cfg = &agents["coder"];
        // Should be the config-defined version, not the workspace one.
        assert_eq!(cfg.provider, "anthropic");
        assert!(cfg.agentic);
        assert_eq!(cfg.system_prompt.as_deref(), Some("config-defined"));
    }

    #[test]
    fn common_md_files_included_in_prompt() {
        let tmp = TempDir::new().unwrap();
        let base = tmp.path();
        write_common_md(base, "SAFETY.md", "Be safe.");
        write_agent(base, "helper", "agentic = true\n", Some("I help."));

        let mgr = WorkspaceAgentManager::new(
            base.to_path_buf(),
            HashMap::new(),
            "openai".into(),
            "gpt-4".into(),
        );

        let agents = mgr.agents.read();
        let prompt = agents["helper"].system_prompt.as_deref().unwrap();
        assert!(prompt.contains("Be safe."));
        assert!(prompt.contains("I help."));
        // Common context comes first.
        assert!(prompt.find("Be safe.").unwrap() < prompt.find("I help.").unwrap());
    }

    #[test]
    fn skips_dirs_without_config_toml() {
        let tmp = TempDir::new().unwrap();
        let base = tmp.path();
        // Create a directory without config.toml
        std::fs::create_dir_all(base.join("agents").join("incomplete")).unwrap();
        std::fs::write(
            base.join("agents").join("incomplete").join("IDENTITY.md"),
            "no config",
        )
        .unwrap();

        let mgr = WorkspaceAgentManager::new(
            base.to_path_buf(),
            HashMap::new(),
            "openai".into(),
            "gpt-4".into(),
        );

        let agents = mgr.agents.read();
        assert!(!agents.contains_key("incomplete"));
    }

    #[test]
    fn extract_agent_name_works() {
        let root = Path::new("/workspace/agents");
        let path = Path::new("/workspace/agents/researcher/config.toml");
        assert_eq!(
            extract_agent_name_from_path(path, root),
            Some("researcher".into())
        );

        let deep = Path::new("/workspace/agents/coder/skills/search.toml");
        assert_eq!(
            extract_agent_name_from_path(deep, root),
            Some("coder".into())
        );

        let outside = Path::new("/other/path/file.toml");
        assert_eq!(extract_agent_name_from_path(outside, root), None);
    }

    #[tokio::test]
    async fn register_ephemeral_adds_agent() {
        let tmp = TempDir::new().unwrap();
        let mgr = WorkspaceAgentManager::new(
            tmp.path().to_path_buf(),
            HashMap::new(),
            "openai".into(),
            "gpt-4".into(),
        );

        mgr.register_ephemeral(
            "temp_agent".into(),
            DelegateAgentConfig {
                provider: "test".into(),
                model: "test-model".into(),
                system_prompt: None,
                api_key: None,
                temperature: None,
                max_depth: 1,
                agentic: false,
                allowed_tools: vec![],
                max_iterations: 5,
                timeout_secs: None,
                agentic_timeout_secs: None,
                skills_directory: None,
                memory_namespace: None,
                max_context_tokens: None,
                max_tool_result_chars: None,
            },
        );

        let agents = mgr.agents.read();
        assert!(agents.contains_key("temp_agent"));
    }

    #[tokio::test]
    async fn save_agent_writes_files() {
        let tmp = TempDir::new().unwrap();
        let mgr = WorkspaceAgentManager::new(
            tmp.path().to_path_buf(),
            HashMap::new(),
            "openai".into(),
            "gpt-4".into(),
        );

        mgr.register_ephemeral(
            "saved_bot".into(),
            DelegateAgentConfig {
                provider: "anthropic".into(),
                model: "claude-3".into(),
                system_prompt: Some("You are a saved bot.".into()),
                api_key: None,
                temperature: Some(0.7),
                max_depth: 2,
                agentic: true,
                allowed_tools: vec!["shell".into()],
                max_iterations: 8,
                timeout_secs: Some(60),
                agentic_timeout_secs: None,
                skills_directory: None,
                memory_namespace: Some("saved_bot".into()),
                max_context_tokens: None,
                max_tool_result_chars: None,
            },
        );

        mgr.save_agent("saved_bot").await.unwrap();

        let agent_dir = tmp.path().join("agents").join("saved_bot");
        assert!(agent_dir.join("config.toml").exists());
        assert!(agent_dir.join("IDENTITY.md").exists());

        let config_text = std::fs::read_to_string(agent_dir.join("config.toml")).unwrap();
        assert!(config_text.contains("anthropic"));
        assert!(config_text.contains("claude-3"));
        assert!(config_text.contains("shell"));

        let identity_text = std::fs::read_to_string(agent_dir.join("IDENTITY.md")).unwrap();
        assert_eq!(identity_text, "You are a saved bot.");
    }

    #[tokio::test]
    async fn save_agent_not_found_errors() {
        let tmp = TempDir::new().unwrap();
        let mgr = WorkspaceAgentManager::new(
            tmp.path().to_path_buf(),
            HashMap::new(),
            "openai".into(),
            "gpt-4".into(),
        );

        let result = mgr.save_agent("nonexistent").await;
        assert!(result.is_err());
    }
}
