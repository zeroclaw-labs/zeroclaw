//! Shell-based tool derived from a skill's `[[tools]]` section.
//!
//! Each `SkillTool` with `kind = "shell"` or `kind = "script"` is converted
//! into a `SkillShellTool` that implements the `Tool` trait. The tool name is
//! prefixed with the skill name (e.g. `my_skill.run_lint`) to avoid collisions
//! with built-in tools.

use crate::security::SecurityPolicy;
use crate::tools::shell::collect_allowed_shell_env_vars;
use async_trait::async_trait;
use std::collections::HashMap;
use std::sync::Arc;
use std::time::Duration;
use zeroclaw_api::tool::{Tool, ToolResult};

/// Default maximum execution time for a skill shell command (seconds).
const DEFAULT_SKILL_SHELL_TIMEOUT_SECS: u64 = 60;
/// Maximum output size in bytes (1 MB).
const MAX_OUTPUT_BYTES: usize = 1_048_576;

#[derive(Clone, Copy, Debug, Eq, PartialEq)]
enum ShellQuoteState {
    None,
    Single,
    Double,
}

/// A tool derived from a skill's `[[tools]]` section that executes shell commands.
pub struct SkillShellTool {
    tool_name: String,
    tool_description: String,
    command_template: String,
    args: HashMap<String, String>,
    security: Arc<SecurityPolicy>,
    timeout_secs: u64,
}

impl SkillShellTool {
    /// Create a new skill shell tool.
    ///
    /// The tool name is prefixed with the skill name (`skill_name__tool_name`)
    /// to prevent collisions with built-in tools.
    ///
    /// Both the skill name and tool name are sanitized: hyphens are replaced
    /// with underscores and dots are replaced with underscores to satisfy the
    /// `[a-zA-Z0-9_]+` constraint required by Bedrock and other providers.
    pub fn new(
        skill_name: &str,
        tool: &crate::skills::SkillTool,
        security: Arc<SecurityPolicy>,
    ) -> Self {
        let safe_skill = skill_name.replace('-', "_").replace('.', "_");
        let safe_tool = tool.name.replace('-', "_").replace('.', "_");
        Self {
            tool_name: format!("{}__{}", safe_skill, safe_tool),
            tool_description: tool.description.clone(),
            command_template: tool.command.clone(),
            args: tool.args.clone(),
            security,
            timeout_secs: tool
                .timeout_secs
                .unwrap_or(DEFAULT_SKILL_SHELL_TIMEOUT_SECS)
                .max(1),
        }
    }

    fn build_parameters_schema(&self) -> serde_json::Value {
        let mut properties = serde_json::Map::new();
        let mut required = Vec::new();

        for (name, description) in &self.args {
            properties.insert(
                name.clone(),
                serde_json::json!({
                    "type": "string",
                    "description": description
                }),
            );
            required.push(serde_json::Value::String(name.clone()));
        }

        serde_json::json!({
            "type": "object",
            "properties": properties,
            "required": required
        })
    }

    /// Substitute `{{arg_name}}` placeholders in the command template with
    /// the provided argument values. Unknown placeholders are left as-is.
    fn substitute_args(&self, args: &serde_json::Value) -> String {
        let Some(obj) = args.as_object() else {
            return self.command_template.clone();
        };

        let template = self.command_template.as_str();
        let mut rendered = String::with_capacity(template.len());
        let mut i = 0usize;
        let mut quote = ShellQuoteState::None;
        let mut escaped = false;

        while i < template.len() {
            if template[i..].starts_with("{{") {
                if let Some(close_rel) = template[i + 2..].find("}}") {
                    let end = i + 2 + close_rel;
                    let key = &template[i + 2..end];
                    if let Some(value) = obj.get(key).and_then(serde_json::Value::as_str) {
                        rendered.push_str(&Self::escape_placeholder_value(value, quote));
                    } else {
                        rendered.push_str(&template[i..end + 2]);
                    }
                    i = end + 2;
                    continue;
                }
            }

            let ch = template[i..]
                .chars()
                .next()
                .expect("template slicing should always align to char boundary");
            rendered.push(ch);

            match quote {
                ShellQuoteState::Single => {
                    if ch == '\'' {
                        quote = ShellQuoteState::None;
                    }
                }
                ShellQuoteState::Double => {
                    if escaped {
                        escaped = false;
                    } else if ch == '\\' {
                        escaped = true;
                    } else if ch == '"' {
                        quote = ShellQuoteState::None;
                    }
                }
                ShellQuoteState::None => {
                    if escaped {
                        escaped = false;
                    } else if ch == '\\' {
                        escaped = true;
                    } else if ch == '\'' {
                        quote = ShellQuoteState::Single;
                    } else if ch == '"' {
                        quote = ShellQuoteState::Double;
                    }
                }
            }

            i += ch.len_utf8();
        }

        rendered
    }

