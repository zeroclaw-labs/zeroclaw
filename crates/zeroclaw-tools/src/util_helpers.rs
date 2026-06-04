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
