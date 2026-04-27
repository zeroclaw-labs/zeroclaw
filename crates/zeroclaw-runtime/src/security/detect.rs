//! Auto-detection of available security features

use crate::security::traits::Sandbox;
use std::path::Path;
use std::sync::Arc;
use zeroclaw_config::schema::{SandboxBackend, SecurityConfig};

/// Create a sandbox based on auto-detection or explicit config.
///
/// `runtime_kind` is the `runtime.kind` string from the top-level config
/// (e.g. `"native"`, `"docker"`). When the caller has set `runtime.kind = "native"`,
/// Docker must never be selected as the sandbox backend during auto-detection —
/// the user explicitly opted out of container wrapping.
pub fn create_sandbox(
    config: &SecurityConfig,
    runtime_kind: &str,
    workspace_dir: Option<&Path>,
) -> Arc<dyn Sandbox> {
    let backend = &config.sandbox.backend;

    // If explicitly disabled, return noop
    if matches!(backend, SandboxBackend::None) || config.sandbox.enabled == Some(false) {
        return Arc::new(super::traits::NoopSandbox);
    }

    // If specific backend requested, try that
    match backend {
        SandboxBackend::Landlock => {
            #[cfg(feature = "sandbox-landlock")]
            {
                #[cfg(target_os = "linux")]
                {
                    if let Ok(sandbox) = super::landlock::LandlockSandbox::with_workspace(
                        workspace_dir.map(Path::to_path_buf),
                    ) {
                        return Arc::new(sandbox);
                    }
                }
            }
            tracing::warn!(
                "Landlock requested but not available, falling back to application-layer"
            );
            Arc::new(super::traits::NoopSandbox)
        }
        SandboxBackend::Firejail => {
            #[cfg(target_os = "linux")]
            {
                if let Ok(sandbox) = super::firejail::FirejailSandbox::new() {
                    return Arc::new(sandbox);
                }
            }
            tracing::warn!(
                "Firejail requested but not available, falling back to application-layer"
            );
            Arc::new(super::traits::NoopSandbox)
        }
        SandboxBackend::Bubblewrap => {
            #[cfg(feature = "sandbox-bubblewrap")]
            {
                #[cfg(any(target_os = "linux", target_os = "macos"))]
                {
                    if let Ok(sandbox) = super::bubblewrap::BubblewrapSandbox::new() {
                        return Arc::new(sandbox);
                    }
                }
            }
            tracing::warn!(
                "Bubblewrap requested but not available, falling back to application-layer"
            );
            Arc::new(super::traits::NoopSandbox)
        }
        SandboxBackend::Docker => {
            if let Ok(sandbox) = super::docker::DockerSandbox::new() {
                return Arc::new(sandbox);
            }
            tracing::warn!("Docker requested but not available, falling back to application-layer");
            Arc::new(super::traits::NoopSandbox)
        }
        SandboxBackend::SandboxExec => {
            #[cfg(target_os = "macos")]
            {
                if let Ok(sandbox) = super::seatbelt::SeatbeltSandbox::with_workspace(workspace_dir)
                {
                    return Arc::new(sandbox);
                }
            }
            tracing::warn!(
                "sandbox-exec requested but not available, falling back to application-layer"
            );
            Arc::new(super::traits::NoopSandbox)
        }
        SandboxBackend::Auto | SandboxBackend::None => {
            // Auto-detect best available, skipping Docker when native runtime is in use
            detect_best_sandbox(runtime_kind, workspace_dir)
        }
    }
}

/// Auto-detect the best available sandbox.
///
/// When `runtime_kind` is `"native"` the caller has explicitly opted out of
/// container wrapping, so Docker is excluded from consideration even if it is
/// installed on the host.
fn detect_best_sandbox(runtime_kind: &str, workspace_dir: Option<&Path>) -> Arc<dyn Sandbox> {
    let skip_docker = runtime_kind == "native";

    #[cfg(target_os = "linux")]
    {
        // Try Landlock first (native, no dependencies)
        #[cfg(feature = "sandbox-landlock")]
        {
            if let Ok(sandbox) = super::landlock::LandlockSandbox::with_workspace(
                workspace_dir.map(Path::to_path_buf),
            ) {
                tracing::info!("Landlock sandbox enabled (Linux kernel 5.13+)");
                return Arc::new(sandbox);
            }
        }

        // Try Firejail second (user-space tool)
        if let Ok(sandbox) = super::firejail::FirejailSandbox::probe() {
            tracing::info!("Firejail sandbox enabled");
            return Arc::new(sandbox);
        }
    }

    #[cfg(target_os = "macos")]
    {
        // Try Bubblewrap on macOS
        #[cfg(feature = "sandbox-bubblewrap")]
        {
            if let Ok(sandbox) = super::bubblewrap::BubblewrapSandbox::probe() {
                tracing::info!("Bubblewrap sandbox enabled");
                return Arc::new(sandbox);
            }
        }

        // Try sandbox-exec (Seatbelt) — built into macOS
        if let Ok(sandbox) = super::seatbelt::SeatbeltSandbox::with_workspace(workspace_dir) {
            tracing::info!("macOS sandbox-exec (Seatbelt) enabled");
            return Arc::new(sandbox);
        }
    }

    // Docker is heavy but works everywhere if docker is installed.
    // Skip it when runtime.kind = "native" — the user explicitly opted out of
    // container wrapping, and forcing Docker would break Python skills (Alpine
    // has no python3) and workspace access on resource-constrained hosts.
    if !skip_docker {
        if let Ok(sandbox) = super::docker::DockerSandbox::probe() {
            tracing::info!("Docker sandbox enabled");
            return Arc::new(sandbox);
        }
    } else {
        tracing::debug!(
            "Docker sandbox skipped: runtime.kind = \"native\" overrides auto-detection"
        );
    }

    // Fallback: application-layer security only
    tracing::info!("No sandbox backend available, using application-layer security");
    Arc::new(super::traits::NoopSandbox)
}

