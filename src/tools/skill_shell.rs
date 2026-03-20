use super::shell::{collect_allowed_shell_env_vars, MAX_OUTPUT_BYTES, SHELL_TIMEOUT_SECS};
use super::traits::{Tool, ToolResult, ToolSpec};
use crate::runtime::RuntimeAdapter;
use crate::security::SecurityPolicy;
use crate::skills::SkillTool;
use async_trait::async_trait;
use std::sync::Arc;
use std::time::Duration;

/// Sanitize a skill or tool name for use in namespaced function-calling identifiers.
///
/// Replaces hyphens and spaces with underscores so names are valid in all
/// LLM function-calling formats (OpenAI, Anthropic, etc.).
fn sanitize_name(s: &str) -> String {
    s.chars()
        .map(|ch| if ch == '-' || ch == ' ' { '_' } else { ch })
        .collect()
}

/// A Tool trait implementation backed by a skill's `[[tools]]` definition.
///
/// `SkillShellTool` bridges the gap between ZeroClaw's skill TOML metadata and
/// the LLM function-calling API: it wraps a [`SkillTool`] (parsed from
/// `SKILL.toml`) and executes the tool's `command` as a SecurityPolicy-validated
/// shell command, substituting LLM-provided arguments into `{placeholder}` slots.
///
/// # Name namespacing
///
/// The tool name exposed to the LLM is `{skill}__{tool}` (with hyphens replaced
/// by underscores in both parts), e.g. `knowledge_vault__vault_commit`. This
/// avoids collisions between tools from different skills.
///
/// # Security
///
/// Security enforcement is identical to [`super::shell::ShellTool`]:
/// - Rate limit check
/// - `validate_command_execution` (allowed-commands list, risk classification)
/// - Forbidden-path argument scan
/// - `record_action` budget
/// - Environment sanitization (`env_clear` + safe-var restoration)
/// - Timeout (`SHELL_TIMEOUT_SECS`)
pub struct SkillShellTool {
    /// Cached namespaced name (`{skill}__{tool}`) — stored so `fn name()` can
    /// return `&str` without allocating on every call.
    tool_name: String,
    /// Original skill name (unhyphenated form), kept for audit logging.
    skill_name: String,
    /// The parsed skill tool definition (name, description, kind, command, args).
    tool_def: SkillTool,
    /// Shared security policy reference injected at construction time.
    security: Arc<SecurityPolicy>,
    /// Platform-abstracted shell execution adapter.
    runtime: Arc<dyn RuntimeAdapter>,
}

impl SkillShellTool {
    /// Construct a new `SkillShellTool`.
    ///
    /// - `skill_name`: the skill's identifier (e.g. `"knowledge-vault"`).
    ///   Hyphens are replaced with underscores in the exposed tool name.
    /// - `tool_def`: the `[[tools]]` entry parsed from `SKILL.toml`.
    /// - `security`: shared security policy (rate limiting, allowed commands, etc.).
    /// - `runtime`: platform shell adapter (use [`crate::runtime::NativeRuntime`] in production).
    pub fn new(
        skill_name: &str,
        tool_def: SkillTool,
        security: Arc<SecurityPolicy>,
        runtime: Arc<dyn RuntimeAdapter>,
    ) -> Self {
        let tool_name = format!(
            "{}__{}",
            sanitize_name(skill_name),
            sanitize_name(&tool_def.name)
        );
        Self {
            tool_name,
            skill_name: skill_name.to_string(),
            tool_def,
            security,
            runtime,
        }
    }

