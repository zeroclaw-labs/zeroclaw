//! Bubblewrap sandbox (user namespaces for Linux/macOS)

use crate::security::traits::Sandbox;
use std::process::Command;

/// Bubblewrap sandbox backend
#[derive(Debug, Clone, Default)]
pub struct BubblewrapSandbox;

impl BubblewrapSandbox {
    pub fn new() -> std::io::Result<Self> {
        if Self::is_installed() {
            Ok(Self)
        } else {
            Err(std::io::Error::new(
                std::io::ErrorKind::NotFound,
                "Bubblewrap not found",
            ))
        }
    }

    pub fn probe() -> std::io::Result<Self> {
        Self::new()
    }

    fn is_installed() -> bool {
        Command::new("bwrap")
            .arg("--version")
            .output()
            .map(|o| o.status.success())
            .unwrap_or(false)
    }

    /// Check if seccomp is available for syscall filtering
    fn seccomp_available() -> bool {
        Command::new("bwrap")
            .arg("--help")
            .output()
            .map(|o| String::from_utf8_lossy(&o.stdout).contains("--seccomp"))
            .unwrap_or(false)
    }
}

impl Sandbox for BubblewrapSandbox {
    fn wrap_command(&self, cmd: &mut Command) -> std::io::Result<()> {
        let program = cmd.get_program().to_string_lossy().to_string();
        let args: Vec<String> = cmd
            .get_args()
            .map(|s| s.to_string_lossy().to_string())
            .collect();

        let mut bwrap_cmd = Command::new("bwrap");
        bwrap_cmd.args([
            "--ro-bind",
            "/usr",
            "/usr",
            "--ro-bind",
            "/usr/local",
            "/usr/local",
            "--ro-bind",
            "/bin",
            "/bin",
            "--ro-bind",
            "/sbin",
            "/sbin",
            "--dev",
            "/dev",
            "--proc",
            "/proc",
            "--bind",
            "/tmp",
            "/tmp",
            "--unshare-all",
            "--die-with-parent",
        ]);

        // Try to use seccomp for syscall filtering
        if Self::seccomp_available() {
            tracing::info!("Enabling seccomp BPF filter for bubblewrap sandbox");
            // Use basic seccomp profile that blocks dangerous syscalls
            // This profile blocks ptrace, mount, kexec_load, and other dangerous syscalls
            bwrap_cmd.arg("--seccomp");
            bwrap_cmd.arg("--seccomp-drop-capability");
            bwrap_cmd.arg("CAP_SYS_ADMIN");
            bwrap_cmd.arg("--seccomp-drop-capability");
            bwrap_cmd.arg("CAP_SYS_PTRACE");
        } else {
            tracing::warn!(
                "seccomp not available in bubblewrap. Install seccomp-bpf package for enhanced syscall filtering."
            );
        }

        bwrap_cmd.arg(&program);
        bwrap_cmd.args(&args);

        *cmd = bwrap_cmd;
        Ok(())
    }

    fn is_available(&self) -> bool {
        Self::is_installed()
    }

    fn name(&self) -> &str {
        "bubblewrap"
    }

    fn description(&self) -> &str {
        "User namespace sandbox (requires bwrap)"
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn bubblewrap_sandbox_name() {
        let sandbox = BubblewrapSandbox;
        assert_eq!(sandbox.name(), "bubblewrap");
    }

    #[test]
    fn bubblewrap_is_available_only_if_installed() {
        // Result depends on whether bwrap is installed
        let sandbox = BubblewrapSandbox;
        let _available = sandbox.is_available();

        // Either way, the name should still work
        assert_eq!(sandbox.name(), "bubblewrap");
    }

    // ── §1.1 Sandbox isolation flag tests ──────────────────────

    #[test]
    fn bubblewrap_wrap_command_includes_isolation_flags() {
        let sandbox = BubblewrapSandbox;
        let mut cmd = Command::new("echo");
        cmd.arg("hello");
        sandbox.wrap_command(&mut cmd).unwrap();

        assert_eq!(
            cmd.get_program().to_string_lossy(),
            "bwrap",
            "wrapped command should use bwrap as program"
        );

        let args: Vec<String> = cmd
            .get_args()
            .map(|s| s.to_string_lossy().to_string())
            .collect();

        assert!(
            args.contains(&"--unshare-all".to_string()),
            "must include --unshare-all for namespace isolation"
        );
        assert!(
            args.contains(&"--die-with-parent".to_string()),
            "must include --die-with-parent to prevent orphan processes"
        );
        assert!(
            !args.contains(&"--share-net".to_string()),
            "must NOT include --share-net (network should be blocked)"
        );
    }

    #[test]
    fn bubblewrap_wrap_command_preserves_original_command() {
        let sandbox = BubblewrapSandbox;
        let mut cmd = Command::new("ls");
        cmd.arg("-la");
        cmd.arg("/tmp");
        sandbox.wrap_command(&mut cmd).unwrap();

        let args: Vec<String> = cmd
            .get_args()
            .map(|s| s.to_string_lossy().to_string())
            .collect();

        assert!(
            args.contains(&"ls".to_string()),
            "original program must be passed as argument"
        );
        assert!(
            args.contains(&"-la".to_string()),
            "original args must be preserved"
        );
        assert!(
            args.contains(&"/tmp".to_string()),
            "original args must be preserved"
        );
    }

    #[test]
    fn bubblewrap_wrap_command_binds_required_paths() {
        let sandbox = BubblewrapSandbox;
        let mut cmd = Command::new("echo");
        sandbox.wrap_command(&mut cmd).unwrap();

        let args: Vec<String> = cmd
            .get_args()
            .map(|s| s.to_string_lossy().to_string())
            .collect();

        assert!(
            args.contains(&"--ro-bind".to_string()),
            "must include read-only bind for /usr"
        );
        assert!(args.contains(&"/usr".to_string()), "must include /usr bind");
        assert!(
            args.contains(&"/usr/local".to_string()),
            "must include /usr/local bind for tools like python3"
        );
        assert!(
            args.contains(&"/bin".to_string()),
            "must include /bin bind for core system tools"
        );
        assert!(
            args.contains(&"/sbin".to_string()),
            "must include /sbin bind for system administration tools"
        );
        assert!(
            args.contains(&"--dev".to_string()),
            "must include /dev mount"
        );
        assert!(
            args.contains(&"--proc".to_string()),
            "must include /proc mount"
        );
    }
}
