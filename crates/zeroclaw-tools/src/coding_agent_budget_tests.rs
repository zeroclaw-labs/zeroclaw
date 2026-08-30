use crate::claude_code::ClaudeCodeTool;
use crate::claude_code_runner::ClaudeCodeRunnerTool;
use crate::codex_cli::CodexCliTool;
use crate::coding_cli::{CodingCliCommand, CodingCliExecutionError, CodingCliExecutor};
use crate::gemini_cli::GeminiCliTool;
use crate::opencode_cli::OpenCodeCliTool;
use crate::wrappers::RateLimitedTool;
use async_trait::async_trait;
use serde_json::json;
use std::path::Path;
use std::process::{ExitStatus, Output};
use std::sync::Arc;
use zeroclaw_api::tool::Tool;
use zeroclaw_config::autonomy::AutonomyLevel;
use zeroclaw_config::policy::SecurityPolicy;
use zeroclaw_config::schema::{
    ClaudeCodeConfig, ClaudeCodeRunnerConfig, CodexCliConfig, GeminiCliConfig, OpenCodeCliConfig,
};

#[derive(Debug)]
struct SuccessfulExecutor;

#[async_trait]
impl CodingCliExecutor for SuccessfulExecutor {
    async fn output(&self, _command: CodingCliCommand) -> Result<Output, CodingCliExecutionError> {
        Ok(Output {
            status: successful_exit_status(),
            stdout: b"ok".to_vec(),
            stderr: Vec::new(),
        })
    }
}

#[cfg(unix)]
fn successful_exit_status() -> ExitStatus {
    use std::os::unix::process::ExitStatusExt;
    ExitStatus::from_raw(0)
}

#[cfg(windows)]
fn successful_exit_status() -> ExitStatus {
    use std::os::windows::process::ExitStatusExt;
    ExitStatus::from_raw(0)
}

fn policy(
    autonomy: AutonomyLevel,
    max_actions_per_hour: u32,
    workspace: &Path,
) -> Arc<SecurityPolicy> {
    Arc::new(SecurityPolicy {
        autonomy,
        max_actions_per_hour,
        workspace_dir: workspace.to_path_buf(),
        ..SecurityPolicy::default()
    })
}

fn wrapped_tool<T: Tool + 'static>(inner: T, security: Arc<SecurityPolicy>) -> Box<dyn Tool> {
    Box::new(RateLimitedTool::new(inner, security))
}

#[cfg(unix)]
fn write_successful_tmux(path: &Path) {
    use std::os::unix::fs::PermissionsExt;

    std::fs::write(path, "#!/bin/sh\nexit 0\n").expect("write tmux fixture");
    let mut permissions = std::fs::metadata(path)
        .expect("tmux fixture metadata")
        .permissions();
    permissions.set_mode(0o755);
    std::fs::set_permissions(path, permissions).expect("make tmux fixture executable");
}

#[cfg(unix)]
fn coding_agent_cases(
    autonomy: AutonomyLevel,
    max_actions_per_hour: u32,
    workspace: &Path,
    tmux_binary: &Path,
) -> Vec<(Arc<SecurityPolicy>, Box<dyn Tool>)> {
    let executor: Arc<dyn CodingCliExecutor> = Arc::new(SuccessfulExecutor);
    let mut cases = Vec::new();

    let security = policy(autonomy, max_actions_per_hour, workspace);
    cases.push((
        security.clone(),
        wrapped_tool(
            ClaudeCodeTool::new_with_executor(
                security.clone(),
                ClaudeCodeConfig::default(),
                executor.clone(),
            ),
            security,
        ),
    ));

    let security = policy(autonomy, max_actions_per_hour, workspace);
    cases.push((
        security.clone(),
        wrapped_tool(
            ClaudeCodeRunnerTool::new(
                security.clone(),
                ClaudeCodeRunnerConfig::default(),
                "http://localhost:3000".into(),
            )
            .with_tmux_binary(tmux_binary.to_path_buf()),
            security,
        ),
    ));

    let security = policy(autonomy, max_actions_per_hour, workspace);
    cases.push((
        security.clone(),
        wrapped_tool(
            CodexCliTool::new_with_executor(
                security.clone(),
                CodexCliConfig::default(),
                executor.clone(),
            ),
            security,
        ),
    ));

    let security = policy(autonomy, max_actions_per_hour, workspace);
    cases.push((
        security.clone(),
        wrapped_tool(
            GeminiCliTool::new_with_executor(
                security.clone(),
                GeminiCliConfig::default(),
                executor.clone(),
            ),
            security,
        ),
    ));

    let security = policy(autonomy, max_actions_per_hour, workspace);
    cases.push((
        security.clone(),
        wrapped_tool(
            OpenCodeCliTool::new_with_executor(
                security.clone(),
                OpenCodeCliConfig::default(),
                executor,
            ),
            security,
        ),
    ));

    cases
}

#[cfg(unix)]
#[tokio::test]
async fn coding_agent_wrappers_charge_one_action_each() {
    let workspace = tempfile::TempDir::new().expect("workspace");
    let tmux_binary = workspace.path().join("tmux");
    write_successful_tmux(&tmux_binary);

    for (security, tool) in
        coding_agent_cases(AutonomyLevel::Full, 2, workspace.path(), &tmux_binary)
    {
        let tool_name = tool.name().to_string();
        let result = tool
            .execute(json!({"prompt": "hello"}))
            .await
            .expect("coding-agent invocation");
        assert!(result.success, "{tool_name} should succeed: {result:?}");
        assert!(
            security.record_action(),
            "{tool_name} should leave exactly one of two action slots available"
        );
        assert!(
            !security.record_action(),
            "{tool_name} must not leave a third action slot"
        );
    }
}

#[cfg(unix)]
#[tokio::test]
async fn coding_agent_readonly_rejection_consumes_no_action() {
    let workspace = tempfile::TempDir::new().expect("workspace");
    let tmux_binary = workspace.path().join("tmux");

    for (security, tool) in
        coding_agent_cases(AutonomyLevel::ReadOnly, 1, workspace.path(), &tmux_binary)
    {
        let tool_name = tool.name().to_string();
        let result = tool
            .execute(json!({"prompt": "hello"}))
            .await
            .expect("read-only rejection");
        assert!(
            !result.success,
            "{tool_name} must be rejected in read-only mode"
        );
        assert!(
            result.error.as_deref().unwrap_or("").contains("read-only"),
            "{tool_name} should report the autonomy rejection"
        );
        assert!(
            security.record_action(),
            "{tool_name} rejection must leave the action slot available"
        );
    }
}
