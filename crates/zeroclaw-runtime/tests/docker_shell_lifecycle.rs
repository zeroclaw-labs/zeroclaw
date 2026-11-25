//! Docker lifecycle regressions. Live daemon tests require `--ignored`.

use std::sync::Arc;
use std::time::Duration;

use serde_json::json;
use tempfile::TempDir;
use zeroclaw_api::tool::Tool;
use zeroclaw_config::platform::DockerRuntime;
use zeroclaw_config::schema::{Config, RiskProfileConfig, RuntimeKind};
use zeroclaw_runtime::security::{AutonomyLevel, SecurityPolicy};
use zeroclaw_runtime::tools::{ShellTool, shell_tool_for_runtime};

fn docker_tool(workspace: &std::path::Path, timeout_secs: u64) -> ShellTool {
    let mut config = Config::default();
    config.runtime.kind = RuntimeKind::Docker;
    config.runtime.docker.image = "alpine:3.23".into();
    config.runtime.docker.mount_workspace = true;
    config.runtime.docker.network = "none".into();
    config.runtime.docker.read_only_rootfs = true;
    let security = Arc::new(SecurityPolicy {
        autonomy: AutonomyLevel::Full,
        workspace_dir: workspace.to_path_buf(),
        allowed_commands: vec!["*".into()],
        block_high_risk_commands: false,
        shell_timeout_secs: timeout_secs,
        ..SecurityPolicy::default()
    });
    shell_tool_for_runtime(
        security,
        Arc::new(DockerRuntime::new(config.runtime.docker.clone())),
        &RiskProfileConfig {
            sandbox_backend: Some("docker".into()),
            ..RiskProfileConfig::default()
        },
        &config,
    )
}

async fn wait_for_file(path: &std::path::Path) {
    tokio::time::timeout(Duration::from_secs(10), async {
        while !path.exists() {
            tokio::time::sleep(Duration::from_millis(20)).await;
        }
    })
    .await
    .expect("fixture must reach the expected state");
}

struct Workspace(TempDir);

impl Workspace {
    fn new() -> Self {
        Self(tempfile::tempdir().expect("create test workspace"))
    }

    fn containers(&self) -> Vec<String> {
        let output = std::process::Command::new("docker")
            .args(["ps", "--all", "--quiet", "--filter"])
            .arg(format!(
                "volume={}",
                self.0.path().canonicalize().unwrap().display()
            ))
            .output()
            .expect("list test containers");
        assert!(output.status.success(), "Docker must be available");
        String::from_utf8(output.stdout)
            .unwrap()
            .lines()
            .map(str::to_owned)
            .collect()
    }

    fn tool(&self, timeout_secs: u64) -> ShellTool {
        docker_tool(self.0.path(), timeout_secs)
    }

    async fn wait_for_file(&self, name: &str) {
        wait_for_file(&self.0.path().join(name)).await;
    }
}

#[cfg(unix)]
mod without_daemon {
    use super::*;
    use std::os::unix::fs::PermissionsExt;

    struct DockerFixture(TempDir);

    impl DockerFixture {
        fn new() -> Self {
            let root = tempfile::tempdir().unwrap();
            let launcher = root.path().join("docker");
            std::fs::copy(
                concat!(
                    env!("CARGO_MANIFEST_DIR"),
                    "/tests/fixtures/docker-lifecycle.sh"
                ),
                &launcher,
            )
            .unwrap();
            std::fs::set_permissions(launcher, std::fs::Permissions::from_mode(0o755)).unwrap();
            Self(root)
        }

        fn tool(&self, timeout_secs: u64) -> ShellTool {
            docker_tool(self.0.path(), timeout_secs).with_tui_env(Some(
                std::collections::HashMap::from([
                    ("PATH".into(), self.0.path().to_str().unwrap().into()),
                    (
                        "ZEROCLAW_DOCKER_TEST_STATE".into(),
                        self.0.path().to_str().unwrap().into(),
                    ),
                ]),
            ))
        }

