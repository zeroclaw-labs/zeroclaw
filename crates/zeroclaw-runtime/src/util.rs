//! Utility functions for `ZeroClaw`.
//!
//! This module contains reusable helper functions used across the codebase.

/// Allowed serial device path prefixes — reject arbitrary paths for security.
/// Used by hardware serial transport and peripherals.
const SERIAL_ALLOWED_PATH_PREFIXES: &[&str] = &[
    "/dev/ttyACM",
    "/dev/ttyUSB",
    "/dev/tty.usbmodem",
    "/dev/cu.usbmodem",
    "/dev/tty.usbserial",
    "/dev/cu.usbserial", // Arduino Uno (FTDI), clones
    "COM",               // Windows
];

/// Returns true if the path is an allowed serial device (USB CDC, FTDI, etc.).
/// Rejects arbitrary paths like /etc/passwd or /dev/sda.
pub fn is_serial_path_allowed(path: &str) -> bool {
    SERIAL_ALLOWED_PATH_PREFIXES
        .iter()
        .any(|prefix| path.starts_with(prefix))
}

/// Truncate a string to at most `max_chars` characters, appending "..." if truncated.
///
/// This function safely handles multi-byte UTF-8 characters (emoji, CJK, accented characters)
/// by using character boundaries instead of byte indices.
///
/// # Arguments
/// * `s` - The string to truncate
/// * `max_chars` - Maximum number of characters to keep (excluding "...")
///
/// # Returns
/// * Original string if length <= `max_chars`
/// * Truncated string with "..." appended if length > `max_chars`
///
/// # Examples
/// ```ignore
/// use zeroclaw::util::truncate_with_ellipsis;
///
/// // ASCII string - no truncation needed
/// assert_eq!(truncate_with_ellipsis("hello", 10), "hello");
///
/// // ASCII string - truncation needed
/// assert_eq!(truncate_with_ellipsis("hello world", 5), "hello...");
///
/// // Multi-byte UTF-8 (emoji) - safe truncation
/// assert_eq!(truncate_with_ellipsis("Hello 🦀 World", 8), "Hello 🦀...");
/// assert_eq!(truncate_with_ellipsis("😀😀😀😀", 2), "😀😀...");
///
/// // Empty string
/// assert_eq!(truncate_with_ellipsis("", 10), "");
/// ```
pub fn truncate_with_ellipsis(s: &str, max_chars: usize) -> String {
    match s.char_indices().nth(max_chars) {
        Some((idx, _)) => {
            let truncated = &s[..idx];
            // Trim trailing whitespace for cleaner output
            format!("{}...", truncated.trim_end())
        }
        None => s.to_string(),
    }
}

/// Truncate a string to at most `max_chars` characters of **content**, then
/// append a friendly marker of the form `…[truncated {n} of {total} chars]`.
/// Returns `None` if `max_chars == 0` (meaning: don't write this field).
///
/// The marker reports how many characters were cut (`n`) out of the original
/// `total`, so `n = total - max_chars` when the original exceeds the budget.
///
/// The marker is metadata and does **not** count against `max_chars`: the kept
/// content is always exactly `max_chars` characters (char-boundary safe), with
/// the marker appended on top. Callers that need a hard ceiling on total output
/// length (content + marker) should apply their own bound downstream.
///
/// Used for OTel attribute truncation where `None` signals "omit this attribute entirely".
pub fn truncate_field(s: &str, max_chars: usize) -> Option<String> {
    if max_chars == 0 {
        return None;
    }
    let total = s.chars().count();
    if total <= max_chars {
        return Some(s.to_string());
    }

    // Keep exactly `max_chars` chars of content; the marker is appended on top
    // and does not eat into the content budget.
    let n = total - max_chars;
    let head = take_first_chars(s, max_chars);
    Some(format!(
        "{}…[truncated {} of {} chars]",
        head.trim_end(),
        n,
        total
    ))
}

/// Return the leading `n` chars of `s` as a `&str` slice (char-boundary safe).
fn take_first_chars(s: &str, n: usize) -> &str {
    match s.char_indices().nth(n) {
        Some((idx, _)) => &s[..idx],
        None => s,
    }
}

