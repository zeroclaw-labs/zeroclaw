use super::*;
use crate::platform::{NativeRuntime, RuntimeAdapter};
use crate::security::AutonomyLevel;
use std::path::{Path, PathBuf};
use zeroclaw_api::runtime_traits::ShellDialect;

// Use a local lease instead of a container to exercise the executor contract
// on every host platform, without coupling it to Docker's removal protocol.
struct LeaseRuntime {
    native: NativeRuntime,
    fail_prepare: bool,
}

impl RuntimeAdapter for LeaseRuntime {
    fn name(&self) -> &str {
        "test-lease"
    }

    fn has_filesystem_access(&self) -> bool {
        true
    }

    fn storage_path(&self) -> PathBuf {
        self.native.storage_path()
    }

    fn supports_long_running(&self) -> bool {
        true
    }

    fn shell_dialect(&self) -> ShellDialect {
        self.native.shell_dialect()
    }

    fn shell_program(&self) -> Option<&OsStr> {
        self.native.shell_program()
    }

    fn build_shell_command(
        &self,
        command: &str,
        workspace_dir: &Path,
    ) -> anyhow::Result<tokio::process::Command> {
        self.native.build_shell_command(command, workspace_dir)
    }

    fn shell_command_cleanup(
        &self,
        command: &tokio::process::Command,
    ) -> anyhow::Result<Option<Box<dyn ShellCommandCleanup>>> {
        anyhow::ensure!(!self.fail_prepare, "lease preparation refused");
        let lease = command
            .as_std()
            .get_current_dir()
            .unwrap()
            .join("runtime-lease");
        std::fs::write(&lease, [])?;
        Ok(Some(Box::new(LeaseCleanup(lease))))
    }
}

struct LeaseCleanup(PathBuf);

impl Drop for LeaseCleanup {
    fn drop(&mut self) {
        let _ = std::fs::write(self.0.with_extension("released"), []);
    }
}

#[async_trait::async_trait]
impl ShellCommandCleanup for LeaseCleanup {
    async fn cleanup(
        &mut self,
        child: &mut tokio::process::Child,
        _process: &tokio::process::Command,
    ) -> anyhow::Result<()> {
        anyhow::ensure!(
            child.try_wait()?.is_none(),
            "launcher exited before cleanup"
        );
        tokio::fs::remove_file(&self.0).await?;
        Ok(())
    }
}

fn tool(workspace: &Path, fail_prepare: bool) -> ShellTool {
    ShellTool::new(
        Arc::new(SecurityPolicy {
            autonomy: AutonomyLevel::Full,
            workspace_dir: workspace.to_path_buf(),
            allowed_commands: vec!["*".into()],
            block_high_risk_commands: false,
            ..SecurityPolicy::default()
        }),
        Arc::new(LeaseRuntime {
            native: NativeRuntime::new(),
            fail_prepare,
        }),
    )
}

fn long_command(tool: &ShellTool) -> &'static str {
    match tool.runtime.shell_dialect() {
        ShellDialect::Posix => "sleep 75",
        ShellDialect::PowerShell => "Start-Sleep -Seconds 75",
        ShellDialect::WindowsCmd => "for /L %i in (0,0,1) do @rem",
        ShellDialect::None => panic!("test runtime must expose a shell"),
    }
}

#[tokio::test]
async fn timeout_releases_a_non_docker_resource_before_killing_its_launcher() {
    let workspace = tempfile::tempdir().unwrap();
    let tool = tool(workspace.path(), false).with_timeout_secs(1);
    let result = tool
        .execute(json!({"command": long_command(&tool)}))
        .await
        .unwrap();
    assert!(!result.success);
    let error = result.error.unwrap();
    assert!(error.contains("timed out after 1s"), "{error}");
    assert!(!error.contains("cleanup"), "{error}");
    assert!(!workspace.path().join("runtime-lease").exists());
}

#[tokio::test]
async fn cancelling_the_caller_releases_a_non_docker_resource() {
    let workspace = tempfile::tempdir().unwrap();
    let tool = tool(workspace.path(), false);
    let task = zeroclaw_spawn::spawn!(async move {
        tool.execute(json!({"command": long_command(&tool)})).await
    });
    let lease = workspace.path().join("runtime-lease");
    tokio::time::timeout(Duration::from_secs(5), async {
        while !lease.exists() {
            tokio::time::sleep(Duration::from_millis(10)).await;
        }
    })
    .await
    .expect("the runtime must prepare the resource");
    task.abort();
    assert!(task.await.unwrap_err().is_cancelled());
    // Wait for the resource owner to finish, including launcher termination,
    // before this test drops its Tokio runtime.
    let released = lease.with_extension("released");
    tokio::time::timeout(Duration::from_secs(5), async {
        while !released.exists() {
            tokio::time::sleep(Duration::from_millis(10)).await;
        }
    })
    .await
    .expect("the owned cleanup must survive caller cancellation");
    assert!(!lease.exists());
}

#[tokio::test]
async fn cleanup_preparation_failure_prevents_command_execution() {
    let workspace = tempfile::tempdir().unwrap();
    let result = tool(workspace.path(), true)
        .execute(json!({"command": "echo unexpected > ran"}))
        .await
        .unwrap();
    assert!(!result.success);
    let error = result.error.unwrap();
    assert!(error.contains("lease preparation refused"), "{error}");
    assert!(!workspace.path().join("ran").exists());
}
