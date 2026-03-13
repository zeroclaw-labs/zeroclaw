//! Skill tool handler - Bridges SKILL.toml shell-based tool definitions to native tool calling.
//!
//! This module solves the fundamental mismatch between:
//! - Skills defining tools as shell commands with `{placeholder}` parameters
//! - LLM providers expecting native tool calling with JSON arguments
//!
//! ## Architecture
//!
//! 1. Parse SKILL.toml `[[tools]]` definitions (command template + args metadata)
//! 2. Generate JSON schemas for native function calling
//! 3. Substitute JSON arguments into command templates
//! 4. Execute shell commands and return structured results
//!
//! ## Example Transformation
//!
//! SKILL.toml:
//! ```toml
//! [[tools]]
//! name = "telegram_list_dialogs"
//! command = "python3 script.py --limit {limit}"
//! [tools.args]
//! limit = "Maximum number of dialogs"
//! ```
//!
//! Becomes:
//! - Tool name: `telegram_list_dialogs`
//! - JSON schema: `{"type": "object", "properties": {"limit": {"type": "integer", "description": "Maximum number of dialogs"}}}`
//! - Model calls: `{"name": "telegram_list_dialogs", "arguments": {"limit": 50}}`
//! - Executed: `python3 script.py --limit 50`
//!
//! ## Security
//!
//! - All arguments are validated and shell-escaped
//! - Commands execute within existing SecurityPolicy constraints
//! - No arbitrary code injection

use crate::security::SecurityPolicy;
use crate::skills::SkillTool;
use crate::tools::traits::{Tool, ToolResult};
use anyhow::{bail, Context, Result};
use async_trait::async_trait;
use regex::Regex;
use std::collections::{HashMap, HashSet};
use std::sync::{Arc, LazyLock};

/// Regex to extract {placeholder} names from command templates
static PLACEHOLDER_REGEX: LazyLock<Regex> =
    LazyLock::new(|| Regex::new(r"\{(\w+)\}").expect("placeholder regex compilation failed"));

/// Parameter metadata for skill tools
#[derive(Debug, Clone)]
pub struct SkillToolParameter {
    pub name: String,
    pub description: String,
    pub required: bool,
    pub param_type: ParameterType,
}

/// Supported parameter types for skill tools
#[derive(Debug, Clone, PartialEq)]
pub enum ParameterType {
    String,
    Integer,
    Boolean,
}

/// Skill tool handler implementing the Tool trait
pub struct SkillToolHandler {
    skill_name: String,
    tool_def: SkillTool,
    parameters: Vec<SkillToolParameter>,
    security: Arc<SecurityPolicy>,
    tags: Vec<String>,
    terminal: bool,
}

impl SkillToolHandler {
    /// Create a new skill tool handler from a skill tool definition
    pub fn new(
        skill_name: String,
        tool_def: SkillTool,
        security: Arc<SecurityPolicy>,
    ) -> Result<Self> {
        if !tool_def.kind.eq_ignore_ascii_case("shell") {
            tracing::warn!(
                skill = %skill_name,
                tool = %tool_def.name,
                kind = %tool_def.kind,
                "Skipping skill tool: only kind=\"shell\" is supported"
            );
            bail!(
                "Unsupported tool kind '{}': only shell tools are supported",
                tool_def.kind
            );
        }
        let parameters = Self::extract_parameters(&tool_def)?;
        let tags = tool_def.tags.clone();
        let terminal = tool_def.terminal;
        Ok(Self {
            skill_name,
            tool_def,
            parameters,
            security,
            tags,
            terminal,
        })
    }

    /// Extract parameter definitions from tool args and command template
    fn extract_parameters(tool_def: &SkillTool) -> Result<Vec<SkillToolParameter>> {
        let placeholders = Self::extract_placeholders(&tool_def.command);
        let mut parameters = Vec::new();

        for placeholder in placeholders {
            let description = tool_def
                .args
                .get(&placeholder)
                .cloned()
                .unwrap_or_else(|| format!("Parameter: {}", placeholder));

            // Infer type from name + description (name takes priority)
            let param_type = Self::infer_parameter_type(&placeholder, &description);

            // Infer required from description hints
            let is_optional = {
                let desc_lower = description.to_lowercase();
                desc_lower.contains("(optional)")
                    || desc_lower.contains("default:")
                    || desc_lower.contains("default ")
            };

            parameters.push(SkillToolParameter {
                name: placeholder,
                description,
                required: !is_optional,
                param_type,
            });
        }

        Ok(parameters)
    }

