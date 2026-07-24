//! Shell-based tool derived from a skill's `[[tools]]` section.

use crate::platform::{NativeRuntime, RuntimeAdapter};
use crate::security::SecurityPolicy;
use async_trait::async_trait;
use std::collections::HashMap;
use std::sync::Arc;
use std::time::Duration;
use zeroclaw_api::tool::{Tool, ToolOutput, ToolResult};

/// Default execution time for a skill shell command when the manifest does not
/// set `timeout_secs` (seconds). A skill may raise this via `timeout_secs` in
/// its SKILL.toml `[[tools]]` entry.
const SKILL_SHELL_TIMEOUT_SECS: u64 = 60;
/// Maximum output size in bytes (1 MB).
const MAX_OUTPUT_BYTES: usize = 1_048_576;

#[cfg(not(target_os = "windows"))]
const SAFE_ENV_VARS: &[&str] = &[
    "PATH", "HOME", "TERM", "LANG", "LC_ALL", "LC_CTYPE", "USER", "SHELL", "TMPDIR",
];

#[cfg(target_os = "windows")]
const SAFE_ENV_VARS: &[&str] = &[
    "PATH",
    "PATHEXT",
    "HOME",
    "USERPROFILE",
    "HOMEDRIVE",
    "HOMEPATH",
    "SYSTEMROOT",
    "SYSTEMDRIVE",
    "WINDIR",
    "COMSPEC",
    "TEMP",
    "TMP",
    "TERM",
    "LANG",
    "USERNAME",
];

const MAX_TOOL_NAME_LEN: usize = 64;

fn is_name_char(c: char) -> bool {
    c.is_ascii_alphanumeric() || c == '_' || c == '-'
}

/// Dependency-free, build-stable 64-bit FNV-1a hash, rendered as 16 hex chars.
/// Used only to disambiguate names that had to be altered. A bounded 64-bit
/// hash reduces accidental collisions among sanitized names; it is not a
/// uniqueness proof and cannot make the mapping injective.
fn short_hash(s: &str) -> String {
    let mut h: u64 = 0xcbf2_9ce4_8422_2325;
    for b in s.bytes() {
        h ^= b as u64;
        h = h.wrapping_mul(0x0000_0100_0000_01b3);
    }
    format!("{h:016x}")
}

pub(crate) fn composed_tool_name(skill_name: &str, tool_name: &str) -> String {
    sanitize_tool_name(&format!("{skill_name}__{tool_name}"))
}

fn sanitize_tool_name(raw: &str) -> String {
    let already_valid =
        !raw.is_empty() && raw.len() <= MAX_TOOL_NAME_LEN && raw.chars().all(is_name_char);
    if already_valid {
        return raw.to_string();
    }

    let mapped: String = raw
        .chars()
        .map(|c| if is_name_char(c) { c } else { '_' })
        .collect();

    // Reserve room for `_<16 hex>` so the disambiguating hash always survives.
    let suffix = format!("_{}", short_hash(raw));
    let budget = MAX_TOOL_NAME_LEN - suffix.len();
    let head: String = mapped.chars().take(budget).collect();
    format!("{head}{suffix}")
}

/// Name of the environment variable that carries the in-flight session key
/// into skill shell tools.
const SESSION_ID_ENV_VAR: &str = "ZEROCLAW_SESSION_ID";

/// The session key for the current turn, or `None` when the turn is unscoped
/// (one-shot / webhook). Empty keys are treated as absent.
fn get_session_id() -> Option<String> {
    zeroclaw_api::TOOL_LOOP_SESSION_KEY
        .try_with(Clone::clone)
        .ok()
        .flatten()
        .filter(|key| !key.is_empty())
}

/// A tool derived from a skill's `[[tools]]` section that executes shell commands.
pub struct SkillShellTool {
    tool_name: String,
    tool_description: String,
    command_template: String,
    args: HashMap<String, String>,
    security: Arc<SecurityPolicy>,
    runtime: Arc<dyn RuntimeAdapter>,
    /// Resolved per-command timeout in seconds (manifest `timeout_secs`, or the
    /// `SKILL_SHELL_TIMEOUT_SECS` default), clamped to a minimum of 1.
    timeout_secs: u64,
}