    fn escape_placeholder_value(value: &str, quote: ShellQuoteState) -> String {
        match quote {
            ShellQuoteState::Double => Self::escape_for_double_quotes(value),
            ShellQuoteState::Single => value.replace('\'', "'\\''"),
            ShellQuoteState::None => Self::shell_single_quote(value),
        }
    }

    fn escape_for_double_quotes(value: &str) -> String {
        let mut escaped = String::with_capacity(value.len());
        for ch in value.chars() {
            match ch {
                '\\' | '"' | '$' | '`' => {
                    escaped.push('\\');
                    escaped.push(ch);
                }
                _ => escaped.push(ch),
            }
        }
        escaped
    }

    fn shell_single_quote(value: &str) -> String {
        let mut quoted = String::with_capacity(value.len() + 2);
        quoted.push('\'');
        for ch in value.chars() {
            if ch == '\'' {
                quoted.push_str("'\"'\"'");
            } else {
                quoted.push(ch);
            }
        }
        quoted.push('\'');
        quoted
    }
}

#[async_trait]
impl Tool for SkillShellTool {
    fn name(&self) -> &str {
        &self.tool_name
    }

    fn description(&self) -> &str {
        &self.tool_description
    }

    fn parameters_schema(&self) -> serde_json::Value {
        self.build_parameters_schema()
    }

