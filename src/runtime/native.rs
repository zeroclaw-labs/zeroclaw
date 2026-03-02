use super::traits::RuntimeAdapter;
use std::path::{Path, PathBuf};

/// Native runtime — full access, runs on Mac/Linux/Docker/Raspberry Pi
pub struct NativeRuntime {
    shell: Option<ShellProgram>,
}

#[derive(Debug, Clone, PartialEq, Eq)]
struct ShellProgram {
    kind: ShellKind,
    program: PathBuf,
}

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
enum ShellKind {
    Sh,
    Bash,
    Pwsh,
    PowerShell,
    Cmd,
}

impl ShellKind {
    fn as_str(self) -> &'static str {
        match self {
            ShellKind::Sh => "sh",
            ShellKind::Bash => "bash",
            ShellKind::Pwsh => "pwsh",
            ShellKind::PowerShell => "powershell",
            ShellKind::Cmd => "cmd",
        }
    }
}

impl ShellProgram {
    fn add_shell_args(&self, process: &mut tokio::process::Command, command: &str) {
        match self.kind {
            ShellKind::Sh | ShellKind::Bash => {
                process.arg("-c").arg(command);
            }
            ShellKind::Pwsh | ShellKind::PowerShell => {
                process
                    .arg("-NoLogo")
                    .arg("-NoProfile")
                    .arg("-NonInteractive")
                    .arg("-Command")
                    .arg(command);
            }
            ShellKind::Cmd => {
                process.arg("/C").arg(command);
            }
        }
    }
}

fn detect_native_shell() -> Option<ShellProgram> {
    #[cfg(target_os = "windows")]
    {
        let comspec = std::env::var_os("COMSPEC").map(PathBuf::from);
        detect_native_shell_with(true, |name| which::which(name).ok(), comspec)
    }
    #[cfg(not(target_os = "windows"))]
    {
        detect_native_shell_with(false, |name| which::which(name).ok(), None)
    }
}

fn detect_native_shell_with<F>(
    is_windows: bool,
    mut resolve: F,
    comspec: Option<PathBuf>,
) -> Option<ShellProgram>
where
    F: FnMut(&str) -> Option<PathBuf>,
{
    if is_windows {
        for (name, kind) in [
            ("bash", ShellKind::Bash),
            ("sh", ShellKind::Sh),
            ("pwsh", ShellKind::Pwsh),
            ("powershell", ShellKind::PowerShell),
            ("cmd", ShellKind::Cmd),
            ("cmd.exe", ShellKind::Cmd),
        ] {
            if let Some(program) = resolve(name) {
                return Some(ShellProgram { kind, program });
            }
        }
        if let Some(program) = comspec {
            return Some(ShellProgram {
                kind: ShellKind::Cmd,
                program,
            });
        }
        return None;
    }

    for (name, kind) in [("sh", ShellKind::Sh), ("bash", ShellKind::Bash)] {
        if let Some(program) = resolve(name) {
            return Some(ShellProgram { kind, program });
        }
    }
    None
}

fn missing_shell_error() -> &'static str {
    #[cfg(target_os = "windows")]
    {
        "Native runtime could not find a usable shell (tried: bash, sh, pwsh, powershell, cmd). \
         Install Git Bash or PowerShell and ensure it is available on PATH."
    }
    #[cfg(not(target_os = "windows"))]
    {
        "Native runtime could not find a usable shell (tried: sh, bash). \
         Install a POSIX shell and ensure it is available on PATH."
    }
}

impl NativeRuntime {
    pub fn new() -> Self {
        Self {
            shell: detect_native_shell(),
        }
    }

    pub(crate) fn selected_shell_kind(&self) -> Option<&'static str> {
        self.shell.as_ref().map(|shell| shell.kind.as_str())
    }

    pub(crate) fn selected_shell_program(&self) -> Option<&Path> {
        self.shell.as_ref().map(|shell| shell.program.as_path())
    }

    #[cfg(test)]
    fn new_for_test(shell: Option<ShellProgram>) -> Self {
        Self { shell }
    }
}

impl RuntimeAdapter for NativeRuntime {
    fn as_any(&self) -> &dyn std::any::Any {
        self
    }

    fn name(&self) -> &str {
        "native"
    }

