//! Auto-detection of available security features

use crate::security::policy::SandboxPolicy;
use crate::security::traits::Sandbox;
use std::path::Path;
use std::sync::Arc;
use zeroclaw_config::schema::{SandboxBackend, SandboxConfig};

const NOOP_DESCRIPTION: &str = "No sandboxing (application-layer security only)";
const LANDLOCK_DESCRIPTION: &str = "Linux kernel LSM sandboxing (filesystem access control)";
const FIREJAIL_DESCRIPTION: &str = "Linux user-space sandbox (requires firejail to be installed)";
const BUBBLEWRAP_DESCRIPTION: &str = "User namespace sandbox (requires bwrap)";
const DOCKER_DESCRIPTION: &str = "Docker container isolation (requires docker)";
const SEATBELT_DESCRIPTION: &str = "macOS Seatbelt sandbox (built-in sandbox-exec)";

/// Side-effect-light description of the sandbox backend the runtime would use.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct SandboxPosture {
    pub requested_backend: &'static str,
    pub active_backend: &'static str,
    pub active_description: &'static str,
    pub fallback: bool,
}

/// Inspect sandbox backend selection without constructing a sandbox instance.
#[must_use]
pub fn sandbox_posture(
    sandbox: &SandboxConfig,
    runtime_kind: &str,
    workspace_dir: Option<&Path>,
) -> SandboxPosture {
    let requested_backend = sandbox_backend_name(&sandbox.backend);
    if matches!(sandbox.backend, SandboxBackend::None) || sandbox.enabled == Some(false) {
        return sandbox_posture_result(requested_backend, "none", NOOP_DESCRIPTION);
    }

    let active_backend =
        configured_backend_selection(&sandbox.backend, runtime_kind, workspace_dir);

    sandbox_posture_result(
        requested_backend,
        active_backend.name(),
        active_backend.description(),
    )
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

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
enum SelectedSandboxBackend {
    None,
    Landlock,
    Firejail,
    Bubblewrap,
    Docker,
    SandboxExec,
}

impl SelectedSandboxBackend {
    fn name(self) -> &'static str {
        match self {
            Self::None => "none",
            Self::Landlock => "landlock",
            Self::Firejail => "firejail",
            Self::Bubblewrap => "bubblewrap",
            Self::Docker => "docker",
            Self::SandboxExec => "sandbox-exec",
        }
    }

    fn description(self) -> &'static str {
        match self {
            Self::None => NOOP_DESCRIPTION,
            Self::Landlock => LANDLOCK_DESCRIPTION,
            Self::Firejail => FIREJAIL_DESCRIPTION,
            Self::Bubblewrap => BUBBLEWRAP_DESCRIPTION,
            Self::Docker => DOCKER_DESCRIPTION,
            Self::SandboxExec => SEATBELT_DESCRIPTION,
        }
    }

    fn from_config(backend: &SandboxBackend) -> Option<Self> {
        match backend {
            SandboxBackend::Auto | SandboxBackend::None => None,
            SandboxBackend::Landlock => Some(Self::Landlock),
            SandboxBackend::Firejail => Some(Self::Firejail),
            SandboxBackend::Bubblewrap => Some(Self::Bubblewrap),
            SandboxBackend::Docker => Some(Self::Docker),
            SandboxBackend::SandboxExec => Some(Self::SandboxExec),
        }
    }
}

fn configured_backend_selection(
    backend: &SandboxBackend,
    runtime_kind: &str,
    workspace_dir: Option<&Path>,
) -> SelectedSandboxBackend {
    if matches!(backend, SandboxBackend::Auto) {
        return detect_best_backend(runtime_kind, workspace_dir);
    }

    SelectedSandboxBackend::from_config(backend)
        .filter(|selected| sandbox_backend_available(*selected, workspace_dir))
        .unwrap_or(SelectedSandboxBackend::None)
}