/// Recursively truncate all string leaves in a JSON value to `max_chars`.
/// Returns `None` if `max_chars == 0` (meaning: don't write this field).
///
/// Preserves JSON structure (object keys, array order, nesting) — only
/// string values are truncated. Used for tool arguments JSON where we want
/// to keep the structure while bounding individual string values.
pub fn truncate_json_leaves(v: &serde_json::Value, max_chars: usize) -> Option<serde_json::Value> {
    if max_chars == 0 {
        return None;
    }
    match v {
        serde_json::Value::String(s) => truncate_field(s, max_chars).map(serde_json::Value::String),
        serde_json::Value::Array(arr) => {
            let truncated: Option<Vec<serde_json::Value>> = arr
                .iter()
                .map(|item| truncate_json_leaves(item, max_chars))
                .collect();
            truncated.map(serde_json::Value::Array)
        }
        serde_json::Value::Object(obj) => {
            let truncated_map: Option<serde_json::Map<String, serde_json::Value>> = obj
                .iter()
                .map(|(k, v)| truncate_json_leaves(v, max_chars).map(|tv| (k.clone(), tv)))
                .collect();
            truncated_map.map(serde_json::Value::Object)
        }
        _ => Some(v.clone()), // Numbers, bools, null pass through unchanged
    }
}

/// Utility enum for handling optional values.
pub enum MaybeSet<T> {
    Set(T),
    Unset,
    Null,
}

/// Return free heap memory at the top of glibc's arenas to the kernel.
///
/// After the session reaper or an explicit `session/close` drops an `Agent`
/// and its conversation history, glibc keeps the freed pages in its per-arena
/// free lists instead of `munmap`-ing them, so resident set size stays flat
/// despite a correct free. This releases the arena tops so the daemon's RSS
/// actually falls. No-op on targets without glibc's `malloc_trim`.
///
/// Gated on Linux + glibc specifically: `libc` is a `cfg(unix)`-only
/// dependency, and `malloc_trim` is a glibc extension. A bare
/// `target_env = "gnu"` also matches the `windows-gnu` target, where `libc`
/// is absent and the call fails to resolve.
#[cfg(all(target_os = "linux", target_env = "gnu"))]
pub fn release_freed_heap() {
    // SAFETY: `malloc_trim` only inspects and releases the allocator's own
    // free lists. It takes no Rust-owned pointer and frees nothing the program
    // still references, so it cannot dangle a pointer or double free.
    unsafe {
        libc::malloc_trim(0);
    }
}