        fn touch(&self, name: &str) {
            std::fs::write(self.0.path().join(name), "").unwrap();
        }

        async fn wait(&self, name: &str) {
            wait_for_file(&self.0.path().join(name)).await;
        }

        async fn assert_removed(&self) {
            self.wait("created").await;
            let name = std::fs::read_to_string(self.0.path().join("created")).unwrap();
            tokio::time::timeout(Duration::from_secs(5), async {
                while self.0.path().join(&name).exists() {
                    tokio::time::sleep(Duration::from_millis(20)).await;
                }
            })
            .await
            .expect("cleanup must remove the container created after cancellation/timeout");
            assert_eq!(
                std::fs::read_to_string(self.0.path().join("cleanup-target")).unwrap(),
                name
            );
        }
    }

    #[tokio::test]
    async fn timeout_waits_for_in_flight_container_creation() {
        let fixture = DockerFixture::new();
        fixture.touch("defer-create");
        let tool = fixture.tool(1);
        let task =
            zeroclaw_spawn::spawn!(
                async move { tool.execute(json!({"command": "sleep 75"})).await }
            );
        fixture.wait("cleanup-target").await;
        std::fs::remove_file(fixture.0.path().join("defer-create")).unwrap();
        let result = task.await.unwrap().unwrap();
        assert!(result.error.unwrap().contains("timed out after 1s"));
        fixture.assert_removed().await;
    }

    #[tokio::test]
    async fn cancellation_waits_for_in_flight_container_creation() {
        let fixture = DockerFixture::new();
        fixture.touch("defer-create");
        let tool = fixture.tool(90);
        let task =
            zeroclaw_spawn::spawn!(
                async move { tool.execute(json!({"command": "sleep 75"})).await }
            );
        fixture.wait("launcher-ready").await;
        task.abort();
        assert!(task.await.unwrap_err().is_cancelled());
        fixture.wait("cleanup-target").await;
        std::fs::remove_file(fixture.0.path().join("defer-create")).unwrap();
        fixture.assert_removed().await;
    }

    #[tokio::test]
    async fn cleanup_failure_reports_that_the_container_may_still_be_running() {
        let fixture = DockerFixture::new();
        fixture.touch("fail-cleanup");
        let result = fixture
            .tool(1)
            .execute(json!({"command": "sleep 75"}))
            .await
            .unwrap();
        assert!(!result.success);
        let error = result.error.unwrap();
        assert!(error.contains("may still be running"), "{error}");
        assert!(error.contains("daemon unavailable"), "{error}");
        assert!(!error.contains("was killed"), "{error}");
    }

    #[tokio::test]
    async fn cleanup_rechecks_absence_after_launcher_exits() {
        let fixture = DockerFixture::new();
        fixture.touch("defer-create");
        fixture.touch("create-after-miss");
        let result = fixture
            .tool(1)
            .execute(json!({"command": "sleep 75"}))
            .await
            .unwrap();
        assert!(result.error.unwrap().contains("timed out after 1s"));
        fixture.assert_removed().await;
    }

    #[tokio::test]
    async fn missing_container_after_launcher_failure_preserves_diagnostic() {
        let fixture = DockerFixture::new();
        fixture.touch("fail-launch");
        let result = fixture
            .tool(5)
            .execute(json!({"command": "sleep 75"}))
            .await
            .unwrap();
        assert!(!result.success);
        let error = result.error.unwrap();
        assert!(error.contains("exit status 125"), "{error}");
        assert!(error.contains("cannot start fixture container"), "{error}");
        assert!(!error.contains("cleanup"), "{error}");
    }

