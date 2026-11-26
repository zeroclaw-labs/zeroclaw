//! Executable skill manifest declarations. These types do not grant execution authority.

use serde::{Deserialize, Serialize};
use std::collections::HashMap;

pub mod argv;

/// A tool defined by a skill (shell command, HTTP call, etc.)
#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct SkillTool {
    pub name: String,
    pub description: String,
    /// "shell", "http", "script", "builtin", "mcp"
    pub kind: String,
    /// The command/URL/script to execute (unused for builtin/mcp kinds)
    #[serde(flatten)]
    pub command: SkillCommand,
    #[serde(default)]
    pub args: HashMap<String, String>,
    /// For `kind = "builtin"`: the name of the built-in tool to delegate to.
    /// For `kind = "mcp"`: the prefixed MCP tool name `{server}__{tool}`
    /// (e.g. `images__generate`).
    #[serde(default)]
    pub target: Option<String>,
    #[serde(default, alias = "default_args")]
    pub locked_args: HashMap<String, String>,
    /// For `kind = "shell"` / `kind = "script"`: maximum execution time in
    /// seconds before the command is killed. Unset falls back to the built-in
    /// `SKILL_SHELL_TIMEOUT_SECS` (60s) default; long-running skills (e.g. a
    /// build pipeline) raise it via `timeout_secs` in SKILL.toml.
    #[serde(default)]
    pub timeout_secs: Option<u64>,
}

/// Mutually exclusive manifest fields; a missing command retains its legacy empty default.
#[derive(Debug, Clone, Serialize, Deserialize)]
#[serde(try_from = "WireCommand", into = "WireCommand")]
pub enum SkillCommand {
    Text(String),
    Argv(Vec<String>),
}

#[derive(Serialize, Deserialize)]
struct WireCommand {
    #[serde(default, skip_serializing_if = "Option::is_none")]
    command: Option<String>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    argv: Option<Vec<String>>,
}

impl TryFrom<WireCommand> for SkillCommand {
    type Error = &'static str;
    fn try_from(value: WireCommand) -> Result<Self, Self::Error> {
        match (value.command, value.argv) {
            (Some(_), Some(_)) => Err("command and argv cannot both be present"),
            (None, Some(argv)) => Ok(Self::Argv(argv)),
            (text, None) => Ok(Self::Text(text.unwrap_or_default())),
        }
    }
}

impl From<SkillCommand> for WireCommand {
    fn from(value: SkillCommand) -> Self {
        match value {
            SkillCommand::Text(text) => Self {
                command: Some(text),
                argv: None,
            },
            SkillCommand::Argv(argv) => Self {
                command: None,
                argv: Some(argv),
            },
        }
    }
}
impl From<String> for SkillCommand {
    fn from(value: String) -> Self {
        Self::Text(value)
    }
}
impl From<&str> for SkillCommand {
    fn from(value: &str) -> Self {
        Self::Text(value.to_owned())
    }
}
impl SkillCommand {
    pub fn text(&self) -> Option<&str> {
        match self {
            Self::Text(text) => Some(text),
            Self::Argv(_) => None,
        }
    }
}
impl SkillTool {
    pub fn validate_literal_argv(&self) -> Result<(), &'static str> {
        if let SkillCommand::Argv(elements) = &self.command {
            if !matches!(self.kind.as_str(), "shell" | "script") {
                return Err("argv requires kind shell or script");
            }
            argv::validate(elements, &self.args)?;
        }
        Ok(())
    }
}

/// Install and discovery use the same argv declaration and binder as the loader.
/// Preserve the existing audit of legacy command-only tools.
pub fn audit_tool(tool: &toml::Value) -> Result<(), String> {
    if tool.get("argv").is_some() {
        let decl: SkillTool = tool
            .clone()
            .try_into()
            .map_err(|_| "has an invalid argv declaration")?;
        return decl.validate_literal_argv().map_err(String::from);
    }
    let command = tool
        .get("command")
        .and_then(toml::Value::as_str)
        .ok_or("is missing a command field.")?;
    let kind = tool
        .get("kind")
        .and_then(toml::Value::as_str)
        .unwrap_or_default();
    if (kind.eq_ignore_ascii_case("shell") || kind.eq_ignore_ascii_case("script"))
        && command.trim().is_empty()
    {
        return Err(format!("has an empty {kind} command."));
    }
    Ok(())
}
