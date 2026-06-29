//! Shared path helpers used by both schema-tier validation and the
//! scoped file browser. Single source of truth for "lexically normalize a
//! path" and "resolve a relative input under a fixed root with no escape".

use std::path::{Component, Path, PathBuf};

/// Resolve `.` and `..` components lexically — never touches the
/// filesystem. Sufficient for "stays inside `<root>`" reasoning where the
/// path may not yet exist.
#[must_use]
pub fn normalize_lexical(path: &Path) -> PathBuf {
    let mut out = PathBuf::new();
    for component in path.components() {
        match component {
            Component::ParentDir => {
                out.pop();
            }
            Component::CurDir => {}
            other => out.push(other.as_os_str()),
        }
    }
    out
}

/// Resolve `raw` (interpreted as relative-to-root unless absolute) and
/// assert the result stays under `root` after lexical normalization.
/// Returns the normalized absolute path on success.
pub fn resolve_under(root: &Path, raw: &str) -> Result<PathBuf, RootEscapeError> {
    let trimmed = raw.trim_matches('/');
    let candidate = if trimmed.is_empty() {
        root.to_path_buf()
    } else {
        root.join(trimmed)
    };
    let normalized = normalize_lexical(&candidate);
    let root_normalized = normalize_lexical(root);
    if !normalized.starts_with(&root_normalized) {
        return Err(RootEscapeError {
            input: raw.to_string(),
            root: root_normalized.display().to_string(),
        });
    }
    Ok(normalized)
}

#[derive(Debug, thiserror::Error)]
#[error("path '{input}' escapes root '{root}'")]
pub struct RootEscapeError {
    pub input: String,
    pub root: String,
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn empty_input_resolves_to_root() {
        let root = Path::new("/tmp/install/shared");
        assert_eq!(resolve_under(root, "").unwrap(), root);
        assert_eq!(resolve_under(root, "/").unwrap(), root);
    }

    #[test]
    fn relative_input_joins_under_root() {
        let root = Path::new("/tmp/install/shared");
        assert_eq!(
            resolve_under(root, "skills/coding").unwrap(),
            root.join("skills/coding"),
        );
    }

    #[test]
    fn dotdot_escape_is_rejected() {
        let root = Path::new("/tmp/install/shared");
        assert!(resolve_under(root, "../etc").is_err());
        assert!(resolve_under(root, "skills/../../etc").is_err());
    }

    #[test]
    fn double_slash_normalized_away() {
        let root = Path::new("/tmp/install/shared");
        assert_eq!(
            resolve_under(root, "skills//coding/").unwrap(),
            root.join("skills/coding"),
        );
    }

    #[test]
    fn dotdot_within_root_is_normalized() {
        let root = Path::new("/tmp/install/shared");
        assert_eq!(
            resolve_under(root, "skills/../coding").unwrap(),
            root.join("coding"),
        );
    }
}
