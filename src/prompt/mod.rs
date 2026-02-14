//! System prompt construction — composable, modular prompt builder.
//!
//! Builds the full system prompt from multiple sources:
//! - **Config** — autonomy level, model, provider, workspace path
//! - **Tools** — native tools and Aria registry tools (tenant-scoped)
//! - **Skills** — workspace skill definitions
//! - **Workspace files** — SOUL.md, IDENTITY.md, AGENTS.md, etc.
//! - **Security policy** — autonomy constraints, forbidden paths
//! - **Runtime** — host, OS, model, capabilities
//! - **Aria registries** — tool and agent definitions (tenant-scoped)
//!
//! # Usage
//!
//! ```ignore
//! let prompt = SystemPromptBuilder::new(&config.workspace_dir)
//!     .tools(&[("shell", "Run commands"), ("file_read", "Read files")])
//!     .skills(&loaded_skills)
//!     .autonomy(AutonomyLevel::Supervised)
//!     .model("claude-sonnet-4")
//!     .registry_tools_section("## Available Tools\n\n- **calc**: Calculator\n")
//!     .build();
//! ```

use std::fmt::Write;
use std::path::{Path, PathBuf};

use crate::security::AutonomyLevel;

/// Minimal skill descriptor for prompt construction.
///
/// Decoupled from `skills::Skill` so the prompt module can live in the library
/// crate without pulling in binary-only CLI command types.
#[derive(Debug, Clone)]
pub struct SkillDescriptor {
    pub name: String,
    pub description: String,
}

/// Maximum characters to inject from a single workspace file.
const BOOTSTRAP_MAX_CHARS: usize = 20_000;

/// Default fallback prompt when nothing else produces output.
const FALLBACK_PROMPT: &str =
    "You are Aria, a fast and efficient AI assistant built in Rust. Be helpful, concise, and direct.";

/// Ordered list of workspace files always injected (with missing-file markers).
const REQUIRED_WORKSPACE_FILES: &[&str] = &[
    "AGENTS.md",
    "SOUL.md",
    "TOOLS.md",
    "IDENTITY.md",
    "USER.md",
    "HEARTBEAT.md",
    "MEMORY.md",
];

/// Workspace files injected only when present (no missing-file marker).
const OPTIONAL_WORKSPACE_FILES: &[&str] = &["BOOTSTRAP.md"];

// ── Builder ──────────────────────────────────────────────────────

/// Composable system prompt builder.
///
/// Each `with_*` / setter method returns `&mut Self` for chaining.
/// Call [`build`](SystemPromptBuilder::build) to produce the final prompt string.
pub struct SystemPromptBuilder {
    workspace_dir: PathBuf,

    // Section data
    tools: Vec<(String, String)>,
    skills: Vec<SkillDescriptor>,
    model_name: String,
    autonomy: AutonomyLevel,
    workspace_only: bool,
    allowed_commands: Vec<String>,

    // Pre-rendered registry sections (from Aria registries)
    registry_tools_section: Option<String>,
    registry_agents_section: Option<String>,

    // Extra sections injected by callers (e.g. channel-specific context)
    extra_sections: Vec<(String, String)>,
}

impl SystemPromptBuilder {
    /// Create a builder rooted at `workspace_dir`.
    pub fn new(workspace_dir: &Path) -> Self {
        Self {
            workspace_dir: workspace_dir.to_path_buf(),
            tools: Vec::new(),
            skills: Vec::new(),
            model_name: String::new(),
            autonomy: AutonomyLevel::Supervised,
            workspace_only: true,
            allowed_commands: Vec::new(),
            registry_tools_section: None,
            registry_agents_section: None,
            extra_sections: Vec::new(),
        }
    }

    /// Set native tool descriptions (name, description).
    pub fn tools(mut self, tools: &[(&str, &str)]) -> Self {
        self.tools = tools
            .iter()
            .map(|(n, d)| ((*n).to_string(), (*d).to_string()))
            .collect();
        self
    }

    /// Set loaded workspace skills.
    pub fn skills(mut self, skills: &[SkillDescriptor]) -> Self {
        self.skills = skills.to_vec();
        self
    }

    /// Set the model name for the runtime section.
    pub fn model(mut self, model_name: &str) -> Self {
        self.model_name = model_name.to_string();
        self
    }

