use crate::schema::DockerRuntimeConfig;
use anyhow::{Context, Result};
use std::ffi::{OsStr, OsString};
use std::path::{Path, PathBuf};
use std::process::Command;
use zeroclaw_api::runtime_traits::{RuntimeAdapter, ShellDialect, ShellExecutionDomain};

/// Canonicalization failures that the runtime layer can present through
/// localized tool diagnostics without parsing an English error chain.
#[derive(Debug, thiserror::Error)]
pub enum DockerWorkspaceMountError {
    #[error("Failed to canonicalize Docker workspace path {path}")]
    WorkspacePath {
        path: String,
        #[source]
        source: std::io::Error,
    },

    #[error("Failed to canonicalize Docker workspace root {path}")]
    AllowedRoot {
        path: String,
        #[source]
        source: std::io::Error,
    },
}

/// Docker runtime with lightweight container isolation.
#[derive(Debug, Clone)]
pub struct DockerRuntime {
    config: DockerRuntimeConfig,
}

impl DockerRuntime {
    pub fn new(config: DockerRuntimeConfig) -> Self {
        Self { config }
    }

    fn workspace_mount_path(&self, workspace_dir: &Path) -> Result<PathBuf> {
        let resolved = workspace_dir.canonicalize().map_err(|source| {
            DockerWorkspaceMountError::WorkspacePath {
                path: workspace_dir.display().to_string(),
                source,
            }
        })?;

        if !resolved.is_absolute() {
            anyhow::bail!(
                "Docker runtime requires an absolute workspace path, got: {}",
                resolved.display()
            );
        }

        if resolved == Path::new("/") {
            anyhow::bail!("Refusing to mount filesystem root (/) into docker runtime");
        }

        if self.config.allowed_workspace_roots.is_empty() {
            return Ok(resolved);
        }

        let allowed_roots = self
            .config
            .allowed_workspace_roots
            .iter()
            .map(|root| {
                Path::new(root).canonicalize().map_err(|source| {
                    DockerWorkspaceMountError::AllowedRoot {
                        path: root.clone(),
                        source,
                    }
                })
            })
            .collect::<std::result::Result<Vec<_>, _>>()?;
        let allowed = allowed_roots.iter().any(|root| resolved.starts_with(root));

        if !allowed {
            anyhow::bail!(
                "Workspace path {} is not in runtime.docker.allowed_workspace_roots",
                resolved.display()
            );
        }

        Ok(resolved)
    }

    fn build_shell_command_inner(
        &self,
        command: &str,
        workspace_dir: &Path,
        env_keys: &[&OsStr],
        program: &Path,
    ) -> anyhow::Result<tokio::process::Command> {
        let mut process = tokio::process::Command::new(program);
        process
            .arg("run")
            .arg("--rm")
            .arg("--init")
            .arg("--interactive");

        let network = self.config.network.trim();
        if !network.is_empty() {
            process.arg("--network").arg(network);
        }

        if let Some(memory_limit_mb) = self.config.memory_limit_mb.filter(|mb| *mb > 0) {
            process.arg("--memory").arg(format!("{memory_limit_mb}m"));
        }

        if let Some(cpu_limit) = self.config.cpu_limit.filter(|cpus| *cpus > 0.0) {
            process.arg("--cpus").arg(cpu_limit.to_string());
        }

        if self.config.read_only_rootfs {
            process.arg("--read-only");
        }

        for key in env_keys {
            let key = docker_env_key(key)?;
            process.arg("--env").arg(key);
        }

        if self.config.mount_workspace {
            let host_workspace = self.workspace_mount_path(workspace_dir).with_context(|| {
                format!(
                    "Failed to validate workspace mount path {}",
                    workspace_dir.display()
                )
            })?;

            process
                .arg("--volume")
                .arg(format!("{}:/workspace:rw", host_workspace.display()))
                .arg("--workdir")
                .arg("/workspace");
        }

        process
            .arg(self.config.image.trim())
            .arg("sh")
            .arg("-c")
            .arg(command);

        Ok(process)
    }

    fn pinned_image_id(&self, launcher: &Path) -> Result<String> {
        let output = Command::new(launcher)
            .arg("image")
            .arg("inspect")
            .arg("--format={{.Id}}")
            .arg(self.config.image.trim())
            .output()
            .with_context(|| {
                format!(
                    "Failed to inspect Docker runtime image with {}",
                    launcher.display()
                )
            })?;
        if !output.status.success() {
            anyhow::bail!(
                "Docker runtime image inspection failed with status {}",
                output.status
            );
        }
        parse_docker_image_id(&output.stdout)
    }
}

