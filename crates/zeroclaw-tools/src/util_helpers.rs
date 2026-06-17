/// Truncate a string to `max_chars` Unicode characters, appending "..." if truncated.
pub fn truncate_with_ellipsis(s: &str, max_chars: usize) -> String {
    match s.char_indices().nth(max_chars) {
        Some((idx, _)) => format!("{}...", s[..idx].trim_end()),
        None => s.to_string(),
    }
}

/// Largest byte index `<= max_bytes` that is still a valid UTF-8 boundary.
pub(crate) fn floor_char_boundary(s: &str, max_bytes: usize) -> usize {
    if max_bytes >= s.len() {
        return s.len();
    }
    let mut end = max_bytes;
    while end > 0 && !s.is_char_boundary(end) {
        end -= 1;
    }
    end
}

/// Utility enum for handling optional values in config set/unset operations.
pub enum MaybeSet<T> {
    Set(T),
    Unset,
    Null,
}

/// Adjusts a path on Windows to strip the UNC verbatim prefix `\\?\` if present.
/// On Windows, `cmd.exe` and some legacy tools do not support paths starting with `\\?\`
/// as the current directory or within arguments.
pub fn clean_verbatim_path(path: &std::path::Path) -> std::path::PathBuf {
    #[cfg(target_os = "windows")]
    {
        let path_str = path.to_string_lossy();
        if path_str.starts_with(r"\\?\") {
            // Check if it's a local drive path (e.g. \\?\C:\...) by checking if the 6th char is ':'
            if path_str.chars().nth(5) == Some(':') {
                return std::path::PathBuf::from(&path_str[4..]);
            }
        }
    }
    path.to_path_buf()
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn floor_char_boundary_handles_mid_codepoint_offset() {
        let text = "abc😀def";

        assert_eq!(floor_char_boundary(text, 5), 3);
        assert_eq!(floor_char_boundary(text, usize::MAX), text.len());
    }

    #[test]
    fn clean_verbatim_path_strips_unc_prefix_on_windows() {
        // Simulate a Windows verbatim UNC path
        let verbatim_path = std::path::Path::new(r"\\?\C:\Users\me\repo");
        let cleaned = clean_verbatim_path(verbatim_path);
        // On Windows, the prefix should be stripped; on other platforms, unchanged
        #[cfg(target_os = "windows")]
        assert_eq!(cleaned.to_string_lossy(), r"C:\Users\me\repo");
        #[cfg(not(target_os = "windows"))]
        assert_eq!(cleaned.to_string_lossy(), r"\\?\C:\Users\me\repo");
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
    fn clean_verbatim_path_does_not_strip_unc_driveless_path() {
        // UNC path without drive letter (e.g. \\?\UNC\server\share) should not be stripped
        let unc_server_path = std::path::Path::new(r"\\?\UNC\server\share");
        let cleaned = clean_verbatim_path(unc_server_path);
        // Should remain unchanged since there's no drive letter at position 5
        assert_eq!(cleaned.to_string_lossy(), r"\\?\UNC\server\share");
    }
}