    /// Substitute `{placeholder}` tokens in the command template with LLM-provided
    /// argument values.
    ///
    /// For each key in `args` that is a JSON object, the corresponding
    /// `{key}` token in the command string is replaced with the string value.
    /// No-arg tools (empty `args`) return the command unchanged.
    fn substitute_args(&self, args: &serde_json::Value) -> String {
        let mut command = self.tool_def.command.clone();
        if let Some(obj) = args.as_object() {
            for (key, val) in obj {
                let placeholder = format!("{{{}}}", key);
                let value = val.as_str().unwrap_or_default();
                command = command.replace(&placeholder, value);
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
        &self.tool_def.description
    }

    /// Build a JSON Schema `object` from the skill tool's declared arguments.
    ///
    /// Each entry in `tool_def.args` (`HashMap<String, String>`) maps
    /// `arg_name -> description` and becomes a `string`-typed schema property.
    /// All declared args are marked required.
    ///
    /// No-arg tools return `{"type":"object","properties":{}}` with no
    /// `required` field, matching the convention used by the built-in tools.
    fn parameters_schema(&self) -> serde_json::Value {
        let mut properties = serde_json::Map::new();
        let mut required: Vec<serde_json::Value> = Vec::new();

        // Iterate in sorted order so the schema is deterministic.
        let mut args: Vec<(&String, &String)> = self.tool_def.args.iter().collect();
        args.sort_by_key(|(k, _)| k.as_str());

        for (arg_name, arg_desc) in args {
            properties.insert(
                arg_name.clone(),
                serde_json::json!({
                    "type": "string",
                    "description": arg_desc,
                }),
            );
            required.push(serde_json::Value::String(arg_name.clone()));
        }

        if required.is_empty() {
            serde_json::json!({
                "type": "object",
                "properties": properties,
            })
        } else {
            serde_json::json!({
                "type": "object",
                "properties": properties,
                "required": required,
            })
        }
    }

    /// Execute the skill tool command with full SecurityPolicy enforcement.
    ///
    /// The execution sequence mirrors [`super::shell::ShellTool::execute`] exactly:
    /// 1. Rate-limit check
    /// 2. `validate_command_execution` (allowed-commands list, risk classification)
    /// 3. Forbidden-path argument scan
    /// 4. `record_action` budget
    /// 5. Build command via runtime adapter
    /// 6. `env_clear` + safe env-var restoration
    /// 7. `tokio::time::timeout` with `SHELL_TIMEOUT_SECS`
    /// 8. UTF-8 lossy conversion + `MAX_OUTPUT_BYTES` truncation
    async fn execute(&self, args: serde_json::Value) -> anyhow::Result<ToolResult> {
        let command = self.substitute_args(&args);
        let approved = args
            .get("approved")
            .and_then(|v| v.as_bool())
            .unwrap_or(false);

        tracing::debug!(
            skill = %self.skill_name,
            tool  = %self.tool_name,
            "Executing skill tool"
        );

        // 1. Rate-limit check
        if self.security.is_rate_limited() {
            return Ok(ToolResult {
                success: false,
                output: String::new(),
                error: Some("Rate limit exceeded: too many actions in the last hour".into()),
            });
        }

        // 2. Validate command execution (allowed-commands list, risk classification)
        match self.security.validate_command_execution(&command, approved) {
            Ok(_) => {}
            Err(reason) => {
                return Ok(ToolResult {
                    success: false,
                    output: String::new(),
                    error: Some(reason),
                });
            }
        }

        // 3. Forbidden-path argument scan
        if let Some(path) = self.security.forbidden_path_argument(&command) {
            return Ok(ToolResult {
                success: false,
                output: String::new(),
                error: Some(format!("Path blocked by security policy: {path}")),
            });
        }

        // 4. Record action / budget check
        if !self.security.record_action() {
            return Ok(ToolResult {
                success: false,
                output: String::new(),
                error: Some("Rate limit exceeded: action budget exhausted".into()),
            });
        }

        // 5. Build command via runtime adapter
        let mut cmd = match self
            .runtime
            .build_shell_command(&command, &self.security.workspace_dir)
        {
            Ok(cmd) => cmd,
            Err(e) => {
                return Ok(ToolResult {
                    success: false,
                    output: String::new(),
                    error: Some(format!("Failed to build runtime command: {e}")),
                });
            }
        };

        // 6. env_clear + safe env-var restoration (CWE-200: prevent API key leakage)
        cmd.env_clear();
        for var in collect_allowed_shell_env_vars(&self.security) {
            if let Ok(val) = std::env::var(&var) {
                cmd.env(&var, val);
            }
        }

        // 7. Execute with timeout
        let result =
            tokio::time::timeout(Duration::from_secs(SHELL_TIMEOUT_SECS), cmd.output()).await;

        // 8. Collect output with UTF-8 lossy conversion and truncation
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
                    "Command timed out after {SHELL_TIMEOUT_SECS}s and was killed"
                )),
            }),
        }
    }

    fn spec(&self) -> ToolSpec {
        ToolSpec {
            name: self.name().to_string(),
            description: self.description().to_string(),
            parameters: self.parameters_schema(),
        }
    }
}

// ── Unit tests ────────────────────────────────────────────────────────────────
//
// Privacy compliance (ZeroClaw PR discipline):
//   - Use `zeroclaw_user`, `zeroclaw_project`, `/tmp/zeroclaw_test` in fixtures.
//   - Never use real names, real vault paths, or personal data.
//   - Commit messages in tests: "zeroclaw: test commit".
#[cfg(test)]
mod tests {
    use super::*;
    use crate::runtime::NativeRuntime;
    use crate::security::{AutonomyLevel, SecurityPolicy};
    use crate::tools::shell::SAFE_ENV_VARS;
    use std::collections::HashMap;

