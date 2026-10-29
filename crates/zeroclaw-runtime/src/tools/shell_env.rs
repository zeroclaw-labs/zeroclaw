//! Shared baseline environment for shell child processes.

use crate::security::SecurityPolicy;
use std::collections::HashSet;
use zeroclaw_api::runtime_traits::RuntimeAdapter;

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

fn is_valid_env_var_name(name: &str) -> bool {
    let mut chars = name.chars();
    match chars.next() {
        Some(first) if first.is_ascii_alphabetic() || first == '_' => {}
        _ => return false,
    }
    chars.all(|ch| ch.is_ascii_alphanumeric() || ch == '_')
}

pub(crate) fn collect_allowed_shell_env_vars(security: &SecurityPolicy) -> Vec<String> {
    let mut out = Vec::new();
    let mut seen = HashSet::new();
    for key in SAFE_SHELL_ENV_VARS
        .iter()
        .copied()
        .chain(security.shell_env_passthrough.iter().map(|s| s.as_str()))
    {
        let candidate = key.trim();
        if candidate.is_empty() || !is_valid_env_var_name(candidate) {
            continue;
        }
        if seen.insert(candidate.to_string()) {
            out.push(candidate.to_string());
        }
    }
    out
}

/// Resolve allowed host values when executing, after any sandbox wrapping.
/// Guest environment forwarding remains the command builder's explicit contract.
pub(crate) fn apply_shell_environment(
    command: &mut tokio::process::Command,
    security: &SecurityPolicy,
    runtime: &dyn RuntimeAdapter,
) {
    command.env_clear();
    for key in collect_allowed_shell_env_vars(security) {
        if let Ok(value) = std::env::var(&key) {
            command.env(key, value);
        }
    }
    for key in runtime.shell_launcher_env_vars() {
        if let Ok(value) = std::env::var(key) {
            command.env(key, value);
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
