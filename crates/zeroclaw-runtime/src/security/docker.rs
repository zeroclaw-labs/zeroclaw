//! Docker sandbox (container isolation)

use crate::security::traits::Sandbox;
use std::path::PathBuf;
use std::process::Command;

/// Docker sandbox backend
#[derive(Debug, Clone)]
pub struct DockerSandbox {
    image: String,
    workspace_dir: Option<PathBuf>,
}

impl Default for DockerSandbox {
    fn default() -> Self {
        Self {
            image: "alpine:latest".to_string(),
            workspace_dir: None,
        }
    }
}

impl DockerSandbox {
    /// Default container image used when no explicit image is configured.
    /// Exposed so callers constructing via with_workspace() without a custom
    /// image don't duplicate the default-image string.
    pub fn default_image() -> String {
        Self::default().image
    }

    /// Construct a Docker sandbox with a workspace bind-mount (read-only).
    /// Used by Python/R/Julia skills that need to access script files from
    /// the workspace inside the container.
    pub fn with_workspace(image: String, workspace_dir: PathBuf) -> std::io::Result<Self> {
        if Self::is_installed() {
            Ok(Self {
                image,
                workspace_dir: Some(workspace_dir),
            })
        } else {
            Err(std::io::Error::new(
                std::io::ErrorKind::NotFound,
                "Docker not found",
            ))
        }
    }

    pub fn new() -> std::io::Result<Self> {
        if Self::is_installed() {
            Ok(Self::default())
        } else {
            Err(std::io::Error::new(
                std::io::ErrorKind::NotFound,
                "Docker not found",
            ))
        }
    }

    pub fn with_image(image: String) -> std::io::Result<Self> {
        if Self::is_installed() {
            Ok(Self {
                image,
                workspace_dir: None,
            })
        } else {
            Err(std::io::Error::new(
                std::io::ErrorKind::NotFound,
                "Docker not found",
            ))
        }
    }

    pub fn probe() -> std::io::Result<Self> {
        Self::new()
    }

    fn is_installed() -> bool {
        Command::new("docker")
            .arg("--version")
            .output()
            .map(|o| o.status.success())
            .unwrap_or(false)
    }
}

impl Sandbox for DockerSandbox {
    fn wrap_command(&self, cmd: &mut Command) -> std::io::Result<()> {
        let program = cmd.get_program().to_string_lossy().to_string();
        let args: Vec<String> = cmd
            .get_args()
            .map(|s| s.to_string_lossy().to_string())
            .collect();

        let mut docker_cmd = Command::new("docker");
        docker_cmd.args([
            "run",
            "--rm",
            "--memory",
            "512m",
            "--cpus",
            "1.0",
            "--network",
            "none",
        ]);

        // Read-only workspace bind-mount. Same path inside and outside the
        // container so workspace-relative paths resolve identically in both.
        // --workdir sets the container's CWD to the workspace, so relative-path
        // script invocations (`python3 script.py`) and CWD-relative I/O
        // (`open("relative_file.txt")`) resolve correctly inside the sandbox
        // without callers having to fully-qualify every path.
        if let Some(workspace) = &self.workspace_dir {
            let workspace_str = workspace.to_string_lossy();
            docker_cmd.arg("-v");
            docker_cmd.arg(format!("{workspace_str}:{workspace_str}:ro"));
            docker_cmd.arg("--workdir");
            docker_cmd.arg(workspace_str.as_ref());
        }

        docker_cmd.arg(&self.image);
        docker_cmd.arg(&program);
        docker_cmd.args(&args);

        *cmd = docker_cmd;
        Ok(())
    }

    fn is_available(&self) -> bool {
        Self::is_installed()
    }

    fn name(&self) -> &str {
        "docker"
    }

