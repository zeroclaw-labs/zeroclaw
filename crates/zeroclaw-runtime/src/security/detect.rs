//! Auto-detection of available security features

use crate::security::traits::Sandbox;
use std::path::Path;
use std::process::Command;
use std::sync::Arc;
use zeroclaw_config::schema::{SandboxBackend, SandboxConfig};

const NOOP_DESCRIPTION: &str = "No sandboxing (application-layer security only)";
const LANDLOCK_DESCRIPTION: &str = "Linux kernel LSM sandboxing (filesystem access control)";
const FIREJAIL_DESCRIPTION: &str = "Linux user-space sandbox (requires firejail to be installed)";
const BUBBLEWRAP_DESCRIPTION: &str = "User namespace sandbox (requires bwrap)";
const DOCKER_DESCRIPTION: &str = "Docker container isolation (requires docker)";
const SEATBELT_DESCRIPTION: &str = "macOS Seatbelt sandbox (built-in sandbox-exec)";

/// Side-effect-light description of the sandbox backend the runtime would use.
///
/// Unlike [`create_sandbox`], this does not instantiate backend wrappers, so a
/// status/doctor command can report sandbox posture without creating temporary
/// Seatbelt policy files or emitting fallback logs.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct SandboxPosture {
    pub requested_backend: &'static str,
    pub active_backend: &'static str,
    pub active_description: &'static str,
    pub fallback: bool,
}

/// Inspect sandbox backend selection without constructing a sandbox instance.
#[must_use]
pub fn sandbox_posture(sandbox: &SandboxConfig, runtime_kind: &str) -> SandboxPosture {
    let requested_backend = sandbox_backend_name(&sandbox.backend);
    if matches!(sandbox.backend, SandboxBackend::None) || sandbox.enabled == Some(false) {
        return sandbox_posture_result(requested_backend, "none", NOOP_DESCRIPTION);
    }

    let (active_backend, active_description) = match sandbox.backend {
        SandboxBackend::Landlock => {
            if landlock_available() {
                ("landlock", LANDLOCK_DESCRIPTION)
            } else {
                ("none", NOOP_DESCRIPTION)
            }
        }
        SandboxBackend::Firejail => {
            if command_succeeds("firejail", &["--version"]) {
                ("firejail", FIREJAIL_DESCRIPTION)
            } else {
                ("none", NOOP_DESCRIPTION)
            }
        }
        SandboxBackend::Bubblewrap => {
            if command_succeeds("bwrap", &["--version"]) {
                ("bubblewrap", BUBBLEWRAP_DESCRIPTION)
            } else {
                ("none", NOOP_DESCRIPTION)
            }
        }
        SandboxBackend::Docker => {
            if command_succeeds("docker", &["--version"]) {
                ("docker", DOCKER_DESCRIPTION)
            } else {
                ("none", NOOP_DESCRIPTION)
            }
        }
        SandboxBackend::SandboxExec => {
            if seatbelt_available() {
                ("sandbox-exec", SEATBELT_DESCRIPTION)
            } else {
                ("none", NOOP_DESCRIPTION)
            }
        }
        SandboxBackend::Auto => detect_best_sandbox_posture(runtime_kind),
        SandboxBackend::None => ("none", NOOP_DESCRIPTION),
    };

    sandbox_posture_result(requested_backend, active_backend, active_description)
}

fn sandbox_posture_result(
    requested_backend: &'static str,
    active_backend: &'static str,
    active_description: &'static str,
) -> SandboxPosture {
    SandboxPosture {
        requested_backend,
        active_backend,
        active_description,
        fallback: !matches!(requested_backend, "auto" | "none")
            && active_backend != requested_backend,
    }
}

fn detect_best_sandbox_posture(runtime_kind: &str) -> (&'static str, &'static str) {
    let skip_docker = runtime_kind == "native";

    #[cfg(target_os = "linux")]
    {
        #[cfg(feature = "sandbox-landlock")]
        {
            if landlock_available() {
                return ("landlock", LANDLOCK_DESCRIPTION);
            }
        }

        if command_succeeds("firejail", &["--version"]) {
            return ("firejail", FIREJAIL_DESCRIPTION);
        }
    }

    #[cfg(target_os = "macos")]
    {
        #[cfg(feature = "sandbox-bubblewrap")]
        {
            if command_succeeds("bwrap", &["--version"]) {
                return ("bubblewrap", BUBBLEWRAP_DESCRIPTION);
            }
        }

        if seatbelt_available() {
            return ("sandbox-exec", SEATBELT_DESCRIPTION);
        }
    }

    if !skip_docker && command_succeeds("docker", &["--version"]) {
        return ("docker", DOCKER_DESCRIPTION);
    }

    ("none", NOOP_DESCRIPTION)
}

