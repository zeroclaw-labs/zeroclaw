use std::path::{Path, PathBuf};
use zeroclaw_api::runtime_traits::RuntimeAdapter;

/// Shell choice on Windows. Detected once at runtime construction and cached;
/// prefers PowerShell 7 (`pwsh.exe`), then Windows PowerShell 5.1
/// (`powershell.exe`), falling back to `cmd.exe` only if neither is on PATH.
#[cfg(target_os = "windows")]
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
enum WindowsShell {
    Pwsh,
    PowerShell,
    Cmd,
}

#[cfg(target_os = "windows")]
impl WindowsShell {
    fn detect() -> Self {
        if find_in_path("pwsh.exe") {
            Self::Pwsh
        } else if find_in_path("powershell.exe") {
            Self::PowerShell
        } else {
            Self::Cmd
        }
    }

    fn exe(self) -> &'static str {
        match self {
            Self::Pwsh => "pwsh.exe",
            Self::PowerShell => "powershell.exe",
            Self::Cmd => "cmd.exe",
        }
    }
}

#[cfg(target_os = "windows")]
fn find_in_path(name: &str) -> bool {
    let Some(path) = std::env::var_os("PATH") else {
        return false;
    };
    std::env::split_paths(&path).any(|dir| dir.join(name).is_file())
}

/// Native runtime — full access, runs on Mac/Linux/Windows/Docker/Raspberry Pi
pub struct NativeRuntime {
    #[cfg(target_os = "windows")]
    shell: WindowsShell,
}

impl Default for NativeRuntime {
    fn default() -> Self {
        Self::new()
    }
}

impl NativeRuntime {
    pub fn new() -> Self {
        Self {
            #[cfg(target_os = "windows")]
            shell: WindowsShell::detect(),
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
            let mut process = tokio::process::Command::new("sh");
            process.arg("-c").arg(command).current_dir(workspace_dir);
            Ok(process)
        }

        #[cfg(target_os = "windows")]
        {
            const CREATE_NO_WINDOW: u32 = 0x08000000;

            // PowerShell is preferred on Windows so that cmdlets in the
            // operator's `allowed_commands` (Get-ChildItem, Format-Table, …)
            // can actually execute — cmd.exe cannot run cmdlets, which would
            // make any PowerShell-style allowlist useless. The shell is
            // detected once at construction (see WindowsShell::detect) and
            // only falls back to cmd.exe if neither pwsh.exe nor
            // powershell.exe is on PATH.
            let mut process = tokio::process::Command::new(self.shell.exe());
            match self.shell {
                WindowsShell::Pwsh | WindowsShell::PowerShell => {
                    process
                        .arg("-NoProfile")
                        .arg("-NonInteractive")
                        .arg("-Command")
                        .arg(command);
                }
                WindowsShell::Cmd => {
                    process.arg("/C").arg(command);
                }
            }
            process
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

    #[cfg(target_os = "windows")]
    #[test]
    fn windows_shell_detection_prefers_pwsh_over_powershell_over_cmd() {
        // On any normal Windows install, at least powershell.exe is on PATH,
        // so detect() must never silently fall back to Cmd unless PATH is
        // genuinely empty of both PowerShell editions.
        let shell = WindowsShell::detect();
        let pwsh_present = find_in_path("pwsh.exe");
        let posh_present = find_in_path("powershell.exe");

        let expected = if pwsh_present {
            WindowsShell::Pwsh
        } else if posh_present {
            WindowsShell::PowerShell
        } else {
            WindowsShell::Cmd
        };
        assert_eq!(shell, expected);
    }

    #[cfg(target_os = "windows")]
    #[test]
    fn windows_shell_command_uses_command_flag_for_powershell() {
        // Whichever PowerShell edition is detected, the command must be
        // passed via -Command (not /C). For cmd fallback, /C must be used.
        let cwd = std::env::temp_dir();
        let rt = NativeRuntime::new();
        let cmd = rt.build_shell_command("Get-ChildItem", &cwd).unwrap();
        let debug = format!("{cmd:?}");
        match rt.shell {
            WindowsShell::Pwsh | WindowsShell::PowerShell => {
                assert!(debug.contains("-Command"), "PowerShell must use -Command, got: {debug}");
                assert!(debug.contains("-NoProfile"), "must isolate from user profile, got: {debug}");
            }
            WindowsShell::Cmd => {
                assert!(debug.contains("/C"), "cmd fallback must use /C, got: {debug}");
            }
        }
    }
}