    async fn execute(&self, args: serde_json::Value) -> anyhow::Result<ToolResult> {
        let command = self.substitute_args(&args);

        // Rate limit check
        if self.security.is_rate_limited() {
            return Ok(ToolResult {
                success: false,
                output: String::new(),
                error: Some("Rate limit exceeded: too many actions in the last hour".into()),
            });
        }

        // Security validation — always requires explicit approval (approved=true)
        // since skill tools are user-defined and should be treated as medium-risk.
        match self.security.validate_command_execution(&command, true) {
            Ok(_) => {}
            Err(reason) => {
                return Ok(ToolResult {
                    success: false,
                    output: String::new(),
                    error: Some(format!(
                        "{reason}. \
                         HINT: This skill command was blocked by security policy. \
                         Check that the command does not contain disallowed operators \
                         or forbidden paths. Do NOT retry the same blocked command."
                    )),
                });
            }
        }

        if let Some(path) = self.security.forbidden_path_argument(&command) {
            return Ok(ToolResult {
                success: false,
                output: String::new(),
                error: Some(format!("Path blocked by security policy: {path}")),
            });
        }

        if !self.security.record_action() {
            return Ok(ToolResult {
                success: false,
                output: String::new(),
                error: Some("Rate limit exceeded: action budget exhausted".into()),
            });
        }

        // Build and execute the command
        let mut cmd = tokio::process::Command::new("sh");
        cmd.arg("-c").arg(&command);
        cmd.current_dir(&self.security.workspace_dir);
        cmd.env_clear();

        // Match the built-in shell tool's env policy so skill subprocesses
        // can access explicitly-allowlisted runtime settings (provider auth,
        // skill-proxy endpoints, session IDs, etc.) without inheriting the
        // full parent environment.
        for var in collect_allowed_shell_env_vars(&self.security) {
            if let Ok(val) = std::env::var(&var) {
                cmd.env(&var, val);
            }
        }

        #[cfg(feature = "one2x")]
        if let Ok(sid) = std::env::var("ZEROCLAW_SESSION_ID") {
            cmd.env("ZEROCLAW_SESSION_ID", &sid);
        }

        let result =
            tokio::time::timeout(Duration::from_secs(self.timeout_secs), cmd.output()).await;

        match result {
            Ok(Ok(output)) => {
                let mut stdout = String::from_utf8_lossy(&output.stdout).to_string();
                let mut stderr = String::from_utf8_lossy(&output.stderr).to_string();

                if stdout.len() > MAX_OUTPUT_BYTES {
                    let mut b = MAX_OUTPUT_BYTES.min(stdout.len());
                    while b > 0 && !stdout.is_char_boundary(b) {
                        b -= 1;
                    }
                    stdout.truncate(b);
                    stdout.push_str("\n... [output truncated at 1MB]");
                }
                if stderr.len() > MAX_OUTPUT_BYTES {
                    let mut b = MAX_OUTPUT_BYTES.min(stderr.len());
                    while b > 0 && !stderr.is_char_boundary(b) {
                        b -= 1;
                    }
                    stderr.truncate(b);
                    stderr.push_str("\n... [stderr truncated at 1MB]");
                }

                Ok(ToolResult {
                    success: output.status.success(),
                    output: stdout,
                    error: if stderr.is_empty() {
                        None
                    } else {
                        Some(stderr)
                    },
                })
            }
            Ok(Err(e)) => Ok(ToolResult {
                success: false,
                output: String::new(),
                error: Some(format!("Failed to execute command: {e}")),
            }),
            Err(_) => Ok(ToolResult {
                success: false,
                output: String::new(),
                error: Some(format!(
                    "Command timed out after {}s and was killed",
                    self.timeout_secs
                )),
            }),
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::security::{AutonomyLevel, SecurityPolicy};
    use crate::skills::SkillTool;

    fn test_security() -> Arc<SecurityPolicy> {
        Arc::new(SecurityPolicy {
            autonomy: AutonomyLevel::Full,
            workspace_dir: std::env::temp_dir(),
            ..SecurityPolicy::default()
        })
    }

    fn test_security_with_env_passthrough(vars: &[&str]) -> Arc<SecurityPolicy> {
        Arc::new(SecurityPolicy {
            autonomy: AutonomyLevel::Full,
            workspace_dir: std::env::temp_dir(),
            allowed_commands: vec!["env".into()],
            shell_env_passthrough: vars.iter().map(|v| (*v).to_string()).collect(),
            ..SecurityPolicy::default()
        })
    }

    struct EnvGuard {
        key: &'static str,
        original: Option<String>,
    }

    impl EnvGuard {
        fn set(key: &'static str, value: &str) -> Self {
            let original = std::env::var(key).ok();
            unsafe { std::env::set_var(key, value) };
            Self { key, original }
        }
    }

    impl Drop for EnvGuard {
        fn drop(&mut self) {
            match &self.original {
                Some(val) => unsafe { std::env::set_var(self.key, val) },
                None => unsafe { std::env::remove_var(self.key) },
            }
        }
    }

    fn sample_skill_tool() -> SkillTool {
        let mut args = HashMap::new();
        args.insert("file".to_string(), "The file to lint".to_string());
        args.insert(
            "format".to_string(),
            "Output format (json|text)".to_string(),
        );

        SkillTool {
            name: "run_lint".to_string(),
            description: "Run the linter on a file".to_string(),
            kind: "shell".to_string(),
            command: "lint --file {{file}} --format {{format}}".to_string(),
            args,
            timeout_secs: None,
        }
    }

    #[test]
    fn skill_shell_tool_name_is_prefixed() {
        let tool = SkillShellTool::new("my_skill", &sample_skill_tool(), test_security());
        assert_eq!(tool.name(), "my_skill__run_lint");
    }

    #[test]
    fn skill_shell_tool_description() {
        let tool = SkillShellTool::new("my_skill", &sample_skill_tool(), test_security());
        assert_eq!(tool.description(), "Run the linter on a file");
    }

    #[test]
    fn skill_shell_tool_parameters_schema() {
        let tool = SkillShellTool::new("my_skill", &sample_skill_tool(), test_security());
        let schema = tool.parameters_schema();

        assert_eq!(schema["type"], "object");
        assert!(schema["properties"]["file"].is_object());
        assert_eq!(schema["properties"]["file"]["type"], "string");
        assert!(schema["properties"]["format"].is_object());

        let required = schema["required"]
            .as_array()
            .expect("required should be array");
        assert_eq!(required.len(), 2);
    }

    #[test]
    fn skill_shell_tool_substitute_args() {
        let tool = SkillShellTool::new("my_skill", &sample_skill_tool(), test_security());
        let result = tool.substitute_args(&serde_json::json!({
            "file": "src/main.rs",
            "format": "json"
        }));
        assert_eq!(result, "lint --file 'src/main.rs' --format 'json'");
    }

    #[test]
    fn skill_shell_tool_substitute_missing_arg() {
        let tool = SkillShellTool::new("my_skill", &sample_skill_tool(), test_security());
        let result = tool.substitute_args(&serde_json::json!({"file": "test.rs"}));
        // Missing {{format}} placeholder stays in the command
        assert!(result.contains("{{format}}"));
        assert!(result.contains("test.rs"));
    }

    #[test]
    fn skill_shell_tool_substitute_args_inside_double_quotes() {
        let st = SkillTool {
            name: "echo_message".to_string(),
            description: "Echo a message".to_string(),
            kind: "shell".to_string(),
            command: "echo \"{{message}}\"".to_string(),
            args: HashMap::from([("message".to_string(), "Message".to_string())]),
            timeout_secs: None,
        };
        let tool = SkillShellTool::new("test", &st, test_security());
        let result = tool.substitute_args(&serde_json::json!({
            "message": "he said \"ship it\" & $HOME"
        }));
        assert_eq!(result, "echo \"he said \\\"ship it\\\" & \\$HOME\"");
    }

    #[test]
    fn skill_shell_tool_substitute_args_inside_single_quotes() {
        let st = SkillTool {
            name: "echo_message".to_string(),
            description: "Echo a message".to_string(),
            kind: "shell".to_string(),
            command: "echo '{{message}}'".to_string(),
            args: HashMap::from([("message".to_string(), "Message".to_string())]),
            timeout_secs: None,
        };
        let tool = SkillShellTool::new("test", &st, test_security());
        let result = tool.substitute_args(&serde_json::json!({
            "message": "it's safe & literal"
        }));
        assert_eq!(result, "echo 'it'\\''s safe & literal'");
    }

    #[test]
    fn skill_shell_tool_empty_args_schema() {
        let st = SkillTool {
            name: "simple".to_string(),
            description: "Simple tool".to_string(),
            kind: "shell".to_string(),
            command: "echo hello".to_string(),
            args: HashMap::new(),
            timeout_secs: None,
        };
        let tool = SkillShellTool::new("s", &st, test_security());
        let schema = tool.parameters_schema();
        assert_eq!(schema["type"], "object");
        assert!(schema["properties"].as_object().unwrap().is_empty());
        assert!(schema["required"].as_array().unwrap().is_empty());
    }

    #[tokio::test]
    async fn skill_shell_tool_executes_echo() {
        let st = SkillTool {
            name: "hello".to_string(),
            description: "Say hello".to_string(),
            kind: "shell".to_string(),
            command: "echo hello-skill".to_string(),
            args: HashMap::new(),
            timeout_secs: None,
        };
        let tool = SkillShellTool::new("test", &st, test_security());
        let result = tool.execute(serde_json::json!({})).await.unwrap();
        assert!(result.success);
        assert!(result.output.contains("hello-skill"));
    }

    #[tokio::test]
    async fn skill_shell_tool_executes_special_chars_inside_quoted_placeholder() {
        let st = SkillTool {
            name: "echo_message".to_string(),
            description: "Echo a message".to_string(),
            kind: "shell".to_string(),
            command: "echo \"{{message}}\"".to_string(),
            args: HashMap::from([("message".to_string(), "Message".to_string())]),
            timeout_secs: None,
        };
        let tool = SkillShellTool::new("test", &st, test_security());
        let result = tool
            .execute(serde_json::json!({
                "message": "Translate & Auto-Cut says \"ship it\""
            }))
            .await
            .unwrap();
        assert!(result.success);
        assert!(
            result
                .output
                .contains("Translate & Auto-Cut says \"ship it\"")
        );
    }

    #[tokio::test(flavor = "current_thread")]
    async fn skill_shell_tool_allows_configured_env_passthrough() {
        let _guard = EnvGuard::set("ZEROCLAW_TEST_PASSTHROUGH", "db://unit-test");
        let st = SkillTool {
            name: "print_env".to_string(),
            description: "Print env".to_string(),
            kind: "shell".to_string(),
            command: "env".to_string(),
            args: HashMap::new(),
            timeout_secs: None,
        };
        let tool = SkillShellTool::new(
            "test",
            &st,
            test_security_with_env_passthrough(&["ZEROCLAW_TEST_PASSTHROUGH"]),
        );
        let result = tool.execute(serde_json::json!({})).await.unwrap();
        assert!(result.success);
        assert!(
            result
                .output
                .contains("ZEROCLAW_TEST_PASSTHROUGH=db://unit-test")
        );
    }

    #[test]
    fn skill_shell_tool_spec_roundtrip() {
        let tool = SkillShellTool::new("my_skill", &sample_skill_tool(), test_security());
        let spec = tool.spec();
        assert_eq!(spec.name, "my_skill__run_lint");
        assert_eq!(spec.description, "Run the linter on a file");
        assert_eq!(spec.parameters["type"], "object");
    }

    #[tokio::test]
    async fn skill_shell_tool_respects_custom_timeout() {
        let st = SkillTool {
            name: "slow".to_string(),
            description: "Sleep briefly".to_string(),
            kind: "shell".to_string(),
            command: "sleep 2".to_string(),
            args: HashMap::new(),
            timeout_secs: Some(1),
        };

        let security = Arc::new(SecurityPolicy {
            autonomy: AutonomyLevel::Full,
            workspace_dir: std::env::temp_dir(),
            allowed_commands: vec!["sleep".into()],
            ..SecurityPolicy::default()
        });
        let tool = SkillShellTool::new("test", &st, security);
        let result = tool.execute(serde_json::json!({})).await.unwrap();

        assert!(!result.success);
        assert_eq!(
            result.error.as_deref(),
            Some("Command timed out after 1s and was killed")
        );
    }
}
