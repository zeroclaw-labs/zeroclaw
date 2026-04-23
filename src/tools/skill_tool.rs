//! Shell-based tool derived from a skill's `[[tools]]` section.
//!
//! Each `SkillTool` with `kind = "shell"` or `kind = "script"` is converted
//! into a `SkillShellTool` that implements the `Tool` trait. The tool name is
//! prefixed with the skill name (e.g. `my_skill.run_lint`) to avoid collisions
//! with built-in tools.

use super::shell::populate_shell_environment;
use super::traits::{Tool, ToolResult};
use crate::security::SecurityPolicy;
use async_trait::async_trait;
use std::collections::HashMap;
use std::path::PathBuf;
use std::sync::Arc;
use std::time::Duration;

/// Maximum execution time for a skill shell command (seconds).
const SKILL_SHELL_TIMEOUT_SECS: u64 = 60;
/// Maximum output size in bytes (1 MB).
const MAX_OUTPUT_BYTES: usize = 1_048_576;

/// A tool derived from a skill's `[[tools]]` section that executes shell commands.
pub struct SkillShellTool {
    tool_name: String,
    tool_description: String,
    command_template: String,
    args: HashMap<String, String>,
    working_dir: PathBuf,
    security: Arc<SecurityPolicy>,
}

impl SkillShellTool {
    /// Create a new skill shell tool.
    ///
    /// The tool name is prefixed with the skill name (`skill_name.tool_name`)
    /// to prevent collisions with built-in tools.
    pub fn new(
        skill_name: &str,
        tool: &crate::skills::SkillTool,
        security: Arc<SecurityPolicy>,
    ) -> Self {
        let working_dir = security.workspace_dir.clone();
        Self {
            tool_name: format!("{}.{}", skill_name, tool.name),
            tool_description: tool.description.clone(),
            command_template: tool.command.clone(),
            args: tool.args.clone(),
            working_dir,
            security,
        }
    }

    pub fn with_working_dir(mut self, working_dir: PathBuf) -> Self {
        self.working_dir = working_dir;
        self
    }

    fn resolve_working_dir(&self) -> Result<PathBuf, String> {
        let canonical_wd = self.working_dir.canonicalize().map_err(|_| {
            format!(
                "skill working directory '{}' does not exist or is not accessible",
                self.working_dir.display()
            )
        })?;

        if !canonical_wd.is_dir() {
            return Err(format!(
                "skill working directory '{}' is not a directory",
                canonical_wd.display()
            ));
        }

        if !self.security.is_resolved_path_allowed(&canonical_wd) {
            return Err(self.security.resolved_path_violation_message(&canonical_wd));
        }

        Ok(canonical_wd)
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
        let mut command = self.command_template.clone();
        if let Some(obj) = args.as_object() {
            for (key, value) in obj {
                let placeholder = format!("{{{{{}}}}}", key);
                let replacement = value.as_str().unwrap_or_default();
                command = command.replace(&placeholder, replacement);
            }
        }
        command
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
                    error: Some(reason),
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

        let working_dir = match self.resolve_working_dir() {
            Ok(path) => path,
            Err(reason) => {
                return Ok(ToolResult {
                    success: false,
                    output: String::new(),
                    error: Some(reason),
                });
            }
        };

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
        cmd.current_dir(&working_dir);
        populate_shell_environment(&mut cmd, &self.security);

        let result =
            tokio::time::timeout(Duration::from_secs(SKILL_SHELL_TIMEOUT_SECS), cmd.output()).await;

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
                    "Command timed out after {SKILL_SHELL_TIMEOUT_SECS}s and was killed"
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
    use tempfile::tempdir;

    fn test_security() -> Arc<SecurityPolicy> {
        Arc::new(SecurityPolicy {
            autonomy: AutonomyLevel::Full,
            workspace_dir: std::env::temp_dir(),
            ..SecurityPolicy::default()
        })
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
        }
    }