#[cfg(not(all(target_os = "linux", target_env = "gnu")))]
pub fn release_freed_heap() {}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn test_truncate_ascii_no_truncation() {
        // ASCII string shorter than limit - no change
        assert_eq!(truncate_with_ellipsis("hello", 10), "hello");
        assert_eq!(truncate_with_ellipsis("hello world", 50), "hello world");
    }

    #[test]
    fn test_truncate_ascii_with_truncation() {
        // ASCII string longer than limit - truncates
        assert_eq!(truncate_with_ellipsis("hello world", 5), "hello...");
        assert_eq!(
            truncate_with_ellipsis("This is a long message", 10),
            "This is a..."
        );
    }

    #[test]
    fn test_truncate_empty_string() {
        assert_eq!(truncate_with_ellipsis("", 10), "");
    }

    #[test]
    fn test_truncate_at_exact_boundary() {
        // String exactly at boundary - no truncation
        assert_eq!(truncate_with_ellipsis("hello", 5), "hello");
    }

    #[test]
    fn test_truncate_emoji_single() {
        // Single emoji (4 bytes) - should not panic
        let s = "🦀";
        assert_eq!(truncate_with_ellipsis(s, 10), s);
        assert_eq!(truncate_with_ellipsis(s, 1), s);
    }

    #[test]
    fn test_truncate_emoji_multiple() {
        // Multiple emoji - safe truncation at character boundary
        let s = "😀😀😀😀"; // 4 emoji, each 4 bytes = 16 bytes total
        assert_eq!(truncate_with_ellipsis(s, 2), "😀😀...");
        assert_eq!(truncate_with_ellipsis(s, 3), "😀😀😀...");
    }

    #[test]
    fn test_truncate_mixed_ascii_emoji() {
        // Mixed ASCII and emoji
        assert_eq!(truncate_with_ellipsis("Hello 🦀 World", 8), "Hello 🦀...");
        assert_eq!(truncate_with_ellipsis("Hi 😊", 10), "Hi 😊");
    }

    #[test]
    fn test_truncate_cjk_characters() {
        // CJK characters (Chinese - each is 3 bytes)
        let s = "这是一个测试消息用来触发崩溃的中文"; // 21 characters
        let result = truncate_with_ellipsis(s, 16);
        assert!(result.ends_with("..."));
        assert!(result.is_char_boundary(result.len() - 1));
    }

    #[test]
    fn test_truncate_accented_characters() {
        // Accented characters (2 bytes each in UTF-8)
        let s = "café résumé naïve";
        assert_eq!(truncate_with_ellipsis(s, 10), "café résum...");
    }

    #[test]
    fn test_truncate_unicode_edge_case() {
        // Mix of 1-byte, 2-byte, 3-byte, and 4-byte characters
        let s = "aé你好🦀"; // 1 + 1 + 2 + 2 + 4 bytes = 10 bytes, 5 chars
        assert_eq!(truncate_with_ellipsis(s, 3), "aé你...");
    }

    #[test]
    fn test_truncate_long_string() {
        // Long ASCII string
        let s = "a".repeat(200);
        let result = truncate_with_ellipsis(&s, 50);
        assert_eq!(result.len(), 53); // 50 + "..."
        assert!(result.ends_with("..."));
    }

    #[test]
    fn test_truncate_zero_max_chars() {
        // Edge case: max_chars = 0
        assert_eq!(truncate_with_ellipsis("hello", 0), "...");
    }

    #[test]
    fn test_truncate_field_zero_returns_none() {
        assert_eq!(truncate_field("hello", 0), None);
    }

    #[test]
    fn test_truncate_field_short_returns_some() {
        assert_eq!(truncate_field("hello", 10), Some("hello".to_string()));
    }

    #[test]
    fn test_truncate_field_truncates_with_marker() {
        // 500 'a's truncated at 50: marker reports chars cut = 450, total = 500.
        let s = "a".repeat(500);
        let result = truncate_field(&s, 50).unwrap();
        assert!(result.starts_with(&"a".repeat(50)), "got: {result}");
        assert!(
            result.contains("truncated 450 of 500 chars]"),
            "got: {result}"
        );
    }

    #[test]
    fn test_truncate_field_emoji_safe() {
        // 30 suns (total=30) truncated at 28 keeps 28 glyphs + marker,
        // cutting on a char boundary (no panic, no split surrogate).
        let result = truncate_field("☀".repeat(30).as_str(), 28).unwrap();
        assert!(result.starts_with(&"☀".repeat(28)), "got: {result}");
        assert!(result.contains("truncated 2 of 30 chars]"), "got: {result}");
    }

    #[test]
    fn test_truncate_field_keeps_exactly_max_chars() {
        // The marker is metadata and must not eat into the content budget:
        // the kept content is exactly `max_chars`, with `n = total - max_chars`.
        for &(len, max) in &[(500, 50), (12345, 100), (7, 6), (99, 5), (1000, 3)] {
            let s = "x".repeat(len);
            let result = truncate_field(&s, max).unwrap();
            // Content prefix = exactly `max_chars` 'x's.
            assert!(
                result.starts_with(&"x".repeat(max)),
                "len={len} max={max} → got {result}"
            );
            // Marker reports the right cut count.
            let expected = format!("truncated {} of {} chars]", len - max, len);
            assert!(
                result.contains(&expected),
                "len={len} max={max} → got {result}"
            );
        }
    }

    #[test]
    fn test_truncate_json_leaves_zero_returns_none() {
        let json = serde_json::json!({"key": "value"});
        assert_eq!(truncate_json_leaves(&json, 0), None);
    }

    #[test]
    fn test_truncate_json_leaves_preserves_structure() {
        let json = serde_json::json!({
            "name": "Alice",
            "nested": {"value": "long string that should be truncated"}
        });
        let result = truncate_json_leaves(&json, 10);
        assert!(result.is_some());
        let binding = result.unwrap();
        let obj = binding.as_object().unwrap();
        assert!(obj.contains_key("name"));
        assert!(obj.contains_key("nested"));
    }

    #[test]
    fn test_truncate_json_leaves_array() {
        let json = serde_json::json!(["short", "very long string here"]);
        let result = truncate_json_leaves(&json, 10);
        assert!(result.is_some());
        let binding = result.unwrap();
        let arr = binding.as_array().unwrap();
        assert_eq!(arr.len(), 2);
    }
}