    /// Set autonomy level for the security section.
    pub fn autonomy(mut self, level: AutonomyLevel) -> Self {
        self.autonomy = level;
        self
    }

    /// Set whether the agent is restricted to the workspace directory.
    pub fn workspace_only(mut self, restricted: bool) -> Self {
        self.workspace_only = restricted;
        self
    }

    /// Set the list of allowed shell commands.
    pub fn allowed_commands(mut self, commands: &[String]) -> Self {
        self.allowed_commands = commands.to_vec();
        self
    }

    /// Inject a pre-rendered Aria tool registry section (tenant-scoped).
    pub fn registry_tools_section(mut self, section: String) -> Self {
        if !section.is_empty() {
            self.registry_tools_section = Some(section);
        }
        self
    }

    /// Inject a pre-rendered Aria agent registry section (tenant-scoped).
    pub fn registry_agents_section(mut self, section: String) -> Self {
        if !section.is_empty() {
            self.registry_agents_section = Some(section);
        }
        self
    }

    /// Add a custom section with a heading and body.
    pub fn extra_section(mut self, heading: &str, body: &str) -> Self {
        self.extra_sections
            .push((heading.to_string(), body.to_string()));
        self
    }

    /// Build the final system prompt string.
    pub fn build(&self) -> String {
        let mut prompt = String::with_capacity(8192);

        self.build_tools_section(&mut prompt);
        self.build_safety_section(&mut prompt);
        self.build_autonomy_section(&mut prompt);
        self.build_skills_section(&mut prompt);
        self.build_registry_sections(&mut prompt);
        self.build_workspace_section(&mut prompt);
        self.build_project_context(&mut prompt);
        self.build_extra_sections(&mut prompt);
        self.build_datetime_section(&mut prompt);
        self.build_runtime_section(&mut prompt);

        if prompt.is_empty() {
            FALLBACK_PROMPT.to_string()
        } else {
            prompt
        }
    }

    // ── Section builders ─────────────────────────────────────────

    fn build_tools_section(&self, prompt: &mut String) {
        if self.tools.is_empty() {
            return;
        }
        prompt.push_str("## Tools\n\n");
        prompt.push_str("You have access to the following tools:\n\n");
        for (name, desc) in &self.tools {
            let _ = writeln!(prompt, "- **{name}**: {desc}");
        }
        prompt.push('\n');
    }

    fn build_safety_section(&self, prompt: &mut String) {
        prompt.push_str("## Safety\n\n");
        prompt.push_str(
            "- Do not exfiltrate private data.\n\
             - Do not run destructive commands without asking.\n\
             - Do not bypass oversight or approval mechanisms.\n\
             - Prefer `trash` over `rm` (recoverable beats gone forever).\n\
             - When in doubt, ask before acting externally.\n\n",
        );
    }

    fn build_autonomy_section(&self, prompt: &mut String) {
        prompt.push_str("## Autonomy\n\n");

        let level_desc = match self.autonomy {
            AutonomyLevel::ReadOnly => "**Read-Only** — you can observe but not act.",
            AutonomyLevel::Supervised => {
                "**Supervised** — you may act but must ask before risky operations."
            }
            AutonomyLevel::Full => {
                "**Full** — you may act autonomously within policy bounds."
            }
        };
        let _ = writeln!(prompt, "Level: {level_desc}");

        if self.workspace_only {
            let _ = writeln!(
                prompt,
                "Scope: workspace only (`{}`)",
                self.workspace_dir.display()
            );
        }

        if !self.allowed_commands.is_empty() {
            let _ = writeln!(
                prompt,
                "Allowed commands: {}",
                self.allowed_commands.join(", ")
            );
        }

        prompt.push('\n');
    }

    fn build_skills_section(&self, prompt: &mut String) {
        if self.skills.is_empty() {
            return;
        }
        prompt.push_str("## Available Skills\n\n");
        prompt.push_str(
            "Skills are loaded on demand. Use `read` on the skill path to get full instructions.\n\n",
        );
        prompt.push_str("<available_skills>\n");
        for skill in &self.skills {
            let _ = writeln!(prompt, "  <skill>");
            let _ = writeln!(prompt, "    <name>{}</name>", skill.name);
            let _ = writeln!(
                prompt,
                "    <description>{}</description>",
                skill.description
            );
            let location = self
                .workspace_dir
                .join("skills")
                .join(&skill.name)
                .join("SKILL.md");
            let _ = writeln!(prompt, "    <location>{}</location>", location.display());
            let _ = writeln!(prompt, "  </skill>");
        }
        prompt.push_str("</available_skills>\n\n");
    }

