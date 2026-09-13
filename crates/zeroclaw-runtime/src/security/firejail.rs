//! Firejail sandbox (Linux user-space sandboxing)
//! Firejail is a SUID sandbox program that Linux applications use to sandbox themselves.

use crate::security::traits::Sandbox;
use std::ffi::OsStr;
use std::path::{Path, PathBuf};
use std::process::Command;
use zeroclaw_config::platform::resolve_executable;

#[derive(Debug, Clone, Copy, Default)]
struct FirejailHardeningSupport {
    seccomp: bool,
    caps_drop: bool,
    noroot: bool,
}

/// Firejail sandbox backend for Linux
#[derive(Debug, Clone, Copy, Default)]
pub struct FirejailSandbox;

impl FirejailSandbox {
    /// Create a new Firejail sandbox
    pub fn new() -> std::io::Result<Self> {
        let sandbox = Self;
        sandbox.version_launcher()?;
        Ok(sandbox)
    }

    /// Probe if Firejail is available (for auto-detection)
    pub fn probe() -> std::io::Result<Self> {
        Self::new()
    }

    fn resolve_launcher() -> std::io::Result<PathBuf> {
        resolve_executable(OsStr::new("firejail")).map_err(|error| {
            std::io::Error::new(
                error.kind(),
                format!("Firejail launcher could not be resolved: {error}"),
            )
        })
    }

    fn version_launcher(&self) -> std::io::Result<PathBuf> {
        let launcher = Self::resolve_launcher()?;
        let output = Command::new(&launcher)
            .arg("--version")
            .output()
            .map_err(|error| {
                std::io::Error::new(
                    error.kind(),
                    format!(
                        "Firejail launcher at {} could not be probed: {error}",
                        launcher.display()
                    ),
                )
            })?;
        if !output.status.success() {
            return Err(std::io::Error::new(
                std::io::ErrorKind::NotFound,
                format!("Firejail launcher at {} is unavailable", launcher.display()),
            ));
        }

        Ok(launcher)
    }

    fn detect_hardening_support(launcher: &std::path::Path) -> FirejailHardeningSupport {
        let support = Command::new(launcher)
            .arg("--help")
            .env_clear()
            .output()
            .map(|output| {
                Self::support_from_help(
                    &String::from_utf8_lossy(&output.stdout),
                    &String::from_utf8_lossy(&output.stderr),
                )
            })
            .unwrap_or_default();

        Self::log_incomplete_hardening_support(support);
        support
    }

    #[cfg(test)]
    fn for_test() -> Self {
        Self
    }

    fn support_from_help(stdout: &str, stderr: &str) -> FirejailHardeningSupport {
        let contains = |flag| stdout.contains(flag) || stderr.contains(flag);

        FirejailHardeningSupport {
            seccomp: contains("--seccomp"),
            caps_drop: contains("--caps.drop"),
            noroot: contains("--noroot"),
        }
    }

    fn log_incomplete_hardening_support(support: FirejailHardeningSupport) {
        if support.seccomp && support.caps_drop && support.noroot {
            return;
        }

        ::zeroclaw_log::record!(
            WARN,
            ::zeroclaw_log::Event::new(module_path!(), ::zeroclaw_log::Action::Note)
                .with_outcome(::zeroclaw_log::EventOutcome::Unknown)
                .with_attrs(::serde_json::json!({
                    "backend": "firejail",
                    "seccomp": support.seccomp,
                    "caps_drop": support.caps_drop,
                    "noroot": support.noroot,
                })),
            "firejail sandbox hardening support is incomplete"
        );
    }

    fn append_hardening_flags(cmd: &mut Command, support: FirejailHardeningSupport) {
        if support.seccomp {
            cmd.arg("--seccomp");
        }

        if support.caps_drop {
            cmd.arg("--caps.drop=all");
        }

        if support.noroot {
            cmd.arg("--noroot");
        }
    }

    #[cfg(test)]
    fn wrap_command_with_support(
        &self,
        cmd: &mut Command,
        support: FirejailHardeningSupport,
    ) -> std::io::Result<()> {
        self.wrap_command_with_launcher(cmd, support, Path::new("firejail"))
    }

