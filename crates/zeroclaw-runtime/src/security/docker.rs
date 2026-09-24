//! Docker sandbox (container isolation)

use crate::security::traits::{Sandbox, SandboxShellProgram};
use std::ffi::{OsStr, OsString};
use std::path::{Path, PathBuf};
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
            // Read from the config crate so the sandbox default and the
            // `[security.sandbox].image` serde default cannot drift apart.
            image: zeroclaw_config::schema::DEFAULT_SANDBOX_IMAGE.to_string(),
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

    fn wrap_command_with_inner_program(
        &self,
        cmd: &mut Command,
        inner_program: &std::ffi::OsStr,
    ) -> std::io::Result<()> {
        let program = inner_program.to_string_lossy().to_string();
        let args: Vec<String> = cmd
            .get_args()
            .map(|s| s.to_string_lossy().to_string())
            .collect();
        let launcher = which::which("docker")
            .ok()
            .and_then(|path| path.canonicalize().ok())
            .unwrap_or_else(|| "docker".into());
        let mut docker_cmd = Command::new(launcher);
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

    fn pinned_image_id(&self, launcher: &Path) -> std::io::Result<String> {
        let output = Command::new(launcher)
            .arg("image")
            .arg("inspect")
            .arg("--format={{.Id}}")
            .arg(&self.image)
            .output()?;
        if !output.status.success() {
            return Err(std::io::Error::other(format!(
                "Docker sandbox image inspection failed with status {}",
                output.status
            )));
        }
        parse_docker_image_id(&output.stdout)
    }
}

fn parse_docker_image_id(stdout: &[u8]) -> std::io::Result<String> {
    let image_id = std::str::from_utf8(stdout)
        .map_err(|_| std::io::Error::other("Docker sandbox image ID was not valid UTF-8"))?
        .trim();
    let digest = image_id.strip_prefix("sha256:").ok_or_else(|| {
        std::io::Error::other("Docker sandbox image inspection returned a non-content-addressed ID")
    })?;
    if digest.len() != 64 || !digest.bytes().all(|byte| byte.is_ascii_hexdigit()) {
        return Err(std::io::Error::other(
            "Docker sandbox image inspection returned an invalid image ID",
        ));
    }
    Ok(format!("sha256:{}", digest.to_ascii_lowercase()))
}

impl Sandbox for DockerSandbox {
    fn wrap_command(&self, cmd: &mut Command) -> std::io::Result<()> {
        let program = cmd.get_program().to_os_string();
        self.wrap_command_with_inner_program(cmd, &program)
    }

    fn wrap_shell_command(
        &self,
        cmd: &mut Command,
        original_program: &std::ffi::OsStr,
    ) -> std::io::Result<SandboxShellProgram> {
        self.wrap_command_with_inner_program(cmd, original_program)?;
        Ok(SandboxShellProgram::Isolated {
            program: original_program.to_os_string(),
            mutable_mount: self.workspace_dir.is_some(),
        })
    }

    fn is_available(&self) -> bool {
        Self::is_installed()
    }

    fn execution_fingerprint_material(
        &self,
        _launch_program: &std::path::Path,
    ) -> std::io::Result<Vec<u8>> {
        let mut material =
            format!("sandbox-policy-v1:docker:image={}:workspace=", self.image).into_bytes();
        if let Some(workspace) = &self.workspace_dir {
            append_path_material(&mut material, workspace);
        } else {
            material.extend_from_slice(b"none");
        }
        Ok(material)
    }