    fn build_registry_sections(&self, prompt: &mut String) {
        if let Some(ref section) = self.registry_tools_section {
            prompt.push_str(section);
            if !section.ends_with('\n') {
                prompt.push('\n');
            }
            prompt.push('\n');
        }

        if let Some(ref section) = self.registry_agents_section {
            prompt.push_str(section);
            if !section.ends_with('\n') {
                prompt.push('\n');
            }
            prompt.push('\n');
        }
    }

    fn build_workspace_section(&self, prompt: &mut String) {
        let _ = writeln!(
            prompt,
            "## Workspace\n\nWorking directory: `{}`\n",
            self.workspace_dir.display()
        );
    }

    fn build_project_context(&self, prompt: &mut String) {
        prompt.push_str("## Project Context\n\n");
        prompt.push_str(
            "The following workspace files define your identity, behavior, and context.\n\n",
        );

        for filename in REQUIRED_WORKSPACE_FILES {
            inject_workspace_file(prompt, &self.workspace_dir, filename);
        }

        for filename in OPTIONAL_WORKSPACE_FILES {
            let path = self.workspace_dir.join(filename);
            if path.exists() {
                inject_workspace_file(prompt, &self.workspace_dir, filename);
            }
        }
    }

    fn build_extra_sections(&self, prompt: &mut String) {
        for (heading, body) in &self.extra_sections {
            let _ = writeln!(prompt, "## {heading}\n\n{body}\n");
        }
    }

    fn build_datetime_section(&self, prompt: &mut String) {
        let now = chrono::Local::now();
        let tz = now.format("%Z").to_string();
        let _ = writeln!(prompt, "## Current Date & Time\n\nTimezone: {tz}\n");
    }

    fn build_runtime_section(&self, prompt: &mut String) {
        let host = hostname::get()
            .map_or_else(|_| "unknown".into(), |h| h.to_string_lossy().to_string());
        let model = if self.model_name.is_empty() {
            "unknown"
        } else {
            &self.model_name
        };
        let _ = writeln!(
            prompt,
            "## Runtime\n\nHost: {host} | OS: {} | Model: {model}\n",
            std::env::consts::OS,
        );
    }
}

// ── File injection helper ────────────────────────────────────────

/// Inject a single workspace file into the prompt with truncation and missing-file markers.
fn inject_workspace_file(prompt: &mut String, workspace_dir: &Path, filename: &str) {
    let path = workspace_dir.join(filename);
    match std::fs::read_to_string(&path) {
        Ok(content) => {
            let trimmed = content.trim();
            if trimmed.is_empty() {
                return;
            }
            let _ = writeln!(prompt, "### {filename}\n");
            if trimmed.len() > BOOTSTRAP_MAX_CHARS {
                prompt.push_str(&trimmed[..BOOTSTRAP_MAX_CHARS]);
                let _ = writeln!(
                    prompt,
                    "\n\n[... truncated at {BOOTSTRAP_MAX_CHARS} chars — use `read` for full file]\n"
                );
            } else {
                prompt.push_str(trimmed);
                prompt.push_str("\n\n");
            }
        }
        Err(_) => {
            let _ = writeln!(prompt, "### {filename}\n\n[File not found: {filename}]\n");
        }
    }
}

// ── Convenience ──────────────────────────────────────────────────

/// Quick one-shot prompt builder (backwards-compatible with channels::build_system_prompt).
///
/// Accepts any type that has `name: String` and `description: String` fields by
/// converting to `SkillDescriptor` via the `name` and `description` closures.
/// For the simplest case, pass `&[]` for no skills, or convert from `skills::Skill`.
pub fn build_system_prompt(
    workspace_dir: &Path,
    model_name: &str,
    tools: &[(&str, &str)],
    skills: &[SkillDescriptor],
) -> String {
    SystemPromptBuilder::new(workspace_dir)
        .tools(tools)
        .skills(skills)
        .model(model_name)
        .build()
}

// ── Tests ────────────────────────────────────────────────────────

#[cfg(test)]
mod tests {
    use super::*;
    use tempfile::TempDir;