    fn wrap_command_with_launcher(
        &self,
        cmd: &mut Command,
        support: FirejailHardeningSupport,
        launcher: &Path,
    ) -> std::io::Result<()> {
        // Prepend firejail to the command
        let program = cmd.get_program().to_string_lossy().to_string();
        let args: Vec<String> = cmd
            .get_args()
            .map(|s| s.to_string_lossy().to_string())
            .collect();
        let current_dir = cmd.get_current_dir().map(std::path::Path::to_path_buf);

        // Build firejail wrapper with security flags
        let mut firejail_cmd = Command::new(launcher);
        firejail_cmd.args([
            "--private=home", // New home directory
            "--private-dev",  // Minimal /dev
            "--nosound",      // No audio
            "--no3d",         // No 3D acceleration
            "--novideo",      // No video devices
            "--nowheel",      // No input devices
            "--notv",         // No TV devices
            "--noprofile",    // Skip profile loading
            "--quiet",        // Suppress warnings
        ]);
        Self::append_hardening_flags(&mut firejail_cmd, support);

        // Add the original command
        firejail_cmd.arg(&program);
        firejail_cmd.args(&args);
        if let Some(current_dir) = current_dir {
            firejail_cmd.current_dir(current_dir);
        }

        // Replace the command
        *cmd = firejail_cmd;
        Ok(())
    }
}

impl Sandbox for FirejailSandbox {
    fn wrap_command(&self, cmd: &mut Command) -> std::io::Result<()> {
        let launcher = Self::resolve_launcher()?;
        let support = Self::detect_hardening_support(&launcher);
        self.wrap_command_with_launcher(cmd, support, &launcher)
    }

    fn is_available(&self) -> bool {
        self.version_launcher().is_ok()
    }

    fn name(&self) -> &str {
        "firejail"
    }

