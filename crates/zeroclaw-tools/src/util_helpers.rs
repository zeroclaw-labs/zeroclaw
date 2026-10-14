/// Truncate a string to `max_chars` Unicode characters, appending "..." if truncated.
pub fn truncate_with_ellipsis(s: &str, max_chars: usize) -> String {
    match s.char_indices().nth(max_chars) {
        Some((idx, _)) => format!("{}...", s[..idx].trim_end()),
        None => s.to_string(),
    }
}

/// Utility enum for handling optional values in config set/unset operations.
pub enum MaybeSet<T> {
    Set(T),
    Unset,
    Null,
}

/// Adjusts a Windows drive or UNC path to strip its verbatim prefix.
/// On Windows, Git and some legacy tools do not support paths starting with `\\?\`
/// as the current directory or within arguments.
/// Non-Unicode paths remain unchanged so callers can reject them explicitly.
/// Unsupported verbatim paths are passed through unchanged.
pub fn clean_verbatim_path(path: &std::path::Path) -> std::path::PathBuf {
    #[cfg(any(target_os = "windows", test))]
    {
        let Some(path_str) = path.to_str() else {
            return path.to_path_buf();
        };
        if let Some(rest) = path_str.strip_prefix(r"\\?\UNC\") {
            return std::path::PathBuf::from(format!(r"\\{rest}"));
        }
        // A drive path begins with `<drive>:` after the verbatim prefix. Leave
        // unsupported forms, such as volume-GUID paths, untouched.
        if let Some(rest) = path_str
            .strip_prefix(r"\\?\")
            .filter(|rest| rest.chars().nth(1) == Some(':'))
        {
            return std::path::PathBuf::from(rest);
        }
    }
    path.to_path_buf()
}

#[cfg(test)]
pub(crate) fn workspace_prefixed_relative_path_for_test(
    workspace: &std::path::Path,
) -> std::path::PathBuf {
    let mut relative = std::path::PathBuf::new();
    for component in workspace.components() {
        match component {
            std::path::Component::Prefix(_)
            | std::path::Component::RootDir
            | std::path::Component::CurDir => {}
            std::path::Component::ParentDir => {
                panic!("test workspace path must not contain parent components")
            }
            std::path::Component::Normal(part) => relative.push(part),
        }
    }
    relative
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn clean_verbatim_path_strips_verbatim_drive_prefix() {
        let verbatim_path = std::path::Path::new(r"\\?\C:\Users\me\repo");
        let cleaned = clean_verbatim_path(verbatim_path);
        assert_eq!(cleaned.to_string_lossy(), r"C:\Users\me\repo");
    }

    #[test]
    fn clean_verbatim_path_leaves_normal_path_unchanged() {
        let normal_path = std::path::Path::new(r"C:\Users\me\repo");
        let cleaned = clean_verbatim_path(normal_path);
        assert_eq!(cleaned.to_string_lossy(), r"C:\Users\me\repo");
    }

    #[test]
    fn clean_verbatim_path_leaves_unix_path_unchanged() {
        let unix_path = std::path::Path::new("/home/me/repo");
        let cleaned = clean_verbatim_path(unix_path);
        assert_eq!(cleaned.to_string_lossy(), "/home/me/repo");
    }

    #[test]
    fn clean_verbatim_path_converts_verbatim_unc_path() {
        let unc_server_path = std::path::Path::new(r"\\?\UNC\server\share");
        let cleaned = clean_verbatim_path(unc_server_path);
        assert_eq!(cleaned.to_string_lossy(), r"\\server\share");
    }

    #[test]
    fn clean_verbatim_path_preserves_unsupported_verbatim_prefixes() {
        let volume_path =
            std::path::Path::new(r"\\?\Volume{01234567-89ab-cdef-0123-456789abcdef}\repo");
        assert_eq!(clean_verbatim_path(volume_path), volume_path);
    }

    #[cfg(unix)]
    #[test]
    fn clean_verbatim_path_preserves_non_unicode_paths() {
        use std::os::unix::ffi::{OsStrExt, OsStringExt};

        let path = std::path::PathBuf::from(std::ffi::OsString::from_vec(
            b"\\\\?\\C:\\Users\\me\\re\xffpo".to_vec(),
        ));
        let cleaned = clean_verbatim_path(&path);

        assert_eq!(cleaned.as_os_str().as_bytes(), path.as_os_str().as_bytes());
        assert!(cleaned.to_str().is_none());
    }

    #[test]
    fn truncate_with_ellipsis_keeps_short_or_exact_strings() {
        assert_eq!(truncate_with_ellipsis("hello", 10), "hello");
        // Exactly at the char budget is not truncated.
        assert_eq!(truncate_with_ellipsis("hello", 5), "hello");
        assert_eq!(truncate_with_ellipsis("", 3), "");
    }

    #[test]
    fn truncate_with_ellipsis_truncates_and_appends() {
        assert_eq!(truncate_with_ellipsis("hello world", 5), "hello...");
    }

    #[test]
    fn truncate_with_ellipsis_trims_trailing_space_before_ellipsis() {
        // The kept slice "ab " is trimmed before the ellipsis is appended.
        assert_eq!(truncate_with_ellipsis("ab cd", 3), "ab...");
    }

    #[test]
    fn truncate_with_ellipsis_counts_unicode_chars_not_bytes() {
        // "héllo" cut after 2 chars keeps "hé" (3 bytes), not 2 bytes.
        assert_eq!(truncate_with_ellipsis("héllo", 2), "hé...");
    }
}
