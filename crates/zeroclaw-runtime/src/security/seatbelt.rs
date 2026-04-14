//! macOS sandbox-exec (Seatbelt) sandbox backend.
//!
//! Uses Apple's built-in `sandbox-exec` tool to enforce per-session Seatbelt
//! profiles that restrict network access, filesystem writes, and process
//! spawning. Policy files are generated in `.sb` format and written to a
//! temporary directory that is cleaned up when the sandbox is dropped.

use crate::security::traits::Sandbox;
use std::path::{Path, PathBuf};
use std::process::Command;

/// macOS sandbox-exec (Seatbelt) sandbox backend.
///
/// Generates per-session `.sb` policy files and wraps commands with
/// `sandbox-exec -f <policy>`. The policy denies network and filesystem
/// writes by default, allowing only the workspace directory.
#[derive(Debug, Clone)]
pub struct SeatbeltSandbox {
    /// Directory where per-session policy files are stored.
    policy_dir: PathBuf,
    /// Path to the generated policy file for this session.
    policy_path: PathBuf,
}

impl SeatbeltSandbox {
    /// Create a new Seatbelt sandbox, generating a per-session policy file.
    ///
    /// Returns an error if `sandbox-exec` is not available or the policy file
    /// cannot be written.
    pub fn new() -> std::io::Result<Self> {
        if !Self::is_installed() {
            return Err(std::io::Error::new(
                std::io::ErrorKind::NotFound,
                "sandbox-exec not found (requires macOS)",
            ));
        }

        let policy_dir = std::env::temp_dir().join("zeroclaw-seatbelt");
        std::fs::create_dir_all(&policy_dir)?;

        let session_id = uuid::Uuid::new_v4();
        let policy_path = policy_dir.join(format!("{session_id}.sb"));

        let workspace = std::env::current_dir().unwrap_or_else(|_| PathBuf::from("/tmp"));
        let policy = generate_policy(&workspace);
        std::fs::write(&policy_path, &policy)?;

        Ok(Self {
            policy_dir,
            policy_path,
        })
    }

    /// Probe if sandbox-exec is available (for auto-detection).
    pub fn probe() -> std::io::Result<Self> {
        Self::new()
    }

    /// Check if `sandbox-exec` is available on this system.
    fn is_installed() -> bool {
        // sandbox-exec is a built-in macOS binary at /usr/bin/sandbox-exec
        Path::new("/usr/bin/sandbox-exec").exists()
            || Command::new("sandbox-exec")
                .arg("-n")
                .arg("no-network")
                .arg("true")
                .output()
                .map(|o| o.status.success())
                .unwrap_or(false)
    }

    /// Return the path to the generated policy file.
    pub fn policy_path(&self) -> &Path {
        &self.policy_path
    }

    /// Return the policy directory path.
    pub fn policy_dir(&self) -> &Path {
        &self.policy_dir
    }
}

impl Drop for SeatbeltSandbox {
    fn drop(&mut self) {
        // Clean up the per-session policy file
        let _ = std::fs::remove_file(&self.policy_path);
    }
}

impl Sandbox for SeatbeltSandbox {
    fn wrap_command(&self, cmd: &mut Command) -> std::io::Result<()> {
        let program = cmd.get_program().to_string_lossy().to_string();
        let args: Vec<String> = cmd
            .get_args()
            .map(|s| s.to_string_lossy().to_string())
            .collect();

        let mut sandbox_cmd = Command::new("sandbox-exec");
        sandbox_cmd.arg("-f");
        sandbox_cmd.arg(&self.policy_path);
        sandbox_cmd.arg(&program);
        sandbox_cmd.args(&args);

        *cmd = sandbox_cmd;
        Ok(())
    }

    fn is_available(&self) -> bool {
        Self::is_installed() && self.policy_path.exists()
    }

    fn name(&self) -> &str {
        "sandbox-exec"
    }

    fn description(&self) -> &str {
        "macOS Seatbelt sandbox (built-in sandbox-exec)"
    }
}

