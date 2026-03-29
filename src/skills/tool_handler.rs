//! Skill tool handler — bridges SKILL.toml shell-based tool definitions to native tool calling.
//!
//! Each `[[tools]]` entry in a SKILL.toml becomes a native `Tool` trait object
//! that LLM providers can call via structured function calling. The handler
//! parses `{placeholder}` templates, generates JSON schemas, shell-escapes
//! arguments, and executes commands in a sandboxed environment.

use crate::security::SecurityPolicy;
use crate::skills::SkillTool;
use crate::tools::shell::collect_allowed_shell_env_vars;
use crate::tools::traits::{Tool, ToolResult};
use anyhow::{Context, Result, bail};
use async_trait::async_trait;
use regex::Regex;
use std::collections::{HashMap, HashSet};
use std::path::PathBuf;
use std::sync::{Arc, LazyLock};

/// Regex to extract `{placeholder}` names from command templates.
static PLACEHOLDER_REGEX: LazyLock<Regex> =
    LazyLock::new(|| Regex::new(r"\{(\w+)\}").expect("placeholder regex compilation failed"));

/// Parameter metadata for skill tools.
#[derive(Debug, Clone)]
pub struct SkillToolParameter {
    pub name: String,
    pub description: String,
    pub required: bool,
    pub param_type: ParameterType,
}

/// Supported parameter types for skill tools.
#[derive(Debug, Clone, PartialEq)]
pub enum ParameterType {
    String,
    Integer,
    Boolean,
}

/// Skill tool handler implementing the [`Tool`] trait.
pub struct SkillToolHandler {
    skill_name: String,
    tool_def: SkillTool,
    parameters: Vec<SkillToolParameter>,
    security: Arc<SecurityPolicy>,
    /// Directory containing the SKILL.toml — set as `SKILL_DIR` env var.
    skill_dir: Option<PathBuf>,
    /// Per-tool concurrency limiter. None = unlimited.
    concurrency_limit: Option<Arc<tokio::sync::Semaphore>>,
    /// Gateway credentials for trusted skills (None for untrusted).
    gateway_token: Option<String>,
    gateway_url: Option<String>,
}

impl SkillToolHandler {
    /// Create a new skill tool handler from a skill tool definition.
    ///
    /// `gateway_creds` is `Some((token, url))` for trusted skills that should
    /// receive gateway credentials as environment variables.
    pub fn new(
        skill_name: String,
        tool_def: SkillTool,
        security: Arc<SecurityPolicy>,
        skill_dir: Option<PathBuf>,
        gateway_creds: Option<(String, String)>,
    ) -> Result<Self> {
        if !tool_def.kind.eq_ignore_ascii_case("shell") {
            bail!(
                "Unsupported tool kind '{}': only shell tools are supported",
                tool_def.kind
            );
        }
        let parameters = Self::extract_parameters(&tool_def)?;
        let concurrency_limit = tool_def
            .max_parallel
            .map(|n| Arc::new(tokio::sync::Semaphore::new(n)));
        let (gateway_token, gateway_url) = match gateway_creds {
            Some((t, u)) => (Some(t), Some(u)),
            None => (None, None),
        };
        Ok(Self {
            skill_name,
            tool_def,
            parameters,
            security,
            skill_dir,
            concurrency_limit,
            gateway_token,
            gateway_url,
        })
    }