fn command_succeeds(program: &str, args: &[&str]) -> bool {
    Command::new(program)
        .args(args)
        .output()
        .map(|output| output.status.success())
        .unwrap_or(false)
}

#[cfg(target_os = "macos")]
fn seatbelt_available() -> bool {
    Path::new("/usr/bin/sandbox-exec").exists()
        || command_succeeds("sandbox-exec", &["-n", "no-network", "true"])
}

#[cfg(not(target_os = "macos"))]
fn seatbelt_available() -> bool {
    false
}

#[cfg(all(feature = "sandbox-landlock", target_os = "linux"))]
fn landlock_available() -> bool {
    super::landlock::LandlockSandbox::probe().is_ok()
}

#[cfg(not(all(feature = "sandbox-landlock", target_os = "linux")))]
fn landlock_available() -> bool {
    false
}

fn sandbox_backend_name(backend: &SandboxBackend) -> &'static str {
    match backend {
        SandboxBackend::Auto => "auto",
        SandboxBackend::Landlock => "landlock",
        SandboxBackend::Firejail => "firejail",
        SandboxBackend::Bubblewrap => "bubblewrap",
        SandboxBackend::Docker => "docker",
        SandboxBackend::SandboxExec => "sandbox-exec",
        SandboxBackend::None => "none",
    }
}

