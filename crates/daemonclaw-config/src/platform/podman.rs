use crate::schema::PodmanRuntimeConfig;
use anyhow::{Context, Result};
use std::path::{Path, PathBuf};
use daemonclaw_api::runtime_traits::RuntimeAdapter;

/// Podman runtime — rootless, daemonless container isolation.
///
/// Calls `podman` directly instead of going through the Docker CLI or
/// podman-docker shim, avoiding SIGSYS failures under restrictive
/// systemd sandboxes.
#[derive(Debug, Clone)]
pub struct PodmanRuntime {
    config: PodmanRuntimeConfig,
}

impl PodmanRuntime {
    pub fn new(config: PodmanRuntimeConfig) -> Self {
        Self { config }
    }

    fn workspace_mount_path(&self, workspace_dir: &Path) -> Result<PathBuf> {
        let resolved = workspace_dir
            .canonicalize()
            .unwrap_or_else(|_| workspace_dir.to_path_buf());

        if !resolved.is_absolute() {
            anyhow::bail!(
                "Podman runtime requires an absolute workspace path, got: {}",
                resolved.display()
            );
        }

        if resolved == Path::new("/") {
            anyhow::bail!("Refusing to mount filesystem root (/) into podman runtime");
        }

        if self.config.allowed_workspace_roots.is_empty() {
            return Ok(resolved);
        }

        let allowed = self.config.allowed_workspace_roots.iter().any(|root| {
            let root_path = Path::new(root)
                .canonicalize()
                .unwrap_or_else(|_| PathBuf::from(root));
            resolved.starts_with(root_path)
        });

        if !allowed {
            anyhow::bail!(
                "Workspace path {} is not in runtime.podman.allowed_workspace_roots",
                resolved.display()
            );
        }

        Ok(resolved)
    }
}

impl RuntimeAdapter for PodmanRuntime {
    fn name(&self) -> &str {
        "podman"
    }

    fn has_shell_access(&self) -> bool {
        true
    }

    fn has_filesystem_access(&self) -> bool {
        self.config.mount_workspace
    }

    fn storage_path(&self) -> PathBuf {
        if self.config.mount_workspace {
            PathBuf::from("/workspace/.daemonclaw")
        } else {
            PathBuf::from("/tmp/.daemonclaw")
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
        let mut process = tokio::process::Command::new("podman");
        process
            .arg("run")
            .arg("--rm")
            .arg("--init")
            .arg("--interactive");

        // Rootless UID mapping
        process.arg("--userns").arg(&self.config.userns);

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

            let vol_suffix = if self.config.selinux_label { ":Z" } else { "" };
            process
                .arg("--volume")
                .arg(format!(
                    "{}:/workspace:rw{}",
                    host_workspace.display(),
                    vol_suffix
                ))
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
    fn podman_runtime_name() {
        let runtime = PodmanRuntime::new(PodmanRuntimeConfig::default());
        assert_eq!(runtime.name(), "podman");
    }

    #[test]
    fn podman_runtime_memory_budget() {
        let cfg = PodmanRuntimeConfig {
            memory_limit_mb: Some(256),
            ..Default::default()
        };
        let runtime = PodmanRuntime::new(cfg);
        assert_eq!(runtime.memory_budget(), 256 * 1024 * 1024);
    }

    #[test]
    fn podman_build_shell_command_includes_runtime_flags() {
        let cfg = PodmanRuntimeConfig {
            image: "ubuntu:24.04".into(),
            network: "none".into(),
            memory_limit_mb: Some(128),
            cpu_limit: Some(1.5),
            read_only_rootfs: true,
            mount_workspace: true,
            allowed_workspace_roots: Vec::new(),
            userns: "keep-id".into(),
            selinux_label: true,
        };
        let runtime = PodmanRuntime::new(cfg);

        let workspace = std::env::temp_dir();
        let command = runtime
            .build_shell_command("echo hello", &workspace)
            .unwrap();
        let debug = format!("{command:?}");

        assert!(debug.contains("podman"));
        assert!(debug.contains("--userns"));
        assert!(debug.contains("keep-id"));
        assert!(debug.contains("--memory"));
        assert!(debug.contains("128m"));
        assert!(debug.contains("--cpus"));
        assert!(debug.contains("1.5"));
        assert!(debug.contains("--read-only"));
        assert!(debug.contains("--workdir"));
        assert!(debug.contains(":Z"));
        assert!(debug.contains("echo hello"));
    }

    #[test]
    fn podman_build_shell_command_no_selinux_label() {
        let cfg = PodmanRuntimeConfig {
            selinux_label: false,
            mount_workspace: true,
            ..Default::default()
        };
        let runtime = PodmanRuntime::new(cfg);
        let workspace = std::env::temp_dir();
        let cmd = runtime
            .build_shell_command("echo hello", &workspace)
            .unwrap();
        let debug = format!("{cmd:?}");
        assert!(!debug.contains(":Z"));
    }

    #[test]
    fn podman_workspace_allowlist_blocks_outside_paths() {
        let cfg = PodmanRuntimeConfig {
            allowed_workspace_roots: vec!["/tmp/allowed".into()],
            ..Default::default()
        };
        let runtime = PodmanRuntime::new(cfg);
        let outside = PathBuf::from("/tmp/blocked_workspace");
        let result = runtime.build_shell_command("echo test", &outside);
        assert!(result.is_err());
    }

    #[cfg(unix)]
    #[test]
    fn podman_refuses_root_mount() {
        let cfg = PodmanRuntimeConfig {
            mount_workspace: true,
            ..Default::default()
        };
        let runtime = PodmanRuntime::new(cfg);
        let result = runtime.build_shell_command("echo test", Path::new("/"));
        assert!(result.is_err());
        let error_chain = format!("{:#}", result.unwrap_err());
        assert!(error_chain.contains("root"));
    }

    #[test]
    fn podman_no_memory_flag_when_not_configured() {
        let cfg = PodmanRuntimeConfig {
            memory_limit_mb: None,
            ..Default::default()
        };
        let runtime = PodmanRuntime::new(cfg);
        let workspace = std::env::temp_dir();
        let cmd = runtime
            .build_shell_command("echo hello", &workspace)
            .unwrap();
        let debug = format!("{cmd:?}");
        assert!(!debug.contains("--memory"));
    }

    #[test]
    fn podman_build_shell_command_includes_network_flag() {
        let cfg = PodmanRuntimeConfig {
            network: "slirp4netns".into(),
            ..Default::default()
        };
        let runtime = PodmanRuntime::new(cfg);
        let workspace = std::env::temp_dir();
        let cmd = runtime
            .build_shell_command("echo hello", &workspace)
            .unwrap();
        let debug = format!("{cmd:?}");
        assert!(debug.contains("--network") && debug.contains("slirp4netns"));
    }
}
