//! Shared baseline environment for shell child processes.

use std::ffi::OsString;

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
    // Preserve the host-selected analysis cache to avoid costly module rediscovery.
    "PSModuleAnalysisCachePath",
    "TEMP",
    "TMP",
    "TERM",
    "LANG",
    "USERNAME",
];

/// Variables the runtime's command builder set on `process`; `None` marks one
/// the builder removed. They are part of the builder's spawn contract, which
/// `zeroclaw_config::platform::native` owns, so a spawner that must
/// `env_clear` takes this snapshot first and restores it with
/// [`reapply_command_env`].
pub(crate) fn command_env_snapshot(
    process: &tokio::process::Command,
) -> Vec<(OsString, Option<OsString>)> {
    process
        .as_std()
        .get_envs()
        .map(|(key, value)| (key.to_os_string(), value.map(|value| value.to_os_string())))
        .collect()
}

/// Restore a [`command_env_snapshot`] after `env_clear`.
pub(crate) fn reapply_command_env(
    process: &mut tokio::process::Command,
    snapshot: Vec<(OsString, Option<OsString>)>,
) {
    for (key, value) in snapshot {
        match value {
            Some(value) => {
                process.env(key, value);
            }
            None => {
                process.env_remove(key);
            }
        }
    }
}

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
}
