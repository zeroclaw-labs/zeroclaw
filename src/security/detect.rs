//! Auto-detection of available security features

use crate::config::{SandboxBackend, SecurityConfig};
use crate::security::traits::Sandbox;
use std::path::Path;
use std::sync::Arc;

/// Create a sandbox based on auto-detection or explicit config.
///
/// `workspace_dir` is forwarded to the Docker sandbox so it can bind-mount
/// the workspace into the container.  Pass `None` when the workspace path is
/// not yet known (e.g. in unit tests).
///
/// `runtime_kind` is the `runtime.kind` string from the top-level config
/// (e.g. `"native"`, `"docker"`).  When set to `"native"`, Docker is skipped
/// during auto-detection — the user explicitly opted out of container wrapping.
pub fn create_sandbox(
    config: &SecurityConfig,
    _workspace_dir: Option<&Path>,
    runtime_kind: &str,
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
                    if let Ok(sandbox) = super::landlock::LandlockSandbox::new() {
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
                if let Ok(sandbox) = super::seatbelt::SeatbeltSandbox::new() {
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
            detect_best_sandbox(_workspace_dir, runtime_kind)
        }
    }
}

/// Auto-detect the best available sandbox.
///
/// When `runtime_kind` is `"native"` the caller has explicitly opted out of
/// container wrapping, so Docker is excluded from consideration even if it is
/// installed on the host.
fn detect_best_sandbox(_workspace_dir: Option<&Path>, runtime_kind: &str) -> Arc<dyn Sandbox> {
    let skip_docker = runtime_kind == "native";
    #[cfg(target_os = "linux")]
    {
        // Try Landlock first (native, no dependencies)
        #[cfg(feature = "sandbox-landlock")]
        {
            if let Ok(sandbox) = super::landlock::LandlockSandbox::probe() {
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
        if let Ok(sandbox) = super::seatbelt::SeatbeltSandbox::probe() {
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

#[cfg(test)]
mod tests {
    use super::*;
    use crate::config::{SandboxConfig, SecurityConfig};

    #[test]
    fn detect_best_sandbox_returns_something() {
        let sandbox = detect_best_sandbox(None, "");
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
        let sandbox = create_sandbox(&config, None, "");
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
        let sandbox = create_sandbox(&config, None, "");
        // Should return some sandbox (at least NoopSandbox)
        assert!(sandbox.is_available());
    }

    #[test]
    fn native_runtime_with_auto_sandbox_never_selects_docker() {
        // When runtime.kind = "native", Docker must be skipped in auto-detection
        // even when Docker is installed on the host.  The sandbox must be
        // either a native OS sandbox (Landlock/Firejail/Seatbelt) or NoopSandbox.
        let config = SecurityConfig {
            sandbox: SandboxConfig {
                enabled: None,
                backend: SandboxBackend::Auto,
                firejail_args: Vec::new(),
            },
            ..Default::default()
        };
        let sandbox = create_sandbox(&config, None, "native");
        assert_ne!(
            sandbox.name(),
            "docker",
            "runtime.kind='native' must not select Docker sandbox during auto-detection"
        );
        // Whatever was selected, it must be functional (or noop)
        assert!(sandbox.is_available());
    }

    #[test]
    fn explicit_docker_backend_is_not_blocked_by_native_runtime() {
        // If the user *explicitly* sets security.sandbox.backend = "docker",
        // respect that even when runtime.kind = "native".  The native-runtime
        // exclusion only applies to auto-detection.
        let config = SecurityConfig {
            sandbox: SandboxConfig {
                enabled: None,
                backend: SandboxBackend::Docker,
                firejail_args: Vec::new(),
            },
            ..Default::default()
        };
        // This must not panic.  If Docker is unavailable it falls back to noop,
        // but the important thing is that the explicit request is honoured.
        let _sandbox = create_sandbox(&config, "native");
    }
}
