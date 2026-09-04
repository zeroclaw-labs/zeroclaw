use async_trait::async_trait;
use serde_json::json;
use std::path::{Path, PathBuf};
use std::sync::Arc;
use zeroclaw_api::tool::{Tool, ToolOutput, ToolResult};
use zeroclaw_config::policy::SecurityPolicy;
use zeroclaw_config::policy::ToolOperation;
use zeroclaw_config::schema::CodexCliConfig;

use crate::coding_cli::{
    CodingCliCommand, CodingCliExecutionError, CodingCliExecutor, DirectCodingCliExecutor,
    add_coding_cli_env,
};

tokio::task_local! {
    /// Host-minted repair prompt. Model-authored tool arguments cannot set or
    /// forge this task-local value, so `codex_cli` cannot be used to delegate
    /// the original application task.
    static ZEROCLAW_RECOVERY_PROMPT: String;
}

/// Run one Codex tool invocation inside ZeroClaw's repair-only boundary.
///
/// The runtime constructs `prompt` from typed, sanitized failure metadata.
/// Outside this scope [`CodexCliTool::execute`] fails closed before spawning a
/// process, even if a model supplies a `prompt` field itself.
pub async fn scope_zeroclaw_recovery<F: std::future::Future>(
    prompt: String,
    future: F,
) -> F::Output {
    ZEROCLAW_RECOVERY_PROMPT.scope(prompt, future).await
}

pub struct CodexCliTool {
    security: Arc<SecurityPolicy>,
    config: CodexCliConfig,
    executor: Arc<dyn CodingCliExecutor>,
}

impl CodexCliTool {
    /// Construct a standalone tool that executes directly on the host.
    ///
    /// Runtime registries should use `new_with_executor` so the configured
    /// runtime and sandbox own process execution.
    pub fn new(security: Arc<SecurityPolicy>, config: CodexCliConfig) -> Self {
        Self::new_with_executor(security, config, DirectCodingCliExecutor::shared())
    }

    /// Construct the tool with an injected process executor.
    pub fn new_with_executor(
        security: Arc<SecurityPolicy>,
        config: CodexCliConfig,
        executor: Arc<dyn CodingCliExecutor>,
    ) -> Self {
        Self {
            security,
            config,
            executor,
        }
    }
}

fn codex_exec_args<'a>(config: &'a CodexCliConfig, prompt: &'a str) -> Vec<&'a str> {
    let mut args = vec!["exec"];
    let mut has_terminator = false;

    for (_, arg) in config.effective_extra_args() {
        has_terminator |= arg == "--";
        args.push(arg);
    }

    // Keep the runtime-minted prompt in the positional-argument lane. Without
    // this boundary, a dangling value-taking extra arg could consume the
    // prompt as its value instead of letting Codex parse it as the prompt.
    if !has_terminator {
        args.push("--");
    }
    args.push(prompt);

    args
}

fn canonical_codex_executable(config: &CodexCliConfig) -> Result<PathBuf, &'static str> {
    let configured = config
        .executable_path
        .as_deref()
        .ok_or("Codex recovery unavailable: codex_cli.executable_path is not configured")?;
    if !configured.is_absolute() {
        return Err(
            "Codex recovery unavailable: codex_cli.executable_path must be an absolute path",
        );
    }

    let executable = std::fs::canonicalize(configured).map_err(|_| {
        "Codex recovery unavailable: codex_cli.executable_path does not resolve to an accessible file"
    })?;
    let metadata = std::fs::metadata(&executable).map_err(|_| {
        "Codex recovery unavailable: codex_cli.executable_path does not resolve to an accessible file"
    })?;
    if !metadata.is_file() || !permissions_allow_execution(&metadata) {
        return Err(
            "Codex recovery unavailable: codex_cli.executable_path is not an executable regular file",
        );
    }
    Ok(executable)
}

#[cfg(unix)]
fn permissions_allow_execution(metadata: &std::fs::Metadata) -> bool {
    use std::os::unix::fs::PermissionsExt;

    metadata.permissions().mode() & 0o111 != 0
}

