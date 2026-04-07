//! Multi-agent registry — manages named agent definitions with isolated
//! identities, skills, tools, and memory namespaces.
//!
//! Each agent directory lives under `<workspace>/agents/<id>/` and contains:
//! - `agent.toml` — metadata (display name, avatar, allowed tools, etc.)
//! - `IDENTITY.md` — agent personality
//! - `SOUL.md` — core operating principles
//! - `skills/` — per-agent skill directory

use anyhow::{Context, Result};
use serde::{Deserialize, Serialize};
use std::collections::HashMap;
use std::path::{Path, PathBuf};
use tracing::{info, warn};

// ── Agent definition (on-disk) ──────────────────────────────────

/// Persistent agent definition loaded from `agent.toml`.
#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct AgentDefinition {
    /// Unique identifier (directory name).
    pub id: String,
    /// Human-readable name shown in the UI.
    pub display_name: String,
    /// Avatar key — resolved to an SVG/PNG by the frontend.
    #[serde(default)]
    pub avatar: String,
    /// Tool names this agent is allowed to use (empty = all tools).
    #[serde(default)]
    pub allowed_tools: Vec<String>,
    /// Memory namespace prefix for isolation.
    #[serde(default)]
    pub memory_namespace: Option<String>,
    /// Optional project directory this agent operates on.
    #[serde(default)]
    pub project_dir: Option<String>,
    /// Agent role description (short).
    #[serde(default)]
    pub role: String,
    /// Focus areas / tags.
    #[serde(default)]
    pub focus: Vec<String>,
}

impl AgentDefinition {
    /// Effective memory namespace — falls back to agent id.
    pub fn effective_memory_namespace(&self) -> &str {
        self.memory_namespace.as_deref().unwrap_or(&self.id)
    }
}

// ── Runtime state ───────────────────────────────────────────────

/// Current operational status of an agent instance.
#[derive(Debug, Clone, Serialize, Deserialize)]
#[serde(tag = "state", rename_all = "snake_case")]
pub enum AgentStatus {
    Idle,
    Working { task: String },
    Error { message: String },
}

impl Default for AgentStatus {
    fn default() -> Self {
        Self::Idle
    }
}

/// A running agent instance combining definition with runtime state.
#[derive(Debug, Clone)]
pub struct AgentInstance {
    pub definition: AgentDefinition,
    /// Content of IDENTITY.md (loaded from disk).
    pub identity_content: String,
    /// Content of SOUL.md (loaded from disk).
    pub soul_content: String,
    pub status: AgentStatus,
    /// Epoch millis of last activity (for serialization).
    pub last_activity_ms: u64,
    /// Path to this agent's directory on disk.
    pub dir: PathBuf,
}

impl AgentInstance {
    /// Serializable snapshot for REST API responses.
    pub fn to_api_json(&self) -> serde_json::Value {
        serde_json::json!({
            "id": self.definition.id,
            "display_name": self.definition.display_name,
            "avatar": self.definition.avatar,
            "role": self.definition.role,
            "focus": self.definition.focus,
            "allowed_tools": self.definition.allowed_tools,
            "memory_namespace": self.definition.effective_memory_namespace(),
            "project_dir": self.definition.project_dir,
            "status": self.status,
            "last_activity_ms": self.last_activity_ms,
        })
    }

    /// Update status and touch last-activity timestamp.
    pub fn set_status(&mut self, status: AgentStatus) {
        self.status = status;
        self.last_activity_ms = std::time::SystemTime::now()
            .duration_since(std::time::UNIX_EPOCH)
            .unwrap_or_default()
            .as_millis() as u64;
    }
}

// ── Registry ────────────────────────────────────────────────────

/// Central registry that manages all named agent instances.
pub struct AgentRegistry {
    agents: HashMap<String, AgentInstance>,
    /// Root directory: `<workspace>/agents/`.
    agents_dir: PathBuf,
}

impl AgentRegistry {
    /// Create a new registry rooted at `<workspace>/agents/`.
    pub fn new(workspace_dir: &Path) -> Self {
        Self {
            agents: HashMap::new(),
            agents_dir: workspace_dir.join("agents"),
        }
    }