    fn description(&self) -> &str {
        "Linux user-space sandbox (requires firejail to be installed)"
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    fn args(cmd: &Command) -> Vec<String> {
        cmd.get_args()
            .map(|s| s.to_string_lossy().to_string())
            .collect()
    }

    #[test]
    fn firejail_sandbox_name() {
        assert_eq!(FirejailSandbox::for_test().name(), "firejail");
    }

    #[test]
    fn firejail_description_mentions_dependency() {
        let sandbox = FirejailSandbox::for_test();
        let desc = sandbox.description();
        assert!(desc.contains("firejail"));
    }

    #[test]
    fn firejail_new_fails_if_not_installed() {
        // This will fail unless firejail is actually installed
        let result = FirejailSandbox::new();
        match result {
            Ok(_) => println!("Firejail is installed"),
            Err(e) => assert!(
                e.kind() == std::io::ErrorKind::NotFound
                    || e.kind() == std::io::ErrorKind::Unsupported
            ),
        }
    }

    #[test]
    fn firejail_wrap_command_prepends_firejail() {
        let sandbox = FirejailSandbox::for_test();
        let mut cmd = Command::new("echo");
        cmd.arg("test");

        // Note: wrap_command will fail if firejail isn't installed,
        // but we can still test the logic structure
        let _ = sandbox.wrap_command(&mut cmd);

        // After wrapping, the program should be firejail
        if sandbox.is_available() {
            let expected_launcher = FirejailSandbox::resolve_launcher().unwrap();
            assert_eq!(cmd.get_program(), expected_launcher.as_os_str());
        }
    }

    #[test]
    fn firejail_wrap_command_preserves_current_dir() {
        let sandbox = FirejailSandbox::for_test();
        let workspace = std::env::temp_dir().join("zeroclaw-firejail-workspace");
        let mut cmd = Command::new("echo");
        cmd.current_dir(&workspace).arg("test");

        sandbox
            .wrap_command_with_support(&mut cmd, FirejailHardeningSupport::default())
            .unwrap();

        assert_eq!(cmd.get_current_dir(), Some(workspace.as_path()));
    }

    #[cfg(unix)]
    #[test]
    fn firejail_wrap_uses_the_resolved_launcher_identity() {
        use std::os::unix::fs::PermissionsExt;

        let dir = tempfile::tempdir().unwrap();
        let launcher = dir.path().join("firejail");
        std::fs::write(&launcher, "#!/bin/sh\nexit 0\n").unwrap();
        std::fs::set_permissions(&launcher, std::fs::Permissions::from_mode(0o755)).unwrap();
        let launcher = launcher.canonicalize().unwrap();
        let sandbox = FirejailSandbox;
        assert!(
            Command::new(&launcher)
                .arg("--version")
                .output()
                .unwrap()
                .status
                .success()
        );
        let support = FirejailSandbox::detect_hardening_support(&launcher);
        let mut command = Command::new("echo");
        sandbox
            .wrap_command_with_launcher(&mut command, support, &launcher)
            .unwrap();
        assert_eq!(command.get_program(), launcher.as_os_str());
    }

    // ── §1.1 Sandbox isolation flag tests ──────────────────────

    #[test]
    fn firejail_wrap_command_includes_all_security_flags() {
        let sandbox = FirejailSandbox::for_test();
        let mut cmd = Command::new("echo");
        cmd.arg("test");
        sandbox
            .wrap_command_with_support(&mut cmd, FirejailHardeningSupport::default())
            .unwrap();

        assert_eq!(
            cmd.get_program().to_string_lossy(),
            "firejail",
            "wrapped command should use firejail as program"
        );

        let args = args(&cmd);

        let expected_flags = [
            "--private=home",
            "--private-dev",
            "--nosound",
            "--no3d",
            "--novideo",
            "--nowheel",
            "--notv",
            "--noprofile",
            "--quiet",
        ];

        for flag in &expected_flags {
            assert!(
                args.contains(&flag.to_string()),
                "must include security flag: {flag}"
            );
        }
    }

    #[test]
    fn firejail_supported_hardening_flags_include_seccomp_cap_drop_and_noroot() {
        let mut cmd = Command::new("firejail");
        FirejailSandbox::append_hardening_flags(
            &mut cmd,
            FirejailHardeningSupport {
                seccomp: true,
                caps_drop: true,
                noroot: true,
            },
        );

        let args = args(&cmd);
        assert!(
            args.windows(3)
                .any(|window| window == ["--seccomp", "--caps.drop=all", "--noroot"]),
            "supported firejail hardening flags must be appended together"
        );
    }

    #[test]
    fn firejail_support_from_help_detects_stdout_and_stderr_flags() {
        let support = FirejailSandbox::support_from_help("--seccomp --caps.drop", "--noroot");

        assert!(support.seccomp);
        assert!(support.caps_drop);
        assert!(support.noroot);
    }

    #[test]
    fn firejail_wrap_command_applies_supported_hardening_flags() {
        let sandbox = FirejailSandbox::for_test();
        let mut cmd = Command::new("echo");
        cmd.arg("test");
        sandbox
            .wrap_command_with_support(
                &mut cmd,
                FirejailHardeningSupport {
                    seccomp: true,
                    caps_drop: true,
                    noroot: true,
                },
            )
            .unwrap();

        let args = args(&cmd);
        let expected_flags = ["--seccomp", "--caps.drop=all", "--noroot"];
        for flag in expected_flags {
            assert!(
                args.contains(&flag.to_string()),
                "wrap_command must apply supported hardening flag: {flag}"
            );
        }
    }

    #[test]
    fn firejail_skips_unadvertised_hardening_flags() {
        let mut cmd = Command::new("firejail");
        FirejailSandbox::append_hardening_flags(
            &mut cmd,
            FirejailHardeningSupport {
                seccomp: true,
                caps_drop: false,
                noroot: true,
            },
        );

        let args = args(&cmd);
        assert!(args.contains(&"--seccomp".to_string()));
        assert!(!args.contains(&"--caps.drop=all".to_string()));
        assert!(args.contains(&"--noroot".to_string()));
    }

    #[test]
    fn firejail_wrap_command_preserves_original_command() {
        let sandbox = FirejailSandbox::for_test();
        let mut cmd = Command::new("ls");
        cmd.arg("-la");
        cmd.arg("/workspace");
        sandbox
            .wrap_command_with_support(&mut cmd, FirejailHardeningSupport::default())
            .unwrap();

        let args = args(&cmd);

        assert!(
            args.contains(&"ls".to_string()),
            "original program must be passed as argument"
        );
        assert!(
            args.contains(&"-la".to_string()),
            "original args must be preserved"
        );
        assert!(
            args.contains(&"/workspace".to_string()),
            "original args must be preserved"
        );
    }
}
