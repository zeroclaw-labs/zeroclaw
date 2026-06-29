use std::path::{Path, PathBuf};
#[cfg(not(target_os = "windows"))]
use zeroclaw_api::platform::is_android;
use zeroclaw_api::runtime_traits::RuntimeAdapter;

/// Command-line argument passed after `cmd.exe /C`.
///
/// The outer quotes make `cmd.exe` receive the whole configured command as one
/// command string, while internal quotes remain verbatim for paths and args
/// with spaces. This preserves the #7083 quoting contract for all Windows
/// platform-shell call sites.
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
pub struct NativeRuntime;

impl Default for NativeRuntime {
    fn default() -> Self {
        Self::new()
    }
}

impl NativeRuntime {
    pub fn new() -> Self {
        Self
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
            let shell = if is_android() { "/system/bin/sh" } else { "sh" };
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
}