    #[test]
    fn skill_shell_tool_name_is_prefixed() {
        let tool = SkillShellTool::new("my_skill", &sample_skill_tool(), test_security());
        assert_eq!(tool.name(), "my_skill.run_lint");
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
        assert_eq!(result, "lint --file src/main.rs --format json");
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
    fn skill_shell_tool_empty_args_schema() {
        let st = SkillTool {
            name: "simple".to_string(),
            description: "Simple tool".to_string(),
            kind: "shell".to_string(),
            command: "echo hello".to_string(),
            args: HashMap::new(),
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
        };
        let tool = SkillShellTool::new("test", &st, test_security());
        let result = tool.execute(serde_json::json!({})).await.unwrap();
        assert!(result.success);
        assert!(result.output.contains("hello-skill"));
    }

    #[tokio::test]
    async fn skill_shell_tool_executes_relative_script_from_skill_dir() {
        let workspace = tempdir().unwrap();
        let skill_dir = workspace.path().join("skills").join("demo");
        let scripts_dir = skill_dir.join("scripts");
        std::fs::create_dir_all(&scripts_dir).unwrap();
        std::fs::write(
            scripts_dir.join("hello.py"),
            "print('relative-skill-script')\n",
        )
        .unwrap();

        let st = SkillTool {
            name: "hello".to_string(),
            description: "Say hello".to_string(),
            kind: "shell".to_string(),
            command: "python3 scripts/hello.py".to_string(),
            args: HashMap::new(),
        };
        let security = Arc::new(SecurityPolicy {
            autonomy: AutonomyLevel::Full,
            workspace_dir: workspace.path().to_path_buf(),
            ..SecurityPolicy::default()
        });
        let tool = SkillShellTool::new("test", &st, security).with_working_dir(skill_dir);
        let result = tool.execute(serde_json::json!({})).await.unwrap();

        assert!(result.success);
        assert!(result.output.contains("relative-skill-script"));
    }

    #[tokio::test]
    async fn skill_shell_tool_blocks_working_dir_outside_workspace_allowlist() {
        let workspace = tempdir().unwrap();
        let external = tempdir().unwrap();

        let st = SkillTool {
            name: "hello".to_string(),
            description: "Say hello".to_string(),
            kind: "shell".to_string(),
            command: "echo hello-skill".to_string(),
            args: HashMap::new(),
        };
        let security = Arc::new(SecurityPolicy {
            autonomy: AutonomyLevel::Full,
            workspace_dir: workspace.path().to_path_buf(),
            ..SecurityPolicy::default()
        });
        let tool = SkillShellTool::new("test", &st, security)
            .with_working_dir(external.path().to_path_buf());
        let result = tool.execute(serde_json::json!({})).await.unwrap();

        assert!(!result.success);
        assert!(
            result
                .error
                .as_deref()
                .unwrap_or("")
                .contains("Resolved path escapes workspace allowlist")
        );
    }

    #[tokio::test]
    async fn skill_shell_tool_allows_working_dir_in_allowed_root() {
        let workspace = tempdir().unwrap();
        let external = tempdir().unwrap();
        let skill_dir = external.path().join("demo");
        let scripts_dir = skill_dir.join("scripts");
        std::fs::create_dir_all(&scripts_dir).unwrap();
        std::fs::write(
            scripts_dir.join("hello.py"),
            "print('allowed-root-skill-script')\n",
        )
        .unwrap();

        let st = SkillTool {
            name: "hello".to_string(),
            description: "Say hello".to_string(),
            kind: "shell".to_string(),
            command: "python3 scripts/hello.py".to_string(),
            args: HashMap::new(),
        };
        let security = Arc::new(SecurityPolicy {
            autonomy: AutonomyLevel::Full,
            workspace_dir: workspace.path().to_path_buf(),
            allowed_roots: vec![external.path().canonicalize().unwrap()],
            ..SecurityPolicy::default()
        });
        let tool = SkillShellTool::new("test", &st, security).with_working_dir(skill_dir);
        let result = tool.execute(serde_json::json!({})).await.unwrap();

        assert!(result.success);
        assert!(result.output.contains("allowed-root-skill-script"));
    }

    #[tokio::test]
    async fn skill_shell_tool_passes_through_allowed_env_vars() {
        let workspace = tempdir().unwrap();
        let skill_dir = workspace.path().join("skills").join("demo");
        let scripts_dir = skill_dir.join("scripts");
        std::fs::create_dir_all(&scripts_dir).unwrap();
        std::fs::write(
            scripts_dir.join("print_env.py"),
            "import os\nprint(os.environ.get('ZEROCLAW_GATEWAY_TOKEN', ''), end='')\n",
        )
        .unwrap();

        let st = SkillTool {
            name: "env".to_string(),
            description: "Print env".to_string(),
            kind: "shell".to_string(),
            command: "python3 scripts/print_env.py".to_string(),
            args: HashMap::new(),
        };
        let original = std::env::var("ZEROCLAW_GATEWAY_TOKEN").ok();
        unsafe {
            std::env::set_var("ZEROCLAW_GATEWAY_TOKEN", "gateway-token-test");
        }
        let security = Arc::new(SecurityPolicy {
            autonomy: AutonomyLevel::Full,
            workspace_dir: workspace.path().to_path_buf(),
            shell_env_passthrough: vec!["ZEROCLAW_GATEWAY_TOKEN".into()],
            ..SecurityPolicy::default()
        });
        let tool = SkillShellTool::new("test", &st, security).with_working_dir(skill_dir);
        let result = tool.execute(serde_json::json!({})).await.unwrap();

        match original {
            Some(value) => unsafe {
                std::env::set_var("ZEROCLAW_GATEWAY_TOKEN", value);
            },
            None => unsafe {
                std::env::remove_var("ZEROCLAW_GATEWAY_TOKEN");
            },
        }

        assert!(result.success);
        assert_eq!(result.output, "gateway-token-test");
    }

    #[test]
    fn skill_shell_tool_spec_roundtrip() {
        let tool = SkillShellTool::new("my_skill", &sample_skill_tool(), test_security());
        let spec = tool.spec();
        assert_eq!(spec.name, "my_skill.run_lint");
        assert_eq!(spec.description, "Run the linter on a file");
        assert_eq!(spec.parameters["type"], "object");
    }
}
