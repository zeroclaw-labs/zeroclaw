use std::path::{Path, PathBuf};
#[cfg(not(target_os = "windows"))]
use zeroclaw_api::platform::is_android;
use zeroclaw_api::runtime_traits::RuntimeAdapter;

/// Native runtime — full access, runs on Mac/Linux/Windows/Docker/Raspberry Pi
pub struct NativeRuntime {
    /// Shell binary to invoke for command execution (e.g. `"sh"`, `"bash"`).
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
        Self { shell: "sh".into() }
    }

    /// Create a native runtime that uses a specific shell binary.
    ///
    /// `shell` should be a path or name resolvable via `PATH`,
    /// e.g. `"bash"`, `"/bin/zsh"`, `"/usr/bin/fish"`.
    pub fn with_shell(shell: String) -> Self {
        Self { shell }
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
            // Windows always uses cmd.exe regardless of the configured shell.
            const CREATE_NO_WINDOW: u32 = 0x08000000;

            let mut process = tokio::process::Command::new("cmd.exe");
            // Use raw_arg so the command string is passed verbatim to cmd.exe,
            // bypassing Rust's CommandLineToArgvW escaping which would mangle
            // embedded double quotes with backslash escapes that cmd doesn't
            // understand (see #7083).
            process
                .raw_arg("/C")
                .raw_arg(format!("\"{command}\""))
                .current_dir(workspace_dir)
                .creation_flags(CREATE_NO_WINDOW);
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

    /// On Windows, `std::process::Command` applies `CommandLineToArgvW`
    /// escaping to each `.arg()`, which mangles embedded double quotes
    /// with backslash escapes that `cmd.exe` does not understand.
    /// `raw_arg` must pass the command verbatim so that quoted paths
    /// and arguments survive intact (see #7083).
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
        // (the core issue in #7083).
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

    /// A command with mixed quoted and unquoted segments must pass
    /// through without mangling any part of the command line.
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

    /// On Windows, actually invoke `cmd /C` with a quoted `echo`
    /// argument to confirm the fix works end-to-end. Skipped on
    /// non-Windows hosts since there's no `cmd.exe`.
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

    /// On Windows, verify `dir` with a quoted path works (previous
    /// behavior: "The filename, directory name, or volume label
    /// syntax is incorrect").
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

    /// Verify a command with entirely unquoted arguments still works
    /// (regression check for the raw_arg conversion).
    #[test]
    fn shell_command_no_quotes_still_works() {
        let cwd = std::env::temp_dir();
        let command = NativeRuntime::new()
            .build_shell_command("echo hello_world", &cwd)
            .unwrap();
        let debug = format!("{command:?}");
        assert!(debug.contains("echo hello_world"));
    }

    /// Verify `echo %VAR%` expansion syntax is preserved verbatim
    /// and not mangled by escaping.
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
    fn native_with_shell_defaults_to_sh() {
        let runtime = NativeRuntime::new();
        let cwd = std::env::temp_dir();
        let cmd = runtime.build_shell_command("echo hi", &cwd).unwrap();
        #[cfg(not(target_os = "windows"))]
        assert!(
            format!("{cmd:?}").contains("\"sh\""),
            "default shell should be 'sh', got: {cmd:?}"
        );
    }

    #[test]
    fn native_with_shell_bash() {
        let runtime = NativeRuntime::with_shell("bash".into());
        let cwd = std::env::temp_dir();
        let cmd = runtime.build_shell_command("echo hi", &cwd).unwrap();
        #[cfg(not(target_os = "windows"))]
        assert!(
            format!("{cmd:?}").contains("\"bash\""),
            "configured shell should appear in command debug, got: {cmd:?}"
        );
    }

    #[test]
    fn native_with_shell_absolute_path() {
        let runtime = NativeRuntime::with_shell("/usr/bin/zsh".into());
        let cwd = std::env::temp_dir();
        let cmd = runtime.build_shell_command("echo hi", &cwd).unwrap();
        #[cfg(not(target_os = "windows"))]
        assert!(
            format!("{cmd:?}").contains("/usr/bin/zsh"),
            "absolute path should appear verbatim, got: {cmd:?}"
        );
    }

    #[test]
    fn native_default_and_with_shell_are_different() {
        let default = NativeRuntime::new();
        let configured = NativeRuntime::with_shell("bash".into());
        let cwd = std::env::temp_dir();
        #[cfg(not(target_os = "windows"))]
        {
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
        #[cfg(unix)]
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
        #[cfg(windows)]
        {
            assert!(
                debug.contains("cmd.exe") && debug.contains("/C"),
                "Windows should use the cmd.exe /C boundary, got: {debug}"
            );
            assert!(
                !debug.contains("bash"),
                "Windows must ignore the configured shell, got: {debug}"
            );
        }
    }
}