#[cfg(not(unix))]
fn permissions_allow_execution(_metadata: &std::fs::Metadata) -> bool {
    true
}

fn canonical_zeroclaw_source_workspace(config: &CodexCliConfig) -> Result<PathBuf, &'static str> {
    let configured = config.recovery_source_workspace.as_deref().ok_or(
        "Codex recovery unavailable: codex_cli.recovery_source_workspace is not configured",
    )?;
    if !configured.is_absolute() {
        return Err(
            "Codex recovery unavailable: codex_cli.recovery_source_workspace must be an absolute path",
        );
    }

    let workspace = std::fs::canonicalize(configured).map_err(|_| {
        "Codex recovery unavailable: codex_cli.recovery_source_workspace does not resolve to an accessible directory"
    })?;
    if !workspace.is_dir() || !is_zeroclaw_source_workspace(&workspace) {
        return Err(
            "Codex recovery unavailable: codex_cli.recovery_source_workspace is not a valid ZeroClaw source tree",
        );
    }
    Ok(workspace)
}

fn is_zeroclaw_source_workspace(workspace: &Path) -> bool {
    let Some(root_manifest) = source_manifest(workspace, "Cargo.toml") else {
        return false;
    };
    let Some(runtime_manifest) = source_manifest(workspace, "crates/zeroclaw-runtime/Cargo.toml")
    else {
        return false;
    };
    let Some(tools_manifest) = source_manifest(workspace, "crates/zeroclaw-tools/Cargo.toml")
    else {
        return false;
    };

    manifest_package_name(&root_manifest) == Some("zeroclaw")
        && workspace_has_member(&root_manifest, "crates/zeroclaw-runtime")
        && workspace_has_member(&root_manifest, "crates/zeroclaw-tools")
        && manifest_package_name(&runtime_manifest) == Some("zeroclaw-runtime")
        && manifest_package_name(&tools_manifest) == Some("zeroclaw-tools")
}

fn source_manifest(workspace: &Path, relative_path: &str) -> Option<toml::Value> {
    let manifest_path = std::fs::canonicalize(workspace.join(relative_path)).ok()?;
    if !manifest_path.starts_with(workspace) || !manifest_path.is_file() {
        return None;
    }
    let manifest = std::fs::read_to_string(manifest_path).ok()?;
    toml::from_str(&manifest).ok()
}

fn manifest_package_name(manifest: &toml::Value) -> Option<&str> {
    manifest.get("package")?.get("name")?.as_str()
}

fn workspace_has_member(manifest: &toml::Value, expected: &str) -> bool {
    manifest
        .get("workspace")
        .and_then(|workspace| workspace.get("members"))
        .and_then(toml::Value::as_array)
        .is_some_and(|members| {
            members.iter().any(|member| {
                member.as_str().is_some_and(|member| {
                    member.trim_start_matches("./").trim_end_matches('/') == expected
                })
            })
        })
}

#[async_trait]
impl Tool for CodexCliTool {
    fn name(&self) -> &str {
        "codex_cli"
    }

    fn description(&self) -> &str {
        "Internal ZeroClaw repair capability. The runtime invokes it only after a typed stuck-condition trigger; it cannot accept an application task from the model."
    }

    fn parameters_schema(&self) -> serde_json::Value {
        json!({
            "type": "object",
            "properties": {},
            "additionalProperties": false
        })
    }