    /// Load all agent definitions from disk.
    pub fn load_all(&mut self) -> Result<()> {
        self.agents.clear();

        if !self.agents_dir.exists() {
            info!("No agents directory found at {}", self.agents_dir.display());
            return Ok(());
        }

        let entries = std::fs::read_dir(&self.agents_dir)
            .with_context(|| format!("reading agents dir: {}", self.agents_dir.display()))?;

        for entry in entries {
            let entry = entry?;
            let path = entry.path();
            if !path.is_dir() {
                continue;
            }

            let agent_toml = path.join("agent.toml");
            if !agent_toml.exists() {
                continue;
            }

            match self.load_agent(&path) {
                Ok(instance) => {
                    info!(agent = %instance.definition.id, "Loaded agent definition");
                    self.agents.insert(instance.definition.id.clone(), instance);
                }
                Err(e) => {
                    warn!(path = %path.display(), error = %e, "Failed to load agent definition");
                }
            }
        }

        info!("Agent registry loaded {} agents", self.agents.len());
        Ok(())
    }

    fn load_agent(&self, agent_dir: &Path) -> Result<AgentInstance> {
        let agent_toml = agent_dir.join("agent.toml");
        let toml_content = std::fs::read_to_string(&agent_toml)
            .with_context(|| format!("reading {}", agent_toml.display()))?;
        let mut def: AgentDefinition = toml::from_str(&toml_content)
            .with_context(|| format!("parsing {}", agent_toml.display()))?;

        // Use directory name as id if not set
        if def.id.is_empty() {
            def.id = agent_dir
                .file_name()
                .unwrap_or_default()
                .to_string_lossy()
                .to_string();
        }

        let identity_content =
            std::fs::read_to_string(agent_dir.join("IDENTITY.md")).unwrap_or_default();
        let soul_content = std::fs::read_to_string(agent_dir.join("SOUL.md")).unwrap_or_default();

        Ok(AgentInstance {
            definition: def,
            identity_content,
            soul_content,
            status: AgentStatus::default(),
            last_activity_ms: 0,
            dir: agent_dir.to_path_buf(),
        })
    }

    /// Get an agent by id.
    pub fn get(&self, id: &str) -> Option<&AgentInstance> {
        self.agents.get(id)
    }

    /// Get a mutable reference to an agent by id.
    pub fn get_mut(&mut self, id: &str) -> Option<&mut AgentInstance> {
        self.agents.get_mut(id)
    }

    /// List all agents.
    pub fn list(&self) -> Vec<&AgentInstance> {
        self.agents.values().collect()
    }

    /// Number of registered agents.
    pub fn len(&self) -> usize {
        self.agents.len()
    }

    /// Whether the registry is empty.
    pub fn is_empty(&self) -> bool {
        self.agents.is_empty()
    }

    /// Root agents directory.
    pub fn agents_dir(&self) -> &Path {
        &self.agents_dir
    }

    /// Create a new agent definition on disk and register it.
    pub fn create(&mut self, def: AgentDefinition) -> Result<&AgentInstance> {
        let agent_dir = self.agents_dir.join(&def.id);
        std::fs::create_dir_all(&agent_dir)
            .with_context(|| format!("creating agent dir: {}", agent_dir.display()))?;

        // Write agent.toml
        let toml_content = toml::to_string_pretty(&def).context("serializing agent definition")?;
        std::fs::write(agent_dir.join("agent.toml"), &toml_content)?;

        // Write empty IDENTITY.md and SOUL.md if they don't exist
        let identity_path = agent_dir.join("IDENTITY.md");
        let soul_path = agent_dir.join("SOUL.md");
        let identity_content = if identity_path.exists() {
            std::fs::read_to_string(&identity_path).unwrap_or_default()
        } else {
            let content = format!("# {}\n\n{}\n", def.display_name, def.role);
            std::fs::write(&identity_path, &content)?;
            content
        };
        let soul_content = if soul_path.exists() {
            std::fs::read_to_string(&soul_path).unwrap_or_default()
        } else {
            let content = format!("# {} — Core Principles\n", def.display_name);
            std::fs::write(&soul_path, &content)?;
            content
        };

        // Create skills directory
        let skills_dir = agent_dir.join("skills");
        std::fs::create_dir_all(&skills_dir)?;

        let instance = AgentInstance {
            definition: def.clone(),
            identity_content,
            soul_content,
            status: AgentStatus::default(),
            last_activity_ms: 0,
            dir: agent_dir,
        };

        self.agents.insert(def.id.clone(), instance);
        Ok(self.agents.get(&def.id).unwrap())
    }