    fn make_workspace() -> TempDir {
        let tmp = TempDir::new().unwrap();
        std::fs::write(tmp.path().join("SOUL.md"), "Be helpful").unwrap();
        std::fs::write(tmp.path().join("IDENTITY.md"), "You are Aria").unwrap();
        std::fs::write(tmp.path().join("AGENTS.md"), "Agent patterns").unwrap();
        std::fs::write(tmp.path().join("TOOLS.md"), "Tool usage").unwrap();
        std::fs::write(tmp.path().join("USER.md"), "User prefs").unwrap();
        std::fs::write(tmp.path().join("HEARTBEAT.md"), "Check in").unwrap();
        std::fs::write(tmp.path().join("MEMORY.md"), "Long-term notes").unwrap();
        tmp
    }

    #[test]
    fn basic_prompt_has_all_sections() {
        let ws = make_workspace();
        let prompt = SystemPromptBuilder::new(ws.path())
            .tools(&[("shell", "Run commands"), ("file_read", "Read files")])
            .model("claude-sonnet-4")
            .build();

        assert!(prompt.contains("## Tools"), "missing Tools section");
        assert!(prompt.contains("## Safety"), "missing Safety section");
        assert!(prompt.contains("## Autonomy"), "missing Autonomy section");
        assert!(prompt.contains("## Workspace"), "missing Workspace section");
        assert!(prompt.contains("## Project Context"), "missing Project Context");
        assert!(prompt.contains("## Current Date & Time"), "missing DateTime");
        assert!(prompt.contains("## Runtime"), "missing Runtime");
    }

    #[test]
    fn tools_listed() {
        let ws = make_workspace();
        let prompt = SystemPromptBuilder::new(ws.path())
            .tools(&[
                ("shell", "Run commands"),
                ("memory_recall", "Search memory"),
            ])
            .build();

        assert!(prompt.contains("**shell**"));
        assert!(prompt.contains("Run commands"));
        assert!(prompt.contains("**memory_recall**"));
    }

    #[test]
    fn safety_guardrails_present() {
        let ws = make_workspace();
        let prompt = SystemPromptBuilder::new(ws.path()).build();

        assert!(prompt.contains("Do not exfiltrate private data"));
        assert!(prompt.contains("Do not run destructive commands"));
        assert!(prompt.contains("Prefer `trash` over `rm`"));
    }

    #[test]
    fn autonomy_levels() {
        let ws = make_workspace();

        let ro = SystemPromptBuilder::new(ws.path())
            .autonomy(AutonomyLevel::ReadOnly)
            .build();
        assert!(ro.contains("Read-Only"));

        let sup = SystemPromptBuilder::new(ws.path())
            .autonomy(AutonomyLevel::Supervised)
            .build();
        assert!(sup.contains("Supervised"));

        let full = SystemPromptBuilder::new(ws.path())
            .autonomy(AutonomyLevel::Full)
            .build();
        assert!(full.contains("Full"));
    }

    #[test]
    fn autonomy_shows_allowed_commands() {
        let ws = make_workspace();
        let prompt = SystemPromptBuilder::new(ws.path())
            .allowed_commands(&["git".into(), "cargo".into()])
            .build();

        assert!(prompt.contains("Allowed commands: git, cargo"));
    }

    #[test]
    fn workspace_files_injected() {
        let ws = make_workspace();
        let prompt = SystemPromptBuilder::new(ws.path()).build();

        assert!(prompt.contains("### SOUL.md"), "missing SOUL.md header");
        assert!(prompt.contains("Be helpful"), "missing SOUL content");
        assert!(prompt.contains("### IDENTITY.md"), "missing IDENTITY.md");
        assert!(prompt.contains("You are Aria"), "missing IDENTITY content");
    }

    #[test]
    fn missing_workspace_files_get_marker() {
        let tmp = TempDir::new().unwrap();
        let prompt = SystemPromptBuilder::new(tmp.path()).build();

        assert!(prompt.contains("[File not found: SOUL.md]"));
        assert!(prompt.contains("[File not found: AGENTS.md]"));
        assert!(prompt.contains("[File not found: IDENTITY.md]"));
    }

    #[test]
    fn empty_workspace_files_skipped() {
        let ws = make_workspace();
        std::fs::write(ws.path().join("TOOLS.md"), "").unwrap();
        let prompt = SystemPromptBuilder::new(ws.path()).build();

        // Empty TOOLS.md should not have a header
        assert!(!prompt.contains("### TOOLS.md"));
    }