    async fn execute(&self, _args: serde_json::Value) -> anyhow::Result<ToolResult> {
        // Rate limiting is applied by the RateLimitedTool wrapper at
        // registration time (see zeroclaw-runtime::tools::mod).

        // The production wrapper owns accounting; the adapter owns authorization.
        if let Err(error) = self
            .security
            .authorize_tool_operation(ToolOperation::Act, "codex_cli")
        {
            return Ok(ToolResult {
                success: false,
                output: ToolOutput::default(),
                error: Some(error),
            });
        }

        let prompt = match ZEROCLAW_RECOVERY_PROMPT.try_with(Clone::clone) {
            Ok(prompt) => prompt,
            Err(_) => {
                return Ok(ToolResult::err(
                    "codex_cli is reserved for ZeroClaw runtime recovery and cannot accept model-authored tasks",
                ));
            }
        };

        // Typed operator config is the only executable and working-directory
        // source. The ambient PATH, application workspace in SecurityPolicy,
        // and all model-authored arguments are deliberately ignored.
        let executable = match canonical_codex_executable(&self.config) {
            Ok(executable) => executable,
            Err(error) => return Ok(ToolResult::err(error)),
        };
        let work_dir = match canonical_zeroclaw_source_workspace(&self.config) {
            Ok(work_dir) => work_dir,
            Err(error) => return Ok(ToolResult::err(error)),
        };
        if self.config.recovery_workspace_override_arg().is_some() {
            return Ok(ToolResult::err(
                "Codex recovery unavailable: codex_cli.extra_args cannot override recovery_source_workspace with --cd or -C",
            ));
        }

        // Build CLI command: `codex exec [extra_args...] <prompt>`
        let mut cmd = CodingCliCommand::new(executable, work_dir.clone(), self.config.timeout_secs);
        cmd.args(codex_exec_args(&self.config, &prompt));

        add_coding_cli_env(&mut cmd, &self.config.env_passthrough);

        match self.executor.output(cmd).await {
            Ok(output) => {
                let mut stdout = String::from_utf8_lossy(&output.stdout).to_string();
                let stderr = String::from_utf8_lossy(&output.stderr).to_string();

                // Truncate to max_output_bytes with char-boundary safety
                if stdout.len() > self.config.max_output_bytes {
                    let mut b = self.config.max_output_bytes.min(stdout.len());
                    while b > 0 && !stdout.is_char_boundary(b) {
                        b -= 1;
                    }
                    stdout.truncate(b);
                    stdout.push_str("\n... [output truncated]");
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
            Err(CodingCliExecutionError::Io(e)) => Ok(ToolResult {
                success: false,
                output: ToolOutput::default(),
                error: Some(format!(
                    "Failed to execute configured Codex executable: {e}"
                )),
            }),
            Err(CodingCliExecutionError::Timeout) => Ok(ToolResult {
                success: false,
                output: ToolOutput::default(),
                error: Some(format!(
                    "Codex CLI timed out after {}s and was killed",
                    self.config.timeout_secs
                )),
            }),
            Err(CodingCliExecutionError::Prepare(e)) => Ok(ToolResult {
                success: false,
                output: ToolOutput::default(),
                error: Some(format!("Failed to prepare codex execution: {e}")),
            }),
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::coding_cli::{CodingCliCommand, CodingCliExecutionError};
    use std::sync::Mutex;
    use zeroclaw_config::autonomy::AutonomyLevel;
    use zeroclaw_config::policy::SecurityPolicy;
    use zeroclaw_config::schema::CodexCliConfig;

    fn test_config() -> CodexCliConfig {
        CodexCliConfig::default()
    }

    fn recovery_config(workspace: &Path) -> CodexCliConfig {
        CodexCliConfig {
            executable_path: Some(
                std::env::current_exe().expect("test executable path should be available"),
            ),
            recovery_source_workspace: Some(workspace.to_path_buf()),
            ..CodexCliConfig::default()
        }
    }

    fn test_security(autonomy: AutonomyLevel) -> Arc<SecurityPolicy> {
        Arc::new(SecurityPolicy {
            autonomy,
            workspace_dir: std::env::temp_dir(),
            ..SecurityPolicy::default()
        })
    }

    fn test_security_with_workspace(
        autonomy: AutonomyLevel,
        workspace_dir: std::path::PathBuf,
    ) -> Arc<SecurityPolicy> {
        Arc::new(SecurityPolicy {
            autonomy,
            workspace_dir,
            ..SecurityPolicy::default()
        })
    }

    fn mark_as_zeroclaw_source(workspace: &std::path::Path) {
        std::fs::create_dir_all(workspace.join("crates/zeroclaw-runtime"))
            .expect("runtime marker directory");
        std::fs::create_dir_all(workspace.join("crates/zeroclaw-tools"))
            .expect("tools marker directory");
        std::fs::write(
            workspace.join("Cargo.toml"),
            r#"[workspace]
members = ["crates/zeroclaw-runtime", "crates/zeroclaw-tools"]

[package]
name = "zeroclaw"
version = "0.0.0"
"#,
        )
        .expect("root source manifest");
        std::fs::write(
            workspace.join("crates/zeroclaw-runtime/Cargo.toml"),
            "[package]\nname = \"zeroclaw-runtime\"\nversion = \"0.0.0\"\n",
        )
        .expect("runtime source manifest");
        std::fs::write(
            workspace.join("crates/zeroclaw-tools/Cargo.toml"),
            "[package]\nname = \"zeroclaw-tools\"\nversion = \"0.0.0\"\n",
        )
        .expect("tools source manifest");
    }

    #[derive(Default)]
    struct RecordingExecutor {
        commands: Mutex<Vec<CodingCliCommand>>,
    }

    #[async_trait]
    impl CodingCliExecutor for RecordingExecutor {
        async fn output(
            &self,
            command: CodingCliCommand,
        ) -> Result<std::process::Output, CodingCliExecutionError> {
            self.commands.lock().unwrap().push(command);
            Err(CodingCliExecutionError::Timeout)
        }
    }

    #[test]
    fn codex_cli_tool_name() {
        let tool = CodexCliTool::new(test_security(AutonomyLevel::Supervised), test_config());
        assert_eq!(tool.name(), "codex_cli");
    }

    #[test]
    fn codex_cli_tool_schema_accepts_no_model_authored_task() {
        let tool = CodexCliTool::new(test_security(AutonomyLevel::Supervised), test_config());
        let schema = tool.parameters_schema();
        assert_eq!(schema["properties"], json!({}));
        assert_eq!(schema["additionalProperties"], false);
    }

    #[tokio::test]
    async fn codex_cli_blocks_rate_limited() {
        let security = Arc::new(SecurityPolicy {
            autonomy: AutonomyLevel::Supervised,
            max_actions_per_hour: 0,
            workspace_dir: std::env::temp_dir(),
            ..SecurityPolicy::default()
        });
        let tool = crate::wrappers::RateLimitedTool::new(
            CodexCliTool::new(security.clone(), test_config()),
            security,
        );
        let result =
            scope_zeroclaw_recovery("repair ZeroClaw".to_string(), tool.execute(json!({})))
                .await
                .expect("rate-limited should return a result");
        assert!(!result.success);
        assert!(result.error.as_deref().unwrap_or("").contains("Rate limit"));
    }

    #[tokio::test]
    async fn codex_cli_blocks_readonly() {
        let tool = CodexCliTool::new(test_security(AutonomyLevel::ReadOnly), test_config());
        let result =
            scope_zeroclaw_recovery("repair ZeroClaw".to_string(), tool.execute(json!({})))
                .await
                .expect("readonly should return a result");
        assert!(!result.success);
        assert!(
            result
                .error
                .as_deref()
                .unwrap_or("")
                .contains("read-only mode")
        );
    }

    #[tokio::test]
    async fn codex_cli_rejects_model_authored_task_outside_recovery_scope() {
        let tool = CodexCliTool::new(test_security(AutonomyLevel::Supervised), test_config());
        let result = tool
            .execute(json!({"prompt": "complete the original task"}))
            .await
            .expect("out-of-scope request should fail closed as a tool result");
        assert!(!result.success);
        assert!(
            result
                .error
                .unwrap_or_default()
                .contains("runtime recovery")
        );
    }

    #[tokio::test]
    async fn codex_cli_recovery_uses_only_canonical_configured_source_workspace() {
        let application_workspace = tempfile::TempDir::new().expect("application workspace");
        let source_parent = tempfile::TempDir::new().expect("source parent");
        let source_workspace = source_parent.path().join("zeroclaw-source");
        std::fs::create_dir(&source_workspace).expect("source workspace");
        mark_as_zeroclaw_source(&source_workspace);
        let configured_source = source_workspace.join(".");
        let model_selected = tempfile::TempDir::new().expect("model-selected directory");
        let executor = Arc::new(RecordingExecutor::default());
        let tool = CodexCliTool::new_with_executor(
            test_security_with_workspace(
                AutonomyLevel::Full,
                application_workspace.path().to_path_buf(),
            ),
            recovery_config(&configured_source),
            executor.clone(),
        );
        let result = scope_zeroclaw_recovery(
            "repair ZeroClaw".to_string(),
            tool.execute(json!({
                "prompt": "complete the original task",
                "working_directory": model_selected.path()
            })),
        )
        .await
        .expect("scoped recovery should return a tool result");
        assert!(!result.success);
        let commands = executor.commands.lock().unwrap();
        assert_eq!(commands.len(), 1);
        assert_eq!(
            commands[0].program,
            std::fs::canonicalize(std::env::current_exe().expect("test executable path"))
                .expect("canonical test executable path")
                .into_os_string()
        );
        assert_ne!(application_workspace.path(), source_workspace);
        assert_eq!(
            commands[0].working_dir,
            std::fs::canonicalize(&source_workspace).expect("canonical source workspace")
        );
        let args = commands[0]
            .args
            .iter()
            .map(|arg| arg.to_string_lossy())
            .collect::<Vec<_>>();
        assert_eq!(args.last().map(|arg| arg.as_ref()), Some("repair ZeroClaw"));
        assert!(!args.iter().any(|arg| arg.contains("original task")));
    }

    #[tokio::test]
    async fn codex_cli_recovery_without_a_configured_source_fails_closed() {
        let workspace = tempfile::TempDir::new().expect("application workspace");
        let executor = Arc::new(RecordingExecutor::default());
        let tool = CodexCliTool::new_with_executor(
            test_security_with_workspace(AutonomyLevel::Full, workspace.path().to_path_buf()),
            CodexCliConfig {
                executable_path: Some(
                    std::env::current_exe().expect("test executable path should be available"),
                ),
                ..test_config()
            },
            executor.clone(),
        );

        let result =
            scope_zeroclaw_recovery("repair ZeroClaw".to_string(), tool.execute(json!({})))
                .await
                .expect("workspace rejection should be a tool result");

        assert!(!result.success);
        assert!(
            result
                .error
                .unwrap_or_default()
                .contains("recovery_source_workspace is not configured")
        );
        assert!(executor.commands.lock().unwrap().is_empty());
    }

    #[tokio::test]
    async fn codex_cli_recovery_without_a_configured_executable_fails_closed() {
        let source = tempfile::TempDir::new().expect("source workspace");
        mark_as_zeroclaw_source(source.path());
        let executor = Arc::new(RecordingExecutor::default());
        let tool = CodexCliTool::new_with_executor(
            test_security(AutonomyLevel::Full),
            CodexCliConfig {
                recovery_source_workspace: Some(source.path().to_path_buf()),
                ..CodexCliConfig::default()
            },
            executor.clone(),
        );

        let result =
            scope_zeroclaw_recovery("repair ZeroClaw".to_string(), tool.execute(json!({})))
                .await
                .expect("executable rejection should be a tool result");

        assert!(!result.success);
        assert!(
            result
                .error
                .unwrap_or_default()
                .contains("executable_path is not configured")
        );
        assert!(executor.commands.lock().unwrap().is_empty());
    }

    #[tokio::test]
    async fn codex_cli_recovery_with_a_relative_executable_fails_closed() {
        let source = tempfile::TempDir::new().expect("source workspace");
        mark_as_zeroclaw_source(source.path());
        let executor = Arc::new(RecordingExecutor::default());
        let tool = CodexCliTool::new_with_executor(
            test_security(AutonomyLevel::Full),
            CodexCliConfig {
                executable_path: Some(PathBuf::from("codex")),
                recovery_source_workspace: Some(source.path().to_path_buf()),
                ..CodexCliConfig::default()
            },
            executor.clone(),
        );

        let result =
            scope_zeroclaw_recovery("repair ZeroClaw".to_string(), tool.execute(json!({})))
                .await
                .expect("executable rejection should be a tool result");

        assert!(!result.success);
        assert!(
            result
                .error
                .unwrap_or_default()
                .contains("executable_path must be an absolute path")
        );
        assert!(executor.commands.lock().unwrap().is_empty());
    }

    #[cfg(unix)]
    #[tokio::test]
    async fn codex_cli_recovery_canonicalizes_the_configured_executable() {
        use std::os::unix::fs::symlink;

        let source = tempfile::TempDir::new().expect("source workspace");
        mark_as_zeroclaw_source(source.path());
        let executable_link = source.path().join("codex-link");
        let test_executable = std::env::current_exe().expect("test executable path");
        symlink(&test_executable, &executable_link).expect("executable symlink fixture");
        let executor = Arc::new(RecordingExecutor::default());
        let tool = CodexCliTool::new_with_executor(
            test_security(AutonomyLevel::Full),
            CodexCliConfig {
                executable_path: Some(executable_link),
                recovery_source_workspace: Some(source.path().to_path_buf()),
                ..CodexCliConfig::default()
            },
            executor.clone(),
        );

        let result =
            scope_zeroclaw_recovery("repair ZeroClaw".to_string(), tool.execute(json!({})))
                .await
                .expect("canonical executable should reach the executor");

        assert!(!result.success, "recording executor returns timeout");
        let commands = executor.commands.lock().unwrap();
        assert_eq!(commands.len(), 1);
        assert_eq!(
            commands[0].program,
            std::fs::canonicalize(test_executable)
                .expect("canonical test executable")
                .into_os_string()
        );
    }

    #[tokio::test]
    async fn codex_cli_recovery_with_a_directory_executable_fails_closed() {
        let source = tempfile::TempDir::new().expect("source workspace");
        mark_as_zeroclaw_source(source.path());
        let executor = Arc::new(RecordingExecutor::default());
        let tool = CodexCliTool::new_with_executor(
            test_security(AutonomyLevel::Full),
            CodexCliConfig {
                executable_path: Some(source.path().to_path_buf()),
                recovery_source_workspace: Some(source.path().to_path_buf()),
                ..CodexCliConfig::default()
            },
            executor.clone(),
        );

        let result =
            scope_zeroclaw_recovery("repair ZeroClaw".to_string(), tool.execute(json!({})))
                .await
                .expect("executable rejection should be a tool result");

        assert!(!result.success);
        assert!(
            result
                .error
                .unwrap_or_default()
                .contains("not an executable regular file")
        );
        assert!(executor.commands.lock().unwrap().is_empty());
    }

    #[cfg(unix)]
    #[tokio::test]
    async fn codex_cli_recovery_with_a_non_executable_file_fails_closed() {
        use std::os::unix::fs::PermissionsExt;

        let source = tempfile::TempDir::new().expect("source workspace");
        mark_as_zeroclaw_source(source.path());
        let non_executable = source.path().join("codex");
        std::fs::write(&non_executable, "not executable").expect("non-executable fixture");
        std::fs::set_permissions(&non_executable, std::fs::Permissions::from_mode(0o600))
            .expect("non-executable permissions");
        let executor = Arc::new(RecordingExecutor::default());
        let tool = CodexCliTool::new_with_executor(
            test_security(AutonomyLevel::Full),
            CodexCliConfig {
                executable_path: Some(non_executable),
                recovery_source_workspace: Some(source.path().to_path_buf()),
                ..CodexCliConfig::default()
            },
            executor.clone(),
        );

        let result =
            scope_zeroclaw_recovery("repair ZeroClaw".to_string(), tool.execute(json!({})))
                .await
                .expect("executable rejection should be a tool result");

        assert!(!result.success);
        assert!(
            result
                .error
                .unwrap_or_default()
                .contains("not an executable regular file")
        );
        assert!(executor.commands.lock().unwrap().is_empty());
    }

    #[tokio::test]
    async fn codex_cli_recovery_with_an_invalid_source_fails_closed() {
        let application_workspace = tempfile::TempDir::new().expect("application workspace");
        let invalid_source = tempfile::TempDir::new().expect("invalid source workspace");
        mark_as_zeroclaw_source(invalid_source.path());
        std::fs::write(
            invalid_source
                .path()
                .join("crates/zeroclaw-tools/Cargo.toml"),
            "[package]\nname = \"lookalike-tools\"\nversion = \"0.0.0\"\n",
        )
        .expect("replace tools manifest with invalid package identity");
        let executor = Arc::new(RecordingExecutor::default());
        let tool = CodexCliTool::new_with_executor(
            test_security_with_workspace(
                AutonomyLevel::Full,
                application_workspace.path().to_path_buf(),
            ),
            recovery_config(invalid_source.path()),
            executor.clone(),
        );

        let result =
            scope_zeroclaw_recovery("repair ZeroClaw".to_string(), tool.execute(json!({})))
                .await
                .expect("source rejection should be a tool result");

        assert!(!result.success);
        assert!(
            result
                .error
                .unwrap_or_default()
                .contains("not a valid ZeroClaw source tree")
        );
        assert!(executor.commands.lock().unwrap().is_empty());
    }

    #[tokio::test]
    async fn codex_cli_extra_args_cannot_replace_the_recovery_workspace() {
        let source = tempfile::TempDir::new().expect("source workspace");
        mark_as_zeroclaw_source(source.path());
        let executor = Arc::new(RecordingExecutor::default());
        let mut config = recovery_config(source.path());
        config.extra_args = vec!["--cd".into(), "/tmp".into()];
        let tool = CodexCliTool::new_with_executor(
            test_security(AutonomyLevel::Full),
            config,
            executor.clone(),
        );

        let result =
            scope_zeroclaw_recovery("repair ZeroClaw".to_string(), tool.execute(json!({})))
                .await
                .expect("working-directory override rejection should be a tool result");

        assert!(!result.success);
        assert!(result.error.unwrap_or_default().contains("--cd or -C"));
        assert!(executor.commands.lock().unwrap().is_empty());
    }

    #[test]
    fn codex_cli_env_passthrough_defaults() {
        let config = CodexCliConfig::default();
        assert!(
            config.env_passthrough.is_empty(),
            "env_passthrough should default to empty"
        );
    }

    #[test]
    fn codex_cli_extra_args_defaults() {
        let config = CodexCliConfig::default();
        assert!(
            config.extra_args.is_empty(),
            "extra_args should default to empty"
        );
    }

    #[test]
    fn codex_cli_command_args_separate_prompt_from_dangling_value_flags() {
        let prompt = "danger-full-access";

        for flag in [
            "--sandbox",
            "--config",
            "-c",
            "--profile",
            "--cd",
            "-C",
            "--add-dir",
            "--enable",
            "--disable",
        ] {
            let mut config = test_config();
            config.extra_args = vec![flag.to_string()];

            assert_eq!(
                codex_exec_args(&config, prompt),
                vec!["exec", flag, "--", prompt],
                "{flag} must not consume the prompt as its value"
            );
        }
    }

    #[test]
    fn codex_cli_command_args_preserve_an_explicit_terminator() {
        let mut config = test_config();
        config.extra_args = vec!["  --skip-git-repo-check  ".to_string(), "--".to_string()];

        assert_eq!(
            codex_exec_args(&config, "--prompt-starting-with-a-dash"),
            vec![
                "exec",
                "--skip-git-repo-check",
                "--",
                "--prompt-starting-with-a-dash"
            ]
        );
    }

    #[test]
    fn codex_cli_default_config_values() {
        let config = CodexCliConfig::default();
        assert!(!config.enabled);
        assert!(config.executable_path.is_none());
        assert!(config.recovery_source_workspace.is_none());
        assert_eq!(config.timeout_secs, 600);
        assert_eq!(config.max_output_bytes, 2_097_152);
    }
}