    // ── Helpers ───────────────────────────────────────────────────────────────

    fn make_tool_def(name: &str, command: &str, args: HashMap<String, String>) -> SkillTool {
        SkillTool {
            name: name.to_string(),
            description: format!("Test tool: {name}"),
            kind: "shell".to_string(),
            command: command.to_string(),
            args,
        }
    }

    fn full_security() -> Arc<SecurityPolicy> {
        Arc::new(SecurityPolicy {
            autonomy: AutonomyLevel::Full,
            workspace_dir: std::env::temp_dir(),
            ..SecurityPolicy::default()
        })
    }

    fn supervised_security_with_cmds(cmds: Vec<String>) -> Arc<SecurityPolicy> {
        Arc::new(SecurityPolicy {
            autonomy: AutonomyLevel::Supervised,
            workspace_dir: std::env::temp_dir(),
            allowed_commands: cmds,
            ..SecurityPolicy::default()
        })
    }

    fn test_runtime() -> Arc<dyn RuntimeAdapter> {
        Arc::new(NativeRuntime::new())
    }

    // ── Name construction ─────────────────────────────────────────────────────

    #[test]
    fn name_is_namespaced_with_double_underscore() {
        let tool_def = make_tool_def("vault-commit", "git commit", HashMap::new());
        let tool =
            SkillShellTool::new("knowledge-vault", tool_def, full_security(), test_runtime());
        assert_eq!(tool.name(), "knowledge_vault__vault_commit");
    }

    #[test]
    fn name_replaces_hyphens_in_both_parts() {
        let tool_def = make_tool_def("my-tool", "echo hi", HashMap::new());
        let tool = SkillShellTool::new("my-skill", tool_def, full_security(), test_runtime());
        assert_eq!(tool.name(), "my_skill__my_tool");
    }

    #[test]
    fn name_passthrough_for_names_without_hyphens() {
        let tool_def = make_tool_def("run", "echo run", HashMap::new());
        let tool = SkillShellTool::new("vault", tool_def, full_security(), test_runtime());
        assert_eq!(tool.name(), "vault__run");
    }

    // ── Schema generation ─────────────────────────────────────────────────────

    #[test]
    fn schema_for_no_arg_tool_has_empty_properties_and_no_required() {
        let tool_def = make_tool_def("no-args", "git status", HashMap::new());
        let tool = SkillShellTool::new(
            "zeroclaw_project",
            tool_def,
            full_security(),
            test_runtime(),
        );
        let schema = tool.parameters_schema();

        assert_eq!(schema["type"], "object");
        assert!(schema["properties"].is_object());
        assert_eq!(
            schema["properties"]
                .as_object()
                .map(|m| m.len())
                .unwrap_or(1),
            0,
            "properties must be empty for no-arg tool"
        );
        assert!(
            schema.get("required").is_none(),
            "no-arg tool must not have a 'required' field"
        );
    }

    #[test]
    fn schema_for_tool_with_args_includes_properties_and_required() {
        let mut args = HashMap::new();
        args.insert("message".to_string(), "Commit message".to_string());
        let tool_def = make_tool_def("vault-commit", "git commit -m {message}", args);
        let tool =
            SkillShellTool::new("knowledge-vault", tool_def, full_security(), test_runtime());
        let schema = tool.parameters_schema();

        assert_eq!(schema["type"], "object");
        assert!(schema["properties"]["message"].is_object());
        assert_eq!(schema["properties"]["message"]["type"], "string");
        assert_eq!(
            schema["properties"]["message"]["description"],
            "Commit message"
        );
        let required = schema["required"]
            .as_array()
            .expect("required must be array");
        assert!(required.contains(&serde_json::json!("message")));
    }

    #[test]
    fn schema_multi_arg_all_required() {
        let mut args = HashMap::new();
        args.insert("path".to_string(), "Target path".to_string());
        args.insert("message".to_string(), "Commit message".to_string());
        let tool_def = make_tool_def("commit", "git -C {path} commit -m {message}", args);
        let tool = SkillShellTool::new(
            "zeroclaw_project",
            tool_def,
            full_security(),
            test_runtime(),
        );
        let schema = tool.parameters_schema();

        let required = schema["required"]
            .as_array()
            .expect("required must be array");
        assert_eq!(required.len(), 2);
        assert!(required.contains(&serde_json::json!("path")));
        assert!(required.contains(&serde_json::json!("message")));
    }