    /// Extract parameter definitions from tool args and command template.
    fn extract_parameters(tool_def: &SkillTool) -> Result<Vec<SkillToolParameter>> {
        let placeholders = Self::extract_placeholders(&tool_def.command);
        let mut parameters = Vec::new();

        for placeholder in placeholders {
            let description = tool_def
                .args
                .get(&placeholder)
                .cloned()
                .unwrap_or_else(|| format!("Parameter: {}", placeholder));

            let param_type = Self::infer_parameter_type(&description);

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

    /// Extract `{placeholder}` names from command template, preserving order.
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

    /// Infer parameter type from description text.
    fn infer_parameter_type(description: &str) -> ParameterType {
        let desc_lower = description.to_lowercase();

        // "number of X" / "count of" / "limit" / "maximum" / "minimum" → integer
        // But NOT "phone number", "account number", "order number", "ID number", etc.
        // Use word-boundary-aware checks to avoid false positives (e.g. "account" contains "count").
        let has_integer_hint = desc_lower.contains("number of")
            || desc_lower.contains("limit")
            || desc_lower.contains("maximum")
            || desc_lower.contains("minimum")
            || desc_lower
                .split(|c: char| !c.is_alphanumeric())
                .any(|w| w == "count");

        if has_integer_hint {
            return ParameterType::Integer;
        }

        if desc_lower.contains("enable")
            || desc_lower.contains("disable")
            || desc_lower.contains("true")
            || desc_lower.contains("false")
            || desc_lower.contains("flag")
        {
            return ParameterType::Boolean;
        }

        ParameterType::String
    }

    /// Generate JSON schema for tool parameters.
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

    /// Substitute arguments into command template.
    fn render_command(&self, args: &serde_json::Value) -> Result<String> {
        let mut command = self.tool_def.command.clone();

        let args_obj = args
            .as_object()
            .context("Tool arguments must be a JSON object")?;

        let param_types: HashMap<String, ParameterType> = self
            .parameters
            .iter()
            .map(|p| (p.name.clone(), p.param_type.clone()))
            .collect();
        let required_params: HashSet<&str> = self
            .parameters
            .iter()
            .filter(|p| p.required)
            .map(|p| p.name.as_str())
            .collect();

        // Build a map of available arguments; error on null/empty required params.
        let mut arg_values = HashMap::new();
        for (key, value) in args_obj {
            let is_required = required_params.contains(key.as_str());
            if value.is_null() {
                if is_required {
                    bail!("Required parameter '{}' must not be null", key);
                }
                continue;
            }
            let value_str = Self::format_argument_value(value)?;
            if value_str.is_empty() {
                if is_required {
                    bail!("Required parameter '{}' must not be empty", key);
                }
                continue;
            }
            arg_values.insert(key.clone(), value_str);
        }

        // Replace placeholders.
        let placeholders = Self::extract_placeholders(&command);
        for placeholder in placeholders {
            let pattern = format!("{{{}}}", placeholder);

            if let Some(value) = arg_values.get(&placeholder) {
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
                // Parameter not provided — remove the flag/option entirely.
                let flag_name = placeholder.replace('_', "-");

                let flag_patterns = [
                    format!("--{} {}", flag_name, pattern),
                    format!("--{}={}", flag_name, pattern),
                    format!("-{} {}", flag_name.chars().next().unwrap_or('x'), pattern),
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
                    command = command.replace(&pattern, "");
                }
            }
        }

        // Clean up extra whitespace.
        command = command.split_whitespace().collect::<Vec<_>>().join(" ");

        Ok(command)
    }

    /// Format a JSON value as a string for shell substitution.
    fn format_argument_value(value: &serde_json::Value) -> Result<String> {
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
        &self.tool_def.tags
    }

    fn is_terminal(&self) -> bool {
        self.tool_def.terminal
    }

    fn max_result_chars(&self) -> Option<usize> {
        self.tool_def.max_result_chars
    }

    fn max_calls_per_turn(&self) -> Option<usize> {
        self.tool_def.max_calls_per_turn
    }

    async fn execute(&self, args: serde_json::Value) -> Result<ToolResult> {
        // Per-tool concurrency limit — held for the duration of execute()
        let _permit = match &self.concurrency_limit {
            Some(sem) => Some(
                sem.acquire()
                    .await
                    .map_err(|e| anyhow::anyhow!("Semaphore closed: {e}"))?,
            ),
            None => None,
        };

        if self.security.is_rate_limited() {
            return Ok(ToolResult {
                output: "Rate limit exceeded — try again later.".into(),
                success: false,
                error: None,
            });
        }

        let command = match self.render_command(&args) {
            Ok(cmd) => cmd,
            Err(e) => {
                anyhow::bail!(
                    "Failed to render skill tool command. args={} error={}",
                    args,
                    e
                );
            }
        };

        // Trusted skills (those with gateway credentials) skip command validation.
        // Their commands contain user-provided text that may include shell
        // metacharacters (>, <, backticks) which would otherwise be blocked.
        let is_trusted = self.gateway_token.is_some();
        if !is_trusted {
            if let Err(e) = self.security.validate_command_execution(&command, false) {
                tracing::warn!(
                    skill = %self.skill_name,
                    tool = %self.tool_def.name,
                    reason = %e,
                    "Skill tool blocked by security policy"
                );
                return Ok(ToolResult {
                    output: format!("Blocked by security policy: {e}"),
                    success: false,
                    error: None,
                });
            }
        }

        if !self.security.record_action() {
            return Ok(ToolResult {
                output: "Action limit exceeded — try again later.".into(),
                success: false,
                error: None,
            });
        }

        let args_summary: String = args.to_string().chars().take(200).collect();
        tracing::info!(
            skill = %self.skill_name,
            tool = %self.tool_def.name,
            args = %args_summary,
            "Executing skill tool"
        );

        let start = std::time::Instant::now();

        // Sandboxed execution: clear env, pass only allowed vars + SKILL_DIR.
        let mut cmd = tokio::process::Command::new("bash");
        cmd.arg("-c").arg(&command);
        cmd.env_clear();

        for var in collect_allowed_shell_env_vars(&self.security) {
            if let Ok(val) = std::env::var(&var) {
                cmd.env(&var, val);
            }
        }

        if let Some(ref dir) = self.skill_dir {
            cmd.env("SKILL_DIR", dir);
        }

        for (key, val) in &self.tool_def.env {
            cmd.env(key, val);
        }

        // Inject gateway credentials for trusted skills.
        if let Some(ref token) = self.gateway_token {
            cmd.env("ZEROCLAW_GATEWAY_TOKEN", token);
        }
        if let Some(ref url) = self.gateway_url {
            cmd.env("ZEROCLAW_GATEWAY_URL", url);
        }

        // Inject reply-to message ID from channel context so skill scripts
        // can reply to the user's original message.
        if let Ok(Some(reply_to)) =
            crate::agent::loop_::TOOL_LOOP_REPLY_TO_MESSAGE_ID.try_with(|v| v.clone())
        {
            cmd.env("ZC_REPLY_TO_MESSAGE_ID", reply_to);
        }

        // Inject thread ID from channel context so skill scripts
        // can route replies to the correct Telegram thread.
        if let Ok(Some(thread_id)) =
            crate::agent::loop_::TOOL_LOOP_THREAD_ID.try_with(|v| v.clone())
        {
            cmd.env("ZC_THREAD_ID", thread_id);
        }

        // Inject the full conversation-history sender key so skill scripts can call
        // DELETE /api/history/{sender_key} to clear ZeroClaw's in-memory history.
        if let Ok(Some(sender_key)) =
            crate::agent::loop_::TOOL_LOOP_SENDER_KEY.try_with(|v| v.clone())
        {
            cmd.env("ZC_SENDER_KEY", sender_key.clone());
        }

        let output = cmd
            .output()
            .await
            .context("Failed to execute skill tool command")?;

        let duration = start.elapsed();
        let stdout = String::from_utf8_lossy(&output.stdout).to_string();
        let stderr = String::from_utf8_lossy(&output.stderr).to_string();
        let success = output.status.success();

        let scrubbed_stdout = crate::agent::loop_::scrub_credentials(&stdout);
        let scrubbed_stderr = crate::agent::loop_::scrub_credentials(&stderr);

        let output_preview: String = scrubbed_stdout.chars().take(200).collect();
        if success {
            tracing::info!(
                skill = %self.skill_name,
                tool = %self.tool_def.name,
                duration_ms = u64::try_from(duration.as_millis()).unwrap_or(u64::MAX),
                output_preview = %output_preview,
                "Skill tool completed"
            );
        } else {
            tracing::warn!(
                skill = %self.skill_name,
                tool = %self.tool_def.name,
                duration_ms = u64::try_from(duration.as_millis()).unwrap_or(u64::MAX),
                exit_code = ?output.status.code(),
                stderr = %scrubbed_stderr.chars().take(500).collect::<String>(),
                "Skill tool failed"
            );
        }

        Ok(ToolResult {
            success,
            output: if success {
                scrubbed_stdout
            } else if !scrubbed_stdout.is_empty() {
                // Prefer stdout: skill scripts print structured error JSON to stdout
                // even on failure (exit 1). Include stderr as a suffix if present.
                if scrubbed_stderr.is_empty() {
                    scrubbed_stdout
                } else {
                    format!("{}\n{}", scrubbed_stdout, scrubbed_stderr)
                }
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

    fn test_tool_def(command: &str, args: HashMap<String, String>) -> SkillTool {
        SkillTool {
            name: "test_tool".to_string(),
            description: "Test tool".to_string(),
            kind: "shell".to_string(),
            command: command.to_string(),
            args,
            tags: vec!["test-tag".to_string()],
            terminal: true,
            max_parallel: None,
            max_result_chars: None,
            max_calls_per_turn: None,
            env: HashMap::new(),
        }
    }

    #[test]
    fn test_skill_tool_handler_name() {
        let tool_def = test_tool_def("echo hello", HashMap::new());
        let security = Arc::new(SecurityPolicy::default());
        let handler =
            SkillToolHandler::new("test-skill".into(), tool_def, security, None, None).unwrap();
        assert_eq!(handler.name(), "test_tool");
    }

    #[test]
    fn test_skill_tool_handler_description() {
        let tool_def = test_tool_def("echo hello", HashMap::new());
        let security = Arc::new(SecurityPolicy::default());
        let handler =
            SkillToolHandler::new("test-skill".into(), tool_def, security, None, None).unwrap();
        assert_eq!(handler.description(), "Test tool");
    }

    #[test]
    fn test_skill_tool_handler_tags() {
        let tool_def = test_tool_def("echo hello", HashMap::new());
        let security = Arc::new(SecurityPolicy::default());
        let handler =
            SkillToolHandler::new("test-skill".into(), tool_def, security, None, None).unwrap();
        assert_eq!(handler.tags(), &["test-tag".to_string()]);
    }

    #[test]
    fn test_skill_tool_handler_terminal() {
        let tool_def = test_tool_def("echo hello", HashMap::new());
        let security = Arc::new(SecurityPolicy::default());
        let handler =
            SkillToolHandler::new("test-skill".into(), tool_def, security, None, None).unwrap();
        assert!(handler.is_terminal());
    }

    #[test]
    fn test_skill_tool_handler_schema() {
        let args = [
            ("message".to_string(), "The message to echo".to_string()),
            ("count".to_string(), "Number of times".to_string()),
        ]
        .into_iter()
        .collect();
        let tool_def = test_tool_def("echo {message} --count {count}", args);
        let security = Arc::new(SecurityPolicy::default());
        let handler =
            SkillToolHandler::new("test-skill".into(), tool_def, security, None, None).unwrap();
        let schema = handler.parameters_schema();

        assert_eq!(schema["type"], "object");
        assert_eq!(schema["properties"]["message"]["type"], "string");
        assert_eq!(schema["properties"]["count"]["type"], "integer");
    }

    #[test]
    fn extract_placeholders_from_command() {
        let placeholders = SkillToolHandler::extract_placeholders(
            "python3 script.py --limit {limit} --name {name}",
        );
        assert_eq!(placeholders, vec!["limit", "name"]);
    }

    #[test]
    fn extract_placeholders_deduplicates() {
        let placeholders = SkillToolHandler::extract_placeholders("echo {value} and {value} again");
        assert_eq!(placeholders, vec!["value"]);
    }

    #[test]
    fn infer_integer_type() {
        assert_eq!(
            SkillToolHandler::infer_parameter_type("Maximum number of items"),
            ParameterType::Integer
        );
        assert_eq!(
            SkillToolHandler::infer_parameter_type("number of results"),
            ParameterType::Integer
        );
        assert_eq!(
            SkillToolHandler::infer_parameter_type("count of messages"),
            ParameterType::Integer
        );
        assert_eq!(
            SkillToolHandler::infer_parameter_type("limit"),
            ParameterType::Integer
        );
    }

    #[test]
    fn infer_boolean_type() {
        assert_eq!(
            SkillToolHandler::infer_parameter_type("Enable verbose mode"),
            ParameterType::Boolean
        );
    }

    #[test]
    fn infer_string_type_default() {
        assert_eq!(
            SkillToolHandler::infer_parameter_type("User name or email"),
            ParameterType::String
        );
    }

    #[test]
    fn infer_string_for_phone_number() {
        // "phone number" should NOT trigger integer inference
        assert_eq!(
            SkillToolHandler::infer_parameter_type("Telegram contact username or phone number"),
            ParameterType::String
        );
        assert_eq!(
            SkillToolHandler::infer_parameter_type("account number or ID"),
            ParameterType::String
        );
        assert_eq!(
            SkillToolHandler::infer_parameter_type("order number"),
            ParameterType::String
        );
    }

    #[test]
    fn render_command_with_all_args() {
        let args_map = [
            ("limit".to_string(), "Maximum number of items".to_string()),
            ("name".to_string(), "User name".to_string()),
        ]
        .into_iter()
        .collect();
        let tool_def = test_tool_def("python3 script.py --limit {limit} --name {name}", args_map);
        let security = Arc::new(SecurityPolicy::default());
        let handler = SkillToolHandler::new("test".into(), tool_def, security, None, None).unwrap();

        let args = serde_json::json!({"limit": 100, "name": "alice"});
        let command = handler.render_command(&args).unwrap();
        assert!(command.contains("--limit 100"));
        assert!(command.contains("--name 'alice'"));
    }

    #[test]
    fn render_command_with_optional_params_omitted() {
        let args_map = [
            ("required".to_string(), "Required value".to_string()),
            ("optional".to_string(), "Optional value".to_string()),
        ]
        .into_iter()
        .collect();
        let tool_def = test_tool_def(
            "python3 script.py --required {required} --optional {optional}",
            args_map,
        );
        let security = Arc::new(SecurityPolicy::default());
        let handler = SkillToolHandler::new("test".into(), tool_def, security, None, None).unwrap();

        let args = serde_json::json!({"required": "value"});
        let command = handler.render_command(&args).unwrap();
        assert!(command.contains("--required 'value'"));
        assert!(!command.contains("--optional"));
    }

    #[test]
    fn shell_escape_prevents_injection() {
        let args_map = [("message".to_string(), "A text message".to_string())]
            .into_iter()
            .collect();
        let tool_def = test_tool_def("echo {message}", args_map);
        let security = Arc::new(SecurityPolicy::default());
        let handler = SkillToolHandler::new("test".into(), tool_def, security, None, None).unwrap();

        let args = serde_json::json!({"message": "hello; rm -rf /"});
        let command = handler.render_command(&args).unwrap();
        assert!(command.contains("echo '"));
        assert!(!command.starts_with("echo hello; rm"));
    }

    #[test]
    fn null_param_skipped_in_render_command() {
        let args_map = [
            ("query".to_string(), "Search query".to_string()),
            (
                "channel_filter".to_string(),
                "Channel filter (optional)".to_string(),
            ),
            ("limit".to_string(), "Maximum number of results".to_string()),
        ]
        .into_iter()
        .collect();
        let tool_def = test_tool_def(
            "python3 script.py --query {query} --channel-filter {channel_filter} --limit {limit}",
            args_map,
        );
        let security = Arc::new(SecurityPolicy::default());
        let handler = SkillToolHandler::new("test".into(), tool_def, security, None, None).unwrap();

        let args = serde_json::json!({"query": "test search", "channel_filter": null, "limit": 50});
        let command = handler.render_command(&args).unwrap();
        assert!(command.contains("--query 'test search'"));
        assert!(command.contains("--limit 50"));
        assert!(!command.contains("--channel-filter"));
    }

    #[test]
    fn empty_string_param_skipped() {
        let args_map = [
            ("query".to_string(), "Search query".to_string()),
            ("date_from".to_string(), "Start date (optional)".to_string()),
        ]
        .into_iter()
        .collect();
        let tool_def = test_tool_def(
            "python3 script.py --query {query} --date-from {date_from}",
            args_map,
        );
        let security = Arc::new(SecurityPolicy::default());
        let handler = SkillToolHandler::new("test".into(), tool_def, security, None, None).unwrap();

        let args = serde_json::json!({"query": "test", "date_from": ""});
        let command = handler.render_command(&args).unwrap();
        assert!(command.contains("--query 'test'"));
        assert!(!command.contains("--date-from"));
    }

    #[test]
    fn render_command_quotes_numeric_strings() {
        let args_map = [
            (
                "contact_name".to_string(),
                "Telegram contact username or ID".to_string(),
            ),
            ("limit".to_string(), "Maximum number of results".to_string()),
        ]
        .into_iter()
        .collect();
        let tool_def = test_tool_def(
            "python3 script.py --contact-name {contact_name} --limit {limit}",
            args_map,
        );
        let security = Arc::new(SecurityPolicy::default());
        let handler = SkillToolHandler::new("test".into(), tool_def, security, None, None).unwrap();

        let args = serde_json::json!({"contact_name": 5_084_292_206_i64, "limit": 100});
        let command = handler.render_command(&args).unwrap();
        assert!(command.contains("--contact-name '5084292206'"));
        assert!(command.contains("--limit 100"));
        assert!(!command.contains("--limit '100'"));
    }

    #[test]
    fn unsupported_kind_rejected() {
        let tool_def = SkillTool {
            name: "test".into(),
            description: "test".into(),
            kind: "http".into(),
            command: "GET https://example.com".into(),
            args: HashMap::new(),
            tags: Vec::new(),
            terminal: false,
            max_parallel: None,
            max_result_chars: None,
            max_calls_per_turn: None,
            env: HashMap::new(),
        };
        let security = Arc::new(SecurityPolicy::default());
        let result = SkillToolHandler::new("test".into(), tool_def, security, None, None);
        assert!(result.is_err());
    }

    #[tokio::test]
    async fn test_execute_echo() {
        let tool_def = SkillTool {
            name: "echo_tool".into(),
            description: "Echo a message".into(),
            kind: "shell".into(),
            command: "echo {message}".into(),
            args: [("message".to_string(), "The message".to_string())]
                .into_iter()
                .collect(),
            tags: Vec::new(),
            terminal: false,
            max_parallel: None,
            max_result_chars: None,
            max_calls_per_turn: None,
            env: HashMap::new(),
        };
        let security = Arc::new(SecurityPolicy::default());
        let handler = SkillToolHandler::new("test".into(), tool_def, security, None, None).unwrap();

        let result = handler
            .execute(serde_json::json!({"message": "hello world"}))
            .await
            .unwrap();
        assert!(result.success);
        assert!(result.output.trim().contains("hello world"));
    }

    #[tokio::test]
    async fn test_execute_passes_env_vars() {
        let tool_def = SkillTool {
            name: "env_tool".into(),
            description: "Print env var".into(),
            kind: "shell".into(),
            command: "env".into(),
            args: HashMap::new(),
            tags: Vec::new(),
            terminal: false,
            max_parallel: None,
            max_result_chars: None,
            max_calls_per_turn: None,
            env: [("SKILL_MY_VAR".to_string(), "hello_from_env".to_string())]
                .into_iter()
                .collect(),
        };
        let mut policy = SecurityPolicy::default();
        policy.allowed_commands.push("env".into());
        let security = Arc::new(policy);
        let handler = SkillToolHandler::new("test".into(), tool_def, security, None, None).unwrap();

        let result = handler.execute(serde_json::json!({})).await.unwrap();
        assert!(
            result.success,
            "Command should succeed, got: {}",
            result.output
        );
        assert!(
            result.output.contains("SKILL_MY_VAR=hello_from_env"),
            "Expected env var in output, got: {}",
            result.output
        );
    }

    #[tokio::test]
    async fn test_execute_injects_gateway_creds_for_trusted_skill() {
        let tool_def = SkillTool {
            name: "env_tool".into(),
            description: "Print env".into(),
            kind: "shell".into(),
            command: "env".into(),
            args: HashMap::new(),
            tags: Vec::new(),
            terminal: false,
            max_parallel: None,
            max_result_chars: None,
            max_calls_per_turn: None,
            env: HashMap::new(),
        };
        let mut policy = SecurityPolicy::default();
        policy.allowed_commands.push("env".into());
        let security = Arc::new(policy);
        let creds = Some(("zc_trusted_token".to_string(), "http://gw:1234".to_string()));
        let handler =
            SkillToolHandler::new("trusted-skill".into(), tool_def, security, None, creds).unwrap();

        let result = handler.execute(serde_json::json!({})).await.unwrap();
        assert!(result.success, "Command should succeed: {}", result.output);
        assert!(
            result.output.contains("ZEROCLAW_GATEWAY_TOKEN="),
            "Should contain gateway token env var: {}",
            result.output
        );
        assert!(
            result
                .output
                .contains("ZEROCLAW_GATEWAY_URL=http://gw:1234"),
            "Should contain gateway URL env var: {}",
            result.output
        );
    }

    #[tokio::test]
    async fn test_execute_no_gateway_creds_for_untrusted_skill() {
        let tool_def = SkillTool {
            name: "env_tool".into(),
            description: "Print env".into(),
            kind: "shell".into(),
            command: "env".into(),
            args: HashMap::new(),
            tags: Vec::new(),
            terminal: false,
            max_parallel: None,
            max_result_chars: None,
            max_calls_per_turn: None,
            env: HashMap::new(),
        };
        let mut policy = SecurityPolicy::default();
        policy.allowed_commands.push("env".into());
        let security = Arc::new(policy);
        let handler =
            SkillToolHandler::new("untrusted-skill".into(), tool_def, security, None, None)
                .unwrap();

        let result = handler.execute(serde_json::json!({})).await.unwrap();
        assert!(result.success, "Command should succeed: {}", result.output);
        assert!(
            !result.output.contains("ZEROCLAW_GATEWAY_TOKEN="),
            "Should NOT contain gateway token: {}",
            result.output
        );
    }

    #[tokio::test]
    async fn test_zc_thread_id_injected() {
        use crate::agent::loop_::{TOOL_LOOP_THREAD_ID, scope_thread_id};
        let thread_id = Some("tg_thread_42".to_string());
        let result = scope_thread_id(thread_id.clone(), None, async {
            TOOL_LOOP_THREAD_ID.try_with(|v| v.clone()).unwrap()
        })
        .await;
        assert_eq!(result, thread_id);
    }

    #[tokio::test]
    async fn test_zc_thread_id_none_when_unset() {
        use crate::agent::loop_::TOOL_LOOP_THREAD_ID;
        // Outside any scope, try_with returns Err (not set), so env injection is skipped
        let result = TOOL_LOOP_THREAD_ID.try_with(|v| v.clone());
        assert!(result.is_err());
    }
}