    #[tokio::test]
    async fn cleanup_of_an_unresponsive_daemon_is_bounded() {
        let fixture = DockerFixture::new();
        fixture.touch("hang-cleanup");
        let result = tokio::time::timeout(
            Duration::from_secs(20),
            fixture.tool(1).execute(json!({"command": "sleep 75"})),
        )
        .await
        .expect("cleanup must not wait indefinitely")
        .unwrap();
        assert!(!result.success);
        let error = result.error.unwrap();
        assert!(error.contains("may still be running"), "{error}");
        assert!(error.contains("10 seconds"), "{error}");
        assert!(!error.contains("was killed"), "{error}");
    }
}

impl Drop for Workspace {
    fn drop(&mut self) {
        // Only containers mounting this unique temporary workspace belong to us.
        for id in self.containers() {
            let _ = std::process::Command::new("docker")
                .args(["rm", "--force", &id])
                .output();
        }
    }
}

#[tokio::test]
#[ignore = "requires a Docker daemon and alpine:3.23"]
async fn docker_timeout_removes_container_before_returning() {
    let workspace = Workspace::new();
    let result = workspace
        .tool(3)
        .execute(json!({"command": "echo ready > ready; sleep 75; echo leaked > leaked"}))
        .await
        .unwrap();

    assert!(!result.success);
    assert!(
        result
            .error
            .as_deref()
            .unwrap_or_default()
            .contains("timed out after 3s"),
        "{result:?}"
    );
    assert!(workspace.0.path().join("ready").exists());
    assert!(
        workspace.containers().is_empty(),
        "a timed-out command must not retain a container with workspace write access"
    );
    assert!(!workspace.0.path().join("leaked").exists());
}

#[tokio::test]
#[ignore = "requires a Docker daemon and alpine:3.23"]
async fn docker_cancellation_removes_container() {
    let workspace = Workspace::new();
    let tool = workspace.tool(90);
    let task = zeroclaw_spawn::spawn!(async move {
        tool.execute(json!({"command": "echo ready > ready; sleep 75"}))
            .await
    });
    workspace.wait_for_file("ready").await;
    assert_eq!(workspace.containers().len(), 1);
    task.abort();
    assert!(task.await.unwrap_err().is_cancelled());

    tokio::time::timeout(Duration::from_secs(10), async {
        while !workspace.containers().is_empty() {
            tokio::time::sleep(Duration::from_millis(50)).await;
        }
    })
    .await
    .expect("cancelled tool must remove its container");
}

#[tokio::test]
#[ignore = "requires a Docker daemon and alpine:3.23"]
async fn docker_nonzero_exit_preserves_diagnostic_after_auto_removal() {
    let workspace = Workspace::new();
    let result = workspace
        .tool(5)
        .execute(json!({"command": "echo command-failed >&2; exit 7"}))
        .await
        .unwrap();
    assert!(!result.success);
    let error = result.error.unwrap();
    assert!(error.contains("exit status 7"), "{error}");
    assert!(error.contains("command-failed"), "{error}");
    assert!(!error.contains("cleanup"), "{error}");
    assert!(workspace.containers().is_empty());
}

#[tokio::test]
#[ignore = "requires a Docker daemon and alpine:3.23"]
async fn docker_timeout_leaves_another_workspace_running() {
    let timed_out = Workspace::new();
    let other = Workspace::new();
    let tool = other.tool(20);
    let task = zeroclaw_spawn::spawn!(async move {
        tool.execute(json!({"command": "echo ready > ready; sleep 5; echo kept > result"}))
            .await
    });
    other.wait_for_file("ready").await;

    let result = timed_out
        .tool(1)
        .execute(json!({"command": "sleep 75"}))
        .await
        .unwrap();
    assert!(!result.success);
    assert!(timed_out.containers().is_empty());
    assert_eq!(
        other.containers().len(),
        1,
        "cleanup must target only its own container"
    );
    assert!(task.await.unwrap().unwrap().success);
    let result = other
        .tool(3)
        .execute(json!({"command": "cat result"}))
        .await
        .unwrap();
    assert!(result.success);
    assert_eq!(result.output.trim(), "kept");
    assert!(other.containers().is_empty());
}