fn detect_best_backend(runtime_kind: &str, workspace_dir: Option<&Path>) -> SelectedSandboxBackend {
    let skip_docker = runtime_kind == "native";
    #[cfg(target_os = "linux")]
    {
        #[cfg(feature = "sandbox-landlock")]
        {
            if sandbox_backend_available(SelectedSandboxBackend::Landlock, workspace_dir) {
                return SelectedSandboxBackend::Landlock;
            }
        }

        if sandbox_backend_available(SelectedSandboxBackend::Firejail, workspace_dir) {
            return SelectedSandboxBackend::Firejail;
        }
    }

    #[cfg(target_os = "macos")]
    {
        #[cfg(feature = "sandbox-bubblewrap")]
        {
            if sandbox_backend_available(SelectedSandboxBackend::Bubblewrap, workspace_dir) {
                return SelectedSandboxBackend::Bubblewrap;
            }
        }

        if sandbox_backend_available(SelectedSandboxBackend::SandboxExec, workspace_dir) {
            return SelectedSandboxBackend::SandboxExec;
        }
    }

    if !skip_docker && sandbox_backend_available(SelectedSandboxBackend::Docker, workspace_dir) {
        return SelectedSandboxBackend::Docker;
    }

    SelectedSandboxBackend::None
}

fn sandbox_backend_available(
    backend: SelectedSandboxBackend,
    workspace_dir: Option<&Path>,
) -> bool {
    match backend {
        SelectedSandboxBackend::None => true,
        SelectedSandboxBackend::Landlock => landlock_available(workspace_dir),
        SelectedSandboxBackend::Firejail => {
            #[cfg(target_os = "linux")]
            {
                super::firejail::FirejailSandbox::probe().is_ok()
            }
            #[cfg(not(target_os = "linux"))]
            {
                false
            }
        }
        SelectedSandboxBackend::Bubblewrap => {
            #[cfg(feature = "sandbox-bubblewrap")]
            {
                #[cfg(any(target_os = "linux", target_os = "macos"))]
                {
                    super::bubblewrap::BubblewrapSandbox::probe().is_ok()
                }
                #[cfg(not(any(target_os = "linux", target_os = "macos")))]
                {
                    false
                }
            }
            #[cfg(not(feature = "sandbox-bubblewrap"))]
            {
                false
            }
        }
        SelectedSandboxBackend::Docker => {
            let result = if let Some(ws) = workspace_dir {
                super::docker::DockerSandbox::with_workspace(
                    super::docker::DockerSandbox::default_image(),
                    ws.to_path_buf(),
                )
            } else {
                super::docker::DockerSandbox::probe()
            };
            result.is_ok()
        }
        SelectedSandboxBackend::SandboxExec => seatbelt_available(),
    }
}

#[cfg(target_os = "macos")]
fn seatbelt_available() -> bool {
    Path::new("/usr/bin/sandbox-exec").exists()
        || std::process::Command::new("sandbox-exec")
            .args(["-n", "no-network", "true"])
            .output()
            .map(|output| output.status.success())
            .unwrap_or(false)
}

#[cfg(not(target_os = "macos"))]
fn seatbelt_available() -> bool {
    false
}

#[cfg(all(feature = "sandbox-landlock", target_os = "linux"))]
fn landlock_available(workspace_dir: Option<&Path>) -> bool {
    super::landlock::LandlockSandbox::with_workspace(workspace_dir.map(Path::to_path_buf)).is_ok()
}

