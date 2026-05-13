//! Podman sandbox (rootless, daemonless container isolation)
//!
//! Podman differs from Docker in key ways that affect sandbox behavior:
//! - **Daemonless**: fork+exec, no dockerd required
//! - **Rootless by default**: uses user namespaces (requires subuid/subgid)
//! - **SELinux-aware**: volume mounts need `:Z` suffix for relabeling
//! - **Different syscalls**: needs clone3, unshare, setns, pivot_root, mount
//! - **Socket path**: `/run/user/{UID}/podman/podman.sock` (not docker.sock)
//!
//! Detection distinguishes real Docker from the podman-docker compatibility
//! shim to avoid silent SIGSYS failures under restrictive systemd sandboxes.

use crate::security::traits::Sandbox;
use std::process::Command;
use daemonclaw_config::schema::PodmanSandboxConfig;

/// Podman sandbox backend
#[derive(Debug, Clone)]
pub struct PodmanSandbox {
    config: PodmanSandboxConfig,
}

impl Default for PodmanSandbox {
    fn default() -> Self {
        Self {
            config: PodmanSandboxConfig::default(),
        }
    }
}

impl PodmanSandbox {
    pub fn new() -> std::io::Result<Self> {
        if Self::is_installed() {
            Ok(Self::default())
        } else {
            Err(std::io::Error::new(
                std::io::ErrorKind::NotFound,
                "Podman not found",
            ))
        }
    }

    pub fn with_config(config: PodmanSandboxConfig) -> std::io::Result<Self> {
        if Self::is_installed() {
            Ok(Self { config })
        } else {
            Err(std::io::Error::new(
                std::io::ErrorKind::NotFound,
                "Podman not found",
            ))
        }
    }

    pub fn probe() -> std::io::Result<Self> {
        Self::new()
    }

    fn is_installed() -> bool {
        Command::new("podman")
            .arg("--version")
            .output()
            .map(|o| o.status.success())
            .unwrap_or(false)
    }

    /// Detect whether the `docker` command is actually the podman-docker shim.
    ///
    /// The shim masquerades as Docker but behaves like Podman — it requires
    /// different syscalls, different volume flags, and a different socket.
    /// Using it as Docker leads to silent SIGSYS failures under systemd's
    /// SystemCallFilter.
    pub fn is_podman_docker() -> bool {
        Command::new("docker")
            .arg("--version")
            .output()
            .ok()
            .map(|o| {
                let stdout = String::from_utf8_lossy(&o.stdout).to_lowercase();
                stdout.contains("podman")
            })
            .unwrap_or(false)
    }

    /// Check Podman rootless prerequisites:
    /// - podman binary exists
    /// - subuid/subgid entries for the current user
    /// - newuidmap/newgidmap have setuid bits
    pub fn check_rootless_prereqs() -> Vec<String> {
        let mut issues = Vec::new();

        if !Self::is_installed() {
            issues.push("podman binary not found".into());
            return issues; // No point checking further
        }

        // Check subuid/subgid
        if let Ok(output) = Command::new("podman")
            .args(["info", "--format", "{{.Host.Security.Rootless}}"])
            .output()
        {
            let stdout = String::from_utf8_lossy(&output.stdout).trim().to_string();
            if stdout != "true" {
                issues.push("podman not running in rootless mode".into());
            }
        }

        // Check newuidmap/newgidmap setuid
        for binary in ["newuidmap", "newgidmap"] {
            if let Ok(path) = which::which(binary) {
                if let Ok(meta) = std::fs::metadata(&path) {
                    use std::os::unix::fs::PermissionsExt;
                    let mode = meta.permissions().mode();
                    if mode & 0o4000 == 0 {
                        issues.push(format!("{} missing setuid bit", binary));
                    }
                }
            } else {
                issues.push(format!("{} not found in PATH", binary));
            }
        }

        issues
    }
    /// Ensure the sandbox image is available locally, pulling if missing.
    /// Uses `--pull=missing` to avoid redundant network calls.
    fn ensure_image(image: &str) -> bool {
        Command::new("podman")
            .args(["image", "exists", image])
            .status()
            .map(|s| s.success())
            .unwrap_or(false)
            || Command::new("podman")
                .args(["pull", "--quiet", image])
                .status()
                .map(|s| s.success())
            .unwrap_or(false)
    }
}

