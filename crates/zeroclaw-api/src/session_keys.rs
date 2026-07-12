//! Session key normalization shared across infra and memory backends.
//!
//! Channel orchestration uses two identifiers derived from a `ChannelMessage`:
//! one ends up as a JSONL filename (via `SessionStore::session_path`) and as
//! an in-memory HashMap key for the conversation history cache, while the
//! same identifier is also passed to `Memory::store`/`Memory::recall` as the
//! `session_id` filter. Because filesystem-safe sanitization is applied when
//! writing the JSONL file, every other layer must use the same sanitized form
//! to keep lookups consistent across daemon restarts and persisted backends.

/// Replace every character outside `[A-Za-z0-9_-]` with `_`. Idempotent.
///
/// Callers building session keys must pre-apply this so the runtime HashMap
/// key, the on-disk JSONL filename, and the `session_id` column in memory
/// backends all agree.
pub fn sanitize_session_key(key: &str) -> String {
    key.chars()
        .map(|c| {
            if c.is_alphanumeric() || c == '_' || c == '-' {
                c
            } else {
                '_'
            }
        })
        .collect()
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn replaces_special_characters_with_underscore() {
        assert_eq!(
            sanitize_session_key("slack_C123_1.2_user one"),
            "slack_C123_1_2_user_one"
        );
    }

    #[test]
    fn preserves_alphanumeric_underscore_and_hyphen() {
        let key = "abc-DEF_123";
        assert_eq!(sanitize_session_key(key), key);
    }

    #[test]
    fn is_idempotent() {
        let once = sanitize_session_key("whatsapp_123@g.us_alice");
        let twice = sanitize_session_key(&once);
        assert_eq!(once, twice);
    }

    #[test]
    fn handles_empty_string() {
        assert_eq!(sanitize_session_key(""), "");
    }

    #[test]
    fn preserves_unicode_alphanumeric() {
        // is_alphanumeric() treats unicode letters/digits as alphanumeric.
        assert_eq!(sanitize_session_key("user_Алиса"), "user_Алиса");
    }
}