#[cfg(not(all(feature = "sandbox-landlock", target_os = "linux")))]
fn landlock_available(_workspace_dir: Option<&Path>) -> bool {
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
/// `RiskProfileConfig::sandbox_config()`) and a resolved [`SandboxPolicy`]
/// (from `SandboxPolicy::from_risk_profile`). `runtime_kind` is the
/// `runtime.kind` string from the top-level config. When the caller has set
/// `runtime.kind = "native"`, Docker must never be selected as the sandbox
/// backend during auto-detection — the user explicitly opted out of container
/// wrapping.
///
/// `policy` is accepted to establish a stable call-site contract but is not
/// yet forwarded to individual backends; sandbox selection is currently driven
/// solely by `SandboxConfig` and `runtime_kind`. It IS consulted for one
/// thing: deciding whether to warn that `deny_write`/`deny_read` are
/// unenforced (see [`warn_if_denials_unenforced`]).
pub fn create_sandbox(
    sandbox: &SandboxConfig,
    policy: &SandboxPolicy,
    runtime_kind: &str,
    workspace_dir: Option<&Path>,
) -> Arc<dyn Sandbox> {
    let backend = &sandbox.backend;

    // If explicitly disabled, return noop
    if matches!(backend, SandboxBackend::None) || sandbox.enabled == Some(false) {
        warn_if_denials_unenforced(policy);
        return Arc::new(super::traits::NoopSandbox);
    }

    match backend {
        SandboxBackend::Auto | SandboxBackend::None => {
            let sandbox = detect_best_sandbox(runtime_kind, workspace_dir);
            warn_if_denials_unenforced(policy);
            sandbox
        }
        requested => {
            let selected = configured_backend_selection(requested, runtime_kind, workspace_dir);
            if let Some(sandbox) = create_selected_sandbox(selected, workspace_dir) {
                warn_if_denials_unenforced(policy);
                return sandbox;
            }
            log_requested_backend_unavailable(selected_backend_label(requested));
            warn_if_denials_unenforced(policy);
            Arc::new(super::traits::NoopSandbox)
        }
    }
}

/// Whether `policy`'s `deny_write`/`deny_read` denials are NOT enforced by
/// any active sandbox backend. Currently always `true` whenever any denial
/// is configured: `create_selected_sandbox` does not forward `policy` to any
/// backend constructor (Landlock/Bubblewrap/Seatbelt/Docker/Firejail), so
/// enforcement against arbitrary shell/script child-process I/O is
/// application-layer-only (`SecurityPolicy`, `zeroclaw-config`) regardless of
/// which backend is selected, until per-backend OS sandbox wiring lands (RFC
/// 6996 Phase 2). Deliberately backend-name-agnostic — a backend that merely
/// *activates* does not yet *enforce* these fields; once a backend PR wires
/// `policy` through to its constructor, that backend should gain a real
/// capability signal here, not before. Pure predicate, split out from
/// [`warn_if_denials_unenforced`] so it is unit-testable without a logging
/// harness.
#[must_use]
fn sandbox_denials_unenforced(policy: &SandboxPolicy) -> bool {
    !policy.deny_write.is_empty() || !policy.deny_read.is_empty()
}

/// Emit a one-time-per-call WARN naming the enforcement gap when denials are
/// configured but no active backend can enforce them against shell/script
/// child-process I/O. See [`sandbox_denials_unenforced`].
fn warn_if_denials_unenforced(policy: &SandboxPolicy) {
    if sandbox_denials_unenforced(policy) {
        ::zeroclaw_log::record!(
            WARN,
            ::zeroclaw_log::Event::new(module_path!(), ::zeroclaw_log::Action::Note)
                .with_outcome(::zeroclaw_log::EventOutcome::Unknown),
            "sandbox_policy denials are enforced for file tools only; shell child processes \
             are not confined (no OS sandbox backend forwards this policy yet)"
        );
    }
}

/// Auto-detect the best available sandbox.
///
/// When `runtime_kind` is `"native"` the caller has explicitly opted out of
/// container wrapping, so Docker is excluded from consideration even if it is
/// installed on the host.
fn detect_best_sandbox(runtime_kind: &str, workspace_dir: Option<&Path>) -> Arc<dyn Sandbox> {
    let selected = detect_best_backend(runtime_kind, workspace_dir);
    if let Some(sandbox) = create_selected_sandbox(selected, workspace_dir) {
        log_auto_backend_selection(selected, runtime_kind);
        return sandbox;
    }

    log_auto_backend_selection(SelectedSandboxBackend::None, runtime_kind);
    Arc::new(super::traits::NoopSandbox)
}

fn create_selected_sandbox(
    selected: SelectedSandboxBackend,
    workspace_dir: Option<&Path>,
) -> Option<Arc<dyn Sandbox>> {
    match selected {
        SelectedSandboxBackend::None => None,
        SelectedSandboxBackend::Landlock => {
            #[cfg(all(feature = "sandbox-landlock", target_os = "linux"))]
            {
                super::landlock::LandlockSandbox::with_workspace(
                    workspace_dir.map(Path::to_path_buf),
                )
                .map(|sandbox| Arc::new(sandbox) as Arc<dyn Sandbox>)
                .ok()
            }
            #[cfg(not(all(feature = "sandbox-landlock", target_os = "linux")))]
            {
                None
            }
        }
        SelectedSandboxBackend::Firejail => {
            #[cfg(target_os = "linux")]
            {
                super::firejail::FirejailSandbox::new()
                    .map(|sandbox| Arc::new(sandbox) as Arc<dyn Sandbox>)
                    .ok()
            }
            #[cfg(not(target_os = "linux"))]
            {
                None
            }
        }
        SelectedSandboxBackend::Bubblewrap => {
            #[cfg(all(
                feature = "sandbox-bubblewrap",
                any(target_os = "linux", target_os = "macos")
            ))]
            {
                super::bubblewrap::BubblewrapSandbox::new()
                    .map(|sandbox| Arc::new(sandbox) as Arc<dyn Sandbox>)
                    .ok()
            }
            #[cfg(not(all(
                feature = "sandbox-bubblewrap",
                any(target_os = "linux", target_os = "macos")
            )))]
            {
                None
            }
        }
        SelectedSandboxBackend::Docker => {
            let result = if let Some(ws) = workspace_dir {
                super::docker::DockerSandbox::with_workspace(
                    super::docker::DockerSandbox::default_image(),
                    ws.to_path_buf(),
                )
            } else {
                super::docker::DockerSandbox::new()
            };
            result
                .map(|sandbox| Arc::new(sandbox) as Arc<dyn Sandbox>)
                .ok()
        }
        SelectedSandboxBackend::SandboxExec => {
            #[cfg(target_os = "macos")]
            {
                super::seatbelt::SeatbeltSandbox::with_workspace(workspace_dir)
                    .map(|sandbox| Arc::new(sandbox) as Arc<dyn Sandbox>)
                    .ok()
            }
            #[cfg(not(target_os = "macos"))]
            {
                None
            }
        }
    }
}