/// Generate a Seatbelt `.sb` policy with restrictive defaults.
///
/// The policy:
/// - Denies all network operations by default
/// - Allows DNS lookups and outbound connections to localhost only
/// - Denies filesystem writes outside the workspace and temp directories
/// - Allows reads to system paths required for process execution
/// - Restricts process spawning to essential operations
fn generate_policy(workspace: &Path) -> String {
    let workspace_str = workspace.to_string_lossy();
    format!(
        r#"(version 1)

;; Deny everything by default
(deny default)

;; ── Process execution ──────────────────────────────────────
;; Allow basic process operations needed for command execution
(allow process-exec)
(allow process-fork)
(allow signal (target self))

;; ── Filesystem reads ───────────────────────────────────────
;; Allow reading system libraries, frameworks, and executables
(allow file-read*
    (subpath "/usr")
    (subpath "/bin")
    (subpath "/sbin")
    (subpath "/Library")
    (subpath "/System")
    (subpath "/private/var")
    (subpath "/dev")
    (subpath "/etc")
    (subpath "/Applications")
    (subpath "/opt")
    (subpath "/nix")
    (literal "/")
    (subpath "/var"))

;; Allow reading the workspace
(allow file-read* (subpath "{workspace}"))

;; Allow reading temp directories (needed for policy file itself)
(allow file-read* (subpath "/tmp"))
(allow file-read* (subpath "/private/tmp"))
(allow file-read*
    (regex #"^/private/var/folders/"))

;; Allow reading user home for tool configs
(allow file-read*
    (regex #"^/Users/[^/]+/\\."))

;; ── Filesystem writes ──────────────────────────────────────
;; Only allow writes to workspace and temp directories
(allow file-write*
    (subpath "{workspace}"))
(allow file-write*
    (subpath "/tmp")
    (subpath "/private/tmp"))
(allow file-write*
    (regex #"^/private/var/folders/"))
(allow file-write* (subpath "/dev/null"))
(allow file-write* (subpath "/dev/tty"))

;; ── Network ────────────────────────────────────────────────
;; Deny all network by default (inherited from deny default)
;; Allow DNS resolution only
(allow network-outbound
    (remote unix-socket (path-literal "/var/run/mDNSResponder")))
(allow system-socket)

;; Allow localhost connections only (for local dev servers).
;; Note: macOS sandbox-exec only accepts "localhost:*" or "*:port" in
;; (remote ip ...) filters — raw IP addresses cause the entire policy
;; to fail to parse.
(allow network-outbound
    (remote ip "localhost:*"))

;; ── Mach / IPC ─────────────────────────────────────────────
;; Allow basic mach services needed for process execution
(allow mach-lookup
    (global-name "com.apple.system.logger")
    (global-name "com.apple.system.notification_center")
    (global-name "com.apple.SecurityServer")
    (global-name "com.apple.CoreServices.coreservicesd"))

;; ── Sysctl / misc ──────────────────────────────────────────
(allow sysctl-read)
(allow mach-task-name)
"#,
        workspace = workspace_str,
    )
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn seatbelt_sandbox_name() {
        let sandbox = SeatbeltSandbox {
            policy_dir: PathBuf::from("/tmp/test-seatbelt"),
            policy_path: PathBuf::from("/tmp/test-seatbelt/test.sb"),
        };
        assert_eq!(sandbox.name(), "sandbox-exec");
    }

    #[test]
    fn seatbelt_description_mentions_macos() {
        let sandbox = SeatbeltSandbox {
            policy_dir: PathBuf::from("/tmp/test-seatbelt"),
            policy_path: PathBuf::from("/tmp/test-seatbelt/test.sb"),
        };
        assert!(sandbox.description().contains("macOS"));
        assert!(sandbox.description().contains("Seatbelt"));
    }

    #[test]
    fn generate_policy_contains_workspace_path() {
        let workspace = PathBuf::from("/Users/test/project");
        let policy = generate_policy(&workspace);
        assert!(policy.contains("/Users/test/project"));
    }

    #[test]
    fn generate_policy_denies_by_default() {
        let workspace = PathBuf::from("/tmp/workspace");
        let policy = generate_policy(&workspace);
        assert!(policy.contains("(deny default)"));
    }

    #[test]
    fn generate_policy_allows_workspace_writes() {
        let workspace = PathBuf::from("/home/user/code");
        let policy = generate_policy(&workspace);
        assert!(policy.contains("(allow file-write*"));
        assert!(policy.contains("/home/user/code"));
    }

    #[test]
    fn generate_policy_restricts_network() {
        let workspace = PathBuf::from("/tmp/workspace");
        let policy = generate_policy(&workspace);
        assert!(policy.contains("localhost"));
        assert!(!policy.contains("127.0.0.1"));
        assert!(!policy.contains("(allow network*)"));
    }

    #[test]
    fn generate_policy_allows_system_reads() {
        let workspace = PathBuf::from("/tmp/workspace");
        let policy = generate_policy(&workspace);
        assert!(policy.contains("(subpath \"/usr\")"));
        assert!(policy.contains("(subpath \"/bin\")"));
        assert!(policy.contains("(subpath \"/System\")"));
    }

    #[test]
    fn generate_policy_allows_process_execution() {
        let workspace = PathBuf::from("/tmp/workspace");
        let policy = generate_policy(&workspace);
        assert!(policy.contains("(allow process-exec)"));
        assert!(policy.contains("(allow process-fork)"));
    }

    #[test]
    fn seatbelt_wrap_command_prepends_sandbox_exec() {
        let dir = tempfile::tempdir().unwrap();
        let policy_path = dir.path().join("test.sb");
        std::fs::write(&policy_path, "(version 1)\n(deny default)").unwrap();

        let sandbox = SeatbeltSandbox {
            policy_dir: dir.path().to_path_buf(),
            policy_path: policy_path.clone(),
        };

        let mut cmd = Command::new("echo");
        cmd.arg("hello");
        sandbox.wrap_command(&mut cmd).unwrap();

        assert_eq!(cmd.get_program().to_string_lossy(), "sandbox-exec");
        let args: Vec<String> = cmd
            .get_args()
            .map(|s| s.to_string_lossy().to_string())
            .collect();
        assert!(args.contains(&"-f".to_string()));
        assert!(args.contains(&policy_path.to_string_lossy().to_string()));
        assert!(args.contains(&"echo".to_string()));
        assert!(args.contains(&"hello".to_string()));
    }

    #[test]
    fn seatbelt_wrap_command_preserves_original_args() {
        let dir = tempfile::tempdir().unwrap();
        let policy_path = dir.path().join("test.sb");
        std::fs::write(&policy_path, "(version 1)").unwrap();

        let sandbox = SeatbeltSandbox {
            policy_dir: dir.path().to_path_buf(),
            policy_path,
        };

        let mut cmd = Command::new("ls");
        cmd.arg("-la");
        cmd.arg("/workspace");
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
            args.contains(&"/workspace".to_string()),
            "original args must be preserved"
        );
    }

    #[test]
    fn seatbelt_policy_file_cleanup_on_drop() {
        let dir = tempfile::tempdir().unwrap();
        let policy_path = dir.path().join("session.sb");
        std::fs::write(&policy_path, "(version 1)").unwrap();
        assert!(policy_path.exists());

        {
            let _sandbox = SeatbeltSandbox {
                policy_dir: dir.path().to_path_buf(),
                policy_path: policy_path.clone(),
            };
        }

        assert!(
            !policy_path.exists(),
            "policy file should be cleaned up on drop"
        );
    }

    #[test]
    fn seatbelt_new_fails_if_not_installed() {
        let result = SeatbeltSandbox::new();
        match result {
            Ok(sandbox) => {
                assert_eq!(sandbox.name(), "sandbox-exec");
                assert!(sandbox.policy_path().exists());
            }
            Err(e) => {
                assert!(
                    e.kind() == std::io::ErrorKind::NotFound
                        || e.kind() == std::io::ErrorKind::PermissionDenied
                );
            }
        }
    }

    #[test]
    fn seatbelt_is_available_checks_policy_file() {
        let dir = tempfile::tempdir().unwrap();
        let policy_path = dir.path().join("test.sb");

        let sandbox = SeatbeltSandbox {
            policy_dir: dir.path().to_path_buf(),
            policy_path: policy_path.clone(),
        };

        if Path::new("/usr/bin/sandbox-exec").exists() {
            assert!(
                !sandbox.is_available(),
                "should be false without policy file"
            );
        }

        std::fs::write(&policy_path, "(version 1)").unwrap();
        if Path::new("/usr/bin/sandbox-exec").exists() {
            assert!(sandbox.is_available(), "should be true with policy file");
        }
    }

    #[test]
    fn generate_policy_is_valid_sb_format() {
        let workspace = PathBuf::from("/tmp/workspace");
        let policy = generate_policy(&workspace);
        assert!(policy.starts_with("(version 1)"));
        let open = policy.chars().filter(|c| *c == '(').count();
        let close = policy.chars().filter(|c| *c == ')').count();
        assert_eq!(open, close, "parentheses must be balanced in .sb policy");
    }
}
