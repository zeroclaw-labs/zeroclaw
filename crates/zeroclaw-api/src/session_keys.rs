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

/// Canonical session key for a CLI turn backed by `session_state_file`.
/// Sole source of truth for the `cli:<path>` shape; the memory `session_id`
/// filter and the scope-span `session_key` must agree, so both derive here.
pub fn cli_session_key(session_state_file: Option<&std::path::Path>) -> Option<String> {
    let raw = session_state_file?.to_string_lossy().trim().to_string();
    if raw.is_empty() {
        return None;
    }
    Some(sanitize_session_key(&format!("cli:{raw}")))
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

    #[test]
    fn cli_session_key_derives_sanitized_cli_prefix() {
        let p = std::path::PathBuf::from("/var/run/sess a.jsonl");
        assert_eq!(
            super::cli_session_key(Some(&p)),
            Some("cli__var_run_sess_a_jsonl".to_string())
        );
    }

    #[test]
    fn cli_session_key_none_for_missing_or_empty() {
        assert_eq!(super::cli_session_key(None), None);
        let empty = std::path::PathBuf::from("   ");
        assert_eq!(super::cli_session_key(Some(&empty)), None);
    }
}
