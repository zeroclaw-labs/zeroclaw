//! Auto-detection of available security features

use crate::config::{SandboxBackend, SecurityConfig};
use crate::security::traits::Sandbox;
use std::sync::Arc;

/// Check if fallback to NoopSandbox is allowed via environment variable
fn allow_noop_fallback() -> bool {
    std::env::var("ZEROCLAW_ALLOW_NO_SANDBOX")
        .map(|v| !v.is_empty() && v != "0" && v.to_lowercase() != "false")
        .unwrap_or(false)
}

/// Create a sandbox based on auto-detection or explicit config
pub fn create_sandbox(config: &SecurityConfig) -> Arc<dyn Sandbox> {
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
            log_sandbox_unavailable_fallback("Landlock");
            Arc::new(super::traits::NoopSandbox)
        }
        SandboxBackend::Firejail => {
            #[cfg(target_os = "linux")]
            {
                if let Ok(sandbox) = super::firejail::FirejailSandbox::new() {
                    return Arc::new(sandbox);
                }
            }
            log_sandbox_unavailable_fallback("Firejail");
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
            log_sandbox_unavailable_fallback("Bubblewrap");
            Arc::new(super::traits::NoopSandbox)
        }
        SandboxBackend::Docker => {
            if let Ok(sandbox) = super::docker::DockerSandbox::new() {
                return Arc::new(sandbox);
            }
            log_sandbox_unavailable_fallback("Docker");
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
            // Auto-detect best available
            detect_best_sandbox()
        }
    }
}

/// Log sandbox unavailability and log fallback behavior
fn log_sandbox_unavailable_fallback(sandbox_name: &str) {
    let allow_fallback = allow_noop_fallback();

    if allow_fallback {
        tracing::warn!(
            "{sandbox_name} requested but not available, falling back to application-layer. \
             Set ZEROCLAW_ALLOW_NO_SANDBOX=0 to require sandbox availability."
        );
    } else {
        tracing::error!(
            "{sandbox_name} requested but not available. \
             Set ZEROCLAW_ALLOW_NO_SANDBOX=1 to allow fallback to application-layer."
        );
    }
}

/// Auto-detect best available sandbox
fn detect_best_sandbox() -> Arc<dyn Sandbox> {
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

    // Docker is heavy but works everywhere if docker is installed
    if let Ok(sandbox) = super::docker::DockerSandbox::probe() {
        tracing::info!("Docker sandbox enabled");
        return Arc::new(sandbox);
    }

    // Path-validation fallback: software-only deny/allow list enforcement
    let pv = super::path_validation::PathValidationSandbox::new();
    if pv.is_available() {
        tracing::info!("Path-validation sandbox enabled (software-only fallback)");
        return Arc::new(pv);
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
        let sandbox = detect_best_sandbox();
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
        let sandbox = create_sandbox(&config);
        assert_eq!(sandbox.name(), "none");
    }

    #[test]
    fn auto_allow_noop_fallback_returns_true() {
        // Set env var and check
        std::env::set_var("ZEROCLAW_ALLOW_NO_SANDBOX", "1");
        assert!(allow_noop_fallback());
        std::env::remove_var("ZEROCLAW_ALLOW_NO_SANDBOX");
    }

    #[test]
    fn auto_deny_noop_fallback_returns_false() {
        // Set env var to deny fallback
        std::env::set_var("ZEROCLAW_ALLOW_NO_SANDBOX", "0");
        assert!(!allow_noop_fallback());
        std::env::remove_var("ZEROCLAW_ALLOW_NO_SANDBOX");
    }

    #[test]
    fn auto_missing_env_denies_fallback() {
        // Missing env var should deny fallback (fail-closed by default)
        std::env::remove_var("ZEROCLAW_ALLOW_NO_SANDBOX");
        assert!(!allow_noop_fallback());
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
        let sandbox = create_sandbox(&config);
        // Should return some sandbox (at least NoopSandbox)
        assert!(sandbox.is_available());
    }
}
