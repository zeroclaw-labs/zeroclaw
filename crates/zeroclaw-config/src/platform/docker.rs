use crate::schema::DockerRuntimeConfig;
use anyhow::{Context, Result};
use std::path::{Path, PathBuf};
use zeroclaw_api::runtime_traits::RuntimeAdapter;

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
}

impl RuntimeAdapter for DockerRuntime {
    fn name(&self) -> &str {
        "docker"
    }

    fn has_shell_access(&self) -> bool {
        true
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

    fn build_shell_command(
        &self,
        command: &str,
        workspace_dir: &Path,
    ) -> anyhow::Result<tokio::process::Command> {
        let mut process = tokio::process::Command::new("docker");
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
}

#[cfg(test)]
mod tests {
    use super::*;

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
