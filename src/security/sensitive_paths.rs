use std::path::Path;

/// Directories that the agent must never be allowed to write into,
/// regardless of `file_write` approval scope.  This prevents an agent
/// from rewriting its own config (e.g. moving tools from `always_ask`
/// to `auto_approve`) after a single `file_write` approval.
const SELF_CONFIG_COMPONENTS: &[&str] = &[".zeroclaw"];

/// Returns `true` when `path` (raw or canonicalized) resolves inside
/// the agent's own configuration directory tree.
///
/// Checks both the literal path components and, when the target already
/// exists on disk, the canonicalized path so that symlink-based bypasses
/// are caught.
pub fn is_zeroclaw_config_path(path: &Path) -> bool {
    // 1. Check literal path components.
    if path_contains_config_component(path) {
        return true;
    }

    // 2. Check canonical (resolved) path to defeat symlinks.
    if let Ok(canonical) = path.canonicalize() {
        if path_contains_config_component(&canonical) {
            return true;
        }
    }

    false
}

fn path_contains_config_component(path: &Path) -> bool {
    for component in path.components() {
        if let std::path::Component::Normal(name) = component {
            let lower = name.to_string_lossy().to_ascii_lowercase();
            if SELF_CONFIG_COMPONENTS.iter().any(|c| lower == *c) {
                return true;
            }
        }
    }
    false
}

const SENSITIVE_EXACT_FILENAMES: &[&str] = &[
    ".env",
    ".envrc",
    ".secret_key",
    ".npmrc",
    ".pypirc",
    ".git-credentials",
    "credentials",
    "credentials.json",
    "auth-profiles.json",
    "id_rsa",
    "id_dsa",
    "id_ecdsa",
    "id_ed25519",
];

const SENSITIVE_SUFFIXES: &[&str] = &[
    ".pem",
    ".key",
    ".p12",
    ".pfx",
    ".ovpn",
    ".kubeconfig",
    ".netrc",
];

const SENSITIVE_PATH_COMPONENTS: &[&str] = &[
    ".ssh", ".aws", ".gnupg", ".kube", ".docker", ".azure", ".secrets",
];

/// Returns true when a path appears to target secret-bearing material.
///
/// This check is intentionally conservative and case-insensitive to reduce
/// accidental credential exposure through tool I/O.
pub fn is_sensitive_file_path(path: &Path) -> bool {
    for component in path.components() {
        let std::path::Component::Normal(name) = component else {
            continue;
        };
        let lower = name.to_string_lossy().to_ascii_lowercase();
        if SENSITIVE_PATH_COMPONENTS.iter().any(|v| lower == *v) {
            return true;
        }
    }

    let Some(name) = path.file_name().and_then(|n| n.to_str()) else {
        return false;
    };
    let lower_name = name.to_ascii_lowercase();

    if SENSITIVE_EXACT_FILENAMES
        .iter()
        .any(|v| lower_name == v.to_ascii_lowercase())
    {
        return true;
    }

    if lower_name.starts_with(".env.") {
        return true;
    }

    SENSITIVE_SUFFIXES
        .iter()
        .any(|suffix| lower_name.ends_with(suffix))
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn detects_sensitive_exact_filenames() {
        assert!(is_sensitive_file_path(Path::new(".env")));
        assert!(is_sensitive_file_path(Path::new("ID_RSA")));
        assert!(is_sensitive_file_path(Path::new("credentials.json")));
    }

    #[test]
    fn detects_sensitive_suffixes_and_components() {
        assert!(is_sensitive_file_path(Path::new("tls/cert.pem")));
        assert!(is_sensitive_file_path(Path::new(".aws/credentials")));
        assert!(is_sensitive_file_path(Path::new(
            "ops/.secrets/runtime.txt"
        )));
    }

    #[test]
    fn ignores_regular_paths() {
        assert!(!is_sensitive_file_path(Path::new("src/main.rs")));
        assert!(!is_sensitive_file_path(Path::new("notes/readme.md")));
    }

    #[test]
    fn detects_zeroclaw_config_paths() {
        assert!(is_zeroclaw_config_path(Path::new(
            "/home/user/.zeroclaw/config.toml"
        )));
        assert!(is_zeroclaw_config_path(Path::new(".zeroclaw/config.toml")));
        assert!(is_zeroclaw_config_path(Path::new(
            "/home/user/.zeroclaw/agents.db"
        )));
    }

    #[test]
    fn ignores_non_config_paths() {
        assert!(!is_zeroclaw_config_path(Path::new("src/main.rs")));
        assert!(!is_zeroclaw_config_path(Path::new(
            "/home/user/project/file.txt"
        )));
        assert!(!is_zeroclaw_config_path(Path::new(
            "notes/zeroclaw-ideas.md"
        )));
    }
}
