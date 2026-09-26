use std::path::{Path, PathBuf};
#[cfg(not(target_os = "windows"))]
use zeroclaw_api::platform::is_android;
use zeroclaw_api::runtime_traits::{RuntimeAdapter, ShellDialect, ShellProfile};

/// Resolve the platform default shell when `runtime.shell` is omitted.
/// Candidates are only inspected, never executed.
pub fn default_shell() -> String {
    default_shell_for_platform()
}

#[cfg(target_os = "windows")]
fn default_shell_for_platform() -> String {
    first_available(["pwsh", "powershell"], shell_is_available)
        .unwrap_or_else(|| "cmd.exe".to_string())
}

#[cfg(target_os = "android")]
fn default_shell_for_platform() -> String {
    "/system/bin/sh".to_string()
}

#[cfg(any(target_os = "macos", target_os = "linux"))]
fn default_shell_for_platform() -> String {
    #[cfg(target_os = "macos")]
    let fallback = ["zsh", "bash", "/bin/sh"];

    #[cfg(target_os = "linux")]
    let fallback = ["bash", "zsh", "/bin/sh"];

    if let Some(login_shell) = login_shell()
        && is_supported_login_shell(&login_shell)
        && shell_is_available(&login_shell)
    {
        return login_shell;
    }

    first_available(fallback, shell_is_available)
        .unwrap_or_else(|| fallback[fallback.len() - 1].to_string())
}

#[cfg(all(
    unix,
    not(any(target_os = "android", target_os = "macos", target_os = "linux"))
))]
fn default_shell_for_platform() -> String {
    first_available(["sh"], shell_is_available).unwrap_or_else(|| "sh".to_string())
}

#[cfg(not(any(unix, target_os = "windows")))]
fn default_shell_for_platform() -> String {
    "sh".to_string()
}

fn first_available<'a>(
    candidates: impl IntoIterator<Item = &'a str>,
    mut available: impl FnMut(&str) -> bool,
) -> Option<String> {
    candidates
        .into_iter()
        .find(|candidate| available(candidate))
        .map(str::to_owned)
}

/// Return whether a passwd login-shell name maps to a dialect understood by
/// the native runtime.  Explicit `runtime.shell` values retain the broader
/// historical Unix validation; this allowlist only prevents service shells
/// and unsupported interactive shells from becoming an implicit default.
#[cfg(any(test, target_os = "macos", target_os = "linux"))]
fn is_supported_login_shell(shell: &str) -> bool {
    matches!(
        shell_stem(shell).to_ascii_lowercase().as_str(),
        "sh" | "bash" | "zsh" | "ksh" | "dash" | "ash" | "powershell" | "pwsh"
    )
}

#[cfg(unix)]
fn shell_is_available(shell: &str) -> bool {
    use std::os::unix::fs::PermissionsExt;

    let path = Path::new(shell);
    let resolved = if path.is_absolute() {
        path.to_path_buf()
    } else if path.components().count() == 1 {
        match std::env::split_paths(&std::env::var_os("PATH").unwrap_or_default())
            .map(|dir| dir.join(shell))
            .find(|candidate| candidate.is_file())
        {
            Some(found) => found,
            None => return false,
        }
    } else {
        return false;
    };

    resolved.is_file()
        && resolved
            .metadata()
            .map(|metadata| metadata.permissions().mode() & 0o111 != 0)
            .unwrap_or(false)
}

#[cfg(windows)]
fn shell_is_available(shell: &str) -> bool {
    let path = Path::new(shell);
    if path.is_absolute() {
        return path.is_file();
    }
    let path_var = std::env::var_os("PATH").unwrap_or_default();
    std::env::split_paths(&path_var).any(|dir| {
        [shell.to_string(), format!("{shell}.exe")]
            .iter()
            .map(|name| dir.join(name))
            .any(|candidate| candidate.is_file())
    })
}

#[cfg(not(any(unix, windows)))]
fn shell_is_available(_shell: &str) -> bool {
    false
}

