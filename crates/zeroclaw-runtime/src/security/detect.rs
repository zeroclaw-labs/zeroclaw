//! Auto-detection of available security features

use crate::security::traits::Sandbox;
use std::sync::Arc;
use zeroclaw_config::schema::{SandboxBackend, SecurityConfig};

/// Create a sandbox based on auto-detection or explicit config.
///
/// `runtime_kind` is the `runtime.kind` string from the top-level config
/// (e.g. `"native"`, `"docker"`). When the caller has set `runtime.kind = "native"`,
/// Docker must never be selected as the sandbox backend during auto-detection —
/// the user explicitly opted out of container wrapping.
pub fn create_sandbox(config: &SecurityConfig, runtime_kind: &str) -> Arc<dyn Sandbox> {
    let backend = &config.sandbox.backend;
    let podman_config = config.sandbox.podman.clone();

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
        SandboxBackend::Podman => {
            match super::podman::PodmanSandbox::with_config(podman_config) {
                Ok(sandbox) if sandbox.is_available() => {
                    tracing::info!("Podman sandbox enabled (rootless, daemonless)");
                    return Arc::new(sandbox);
                }
                Ok(_) => {
                    let issues = super::podman::PodmanSandbox::check_rootless_prereqs();
                    tracing::warn!(
                        "Podman requested but rootless prerequisites not met: {:?}, \
                         falling back to application-layer",
                        issues
                    );
                }
                Err(_) => {
                    tracing::warn!("Podman requested but not installed, falling back to application-layer");
                }
            }
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
            detect_best_sandbox(runtime_kind, podman_config)
        }
    }
}

/// Auto-detect the best available sandbox.
///
/// When `runtime_kind` is `"native"` the caller has explicitly opted out of
/// container wrapping, so Docker/Podman sandboxes are excluded. When the
/// runtime is itself a container (`"docker"` or `"podman"`), container
/// sandboxes are also skipped to avoid nesting containers.
fn detect_best_sandbox(runtime_kind: &str, podman_config: zeroclaw_config::schema::PodmanSandboxConfig) -> Arc<dyn Sandbox> {
    let skip_containers = matches!(runtime_kind, "native" | "docker" | "podman");

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

    // Container-based sandboxes (Podman, Docker).
    // Skipped when: runtime.kind = "native" (user opted out of containers),
    // or runtime.kind = "docker"/"podman" (already inside a container —
    // nesting would require --privileged and defeat the security purpose).
    //
    // Podman is preferred over Docker when available: it's daemonless,
    // rootless by default, and avoids the podman-docker shim SIGSYS issue.
    if !skip_containers {
        // Try Podman first (daemonless, rootless)
        if let Ok(sandbox) = super::podman::PodmanSandbox::with_config(podman_config) {
            if sandbox.is_available() {
                tracing::info!("Podman sandbox enabled (rootless, daemonless)");
                return Arc::new(sandbox);
            } else {
                let issues = super::podman::PodmanSandbox::check_rootless_prereqs();
                tracing::debug!(
                    "Podman found but rootless prerequisites not met: {:?}",
                    issues
                );
            }
        }

        // Warn if docker is actually podman-docker shim
        if super::podman::PodmanSandbox::is_podman_docker() {
            tracing::warn!(
                "'docker' command is podman-docker (compatibility shim). \
                 Set sandbox.backend = \"podman\" in config.toml for proper support. \
                 The \"docker\" backend may fail under restrictive systemd sandboxing."
            );
        }

        if let Ok(sandbox) = super::docker::DockerSandbox::probe() {
            tracing::info!("Docker sandbox enabled");
            return Arc::new(sandbox);
        }
    } else {
        tracing::debug!(
            "Container sandbox skipped: runtime.kind = \"{}\" — not nesting containers",
            runtime_kind
        );
    }

    // Fallback: application-layer security only
    tracing::info!("No sandbox backend available, using application-layer security");
    Arc::new(super::traits::NoopSandbox)
}

#[cfg(test)]
mod tests {
    use super::*;
    use zeroclaw_config::schema::{PodmanSandboxConfig, SandboxConfig, SecurityConfig};

    #[test]
    fn detect_best_sandbox_returns_something() {
        let sandbox = detect_best_sandbox("", PodmanSandboxConfig::default());
        // Should always return at least NoopSandbox
        assert!(sandbox.is_available());
    }

    #[test]
    fn explicit_none_returns_noop() {
        let config = SecurityConfig {
            sandbox: SandboxConfig {
                enabled: Some(false),
                backend: SandboxBackend::None,
                ..SandboxConfig::default()
            },
            ..Default::default()
        };
        let sandbox = create_sandbox(&config, "");
        assert_eq!(sandbox.name(), "none");
    }

    #[test]
    fn auto_mode_detects_something() {
        let config = SecurityConfig {
            sandbox: SandboxConfig {
                enabled: None, // Auto-detect
                backend: SandboxBackend::Auto,
                ..SandboxConfig::default()
            },
            ..Default::default()
        };
        let sandbox = create_sandbox(&config, "");
        // Should return some sandbox (at least NoopSandbox)
        assert!(sandbox.is_available());
    }

    #[test]
    fn native_runtime_with_auto_sandbox_never_selects_docker_or_podman() {
        let sandbox = detect_best_sandbox("native", PodmanSandboxConfig::default());
        assert_ne!(sandbox.name(), "docker");
        assert_ne!(sandbox.name(), "podman");
    }

    #[test]
    fn docker_runtime_skips_container_sandboxes() {
        let sandbox = detect_best_sandbox("docker", PodmanSandboxConfig::default());
        assert_ne!(sandbox.name(), "docker");
        assert_ne!(sandbox.name(), "podman");
    }

    #[test]
    fn podman_runtime_skips_container_sandboxes() {
        let sandbox = detect_best_sandbox("podman", PodmanSandboxConfig::default());
        assert_ne!(sandbox.name(), "docker");
        assert_ne!(sandbox.name(), "podman");
    }

    #[test]
    fn explicit_docker_backend_is_not_blocked_by_native_runtime() {
        // Even with runtime.kind = "native", explicit `backend = "docker"` in config
        // is respected. Only the auto-detect path is gated by runtime_kind.
        let config = SecurityConfig {
            sandbox: SandboxConfig {
                enabled: None,
                backend: SandboxBackend::Docker,
                ..SandboxConfig::default()
            },
            ..Default::default()
        };
        let sandbox = create_sandbox(&config, "native");
        // If Docker is available, it will be selected; if not, NoopSandbox fallback.
        // The point is that runtime.kind doesn't override explicit `backend = "docker"`.
        assert!(sandbox.is_available());
    }
}
