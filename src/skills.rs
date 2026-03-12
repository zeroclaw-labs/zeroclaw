//! Skills stub module for Augusta.
//! Skills loading from ZeroClaw community registry was stripped.
//! Augusta skills will be loaded from the workspace directory.

use crate::config::{Config, SkillsPromptInjectionMode};
use std::collections::HashMap;
use std::path::Path;

/// A skill definition.
#[derive(Debug, Clone)]
pub struct Skill {
    pub name: String,
    pub description: String,
    pub version: String,
    pub author: Option<String>,
    pub tags: Vec<String>,
    pub tools: Vec<SkillTool>,
    pub prompts: Vec<String>,
    pub location: Option<String>,
}

/// A tool defined within a skill.
#[derive(Debug, Clone)]
pub struct SkillTool {
    pub name: String,
    pub description: String,
    pub kind: String,
    pub command: String,
    pub args: HashMap<String, String>,
}

/// Load skills from workspace directory and config.
pub fn load_skills_with_config(_workspace: &Path, _config: &Config) -> Vec<Skill> {
    // TODO: implement workspace skill loading for Augusta
    Vec::new()
}

/// Convert skills to a prompt section for the LLM.
pub fn skills_to_prompt_with_mode(
    skills: &[Skill],
    _workspace: &Path,
    _mode: SkillsPromptInjectionMode,
) -> String {
    if skills.is_empty() {
        return String::new();
    }

    let mut prompt = String::from("\n\n## Available Skills\n\n");
    for skill in skills {
        prompt.push_str(&format!("### {}\n{}\n", skill.name, skill.description));
        for tool in &skill.tools {
            prompt.push_str(&format!("- Tool: {} — {}\n", tool.name, tool.description));
        }
        prompt.push('\n');
    }
    prompt
}