    // ── Arg substitution ──────────────────────────────────────────────────────

    #[test]
    fn substitute_args_replaces_placeholders() {
        let tool_def = make_tool_def(
            "commit",
            "git -C {path} commit -m {message}",
            HashMap::new(),
        );
        let tool = SkillShellTool::new(
            "zeroclaw_project",
            tool_def,
            full_security(),
            test_runtime(),
        );
        let result = tool.substitute_args(&serde_json::json!({
            "path": "/tmp/zeroclaw_test",
            "message": "zeroclaw: test commit"
        }));
        assert_eq!(
            result,
            "git -C /tmp/zeroclaw_test commit -m zeroclaw: test commit"
        );
    }

    #[test]
    fn substitute_args_no_arg_passthrough() {
        let tool_def = make_tool_def("add", "git -C /tmp/zeroclaw_test add -A", HashMap::new());
        let tool = SkillShellTool::new(
            "zeroclaw_project",
            tool_def,
            full_security(),
            test_runtime(),
        );
        let result = tool.substitute_args(&serde_json::json!({}));
        assert_eq!(result, "git -C /tmp/zeroclaw_test add -A");
    }

    #[test]
    fn substitute_args_partial_substitution() {
        let tool_def = make_tool_def("echo", "echo {msg} done", HashMap::new());
        let tool = SkillShellTool::new(
            "zeroclaw_project",
            tool_def,
            full_security(),
            test_runtime(),
        );
        let result = tool.substitute_args(&serde_json::json!({"msg": "hello"}));
        assert_eq!(result, "echo hello done");
    }

    // ── Security blocking ─────────────────────────────────────────────────────

    #[tokio::test]
    async fn execute_blocks_command_not_in_allowed_list() {
        // Supervised autonomy with an empty allowed list — every command is denied.
        let security = supervised_security_with_cmds(vec![]);
        let tool_def = make_tool_def("run", "rm -rf /tmp/zeroclaw_test", HashMap::new());
        let tool = SkillShellTool::new("zeroclaw_project", tool_def, security, test_runtime());
        let result = tool
            .execute(serde_json::json!({}))
            .await
            .expect("execute must not return Err for a security-blocked command");
        assert!(!result.success);
        assert!(
            result.error.is_some(),
            "error field must be present when command is blocked"
        );
    }

    #[tokio::test]
    async fn execute_allowed_echo_command_succeeds() {
        let tool_def = make_tool_def("greet", "echo zeroclaw_test_output", HashMap::new());
        let tool = SkillShellTool::new(
            "zeroclaw_project",
            tool_def,
            full_security(),
            test_runtime(),
        );
        let result = tool
            .execute(serde_json::json!({}))
            .await
            .expect("allowed echo command must not return Err");
        assert!(result.success, "echo should succeed with Full autonomy");
        assert!(
            result.output.trim().contains("zeroclaw_test_output"),
            "output must contain the echo'd text"
        );
    }

    // ── spec() helper ─────────────────────────────────────────────────────────

    #[test]
    fn spec_returns_namespaced_name_and_correct_schema() {
        let mut args = HashMap::new();
        args.insert("msg".to_string(), "The message".to_string());
        let tool_def = make_tool_def("vault-commit", "git commit -m {msg}", args);
        let tool =
            SkillShellTool::new("knowledge-vault", tool_def, full_security(), test_runtime());
        let spec = tool.spec();

        assert_eq!(spec.name, "knowledge_vault__vault_commit");
        assert!(!spec.description.is_empty());
        assert!(spec.parameters["properties"]["msg"].is_object());
    }

    // ── description passthrough ───────────────────────────────────────────────

    #[test]
    fn description_comes_from_tool_def() {
        let mut tool_def = make_tool_def("run", "echo hi", HashMap::new());
        tool_def.description = "Runs the zeroclaw_project pipeline".to_string();
        let tool = SkillShellTool::new(
            "zeroclaw_project",
            tool_def,
            full_security(),
            test_runtime(),
        );
        assert_eq!(tool.description(), "Runs the zeroclaw_project pipeline");
    }

    // ── SAFE_ENV_VARS sanity (imported from shell module) ────────────────────

    #[test]
    fn safe_env_vars_includes_path_and_home() {
        assert!(SAFE_ENV_VARS.contains(&"PATH"));
        #[cfg(not(target_os = "windows"))]
        assert!(SAFE_ENV_VARS.contains(&"HOME"));
        #[cfg(target_os = "windows")]
        assert!(SAFE_ENV_VARS.contains(&"USERPROFILE") || SAFE_ENV_VARS.contains(&"HOME"));
    }
}
