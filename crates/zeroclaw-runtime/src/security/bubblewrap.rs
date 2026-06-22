//! Bubblewrap sandbox (user namespaces for Linux/macOS)

use crate::security::traits::Sandbox;
use std::process::Command;
use std::sync::OnceLock;

const CAPABILITY_DROPS: &[&str] = &["CAP_SYS_ADMIN", "CAP_SYS_PTRACE"];

#[derive(Debug, Clone, Copy, Default)]
struct BubblewrapHardeningSupport {
    cap_drop: bool,
}

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

    fn hardening_support() -> BubblewrapHardeningSupport {
        static SUPPORT: OnceLock<BubblewrapHardeningSupport> = OnceLock::new();
        *SUPPORT.get_or_init(Self::detect_hardening_support)
    }

    fn detect_hardening_support() -> BubblewrapHardeningSupport {
        let support = Command::new("bwrap")
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

    fn support_from_help(stdout: &str, stderr: &str) -> BubblewrapHardeningSupport {
        let contains = |flag| stdout.contains(flag) || stderr.contains(flag);

        BubblewrapHardeningSupport {
            cap_drop: contains("--cap-drop"),
        }
    }

    fn log_incomplete_hardening_support(support: BubblewrapHardeningSupport) {
        if support.cap_drop {
            return;
        }

        ::zeroclaw_log::record!(
            WARN,
            ::zeroclaw_log::Event::new(module_path!(), ::zeroclaw_log::Action::Note)
                .with_outcome(::zeroclaw_log::EventOutcome::Unknown)
                .with_attrs(::serde_json::json!({
                    "backend": "bubblewrap",
                    "cap_drop": support.cap_drop,
                })),
            "bubblewrap sandbox hardening support is incomplete"
        );
    }

    fn append_capability_drops(cmd: &mut Command, support: BubblewrapHardeningSupport) {
        if !support.cap_drop {
            return;
        }

        for capability in CAPABILITY_DROPS {
            cmd.args(["--cap-drop", capability]);
        }
    }

    fn wrap_command_with_support(
        &self,
        cmd: &mut Command,
        support: BubblewrapHardeningSupport,
    ) -> std::io::Result<()> {
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
        Self::append_capability_drops(&mut bwrap_cmd, support);
        // Conditionally bind dynamic-loader library directories that may exist
        // on the host.  On Fedora / RHEL systems the ELF interpreter and shared
        // libraries live in /lib64; on some older or non-merged-usr distros they
        // live in /lib.  Without these paths inside the sandbox, any dynamically
        // linked executable (including `cargo`) will fail to start even when its
        // binary is reachable.
        for lib_dir in &["/lib64", "/lib"] {
            if std::path::Path::new(lib_dir).exists() {
                bwrap_cmd.args(["--ro-bind", lib_dir, lib_dir]);
            }
        }
        bwrap_cmd.arg(&program);
        bwrap_cmd.args(&args);

        *cmd = bwrap_cmd;
        Ok(())
    }
}

