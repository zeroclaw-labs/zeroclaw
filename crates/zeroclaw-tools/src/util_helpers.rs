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
