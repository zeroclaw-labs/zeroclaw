use std::ffi::OsStr;
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
/// match. Every other value, including an explicit `sh` or `cmd`, does not.
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
/// `interpreter` is the selected executable. Native Unix command preparation
/// supplies its canonical path; the Windows helper receives the configured
/// interpreter string for normal process-spawn resolution.
///
/// `-NoProfile` skips user/host profile scripts for a predictable, faster
/// startup; `-NonInteractive` prevents the shell from blocking on prompts; and
/// `-Command` consumes the final argument as script text. Commands in the
/// policy's bounded PowerShell grammar receive a UTF-8 setup statement inside
/// an empty `try`/`catch`, so unsupported settings never prevent the requested
/// command from running. Full scripts are passed through unchanged: prepending
/// a statement would invalidate leading declarations or named blocks, while a
/// nested script-block wrapper would change scope and process-exit semantics.
fn tokio_powershell_command(
    interpreter: impl AsRef<OsStr>,
    command: &str,
) -> tokio::process::Command {
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

/// Whether probing `candidate` proves that nothing is there: the probe failed
/// with `NotFound`, or with `NotADirectory` (a path component is a regular
/// file, so nothing can live below it).
///
/// Any other failure (an unsearchable parent directory, a symlink loop in a
/// parent component, ...) proves nothing: the entry may well exist.
#[cfg(unix)]
fn probe_proves_absent(candidate: &Path) -> bool {
    match std::fs::symlink_metadata(candidate) {
        Ok(_) => false,
        Err(error) => matches!(
            error.kind(),
            std::io::ErrorKind::NotFound | std::io::ErrorKind::NotADirectory
        ),
    }
}

/// Whether `shell` is absent on this host: every place it could be was probed
/// and none holds an entry of that name. Present but unusable (not executable,
/// a directory) is not absent, it stays a misconfiguration. Android pins its
/// shell to `/system/bin/sh`, so it is never absent here.
///
/// A place that cannot be probed (an unsearchable directory, a symlink loop
/// in a parent component) is not evidence of absence either: the shell may be
/// there. The result is then "not absent" and the resolver reports the
/// underlying error, so an unexpectedly missing shell stays diagnosable.
///
/// This is decided by probing existence, not by the resolver's error kind:
/// the resolver reports the error of *some other* PATH entry (for example a
/// regular file used as a directory) when the shell is nowhere to be found.
#[cfg(unix)]
pub(crate) fn shell_is_absent<I, P>(shell: &OsStr, path_entries: I) -> bool
where
    I: IntoIterator<Item = P>,
    P: AsRef<Path>,
{
    if is_android() {
        return false;
    }
    let requested = Path::new(shell);
    if requested.is_absolute() {
        return probe_proves_absent(requested);
    }
    // Same entries the resolver considers: absolute ones only.
    path_entries.into_iter().all(|entry| {
        let entry = entry.as_ref();
        !entry.is_absolute() || probe_proves_absent(&entry.join(requested))
    })
}

/// [`shell_is_absent`] against the process `PATH`.
#[cfg(unix)]
pub(crate) fn shell_is_absent_on_host(shell: &str) -> bool {
    let path = std::env::var_os("PATH").unwrap_or_default();
    shell_is_absent(OsStr::new(shell), std::env::split_paths(&path))
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
    /// Absolute launcher resolved by the TUI-aware runtime factory, when one
    /// was supplied. Ambient callers continue resolving at command build time.
    resolved_shell: Option<PathBuf>,
    /// Set when the shell is absent from the host (see [`shell_is_absent`]).
    /// The runtime then reports no shell (`ShellDialect::None`)
    /// instead of advertising one that every command would fail to launch.
    shell_unavailable: bool,
}

impl Default for NativeRuntime {
    fn default() -> Self {
        Self::new()
    }
}

impl NativeRuntime {
    /// Create a native runtime using the platform's resolved default shell.
    ///
    /// Like the runtime factory, a default shell that is not on this host
    /// yields a runtime that reports no shell rather than one that advertises
    /// a shell every command would fail to launch.
    pub fn new() -> Self {
        let shell = default_shell();
        #[cfg(unix)]
        if shell_is_absent_on_host(&shell) {
            return Self::without_shell(shell);
        }
        Self::with_shell(shell)
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
        Self {
            shell,
            resolved_shell: None,
            shell_unavailable: false,
        }
    }

    pub(crate) fn with_shell_and_resolved_path(
        shell: String,
        resolved_shell: Option<PathBuf>,
    ) -> Self {
        Self {
            shell,
            resolved_shell,
            shell_unavailable: false,
        }
    }

    /// Create a native runtime whose configured shell is absent from the host.
    /// It reports no shell access and refuses to build shell commands.
    #[cfg(unix)]
    pub(crate) fn without_shell(shell: String) -> Self {
        Self {
            shell,
            resolved_shell: None,
            shell_unavailable: true,
        }
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
        if self.shell_unavailable {
            return ShellDialect::None;
        }

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
            // regardless of the configured value (including explicit `sh`),
            // so the configured name would misreport it.
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

    fn shell_program(&self) -> Option<&OsStr> {
        if self.shell_unavailable {
            return None;
        }

        #[cfg(not(target_os = "windows"))]
        if is_android() {
            return Some(OsStr::new("/system/bin/sh"));
        }

        #[cfg(target_os = "windows")]
        if self.shell_dialect() == ShellDialect::WindowsCmd {
            return Some(OsStr::new(WINDOWS_COMMAND_INTERPRETER));
        }

        Some(OsStr::new(&self.shell))
    }

    fn build_shell_command(
        &self,
        command: &str,
        workspace_dir: &Path,
    ) -> anyhow::Result<tokio::process::Command> {
        if self.shell_unavailable {
            anyhow::bail!(
                "native runtime has no usable shell: {:?} was not found. Set runtime.shell to an installed shell (absolute path or a name on PATH) to enable shell commands",
                self.shell
            );
        }

        #[cfg(not(target_os = "windows"))]
        {
            // Android keeps its shell at /system/bin/sh and it is not always
            // on PATH for spawned processes; use the absolute path when present
            // so the shell can launch (and reach platform tools).
            // User-configured shell is ignored on Android.
            let configured_shell = if is_android() {
                "/system/bin/sh"
            } else {
                &self.shell
            };
            let shell = if let Some(resolved_shell) = &self.resolved_shell {
                resolved_shell.clone()
            } else {
                crate::platform::resolve_executable(std::ffi::OsStr::new(configured_shell))
                    .map_err(|error| {
                        let detail = error.to_string();
                        anyhow::Error::new(error).context(format!(
                            "native runtime shell {configured_shell:?} could not be resolved before setting workspace directory: {detail}"
                        ))
                    })?
            };
            let mut process = if self.shell_dialect() == ShellDialect::PowerShell {
                tokio_powershell_command(shell.as_os_str(), command)
            } else {
                let mut process = tokio::process::Command::new(&shell);
                process.arg("-c").arg(command);
                process
            };
            #[cfg(unix)]
            {
                use std::os::unix::process::CommandExt;

                // Canonicalize the executable, not the identity that selects a
                // multicall applet or the shell's sh compatibility mode.
                process.as_std_mut().arg0(configured_shell);
            }
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

    fn build_shell_command_with_effective_path(
        &self,
        command: &str,
        workspace_dir: &Path,
        effective_path: Option<&std::ffi::OsStr>,
    ) -> anyhow::Result<tokio::process::Command> {
        if self.shell_unavailable {
            return self.build_shell_command(command, workspace_dir);
        }

        #[cfg(not(target_os = "windows"))]
        if let Some(path) = effective_path {
            let configured_shell = if is_android() {
                std::ffi::OsStr::new("/system/bin/sh")
            } else {
                std::ffi::OsStr::new(&self.shell)
            };
            let resolved_shell = crate::platform::resolve_executable_with_path(
                configured_shell,
                std::env::split_paths(path),
            )
            .map_err(|error| {
                let detail = error.to_string();
                anyhow::Error::new(error).context(format!(
                    "native runtime shell {configured_shell:?} could not be resolved in the effective child PATH: {detail}"
                ))
            })?;
            return Self::with_shell_and_resolved_path(self.shell.clone(), Some(resolved_shell))
                .build_shell_command(command, workspace_dir);
        }

        #[cfg(target_os = "windows")]
        let _ = effective_path;

        self.build_shell_command(command, workspace_dir)
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

    #[cfg(all(unix, not(target_os = "android")))]
    #[test]
    fn shell_absence_is_only_reported_for_a_missing_shell() {
        use std::os::unix::fs::PermissionsExt;

        // Absolute paths do not depend on the process PATH.
        assert!(shell_is_absent_on_host(
            "/nonexistent-zeroclaw-shell/bin/sh"
        ));
        // Bare names are probed against explicit entries, not the host PATH:
        // one unsearchable entry there would otherwise change the outcome.
        let empty = tempfile::tempdir().unwrap();
        assert!(shell_is_absent(
            OsStr::new("zc-no-such-shell-on-path"),
            [empty.path()]
        ));
        let with_sh = tempfile::tempdir().unwrap();
        std::fs::write(with_sh.path().join("sh"), "").unwrap();
        assert!(!shell_is_absent(OsStr::new("sh"), [with_sh.path()]));
        // The host PATH can only turn "present" into "not absent" (an entry
        // that cannot be probed counts as not absent), so this holds on any
        // PATH that has a shell.
        assert!(!shell_is_absent_on_host("sh"));

        // Present but unusable is a misconfiguration, not an absent shell.
        let dir = tempfile::tempdir().unwrap();
        let file = dir.path().join("not-executable");
        std::fs::write(&file, "#!/bin/sh\n").unwrap();
        std::fs::set_permissions(&file, std::fs::Permissions::from_mode(0o644)).unwrap();
        assert!(!shell_is_absent_on_host(&file.to_string_lossy()));
        assert!(!shell_is_absent_on_host(&dir.path().to_string_lossy()));
    }

    /// A directory holding an executable `sh` that the current user cannot
    /// search. Permissions are restored on drop so the tempdir can be removed.
    #[cfg(all(unix, not(target_os = "android")))]
    struct LockedDir {
        _outer: tempfile::TempDir,
        path: PathBuf,
    }

    #[cfg(all(unix, not(target_os = "android")))]
    impl Drop for LockedDir {
        fn drop(&mut self) {
            use std::os::unix::fs::PermissionsExt;

            let _ = std::fs::set_permissions(&self.path, std::fs::Permissions::from_mode(0o755));
        }
    }

    /// `None` when the process ignores directory permissions (root): the case
    /// cannot be built there.
    #[cfg(all(unix, not(target_os = "android")))]
    fn unsearchable_dir_with_sh() -> Option<LockedDir> {
        use std::os::unix::fs::PermissionsExt;

        let outer = tempfile::tempdir().unwrap();
        let path = outer.path().join("locked");
        std::fs::create_dir(&path).unwrap();
        let shell = path.join("sh");
        std::fs::write(&shell, "#!/bin/sh\n").unwrap();
        std::fs::set_permissions(&shell, std::fs::Permissions::from_mode(0o755)).unwrap();
        std::fs::set_permissions(&path, std::fs::Permissions::from_mode(0o000)).unwrap();
        let locked = LockedDir {
            _outer: outer,
            path,
        };
        // Directory permissions are not enforced for this process.
        std::fs::symlink_metadata(&shell).is_err().then_some(locked)
    }

    #[cfg(all(unix, not(target_os = "android")))]
    #[test]
    fn shell_absence_is_not_concluded_from_an_unprobeable_entry() {
        let Some(guard) = unsearchable_dir_with_sh() else {
            eprintln!(
                "skipped: directory permissions are not enforced for this process (running as root?)"
            );
            return;
        };
        let locked = guard.path.clone();
        let empty = tempfile::tempdir().unwrap();

        // The only place the shell could be cannot be probed: not absent.
        assert!(!shell_is_absent(OsStr::new("sh"), [&locked]));
        // Absolute path below the unsearchable directory: same.
        assert!(!shell_is_absent(
            OsStr::new(&locked.join("sh")),
            std::iter::empty::<&Path>()
        ));
        // One unprobeable entry among readable, empty ones is enough.
        assert!(!shell_is_absent(
            OsStr::new("sh"),
            [empty.path(), locked.as_path()]
        ));
        // Positive control: probing every entry and finding nothing is absent.
        assert!(shell_is_absent(OsStr::new("sh"), [empty.path()]));
        // A shell in a readable entry means "not absent" in either order, but
        // for different reasons: found when it comes first, and with the
        // unprobeable entry first the probe stops there (`all` short-circuits)
        // and never reaches the readable one.
        let bin = tempfile::tempdir().unwrap();
        std::fs::write(bin.path().join("sh"), "").unwrap();
        assert!(!shell_is_absent(
            OsStr::new("sh"),
            [bin.path(), locked.as_path()]
        ));
        assert!(!shell_is_absent(
            OsStr::new("sh"),
            [locked.as_path(), bin.path()]
        ));
    }

    #[cfg(all(unix, not(target_os = "android")))]
    #[test]
    fn shell_absence_is_not_concluded_from_a_symlink_loop() {
        // ELOOP (a loop in a parent component) proves nothing about the shell.
        // Whether the platform reports it for this layout is checked first.
        let dir = tempfile::tempdir().unwrap();
        let (a, b) = (dir.path().join("a"), dir.path().join("b"));
        std::os::unix::fs::symlink(&b, &a).unwrap();
        std::os::unix::fs::symlink(&a, &b).unwrap();
        if !matches!(
            std::fs::symlink_metadata(a.join("sh")),
            Err(ref error) if error.kind() != std::io::ErrorKind::NotFound
                && error.kind() != std::io::ErrorKind::NotADirectory
        ) {
            eprintln!(
                "skipped: no error other than NotFound or NotADirectory for the symlink loop"
            );
            return;
        }
        assert!(!shell_is_absent(OsStr::new("sh"), [&a]));
    }

    #[cfg(all(unix, not(target_os = "android")))]
    #[test]
    fn shell_absence_ignores_unusable_path_entries() {
        use std::os::unix::fs::PermissionsExt;

        let empty = tempfile::tempdir().unwrap();
        // An entry that is not a directory (the resolver would report
        // NotADirectory for it, masking the real NotFound) and a relative one.
        let not_a_dir = empty.path().join("file");
        std::fs::write(&not_a_dir, "").unwrap();
        let entries = [
            not_a_dir.clone(),
            PathBuf::from("relative-decoy"),
            PathBuf::new(),
            empty.path().to_path_buf(),
        ];
        assert!(shell_is_absent(OsStr::new("sh"), &entries));

        // A shell present in a later entry is found even if earlier entries are unusable.
        let bin = tempfile::tempdir().unwrap();
        let shell = bin.path().join("sh");
        std::fs::write(&shell, "#!/bin/sh\n").unwrap();
        std::fs::set_permissions(&shell, std::fs::Permissions::from_mode(0o755)).unwrap();
        let entries = [not_a_dir, bin.path().to_path_buf()];
        assert!(!shell_is_absent(OsStr::new("sh"), &entries));
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
        // the configured value, so an explicit `sh` must not be reported as
        // if a POSIX shell were going to run.
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
        use std::os::unix::fs::PermissionsExt;

        let script = r#"Write-Output "quoted safe value" | Select-Object -First 1"#;
        let cwd = std::env::temp_dir();
        let dir = tempfile::tempdir().unwrap();
        let launcher = dir.path().join("pwsh");
        std::fs::write(&launcher, "#!/bin/sh\n").unwrap();
        std::fs::set_permissions(&launcher, std::fs::Permissions::from_mode(0o755)).unwrap();
        let command = NativeRuntime::with_shell(launcher.to_string_lossy().into_owned())
            .build_shell_command(script, &cwd)
            .unwrap();
        let command = command.as_std();

        assert_eq!(
            command.get_program(),
            launcher.canonicalize().unwrap().as_os_str(),
            "Unix PowerShell launcher should use the canonical configured path"
        );
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
        let process = tokio_powershell_command("pwsh", command);
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

    #[cfg(all(unix, not(target_os = "android")))]
    #[tokio::test]
    async fn native_canonical_shell_preserves_requested_argv0() {
        let dir = tempfile::tempdir().unwrap();
        let launcher = dir.path().join("sh");
        let canonical_shell = std::fs::canonicalize("/bin/sh").unwrap();
        std::os::unix::fs::symlink(&canonical_shell, &launcher).unwrap();

        for requested in ["sh".to_owned(), launcher.to_str().unwrap().to_owned()] {
            let runtime = NativeRuntime::with_shell(requested.clone());
            let mut command = runtime
                .build_shell_command_with_effective_path(
                    "printf '%s' \"$0\"",
                    dir.path(),
                    Some(dir.path().as_os_str()),
                )
                .unwrap();
            assert_eq!(command.as_std().get_program(), canonical_shell.as_os_str());
            let output = command.output().await.unwrap();
            assert!(output.status.success(), "{output:?}");
            assert_eq!(output.stdout, requested.as_bytes());
        }
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
        let selected = default_shell();
        let expected =
            crate::platform::resolve_executable(std::ffi::OsStr::new(&selected)).unwrap();
        assert_eq!(
            cmd.as_std().get_program(),
            expected.as_os_str(),
            "platform default {selected:?} should use the resolved executable path"
        );
    }

    #[test]
    #[cfg(not(target_os = "windows"))]
    fn native_with_shell_bash() {
        let runtime = NativeRuntime::with_shell("bash".into());
        let cwd = std::env::temp_dir();
        let cmd = runtime.build_shell_command("echo hi", &cwd).unwrap();
        assert_eq!(
            std::path::Path::new(cmd.as_std().get_program()).file_name(),
            Some(std::ffi::OsStr::new("bash")),
            "configured shell should resolve to an executable named 'bash'"
        );
    }

    #[test]
    #[cfg(not(target_os = "windows"))]
    fn native_with_shell_absolute_path() {
        let configured = std::path::Path::new("/bin/sh");
        let runtime = NativeRuntime::with_shell(configured.to_string_lossy().into_owned());
        let cwd = std::env::temp_dir();
        let cmd = runtime.build_shell_command("echo hi", &cwd).unwrap();
        assert_eq!(
            cmd.as_std().get_program(),
            configured.canonicalize().unwrap().as_os_str(),
            "absolute path should be canonicalized before command construction"
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