    /// Extract {placeholder} names from command template
    fn extract_placeholders(command: &str) -> Vec<String> {
        let mut seen = HashSet::new();
        let mut placeholders = Vec::new();

        for cap in PLACEHOLDER_REGEX.captures_iter(command) {
            if let Some(name) = cap.get(1) {
                let name_str = name.as_str().to_string();
                if seen.insert(name_str.clone()) {
                    placeholders.push(name_str);
                }
            }
        }

        placeholders
    }

    /// Parameter names that are always String regardless of description content.
    /// Prevents false positives when a description accidentally contains integer/boolean
    /// keywords (e.g. "Account to use, sets the limit count" → must stay String).
    const ALWAYS_STRING_NAMES: &'static [&'static str] = &[
        "account",
        "username",
        "name",
        "contact_name",
        "query",
        "text",
        "session",
        "mode",
        "folder_name",
        "folder",
        "filter",
        "channel_filter",
        "sender",
        "key",
        "token",
        "path",
        "dir",
        "url",
    ];

    /// Infer parameter type from parameter name and description text.
    /// Name takes priority: known string-identity names are always String even if
    /// the description accidentally contains integer/boolean keywords.
    fn infer_parameter_type(name: &str, description: &str) -> ParameterType {
        // Name-based override: certain param names are always String
        if Self::ALWAYS_STRING_NAMES.contains(&name) {
            return ParameterType::String;
        }

        let desc_lower = description.to_lowercase();

        // Check for integer indicators using whole-word matching to avoid false
        // positives like "account" containing "count", "disable" containing "able", etc.
        let has_word = |word: &str| -> bool {
            desc_lower
                .split(|c: char| !c.is_alphanumeric())
                .any(|w| w == word)
        };

        if has_word("number")
            || has_word("count")
            || has_word("limit")
            || has_word("maximum")
            || has_word("minimum")
        {
            return ParameterType::Integer;
        }

        // Check for boolean indicators
        if has_word("enable")
            || has_word("disable")
            || has_word("true")
            || has_word("false")
            || has_word("flag")
        {
            return ParameterType::Boolean;
        }

        // Default to string
        ParameterType::String
    }

    /// Generate JSON schema for tool parameters
    fn generate_schema(&self) -> serde_json::Value {
        let mut properties = serde_json::Map::new();
        let mut required = Vec::new();

        for param in &self.parameters {
            let type_str = match param.param_type {
                ParameterType::String => "string",
                ParameterType::Integer => "integer",
                ParameterType::Boolean => "boolean",
            };

            properties.insert(
                param.name.clone(),
                serde_json::json!({
                    "type": type_str,
                    "description": param.description
                }),
            );

            if param.required {
                required.push(param.name.clone());
            }
        }

        let mut schema = serde_json::json!({
            "type": "object",
            "properties": properties
        });

        if !required.is_empty() {
            schema["required"] = serde_json::json!(required);
        }

        schema
    }

    /// Escape shell special characters for safe command execution
    fn shell_escape(s: &str) -> String {
        // If the string is simple (alphanumeric + safe chars), return as-is
        if s.chars()
            .all(|c| c.is_alphanumeric() || c == '_' || c == '-' || c == '.' || c == '/')
        {
            return s.to_string();
        }

        // Otherwise, single-quote and escape any single quotes
        format!("'{}'", s.replace('\'', "'\\''"))
    }

    /// Substitute arguments into command template
    fn render_command(&self, args: &serde_json::Value) -> Result<String> {
        let mut command = self.tool_def.command.clone();

        // Get args as object
        let args_obj = args
            .as_object()
            .context("Tool arguments must be a JSON object")?;

        // Build lookup maps for parameter types and required-ness
        let param_types: HashMap<String, ParameterType> = self
            .parameters
            .iter()
            .map(|p| (p.name.clone(), p.param_type.clone()))
            .collect();
        let required_params: std::collections::HashSet<&str> = self
            .parameters
            .iter()
            .filter(|p| p.required)
            .map(|p| p.name.as_str())
            .collect();

        // Build a map of available arguments; error on null/empty required params
        let mut arg_values = HashMap::new();
        for (key, value) in args_obj {
            let is_required = required_params.contains(key.as_str());
            if value.is_null() {
                if is_required {
                    bail!("Required parameter '{}' must not be null", key);
                }
                continue;
            }
            let value_str = self.format_argument_value(value)?;
            if value_str.is_empty() {
                if is_required {
                    bail!("Required parameter '{}' must not be empty", key);
                }
                continue; // skip empty strings for optional params
            }
            arg_values.insert(key.clone(), value_str);
        }

        // Replace placeholders
        let placeholders = Self::extract_placeholders(&command);
        for placeholder in placeholders {
            let pattern = format!("{{{}}}", placeholder);

            if let Some(value) = arg_values.get(&placeholder) {
                // Determine if this should be quoted based on parameter type
                let param_type = param_types
                    .get(&placeholder)
                    .cloned()
                    .unwrap_or(ParameterType::String);

                let escaped_value = match param_type {
                    ParameterType::String => {
                        format!("'{}'", value.replace('\'', "'\\''"))
                    }
                    ParameterType::Integer => {
                        if value.parse::<i64>().is_err() {
                            bail!(
                                "Parameter '{}' declared as integer but got non-numeric value",
                                placeholder
                            );
                        }
                        value.clone()
                    }
                    ParameterType::Boolean => {
                        if value != "true" && value != "false" {
                            bail!(
                                "Parameter '{}' declared as boolean but got '{}'",
                                placeholder,
                                value
                            );
                        }
                        value.clone()
                    }
                };
                command = command.replace(&pattern, &escaped_value);
            } else {
                // Parameter not provided - remove the flag/option entirely
                // This handles optional parameters gracefully

                // Convert underscore to dash for flag names (contact_name -> contact-name)
                let flag_name = placeholder.replace('_', "-");

                // Try to remove the entire flag with various formats
                let flag_patterns = [
                    // --flag {placeholder}
                    format!("--{} {}", flag_name, pattern),
                    // --flag={placeholder}
                    format!("--{}={}", flag_name, pattern),
                    // -f {placeholder} (short form)
                    format!("-{} {}", flag_name.chars().next().unwrap_or('x'), pattern),
                    // Also try with original placeholder name (no dash conversion)
                    format!("--{} {}", placeholder, pattern),
                    format!("--{}={}", placeholder, pattern),
                ];

                let mut removed = false;
                for flag_pattern in &flag_patterns {
                    if command.contains(flag_pattern) {
                        command = command.replace(flag_pattern, "");
                        removed = true;
                        break;
                    }
                }

                if !removed {
                    // Just remove the placeholder itself
                    command = command.replace(&pattern, "");
                }
            }
        }

        // Clean up extra whitespace
        command = command.split_whitespace().collect::<Vec<_>>().join(" ");

        Ok(command)
    }

    /// Format a JSON value as a string for shell substitution
    fn format_argument_value(&self, value: &serde_json::Value) -> Result<String> {
        match value {
            serde_json::Value::String(s) => Ok(s.clone()),
            serde_json::Value::Number(n) => Ok(n.to_string()),
            serde_json::Value::Bool(b) => Ok(b.to_string()),
            serde_json::Value::Null => Ok(String::new()),
            _ => bail!("Unsupported argument type: {:?}", value),
        }
    }
}

