//! Shared baseline environment for shell child processes.

/// Environment variables safe to copy into shell child processes after `env_clear`.
/// Only functional variables are included — never API keys or secrets.
#[cfg(not(target_os = "windows"))]
pub(crate) const SAFE_SHELL_ENV_VARS: &[&str] = &[
    "PATH", "HOME", "TERM", "LANG", "LC_ALL", "LC_CTYPE", "USER", "SHELL", "TMPDIR",
];

/// Windows variables needed for cmd.exe, PowerShell module discovery, and program resolution.
#[cfg(target_os = "windows")]
pub(crate) const SAFE_SHELL_ENV_VARS: &[&str] = &[
    "PATH",
    "PATHEXT",
    "HOME",
    "USERPROFILE",
    "HOMEDRIVE",
    "HOMEPATH",
    "SYSTEMROOT",
    "SYSTEMDRIVE",
    "WINDIR",
    "COMSPEC",
    "PSModulePath",
    "TEMP",
    "TMP",
    "TERM",
    "LANG",
    "USERNAME",
];

#[cfg(test)]
mod tests {
    use super::SAFE_SHELL_ENV_VARS;

    #[test]
    fn safe_shell_env_vars_exclude_secrets() {
        for var in SAFE_SHELL_ENV_VARS {
            let lower = var.to_lowercase();
            assert!(
                !lower.contains("key") && !lower.contains("secret") && !lower.contains("token"),
                "SAFE_SHELL_ENV_VARS must not include sensitive variable: {var}"
            );
        }
    }

    #[test]
    fn safe_shell_env_vars_include_essentials() {
        assert!(SAFE_SHELL_ENV_VARS.contains(&"PATH"));
        assert!(
            SAFE_SHELL_ENV_VARS.contains(&"HOME") || SAFE_SHELL_ENV_VARS.contains(&"USERPROFILE")
        );
        assert!(SAFE_SHELL_ENV_VARS.contains(&"TERM"));
    }

    #[cfg(windows)]
    #[test]
    fn safe_shell_env_vars_preserve_powershell_module_discovery() {
        assert!(SAFE_SHELL_ENV_VARS.contains(&"PSModulePath"));
    }
}
