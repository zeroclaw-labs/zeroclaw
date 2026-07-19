use std::path::{Path, PathBuf};
#[cfg(not(target_os = "windows"))]
use zeroclaw_api::platform::is_android;
use zeroclaw_api::runtime_traits::RuntimeAdapter;

pub fn windows_cmd_shell_raw_arg(command: &str) -> String {
    format!("\"{command}\"")
}

#[cfg(target_os = "windows")]
const CREATE_NO_WINDOW: u32 = 0x08000000;
#[cfg(target_os = "windows")]
const WINDOWS_COMMAND_INTERPRETER: &str = "cmd.exe";
#[cfg(target_os = "windows")]
const WINDOWS_COMMAND_EXECUTE_ARG: &str = "/C";

#[cfg(target_os = "windows")]
pub fn windows_tokio_cmd_shell_command(command: &str) -> tokio::process::Command {
    let mut process = tokio::process::Command::new(WINDOWS_COMMAND_INTERPRETER);
    process
        .raw_arg(WINDOWS_COMMAND_EXECUTE_ARG)
        .raw_arg(windows_cmd_shell_raw_arg(command))
        .creation_flags(CREATE_NO_WINDOW);
    process
}

#[cfg(target_os = "windows")]
pub fn windows_std_cmd_shell_command(command: &str) -> std::process::Command {
    use std::os::windows::process::CommandExt;

    let mut process = std::process::Command::new(WINDOWS_COMMAND_INTERPRETER);
    process
        .raw_arg(WINDOWS_COMMAND_EXECUTE_ARG)
        .raw_arg(windows_cmd_shell_raw_arg(command))
        .creation_flags(CREATE_NO_WINDOW);
    process
}

/// Native runtime — full access, runs on Mac/Linux/Windows/Docker/Raspberry Pi
pub struct NativeRuntime {
    /// Shell binary to invoke for command execution (e.g. `"sh"`, `"bash"`).
    #[cfg(not(target_os = "windows"))]
    shell: String,
}

impl Default for NativeRuntime {
    fn default() -> Self {
        Self::new()
    }
}

impl NativeRuntime {
    /// Create a native runtime that uses the system default shell (`sh`).
    pub fn new() -> Self {
        Self::with_shell("sh".into())
    }

    /// Create a native runtime that uses a specific shell binary.
    /// `shell` should be a path or name resolvable via `PATH`,
    /// e.g. `"bash"`, `"/bin/zsh"`, `"/usr/bin/fish"`.
    pub fn with_shell(shell: String) -> Self {
        #[cfg(not(target_os = "windows"))]
        {
            Self { shell }
        }

        #[cfg(target_os = "windows")]
        {
            drop(shell);
            Self {}
        }
    }
}

impl RuntimeAdapter for NativeRuntime {
    fn name(&self) -> &str {
        "native"
    }

    fn has_shell_access(&self) -> bool {
        true
    }

    fn has_filesystem_access(&self) -> bool {
        true
    }

    fn storage_path(&self) -> PathBuf {
        directories::UserDirs::new().map_or_else(
            || PathBuf::from(".zeroclaw"),
            |u| u.home_dir().join(".zeroclaw"),
        )
    }

    fn supports_long_running(&self) -> bool {
        true
    }