impl SkillShellTool {
    /// Create a new skill shell tool.
    /// The tool name is prefixed with the skill name (`skill_name__tool_name`)
    /// to prevent collisions with built-in tools.
    pub fn new(
        skill_name: &str,
        tool: &crate::skills::SkillTool,
        security: Arc<SecurityPolicy>,
    ) -> Self {
        Self::new_with_runtime(skill_name, tool, security, Arc::new(NativeRuntime::new()))
    }

    pub fn new_with_runtime(
        skill_name: &str,
        tool: &crate::skills::SkillTool,
        security: Arc<SecurityPolicy>,
        runtime: Arc<dyn RuntimeAdapter>,
    ) -> Self {
        Self {
            tool_name: composed_tool_name(skill_name, &tool.name),
            tool_description: tool.description.clone(),
            command_template: tool.command.clone(),
            args: tool.args.clone(),
            security,
            runtime,
            timeout_secs: tool.timeout_secs.unwrap_or(SKILL_SHELL_TIMEOUT_SECS).max(1),
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

        // Security validation — always requires explicit approval (approved=true)
        // since skill tools are user-defined and should be treated as medium-risk.
        match self.security.validate_command_execution(&command, true) {
            Ok(_) => {}
            Err(reason) => {
                return Ok(ToolResult {
                    success: false,
                    output: ToolOutput::default(),
                    error: Some(reason),
                });
            }
        }

        if let Some(path) = self.security.forbidden_path_argument(&command) {
            return Ok(ToolResult {
                success: false,
                output: ToolOutput::default(),
                error: Some(format!("Path blocked by security policy: {path}")),
            });
        }

        let mut cmd = match self
            .runtime
            .build_shell_command(&command, &self.security.workspace_dir)
        {
            Ok(cmd) => cmd,
            Err(e) => {
                return Ok(ToolResult {
                    success: false,
                    output: ToolOutput::default(),
                    error: Some(format!("Failed to build runtime command: {e}")),
                });
            }
        };
        cmd.env_clear();

        // Only pass safe environment variables
        for var in SAFE_ENV_VARS {
            if let Ok(val) = std::env::var(var) {
                cmd.env(var, val);
            }
        }

        // Injected after env_clear so it survives; absent when the turn is unscoped.
        if let Some(session_id) = get_session_id() {
            cmd.env(SESSION_ID_ENV_VAR, session_id);
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
                    output: stdout.into(),
                    error: if stderr.is_empty() {
                        None
                    } else {
                        Some(stderr)
                    },
                })
            }
            Ok(Err(e)) => Ok(ToolResult {
                success: false,
                output: ToolOutput::default(),
                error: Some(format!("Failed to execute command: {e}")),
            }),
            Err(_) => Ok(ToolResult {
                success: false,
                output: ToolOutput::default(),
                error: Some(format!(
                    "Command timed out after {}s and was killed",
                    self.timeout_secs
                )),
            }),
        }
    }
}

// ─── Builtin / MCP delegation tool ───────────────────────────────────────────

pub struct SkillBuiltinTool {
    tool_name: String,
    tool_description: String,
    target_tool: Arc<dyn zeroclaw_api::tool::Tool>,
    locked_args: serde_json::Map<String, serde_json::Value>,
    /// Target schema with the locked keys removed (precomputed at
    /// construction). `Arc`-shared so per-iteration spec assembly hands out
    /// reference counts instead of deep-cloning the (possibly MCP-derived,
    /// tens-of-KB) tree — see the invariant on `Tool::spec`.
    advertised_schema: Arc<serde_json::Value>,
}

impl SkillBuiltinTool {
    pub fn new(
        skill_name: &str,
        tool: &crate::skills::SkillTool,
        target_tool: Arc<dyn zeroclaw_api::tool::Tool>,
        locked_args: HashMap<String, String>,
    ) -> Self {
        let locked: serde_json::Map<String, serde_json::Value> = locked_args
            .into_iter()
            .map(|(k, v)| (k, serde_json::Value::String(v)))
            .collect();
        let advertised_schema = Arc::new(narrow_schema(target_tool.parameters_schema(), &locked));
        Self {
            tool_name: composed_tool_name(skill_name, &tool.name),
            tool_description: tool.description.clone(),
            target_tool,
            locked_args: locked,
            advertised_schema,
        }
    }
}