fn selected_backend_label(backend: &SandboxBackend) -> &'static str {
    match backend {
        SandboxBackend::Auto => "Auto",
        SandboxBackend::Landlock => "Landlock",
        SandboxBackend::Firejail => "Firejail",
        SandboxBackend::Bubblewrap => "Bubblewrap",
        SandboxBackend::Docker => "Docker",
        SandboxBackend::SandboxExec => "sandbox-exec",
        SandboxBackend::None => "None",
    }
}

fn log_requested_backend_unavailable(label: &'static str) {
    ::zeroclaw_log::record!(
        WARN,
        ::zeroclaw_log::Event::new(module_path!(), ::zeroclaw_log::Action::Note)
            .with_outcome(::zeroclaw_log::EventOutcome::Unknown),
        &format!("{label} requested but not available, falling back to application-layer")
    );
}

fn log_auto_backend_selection(selected: SelectedSandboxBackend, runtime_kind: &str) {
    match selected {
        SelectedSandboxBackend::None => {
            if runtime_kind == "native" {
                ::zeroclaw_log::record!(
                    DEBUG,
                    ::zeroclaw_log::Event::new(module_path!(), ::zeroclaw_log::Action::Note),
                    "Docker sandbox skipped: runtime.kind = \"native\" overrides auto-detection"
                );
            }
            ::zeroclaw_log::record!(
                INFO,
                ::zeroclaw_log::Event::new(module_path!(), ::zeroclaw_log::Action::Note),
                "No sandbox backend available, using application-layer security"
            );
        }
        SelectedSandboxBackend::Landlock => {
            ::zeroclaw_log::record!(
                INFO,
                ::zeroclaw_log::Event::new(module_path!(), ::zeroclaw_log::Action::Note),
                "Landlock sandbox enabled (Linux kernel 5.13+)"
            );
        }
        SelectedSandboxBackend::Firejail => {
            ::zeroclaw_log::record!(
                INFO,
                ::zeroclaw_log::Event::new(module_path!(), ::zeroclaw_log::Action::Note),
                "Firejail sandbox enabled"
            );
        }
        SelectedSandboxBackend::Bubblewrap => {
            ::zeroclaw_log::record!(
                INFO,
                ::zeroclaw_log::Event::new(module_path!(), ::zeroclaw_log::Action::Note),
                "Bubblewrap sandbox enabled"
            );
        }
        SelectedSandboxBackend::Docker => {
            ::zeroclaw_log::record!(
                INFO,
                ::zeroclaw_log::Event::new(module_path!(), ::zeroclaw_log::Action::Note),
                "Docker sandbox enabled"
            );
        }
        SelectedSandboxBackend::SandboxExec => {
            ::zeroclaw_log::record!(
                INFO,
                ::zeroclaw_log::Event::new(module_path!(), ::zeroclaw_log::Action::Note),
                "macOS sandbox-exec (Seatbelt) enabled"
            );
        }
    }
}

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
    use crate::security::policy::SandboxPolicy;

    fn default_policy() -> SandboxPolicy {
        SandboxPolicy::default()
    }

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
        let sandbox = create_sandbox(&sandbox_cfg, &default_policy(), "", None);
        assert_eq!(sandbox.name(), "none");
    }

    #[test]
    fn explicit_none_posture_returns_noop_without_fallback() {
        let sandbox_cfg = SandboxConfig {
            enabled: Some(false),
            backend: SandboxBackend::None,
            firejail_args: Vec::new(),
        };
        let posture = sandbox_posture(&sandbox_cfg, "", None);
        assert_eq!(posture.requested_backend, "none");
        assert_eq!(posture.active_backend, "none");
        assert!(!posture.fallback);
    }

    #[test]
    fn auto_mode_detects_something() {
        let sandbox_cfg = SandboxConfig {
            enabled: None,
            backend: SandboxBackend::Auto,
            firejail_args: Vec::new(),
        };
        let sandbox = create_sandbox(&sandbox_cfg, &default_policy(), "", None);
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
        let posture = sandbox_posture(&sandbox_cfg, "native", None);
        assert_ne!(posture.active_backend, "docker");
    }

    #[test]
    fn auto_posture_reports_same_backend_as_runtime_factory() {
        let sandbox_cfg = SandboxConfig {
            enabled: None,
            backend: SandboxBackend::Auto,
            firejail_args: Vec::new(),
        };
        let sandbox = create_sandbox(&sandbox_cfg, &SandboxPolicy::default(), "native", None);
        let posture = sandbox_posture(&sandbox_cfg, "native", None);

        assert_eq!(posture.active_backend, sandbox.name());
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
        let sandbox = create_sandbox(&sandbox_cfg, &default_policy(), "native", None);
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

    // ── sandbox_policy denial enforcement-gap WARN (Blocker 2c) ──────────
    //
    // Withhold-and-document scope decision: `deny_write`/`deny_read` are
    // enforced today for file tools only (`SecurityPolicy` in
    // `zeroclaw-config`), never for arbitrary shell/script child-process I/O,
    // until per-backend OS sandbox wiring lands (RFC 6996 Phase 2, one PR
    // per backend — Bubblewrap/Landlock/Seatbelt). `sandbox_denials_unenforced`
    // is the pure predicate `warn_if_denials_unenforced` gates on; there is no
    // log-capture harness in this crate, so these tests exercise the
    // predicate directly rather than asserting on emitted log records — the
    // predicate IS the enforcement-owner contract this WARN exists to name.

    #[test]
    fn sandbox_denials_unenforced_when_deny_write_configured() {
        let policy = default_policy();
        assert!(
            !policy.deny_write.is_empty(),
            "default profile carries the mandatory deny_write guardrail list"
        );
        assert!(sandbox_denials_unenforced(&policy));
    }

    #[test]
    fn sandbox_denials_unenforced_when_deny_read_configured() {
        let mut policy = default_policy();
        policy.deny_write = Vec::new();
        policy.deny_read = vec![std::path::PathBuf::from("/secret")];
        assert!(sandbox_denials_unenforced(&policy));
    }

    #[test]
    fn sandbox_denials_unenforced_true_regardless_of_backend_name() {
        // No backend constructor receives `policy` today (`create_selected_sandbox`
        // takes only workspace_dir: landlock/bubblewrap/sandbox-exec/docker/
        // firejail all wire the same as "none" here), so the predicate takes
        // no backend argument at all — an active-looking backend must not
        // suppress the warning. A prior revision of this test asserted the
        // opposite and was itself the bug: it treated backend presence as
        // proof of enforcement.
        let policy = default_policy();
        assert!(!policy.deny_write.is_empty());
        assert!(sandbox_denials_unenforced(&policy));
    }

    #[test]
    fn sandbox_denials_unenforced_false_when_no_denials_configured() {
        let mut policy = default_policy();
        policy.deny_write = Vec::new();
        policy.deny_read = Vec::new();
        assert!(!sandbox_denials_unenforced(&policy));
    }

    #[test]
    fn create_sandbox_with_denials_and_explicit_none_backend_does_not_panic() {
        // Exercises the actual create_sandbox call site (not just the
        // predicate) to confirm the WARN wiring compiles and runs without
        // requiring the discarded `_policy` binding removed in this change.
        let sandbox_cfg = SandboxConfig {
            enabled: Some(false),
            backend: SandboxBackend::None,
            firejail_args: Vec::new(),
        };
        let sandbox = create_sandbox(&sandbox_cfg, &default_policy(), "", None);
        assert_eq!(sandbox.name(), "none");
    }
}