/// Create a sandbox based on auto-detection or explicit config.
///
/// Takes a [`SandboxConfig`] (synthesized from the active risk profile via
/// `RiskProfileConfig::sandbox_config()`). `runtime_kind` is the
/// `runtime.kind` string from the top-level config. When the caller has set
/// `runtime.kind = "native"`, Docker must never be selected as the sandbox
/// backend during auto-detection — the user explicitly opted out of container
/// wrapping.
pub fn create_sandbox(
    sandbox: &SandboxConfig,
    runtime_kind: &str,
    workspace_dir: Option<&Path>,
) -> Arc<dyn Sandbox> {
    let backend = &sandbox.backend;

    // If explicitly disabled, return noop
    if matches!(backend, SandboxBackend::None) || sandbox.enabled == Some(false) {
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
            ::zeroclaw_log::record!(
                WARN,
                ::zeroclaw_log::Event::new(module_path!(), ::zeroclaw_log::Action::Note)
                    .with_outcome(::zeroclaw_log::EventOutcome::Unknown),
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
            ::zeroclaw_log::record!(
                WARN,
                ::zeroclaw_log::Event::new(module_path!(), ::zeroclaw_log::Action::Note)
                    .with_outcome(::zeroclaw_log::EventOutcome::Unknown),
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
            ::zeroclaw_log::record!(
                WARN,
                ::zeroclaw_log::Event::new(module_path!(), ::zeroclaw_log::Action::Note)
                    .with_outcome(::zeroclaw_log::EventOutcome::Unknown),
                "Bubblewrap requested but not available, falling back to application-layer"
            );
            Arc::new(super::traits::NoopSandbox)
        }
        SandboxBackend::Docker => {
            let result = if let Some(ws) = workspace_dir {
                super::docker::DockerSandbox::with_workspace(
                    super::docker::DockerSandbox::default_image(),
                    ws.to_path_buf(),
                )
            } else {
                super::docker::DockerSandbox::new()
            };
            if let Ok(sandbox) = result {
                return Arc::new(sandbox);
            }
            ::zeroclaw_log::record!(
                WARN,
                ::zeroclaw_log::Event::new(module_path!(), ::zeroclaw_log::Action::Note)
                    .with_outcome(::zeroclaw_log::EventOutcome::Unknown),
                "Docker requested but not available, falling back to application-layer"
            );
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
            ::zeroclaw_log::record!(
                WARN,
                ::zeroclaw_log::Event::new(module_path!(), ::zeroclaw_log::Action::Note)
                    .with_outcome(::zeroclaw_log::EventOutcome::Unknown),
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
                ::zeroclaw_log::record!(
                    INFO,
                    ::zeroclaw_log::Event::new(module_path!(), ::zeroclaw_log::Action::Note),
                    "Landlock sandbox enabled (Linux kernel 5.13+)"
                );
                return Arc::new(sandbox);
            }
        }

        // Try Firejail second (user-space tool)
        if let Ok(sandbox) = super::firejail::FirejailSandbox::probe() {
            ::zeroclaw_log::record!(
                INFO,
                ::zeroclaw_log::Event::new(module_path!(), ::zeroclaw_log::Action::Note),
                "Firejail sandbox enabled"
            );
            return Arc::new(sandbox);
        }
    }

    #[cfg(target_os = "macos")]
    {
        // Try Bubblewrap on macOS
        #[cfg(feature = "sandbox-bubblewrap")]
        {
            if let Ok(sandbox) = super::bubblewrap::BubblewrapSandbox::probe() {
                ::zeroclaw_log::record!(
                    INFO,
                    ::zeroclaw_log::Event::new(module_path!(), ::zeroclaw_log::Action::Note),
                    "Bubblewrap sandbox enabled"
                );
                return Arc::new(sandbox);
            }
        }

        // Try sandbox-exec (Seatbelt) — built into macOS
        if let Ok(sandbox) = super::seatbelt::SeatbeltSandbox::with_workspace(workspace_dir) {
            ::zeroclaw_log::record!(
                INFO,
                ::zeroclaw_log::Event::new(module_path!(), ::zeroclaw_log::Action::Note),
                "macOS sandbox-exec (Seatbelt) enabled"
            );
            return Arc::new(sandbox);
        }
    }

    // Docker is heavy but works everywhere if docker is installed.
    // Skip it when runtime.kind = "native" — the user explicitly opted out of
    // container wrapping, and forcing Docker would break Python skills (Alpine
    // has no python3) and workspace access on resource-constrained hosts.
    if !skip_docker {
        let docker_result = if let Some(ws) = workspace_dir {
            super::docker::DockerSandbox::with_workspace(
                super::docker::DockerSandbox::default_image(),
                ws.to_path_buf(),
            )
        } else {
            super::docker::DockerSandbox::probe()
        };
        if let Ok(sandbox) = docker_result {
            ::zeroclaw_log::record!(
                INFO,
                ::zeroclaw_log::Event::new(module_path!(), ::zeroclaw_log::Action::Note),
                "Docker sandbox enabled"
            );
            return Arc::new(sandbox);
        }
    } else {
        ::zeroclaw_log::record!(
            DEBUG,
            ::zeroclaw_log::Event::new(module_path!(), ::zeroclaw_log::Action::Note),
            "Docker sandbox skipped: runtime.kind = \"native\" overrides auto-detection"
        );
    }

    // Fallback: application-layer security only
    ::zeroclaw_log::record!(
        INFO,
        ::zeroclaw_log::Event::new(module_path!(), ::zeroclaw_log::Action::Note),
        "No sandbox backend available, using application-layer security"
    );
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

    #[test]
    fn detect_best_sandbox_returns_something() {
        let sandbox = detect_best_sandbox("", None);
        // Should always return at least NoopSandbox
        assert!(sandbox.is_available());
    }

    #[test]
    fn explicit_none_returns_noop() {
        let sandbox_cfg = SandboxConfig {
            enabled: Some(false),
            backend: SandboxBackend::None,
            firejail_args: Vec::new(),
        };
        let sandbox = create_sandbox(&sandbox_cfg, "", None);
        assert_eq!(sandbox.name(), "none");
    }

    #[test]
    fn explicit_none_posture_returns_noop_without_fallback() {
        let sandbox_cfg = SandboxConfig {
            enabled: Some(false),
            backend: SandboxBackend::None,
            firejail_args: Vec::new(),
        };
        let posture = sandbox_posture(&sandbox_cfg, "");
        assert_eq!(posture.requested_backend, "none");
        assert_eq!(posture.active_backend, "none");
        assert!(!posture.fallback);
    }

    #[test]
    fn auto_mode_detects_something() {
        let sandbox_cfg = SandboxConfig {
            enabled: None, // Auto-detect
            backend: SandboxBackend::Auto,
            firejail_args: Vec::new(),
        };
        let sandbox = create_sandbox(&sandbox_cfg, "", None);
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
    fn native_runtime_auto_posture_never_selects_docker() {
        let sandbox_cfg = SandboxConfig {
            enabled: None,
            backend: SandboxBackend::Auto,
            firejail_args: Vec::new(),
        };
        let posture = sandbox_posture(&sandbox_cfg, "native");
        assert_ne!(posture.active_backend, "docker");
    }

    #[test]
    fn explicit_docker_backend_is_not_blocked_by_native_runtime() {
        // Even with runtime.kind = "native", explicit `backend = "docker"` in config
        // is respected. Only the auto-detect path is gated by runtime_kind.
        let sandbox_cfg = SandboxConfig {
            enabled: None,
            backend: SandboxBackend::Docker,
            firejail_args: Vec::new(),
        };
        let sandbox = create_sandbox(&sandbox_cfg, "native", None);
        // If Docker is available, it will be selected; if not, NoopSandbox fallback.
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
