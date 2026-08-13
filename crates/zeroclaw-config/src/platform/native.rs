use std::path::{Path, PathBuf};
#[cfg(not(target_os = "windows"))]
use zeroclaw_api::platform::is_android;
use zeroclaw_api::runtime_traits::{RuntimeAdapter, ShellDialect};

pub fn windows_cmd_shell_raw_arg(command: &str) -> String {
    format!("\"{command}\"")
}

/// Return whether `runtime.shell` names a PowerShell interpreter.
///
/// Matching is on the file name stem, case-insensitively, so `powershell`,
/// `PowerShell.exe`, `pwsh`, and `C:\Program Files\PowerShell\7\pwsh.exe` all
/// match. Every other value, including the cross-platform default `sh` and an
/// explicit `cmd`, does not.
///
/// Both `/` and `\` are treated as path separators regardless of the host OS
/// (so the classification is stable and unit-testable off Windows), and a
/// trailing `.exe` extension is stripped before matching. Batch files are not
/// PowerShell interpreters: Windows launches `.cmd`/`.bat` files through
/// `cmd.exe`, so classifying them as PowerShell would make validation and
/// execution use different shell languages.
fn is_powershell_interpreter(shell: &str) -> bool {
    let file = shell.rsplit(['/', '\\']).next().unwrap_or(shell);
    let stem = match file.rsplit_once('.') {
        Some((stem, ext)) if ext.eq_ignore_ascii_case("exe") => stem,
        _ => file,
    };
    matches!(stem.to_ascii_lowercase().as_str(), "powershell" | "pwsh")
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

/// Build a PowerShell process (`powershell` 5.x or `pwsh` 7+) that runs
/// `command` without loading profiles or prompting interactively.
///
/// `interpreter` is the configured shell string used verbatim as the
/// executable, so a bare name (`powershell`, `pwsh`) resolves via `PATH` while
/// an absolute path (e.g. a side-by-side `pwsh.exe`) is honoured directly.
///
/// `-NoProfile` skips user/host profile scripts for a predictable, faster
/// startup; `-NonInteractive` prevents the shell from blocking on prompts; and
/// `-Command` consumes the final argument as script text. Ordinary `arg`
/// handling keeps the entire script in one process argument and preserves its
/// internal PowerShell quoting.
fn tokio_powershell_command(interpreter: &str, command: &str) -> tokio::process::Command {
    let mut process = tokio::process::Command::new(interpreter);
    process
        .arg("-NoProfile")
        .arg("-NonInteractive")
        .arg("-Command")
        .arg(command);
    process
}

#[cfg(target_os = "windows")]
pub fn windows_tokio_powershell_command(
    interpreter: &str,
    command: &str,
) -> tokio::process::Command {
    let mut process = tokio_powershell_command(interpreter, command);
    process.creation_flags(CREATE_NO_WINDOW);
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
    /// Shell binary to invoke for command execution.
    ///
    /// Unix: POSIX interpreters are invoked as `<shell> -c "<command>"` (e.g.
    /// `"sh"`, `"bash"`, `"/bin/zsh"`). PowerShell interpreters use
    /// `-NoProfile -NonInteractive -Command` on every supported desktop host.
    ///
    /// Windows: [`RuntimeAdapter::shell_dialect`] selects the invocation
    /// convention — `cmd.exe /C` (default, and for the cross-platform default
    /// `sh`) or PowerShell (`powershell`/`pwsh`).
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
    ///
    /// Unix: `shell` is a path or name resolvable via `PATH`, e.g. `"bash"`,
    /// `"/bin/zsh"`, `"/usr/bin/fish"`, or `"pwsh"`. PowerShell names use
    /// the PowerShell invocation convention; other names use `-c`.
    ///
    /// Windows: `shell` selects the invocation convention — `powershell` or
    /// `pwsh` (bare name or absolute path) run through PowerShell; every other
    /// value runs through `cmd.exe /C`.
    pub fn with_shell(shell: String) -> Self {
        Self { shell }
    }
}

impl RuntimeAdapter for NativeRuntime {
    fn name(&self) -> &str {
        "native"
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

    fn shell_dialect(&self) -> ShellDialect {
        // Must match the shell `build_shell_command` actually spawns below:
        // a PowerShell interpreter when configured, else `cmd.exe /C` on
        // Windows and POSIX `sh -c` (incl. Android) everywhere else. This is
        // the only sink that reports `WindowsCmd` or `PowerShell`.
        #[cfg(not(target_os = "windows"))]
        if is_android() {
            return ShellDialect::Posix;
        }

        if is_powershell_interpreter(&self.shell) {
            return ShellDialect::PowerShell;
        }

        #[cfg(target_os = "windows")]
        return ShellDialect::WindowsCmd;

        #[cfg(not(target_os = "windows"))]
        {
            ShellDialect::Posix
        }
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
            let mut process = if self.shell_dialect() == ShellDialect::PowerShell {
                tokio_powershell_command(shell, command)
            } else {
                let mut process = tokio::process::Command::new(shell);
                process.arg("-c").arg(command);
                process
            };
            process.current_dir(workspace_dir);
            Ok(process)
        }

        #[cfg(target_os = "windows")]
        {
            let mut process = if self.shell_dialect() == ShellDialect::PowerShell {
                windows_tokio_powershell_command(&self.shell, command)
            } else {
                windows_tokio_cmd_shell_command(command)
            };
            process.current_dir(workspace_dir);
            Ok(process)
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[cfg(target_os = "windows")]
    #[test]
    fn native_shell_dialect_is_windows_cmd_on_windows() {
        // Native execution on Windows runs through `cmd.exe /C`, so the policy
        // must see `WindowsCmd` and accept the `nul` null device there.
        assert_eq!(
            NativeRuntime::new().shell_dialect(),
            ShellDialect::WindowsCmd
        );
    }

    #[cfg(not(target_os = "windows"))]
    #[test]
    fn native_shell_dialect_is_posix_off_windows() {
        // Unix native (including Android) runs through POSIX `sh -c`, where
        // `nul` is an ordinary filename — the policy must not treat it as safe.
        assert_eq!(NativeRuntime::new().shell_dialect(), ShellDialect::Posix);
    }

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
    #[cfg(not(target_os = "windows"))]
    fn native_reports_posix_shell_dialect() {
        assert_eq!(
            NativeRuntime::with_shell("bash".into()).shell_dialect(),
            ShellDialect::Posix
        );
    }

    #[test]
    fn configured_powershell_maps_to_powershell_policy_dialect() {
        assert_eq!(
            NativeRuntime::with_shell("pwsh".into()).shell_dialect(),
            ShellDialect::PowerShell
        );
    }

    #[test]
    #[cfg(all(unix, not(target_os = "android")))]
    fn unix_powershell_shell_uses_safe_invocation_args() {
        use std::ffi::OsStr;

        let script = r#"Write-Output "quoted safe value" | Select-Object -First 1"#;
        let cwd = std::env::temp_dir();
        let command = NativeRuntime::with_shell("pwsh".into())
            .build_shell_command(script, &cwd)
            .unwrap();
        let command = command.as_std();

        assert_eq!(command.get_program(), OsStr::new("pwsh"));
        let args: Vec<_> = command.get_args().collect();
        let expected = [
            OsStr::new("-NoProfile"),
            OsStr::new("-NonInteractive"),
            OsStr::new("-Command"),
            OsStr::new(script),
        ];
        assert_eq!(args.as_slice(), expected.as_slice());
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

        // On Windows the configured shell selects the interpreter family. A
        // `bash` value is not a PowerShell name, so it classifies as `Cmd` and
        // runs via `cmd.exe /C` — `bash` never appears as the program.
        #[cfg(target_os = "windows")]
        {
            assert!(
                debug.contains(WINDOWS_COMMAND_INTERPRETER)
                    && debug.contains(WINDOWS_COMMAND_EXECUTE_ARG),
                "Windows should use the cmd.exe /C boundary, got: {debug}"
            );
            assert!(
                !debug.contains("bash"),
                "Windows must ignore a non-PowerShell configured shell, got: {debug}"
            );
        }
    }

    // ── PowerShell interpreter recognition (runs on every platform) ────

    #[test]
    fn non_powershell_interpreters_do_not_match() {
        assert!(!is_powershell_interpreter("sh"));
        assert!(!is_powershell_interpreter("cmd"));
        assert!(!is_powershell_interpreter("cmd.exe"));
        assert!(!is_powershell_interpreter("bash"));
    }

    #[test]
    fn powershell_interpreter_names_match() {
        assert!(is_powershell_interpreter("powershell"));
        assert!(is_powershell_interpreter("pwsh"));
    }

    #[test]
    fn powershell_interpreter_match_is_case_insensitive() {
        assert!(is_powershell_interpreter("PowerShell.exe"));
        assert!(is_powershell_interpreter("PWSH.EXE"));
    }

    #[test]
    fn powershell_interpreter_strips_only_exe_suffix() {
        assert!(is_powershell_interpreter("powershell.exe"));
        assert!(is_powershell_interpreter("pwsh.exe"));
        assert!(!is_powershell_interpreter("powershell.cmd"));
        assert!(!is_powershell_interpreter("pwsh.bat"));
        assert!(!is_powershell_interpreter("pwsh.txt"));
        assert!(!is_powershell_interpreter("powershell.com"));
    }

    #[test]
    fn powershell_interpreter_handles_absolute_paths() {
        assert!(is_powershell_interpreter(
            r"C:\Program Files\PowerShell\7\pwsh.exe"
        ));
        assert!(is_powershell_interpreter(
            r"C:\Windows\System32\WindowsPowerShell\v1.0\powershell.exe"
        ));
        assert!(!is_powershell_interpreter(
            r"C:\Program Files\PowerShell\7\pwsh.bat"
        ));
        assert!(!is_powershell_interpreter(
            r"C:\Windows\System32\WindowsPowerShell\v1.0\powershell.cmd"
        ));
        assert!(!is_powershell_interpreter(r"C:\Windows\System32\cmd.exe"));
    }

    #[test]
    fn empty_interpreter_is_not_powershell() {
        // Empty/whitespace is rejected at construction; classification is total
        // and treats anything unrecognised as a non-PowerShell interpreter.
        assert!(!is_powershell_interpreter(""));
    }

    #[test]
    #[cfg(target_os = "windows")]
    fn windows_powershell_shell_builds_powershell_command() {
        let script = r#"Write-Output "quoted safe value" | Select-Object -First 1"#;
        let cwd = std::env::temp_dir();
        let cmd = NativeRuntime::with_shell("pwsh".into())
            .build_shell_command(script, &cwd)
            .unwrap();
        let debug = format!("{cmd:?}");
        assert!(
            debug.contains("pwsh"),
            "PowerShell interpreter should appear, got: {debug}"
        );
        assert!(
            debug.contains("-Command"),
            "PowerShell invocation must use -Command, got: {debug}"
        );
        assert!(
            debug.contains("quoted safe value"),
            "PowerShell invocation must contain the script argument, got: {debug}"
        );
        assert!(
            debug.contains("-NoProfile"),
            "PowerShell invocation should pass -NoProfile, got: {debug}"
        );
        assert!(
            !debug.contains("cmd.exe"),
            "PowerShell shell must not fall back to cmd.exe, got: {debug}"
        );
    }

    #[test]
    #[cfg(target_os = "windows")]
    fn windows_batch_shell_names_use_cmd_boundary() {
        let cwd = std::env::temp_dir();

        for shell in ["pwsh.bat", "powershell.cmd"] {
            let runtime = NativeRuntime::with_shell(shell.into());
            assert_eq!(runtime.shell_dialect(), ShellDialect::WindowsCmd);

            let cmd = runtime.build_shell_command("echo safe", &cwd).unwrap();
            let debug = format!("{cmd:?}");
            assert!(
                debug.contains(WINDOWS_COMMAND_INTERPRETER)
                    && debug.contains(WINDOWS_COMMAND_EXECUTE_ARG),
                "batch shell name must use cmd.exe /C, got: {debug}"
            );
            assert!(
                !debug.contains(shell),
                "batch shell name must not be spawned as PowerShell, got: {debug}"
            );
        }
    }

    #[tokio::test]
    #[cfg(target_os = "windows")]
    async fn windows_powershell_executes_command() {
        let cwd = std::env::temp_dir();
        let output = NativeRuntime::with_shell("powershell".into())
            .build_shell_command(
                r#"Write-Output "quoted safe value" | Select-Object -First 1"#,
                &cwd,
            )
            .unwrap()
            .output()
            .await
            .expect("powershell -Command should execute");

        assert!(output.status.success(), "powershell must exit 0");
        let stdout = String::from_utf8_lossy(&output.stdout);
        assert_eq!(stdout.trim(), "quoted safe value");
    }
}