    fn has_shell_access(&self) -> bool {
        self.shell.is_some()
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
        let shell = self
            .shell
            .as_ref()
            .ok_or_else(|| anyhow::anyhow!(missing_shell_error()))?;

        let mut process = tokio::process::Command::new(&shell.program);
        shell.add_shell_args(&mut process, command);
        process.current_dir(workspace_dir);
        Ok(process)
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use std::collections::HashMap;

    #[test]
    fn native_name() {
        assert_eq!(NativeRuntime::new().name(), "native");
    }

    #[test]
    fn native_has_shell_access() {
        assert_eq!(
            NativeRuntime::new().has_shell_access(),
            detect_native_shell().is_some()
        );
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
    fn detect_shell_windows_prefers_git_bash() {
        let mut map = HashMap::new();
        map.insert("bash", r"C:\Program Files\Git\bin\bash.exe");
        map.insert(
            "powershell",
            r"C:\Windows\System32\WindowsPowerShell\v1.0\powershell.exe",
        );
        map.insert("cmd", r"C:\Windows\System32\cmd.exe");

        let shell = detect_native_shell_with(
            true,
            |name| map.get(name).map(PathBuf::from),
            Some(PathBuf::from(r"C:\Windows\System32\cmd.exe")),
        )
        .expect("windows shell should be detected");

        assert_eq!(shell.kind, ShellKind::Bash);
    }

    #[test]
    fn detect_shell_windows_falls_back_to_powershell_then_cmd() {
        let mut map = HashMap::new();
        map.insert(
            "powershell",
            r"C:\Windows\System32\WindowsPowerShell\v1.0\powershell.exe",
        );

        let shell = detect_native_shell_with(
            true,
            |name| map.get(name).map(PathBuf::from),
            Some(PathBuf::from(r"C:\Windows\System32\cmd.exe")),
        )
        .expect("windows shell should be detected");

        assert_eq!(shell.kind, ShellKind::PowerShell);

        let cmd_shell = detect_native_shell_with(
            true,
            |_name| None,
            Some(PathBuf::from(r"C:\Windows\System32\cmd.exe")),
        )
        .expect("cmd fallback should be detected");
        assert_eq!(cmd_shell.kind, ShellKind::Cmd);
    }

    #[test]
    fn detect_shell_unix_prefers_sh() {
        let mut map = HashMap::new();
        map.insert("sh", "/bin/sh");
        map.insert("bash", "/usr/bin/bash");

        let shell = detect_native_shell_with(false, |name| map.get(name).map(PathBuf::from), None)
            .expect("unix shell should be detected");

        assert_eq!(shell.kind, ShellKind::Sh);
    }

    #[test]
    fn native_without_shell_disables_shell_access() {
        let runtime = NativeRuntime::new_for_test(None);
        assert!(!runtime.has_shell_access());

        let err = runtime
            .build_shell_command("echo hello", Path::new("."))
            .expect_err("build should fail without available shell")
            .to_string();
        assert!(err.contains("could not find a usable shell"));
    }

    #[test]
    fn native_builds_powershell_command() {
        let runtime = NativeRuntime::new_for_test(Some(ShellProgram {
            kind: ShellKind::PowerShell,
            program: PathBuf::from("powershell"),
        }));

        let command = runtime
            .build_shell_command("Get-Location", Path::new("."))
            .expect("powershell command should build");
        let debug = format!("{command:?}");

        assert!(debug.contains("powershell"));
        assert!(debug.contains("-NoProfile"));
        assert!(debug.contains("-Command"));
        assert!(debug.contains("Get-Location"));
    }

    #[test]
    fn native_builds_cmd_command() {
        let runtime = NativeRuntime::new_for_test(Some(ShellProgram {
            kind: ShellKind::Cmd,
            program: PathBuf::from("cmd"),
        }));

        let command = runtime
            .build_shell_command("echo hello", Path::new("."))
            .expect("cmd command should build");
        let debug = format!("{command:?}");

        assert!(debug.contains("cmd"));
        assert!(debug.contains("/C"));
        assert!(debug.contains("echo hello"));
    }

    #[test]
    fn native_builds_shell_command() {
        let runtime = NativeRuntime::new();
        if !runtime.has_shell_access() {
            return;
        }

        let cwd = std::env::temp_dir();
        let command = runtime.build_shell_command("echo hello", &cwd).unwrap();
        let debug = format!("{command:?}");
        assert!(debug.contains("echo hello"));
    }
}