#[cfg(any(target_os = "macos", target_os = "linux"))]
fn login_shell() -> Option<String> {
    use std::ffi::CStr;
    use std::os::raw::c_char;
    use std::ptr;

    let mut capacity = 1024usize;
    loop {
        let mut buffer = vec![0u8; capacity];
        let mut passwd = unsafe { std::mem::zeroed::<libc::passwd>() };
        let mut result = ptr::null_mut();
        let status = unsafe {
            libc::getpwuid_r(
                libc::getuid(),
                &mut passwd,
                buffer.as_mut_ptr().cast::<c_char>(),
                buffer.len(),
                &mut result,
            )
        };

        if status == 0 {
            if result.is_null() || passwd.pw_shell.is_null() {
                return None;
            }
            let shell = unsafe { CStr::from_ptr(passwd.pw_shell) }
                .to_str()
                .ok()?
                .trim();
            return (!shell.is_empty()).then(|| shell.to_string());
        }
        if status != libc::ERANGE || capacity >= 1024 * 1024 {
            return None;
        }
        capacity *= 2;
    }
}

pub fn windows_cmd_shell_raw_arg(command: &str) -> String {
    format!("\"{command}\"")
}

/// Return the bare interpreter name of a configured shell: the final path
/// component, with a trailing `.exe` removed.
///
/// Both `/` and `\` are treated as separators regardless of the host OS, so
/// `/usr/bin/zsh` reduces to `zsh` and
/// `C:\Program Files\PowerShell\7\pwsh.exe` to `pwsh`. Shared by interpreter
/// classification and prompt reporting so the two read the same name out of
/// one configured string. Case is preserved; callers that compare fold it
/// themselves.
fn shell_stem(shell: &str) -> &str {
    let file = shell.rsplit(['/', '\\']).next().unwrap_or(shell);
    match file.rsplit_once('.') {
        Some((stem, ext)) if ext.eq_ignore_ascii_case("exe") => stem,
        _ => file,
    }
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
    matches!(
        shell_stem(shell).to_ascii_lowercase().as_str(),
        "powershell" | "pwsh"
    )
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
/// `-Command` consumes the final argument as script text. Commands in the
/// policy's bounded PowerShell grammar receive a UTF-8 setup statement inside
/// an empty `try`/`catch`, so unsupported settings never prevent the requested
/// command from running. Full scripts are passed through unchanged: prepending
/// a statement would invalidate leading declarations or named blocks, while a
/// nested script-block wrapper would change scope and process-exit semantics.
fn tokio_powershell_command(interpreter: &str, command: &str) -> tokio::process::Command {
    let script = powershell_script_with_utf8_setup(command);
    let mut process = tokio::process::Command::new(interpreter);
    process
        .arg("-NoProfile")
        .arg("-NonInteractive")
        .arg("-Command")
        .arg(script);
    process
}

const POWERSHELL_UTF8_SETUP: &str = "try {\n    $utf8 = [System.Text.UTF8Encoding]::new($false)\n    [Console]::OutputEncoding = $utf8\n    $OutputEncoding = $utf8\n} catch {\n}";

fn powershell_script_with_utf8_setup(command: &str) -> String {
    if crate::policy::powershell_command_supports_statement_prelude(command) {
        format!("{POWERSHELL_UTF8_SETUP}\n{command}")
    } else {
        command.to_owned()
    }
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
    /// convention — `cmd.exe /C` or PowerShell (`powershell`/`pwsh`).
    shell: String,
}

impl Default for NativeRuntime {
    fn default() -> Self {
        Self::new()
    }
}

impl NativeRuntime {
    /// Create a native runtime using the platform's resolved default shell.
    pub fn new() -> Self {
        Self::with_shell(default_shell())
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

    fn shell_profile(&self) -> Option<ShellProfile> {
        // Report the interpreter `build_shell_command` will actually spawn,
        // named the way the operator configured it. Variants within a dialect
        // differ enough to be worth naming: `bash` vs `zsh` under POSIX, and
        // `pwsh` (7+) vs `powershell` (5.1) under PowerShell, which disagree
        // on ternaries, `&&`/`||` pipeline chains, and several parameters.
        let dialect = self.shell_dialect();
        match dialect {
            // Native execution on Windows always routes through `cmd.exe /C`
            // regardless of the configured value (the cross-platform default
            // `sh` lands here), so the configured name would misreport it.
            ShellDialect::WindowsCmd | ShellDialect::None => ShellProfile::from_dialect(dialect),
            ShellDialect::Posix | ShellDialect::PowerShell => {
                // Android pins execution to /system/bin/sh and ignores the
                // configured value; reporting that value would name a shell
                // that never runs.
                #[cfg(not(target_os = "windows"))]
                if is_android() {
                    return ShellProfile::from_dialect(ShellDialect::Posix);
                }

                Some(ShellProfile {
                    name: shell_stem(&self.shell).to_ascii_lowercase(),
                    dialect,
                })
            }
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

    #[test]
    fn default_candidates_skip_unavailable_login_shell() {
        let selected = first_available(["/missing/login-shell", "bash", "/bin/sh"], |candidate| {
            candidate == "bash"
        });
        assert_eq!(selected.as_deref(), Some("bash"));
    }

    #[test]
    fn default_candidates_preserve_probe_order() {
        let selected = first_available(["pwsh", "powershell", "cmd.exe"], |candidate| {
            candidate == "powershell"
        });
        assert_eq!(selected.as_deref(), Some("powershell"));
    }

    #[test]
    fn login_shell_filter_accepts_supported_dialects_only() {
        for shell in [
            "/bin/sh",
            "/bin/bash",
            "/bin/zsh",
            "/usr/bin/ksh",
            "/usr/bin/dash",
            "/usr/bin/pwsh",
            "/usr/bin/powershell",
        ] {
            assert!(
                is_supported_login_shell(shell),
                "expected supported login shell: {shell}"
            );
        }
        for shell in [
            "/usr/bin/fish",
            "/bin/csh",
            "/bin/nu",
            "/sbin/nologin",
            "/bin/false",
        ] {
            assert!(
                !is_supported_login_shell(shell),
                "unsupported/service shell must use fallback: {shell}"
            );
        }
    }

    #[cfg(target_os = "windows")]
    #[test]
    fn explicit_cmd_shell_dialect_is_windows_cmd_on_windows() {
        assert_eq!(
            NativeRuntime::with_shell("cmd.exe".into()).shell_dialect(),
            ShellDialect::WindowsCmd
        );
    }

    #[cfg(target_os = "windows")]
    #[test]
    fn native_shell_dialect_matches_resolved_windows_default() {
        assert_eq!(
            NativeRuntime::new().shell_dialect(),
            NativeRuntime::with_shell(default_shell()).shell_dialect()
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
    fn posix_shell_profile_reports_the_configured_variant() {
        // The point of naming the POSIX variant: `[runtime] shell = "bash"`
        // must be visible to the model, not flattened to a generic `sh`.
        for (configured, expected) in [
            ("sh", "sh"),
            ("bash", "bash"),
            ("/usr/bin/zsh", "zsh"),
            ("/usr/bin/fish", "fish"),
        ] {
            let profile = NativeRuntime::with_shell(configured.into())
                .shell_profile()
                .expect("native runtime has a shell");
            assert_eq!(profile.name, expected, "configured {configured}");
            assert_eq!(profile.dialect, ShellDialect::Posix);
        }
    }

    #[test]
    fn powershell_shell_profile_distinguishes_pwsh_from_windows_powershell() {
        // pwsh (7+) and powershell (5.1) share a dialect but not a syntax
        // surface, so the prompt must be able to tell them apart.
        for (configured, expected) in [
            ("pwsh", "pwsh"),
            ("powershell", "powershell"),
            ("PowerShell.exe", "powershell"),
            ("C:\\Program Files\\PowerShell\\7\\pwsh.exe", "pwsh"),
        ] {
            let profile = NativeRuntime::with_shell(configured.into())
                .shell_profile()
                .expect("native runtime has a shell");
            assert_eq!(profile.name, expected, "configured {configured}");
            assert_eq!(profile.dialect, ShellDialect::PowerShell);
        }
    }

    #[test]
    #[cfg(target_os = "windows")]
    fn windows_cmd_shell_profile_reports_cmd_whatever_was_configured() {
        // Native Windows execution routes through `cmd.exe /C` regardless of
        // the configured value, so the cross-platform default `sh` must not
        // be reported as if a POSIX shell were going to run.
        for configured in ["sh", "cmd", "cmd.exe", "bash"] {
            let profile = NativeRuntime::with_shell(configured.into())
                .shell_profile()
                .expect("native runtime has a shell");
            assert_eq!(profile.name, "cmd", "configured {configured}");
            assert_eq!(profile.dialect, ShellDialect::WindowsCmd);
        }
    }

    #[test]
    fn shell_profile_matches_the_dialect_that_validates_commands() {
        // The reported profile and the policy dialect come from one adapter;
        // if they could disagree, the model would be told one language while
        // policy validated another.
        for configured in ["sh", "bash", "pwsh", "powershell", "cmd"] {
            let runtime = NativeRuntime::with_shell(configured.into());
            let profile = runtime.shell_profile().expect("native runtime has a shell");
            assert_eq!(
                profile.dialect,
                runtime.shell_dialect(),
                "configured {configured}"
            );
        }
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
        ];
        assert_eq!(&args[..3], expected.as_slice());
        let script_arg = args[3].to_string_lossy();
        assert!(script_arg.starts_with("try {"));
        assert!(script_arg.contains("[Console]::OutputEncoding = $utf8"));
        assert!(script_arg.contains("$OutputEncoding = $utf8"));
        assert!(script_arg.contains("} catch {\n}"));
        assert!(script_arg.ends_with(script));
    }

    #[test]
    fn powershell_full_scripts_are_passed_through_unchanged() {
        let command = "# comment\n#requires -Version 5.1 # keep; this comment\nusing namespace System.Text # keep; this comment too\nparam(\n    [string]$Value = @\"\nThis \" and ) # remain literal\n\"@\n)\nWrite-Output $Value";
        let cwd = std::env::temp_dir();
        let process = NativeRuntime::with_shell("pwsh".into())
            .build_shell_command(command, &cwd)
            .unwrap();
        let process = process.as_std();
        let script = process.get_args().nth(3).unwrap().to_string_lossy();

        assert_eq!(script, command);

        let named_blocks = "begin { Write-Output 'begin' }\nprocess { Write-Output 'process' }\nend { Write-Output 'end' }";
        assert_eq!(
            powershell_script_with_utf8_setup(named_blocks),
            named_blocks
        );
    }

    #[tokio::test]
    async fn powershell_full_scripts_execute_with_native_semantics() {
        let Some(interpreter) = ["pwsh", "powershell"].into_iter().find(|candidate| {
            std::process::Command::new(candidate)
                .arg("-NoProfile")
                .arg("-Command")
                .arg("exit 0")
                .output()
                .is_ok()
        }) else {
            return;
        };

        for (command, expected) in [
            (
                "#requires -Version 5.1 # keep; this comment\nusing namespace System.Text # keep; this comment too\n[Console]::Write('声明-ok')",
                "声明-ok",
            ),
            (
                "param(\n    [string]$Name = '参数-ok' # the comment may contain )\n)\n[Console]::Write($Name)",
                "参数-ok",
            ),
            (
                "param(\n    [string]$Message = @\"\nHere \" and ) # stay literal\n\"@\n)\n[Console]::Write($Message.Trim())",
                "Here \" and ) # stay literal",
            ),
            (
                "[CmdletBinding()]\nparam()\n[Console]::Write('attributed-ok')",
                "attributed-ok",
            ),
            (
                "begin { [Console]::Write('begin-') }\nprocess { [Console]::Write('process-') }\nend { [Console]::Write('end') }",
                "begin-process-end",
            ),
        ] {
            let output = tokio_powershell_command(interpreter, command)
                .output()
                .await
                .unwrap();

            assert!(
                output.status.success(),
                "stderr: {}",
                String::from_utf8_lossy(&output.stderr)
            );
            assert_eq!(String::from_utf8(output.stdout).unwrap(), expected);
        }

        let output = tokio_powershell_command(interpreter, "exit 23")
            .output()
            .await
            .unwrap();
        assert_eq!(output.status.code(), Some(23));
    }

    #[tokio::test]
    async fn powershell_utf8_setup_failure_does_not_block_a_simple_command() {
        let Some(interpreter) = ["pwsh", "powershell"].into_iter().find(|candidate| {
            std::process::Command::new(candidate)
                .arg("-NoProfile")
                .arg("-Command")
                .arg("exit 0")
                .output()
                .is_ok()
        }) else {
            return;
        };

        let script = powershell_script_with_utf8_setup("Write-Output constrained-ok");
        let constrained = format!(
            "$ExecutionContext.SessionState.LanguageMode = 'ConstrainedLanguage'\n{script}"
        );
        let output = tokio::process::Command::new(interpreter)
            .arg("-NoProfile")
            .arg("-NonInteractive")
            .arg("-Command")
            .arg(constrained)
            .output()
            .await
            .unwrap();

        assert!(
            output.status.success(),
            "stderr: {}",
            String::from_utf8_lossy(&output.stderr)
        );
        assert_eq!(
            String::from_utf8(output.stdout).unwrap().trim(),
            "constrained-ok"
        );
    }

    #[tokio::test]
    async fn powershell_command_adaptation_preserves_native_status() {
        let Some(interpreter) = ["pwsh", "powershell"].into_iter().find(|candidate| {
            std::process::Command::new(candidate)
                .arg("-NoProfile")
                .arg("-Command")
                .arg("exit 0")
                .output()
                .is_ok()
        }) else {
            return;
        };

        #[cfg(target_os = "windows")]
        let native_failure = "cmd /c exit 7";
        #[cfg(not(target_os = "windows"))]
        let native_failure = "sh -c 'exit 7'";

        for (command, expected_success) in [
            (native_failure.to_owned(), false),
            ("[Console]::Write('status-ok')".to_owned(), true),
            (
                format!("{native_failure}; [Console]::Write('recovered')"),
                true,
            ),
        ] {
            let raw_status = tokio::process::Command::new(interpreter)
                .arg("-NoProfile")
                .arg("-NonInteractive")
                .arg("-Command")
                .arg(&command)
                .output()
                .await
                .unwrap()
                .status;
            let adapted_status = tokio_powershell_command(interpreter, &command)
                .output()
                .await
                .unwrap()
                .status;

            assert_eq!(raw_status.success(), expected_success, "raw: {command}");
            assert_eq!(adapted_status.success(), expected_success, "{command}");
            assert_eq!(adapted_status.code(), raw_status.code(), "{command}");
        }
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
        #[cfg(target_os = "windows")]
        let runtime = NativeRuntime::with_shell("cmd.exe".into());
        #[cfg(not(target_os = "windows"))]
        let runtime = NativeRuntime::new();
        let command = runtime
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
        #[cfg(target_os = "windows")]
        let runtime = NativeRuntime::with_shell("cmd.exe".into());
        #[cfg(not(target_os = "windows"))]
        let runtime = NativeRuntime::new();
        let command = runtime
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
        let output = NativeRuntime::with_shell("cmd.exe".into())
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
        let output = NativeRuntime::with_shell("cmd.exe".into())
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
        let output = NativeRuntime::with_shell("cmd.exe".into())
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
    fn native_with_shell_uses_platform_default() {
        let runtime = NativeRuntime::new();
        let cwd = std::env::temp_dir();
        let cmd = runtime.build_shell_command("echo hi", &cwd).unwrap();
        let expected = default_shell();
        assert!(
            format!("{cmd:?}").contains(&format!("\"{expected}\"")),
            "default shell should be {expected:?}, got: {cmd:?}"
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
        ];
        assert_eq!(&args[..3], expected.as_slice());
        assert!(args[3].to_string_lossy().ends_with(script));
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