impl Sandbox for BubblewrapSandbox {
    fn wrap_command(&self, cmd: &mut Command) -> std::io::Result<()> {
        self.wrap_command_with_support(cmd, Self::hardening_support())
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

    fn args(cmd: &Command) -> Vec<String> {
        cmd.get_args()
            .map(|s| s.to_string_lossy().to_string())
            .collect()
    }

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

        let args = args(&cmd);

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
    fn bubblewrap_supported_capability_drops_include_admin_and_ptrace() {
        let mut cmd = Command::new("bwrap");
        BubblewrapSandbox::append_capability_drops(
            &mut cmd,
            BubblewrapHardeningSupport { cap_drop: true },
        );

        let args = args(&cmd);
        let expected_capability_drops = ["CAP_SYS_ADMIN", "CAP_SYS_PTRACE"];
        for capability in expected_capability_drops {
            assert!(
                args.windows(2)
                    .any(|window| window == ["--cap-drop", capability]),
                "supported bubblewrap hardening must drop {capability}"
            );
        }
    }

    #[test]
    fn bubblewrap_skips_capability_drops_when_not_advertised() {
        let mut cmd = Command::new("bwrap");
        BubblewrapSandbox::append_capability_drops(
            &mut cmd,
            BubblewrapHardeningSupport { cap_drop: false },
        );

        assert!(
            args(&cmd).is_empty(),
            "unsupported bubblewrap capability drops should not be appended"
        );
    }

    #[test]
    fn bubblewrap_support_from_help_detects_stdout_and_stderr_flags() {
        assert!(BubblewrapSandbox::support_from_help("--cap-drop", "").cap_drop);
        assert!(BubblewrapSandbox::support_from_help("", "--cap-drop").cap_drop);
        assert!(!BubblewrapSandbox::support_from_help("", "").cap_drop);
    }

    #[test]
    fn bubblewrap_wrap_command_applies_supported_capability_drops() {
        let sandbox = BubblewrapSandbox;
        let mut cmd = Command::new("echo");
        sandbox
            .wrap_command_with_support(&mut cmd, BubblewrapHardeningSupport { cap_drop: true })
            .unwrap();

        let args = args(&cmd);
        let expected_capability_drops = ["CAP_SYS_ADMIN", "CAP_SYS_PTRACE"];
        for capability in expected_capability_drops {
            assert!(
                args.windows(2)
                    .any(|window| window == ["--cap-drop", capability]),
                "wrap_command must apply supported bubblewrap drop for {capability}"
            );
        }
    }

    #[test]
    fn bubblewrap_wrap_command_does_not_add_bare_seccomp_without_profile_fd() {
        let sandbox = BubblewrapSandbox;
        let mut cmd = Command::new("echo");
        sandbox.wrap_command(&mut cmd).unwrap();

        assert!(
            !args(&cmd).contains(&"--seccomp".to_string()),
            "bubblewrap --seccomp requires a seccomp BPF profile fd and must not be added bare"
        );
    }

    #[test]
    fn bubblewrap_wrap_command_preserves_original_command() {
        let sandbox = BubblewrapSandbox;
        let mut cmd = Command::new("ls");
        cmd.arg("-la");
        cmd.arg("/tmp");
        sandbox.wrap_command(&mut cmd).unwrap();

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
            args.contains(&"/tmp".to_string()),
            "original args must be preserved"
        );
    }

    #[test]
    fn bubblewrap_wrap_command_conditionally_binds_lib_dirs() {
        // /lib64 and /lib must be bind-mounted (with --ro-bind src dst) when
        // they exist on the host so that dynamically linked binaries (e.g.
        // `cargo`) can find the ELF interpreter and shared libraries inside the
        // sandbox.
        let sandbox = BubblewrapSandbox;
        let mut cmd = Command::new("echo");
        sandbox.wrap_command(&mut cmd).unwrap();

        let args = args(&cmd);

        for lib_dir in &["/lib64", "/lib"] {
            if std::path::Path::new(lib_dir).exists() {
                // Verify the triplet `--ro-bind <dir> <dir>` is present and in
                // the correct order; a bare path without the flag would be silently
                // ignored by bwrap and leave the sandbox broken.
                let has_ro_bind_triplet = args
                    .windows(3)
                    .any(|w| w[0] == "--ro-bind" && w[1] == *lib_dir && w[2] == *lib_dir);
                assert!(
                    has_ro_bind_triplet,
                    "{lib_dir} exists on host but --ro-bind {lib_dir} {lib_dir} \
                     is missing from bwrap args — dynamically linked binaries \
                     will fail inside the sandbox"
                );
            }
        }
    }

    #[test]
    fn bubblewrap_wrap_command_binds_required_paths() {
        let sandbox = BubblewrapSandbox;
        let mut cmd = Command::new("echo");
        sandbox.wrap_command(&mut cmd).unwrap();

        let args = args(&cmd);

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
