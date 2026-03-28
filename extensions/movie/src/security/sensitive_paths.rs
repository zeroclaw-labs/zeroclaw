use std::path::Path;

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
}