/// Merge caller args with manifest `locked` args. Locked args ALWAYS win — the
/// caller cannot override a scope key — but the caller may add other keys.
fn merge_locked_args(
    locked: &serde_json::Map<String, serde_json::Value>,
    caller: serde_json::Value,
) -> serde_json::Value {
    if locked.is_empty() {
        return caller;
    }
    let mut merged = match caller {
        serde_json::Value::Object(map) => map,
        _ => serde_json::Map::new(),
    };
    for (k, v) in locked {
        merged.insert(k.clone(), v.clone());
    }
    serde_json::Value::Object(merged)
}

/// Remove `locked` keys from an advertised JSON-schema object so the model
/// neither sees nor tries to set keys the manifest fixes. Non-object schemas
/// (or those without `properties`) pass through unchanged.
fn narrow_schema(
    schema: serde_json::Value,
    locked: &serde_json::Map<String, serde_json::Value>,
) -> serde_json::Value {
    if locked.is_empty() {
        return schema;
    }
    let serde_json::Value::Object(mut obj) = schema else {
        return schema;
    };
    if let Some(serde_json::Value::Object(props)) = obj.get_mut("properties") {
        for k in locked.keys() {
            props.remove(k);
        }
    }
    if let Some(serde_json::Value::Array(required)) = obj.get_mut("required") {
        required.retain(|v| v.as_str().is_none_or(|s| !locked.contains_key(s)));
    }
    serde_json::Value::Object(obj)
}

#[async_trait]
impl Tool for SkillBuiltinTool {
    fn name(&self) -> &str {
        &self.tool_name
    }

    fn description(&self) -> &str {
        &self.tool_description
    }

    fn parameters_schema(&self) -> serde_json::Value {
        (*self.advertised_schema).clone()
    }

    // Hand out the stored schema by `Arc::clone` instead of the trait
    // default's per-call deep clone — specs are rebuilt every agent-loop
    // iteration and elevated skill tools can front MCP-derived schemas.
    fn spec(&self) -> zeroclaw_api::tool::ToolSpec {
        zeroclaw_api::tool::ToolSpec {
            name: self.tool_name.clone(),
            description: self.tool_description.clone(),
            parameters: Arc::clone(&self.advertised_schema),
            output: None,
            param_domains: std::collections::BTreeMap::new(),
        }
    }

    async fn execute(&self, args: serde_json::Value) -> anyhow::Result<ToolResult> {
        // Audit: elevated skill tools delegate to a target that may be blocked
        // by SecurityPolicy or hidden from the model. Record every invocation
        // at INFO with the delegation target and the locked scope keys.
        ::zeroclaw_log::record!(
            INFO,
            ::zeroclaw_log::Event::new(module_path!(), ::zeroclaw_log::Action::Invoke)
                .with_category(::zeroclaw_log::EventCategory::Tool)
                .with_attrs(::serde_json::json!({
                    "skill_tool": self.tool_name,
                    "delegates_to": self.target_tool.name(),
                    "locked_keys": self.locked_args.keys().collect::<Vec<_>>(),
                })),
            "skill-scoped elevated tool invoked"
        );
        let merged = merge_locked_args(&self.locked_args, args);
        self.target_tool.execute(merged).await
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

    #[tokio::test]
    async fn get_session_id_returns_scoped_session_key() {
        let got = crate::agent::loop_::scope_session_key(Some("gw_abc-123".to_string()), async {
            get_session_id()
        })
        .await;
        assert_eq!(got, Some("gw_abc-123".to_string()));
    }

    #[test]
    fn get_session_id_none_outside_a_scoped_turn() {
        assert_eq!(get_session_id(), None);
    }

    #[tokio::test]
    async fn get_session_id_none_for_empty_session_key() {
        let got =
            crate::agent::loop_::scope_session_key(Some(String::new()), async { get_session_id() })
                .await;
        assert_eq!(got, None);
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
            target: None,
            locked_args: HashMap::new(),
            timeout_secs: None,
        }
    }

    #[test]
    fn skill_shell_tool_name_is_prefixed() {
        let tool = SkillShellTool::new("my_skill", &sample_skill_tool(), test_security());
        assert_eq!(tool.name(), "my_skill__run_lint");
    }