impl Sandbox for PodmanSandbox {
    fn wrap_command(&self, cmd: &mut Command) -> std::io::Result<()> {
        if !Self::ensure_image(&self.config.image) {
            return Err(std::io::Error::new(
                std::io::ErrorKind::Other,
                format!("failed to pull sandbox image: {}", self.config.image),
            ));
        }

        let program = cmd.get_program().to_string_lossy().to_string();
        let args: Vec<String> = cmd
            .get_args()
            .map(|s| s.to_string_lossy().to_string())
            .collect();

        let mut podman_cmd = Command::new("podman");
        podman_cmd.args(["run", "--rm"]);

        // Rootless UID mapping — preserves host file ownership
        podman_cmd.args(["--userns", &self.config.userns]);

        // Resource limits
        podman_cmd.args(["--memory", &self.config.memory_limit]);
        podman_cmd.args(["--cpus", &self.config.cpu_limit]);

        // Network isolation — default is "none" (no network access)
        podman_cmd.args(["--network", &self.config.network]);

        // Workspace volume mount — so sandboxed commands can access files.
        // The working directory is set to the same path inside the container
        // so relative paths resolve correctly.
        let workspace = std::env::var("DAEMONCLAW_WORKSPACE")
            .or_else(|_| std::env::var("HOME").map(|h| format!("{h}/.daemonclaw/workspace")))
            .unwrap_or_else(|_| "/tmp".into());

        let vol_suffix = if self.config.selinux_label { ":Z" } else { "" };
        podman_cmd.args([
            "-v",
            &format!("{}:/workspace{}", workspace, vol_suffix),
            "-w",
            "/workspace",
        ]);

        // Extra args escape hatch
        for arg in &self.config.extra_args {
            podman_cmd.arg(arg);
        }

        // Image and command
        podman_cmd.arg(&self.config.image);
        podman_cmd.arg(&program);
        podman_cmd.args(&args);

        *cmd = podman_cmd;
        Ok(())
    }

    fn is_available(&self) -> bool {
        if !Self::is_installed() {
            return false;
        }
        Self::check_rootless_prereqs().is_empty()
    }

    fn name(&self) -> &str {
        "podman"
    }

    fn description(&self) -> &str {
        "Podman container isolation (rootless, daemonless)"
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn podman_sandbox_name() {
        let sandbox = PodmanSandbox::default();
        assert_eq!(sandbox.name(), "podman");
    }

    #[test]
    fn podman_sandbox_default_image() {
        let sandbox = PodmanSandbox::default();
        assert_eq!(sandbox.config.image, "ubuntu:24.04");
    }

    #[test]
    fn podman_sandbox_default_network_is_none() {
        let sandbox = PodmanSandbox::default();
        assert_eq!(sandbox.config.network, "none");
    }

    #[test]
    fn podman_sandbox_default_userns_is_keep_id() {
        let sandbox = PodmanSandbox::default();
        assert_eq!(sandbox.config.userns, "keep-id");
    }

    #[test]
    fn podman_sandbox_default_selinux_enabled() {
        let sandbox = PodmanSandbox::default();
        assert!(sandbox.config.selinux_label);
    }

    #[test]
    fn podman_sandbox_default_no_extra_args() {
        let sandbox = PodmanSandbox::default();
        assert!(sandbox.config.extra_args.is_empty());
    }

    #[test]
    fn podman_with_custom_config() {
        let config = PodmanSandboxConfig {
            image: "alpine:latest".into(),
            userns: "nomodify".into(),
            network: "slirp4netns".into(),
            memory_limit: "256m".into(),
            cpu_limit: "0.5".into(),
            selinux_label: false,
            extra_args: vec!["--security-opt=seccomp=unconfined".into()],
        };
        let sandbox = PodmanSandbox::with_config(config).unwrap();
        assert_eq!(sandbox.config.image, "alpine:latest");
        assert_eq!(sandbox.config.network, "slirp4netns");
        assert_eq!(sandbox.config.extra_args.len(), 1);
    }

    #[test]
    fn podman_wrap_command_produces_valid_args() {
        let sandbox = PodmanSandbox::default();
        let mut cmd = Command::new("echo");
        cmd.arg("hello");
        sandbox.wrap_command(&mut cmd).unwrap();

        let program = cmd.get_program().to_string_lossy().to_string();
        assert_eq!(program, "podman");

        let args: Vec<String> = cmd
            .get_args()
            .map(|s| s.to_string_lossy().to_string())
            .collect();

        assert!(args.contains(&"run".to_string()));
        assert!(args.contains(&"--rm".to_string()));
        assert!(args.contains(&"--userns".to_string()));
        assert!(args.contains(&"keep-id".to_string()));
        assert!(args.contains(&"--memory".to_string()));
        assert!(args.contains(&"512m".to_string()));
        assert!(args.contains(&"--cpus".to_string()));
        assert!(args.contains(&"1.0".to_string()));
        assert!(args.contains(&"--network".to_string()));
        assert!(args.contains(&"none".to_string()));
        assert!(args.contains(&"ubuntu:24.04".to_string()));
        assert!(args.contains(&"echo".to_string()));
        assert!(args.contains(&"hello".to_string()));
    }

    #[test]
    fn podman_wrap_command_includes_extra_args() {
        let config = PodmanSandboxConfig {
            extra_args: vec!["--read-only".into()],
            ..Default::default()
        };
        let sandbox = PodmanSandbox::with_config(config).unwrap();
        let mut cmd = Command::new("ls");
        sandbox.wrap_command(&mut cmd).unwrap();

        let args: Vec<String> = cmd
            .get_args()
            .map(|s| s.to_string_lossy().to_string())
            .collect();
        assert!(args.contains(&"--read-only".to_string()));
    }
}