    /// Update an existing agent definition on disk.
    pub fn update(&mut self, id: &str, def: AgentDefinition) -> Result<()> {
        let agent_dir = self.agents_dir.join(id);
        if !agent_dir.exists() {
            anyhow::bail!("Agent '{}' not found", id);
        }

        let toml_content = toml::to_string_pretty(&def).context("serializing agent definition")?;
        std::fs::write(agent_dir.join("agent.toml"), &toml_content)?;

        // Reload
        let instance = self.load_agent(&agent_dir)?;
        self.agents.insert(id.to_string(), instance);
        Ok(())
    }

    /// Update IDENTITY.md for an agent.
    pub fn update_identity(&mut self, id: &str, content: &str) -> Result<()> {
        let agent = self.agents.get(id).context("Agent not found")?;
        std::fs::write(agent.dir.join("IDENTITY.md"), content)?;
        if let Some(inst) = self.agents.get_mut(id) {
            inst.identity_content = content.to_string();
        }
        Ok(())
    }

    /// Update SOUL.md for an agent.
    pub fn update_soul(&mut self, id: &str, content: &str) -> Result<()> {
        let agent = self.agents.get(id).context("Agent not found")?;
        std::fs::write(agent.dir.join("SOUL.md"), content)?;
        if let Some(inst) = self.agents.get_mut(id) {
            inst.soul_content = content.to_string();
        }
        Ok(())
    }

    /// Read a skill file for an agent. Returns content of `skills/<skill_name>/SKILL.md`.
    pub fn read_skill(&self, agent_id: &str, skill_name: &str) -> Result<String> {
        let agent = self.agents.get(agent_id).context("Agent not found")?;
        let skill_path = agent.dir.join("skills").join(skill_name).join("SKILL.md");
        std::fs::read_to_string(&skill_path)
            .with_context(|| format!("reading skill: {}", skill_path.display()))
    }

    /// Write a skill file for an agent. Creates `skills/<skill_name>/SKILL.md`.
    pub fn write_skill(&self, agent_id: &str, skill_name: &str, content: &str) -> Result<()> {
        let agent = self.agents.get(agent_id).context("Agent not found")?;
        let skill_dir = agent.dir.join("skills").join(skill_name);
        std::fs::create_dir_all(&skill_dir)?;
        std::fs::write(skill_dir.join("SKILL.md"), content)?;
        Ok(())
    }

    /// List skill names for an agent.
    pub fn list_skills(&self, agent_id: &str) -> Result<Vec<String>> {
        let agent = self.agents.get(agent_id).context("Agent not found")?;
        let skills_dir = agent.dir.join("skills");
        if !skills_dir.exists() {
            return Ok(Vec::new());
        }
        let mut names = Vec::new();
        for entry in std::fs::read_dir(&skills_dir)? {
            let entry = entry?;
            if entry.path().is_dir() && entry.path().join("SKILL.md").exists() {
                if let Some(name) = entry.file_name().to_str() {
                    names.push(name.to_string());
                }
            }
        }
        names.sort();
        Ok(names)
    }

    /// Delete an agent from disk and registry.
    pub fn delete(&mut self, id: &str) -> Result<()> {
        let agent_dir = self.agents_dir.join(id);
        if agent_dir.exists() {
            std::fs::remove_dir_all(&agent_dir)
                .with_context(|| format!("removing agent dir: {}", agent_dir.display()))?;
        }
        self.agents.remove(id);
        Ok(())
    }
}
