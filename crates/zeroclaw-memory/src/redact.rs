//! Category-based redaction for durable memory content.
//!
//! [`redact`] rewrites matches for the enabled [`RedactCategory`] set to
//! `[REDACTED:<category>]` placeholders before content is persisted. The
//! write path applies it only when `[memory.policy].redact_on_write` is
//! enabled; the category list comes from
//! `[memory.policy].redact_categories`.

use regex::Regex;
use std::ops::Range;
use std::sync::LazyLock;

/// A redactable content category.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum RedactCategory {
    Secret,
    ApiKey,
    PrivateKey,
    Email,
    Phone,
}

impl RedactCategory {
    /// Parse a config string into a category. Unknown values return `None`
    /// and are skipped by the caller.
    pub fn from_config(value: &str) -> Option<Self> {
        match value.trim().to_ascii_lowercase().as_str() {
            "secret" => Some(Self::Secret),
            "api_key" | "apikey" => Some(Self::ApiKey),
            "private_key" | "privatekey" => Some(Self::PrivateKey),
            "email" => Some(Self::Email),
            "phone" => Some(Self::Phone),
            _ => None,
        }
    }
}

impl std::fmt::Display for RedactCategory {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        match self {
            Self::Secret => write!(f, "secret"),
            Self::ApiKey => write!(f, "api_key"),
            Self::PrivateKey => write!(f, "private_key"),
            Self::Email => write!(f, "email"),
            Self::Phone => write!(f, "phone"),
        }
    }
}

/// A single redaction: the category and the replaced byte range.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct RedactionHit {
    pub category: RedactCategory,
    pub byte_range: Range<usize>,
}

struct RedactionPattern {
    category: RedactCategory,
    regex: Regex,
}

static PATTERNS: LazyLock<Vec<RedactionPattern>> = LazyLock::new(|| {
    vec![
        RedactionPattern {
            category: RedactCategory::PrivateKey,
            regex: Regex::new(r#"-----BEGIN [A-Z ]*PRIVATE KEY-----[\s\S]*?-----END [A-Z ]*PRIVATE KEY-----"#)
                .expect("valid private-key redaction regex"),
        },
        RedactionPattern {
            category: RedactCategory::ApiKey,
            regex: Regex::new(
                r#"(?i)\b(api[_-]?key)\s*[:=]\s*['"]?[A-Za-z0-9_./+=-]{16,}['"]?"#,
            )
            .expect("valid api-key redaction regex"),
        },
        RedactionPattern {
            category: RedactCategory::Secret,
            regex: Regex::new(
                r#"(?i)\b(secret|token|password|credential)\s*[:=]\s*['"]?[A-Za-z0-9_./+=-]{12,}['"]?"#,
            )
            .expect("valid secret redaction regex"),
        },
        RedactionPattern {
            category: RedactCategory::Email,
            regex: Regex::new(r#"\b[A-Za-z0-9._%+-]+@[A-Za-z0-9.-]+\.[A-Za-z]{2,}\b"#)
                .expect("valid email redaction regex"),
        },
        RedactionPattern {
            category: RedactCategory::Phone,
            regex: Regex::new(r#"\b(?:\+?\d[\d .()-]{7,}\d)\b"#)
                .expect("valid phone redaction regex"),
        },
    ]
});

/// Replace matches for the enabled categories with
/// `[REDACTED:<category>]` placeholders. Returns the rewritten content
/// and the list of replacements made.
pub fn redact(content: &str, categories: &[RedactCategory]) -> (String, Vec<RedactionHit>) {
    let mut redacted = content.to_string();
    let mut hits = Vec::new();

    for pattern in PATTERNS
        .iter()
        .filter(|pattern| categories.contains(&pattern.category))
    {
        let mut next = String::with_capacity(redacted.len());
        let mut last = 0usize;
        for hit in pattern.regex.find_iter(&redacted) {
            next.push_str(&redacted[last..hit.start()]);
            next.push_str(&format!("[REDACTED:{}]", pattern.category));
            hits.push(RedactionHit {
                category: pattern.category,
                byte_range: hit.start()..hit.end(),
            });
            last = hit.end();
        }
        if last == 0 {
            continue;
        }
        next.push_str(&redacted[last..]);
        redacted = next;
    }

    (redacted, hits)
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn redacts_email_and_secret() {
        let (redacted, hits) = redact(
            r#"contact user@example.com with token = "abcdefghijklmnop""#,
            &[RedactCategory::Email, RedactCategory::Secret],
        );
        assert!(redacted.contains("[REDACTED:email]"));
        assert!(redacted.contains("[REDACTED:secret]"));
        assert_eq!(hits.len(), 2);
        assert!(hits.iter().all(|hit| !hit.byte_range.is_empty()));
    }

    #[test]
    fn identity_when_no_category_matches() {
        let input = "contact user@example.com";
        let (redacted, hits) = redact(input, &[RedactCategory::Secret]);
        assert_eq!(redacted, input);
        assert!(hits.is_empty());
    }

    #[test]
    fn unknown_config_category_is_skipped() {
        assert_eq!(RedactCategory::from_config("nonsense"), None);
        assert_eq!(
            RedactCategory::from_config(" API_KEY "),
            Some(RedactCategory::ApiKey)
        );
    }
}