    fn pin_shell_command(
        &self,
        command: &mut Command,
        resolved_launcher: &Path,
    ) -> std::io::Result<()> {
        let image_id = self.pinned_image_id(resolved_launcher)?;
        let mut args: Vec<OsString> = command.get_args().map(OsStr::to_os_string).collect();
        let image_index = if self.workspace_dir.is_some() { 12 } else { 8 };
        let image = args.get_mut(image_index).ok_or_else(|| {
            std::io::Error::other("Docker sandbox launch omitted its image argument")
        })?;
        if image != OsStr::new(&self.image) {
            return Err(std::io::Error::other(
                "Docker sandbox launch image did not match its canonical configuration",
            ));
        }
        *image = image_id.into();
        let mut pinned = Command::new(resolved_launcher);
        pinned.args(args);
        *command = pinned;
        Ok(())
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

fn append_path_material(material: &mut Vec<u8>, path: &std::path::Path) {
    #[cfg(unix)]
    {
        use std::os::unix::ffi::OsStrExt;
        material.extend_from_slice(path.as_os_str().as_bytes());
    }
    #[cfg(windows)]
    {
        use std::os::windows::ffi::OsStrExt;
        for unit in path.as_os_str().encode_wide() {
            material.extend_from_slice(&unit.to_be_bytes());
        }
    }
    #[cfg(not(any(unix, windows)))]
    material.extend_from_slice(path.to_string_lossy().as_bytes());
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn docker_sandbox_image_id_parser_requires_a_content_address() {
        let uppercase = format!("sha256:{}\n", "C".repeat(64));
        assert_eq!(
            parse_docker_image_id(uppercase.as_bytes()).unwrap(),
            format!("sha256:{}", "c".repeat(64))
        );
        assert!(parse_docker_image_id(b"ubuntu:latest\n").is_err());
        assert!(parse_docker_image_id(b"sha256:abcd\n").is_err());
    }

    #[cfg(unix)]
    #[test]
    fn docker_sandbox_pins_the_launch_to_the_inspected_image_id() {
        use std::os::unix::fs::PermissionsExt;

        let temp = tempfile::tempdir().unwrap();
        let launcher = temp.path().join("docker");
        std::fs::write(
            &launcher,
            format!("#!/bin/sh\nprintf 'sha256:{}\\n'\n", "d".repeat(64)),
        )
        .unwrap();
        std::fs::set_permissions(&launcher, std::fs::Permissions::from_mode(0o755)).unwrap();

        let sandbox = DockerSandbox::default();
        let mut command = Command::new("sh");
        command.args(["-c", "echo ok"]);
        sandbox
            .wrap_shell_command(&mut command, OsStr::new("sh"))
            .unwrap();
        sandbox.pin_shell_command(&mut command, &launcher).unwrap();
        let args: Vec<_> = command.get_args().collect();
        assert_eq!(
            args.get(8),
            Some(&OsStr::new(&format!("sha256:{}", "d".repeat(64))))
        );
        assert_eq!(command.get_program(), launcher.as_os_str());
    }

    #[test]
    fn docker_sandbox_name() {
        let sandbox = DockerSandbox::default();
        assert_eq!(sandbox.name(), "docker");
    }

    #[test]
    fn docker_sandbox_default_image_tracks_the_config_default() {
        // Asserting against the constant rather than a literal is the point:
        // the sandbox default and `[security.sandbox].image` now have one
        // source, and this fails if someone reintroduces a second one.
        let sandbox = DockerSandbox::default();
        assert_eq!(
            sandbox.image,
            zeroclaw_config::schema::DEFAULT_SANDBOX_IMAGE
        );
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
            std::path::Path::new(cmd.get_program()).file_name(),
            Some(std::ffi::OsStr::new(&format!(
                "docker{}",
                std::env::consts::EXE_SUFFIX
            ))),
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
    fn docker_shell_wrap_preserves_container_program_spelling() {
        let sandbox = DockerSandbox::default();
        let mut cmd = Command::new("/host/usr/bin/dash");
        cmd.args(["-c", "echo hello"]);

        sandbox
            .wrap_shell_command(&mut cmd, std::ffi::OsStr::new("sh"))
            .unwrap();

        let args: Vec<String> = cmd
            .get_args()
            .map(|arg| arg.to_string_lossy().into_owned())
            .collect();
        let image_index = args
            .iter()
            .position(|arg| arg == "alpine:latest")
            .expect("docker image argument");
        assert_eq!(args.get(image_index + 1).map(String::as_str), Some("sh"));
        assert!(!args.iter().any(|arg| arg == "/host/usr/bin/dash"));
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