fn parse_docker_image_id(stdout: &[u8]) -> Result<String> {
    let image_id = std::str::from_utf8(stdout)
        .context("Docker runtime image ID was not valid UTF-8")?
        .trim();
    let digest = image_id
        .strip_prefix("sha256:")
        .context("Docker runtime image inspection returned a non-content-addressed ID")?;
    if digest.len() != 64 || !digest.bytes().all(|byte| byte.is_ascii_hexdigit()) {
        anyhow::bail!("Docker runtime image inspection returned an invalid image ID");
    }
    Ok(format!("sha256:{}", digest.to_ascii_lowercase()))
}

fn docker_env_key(key: &OsStr) -> Result<&str> {
    let key = key
        .to_str()
        .context("Docker runtime environment passthrough key must be valid UTF-8")?;
    if key.is_empty() || key.contains('=') {
        anyhow::bail!("Docker runtime environment passthrough key must be a variable name");
    }
    Ok(key)
}

impl RuntimeAdapter for DockerRuntime {
    fn name(&self) -> &str {
        "docker"
    }

    fn has_filesystem_access(&self) -> bool {
        self.config.mount_workspace
    }

    fn storage_path(&self) -> PathBuf {
        if self.config.mount_workspace {
            PathBuf::from("/workspace/.zeroclaw")
        } else {
            PathBuf::from("/tmp/.zeroclaw")
        }
    }

    fn supports_long_running(&self) -> bool {
        false
    }

    fn memory_budget(&self) -> u64 {
        self.config
            .memory_limit_mb
            .map_or(0, |mb| mb.saturating_mul(1024 * 1024))
    }

    fn shell_dialect(&self) -> ShellDialect {
        ShellDialect::Posix
    }

    fn shell_execution_domain(&self) -> ShellExecutionDomain {
        ShellExecutionDomain::Isolated {
            name: "docker-runtime",
            mutable_mount: self.config.mount_workspace,
        }
    }

    fn build_shell_command(
        &self,
        command: &str,
        workspace_dir: &Path,
    ) -> anyhow::Result<tokio::process::Command> {
        self.build_shell_command_inner(command, workspace_dir, &[], Path::new("docker"))
    }

    fn build_shell_command_with_program(
        &self,
        command: &str,
        workspace_dir: &Path,
        program: &Path,
    ) -> anyhow::Result<tokio::process::Command> {
        self.build_shell_command_inner(command, workspace_dir, &[], program)
    }

    fn pin_shell_command(
        &self,
        command: &mut Command,
        resolved_launcher: &Path,
        _workspace_dir: &Path,
    ) -> anyhow::Result<()> {
        let image_id = self.pinned_image_id(resolved_launcher)?;
        let mut args: Vec<_> = command.get_args().map(OsStr::to_os_string).collect();
        let image_index = args
            .len()
            .checked_sub(4)
            .context("Docker runtime launch omitted its image argument")?;
        if args.get(image_index).map(OsString::as_os_str)
            != Some(OsStr::new(self.config.image.trim()))
        {
            anyhow::bail!("Docker runtime launch image did not match its canonical configuration");
        }
        args[image_index] = image_id.into();
        let mut pinned = Command::new(resolved_launcher);
        pinned.args(args);
        *command = pinned;
        Ok(())
    }

