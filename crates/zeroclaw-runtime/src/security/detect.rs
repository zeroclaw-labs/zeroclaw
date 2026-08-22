//! Auto-detection of available security features

use crate::security::traits::Sandbox;
use std::path::Path;
use std::sync::Arc;
use zeroclaw_config::schema::{RuntimeKind, SandboxBackend, SandboxConfig};

const NOOP_DESCRIPTION: &str = "No sandboxing (application-layer security only)";
const DOCKER_RUNTIME_DESCRIPTION: &str =
    "Docker runtime container isolation (runtime.kind = \"docker\"; no additional sandbox wrapper)";
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
    runtime_kind: RuntimeKind,
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
        // An explicit `backend = "docker"` on the Docker runtime is honored by
        // the runtime container itself, so it is not a fallback.
        fallback: !matches!(requested_backend, "auto" | "none")
            && active_backend != requested_backend
            && !(requested_backend == "docker" && active_backend == "docker-runtime"),
    }
}

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
enum SelectedSandboxBackend {
    None,
    Landlock,
    Firejail,
    Bubblewrap,
    Docker,
    /// No additional sandbox wrapper is constructed, but containment is not
    /// lost: `runtime.kind = "docker"` already runs every command inside the
    /// runtime container. Distinct from `None` so posture reporting does not
    /// describe this state as application-layer-only.
    DockerRuntime,
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
            Self::DockerRuntime => "docker-runtime",
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
            Self::DockerRuntime => DOCKER_RUNTIME_DESCRIPTION,
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
    runtime_kind: RuntimeKind,
    workspace_dir: Option<&Path>,
) -> SelectedSandboxBackend {
    if matches!(backend, SandboxBackend::Auto) {
        return detect_best_backend(runtime_kind, workspace_dir);
    }

    if matches!(backend, SandboxBackend::Docker) && matches!(runtime_kind, RuntimeKind::Docker) {
        return SelectedSandboxBackend::DockerRuntime;
    }

    SelectedSandboxBackend::from_config(backend)
        .filter(|selected| sandbox_backend_compatible_with_runtime(*selected, runtime_kind))
        .filter(|selected| sandbox_backend_available(*selected, workspace_dir))
        .unwrap_or(SelectedSandboxBackend::None)
}

fn sandbox_backend_compatible_with_runtime(
    selected: SelectedSandboxBackend,
    runtime_kind: RuntimeKind,
) -> bool {
    !(matches!(selected, SelectedSandboxBackend::Docker)
        && matches!(runtime_kind, RuntimeKind::Docker))
}

fn auto_backend_compatible_with_runtime(
    selected: SelectedSandboxBackend,
    runtime_kind: RuntimeKind,
) -> bool {
    sandbox_backend_compatible_with_runtime(selected, runtime_kind)
        && !(matches!(selected, SelectedSandboxBackend::Docker)
            && matches!(runtime_kind, RuntimeKind::Native))
}

fn detect_best_backend(
    runtime_kind: RuntimeKind,
    workspace_dir: Option<&Path>,
) -> SelectedSandboxBackend {
    detect_best_backend_with(runtime_kind, |selected| {
        sandbox_backend_available(selected, workspace_dir)
    })
}

fn detect_best_backend_with(
    runtime_kind: RuntimeKind,
    mut is_available: impl FnMut(SelectedSandboxBackend) -> bool,
) -> SelectedSandboxBackend {
    #[cfg(target_os = "linux")]
    {
        #[cfg(feature = "sandbox-landlock")]
        {
            if is_available(SelectedSandboxBackend::Landlock) {
                return SelectedSandboxBackend::Landlock;
            }
        }

        if is_available(SelectedSandboxBackend::Firejail) {
            return SelectedSandboxBackend::Firejail;
        }
    }

    #[cfg(target_os = "macos")]
    {
        #[cfg(feature = "sandbox-bubblewrap")]
        {
            if is_available(SelectedSandboxBackend::Bubblewrap) {
                return SelectedSandboxBackend::Bubblewrap;
            }
        }

        if is_available(SelectedSandboxBackend::SandboxExec) {
            return SelectedSandboxBackend::SandboxExec;
        }
    }

    if auto_backend_compatible_with_runtime(SelectedSandboxBackend::Docker, runtime_kind)
        && is_available(SelectedSandboxBackend::Docker)
    {
        return SelectedSandboxBackend::Docker;
    }

    if matches!(runtime_kind, RuntimeKind::Docker) {
        return SelectedSandboxBackend::DockerRuntime;
    }

    SelectedSandboxBackend::None
}

