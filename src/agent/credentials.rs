use regex::{Regex, RegexSet};
use std::sync::LazyLock;

static SENSITIVE_KEY_PATTERNS: LazyLock<RegexSet> = LazyLock::new(|| {
    RegexSet::new([
        r"(?i)token",
        r"(?i)api[_-]?key",
        r"(?i)password",
        r"(?i)secret",
        r"(?i)user[_-]?key",
        r"(?i)bearer",
        r"(?i)credential",
    ])
    .unwrap()
});

static SENSITIVE_KV_REGEX: LazyLock<Regex> = LazyLock::new(|| {
    Regex::new(r#"(?i)(token|api[_-]?key|password|secret|user[_-]?key|bearer|credential)["']?\s*[:=]\s*(?:"([^"]{8,})"|'([^']{8,})'|([a-zA-Z0-9_\-\.]{8,}))"#).unwrap()
});

/// Scrub credentials from tool output to prevent accidental exfiltration.
/// Replaces known credential patterns with a redacted placeholder while preserving
/// a small prefix for context.
pub(crate) fn scrub_credentials(input: &str) -> String {
    SENSITIVE_KV_REGEX
        .replace_all(input, |caps: &regex::Captures| {
            let full_match = &caps[0];
            let key = &caps[1];
            let val = caps
                .get(2)
                .or(caps.get(3))
                .or(caps.get(4))
                .map(|m| m.as_str())
                .unwrap_or("");

            // Preserve first 4 chars for context, then redact.
            // Use char_indices to find the byte offset of the 4th character
            // so we never slice in the middle of a multi-byte UTF-8 sequence.
            let prefix = if val.len() > 4 {
                val.char_indices()
                    .nth(4)
                    .map(|(byte_idx, _)| &val[..byte_idx])
                    .unwrap_or(val)
            } else {
                ""
            };

            if full_match.contains(':') {
                if full_match.contains('"') {
                    format!("\"{}\": \"{}*[REDACTED]\"", key, prefix)
                } else {
                    format!("{}: {}*[REDACTED]", key, prefix)
                }
            } else if full_match.contains('=') {
                if full_match.contains('"') {
                    format!("{}=\"{}*[REDACTED]\"", key, prefix)
                } else {
                    format!("{}={}*[REDACTED]", key, prefix)
                }
            } else {
                format!("{}: {}*[REDACTED]", key, prefix)
            }
        })
        .to_string()
}

#[cfg(test)]
mod tests {
    use super::scrub_credentials;

    #[test]
    fn scrub_credentials_redacts_bearer_token() {
        let input = "API_KEY=sk-1234567890abcdef; token: 1234567890; password=\"secret123456\"";
        let scrubbed = scrub_credentials(input);
        assert!(scrubbed.contains("API_KEY=sk-1*[REDACTED]"));
        assert!(scrubbed.contains("token: 1234*[REDACTED]"));
        assert!(scrubbed.contains("password=\"secr*[REDACTED]\""));
        assert!(!scrubbed.contains("abcdef"));
        assert!(!scrubbed.contains("secret123456"));
    }

    #[test]
    fn scrub_credentials_redacts_json_api_key() {
        let input = r#"{"api_key": "sk-1234567890", "other": "public"}"#;
        let scrubbed = scrub_credentials(input);
        assert!(scrubbed.contains("\"api_key\": \"sk-1*[REDACTED]\""));
        assert!(scrubbed.contains("public"));
    }

    #[test]
    fn scrub_credentials_empty_input() {
        let result = scrub_credentials("");
        assert_eq!(result, "");
    }

    #[test]
    fn scrub_credentials_no_sensitive_data() {
        let input = "normal text without any secrets";
        let result = scrub_credentials(input);
        assert_eq!(
            result, input,
            "non-sensitive text should pass through unchanged"
        );
    }

    #[test]
    fn scrub_credentials_multibyte_chars_no_panic() {
        // Regression test for #3024: byte index 4 is not a char boundary
        // when the captured value contains multi-byte UTF-8 characters.
        // The regex only matches quoted values for non-ASCII content, since
        // capture group 4 is restricted to [a-zA-Z0-9_\-\.].
        let input = "password=\"\u{4f60}\u{7684}WiFi\u{5bc6}\u{7801}ab\"";
        let result = scrub_credentials(input);
        assert!(
            result.contains("[REDACTED]"),
            "multi-byte quoted value should be redacted without panic, got: {result}"
        );
    }

    #[test]
    fn scrub_credentials_short_values_not_redacted() {
        // Values shorter than 8 chars should not be redacted
        let input = r#"api_key="short""#;
        let result = scrub_credentials(input);
        assert_eq!(result, input, "short values should not be redacted");
    }
}