    fn name_is_provider_valid(name: &str) -> bool {
        !name.is_empty()
            && name.len() <= 64
            && name
                .chars()
                .all(|c| c.is_ascii_alphanumeric() || c == '_' || c == '-')
    }

    #[test]
    fn skill_tool_name_sanitized_for_provider_regex() {
        // Plugin-namespaced skill names (colons), dotted names, spaces, and
        // non-ASCII must all yield a provider-valid function name
        for (skill, tool_name) in [
            ("pr-review-toolkit:code-reviewer", "run.lint"),
            ("my skill", "do thing"),
            ("skill.with.dots", "tool"),
            ("ünïcode", "naïve"),
        ] {
            let mut st = sample_skill_tool();
            st.name = tool_name.to_string();
            let tool = SkillShellTool::new(skill, &st, test_security());
            assert!(
                name_is_provider_valid(tool.name()),
                "illegal tool name `{}` from skill `{}`",
                tool.name(),
                skill
            );
        }
    }

    fn shell_tool_name(skill: &str, tool_name: &str) -> String {
        let mut st = sample_skill_tool();
        st.name = tool_name.to_string();
        SkillShellTool::new(skill, &st, test_security())
            .name()
            .to_string()
    }

    #[test]
    fn skill_tool_already_valid_name_is_unchanged() {
        // The common case must not be perturbed (no spurious hash suffix).
        let tool = SkillShellTool::new("my_skill", &sample_skill_tool(), test_security());
        assert_eq!(tool.name(), "my_skill__run_lint");
    }

    #[test]
    fn skill_tool_name_truncated_to_64_and_stays_distinct() {
        // A raw composed name over 64 chars must be sanitized to <= 64 while
        // two distinct tools under the same long skill name stay distinct, i.e.
        // truncation must not collapse them Anthropic's contract is
        // `^[a-zA-Z0-9_-]{1,64}$`, so 64 is the bound, not 128.
        let long = "a".repeat(200);
        let a = shell_tool_name(&long, "alpha");
        let b = shell_tool_name(&long, "beta");
        assert!(
            a.len() <= 64 && b.len() <= 64,
            "sanitized names exceed the 64-char provider bound: {} / {}",
            a.len(),
            b.len()
        );
        assert!(name_is_provider_valid(&a) && name_is_provider_valid(&b));
        assert_ne!(a, b, "distinct tools under a long skill name collided");
    }

