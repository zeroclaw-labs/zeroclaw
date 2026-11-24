//! Child-environment allowlist primitives shared by ACP CLI providers.
//!
//! Providers clear the inherited environment and forward only the
//! process-runtime entries below plus their own explicit per-alias names.

/// Environment variable names required for process startup, locale, and
/// explicitly configured network proxies.
pub(crate) const ENV_ALLOWLIST_EXACT: &[&str] = &[
    "PATH",
    "PATHEXT",
    "HOME",
    "USER",
    "USERNAME",
    "USERPROFILE",
    "HOMEDRIVE",
    "HOMEPATH",
    "LOGNAME",
    "SHELL",
    "SYSTEMROOT",
    "SYSTEMDRIVE",
    "WINDIR",
    "COMSPEC",
    "TMPDIR",
    "TMP",
    "TEMP",
    "LANG",
    "LANGUAGE",
    "TERM",
    "COLORTERM",
    "NO_COLOR",
    "XDG_RUNTIME_DIR",
    "XDG_CONFIG_HOME",
    "XDG_CACHE_HOME",
    "XDG_DATA_HOME",
    "XDG_STATE_HOME",
    "SSL_CERT_FILE",
    "SSL_CERT_DIR",
    "REQUESTS_CA_BUNDLE",
    "CURL_CA_BUNDLE",
    "HTTP_PROXY",
    "HTTPS_PROXY",
    "NO_PROXY",
    "http_proxy",
    "https_proxy",
    "no_proxy",
    "ALL_PROXY",
    "all_proxy",
];

pub(crate) fn is_valid_env_var_name(name: &str) -> bool {
    let mut chars = name.chars();
    matches!(chars.next(), Some(first) if first == '_' || first.is_ascii_alphabetic())
        && chars.all(|character| character == '_' || character.is_ascii_alphanumeric())
}

/// Environment name equality for allow/deny/dedup. Windows process env is
/// case-insensitive, so mixed-case spellings of the same variable must not
/// bypass denylists or miss allowlists there.
pub(crate) fn env_names_equal(left: &str, right: &str) -> bool {
    if cfg!(windows) {
        left.eq_ignore_ascii_case(right)
    } else {
        left == right
    }
}

pub(crate) fn env_name_starts_with(name: &str, prefix: &str) -> bool {
    if cfg!(windows) {
        name.len() >= prefix.len()
            && name.as_bytes()[..prefix.len()].eq_ignore_ascii_case(prefix.as_bytes())
    } else {
        name.starts_with(prefix)
    }
}

/// Whether `key` is a process-runtime entry every ACP child may inherit.
pub(crate) fn base_env_allowed(key: &str) -> bool {
    ENV_ALLOWLIST_EXACT
        .iter()
        .any(|allowed| env_names_equal(key, allowed))
        || env_name_starts_with(key, "LC_")
}

/// Process environment as Unicode pairs. Non-Unicode names or values are
/// skipped rather than panicking on `std::env::vars()`.
pub(crate) fn unicode_process_env() -> impl Iterator<Item = (String, String)> {
    std::env::vars_os().filter_map(|(key, value)| {
        let key = key.into_string().ok()?;
        let value = value.into_string().ok()?;
        Some((key, value))
    })
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn base_allowlist_admits_runtime_entries_and_locale_only() {
        for allowed in ["PATH", "HOME", "LC_ALL", "LC_CTYPE", "HTTPS_PROXY"] {
            assert!(base_env_allowed(allowed), "{allowed}");
        }
        for blocked in ["AWS_SECRET_ACCESS_KEY", "OPENAI_API_KEY", "GEMINI_API_KEY"] {
            assert!(!base_env_allowed(blocked), "{blocked}");
        }
    }

    #[test]
    fn env_var_names_follow_posix_shape() {
        assert!(is_valid_env_var_name("_A1"));
        assert!(!is_valid_env_var_name("1A"));
        assert!(!is_valid_env_var_name("A-B"));
        assert!(!is_valid_env_var_name(""));
    }
}