    fn build_shell_command_with_env_keys(
        &self,
        command: &str,
        workspace_dir: &Path,
        env_keys: &[&OsStr],
    ) -> anyhow::Result<tokio::process::Command> {
        self.build_shell_command_inner(command, workspace_dir, env_keys, Path::new("docker"))
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn docker_image_id_parser_requires_a_content_address() {
        let uppercase = format!("sha256:{}\n", "A".repeat(64));
        assert_eq!(
            parse_docker_image_id(uppercase.as_bytes()).unwrap(),
            format!("sha256:{}", "a".repeat(64))
        );
        assert!(parse_docker_image_id(b"alpine:latest\n").is_err());
        assert!(parse_docker_image_id(b"sha256:1234\n").is_err());
    }

    #[cfg(unix)]
    #[test]
    fn docker_runtime_pins_the_launch_to_the_inspected_image_id() {
        use std::os::unix::fs::PermissionsExt;

        let temp = tempfile::tempdir().unwrap();
        let launcher = temp.path().join("docker");
        std::fs::write(
            &launcher,
            format!("#!/bin/sh\nprintf 'sha256:{}\\n'\n", "b".repeat(64)),
        )
        .unwrap();
        std::fs::set_permissions(&launcher, std::fs::Permissions::from_mode(0o755)).unwrap();

        let runtime = DockerRuntime::new(DockerRuntimeConfig::default());
        let mut command = runtime
            .build_shell_command("echo ok", temp.path())
            .unwrap()
            .into_std();
        runtime
            .pin_shell_command(&mut command, &launcher, temp.path())
            .unwrap();
        let args: Vec<_> = command.get_args().collect();
        assert_eq!(
            args.get(args.len() - 4),
            Some(&OsStr::new(&format!("sha256:{}", "b".repeat(64))))
        );
        assert_eq!(command.get_program(), launcher.as_os_str());
    }

    #[test]
    fn docker_runtime_name() {
        let runtime = DockerRuntime::new(DockerRuntimeConfig::default());
        assert_eq!(runtime.name(), "docker");
    }

    #[test]
    fn docker_runtime_memory_budget() {
        let cfg = DockerRuntimeConfig {
            memory_limit_mb: Some(256),
            ..Default::default()
        };
        let runtime = DockerRuntime::new(cfg);
        assert_eq!(runtime.memory_budget(), 256 * 1024 * 1024);
    }

    #[test]
    fn docker_reports_posix_shell_dialect() {
        let runtime = DockerRuntime::new(DockerRuntimeConfig::default());
        assert_eq!(runtime.shell_dialect(), ShellDialect::Posix);
    }

    #[test]
    fn docker_build_shell_command_includes_runtime_flags() {
        let cfg = DockerRuntimeConfig {
            image: "alpine:3.20".into(),
            network: "none".into(),
            memory_limit_mb: Some(128),
            cpu_limit: Some(1.5),
            read_only_rootfs: true,
            mount_workspace: true,
            allowed_workspace_roots: Vec::new(),
        };
        let runtime = DockerRuntime::new(cfg);

        let workspace = std::env::temp_dir();
        let command = runtime
            .build_shell_command("echo hello", &workspace)
            .unwrap();
        let debug = format!("{command:?}");

        assert!(debug.contains("docker"));
        assert!(debug.contains("--memory"));
        assert!(debug.contains("128m"));
        assert!(debug.contains("--cpus"));
        assert!(debug.contains("1.5"));
        assert!(debug.contains("--workdir"));
        assert!(debug.contains("echo hello"));
    }

    #[test]
    fn docker_build_shell_command_forwards_env_keys_without_values() {
        let cfg = DockerRuntimeConfig {
            image: "alpine:3.20".into(),
            mount_workspace: false,
            ..DockerRuntimeConfig::default()
        };
        let runtime = DockerRuntime::new(cfg);
        let secret_value = "secret-value-should-not-appear-in-docker-args";

        let command = runtime
            .build_shell_command_with_env_keys(
                "printf '%s' \"$ZC_CLI_TOKEN\"",
                &std::env::temp_dir(),
                &[
                    std::ffi::OsStr::new("ZC_CLI_TOKEN"),
                    std::ffi::OsStr::new("OPENAI_API_KEY"),
                ],
            )
            .unwrap();
        let debug = format!("{command:?}");

        assert!(debug.contains("--env"));
        assert!(debug.contains("ZC_CLI_TOKEN"));
        assert!(debug.contains("OPENAI_API_KEY"));
        assert!(!debug.contains("ZC_CLI_TOKEN="));
        assert!(!debug.contains("OPENAI_API_KEY="));
        assert!(!debug.contains(secret_value));
    }

    #[test]
    fn docker_build_shell_command_rejects_env_key_values() {
        let runtime = DockerRuntime::new(DockerRuntimeConfig {
            mount_workspace: false,
            ..DockerRuntimeConfig::default()
        });

        let result = runtime.build_shell_command_with_env_keys(
            "echo hello",
            &std::env::temp_dir(),
            &[std::ffi::OsStr::new("ZC_CLI_TOKEN=secret")],
        );

        assert!(result.is_err());
        assert!(result.unwrap_err().to_string().contains("variable name"));
    }

    #[test]
    fn docker_workspace_allowlist_blocks_outside_paths() {
        let allowed = tempfile::tempdir().unwrap();
        let outside = tempfile::tempdir().unwrap();
        let cfg = DockerRuntimeConfig {
            allowed_workspace_roots: vec![allowed.path().to_string_lossy().into_owned()],
            ..DockerRuntimeConfig::default()
        };
        let runtime = DockerRuntime::new(cfg);

        let err = runtime
            .build_shell_command("echo test", outside.path())
            .unwrap_err();
        let message = format!("{err:#}");

        assert!(
            message.contains("is not in runtime.docker.allowed_workspace_roots"),
            "unexpected error: {message}"
        );
    }

    #[test]
    fn docker_workspace_allowlist_rejects_missing_traversal_path() {
        let allowed = tempfile::tempdir().unwrap();
        let workspace = allowed
            .path()
            .join("missing")
            .join("..")
            .join("..")
            .join("escape");
        let cfg = DockerRuntimeConfig {
            allowed_workspace_roots: vec![allowed.path().to_string_lossy().into_owned()],
            ..DockerRuntimeConfig::default()
        };
        let runtime = DockerRuntime::new(cfg);

        let err = runtime
            .build_shell_command("echo test", &workspace)
            .unwrap_err();
        let message = format!("{err:#}");

        assert!(
            message.contains("Failed to canonicalize Docker workspace path"),
            "unexpected error: {message}"
        );
    }

    #[test]
    fn docker_workspace_allowlist_rejects_missing_configured_root() {
        let allowed = tempfile::tempdir().unwrap();
        let workspace = allowed.path().join("workspace");
        std::fs::create_dir(&workspace).unwrap();
        let missing_root = allowed.path().join("missing-root");
        let cfg = DockerRuntimeConfig {
            allowed_workspace_roots: vec![
                allowed.path().to_string_lossy().into_owned(),
                missing_root.to_string_lossy().into_owned(),
            ],
            ..DockerRuntimeConfig::default()
        };
        let runtime = DockerRuntime::new(cfg);

        let err = runtime
            .build_shell_command("echo test", &workspace)
            .unwrap_err();
        let message = format!("{err:#}");

        assert!(
            message.contains("Failed to canonicalize Docker workspace root"),
            "unexpected error: {message}"
        );
    }

    #[test]
    fn docker_workspace_allowlist_accepts_existing_path_under_root() {
        let allowed = tempfile::tempdir().unwrap();
        let workspace = allowed.path().join("workspace");
        std::fs::create_dir(&workspace).unwrap();
        let cfg = DockerRuntimeConfig {
            allowed_workspace_roots: vec![allowed.path().to_string_lossy().into_owned()],
            ..DockerRuntimeConfig::default()
        };
        let runtime = DockerRuntime::new(cfg);

        let command = runtime
            .build_shell_command("echo test", &workspace)
            .unwrap();
        let canonical_workspace = workspace.canonicalize().unwrap();
        let expected_mount = format!("{}:/workspace:rw", canonical_workspace.display());

        assert!(
            command
                .as_std()
                .get_args()
                .any(|arg| arg == std::ffi::OsStr::new(&expected_mount))
        );
    }

    // ── §3.3 / §3.4 Docker mount & network isolation tests ──

    #[test]
    fn docker_build_shell_command_includes_network_flag() {
        let cfg = DockerRuntimeConfig {
            network: "none".into(),
            ..DockerRuntimeConfig::default()
        };
        let runtime = DockerRuntime::new(cfg);
        let workspace = std::env::temp_dir();
        let cmd = runtime
            .build_shell_command("echo hello", &workspace)
            .unwrap();
        let debug = format!("{cmd:?}");
        assert!(
            debug.contains("--network") && debug.contains("none"),
            "must include --network none for isolation"
        );
    }

    #[test]
    fn docker_build_shell_command_includes_read_only_flag() {
        let cfg = DockerRuntimeConfig {
            read_only_rootfs: true,
            ..DockerRuntimeConfig::default()
        };
        let runtime = DockerRuntime::new(cfg);
        let workspace = std::env::temp_dir();
        let cmd = runtime
            .build_shell_command("echo hello", &workspace)
            .unwrap();
        let debug = format!("{cmd:?}");
        assert!(
            debug.contains("--read-only"),
            "must include --read-only flag when read_only_rootfs is set"
        );
    }

    #[cfg(unix)]
    #[test]
    fn docker_refuses_root_mount() {
        let cfg = DockerRuntimeConfig {
            mount_workspace: true,
            ..DockerRuntimeConfig::default()
        };
        let runtime = DockerRuntime::new(cfg);
        let result = runtime.build_shell_command("echo test", Path::new("/"));
        assert!(
            result.is_err(),
            "mounting filesystem root (/) must be refused"
        );
        let error_chain = format!("{:#}", result.unwrap_err());
        assert!(
            error_chain.contains("root"),
            "expected root-mount error chain, got: {error_chain}"
        );
    }

    #[test]
    fn docker_no_memory_flag_when_not_configured() {
        let cfg = DockerRuntimeConfig {
            memory_limit_mb: None,
            ..DockerRuntimeConfig::default()
        };
        let runtime = DockerRuntime::new(cfg);
        let workspace = std::env::temp_dir();
        let cmd = runtime
            .build_shell_command("echo hello", &workspace)
            .unwrap();
        let debug = format!("{cmd:?}");
        assert!(
            !debug.contains("--memory"),
            "should not include --memory when not configured"
        );
    }
}
