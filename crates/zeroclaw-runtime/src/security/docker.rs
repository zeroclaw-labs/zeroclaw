//! Docker sandbox (container isolation)

use crate::security::traits::Sandbox;
use std::ffi::OsStr;
use std::path::PathBuf;
use std::process::Command;
use zeroclaw_config::platform::resolve_executable;

/// Docker sandbox backend
#[derive(Debug, Clone)]
pub struct DockerSandbox {
    launcher: Option<PathBuf>,
    image: String,
    workspace_dir: Option<PathBuf>,
}

impl Default for DockerSandbox {
    fn default() -> Self {
        Self {
            launcher: None,
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
        "alpine:latest".to_string()
    }

    /// Construct a Docker sandbox with a workspace bind-mount (read-only).
    /// Used by Python/R/Julia skills that need to access script files from
    /// the workspace inside the container.
    pub fn with_workspace(image: String, workspace_dir: PathBuf) -> std::io::Result<Self> {
        Self::with_resolved_launcher(image, Some(workspace_dir))
    }

    pub fn new() -> std::io::Result<Self> {
        Self::with_resolved_launcher(Self::default_image(), None)
    }

    pub fn with_image(image: String) -> std::io::Result<Self> {
        Self::with_resolved_launcher(image, None)
    }

    pub fn probe() -> std::io::Result<Self> {
        Self::new()
    }

    fn with_resolved_launcher(
        image: String,
        workspace_dir: Option<PathBuf>,
    ) -> std::io::Result<Self> {
        let launcher = resolve_executable(OsStr::new("docker")).map_err(|error| {
            std::io::Error::new(
                error.kind(),
                format!("Docker launcher could not be resolved: {error}"),
            )
        })?;
        let output = Command::new(&launcher)
            .arg("--version")
            .output()
            .map_err(|error| {
                std::io::Error::new(
                    error.kind(),
                    format!(
                        "Docker launcher at {} could not be probed: {error}",
                        launcher.display()
                    ),
                )
            })?;
        if !output.status.success() {
            return Err(std::io::Error::new(
                std::io::ErrorKind::NotFound,
                format!("Docker launcher at {} is unavailable", launcher.display()),
            ));
        }
        Ok(Self {
            launcher: Some(launcher),
            image,
            workspace_dir,
        })
    }

    fn resolve_launcher(&self) -> std::io::Result<PathBuf> {
        if let Some(launcher) = &self.launcher {
            return Ok(launcher.clone());
        }

        resolve_executable(OsStr::new("docker")).map_err(|error| {
            std::io::Error::new(
                error.kind(),
                format!("Docker launcher could not be resolved: {error}"),
            )
        })
    }

    #[cfg(test)]
    fn for_test() -> Self {
        Self {
            launcher: Some(PathBuf::from("docker")),
            image: "alpine:latest".to_string(),
            workspace_dir: None,
        }
    }
}

impl Sandbox for DockerSandbox {
    fn wrap_command(&self, cmd: &mut Command) -> std::io::Result<()> {
        let launcher = self.resolve_launcher()?;
        let program = cmd.get_program().to_string_lossy().to_string();
        let args: Vec<String> = cmd
            .get_args()
            .map(|s| s.to_string_lossy().to_string())
            .collect();

        let mut docker_cmd = Command::new(&launcher);
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
        let launcher = match self.resolve_launcher() {
            Ok(launcher) => launcher,
            Err(_) => return false,
        };
        Command::new(launcher)
            .arg("--version")
            .output()
            .map(|output| output.status.success())
            .unwrap_or(false)
    }

    fn name(&self) -> &str {
        "docker"
    }

    fn description(&self) -> &str {
        "Docker container isolation (requires docker)"
    }

    fn coding_cli_unsupported_reason(&self) -> Option<&'static str> {
        Some(
            "docker sandbox mounts the workspace read-only, fixes the inner workdir at the workspace root, and cannot forward selected coding CLI environment names",
        )
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
        assert!(sandbox.launcher.is_none());
        assert_eq!(sandbox.image, "alpine:latest");
    }

    #[test]
    fn docker_with_custom_image() {
        let result = DockerSandbox::with_image("ubuntu:latest".to_string());
        match result {
            Ok(sandbox) => assert_eq!(sandbox.image, "ubuntu:latest"),
            Err(_) => assert!(!DockerSandbox::default().is_available()),
        }
    }

    // ── §1.1 Sandbox isolation flag tests ──────────────────────

    #[test]
    fn docker_wrap_command_includes_isolation_flags() {
        let sandbox = DockerSandbox::for_test();
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
        let sandbox = DockerSandbox::for_test();
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
            launcher: Some(PathBuf::from("docker")),
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
            launcher: Some(PathBuf::from("docker")),
            image: "alpine:latest".to_string(),
            workspace_dir: Some(ws_path.clone()),
        };
        assert_eq!(sandbox.workspace_dir, Some(ws_path));
    }

    #[cfg(unix)]
    #[test]
    fn docker_wrap_uses_the_resolved_launcher_identity() {
        use std::os::unix::fs::PermissionsExt;

        let dir = tempfile::tempdir().unwrap();
        let launcher = dir.path().join("docker");
        std::fs::write(&launcher, "#!/bin/sh\nexit 0\n").unwrap();
        std::fs::set_permissions(&launcher, std::fs::Permissions::from_mode(0o755)).unwrap();
        let launcher = launcher.canonicalize().unwrap();
        let sandbox = DockerSandbox {
            launcher: Some(launcher.clone()),
            image: "alpine:latest".to_string(),
            workspace_dir: None,
        };

        assert!(sandbox.is_available());
        let mut command = Command::new("echo");
        sandbox.wrap_command(&mut command).unwrap();
        assert_eq!(command.get_program(), launcher.as_os_str());
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
            launcher: Some(PathBuf::from("docker")),
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
        let sandbox = DockerSandbox::for_test();
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

    #[test]
    fn docker_sandbox_rejects_coding_cli_execution() {
        let sandbox = DockerSandbox::default();
        let reason = sandbox
            .coding_cli_unsupported_reason()
            .expect("docker sandbox must fail closed for coding CLIs");

        assert!(reason.contains("read-only"));
        assert!(reason.contains("workdir"));
        assert!(reason.contains("environment"));
    }
}