#[async_trait]
impl Tool for SkillToolHandler {
    fn name(&self) -> &str {
        &self.tool_def.name
    }

    fn description(&self) -> &str {
        &self.tool_def.description
    }

    fn parameters_schema(&self) -> serde_json::Value {
        self.generate_schema()
    }

    fn tags(&self) -> &[String] {
        &self.tags
    }

    fn is_terminal(&self) -> bool {
        self.terminal
    }

    async fn execute(&self, args: serde_json::Value) -> Result<ToolResult> {
        if self.security.is_rate_limited() {
            return Ok(ToolResult {
                output: "Rate limit exceeded — try again later.".into(),
                success: false,
                error: None,
            });
        }

        let command = match self.render_command(&args) {
            Ok(c) => c,
            Err(e) => {
                tracing::warn!(
                    skill = %self.skill_name,
                    tool = %self.tool_def.name,
                    args = %args,
                    error = %e,
                    "Skill tool render failed — returning soft error to LLM"
                );
                return Ok(ToolResult {
                    output: format!(
                        "Invalid tool arguments: {e}. Please retry with corrected parameters."
                    ),
                    success: false,
                    error: None,
                });
            }
        };

        if let Err(e) = self.security.validate_command_execution(&command, false) {
            tracing::warn!(
                skill = %self.skill_name,
                tool = %self.tool_def.name,
                reason = %e,
                command_template = %self.tool_def.command.chars().take(200).collect::<String>(),
                "Skill tool blocked by security policy"
            );
            return Ok(ToolResult {
                output: format!("Blocked by security policy: {e}"),
                success: false,
                error: None,
            });
        }

        if !self.security.record_action() {
            return Ok(ToolResult {
                output: "Action limit exceeded — try again later.".into(),
                success: false,
                error: None,
            });
        }

        tracing::debug!(
            skill = %self.skill_name,
            tool = %self.tool_def.name,
            command_template = %self.tool_def.command,
            "Executing skill tool"
        );

        let output = tokio::process::Command::new("sh")
            .arg("-c")
            .arg(&command)
            .output()
            .await
            .context("Failed to execute skill tool command")?;

        let stdout = String::from_utf8_lossy(&output.stdout).to_string();
        let stderr = String::from_utf8_lossy(&output.stderr).to_string();
        let success = output.status.success();

        // Scrub credentials from output (reuse loop_.rs scrubbing logic)
        let scrubbed_stdout = crate::agent::loop_::scrub_credentials(&stdout);
        let scrubbed_stderr = crate::agent::loop_::scrub_credentials(&stderr);

        if success {
            tracing::debug!(
                skill = %self.skill_name,
                tool = %self.tool_def.name,
                exit_code = ?output.status.code(),
                "Skill tool execution completed"
            );
        } else {
            tracing::warn!(
                skill = %self.skill_name,
                tool = %self.tool_def.name,
                exit_code = ?output.status.code(),
                stderr = %scrubbed_stderr.chars().take(500).collect::<String>(),
                command_template = %self.tool_def.command.chars().take(200).collect::<String>(),
                "Skill tool execution failed"
            );
        }

        // When the command fails but produced stdout (e.g. JSON error from Python
        // script that exits non-zero), include that output so the LLM can see the
        // actual error reason and retry with corrected parameters.
        Ok(ToolResult {
            success,
            output: if success {
                scrubbed_stdout
            } else if !scrubbed_stdout.trim().is_empty() {
                format!("{}\n---\nstderr: {}", scrubbed_stdout, scrubbed_stderr)
            } else {
                format!("Command failed:\n{}", scrubbed_stderr)
            },
            error: if success { None } else { Some(scrubbed_stderr) },
        })
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn extract_placeholders_from_command() {
        let command = "python3 script.py --limit {limit} --name {name}";
        let placeholders = SkillToolHandler::extract_placeholders(command);
        assert_eq!(placeholders, vec!["limit", "name"]);
    }

    #[test]
    fn extract_placeholders_deduplicates() {
        let command = "echo {value} and {value} again";
        let placeholders = SkillToolHandler::extract_placeholders(command);
        assert_eq!(placeholders, vec!["value"]);
    }

    #[test]
    fn infer_integer_type() {
        assert_eq!(
            SkillToolHandler::infer_parameter_type("size", "Maximum number of items"),
            ParameterType::Integer
        );
        assert_eq!(
            SkillToolHandler::infer_parameter_type("items", "Limit the count"),
            ParameterType::Integer
        );
    }

    #[test]
    fn infer_boolean_type() {
        assert_eq!(
            SkillToolHandler::infer_parameter_type("verbose", "Enable verbose mode"),
            ParameterType::Boolean
        );
    }

    #[test]
    fn infer_string_type_default() {
        assert_eq!(
            SkillToolHandler::infer_parameter_type("email", "User name or email"),
            ParameterType::String
        );
    }

    #[test]
    fn infer_string_not_integer_for_known_string_param_names() {
        // "account" must be String even if description accidentally contains integer keywords
        let tricky = "Account to use, sets the limit count";
        assert_eq!(
            SkillToolHandler::infer_parameter_type("account", tricky),
            ParameterType::String
        );
        // same for other common string-identity param names
        for name in &[
            "username",
            "query",
            "session",
            "mode",
            "folder_name",
            "contact_name",
        ] {
            assert_eq!(
                SkillToolHandler::infer_parameter_type(name, "Maximum number of things"),
                ParameterType::String,
                "param '{name}' should always be String"
            );
        }
    }

    #[test]
    fn generate_schema_with_parameters() {
        let tool_def = SkillTool {
            name: "test_tool".to_string(),
            description: "Test tool".to_string(),
            kind: "shell".to_string(),
            command: "echo {message} --count {count}".to_string(),
            args: [
                ("message".to_string(), "The message to echo".to_string()),
                ("count".to_string(), "Number of times".to_string()),
            ]
            .iter()
            .cloned()
            .collect(),
            tags: vec![],
            terminal: false,
        };

        let security = Arc::new(SecurityPolicy::default());
        let handler = SkillToolHandler::new("test-skill".to_string(), tool_def, security).unwrap();
        let schema = handler.generate_schema();

        assert_eq!(schema["type"], "object");
        assert!(schema["properties"]["message"].is_object());
        assert_eq!(schema["properties"]["message"]["type"], "string");
        assert!(schema["properties"]["count"].is_object());
        assert_eq!(schema["properties"]["count"]["type"], "integer");
    }

    #[test]
    fn render_command_with_all_args() {
        let tool_def = SkillTool {
            name: "test_tool".to_string(),
            description: "Test".to_string(),
            kind: "shell".to_string(),
            command: "python3 script.py --limit {limit} --name {name}".to_string(),
            args: [
                ("limit".to_string(), "Maximum number of items".to_string()),
                ("name".to_string(), "User name".to_string()),
            ]
            .iter()
            .cloned()
            .collect(),
            tags: vec![],
            terminal: false,
        };

        let security = Arc::new(SecurityPolicy::default());
        let handler = SkillToolHandler::new("test".to_string(), tool_def, security).unwrap();

        let args = serde_json::json!({
            "limit": 100,
            "name": "alice"
        });

        let command = handler.render_command(&args).unwrap();
        // limit is integer, should not be quoted
        assert!(command.contains("--limit 100"));
        // name is string, should be quoted
        assert!(command.contains("--name 'alice'"));
    }

    #[test]
    fn render_command_with_optional_params_omitted() {
        let tool_def = SkillTool {
            name: "test_tool".to_string(),
            description: "Test".to_string(),
            kind: "shell".to_string(),
            command: "python3 script.py --required {required} --optional {optional}".to_string(),
            args: [
                ("required".to_string(), "Required value".to_string()),
                ("optional".to_string(), "Optional value".to_string()),
            ]
            .iter()
            .cloned()
            .collect(),
            tags: vec![],
            terminal: false,
        };

        let security = Arc::new(SecurityPolicy::default());
        let handler = SkillToolHandler::new("test".to_string(), tool_def, security).unwrap();

        let args = serde_json::json!({
            "required": "value"
        });

        let command = handler.render_command(&args).unwrap();
        // Strings are now quoted
        assert!(command.contains("--required 'value'"));
        assert!(!command.contains("--optional"));
    }

    #[test]
    fn shell_escape_prevents_injection() {
        let tool_def = SkillTool {
            name: "test_tool".to_string(),
            description: "Test".to_string(),
            kind: "shell".to_string(),
            command: "echo {message}".to_string(),
            args: [("message".to_string(), "A text message".to_string())]
                .iter()
                .cloned()
                .collect(),
            tags: vec![],
            terminal: false,
        };

        let security = Arc::new(SecurityPolicy::default());
        let handler = SkillToolHandler::new("test".to_string(), tool_def, security).unwrap();

        let args = serde_json::json!({
            "message": "hello; rm -rf /"
        });

        let command = handler.render_command(&args).unwrap();
        // Shell escape should quote the entire string
        // Our implementation uses single quotes: 'hello; rm -rf /'
        assert!(command.contains("echo '"));
        assert!(command.contains("rm -rf")); // Should be inside quotes
                                             // The dangerous part should NOT be outside quotes (no unquoted semicolon)
        assert!(!command.starts_with("echo hello; rm"));
    }

    #[test]
    fn render_command_removes_optional_flags_with_dashes() {
        let tool_def = SkillTool {
            name: "telegram_search".to_string(),
            description: "Search Telegram".to_string(),
            kind: "shell".to_string(),
            command: "python3 script.py --contact-name {contact_name} --query {query} --date-from {date_from} --limit {limit}".to_string(),
            args: [
                ("contact_name".to_string(), "Contact ID".to_string()),
                ("query".to_string(), "Search query (optional)".to_string()),
                ("date_from".to_string(), "Start date (optional)".to_string()),
                ("limit".to_string(), "Maximum results".to_string()),
            ]
            .iter()
            .cloned()
            .collect(),
        tags: vec![],
        terminal: false,
        };

        let security = Arc::new(SecurityPolicy::default());
        let handler = SkillToolHandler::new("test".to_string(), tool_def, security).unwrap();

        // Only provide contact_name and limit, omit query and date_from
        let args = serde_json::json!({
            "contact_name": "alice",
            "limit": 50
        });

        let command = handler.render_command(&args).unwrap();

        // Should contain provided params
        assert!(command.contains("--contact-name 'alice'"));
        assert!(command.contains("--limit 50"));

        // Should NOT contain optional flags when params are missing
        assert!(!command.contains("--query"));
        assert!(!command.contains("--date-from"));
    }

    #[test]
    fn null_param_skipped_in_render_command() {
        let tool_def = SkillTool {
            name: "telegram_search".to_string(),
            description: "Search Telegram".to_string(),
            kind: "shell".to_string(),
            command: "python3 script.py --query {query} --channel-filter {channel_filter} --limit {limit}".to_string(),
            args: [
                ("query".to_string(), "Search query".to_string()),
                ("channel_filter".to_string(), "Channel filter (optional)".to_string()),
                ("limit".to_string(), "Maximum number of results".to_string()),
            ]
            .iter()
            .cloned()
            .collect(),
        tags: vec![],
        terminal: false,
        };

        let security = Arc::new(SecurityPolicy::default());
        let handler = SkillToolHandler::new("test".to_string(), tool_def, security).unwrap();

        // null value should cause the entire --channel-filter {channel_filter} to be removed
        let args = serde_json::json!({
            "query": "test search",
            "channel_filter": null,
            "limit": 50
        });

        let command = handler.render_command(&args).unwrap();

        assert!(command.contains("--query 'test search'"));
        assert!(command.contains("--limit 50"));
        // null param should be completely removed, not rendered as empty string
        assert!(
            !command.contains("--channel-filter"),
            "null param should remove --channel-filter entirely, got: {command}"
        );
    }

    #[test]
    fn render_command_quotes_numeric_strings() {
        let tool_def = SkillTool {
            name: "telegram_search".to_string(),
            description: "Search Telegram".to_string(),
            kind: "shell".to_string(),
            command: "python3 script.py --contact-name {contact_name} --limit {limit}".to_string(),
            args: [
                (
                    "contact_name".to_string(),
                    "Telegram contact username or ID".to_string(),
                ),
                ("limit".to_string(), "Maximum number of results".to_string()),
            ]
            .iter()
            .cloned()
            .collect(),
            tags: vec![],
            terminal: false,
        };

        let security = Arc::new(SecurityPolicy::default());
        let handler = SkillToolHandler::new("test".to_string(), tool_def, security).unwrap();

        // Model sends contact_name as integer (use i64 for large Telegram IDs)
        let args = serde_json::json!({
            "contact_name": 5_084_292_206_i64,
            "limit": 100
        });

        let command = handler.render_command(&args).unwrap();

        // contact_name should be quoted (it's a String type by inference)
        assert!(command.contains("--contact-name '5084292206'"));

        // limit should NOT be quoted (it's an Integer type)
        assert!(command.contains("--limit 100"));
        assert!(!command.contains("--limit '100'"));
    }

    #[test]
    fn empty_string_param_skipped_in_render_command() {
        let tool_def = SkillTool {
            name: "telegram_search".to_string(),
            description: "Search Telegram".to_string(),
            kind: "shell".to_string(),
            command: "python3 script.py --query {query} --date-from {date_from} --channel-filter {channel_filter}".to_string(),
            args: [
                ("query".to_string(), "Search query".to_string()),
                ("date_from".to_string(), "Start date (optional)".to_string()),
                ("channel_filter".to_string(), "Channel filter (optional)".to_string()),
            ]
            .iter()
            .cloned()
            .collect(),
        tags: vec![],
        terminal: false,
        };

        let security = Arc::new(SecurityPolicy::default());
        let handler = SkillToolHandler::new("test".to_string(), tool_def, security).unwrap();

        // LLM sends "" for optional params — should be treated same as null
        let args = serde_json::json!({
            "query": "test search",
            "date_from": "",
            "channel_filter": ""
        });

        let command = handler.render_command(&args).unwrap();

        assert!(command.contains("--query 'test search'"));
        assert!(
            !command.contains("--date-from"),
            "empty string param should remove --date-from entirely, got: {command}"
        );
        assert!(
            !command.contains("--channel-filter"),
            "empty string param should remove --channel-filter entirely, got: {command}"
        );
    }

    #[test]
    fn infer_required_from_description() {
        let tool_def = SkillTool {
            name: "telegram_search".to_string(),
            description: "Search Telegram".to_string(),
            kind: "shell".to_string(),
            command: "python3 script.py --query {query} --date-from {date_from} --channel-filter {channel_filter} --limit {limit}".to_string(),
            args: [
                ("query".to_string(), "Search query text".to_string()),
                ("date_from".to_string(), "Start date (optional)".to_string()),
                ("channel_filter".to_string(), "Channel name (optional)".to_string()),
                ("limit".to_string(), "Maximum number of results. Default: 50".to_string()),
            ]
            .iter()
            .cloned()
            .collect(),
        tags: vec![],
        terminal: false,
        };

        let security = Arc::new(SecurityPolicy::default());
        let handler = SkillToolHandler::new("test".to_string(), tool_def, security).unwrap();

        let by_name: HashMap<&str, &SkillToolParameter> = handler
            .parameters
            .iter()
            .map(|p| (p.name.as_str(), p))
            .collect();

        assert!(by_name["query"].required, "query should be required");
        assert!(
            !by_name["date_from"].required,
            "date_from should be optional"
        );
        assert!(
            !by_name["channel_filter"].required,
            "channel_filter should be optional"
        );
        assert!(
            !by_name["limit"].required,
            "limit with default should be optional"
        );
    }

    #[tokio::test]
    async fn render_fail_returns_soft_error_not_err() {
        let tool_def = SkillTool {
            name: "telegram_search_global".to_string(),
            description: "Search Telegram globally".to_string(),
            kind: "shell".to_string(),
            command: "python3 script.py --query {query} --date-from {date_from}".to_string(),
            args: [
                ("query".to_string(), "Search query".to_string()),
                ("date_from".to_string(), "Start date (optional)".to_string()),
            ]
            .iter()
            .cloned()
            .collect(),
            tags: vec![],
            terminal: false,
        };

        let security = Arc::new(SecurityPolicy::default());
        let handler =
            SkillToolHandler::new("zeroclaw_skill".to_string(), tool_def, security).unwrap();

        // LLM sends an Array instead of a String for date_from — must not propagate Err
        let args = serde_json::json!({
            "query": "x",
            "date_from": [1, 2, 3]
        });

        let result = handler.execute(args).await;
        assert!(
            result.is_ok(),
            "execute must return Ok, not Err on render fail"
        );
        let tool_result = result.unwrap();
        assert!(
            !tool_result.success,
            "ToolResult.success must be false for render error"
        );
        assert!(
            tool_result
                .output
                .to_lowercase()
                .contains("invalid tool arguments"),
            "output must explain the argument error, got: {}",
            tool_result.output
        );
    }
}
