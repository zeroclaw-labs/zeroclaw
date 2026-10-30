//! Shared baseline environment for shell child processes.

/// One immutable, session-scoped client environment shared by shell execution
/// and RPC admission. Only the handles are cloned; values are neither copied
/// into admission state nor persisted. Eligibility is resolved from live grants.
pub type ForwardedEnvironment = std::sync::Arc<std::collections::HashMap<String, String>>;

/// Environment variables safe to copy into shell child processes after `env_clear`.
/// Only functional variables are included — never API keys or secrets.
#[cfg(not(target_os = "windows"))]
pub(crate) const SAFE_SHELL_ENV_VARS: &[&str] = &[
    "PATH", "HOME", "TERM", "LANG", "LC_ALL", "LC_CTYPE", "USER", "SHELL", "TMPDIR",
];

/// Windows allowlist kept as a named slice so every platform's tests can assert
/// APPDATA / LOCALAPPDATA without scanning this file's source (tests also quote
/// those names).
#[cfg(any(test, target_os = "windows"))]
pub(crate) const WINDOWS_SAFE_SHELL_ENV_VARS: &[&str] = &[
    "PATH",
    "PATHEXT",
    "HOME",
    "USERPROFILE",
    "HOMEDRIVE",
    "HOMEPATH",
    // Per-user config/cred/cache roots used by gh, npm, and most Win32 tools.
    "APPDATA",
    "LOCALAPPDATA",
    "SYSTEMROOT",
    "SYSTEMDRIVE",
    "WINDIR",
    "COMSPEC",
    "PSModulePath",
    // Preserve the host-selected analysis cache to avoid costly module rediscovery.
    "PSModuleAnalysisCachePath",
    "TEMP",
    "TMP",
    "TERM",
    "LANG",
    "USERNAME",
];

/// Windows variables needed for cmd.exe, PowerShell module discovery, and program resolution.
#[cfg(target_os = "windows")]
pub(crate) const SAFE_SHELL_ENV_VARS: &[&str] = WINDOWS_SAFE_SHELL_ENV_VARS;

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
        assert!(SAFE_SHELL_ENV_VARS.contains(&"PSModuleAnalysisCachePath"));
    }

    #[cfg(windows)]
    #[test]
    fn safe_shell_env_vars_forward_windows_profile_roots() {
        assert!(SAFE_SHELL_ENV_VARS.contains(&"APPDATA"));
        assert!(SAFE_SHELL_ENV_VARS.contains(&"LOCALAPPDATA"));
    }

    #[test]
    fn windows_profile_roots_stay_in_the_windows_allowlist() {
        // Cross-platform: assert the Windows allowlist declaration directly so
        // Linux CI still catches a regression that drops APPDATA / LOCALAPPDATA
        // without matching the same strings inside this test module.
        assert!(
            super::WINDOWS_SAFE_SHELL_ENV_VARS.contains(&"APPDATA"),
            "Windows SAFE_SHELL_ENV_VARS must forward APPDATA"
        );
        assert!(
            super::WINDOWS_SAFE_SHELL_ENV_VARS.contains(&"LOCALAPPDATA"),
            "Windows SAFE_SHELL_ENV_VARS must forward LOCALAPPDATA"
        );
    }
}
