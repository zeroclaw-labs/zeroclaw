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

/// Return `true` if `key` is already in canonical form: non-empty and
/// containing only `[A-Za-z0-9_-]` (plus Unicode alphanumerics, matching
/// `sanitize_session_key`). Equivalent to `sanitize_session_key(key) == key`
/// but allocation-free.
///
/// Client-facing entry points (the chat-completions endpoint and the
/// WebSocket handshake) reject noncanonical keys instead of silently
/// folding them: because `sanitize_session_key` maps every disallowed
/// character to `_`, distinct raw keys such as `alpha.beta` and `alpha/beta`
/// would otherwise collapse to the same persistence key (`gw_alpha_beta`)
/// while the client sees two different session IDs — sharing one transcript,
/// ownership record, and memory scope. Restricting inputs to canonical keys
/// makes the `gw_`-prefixed persistence key injective.
pub fn is_canonical_session_key(key: &str) -> bool {
    !key.is_empty()
        && key
            .chars()
            .all(|c| c.is_alphanumeric() || c == '_' || c == '-')
}

/// Canonical memory-session identifier shared by WS and HTTP paths.
///
/// Both transports must pass the same identifier to
/// `Agent::set_memory_session_id` so the memory backend sees a single
/// scope regardless of transport. This is the sanitized form of the
/// client-supplied session ID, matching the on-disk JSONL filename and
/// the `session_id` column in SQLite backends.
pub fn canonical_memory_id(session_id: &str) -> String {
    sanitize_session_key(session_id)
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
    fn canonical_memory_id_preserves_punctuation_key() {
        // WS and HTTP must produce identical memory-scope identifiers for
        // the same client session ID, including keys with punctuation.
        assert_eq!(canonical_memory_id("alpha.beta"), "alpha_beta");
        assert_eq!(canonical_memory_id("test.alpha"), "test_alpha");
        // UUID-based IDs are unaffected.
        let uuid = "550e8400-e29b-41d4-a716-446655440000";
        assert_eq!(canonical_memory_id(uuid), uuid);
    }

    #[test]
    fn is_canonical_accepts_canonical_keys() {
        assert!(is_canonical_session_key("abc-DEF_123"));
        assert!(is_canonical_session_key(
            "550e8400-e29b-41d4-a716-446655440000"
        ));
        assert!(is_canonical_session_key("user_Алиса"));
    }

    #[test]
    fn is_canonical_rejects_noncanonical_keys() {
        // Distinct raw keys that would collapse under sanitize_session_key.
        assert!(!is_canonical_session_key("alpha.beta"));
        assert!(!is_canonical_session_key("alpha/beta"));
        assert!(!is_canonical_session_key("alpha beta"));
        assert!(!is_canonical_session_key("alpha:beta"));
        assert!(!is_canonical_session_key("alpha@beta"));
    }

    #[test]
    fn is_canonical_rejects_empty() {
        assert!(!is_canonical_session_key(""));
    }

    #[test]
    fn is_canonical_agrees_with_sanitize_fixpoint() {
        // is_canonical_session_key(x) must equal (sanitize_session_key(x) == x)
        // for non-empty inputs.
        for k in [
            "abc-DEF_123",
            "alpha.beta",
            "alpha/beta",
            "user_Алиса",
            "550e8400-e29b-41d4",
            "has space",
        ] {
            assert_eq!(
                is_canonical_session_key(k),
                sanitize_session_key(k) == k,
                "mismatch for {k:?}"
            );
        }
    }
}