    fn build_shell_command(
        &self,
        command: &str,
        workspace_dir: &Path,
    ) -> anyhow::Result<tokio::process::Command> {
        #[cfg(not(target_os = "windows"))]
        {
            // Android keeps its shell at /system/bin/sh and it is not always
            // on PATH for spawned processes; use the absolute path when present
            // so the shell can launch (and reach platform tools).
            // User-configured shell is ignored on Android.
            let shell = if is_android() {
                "/system/bin/sh"
            } else {
                &self.shell
            };
            let mut process = tokio::process::Command::new(shell);
            process.arg("-c").arg(command).current_dir(workspace_dir);
            Ok(process)
        }

        #[cfg(target_os = "windows")]
        {
            let mut process = windows_tokio_cmd_shell_command(command);
            process.current_dir(workspace_dir);
            Ok(process)
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn native_name() {
        assert_eq!(NativeRuntime::new().name(), "native");
    }

    #[test]
    fn native_has_shell_access() {
        assert!(NativeRuntime::new().has_shell_access());
    }

    #[test]
    fn native_has_filesystem_access() {
        assert!(NativeRuntime::new().has_filesystem_access());
    }

    #[test]
    fn native_supports_long_running() {
        assert!(NativeRuntime::new().supports_long_running());
    }

    #[test]
    fn native_memory_budget_unlimited() {
        assert_eq!(NativeRuntime::new().memory_budget(), 0);
    }

    #[test]
    fn native_storage_path_contains_zeroclaw() {
        let path = NativeRuntime::new().storage_path();
        assert!(path.to_string_lossy().contains("zeroclaw"));
    }

    #[test]
    fn native_builds_shell_command() {
        let cwd = std::env::temp_dir();
        let command = NativeRuntime::new()
            .build_shell_command("echo hello", &cwd)
            .unwrap();
        let debug = format!("{command:?}");
        assert!(debug.contains("echo hello"));
    }

    #[test]
    fn shell_command_preserves_double_quotes() {
        let cwd = std::env::temp_dir();
        let command = NativeRuntime::new()
            .build_shell_command(r#"dir "C:\Users\test\Desktop" /b"#, &cwd)
            .unwrap();
        let debug = format!("{command:?}");

        // The command string must contain the core command text.
        assert!(
            debug.contains("dir"),
            "debug output must contain the command, got: {debug}"
        );
        assert!(
            debug.contains("Desktop"),
            "debug output must contain the path, got: {debug}"
        );

        // On Windows, raw_arg must NOT produce backslash-escaped quotes
        // (the core issue in
        #[cfg(target_os = "windows")]
        {
            assert!(
                debug.contains(r#""C:\Users\test\Desktop""#),
                "Windows: double-quoted path must appear verbatim, got: {debug}"
            );
            assert!(
                !debug.contains(r#"\\\""#) && !debug.contains(r#"\""#),
                "Windows: must not contain backslash-escaped quotes, got: {debug}"
            );
        }
    }

    #[test]
    fn cmd_shell_raw_arg_wraps_command_for_verbatim_cmd_parsing() {
        assert_eq!(
            windows_cmd_shell_raw_arg(r#"dir "C:\Users\test\Desktop" /b"#),
            r#""dir "C:\Users\test\Desktop" /b""#
        );
    }

    #[test]
    fn cmd_shell_raw_arg_preserves_internal_quotes_and_operators() {
        assert_eq!(
            windows_cmd_shell_raw_arg(
                r#"dir "C:\path with spaces" /b 2>nul || echo "directory missing""#
            ),
            r#""dir "C:\path with spaces" /b 2>nul || echo "directory missing"""#
        );
    }

    #[test]
    fn shell_command_preserves_mixed_quoted_unquoted() {
        let cwd = std::env::temp_dir();
        let command = NativeRuntime::new()
            .build_shell_command(
                r#"dir "C:\path with spaces" /b 2>nul || echo "directory missing""#,
                &cwd,
            )
            .unwrap();
        let debug = format!("{command:?}");

        // The core command text and operators must be present.
        assert!(debug.contains("dir"), "missing dir command, got: {debug}");
        assert!(
            debug.contains("path with spaces"),
            "missing path, got: {debug}"
        );
        assert!(
            debug.contains("2>nul"),
            "redirect operator must be present, got: {debug}"
        );
        assert!(
            debug.contains("||"),
            "pipe operator must be present, got: {debug}"
        );
        assert!(
            debug.contains("directory missing"),
            "missing echo message, got: {debug}"
        );

        // On Windows, raw_arg must preserve quotes verbatim.
        #[cfg(target_os = "windows")]
        {
            assert!(
                debug.contains(r#""C:\path with spaces""#),
                "Windows: quoted path must appear verbatim, got: {debug}"
            );
            assert!(
                debug.contains(r#""directory missing""#),
                "Windows: quoted echo message must appear verbatim, got: {debug}"
            );
        }
    }

    #[tokio::test]
    #[cfg(target_os = "windows")]
    async fn windows_echo_quoted_argument_succeeds() {
        let cwd = std::env::temp_dir();
        let output = NativeRuntime::new()
            .build_shell_command(r#"echo "hello world""#, &cwd)
            .unwrap()
            .output()
            .await
            .expect("cmd /C echo should execute");

        assert!(output.status.success(), "cmd must exit 0");
        let stdout = String::from_utf8_lossy(&output.stdout);
        assert!(
            stdout.contains("hello world"),
            "quoted echo output mismatch, got: {stdout}"
        );
    }

    #[tokio::test]
    #[cfg(target_os = "windows")]
    async fn windows_dir_quoted_path_succeeds() {
        let cwd = std::env::temp_dir();
        let output = NativeRuntime::new()
            .build_shell_command(r#"dir "C:\Windows" /b"#, &cwd)
            .unwrap()
            .output()
            .await
            .expect("cmd /C dir should execute");

        assert!(output.status.success(), "cmd must exit 0");
        let stdout = String::from_utf8_lossy(&output.stdout);
        assert!(
            stdout.contains("explorer.exe") || stdout.contains("System32"),
            "dir should list C:\\Windows contents, got: {stdout}"
        );
    }

    #[test]
    fn shell_command_no_quotes_still_works() {
        let cwd = std::env::temp_dir();
        let command = NativeRuntime::new()
            .build_shell_command("echo hello_world", &cwd)
            .unwrap();
        let debug = format!("{command:?}");
        assert!(debug.contains("echo hello_world"));
    }

    #[tokio::test]
    #[cfg(target_os = "windows")]
    async fn windows_echo_percent_expansion_preserved() {
        let cwd = std::env::temp_dir();
        let output = NativeRuntime::new()
            .build_shell_command("echo %USERPROFILE%", &cwd)
            .unwrap()
            .output()
            .await
            .expect("cmd /C echo should execute");

        assert!(output.status.success(), "cmd must exit 0");
        let stdout = String::from_utf8_lossy(&output.stdout);
        assert!(
            stdout.contains(":\\"),
            "%%USERPROFILE%% should expand to a path, got: {stdout}"
        );
    }

    // ── Configurable shell tests ─────────────────────────────

    #[test]
    #[cfg(not(target_os = "windows"))]
    fn native_with_shell_defaults_to_sh() {
        let runtime = NativeRuntime::new();
        let cwd = std::env::temp_dir();
        let cmd = runtime.build_shell_command("echo hi", &cwd).unwrap();
        assert!(
            format!("{cmd:?}").contains("\"sh\""),
            "default shell should be 'sh', got: {cmd:?}"
        );
    }

    #[test]
    #[cfg(not(target_os = "windows"))]
    fn native_with_shell_bash() {
        let runtime = NativeRuntime::with_shell("bash".into());
        let cwd = std::env::temp_dir();
        let cmd = runtime.build_shell_command("echo hi", &cwd).unwrap();
        assert!(
            format!("{cmd:?}").contains("\"bash\""),
            "configured shell should appear in command debug, got: {cmd:?}"
        );
    }

    #[test]
    #[cfg(not(target_os = "windows"))]
    fn native_with_shell_absolute_path() {
        let runtime = NativeRuntime::with_shell("/usr/bin/zsh".into());
        let cwd = std::env::temp_dir();
        let cmd = runtime.build_shell_command("echo hi", &cwd).unwrap();
        assert!(
            format!("{cmd:?}").contains("/usr/bin/zsh"),
            "absolute path should appear verbatim, got: {cmd:?}"
        );
    }

    #[test]
    #[cfg(not(target_os = "windows"))]
    fn native_default_and_with_shell_are_different() {
        let default = NativeRuntime::new();
        let configured = NativeRuntime::with_shell("bash".into());
        let cwd = std::env::temp_dir();
        let default_debug = format!(
            "{:?}",
            default.build_shell_command("echo hi", &cwd).unwrap()
        );
        let configured_debug = format!(
            "{:?}",
            configured.build_shell_command("echo hi", &cwd).unwrap()
        );
        assert_ne!(
            default_debug, configured_debug,
            "default shell and configured shell should produce different commands"
        );
    }

    #[test]
    fn native_with_shell_passes_c_flag() {
        let runtime = NativeRuntime::with_shell("bash".into());
        let cwd = std::env::temp_dir();
        let cmd = runtime
            .build_shell_command("echo test_command", &cwd)
            .unwrap();
        let debug = format!("{cmd:?}");

        // The command string is preserved verbatim on every platform.
        assert!(
            debug.contains("echo test_command"),
            "command should contain the passed string, got: {debug}"
        );

        // On Unix the shell is invoked as `<shell> -c "<command>"`. Android
        // ignores the configured shell and pins `/system/bin/sh` (it is not on
        // PATH for spawned processes), so mirror that runtime branch here.
        #[cfg(not(target_os = "windows"))]
        {
            assert!(
                debug.contains("-c"),
                "shell command must use -c flag, got: {debug}"
            );
            if is_android() {
                assert!(
                    debug.contains("/system/bin/sh"),
                    "Android should pin /system/bin/sh, got: {debug}"
                );
                assert!(
                    !debug.contains("bash"),
                    "Android must ignore the configured shell, got: {debug}"
                );
            } else {
                assert!(
                    debug.contains("bash"),
                    "configured shell should be used, got: {debug}"
                );
            }
        }

        // On Windows the configured shell is ignored: commands run via
        // `cmd.exe /C` (see the [runtime].shell docs), so there is no `-c`
        // boundary and `bash` never appears.
        #[cfg(target_os = "windows")]
        {
            assert!(
                debug.contains(WINDOWS_COMMAND_INTERPRETER)
                    && debug.contains(WINDOWS_COMMAND_EXECUTE_ARG),
                "Windows should use the cmd.exe /C boundary, got: {debug}"
            );
            assert!(
                !debug.contains("bash"),
                "Windows must ignore the configured shell, got: {debug}"
            );
        }
    }
}