    #[test]
    fn bootstrap_only_when_present() {
        let ws = make_workspace();

        // No BOOTSTRAP.md — should not appear
        let prompt = SystemPromptBuilder::new(ws.path()).build();
        assert!(!prompt.contains("### BOOTSTRAP.md"));

        // Create it — should appear
        std::fs::write(ws.path().join("BOOTSTRAP.md"), "# Bootstrap\nFirst run.").unwrap();
        let prompt2 = SystemPromptBuilder::new(ws.path()).build();
        assert!(prompt2.contains("### BOOTSTRAP.md"));
        assert!(prompt2.contains("First run"));
    }

    #[test]
    fn large_file_truncation() {
        let ws = make_workspace();
        let big = "x".repeat(BOOTSTRAP_MAX_CHARS + 1000);
        std::fs::write(ws.path().join("AGENTS.md"), &big).unwrap();
        let prompt = SystemPromptBuilder::new(ws.path()).build();

        assert!(prompt.contains("truncated at"));
    }

    #[test]
    fn runtime_metadata() {
        let ws = make_workspace();
        let prompt = SystemPromptBuilder::new(ws.path())
            .model("claude-sonnet-4")
            .build();

        assert!(prompt.contains("Model: claude-sonnet-4"));
        assert!(prompt.contains(&format!("OS: {}", std::env::consts::OS)));
        assert!(prompt.contains("Host:"));
    }

    #[test]
    fn skills_section() {
        let ws = make_workspace();
        let skills = vec![SkillDescriptor {
            name: "code-review".into(),
            description: "Review code for bugs".into(),
        }];

        let prompt = SystemPromptBuilder::new(ws.path())
            .skills(&skills)
            .build();

        assert!(prompt.contains("<available_skills>"));
        assert!(prompt.contains("<name>code-review</name>"));
        assert!(prompt.contains("<description>Review code for bugs</description>"));
        assert!(prompt.contains("SKILL.md</location>"));
    }

    #[test]
    fn registry_sections_injected() {
        let ws = make_workspace();
        let prompt = SystemPromptBuilder::new(ws.path())
            .registry_tools_section("## Available Tools\n\n- **calc**: Calculator\n".into())
            .registry_agents_section("## Available Agents\n\n- **writer**: Write docs\n".into())
            .build();

        assert!(prompt.contains("## Available Tools"));
        assert!(prompt.contains("**calc**"));
        assert!(prompt.contains("## Available Agents"));
        assert!(prompt.contains("**writer**"));
    }

    #[test]
    fn empty_registry_sections_not_injected() {
        let ws = make_workspace();
        let prompt = SystemPromptBuilder::new(ws.path())
            .registry_tools_section(String::new())
            .build();

        assert!(!prompt.contains("Available Tools"));
    }

    #[test]
    fn extra_sections() {
        let ws = make_workspace();
        let prompt = SystemPromptBuilder::new(ws.path())
            .extra_section("Channel Context", "Running on Discord #general")
            .build();

        assert!(prompt.contains("## Channel Context"));
        assert!(prompt.contains("Running on Discord #general"));
    }

    #[test]
    fn fallback_prompt() {
        // An impossible scenario (workspace section always produces output),
        // but verify the constant is correct.
        assert!(FALLBACK_PROMPT.contains("Aria"));
    }

    #[test]
    fn backwards_compatible_convenience() {
        let ws = make_workspace();
        let prompt = build_system_prompt(
            ws.path(),
            "test-model",
            &[("shell", "Run commands")],
            &[],
        );

        assert!(prompt.contains("## Tools"));
        assert!(prompt.contains("**shell**"));
        assert!(prompt.contains("Model: test-model"));
    }

    #[test]
    fn workspace_only_scope_shown() {
        let ws = make_workspace();
        let prompt = SystemPromptBuilder::new(ws.path())
            .workspace_only(true)
            .build();

        assert!(prompt.contains("Scope: workspace only"));
    }

    #[test]
    fn workspace_unrestricted_no_scope() {
        let ws = make_workspace();
        let prompt = SystemPromptBuilder::new(ws.path())
            .workspace_only(false)
            .build();

        assert!(!prompt.contains("Scope: workspace only"));
    }
}