    #[test]
    fn skill_tool_name_sanitization_disambiguates_common_collisions() {
        // Inputs differing only by illegal characters must not collide.
        let a = shell_tool_name("a.b", "run");
        let b = shell_tool_name("a:b", "run");
        assert!(name_is_provider_valid(&a) && name_is_provider_valid(&b));
        assert_ne!(a, b, "illegal-char variants collapsed to the same name");
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
            target: None,
            locked_args: HashMap::new(),
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
            target: None,
            locked_args: HashMap::new(),
            timeout_secs: None,
        };
        let tool = SkillShellTool::new("test", &st, test_security());
        let result = tool.execute(serde_json::json!({})).await.unwrap();
        assert!(result.success);
        assert!(result.output.contains("hello-skill"));
    }

    #[test]
    fn skill_shell_tool_uses_default_timeout_when_unset() {
        // `timeout_secs = None` in the manifest falls back to the 60s default.
        let tool = SkillShellTool::new("my_skill", &sample_skill_tool(), test_security());
        assert_eq!(tool.timeout_secs, SKILL_SHELL_TIMEOUT_SECS);
    }

    #[test]
    fn skill_shell_tool_honors_manifest_timeout() {
        // A manifest `timeout_secs` overrides the default — the fix for
        // long-running skills that were killed at the 60s default.
        let mut st = sample_skill_tool();
        st.timeout_secs = Some(3600);
        let tool = SkillShellTool::new("my_skill", &st, test_security());
        assert_eq!(tool.timeout_secs, 3600);
    }

    #[test]
    fn skill_shell_tool_clamps_zero_timeout_to_one() {
        // A zero timeout would fire instantly and kill every command; clamp it.
        let mut st = sample_skill_tool();
        st.timeout_secs = Some(0);
        let tool = SkillShellTool::new("my_skill", &st, test_security());
        assert_eq!(tool.timeout_secs, 1);
    }

    #[test]
    fn skill_tool_serde_parses_timeout_secs() {
        // The manifest field deserializes; absent it defaults to None.
        let with = r#"
            name = "deploy"
            description = "Deploy"
            kind = "shell"
            command = "deploy"
            timeout_secs = 3600
        "#;
        let st: SkillTool = toml::from_str(with).unwrap();
        assert_eq!(st.timeout_secs, Some(3600));

        let without = r#"
            name = "deploy"
            description = "Deploy"
            kind = "shell"
            command = "deploy"
        "#;
        let st: SkillTool = toml::from_str(without).unwrap();
        assert_eq!(st.timeout_secs, None);
    }

    #[test]
    fn skill_shell_tool_spec_roundtrip() {
        let tool = SkillShellTool::new("my_skill", &sample_skill_tool(), test_security());
        let spec = tool.spec();
        assert_eq!(spec.name, "my_skill__run_lint");
        assert_eq!(spec.description, "Run the linter on a file");
        assert_eq!(spec.parameters["type"], "object");
    }

    // ─── SkillBuiltinTool tests ──────────────────────────────────────────────

    /// Minimal mock tool for testing builtin delegation.
    struct MockBuiltinTool {
        name: String,
    }

    impl MockBuiltinTool {
        fn new(name: &str) -> Self {
            Self {
                name: name.to_string(),
            }
        }
    }

    impl ::zeroclaw_api::attribution::Attributable for MockBuiltinTool {
        fn role(&self) -> ::zeroclaw_api::attribution::Role {
            ::zeroclaw_api::attribution::Role::Tool(::zeroclaw_api::attribution::ToolKind::Plugin)
        }
        fn alias(&self) -> &str {
            &self.name
        }
    }

    #[async_trait]
    impl Tool for MockBuiltinTool {
        fn name(&self) -> &str {
            &self.name
        }
        fn description(&self) -> &str {
            "Mock builtin for testing"
        }
        fn parameters_schema(&self) -> serde_json::Value {
            serde_json::json!({
                "type": "object",
                "properties": {
                    "input": { "type": "string" }
                },
                "required": ["input"]
            })
        }
        async fn execute(&self, args: serde_json::Value) -> anyhow::Result<ToolResult> {
            let input = args.get("input").and_then(|v| v.as_str()).unwrap_or("none");
            Ok(ToolResult {
                success: true,
                output: format!("mock_result:{input}").into(),
                error: None,
            })
        }
    }

    fn sample_builtin_skill_tool() -> SkillTool {
        SkillTool {
            name: "use_shell".to_string(),
            description: "Elevated shell access via skill".to_string(),
            kind: "builtin".to_string(),
            command: String::new(),
            args: HashMap::new(),
            target: Some("shell".to_string()),
            locked_args: HashMap::new(),
            timeout_secs: None,
        }
    }

    #[test]
    fn skill_builtin_tool_name_is_prefixed() {
        let target: Arc<dyn Tool> = Arc::new(MockBuiltinTool::new("shell"));
        let tool = SkillBuiltinTool::new(
            "my_skill",
            &sample_builtin_skill_tool(),
            target,
            HashMap::new(),
        );
        assert_eq!(tool.name(), "my_skill__use_shell");
    }

    #[test]
    fn skill_builtin_tool_description() {
        let target: Arc<dyn Tool> = Arc::new(MockBuiltinTool::new("shell"));
        let tool = SkillBuiltinTool::new(
            "my_skill",
            &sample_builtin_skill_tool(),
            target,
            HashMap::new(),
        );
        assert_eq!(tool.description(), "Elevated shell access via skill");
    }

    #[test]
    fn skill_builtin_tool_inherits_target_schema() {
        let target: Arc<dyn Tool> = Arc::new(MockBuiltinTool::new("shell"));
        let tool = SkillBuiltinTool::new(
            "my_skill",
            &sample_builtin_skill_tool(),
            target,
            HashMap::new(),
        );
        let schema = tool.parameters_schema();
        // Schema should come from the mock target, not the skill tool definition
        assert_eq!(schema["type"], "object");
        assert!(schema["properties"]["input"].is_object());
    }

    #[tokio::test]
    async fn skill_builtin_tool_delegates_to_target() {
        let target: Arc<dyn Tool> = Arc::new(MockBuiltinTool::new("shell"));
        let tool = SkillBuiltinTool::new(
            "my_skill",
            &sample_builtin_skill_tool(),
            target,
            HashMap::new(),
        );
        let result = tool
            .execute(serde_json::json!({"input": "hello"}))
            .await
            .unwrap();
        assert!(result.success);
        assert_eq!(result.output, "mock_result:hello");
    }

    #[test]
    fn skill_builtin_tool_spec_roundtrip() {
        let target: Arc<dyn Tool> = Arc::new(MockBuiltinTool::new("shell"));
        let tool = SkillBuiltinTool::new(
            "my_skill",
            &sample_builtin_skill_tool(),
            target,
            HashMap::new(),
        );
        let spec = tool.spec();
        assert_eq!(spec.name, "my_skill__use_shell");
        assert_eq!(spec.description, "Elevated shell access via skill");
    }

    #[test]
    fn skill_tool_serde_new_fields_default() {
        // Verify that TOML without the new fields still parses correctly
        let toml_str = r#"
            name = "test"
            description = "A test tool"
            kind = "shell"
            command = "echo hello"
        "#;
        let st: SkillTool = toml::from_str(toml_str).unwrap();
        assert_eq!(st.name, "test");
        assert_eq!(st.kind, "shell");
        assert!(st.target.is_none());
    }

    #[test]
    fn skill_tool_serde_with_builtin_fields() {
        let toml_str = r#"
            name = "use_shell"
            description = "Shell via skill"
            kind = "builtin"
            target = "shell"
        "#;
        let st: SkillTool = toml::from_str(toml_str).unwrap();
        assert_eq!(st.kind, "builtin");
        assert_eq!(st.target.as_deref(), Some("shell"));
    }

    #[test]
    fn skill_tool_serde_legacy_default_args_aliases_to_locked_args() {
        // The legacy `[default_args]` key still parses into `locked_args`.
        let toml_str = r#"
            name = "generate_pdf"
            description = "Generate PDF via Composio"
            kind = "builtin"
            target = "composio"

            [default_args]
            action_name = "TEXT_TO_PDF"
            app = "pdfco"
        "#;
        let st: SkillTool = toml::from_str(toml_str).unwrap();
        assert_eq!(st.target.as_deref(), Some("composio"));
        assert_eq!(st.locked_args.get("action_name").unwrap(), "TEXT_TO_PDF");
        assert_eq!(st.locked_args.get("app").unwrap(), "pdfco");
    }

    #[test]
    fn skill_tool_serde_mcp_kind_with_locked_args() {
        // `kind = "mcp"` targets a prefixed MCP tool name `{server}__{tool}`.
        let toml_str = r#"
            name = "generate_image"
            description = "Generate an image via MCP"
            kind = "mcp"
            target = "images__generate"

            [locked_args]
            model = "default"
        "#;
        let st: SkillTool = toml::from_str(toml_str).unwrap();
        assert_eq!(st.kind, "mcp");
        assert_eq!(st.target.as_deref(), Some("images__generate"));
        assert_eq!(st.locked_args.get("model").unwrap(), "default");
    }

    #[tokio::test]
    async fn skill_builtin_tool_merges_locked_args() {
        let target: Arc<dyn Tool> = Arc::new(MockBuiltinTool::new("composio"));
        let mut locked = HashMap::new();
        locked.insert("action_name".to_string(), "TEXT_TO_PDF".to_string());
        locked.insert("app".to_string(), "pdfco".to_string());
        let st = SkillTool {
            name: "gen_pdf".to_string(),
            description: "Generate PDF".to_string(),
            kind: "builtin".to_string(),
            command: String::new(),
            args: HashMap::new(),
            target: Some("composio".to_string()),
            locked_args: locked.clone(),
            timeout_secs: None,
        };
        let tool = SkillBuiltinTool::new("my_skill", &st, target, locked);
        // Caller passes only "input"; locked args provide action_name + app.
        let result = tool
            .execute(serde_json::json!({"input": "hello"}))
            .await
            .unwrap();
        assert!(result.success);
        // MockBuiltinTool reads "input" — the caller's non-locked arg passes through.
        assert_eq!(result.output, "mock_result:hello");
    }

    /// Mock target that echoes the full (merged) args it received as JSON, so a
    /// test can assert exactly what reached the delegated target.
    struct EchoArgsTool {
        name: String,
    }
    impl ::zeroclaw_api::attribution::Attributable for EchoArgsTool {
        fn role(&self) -> ::zeroclaw_api::attribution::Role {
            ::zeroclaw_api::attribution::Role::Tool(::zeroclaw_api::attribution::ToolKind::Plugin)
        }
        fn alias(&self) -> &str {
            &self.name
        }
    }
    #[async_trait]
    impl Tool for EchoArgsTool {
        fn name(&self) -> &str {
            &self.name
        }
        fn description(&self) -> &str {
            "Echoes received args"
        }
        fn parameters_schema(&self) -> serde_json::Value {
            serde_json::json!({
                "type": "object",
                "properties": {
                    "action": { "type": "string" },
                    "input": { "type": "string" }
                },
                "required": ["action"]
            })
        }
        async fn execute(&self, args: serde_json::Value) -> anyhow::Result<ToolResult> {
            Ok(ToolResult {
                success: true,
                output: args.to_string().into(),
                error: None,
            })
        }
    }

    fn elevation_skill_tool(
        kind: &str,
        target: &str,
        locked: HashMap<String, String>,
    ) -> SkillTool {
        SkillTool {
            name: "delegate".to_string(),
            description: "d".to_string(),
            kind: kind.to_string(),
            command: String::new(),
            args: HashMap::new(),
            target: Some(target.to_string()),
            locked_args: locked,
            timeout_secs: None,
        }
    }

    #[tokio::test]
    async fn skill_elevated_caller_cannot_override_locked_args() {
        // Security regression: a caller must NOT be able to change a locked
        // scope key (the bug was caller-wins).
        let target: Arc<dyn Tool> = Arc::new(EchoArgsTool {
            name: "composio".into(),
        });
        let mut locked = HashMap::new();
        locked.insert("action".to_string(), "execute".to_string());
        let st = elevation_skill_tool("builtin", "composio", locked.clone());
        let tool = SkillBuiltinTool::new("sk", &st, target, locked);
        let result = tool
            .execute(serde_json::json!({"action": "DANGEROUS", "input": "x"}))
            .await
            .unwrap();
        let merged: serde_json::Value = serde_json::from_str(&result.output).unwrap();
        assert_eq!(
            merged["action"], "execute",
            "locked arg must not be overridable"
        );
        assert_eq!(
            merged["input"], "x",
            "caller's non-locked arg passes through"
        );
    }

    #[test]
    fn skill_elevated_advertised_schema_hides_locked_keys() {
        let target: Arc<dyn Tool> = Arc::new(EchoArgsTool {
            name: "composio".into(),
        });
        let mut locked = HashMap::new();
        locked.insert("action".to_string(), "execute".to_string());
        let st = elevation_skill_tool("builtin", "composio", locked.clone());
        let tool = SkillBuiltinTool::new("sk", &st, target, locked);
        let schema = tool.parameters_schema();
        assert!(
            schema["properties"]["action"].is_null(),
            "locked key must be hidden from advertised schema"
        );
        assert!(schema["properties"]["input"].is_object());
        let required: Vec<&str> = schema["required"]
            .as_array()
            .unwrap()
            .iter()
            .filter_map(|v| v.as_str())
            .collect();
        assert!(
            !required.contains(&"action"),
            "locked key removed from required"
        );
    }

    #[test]
    fn skill_elevated_spec_shares_schema_across_calls() {
        let target: Arc<dyn Tool> = Arc::new(EchoArgsTool {
            name: "composio".into(),
        });
        let mut locked = HashMap::new();
        locked.insert("action".to_string(), "execute".to_string());
        let st = elevation_skill_tool("builtin", "composio", locked.clone());
        let tool = SkillBuiltinTool::new("sk", &st, target, locked);

        assert!(
            Arc::ptr_eq(&tool.spec().parameters, &tool.spec().parameters),
            "spec() must hand out the stored Arc, not deep-clone the \
             advertised schema every agent-loop iteration"
        );
        assert_eq!(
            *tool.spec().parameters,
            tool.parameters_schema(),
            "shared spec schema and legacy owned accessor must agree"
        );
    }

    #[tokio::test]
    async fn skill_elevated_mcp_delegates_with_locked_scope() {
        // A `kind = "mcp"` skill tool resolves to an MCP wrapper (mocked here as
        // a tool named like `{server}__{tool}`) and locks the scope so the model
        // cannot change the fixed MCP arguments.
        let target: Arc<dyn Tool> = Arc::new(EchoArgsTool {
            name: "images__generate".into(),
        });
        let mut locked = HashMap::new();
        locked.insert("model".to_string(), "default".to_string());
        let st = elevation_skill_tool("mcp", "images__generate", locked.clone());
        let tool = SkillBuiltinTool::new("art", &st, target, locked);
        assert_eq!(tool.name(), "art__delegate");
        let result = tool
            .execute(serde_json::json!({"model": "evil", "prompt": "a cat"}))
            .await
            .unwrap();
        let merged: serde_json::Value = serde_json::from_str(&result.output).unwrap();
        assert_eq!(
            merged["model"], "default",
            "locked MCP scope arg cannot be overridden"
        );
        assert_eq!(merged["prompt"], "a cat");
    }

    #[test]
    fn merge_locked_args_locks_win_and_passthrough() {
        let mut locked = serde_json::Map::new();
        locked.insert("action".into(), serde_json::Value::String("execute".into()));
        let out = super::merge_locked_args(&locked, serde_json::json!({"action": "x", "extra": 1}));
        assert_eq!(out["action"], "execute");
        assert_eq!(out["extra"], 1);
        // Empty locked set returns the caller args unchanged.
        let caller = serde_json::json!({"a": 1});
        assert_eq!(
            super::merge_locked_args(&serde_json::Map::new(), caller.clone()),
            caller
        );
    }

    #[test]
    fn elevation_wrapper_survives_policy_filter_that_blocks_raw_target() {
        // The trust-boundary contract a SecurityPolicy blocking the
        // raw tool by name must keep it out of the model-visible registry,
        // while the skill's scoped wrapper — registered under the prefixed
        // name — remains the only callable path to that capability.
        use crate::skills::{Skill, SkillTool};

        let shell: Arc<dyn Tool> = Arc::new(MockBuiltinTool::new("shell"));
        let file_read: Arc<dyn Tool> = Arc::new(MockBuiltinTool::new("file_read"));
        // The resolution registry retains the raw tool so the wrapper can
        // delegate to it even after the policy filter removes it below.
        let resolution: Vec<Arc<dyn Tool>> = vec![Arc::clone(&shell), Arc::clone(&file_read)];

        let mut registry: Vec<Box<dyn Tool>> = vec![
            Box::new(crate::tools::ArcToolRef(Arc::clone(&shell))),
            Box::new(crate::tools::ArcToolRef(Arc::clone(&file_read))),
        ];
        let policy = SecurityPolicy {
            excluded_tools: Some(vec!["shell".to_string()]),
            workspace_dir: std::env::temp_dir(),
            ..SecurityPolicy::default()
        };
        crate::agent::loop_::apply_policy_tool_filter(&mut registry, Some(&policy), None);
        assert!(
            !registry.iter().any(|t| t.name() == "shell"),
            "raw shell must be blocked by the policy filter"
        );

        let skill = Skill {
            name: "ops".to_string(),
            description: "d".to_string(),
            description_localizations: Default::default(),
            version: "1".to_string(),
            author: None,
            tags: vec![],
            tools: vec![SkillTool {
                name: "use_shell".to_string(),
                description: "scoped shell".to_string(),
                kind: "builtin".to_string(),
                command: String::new(),
                args: HashMap::new(),
                target: Some("shell".to_string()),
                locked_args: HashMap::new(),
                timeout_secs: None,
            }],
            prompts: vec![],
            slash_options: Vec::new(),
            location: None,
        };
        crate::tools::register_skill_tools_with_context(
            &mut registry,
            &[skill],
            test_security(),
            &resolution,
        );

        assert!(
            !registry.iter().any(|t| t.name() == "shell"),
            "raw shell must STILL be unavailable after skill registration"
        );
        assert!(
            registry.iter().any(|t| t.name() == "ops__use_shell"),
            "the scoped elevation wrapper must be the only callable path"
        );
    }
}