    fn description(&self) -> &str {
        "Docker container isolation (requires docker)"
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn docker_sandbox_name() {
        let sandbox = DockerSandbox::default();
        assert_eq!(sandbox.name(), "docker");
    }

    #[test]
    fn docker_sandbox_default_image() {
        let sandbox = DockerSandbox::default();
        assert_eq!(sandbox.image, "alpine:latest");
    }

    #[test]
    fn docker_with_custom_image() {
        let result = DockerSandbox::with_image("ubuntu:latest".to_string());
        match result {
            Ok(sandbox) => assert_eq!(sandbox.image, "ubuntu:latest"),
            Err(_) => assert!(!DockerSandbox::is_installed()),
        }
    }

    // ── §1.1 Sandbox isolation flag tests ──────────────────────

    #[test]
    fn docker_wrap_command_includes_isolation_flags() {
        let sandbox = DockerSandbox::default();
        let mut cmd = Command::new("echo");
        cmd.arg("hello");
        sandbox.wrap_command(&mut cmd).unwrap();

        assert_eq!(
            cmd.get_program().to_string_lossy(),
            "docker",
            "wrapped command should use docker as program"
        );

        let args: Vec<String> = cmd
            .get_args()
            .map(|s| s.to_string_lossy().to_string())
            .collect();

        assert!(
            args.contains(&"run".to_string()),
            "must include 'run' subcommand"
        );
        assert!(
            args.contains(&"--rm".to_string()),
            "must include --rm for auto-cleanup"
        );
        assert!(
            args.contains(&"--network".to_string()),
            "must include --network flag"
        );
        assert!(
            args.contains(&"none".to_string()),
            "network must be set to 'none' for isolation"
        );
        assert!(
            args.contains(&"--memory".to_string()),
            "must include --memory limit"
        );
        assert!(
            args.contains(&"512m".to_string()),
            "memory limit must be 512m"
        );
        assert!(
            args.contains(&"--cpus".to_string()),
            "must include --cpus limit"
        );
        assert!(args.contains(&"1.0".to_string()), "CPU limit must be 1.0");
    }

    #[test]
    fn docker_wrap_command_preserves_original_command() {
        let sandbox = DockerSandbox::default();
        let mut cmd = Command::new("ls");
        cmd.arg("-la");
        sandbox.wrap_command(&mut cmd).unwrap();

        let args: Vec<String> = cmd
            .get_args()
            .map(|s| s.to_string_lossy().to_string())
            .collect();

        assert!(
            args.contains(&"alpine:latest".to_string()),
            "must include the container image"
        );
        assert!(
            args.contains(&"ls".to_string()),
            "original program must be passed as argument"
        );
        assert!(
            args.contains(&"-la".to_string()),
            "original args must be preserved"
        );
    }

    #[test]
    fn docker_wrap_command_uses_custom_image() {
        let sandbox = DockerSandbox {
            image: "ubuntu:22.04".to_string(),
            workspace_dir: None,
        };
        let mut cmd = Command::new("echo");
        sandbox.wrap_command(&mut cmd).unwrap();

        let args: Vec<String> = cmd
            .get_args()
            .map(|s| s.to_string_lossy().to_string())
            .collect();

        assert!(
            args.contains(&"ubuntu:22.04".to_string()),
            "must use the custom image"
        );
    }

    #[test]
    fn docker_with_workspace() {
        let ws_path = std::path::PathBuf::from("/tmp/test-workspace-12345");
        // Can't guarantee docker is installed in tests; just verify the
        // struct shape round-trips if construction were to succeed.
        let sandbox = DockerSandbox {
            image: "alpine:latest".to_string(),
            workspace_dir: Some(ws_path.clone()),
        };
        assert_eq!(sandbox.workspace_dir, Some(ws_path));
    }

    #[test]
    fn docker_without_workspace() {
        let sandbox = DockerSandbox::default();
        assert_eq!(sandbox.workspace_dir, None);
    }

    #[test]
    fn docker_wrap_command_emits_bind_mount_when_workspace_configured() {
        let ws = std::path::PathBuf::from("/workspace/skills");
        let sandbox = DockerSandbox {
            image: "alpine:latest".to_string(),
            workspace_dir: Some(ws.clone()),
        };
        let mut cmd = Command::new("python3");
        cmd.arg("script.py");
        sandbox.wrap_command(&mut cmd).unwrap();

        let args: Vec<String> = cmd
            .get_args()
            .map(|s| s.to_string_lossy().to_string())
            .collect();

        assert!(
            args.contains(&"-v".to_string()),
            "must include -v bind-mount flag when workspace is configured"
        );
        let ws_str = ws.to_string_lossy();
        let expected = format!("{ws_str}:{ws_str}:ro");
        assert!(
            args.contains(&expected),
            "bind-mount spec must match host-path:container-path:ro form; args={args:?}"
        );
        // --workdir must be set to the workspace so relative-path script
        // invocations resolve correctly inside the sandbox.
        assert!(
            args.contains(&"--workdir".to_string()),
            "must include --workdir flag when workspace is configured; args={args:?}"
        );
        assert!(
            args.contains(&ws_str.to_string()),
            "--workdir value must equal the workspace path; args={args:?}"
        );
    }

    #[test]
    fn docker_wrap_command_omits_bind_mount_when_no_workspace() {
        let sandbox = DockerSandbox::default();
        let mut cmd = Command::new("echo");
        sandbox.wrap_command(&mut cmd).unwrap();

        let args: Vec<String> = cmd
            .get_args()
            .map(|s| s.to_string_lossy().to_string())
            .collect();

        assert!(
            !args.contains(&"-v".to_string()),
            "must not emit -v when workspace_dir is None"
        );
    }
}