fn sandbox_backend_available(
    backend: SelectedSandboxBackend,
    workspace_dir: Option<&Path>,
) -> bool {
    match backend {
        SelectedSandboxBackend::None => true,
        // Containment comes from the runtime container itself; there is no
        // host-side wrapper to probe.
        SelectedSandboxBackend::DockerRuntime => true,
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
    Path::new(super::seatbelt::SANDBOX_EXEC_PATH).is_file()
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

pub fn create_sandbox(
    sandbox: &SandboxConfig,
    runtime_kind: RuntimeKind,
    workspace_dir: Option<&Path>,
) -> Arc<dyn Sandbox> {
    let backend = &sandbox.backend;

    // If explicitly disabled, return noop
    if matches!(backend, SandboxBackend::None) || sandbox.enabled == Some(false) {
        return Arc::new(super::traits::NoopSandbox);
    }

    match backend {
        SandboxBackend::Auto | SandboxBackend::None => {
            detect_best_sandbox(runtime_kind, workspace_dir)
        }
        requested => {
            let selected = configured_backend_selection(requested, runtime_kind, workspace_dir);
            if matches!(selected, SelectedSandboxBackend::DockerRuntime) {
                log_docker_sandbox_redundant_with_docker_runtime();
                return Arc::new(super::traits::NoopSandbox);
            }
            if let Some(sandbox) = create_selected_sandbox(selected, workspace_dir) {
                return sandbox;
            }
            log_requested_backend_unavailable(selected_backend_label(requested));
            Arc::new(super::traits::NoopSandbox)
        }
    }
}

fn detect_best_sandbox(
    runtime_kind: RuntimeKind,
    workspace_dir: Option<&Path>,
) -> Arc<dyn Sandbox> {
    let selected = detect_best_backend(runtime_kind, workspace_dir);
    if matches!(selected, SelectedSandboxBackend::DockerRuntime) {
        log_auto_backend_selection(selected, runtime_kind);
        return Arc::new(super::traits::NoopSandbox);
    }
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
        // The runtime container owns containment; no wrapper is constructed.
        SelectedSandboxBackend::DockerRuntime => None,
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

fn log_docker_sandbox_redundant_with_docker_runtime() {
    ::zeroclaw_log::record!(
        WARN,
        ::zeroclaw_log::Event::new(module_path!(), ::zeroclaw_log::Action::Note)
            .with_outcome(::zeroclaw_log::EventOutcome::Unknown),
        "Docker sandbox skipped: runtime.kind = \"docker\" already runs commands in a container; \
         a nested Docker sandbox would double-wrap the command"
    );
}

fn log_auto_backend_selection(selected: SelectedSandboxBackend, runtime_kind: RuntimeKind) {
    match selected {
        SelectedSandboxBackend::None => {
            if matches!(runtime_kind, RuntimeKind::Native) {
                ::zeroclaw_log::record!(
                    DEBUG,
                    ::zeroclaw_log::Event::new(module_path!(), ::zeroclaw_log::Action::Note),
                    "Docker sandbox skipped: runtime.kind = \"native\" overrides auto-detection"
                );
            }
            if matches!(runtime_kind, RuntimeKind::Docker) {
                ::zeroclaw_log::record!(
                    INFO,
                    ::zeroclaw_log::Event::new(module_path!(), ::zeroclaw_log::Action::Note),
                    "No additional sandbox backend available; Docker runtime still provides container isolation"
                );
            } else {
                ::zeroclaw_log::record!(
                    INFO,
                    ::zeroclaw_log::Event::new(module_path!(), ::zeroclaw_log::Action::Note),
                    "No sandbox backend available, using application-layer security"
                );
            }
        }
        SelectedSandboxBackend::DockerRuntime => {
            ::zeroclaw_log::record!(
                INFO,
                ::zeroclaw_log::Event::new(module_path!(), ::zeroclaw_log::Action::Note),
                "Docker runtime provides container isolation; no additional sandbox wrapper needed"
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

    #[test]
    fn detect_best_sandbox_returns_something() {
        let sandbox = detect_best_sandbox(RuntimeKind::Cloudflare, None);
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
        let sandbox = create_sandbox(&sandbox_cfg, RuntimeKind::Cloudflare, None);
        assert_eq!(sandbox.name(), "none");
    }

    #[test]
    fn explicit_none_posture_returns_noop_without_fallback() {
        let sandbox_cfg = SandboxConfig {
            enabled: Some(false),
            backend: SandboxBackend::None,
            firejail_args: Vec::new(),
        };
        let posture = sandbox_posture(&sandbox_cfg, RuntimeKind::Cloudflare, None);
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
        let sandbox = create_sandbox(&sandbox_cfg, RuntimeKind::Cloudflare, None);
        // Should return some sandbox (at least NoopSandbox)
        assert!(sandbox.is_available());
    }

    #[test]
    fn native_runtime_with_auto_sandbox_never_selects_docker() {
        // When runtime.kind = "native", Docker must be skipped in auto-detection
        // even when Docker is installed on the host. The sandbox must be
        // NoopSandbox or something OS-native (Landlock, Firejail, Seatbelt).
        let sandbox = detect_best_sandbox(RuntimeKind::Native, None);
        assert_ne!(sandbox.name(), "docker");
    }

    #[test]
    fn native_runtime_auto_posture_never_selects_docker() {
        let sandbox_cfg = SandboxConfig {
            enabled: None,
            backend: SandboxBackend::Auto,
            firejail_args: Vec::new(),
        };
        let posture = sandbox_posture(&sandbox_cfg, RuntimeKind::Native, None);
        assert_ne!(posture.active_backend, "docker");
    }

    #[test]
    fn auto_posture_reports_same_backend_as_runtime_factory() {
        let sandbox_cfg = SandboxConfig {
            enabled: None,
            backend: SandboxBackend::Auto,
            firejail_args: Vec::new(),
        };
        let sandbox = create_sandbox(&sandbox_cfg, RuntimeKind::Native, None);
        let posture = sandbox_posture(&sandbox_cfg, RuntimeKind::Native, None);

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
        let sandbox = create_sandbox(&sandbox_cfg, RuntimeKind::Native, None);
        // If Docker is available, it will be selected; if not, NoopSandbox fallback.
        assert!(sandbox.is_available());
    }

    #[test]
    fn docker_runtime_auto_selection_skips_available_docker_backend() {
        let selected = detect_best_backend_with(RuntimeKind::Docker, |backend| {
            matches!(backend, SelectedSandboxBackend::Docker)
        });

        assert_eq!(selected, SelectedSandboxBackend::DockerRuntime);
    }

    #[test]
    fn docker_runtime_auto_selection_reports_runtime_containment_when_nothing_available() {
        let selected = detect_best_backend_with(RuntimeKind::Docker, |_| false);

        assert_eq!(selected, SelectedSandboxBackend::DockerRuntime);
    }

    #[test]
    fn native_runtime_auto_selection_reports_none_when_nothing_available() {
        let selected = detect_best_backend_with(RuntimeKind::Native, |_| false);

        assert_eq!(selected, SelectedSandboxBackend::None);
    }

    #[test]
    fn native_runtime_auto_selection_skips_available_docker_backend() {
        let selected = detect_best_backend_with(RuntimeKind::Native, |backend| {
            matches!(backend, SelectedSandboxBackend::Docker)
        });

        assert_eq!(selected, SelectedSandboxBackend::None);
    }

    #[test]
    fn non_docker_runtime_auto_selection_can_use_available_docker_backend() {
        let selected = detect_best_backend_with(RuntimeKind::Cloudflare, |backend| {
            matches!(backend, SelectedSandboxBackend::Docker)
        });

        assert_eq!(selected, SelectedSandboxBackend::Docker);
    }

    #[test]
    fn explicit_docker_backend_creates_no_extra_wrapper_on_docker_runtime() {
        let sandbox_cfg = SandboxConfig {
            enabled: None,
            backend: SandboxBackend::Docker,
            firejail_args: Vec::new(),
        };

        let sandbox = create_sandbox(&sandbox_cfg, RuntimeKind::Docker, None);

        assert_eq!(sandbox.name(), "none");
    }

    #[test]
    fn explicit_docker_posture_reports_runtime_containment_on_docker_runtime() {
        let sandbox_cfg = SandboxConfig {
            enabled: None,
            backend: SandboxBackend::Docker,
            firejail_args: Vec::new(),
        };

        let posture = sandbox_posture(&sandbox_cfg, RuntimeKind::Docker, None);

        assert_eq!(posture.requested_backend, "docker");
        assert_eq!(posture.active_backend, "docker-runtime");
        assert_eq!(posture.active_description, DOCKER_RUNTIME_DESCRIPTION);
        assert!(
            !posture.fallback,
            "Docker runtime honors the requested containment; it is not a fallback"
        );
    }

    #[test]
    fn docker_runtime_auto_posture_never_reports_application_layer_only() {
        let sandbox_cfg = SandboxConfig {
            enabled: None,
            backend: SandboxBackend::Auto,
            firejail_args: Vec::new(),
        };

        let posture = sandbox_posture(&sandbox_cfg, RuntimeKind::Docker, None);

        // Depending on the host an OS-native backend may be active, but the
        // posture must never degrade to "none" (containment is not lost) nor
        // nest a second Docker layer.
        assert_ne!(posture.active_backend, "none");
        assert_ne!(posture.active_backend, "docker");
        assert!(!posture.fallback);
    }

    #[test]
    fn only_docker_sandbox_conflicts_with_docker_runtime() {
        assert!(!sandbox_backend_compatible_with_runtime(
            SelectedSandboxBackend::Docker,
            RuntimeKind::Docker,
        ));
        assert!(sandbox_backend_compatible_with_runtime(
            SelectedSandboxBackend::Docker,
            RuntimeKind::Native,
        ));
        assert!(sandbox_backend_compatible_with_runtime(
            SelectedSandboxBackend::SandboxExec,
            RuntimeKind::Docker,
        ));
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