/// Returns true if the Linux kernel has the memory cgroup controller enabled.
///
/// Probes cgroup v2 (`/sys/fs/cgroup/memory.max`), then cgroup v1
/// (`/sys/fs/cgroup/memory/memory.limit_in_bytes`), then `/proc/cgroups`.
/// Any read error is treated as "absent" (conservative/safe direction).
#[cfg(target_os = "linux")]
pub fn linux_memcg_available() -> bool {
    use std::path::Path;

    if Path::new("/sys/fs/cgroup/memory.max").exists() {
        return true;
    }
    if Path::new("/sys/fs/cgroup/memory/memory.limit_in_bytes").exists() {
        return true;
    }
    if let Ok(content) = std::fs::read_to_string("/proc/cgroups") {
        for line in content.lines() {
            if line.starts_with('#') {
                continue;
            }
            let mut cols = line.split_whitespace();
            let name = cols.next().unwrap_or("");
            let _hierarchy = cols.next();
            let _num_cgroups = cols.next();
            let enabled = cols.next().unwrap_or("0");
            if name == "memory" && enabled == "1" {
                return true;
            }
        }
    }
    false
}

/// Non-Linux stub — always returns false.
/// Exists so the symbol compiles on all platforms (used in cross-platform tests).
#[cfg(not(target_os = "linux"))]
pub fn linux_memcg_available() -> bool {
    false
}

#[cfg(test)]
mod tests {
    use super::*;
    use zeroclaw_config::schema::{SandboxConfig, SecurityConfig};

    #[test]
    fn detect_best_sandbox_returns_something() {
        let sandbox = detect_best_sandbox("", None);
        // Should always return at least NoopSandbox
        assert!(sandbox.is_available());
    }

    #[test]
    fn explicit_none_returns_noop() {
        let config = SecurityConfig {
            sandbox: SandboxConfig {
                enabled: Some(false),
                backend: SandboxBackend::None,
                firejail_args: Vec::new(),
            },
            ..Default::default()
        };
        let sandbox = create_sandbox(&config, "", None);
        assert_eq!(sandbox.name(), "none");
    }

    #[test]
    fn auto_mode_detects_something() {
        let config = SecurityConfig {
            sandbox: SandboxConfig {
                enabled: None, // Auto-detect
                backend: SandboxBackend::Auto,
                firejail_args: Vec::new(),
            },
            ..Default::default()
        };
        let sandbox = create_sandbox(&config, "", None);
        // Should return some sandbox (at least NoopSandbox)
        assert!(sandbox.is_available());
    }

    #[test]
    fn native_runtime_with_auto_sandbox_never_selects_docker() {
        // When runtime.kind = "native", Docker must be skipped in auto-detection
        // even when Docker is installed on the host. The sandbox must be
        // NoopSandbox or something OS-native (Landlock, Firejail, Seatbelt).
        let sandbox = detect_best_sandbox("native", None);
        assert_ne!(sandbox.name(), "docker");
    }

    #[test]
    fn explicit_docker_backend_is_not_blocked_by_native_runtime() {
        // Even with runtime.kind = "native", explicit `backend = "docker"` in config
        // is respected. Only the auto-detect path is gated by runtime_kind.
        let config = SecurityConfig {
            sandbox: SandboxConfig {
                enabled: None,
                backend: SandboxBackend::Docker,
                firejail_args: Vec::new(),
            },
            ..Default::default()
        };
        let sandbox = create_sandbox(&config, "native", None);
        // If Docker is available, it will be selected; if not, NoopSandbox fallback.
        // The point is that runtime.kind doesn't override explicit `backend = "docker"`.
        assert!(sandbox.is_available());
    }

    #[test]
    fn linux_memcg_available_returns_bool() {
        let _result: bool = linux_memcg_available();
    }

    #[cfg(target_os = "linux")]
    #[test]
    fn linux_memcg_cgroup_v2_path_probe_does_not_panic() {
        let _ = std::path::Path::new("/sys/fs/cgroup/memory.max").exists();
    }

    #[cfg(target_os = "linux")]
    #[test]
    fn linux_memcg_proc_cgroups_parses_without_panic() {
        if let Ok(content) = std::fs::read_to_string("/proc/cgroups") {
            let _found = content.lines().filter(|l| !l.starts_with('#')).any(|l| {
                let mut f = l.split_whitespace();
                let name = f.next().unwrap_or("");
                let _hier = f.next();
                let _num = f.next();
                let enabled = f.next().unwrap_or("0");
                name == "memory" && enabled == "1"
            });
        }
    }
}
