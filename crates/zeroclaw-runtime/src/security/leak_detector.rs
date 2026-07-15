//! Credential leak detection for outbound content.
//!
//! Scans outbound messages for potential credential leaks before they are sent,
//! preventing accidental exfiltration of API keys, tokens, passwords, and other
//! sensitive values.
//!
//! Contributed from RustyClaw (MIT licensed).

use regex::Regex;
use std::collections::HashMap;
use std::sync::OnceLock;

use super::detection::{DetectionConfidence, DetectionMatch, sanitize_excerpt};

/// Minimum token length considered for high-entropy detection.
const ENTROPY_TOKEN_MIN_LEN: usize = 24;

/// Result of leak detection.
#[derive(Debug, Clone)]
pub enum LeakResult {
    /// No leaks detected.
    Clean,
    /// Potential leaks detected with redacted versions.
    Detected {
        /// Descriptions of detected leak patterns.
        patterns: Vec<String>,
        /// Content with sensitive values redacted.
        redacted: String,
    },
}

// ─── Shared credential-pattern definitions (single source of truth) ─────────
//
// Structured credential patterns are defined once here and consumed by both
// the legacy `scan` projection (via the `check_*` methods) and the typed
// `detect` API [I7]. Private keys (PEM markers) and high-entropy heuristics
// keep their bespoke logic and are handled directly by `detect`.

/// Structured-credential regex groups shared by `scan` and `detect`. Each entry
/// carries the confidence the typed `detect` API attaches: structured
/// key-shaped patterns are `High` (they identify a specific credential format),
/// keyword-anchored generic secrets are `Medium` (a weaker signal that must not
/// reach the screening `Denial` disposition on its own). `scan` ignores the
/// confidence field.
fn api_key_patterns() -> &'static [(Regex, &'static str, DetectionConfidence)] {
    static PATTERNS: OnceLock<Vec<(Regex, &'static str, DetectionConfidence)>> = OnceLock::new();
    PATTERNS.get_or_init(|| {
        vec![
            // Stripe
            (
                Regex::new(r"sk_(live|test)_[a-zA-Z0-9]{24,}").unwrap(),
                "Stripe secret key",
                DetectionConfidence::High,
            ),
            (
                Regex::new(r"pk_(live|test)_[a-zA-Z0-9]{24,}").unwrap(),
                "Stripe publishable key",
                DetectionConfidence::High,
            ),
            // OpenAI
            (
                Regex::new(r"sk-[a-zA-Z0-9]{20,}T3BlbkFJ[a-zA-Z0-9]{20,}").unwrap(),
                "OpenAI API key",
                DetectionConfidence::High,
            ),
            (
                Regex::new(r"sk-[a-zA-Z0-9]{48,}").unwrap(),
                "OpenAI-style API key",
                DetectionConfidence::High,
            ),
            // Anthropic
            (
                Regex::new(r"sk-ant-[a-zA-Z0-9-_]{32,}").unwrap(),
                "Anthropic API key",
                DetectionConfidence::High,
            ),
            // Groq
            (
                Regex::new(r"gsk_[a-zA-Z0-9]{20,}").unwrap(),
                "Groq API key",
                DetectionConfidence::High,
            ),
            // Google
            (
                Regex::new(r"AIza[a-zA-Z0-9_-]{35}").unwrap(),
                "Google API key",
                DetectionConfidence::High,
            ),
            // GitHub
            (
                Regex::new(r"gh[pousr]_[a-zA-Z0-9]{36,}").unwrap(),
                "GitHub token",
                DetectionConfidence::High,
            ),
            (
                Regex::new(r"github_pat_[a-zA-Z0-9_]{22,}").unwrap(),
                "GitHub PAT",
                DetectionConfidence::High,
            ),
            // Generic — keyword-anchored, weaker signal. A README placeholder
            // like `api_key: your_api_key_here_placeholder` matches this; it is
            // Medium so it warns rather than denying the install on its own.
            (
                Regex::new(r#"api[_-]?key[=:]\s*['"]*[a-zA-Z0-9_-]{20,}"#).unwrap(),
                "Generic API key",
                DetectionConfidence::Medium,
            ),
        ]
    })
}

fn aws_patterns() -> &'static [(Regex, &'static str, DetectionConfidence)] {
    static PATTERNS: OnceLock<Vec<(Regex, &'static str, DetectionConfidence)>> = OnceLock::new();
    PATTERNS.get_or_init(|| {
        vec![
            (
                Regex::new(r"AKIA[A-Z0-9]{16}").unwrap(),
                "AWS Access Key ID",
                DetectionConfidence::High,
            ),
            // Keyword-anchored — Medium (weaker signal, placeholder-prone).
            (
                Regex::new(r#"aws[_-]?secret[_-]?access[_-]?key[=:]\s*['"]*[a-zA-Z0-9/+=]{40}"#)
                    .unwrap(),
                "AWS Secret Access Key",
                DetectionConfidence::Medium,
            ),
        ]
    })
}

fn generic_secret_patterns() -> &'static [(Regex, &'static str)] {
    static PATTERNS: OnceLock<Vec<(Regex, &'static str)>> = OnceLock::new();
    PATTERNS.get_or_init(|| {
        vec![
            (
                Regex::new(r#"(?i)password[=:]\s*['"]*[^\s'"]{8,}"#).unwrap(),
                "Password in config",
            ),
            (
                Regex::new(r#"(?i)secret[=:]\s*['"]*[a-zA-Z0-9_-]{16,}"#).unwrap(),
                "Secret value",
            ),
            (
                Regex::new(r#"(?i)token[=:]\s*['"]*[a-zA-Z0-9_.-]{20,}"#).unwrap(),
                "Token value",
            ),
        ]
    })
}

fn db_url_patterns() -> &'static [(Regex, &'static str, DetectionConfidence)] {
    static PATTERNS: OnceLock<Vec<(Regex, &'static str, DetectionConfidence)>> = OnceLock::new();
    PATTERNS.get_or_init(|| {
        vec![
            // A connection URL with an embedded `user:pass@` is a strong,
            // structured credential shape → High.
            (
                Regex::new(r"postgres(ql)?://[^:]+:[^@]+@[^\s]+").unwrap(),
                "PostgreSQL connection URL",
                DetectionConfidence::High,
            ),
            (
                Regex::new(r"mysql://[^:]+:[^@]+@[^\s]+").unwrap(),
                "MySQL connection URL",
                DetectionConfidence::High,
            ),
            (
                Regex::new(r"mongodb(\+srv)?://[^:]+:[^@]+@[^\s]+").unwrap(),
                "MongoDB connection URL",
                DetectionConfidence::High,
            ),
            (
                Regex::new(r"redis://[^:]+:[^@]+@[^\s]+").unwrap(),
                "Redis connection URL",
                DetectionConfidence::High,
            ),
        ]
    })
}

fn jwt_pattern() -> &'static Regex {
    static PATTERN: OnceLock<Regex> = OnceLock::new();
    // JWT: three base64url-encoded parts separated by dots
    PATTERN.get_or_init(|| {
        Regex::new(r"eyJ[a-zA-Z0-9_-]*\.eyJ[a-zA-Z0-9_-]*\.[a-zA-Z0-9_-]*").unwrap()
    })
}

fn bot_token_pattern() -> &'static Regex {
    static PATTERN: OnceLock<Regex> = OnceLock::new();
    PATTERN.get_or_init(|| Regex::new(r"/bot[0-9]+:[A-Za-z0-9_-]+").unwrap())
}

/// PEM private-key block markers `(begin, end, label)`, shared by `scan`'s
/// redaction and `detect`'s span reporting. Labels are the human names
/// `scan` already reported, kept stable so existing behavior is unchanged.
const PRIVATE_KEY_MARKERS: &[(&str, &str, &str)] = &[
    (
        "-----BEGIN RSA PRIVATE KEY-----",
        "-----END RSA PRIVATE KEY-----",
        "RSA private key",
    ),
    (
        "-----BEGIN EC PRIVATE KEY-----",
        "-----END EC PRIVATE KEY-----",
        "EC private key",
    ),
    (
        "-----BEGIN PRIVATE KEY-----",
        "-----END PRIVATE KEY-----",
        "Private key",
    ),
    (
        "-----BEGIN OPENSSH PRIVATE KEY-----",
        "-----END OPENSSH PRIVATE KEY-----",
        "OpenSSH private key",
    ),
];

/// Credential leak detector for outbound content.
#[derive(Debug, Clone)]
pub struct LeakDetector {
    /// Sensitivity threshold (0.0-1.0, higher = more aggressive detection).
    sensitivity: f64,
}

impl Default for LeakDetector {
    fn default() -> Self {
        Self::new()
    }
}

impl LeakDetector {
    /// Create a new leak detector with default sensitivity.
    pub fn new() -> Self {
        Self { sensitivity: 0.7 }
    }

    /// Create a detector with custom sensitivity.
    pub fn with_sensitivity(sensitivity: f64) -> Self {
        Self {
            sensitivity: sensitivity.clamp(0.0, 1.0),
        }
    }

    /// Scan content for potential credential leaks.
    pub fn scan(&self, content: &str) -> LeakResult {
        let mut patterns = Vec::new();
        let mut redacted = content.to_string();

        // Check each pattern type
        self.check_api_keys(content, &mut patterns, &mut redacted);
        self.check_aws_credentials(content, &mut patterns, &mut redacted);
        self.check_generic_secrets(content, &mut patterns, &mut redacted);
        self.check_private_keys(content, &mut patterns, &mut redacted);
        self.check_jwt_tokens(content, &mut patterns, &mut redacted);
        self.check_database_urls(content, &mut patterns, &mut redacted);
        self.check_bot_token(content, &mut patterns, &mut redacted);
        self.check_high_entropy_tokens(content, &mut patterns, &mut redacted);

        if patterns.is_empty() {
            LeakResult::Clean
        } else {
            LeakResult::Detected { patterns, redacted }
        }
    }

    /// Check for common API key patterns.
    fn check_api_keys(&self, content: &str, patterns: &mut Vec<String>, redacted: &mut String) {
        for (regex, name, _confidence) in api_key_patterns() {
            if regex.is_match(content) {
                patterns.push(String::from(*name));
                *redacted = regex
                    .replace_all(redacted, "[REDACTED_API_KEY]")
                    .to_string();
            }
        }
    }

    /// Check for AWS credentials.
    fn check_aws_credentials(
        &self,
        content: &str,
        patterns: &mut Vec<String>,
        redacted: &mut String,
    ) {
        for (regex, name, _confidence) in aws_patterns() {
            if regex.is_match(content) {
                patterns.push(String::from(*name));
                *redacted = regex
                    .replace_all(redacted, "[REDACTED_AWS_CREDENTIAL]")
                    .to_string();
            }
        }
    }

    /// Check for generic secret patterns.
    fn check_generic_secrets(
        &self,
        content: &str,
        patterns: &mut Vec<String>,
        redacted: &mut String,
    ) {
        for (regex, name) in generic_secret_patterns() {
            if regex.is_match(content) && self.sensitivity > 0.5 {
                patterns.push(String::from(*name));
                *redacted = regex.replace_all(redacted, "[REDACTED_SECRET]").to_string();
            }
        }
    }

    /// Check for private keys.
    fn check_private_keys(&self, content: &str, patterns: &mut Vec<String>, redacted: &mut String) {
        for (begin, end, name) in PRIVATE_KEY_MARKERS {
            // Search for `end` only after `begin`. Two independent `find`s
            // panic on `content[start..end]` when the END marker precedes the
            // BEGIN marker (e.g. "-----END …-----…-----BEGIN …-----"), which is
            // attacker-influenceable outbound content. The typed `detect` path
            // already anchors this way.
            if let Some(start_idx) = content.find(begin)
                && let Some(end_rel) = content[start_idx..].find(end)
            {
                patterns.push((*name).to_string());
                // Redact the entire key block.
                let end_idx = start_idx + end_rel + end.len();
                let key_block = &content[start_idx..end_idx];
                *redacted = redacted.replace(key_block, "[REDACTED_PRIVATE_KEY]");
            }
        }
    }

    /// Check for JWT tokens.
    fn check_jwt_tokens(&self, content: &str, patterns: &mut Vec<String>, redacted: &mut String) {
        let regex = jwt_pattern();
        if regex.is_match(content) {
            patterns.push("JWT token".to_string());
            *redacted = regex.replace_all(redacted, "[REDACTED_JWT]").to_string();
        }
    }

    /// Check for database connection URLs.
    fn check_database_urls(
        &self,
        content: &str,
        patterns: &mut Vec<String>,
        redacted: &mut String,
    ) {
        for (regex, name, _confidence) in db_url_patterns() {
            if regex.is_match(content) {
                patterns.push(String::from(*name));
                *redacted = regex
                    .replace_all(redacted, "[REDACTED_DATABASE_URL]")
                    .to_string();
            }
        }
    }

    /// Check for messaging bot tokens embedded in API URLs.
    ///
    /// Telegram bot tokens appear in request URLs as `/bot<id>:<token>` and
    /// would otherwise reach error logs verbatim. The token half is not
    /// guaranteed high-entropy, so it needs an explicit pattern rather than
    /// relying on the entropy scan.
    fn check_bot_token(&self, content: &str, patterns: &mut Vec<String>, redacted: &mut String) {
        let regex = bot_token_pattern();
        if regex.is_match(content) {
            patterns.push("Bot token".to_string());
            *redacted = regex
                .replace_all(redacted, "/bot[REDACTED_BOT_TOKEN]")
                .to_string();
        }
    }

    /// Check for high-entropy tokens that may be leaked credentials.
    ///
    /// Extracts candidate tokens from content (after stripping URLs to avoid
    /// false-positives on path segments) and flags any that exceed the Shannon
    /// entropy threshold derived from the detector's sensitivity.
    fn check_high_entropy_tokens(
        &self,
        content: &str,
        patterns: &mut Vec<String>,
        redacted: &mut String,
    ) {
        // Entropy threshold scales with sensitivity: at 0.7 this is ~4.37.
        let entropy_threshold = 3.5 + self.sensitivity * 1.25;

        // Strip URLs and media markers before extracting tokens so that path
        // segments are not mistaken for high-entropy credentials.
        // Media markers like [IMAGE:/path/to/file.png] contain filesystem paths
        // that look like high-entropy tokens when `/` is included in the token
        // character set.
        static URL_PATTERN: OnceLock<Regex> = OnceLock::new();
        let url_re = URL_PATTERN.get_or_init(|| Regex::new(r"https?://\S+").unwrap());
        static MEDIA_MARKER_PATTERN: OnceLock<Regex> = OnceLock::new();
        let media_re = MEDIA_MARKER_PATTERN.get_or_init(|| {
            Regex::new(r"\[(IMAGE|VIDEO|VOICE|AUDIO|DOCUMENT|FILE):[^\]]*\]").unwrap()
        });
        // Tool receipts (zc-receipt-...) are runtime-generated HMAC tokens that
        // intentionally appear in output. Strip them before entropy scanning so
        // they are not redacted as leaked credentials.
        static RECEIPT_PATTERN: OnceLock<Regex> = OnceLock::new();
        let receipt_re =
            RECEIPT_PATTERN.get_or_init(|| Regex::new(r"zc-receipt-\d+-[A-Za-z0-9_-]+").unwrap());
        let content_stripped = url_re.replace_all(content, "");
        let content_without_urls = media_re.replace_all(&content_stripped, "");
        let content_without_receipts = receipt_re.replace_all(&content_without_urls, "");

        let tokens = extract_candidate_tokens(&content_without_receipts);

        for token in tokens {
            if token.len() >= ENTROPY_TOKEN_MIN_LEN {
                let entropy = shannon_entropy(token);
                if entropy >= entropy_threshold && has_mixed_alpha_digit(token) {
                    patterns.push("High-entropy token".to_string());
                    *redacted = redacted.replace(token, "[REDACTED_HIGH_ENTROPY_TOKEN]");
                }
            }
        }
    }

    /// Typed credential detection for the install-screening layer (1B).
    ///
    /// Returns one [`DetectionMatch`] per credential hit with its byte span and
    /// a redacted excerpt. Confidence encodes match quality so the screening
    /// layer can gate disposition (structured credential shapes are `High` and
    /// warrant denial; keyword-anchored generic secrets are `Medium`;
    /// entropy-heuristic tokens are `Low`/`Medium`). Shares the same compiled
    /// pattern sets as [`scan`](Self::scan) [I7].
    ///
    /// Redacted excerpts never contain the raw credential: structured and
    /// entropy matches are replaced by a `[REDACTED …]` marker; only the small,
    /// non-secret keyword label (e.g. `password=…`) is shown for generic
    /// secrets, with the value elided [I10].
    pub fn detect(&self, content: &str) -> Vec<DetectionMatch> {
        let mut matches = Vec::new();

        // Structured credentials. Key-shaped patterns are High and warrant
        // denial; keyword-anchored generic entries carry Medium so a README
        // placeholder does not block an install on its own.
        for group in [api_key_patterns(), aws_patterns(), db_url_patterns()] {
            for (regex, label, confidence) in group {
                for m in regex.find_iter(content) {
                    matches.push(DetectionMatch {
                        label,
                        confidence: *confidence,
                        span: m.start()..m.end(),
                        redacted_excerpt: format!("[REDACTED {label}]"),
                    });
                }
            }
        }
        for m in jwt_pattern().find_iter(content) {
            matches.push(DetectionMatch {
                label: "JWT token",
                confidence: DetectionConfidence::High,
                span: m.start()..m.end(),
                redacted_excerpt: "[REDACTED JWT]".to_string(),
            });
        }
        for m in bot_token_pattern().find_iter(content) {
            matches.push(DetectionMatch {
                label: "Bot token",
                confidence: DetectionConfidence::High,
                span: m.start()..m.end(),
                redacted_excerpt: "[REDACTED bot token]".to_string(),
            });
        }

        // PEM private-key blocks → High confidence.
        for (begin, end, label) in PRIVATE_KEY_MARKERS {
            if let Some(start_idx) = content.find(begin)
                && let Some(end_rel) = content[start_idx..].find(end)
            {
                let end_idx = start_idx + end_rel + end.len();
                matches.push(DetectionMatch {
                    label,
                    confidence: DetectionConfidence::High,
                    span: start_idx..end_idx,
                    redacted_excerpt: "[REDACTED private key]".to_string(),
                });
            }
        }

        // Keyword-anchored generic secrets → Medium confidence. The value is
        // elided; only the sanitized keyword prefix is shown.
        for (regex, label) in generic_secret_patterns() {
            for m in regex.find_iter(content) {
                let keyword = m.as_str().split(['=', ':']).next().unwrap_or("");
                matches.push(DetectionMatch {
                    label,
                    confidence: DetectionConfidence::Medium,
                    span: m.start()..m.end(),
                    redacted_excerpt: format!("{}=[REDACTED]", sanitize_excerpt(keyword)),
                });
            }
        }

        // High-entropy tokens → Low/Medium confidence (heuristic). Reuse the
        // same URL/media/receipt stripping as `scan` so path segments are not
        // mistaken for credentials.
        let entropy_threshold = 3.5 + self.sensitivity * 1.25;
        static URL_PATTERN: OnceLock<Regex> = OnceLock::new();
        let url_re = URL_PATTERN.get_or_init(|| Regex::new(r"https?://\S+").unwrap());
        static MEDIA_MARKER_PATTERN: OnceLock<Regex> = OnceLock::new();
        let media_re = MEDIA_MARKER_PATTERN.get_or_init(|| {
            Regex::new(r"\[(IMAGE|VIDEO|VOICE|AUDIO|DOCUMENT|FILE):[^\]]*\]").unwrap()
        });
        static RECEIPT_PATTERN: OnceLock<Regex> = OnceLock::new();
        let receipt_re =
            RECEIPT_PATTERN.get_or_init(|| Regex::new(r"zc-receipt-\d+-[A-Za-z0-9_-]+").unwrap());
        // Build a masked copy the same length as `content` so byte spans line
        // up: replace stripped regions with spaces rather than deleting them.
        let mut masked = content.to_string();
        for re in [url_re, media_re, receipt_re] {
            masked = re
                .replace_all(&masked, |caps: &regex::Captures| " ".repeat(caps[0].len()))
                .into_owned();
        }
        for m in token_spans(&masked) {
            let token = &content[m.clone()];
            if token.len() >= ENTROPY_TOKEN_MIN_LEN
                && shannon_entropy(token) >= entropy_threshold
                && has_mixed_alpha_digit(token)
            {
                matches.push(DetectionMatch {
                    label: "High-entropy token",
                    // Entropy is a heuristic: Medium at/above threshold+1 bit,
                    // Low otherwise. Either stays sub-Denial in screening.
                    confidence: if shannon_entropy(token) >= entropy_threshold + 1.0 {
                        DetectionConfidence::Medium
                    } else {
                        DetectionConfidence::Low
                    },
                    span: m,
                    redacted_excerpt: "[REDACTED high-entropy token]".to_string(),
                });
            }
        }

        matches
    }
}

/// True when `c` is part of a candidate credential token (alphanumeric plus
/// the common credential punctuation). Shared by the token extractor and the
/// span variant so both split identically.
fn is_token_char(c: char) -> bool {
    c.is_ascii_alphanumeric() || c == '_' || c == '-' || c == '+' || c == '/'
}

/// Byte spans of candidate tokens in `content` (the range variant of
/// [`extract_candidate_tokens`], used by the typed `detect` API).
fn token_spans(content: &str) -> Vec<std::ops::Range<usize>> {
    let mut spans = Vec::new();
    let mut start: Option<usize> = None;
    for (idx, c) in content.char_indices() {
        if is_token_char(c) {
            start.get_or_insert(idx);
        } else if let Some(s) = start.take() {
            spans.push(s..idx);
        }
    }
    if let Some(s) = start {
        spans.push(s..content.len());
    }
    spans
}

/// Extract candidate tokens by splitting on characters outside the
/// alphanumeric + common credential character set.
fn extract_candidate_tokens(content: &str) -> Vec<&str> {
    content
        .split(|c: char| !is_token_char(c))
        .filter(|s| !s.is_empty())
        .collect()
}

/// Compute Shannon entropy (bits per character) for the given string.
fn shannon_entropy(s: &str) -> f64 {
    let len = s.len() as f64;
    if len == 0.0 {
        return 0.0;
    }
    let mut freq: HashMap<u8, usize> = HashMap::new();
    for &b in s.as_bytes() {
        *freq.entry(b).or_insert(0) += 1;
    }
    freq.values().fold(0.0, |acc, &count| {
        let p = count as f64 / len;
        acc - p * p.log2()
    })
}

/// Check whether a token contains both alphabetic and digit characters.
fn has_mixed_alpha_digit(s: &str) -> bool {
    let has_alpha = s.bytes().any(|b| b.is_ascii_alphabetic());
    let has_digit = s.bytes().any(|b| b.is_ascii_digit());
    has_alpha && has_digit
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn clean_content_passes() {
        let detector = LeakDetector::new();
        let result = detector.scan("This is just some normal text");
        assert!(matches!(result, LeakResult::Clean));
    }

    #[test]
    fn detects_stripe_keys() {
        let detector = LeakDetector::new();
        let content = "My Stripe key is sk_test_1234567890abcdefghijklmnop";
        let result = detector.scan(content);
        match result {
            LeakResult::Detected { patterns, redacted } => {
                assert!(patterns.iter().any(|p| p.contains("Stripe")));
                assert!(redacted.contains("[REDACTED"));
            }
            LeakResult::Clean => panic!("Should detect Stripe key"),
        }
    }

    #[test]
    fn detects_aws_credentials() {
        let detector = LeakDetector::new();
        let content = "AWS key: AKIAIOSFODNN7EXAMPLE";
        let result = detector.scan(content);
        match result {
            LeakResult::Detected { patterns, .. } => {
                assert!(patterns.iter().any(|p| p.contains("AWS")));
            }
            LeakResult::Clean => panic!("Should detect AWS key"),
        }
    }

    #[test]
    fn scan_does_not_panic_on_reversed_private_key_markers() {
        // An END marker appearing before the BEGIN marker used to slice
        // content[start..end] with start > end and panic. scan() runs on
        // attacker/model-influenced outbound content, so it must not crash.
        let detector = LeakDetector::new();
        let content = "-----END PRIVATE KEY-----junk-----BEGIN PRIVATE KEY-----";
        // Must return, not panic. A well-ordered block is absent, so the
        // reversed markers are not treated as a redactable key block.
        let _ = detector.scan(content);
    }

    #[test]
    fn detects_groq_api_keys() {
        let detector = LeakDetector::new();
        let content = "Groq key: gsk_abcdefghijklmnopqrstuvwxyz123456";
        let result = detector.scan(content);
        match result {
            LeakResult::Detected { patterns, redacted } => {
                assert!(patterns.iter().any(|p| p.contains("Groq")));
                assert!(redacted.contains("[REDACTED"));
                assert!(!redacted.contains("gsk_abcdefghijklmnopqrstuvwxyz123456"));
            }
            LeakResult::Clean => panic!("Should detect Groq API key"),
        }
    }

    #[test]
    fn detects_private_keys() {
        let detector = LeakDetector::new();
        let content = r#"
-----BEGIN RSA PRIVATE KEY-----
MIIEowIBAAKCAQEA0ZPr5JeyVDonXsKhfq...
-----END RSA PRIVATE KEY-----
"#;
        let result = detector.scan(content);
        match result {
            LeakResult::Detected { patterns, redacted } => {
                assert!(patterns.iter().any(|p| p.contains("private key")));
                assert!(redacted.contains("[REDACTED_PRIVATE_KEY]"));
            }
            LeakResult::Clean => panic!("Should detect private key"),
        }
    }

    #[test]
    fn detects_jwt_tokens() {
        let detector = LeakDetector::new();
        let content = "Bearer eyJhbGciOiJIUzI1NiIsInR5cCI6IkpXVCJ9.eyJzdWIiOiIxMjM0NTY3ODkwIn0.dozjgNryP4J3jVmNHl0w5N_XgL0n3I9PlFUP0THsR8U";
        let result = detector.scan(content);
        match result {
            LeakResult::Detected { patterns, redacted } => {
                assert!(patterns.iter().any(|p| p.contains("JWT")));
                assert!(redacted.contains("[REDACTED_JWT]"));
            }
            LeakResult::Clean => panic!("Should detect JWT"),
        }
    }

    #[test]
    fn detects_database_urls() {
        let detector = LeakDetector::new();
        let content = "DATABASE_URL=postgres://user:secretpassword@localhost:5432/mydb";
        let result = detector.scan(content);
        match result {
            LeakResult::Detected { patterns, .. } => {
                assert!(patterns.iter().any(|p| p.contains("PostgreSQL")));
            }
            LeakResult::Clean => panic!("Should detect database URL"),
        }
    }

    #[test]
    fn low_sensitivity_skips_generic() {
        let detector = LeakDetector::with_sensitivity(0.3);
        let content = "secret=mygenericvalue123456";
        let result = detector.scan(content);
        // Low sensitivity should not flag generic secrets
        assert!(matches!(result, LeakResult::Clean));
    }

    #[test]
    fn url_path_segments_not_flagged() {
        let detector = LeakDetector::new();
        // URL with a long mixed-alphanumeric path segment that would previously
        // false-positive as a high-entropy token.
        let content =
            "See https://example.org/documents/2024-report-a1b2c3d4e5f6g7h8i9j0.pdf for details";
        let result = detector.scan(content);
        assert!(
            matches!(result, LeakResult::Clean),
            "URL path segments should not trigger high-entropy detection"
        );
    }

    #[test]
    fn url_with_long_path_not_redacted() {
        let detector = LeakDetector::new();
        let content = "Reference: https://gov.example.com/publications/research/2024-annual-fiscal-policy-review-9a8b7c6d5e4f3g2h1i0j.html";
        let result = detector.scan(content);
        assert!(
            matches!(result, LeakResult::Clean),
            "Long URL paths should not be redacted"
        );
    }

    #[test]
    fn tool_receipts_not_redacted_as_high_entropy() {
        let detector = LeakDetector::new();
        let content = "The date is Fri Mar 27.\n\n[receipt: zc-receipt-1774608496-gzpEBuUIRYX1vd4fQl4oYkqhq4-GnoJDStmlYzvQiWA]";
        let result = detector.scan(content);
        assert!(
            matches!(result, LeakResult::Clean),
            "Tool receipts (zc-receipt-...) should not be redacted"
        );
    }

    #[test]
    fn media_markers_not_redacted_as_high_entropy() {
        let detector = LeakDetector::new();
        let content = "Here is the image: [IMAGE:/Users/matt/.zeroclaw/workspace/skills/image-gen/images/20260324_135911.png]";
        let result = detector.scan(content);
        assert!(
            matches!(result, LeakResult::Clean),
            "Local media markers should not be redacted"
        );
    }

    #[test]
    fn detects_high_entropy_token_outside_url() {
        let detector = LeakDetector::new();
        // A standalone high-entropy token (not in a URL) should still be detected.
        let content = "Found credential: aB3xK9mW2pQ7vL4nR8sT1yU6hD0jF5cG";
        let result = detector.scan(content);
        match result {
            LeakResult::Detected { patterns, redacted } => {
                assert!(patterns.iter().any(|p| p.contains("High-entropy")));
                assert!(redacted.contains("[REDACTED_HIGH_ENTROPY_TOKEN]"));
            }
            LeakResult::Clean => panic!("Should detect high-entropy token"),
        }
    }

    #[test]
    fn low_sensitivity_raises_entropy_threshold() {
        let detector = LeakDetector::with_sensitivity(0.3);
        // At low sensitivity the entropy threshold is higher (3.5 + 0.3*1.25 = 3.875).
        // A repetitive mixed token has low entropy and should not be flagged.
        let content = "token found: ab12ab12ab12ab12ab12ab12ab12ab12";
        let result = detector.scan(content);
        assert!(
            matches!(result, LeakResult::Clean),
            "Low-entropy repetitive tokens should not be flagged"
        );
    }

    #[test]
    fn extract_candidate_tokens_splits_correctly() {
        let tokens = extract_candidate_tokens("foo.bar:baz qux-quux key=val");
        assert!(tokens.contains(&"foo"));
        assert!(tokens.contains(&"bar"));
        assert!(tokens.contains(&"baz"));
        assert!(tokens.contains(&"qux-quux"));
        // '=' is a delimiter, not part of tokens
        assert!(tokens.contains(&"key"));
        assert!(tokens.contains(&"val"));
    }

    #[test]
    fn media_marker_image_path_not_redacted() {
        let detector = LeakDetector::new();
        let content = "Here is your image: [IMAGE:/Users/matt/.zeroclaw/workspace/skills/image-gen/images/20260324_135911.png]";
        let result = detector.scan(content);
        assert!(
            matches!(result, LeakResult::Clean),
            "Media marker image paths should not trigger high-entropy detection"
        );
    }

    #[test]
    fn media_marker_video_not_redacted() {
        let detector = LeakDetector::new();
        let content = "Attached: [VIDEO:/path/to/long/video/file/name123456.mp4]";
        let result = detector.scan(content);
        assert!(
            matches!(result, LeakResult::Clean),
            "Media marker video paths should not trigger high-entropy detection"
        );
    }

    #[test]
    fn actual_high_entropy_still_detected() {
        let detector = LeakDetector::new();
        let content = "Leaked credential: aB3xK9mW2pQ7vL4nR8sT1yU6hD0jF5cG";
        let result = detector.scan(content);
        match result {
            LeakResult::Detected { patterns, redacted } => {
                assert!(patterns.iter().any(|p| p.contains("High-entropy")));
                assert!(redacted.contains("[REDACTED_HIGH_ENTROPY_TOKEN]"));
            }
            LeakResult::Clean => {
                panic!("Should still detect high-entropy tokens outside media markers")
            }
        }
    }

    #[test]
    fn shannon_entropy_empty_string() {
        assert_eq!(shannon_entropy(""), 0.0);
    }

    #[test]
    fn shannon_entropy_single_char() {
        // All same characters: entropy = 0
        assert_eq!(shannon_entropy("aaaa"), 0.0);
    }

    #[test]
    fn shannon_entropy_two_equal_chars() {
        // "ab" repeated: entropy = 1.0 bit
        let e = shannon_entropy("abab");
        assert!((e - 1.0).abs() < 0.001);
    }

    #[test]
    fn detects_telegram_bot_token() {
        let detector = LeakDetector::new();
        let content = "error sending request for url (https://api.telegram.org/bot123456:ABC-def_GHI/getUpdates)";
        match detector.scan(content) {
            LeakResult::Detected { patterns, redacted } => {
                assert!(patterns.iter().any(|p| p.contains("Bot token")));
                assert!(redacted.contains("[REDACTED_BOT_TOKEN]"));
                assert!(!redacted.contains("123456:ABC-def_GHI"));
            }
            LeakResult::Clean => panic!("Should detect Telegram bot token"),
        }
    }

    #[test]
    fn bot_token_leaves_unrelated_text_clean() {
        let detector = LeakDetector::new();
        assert!(matches!(
            detector.scan("connection reset by peer"),
            LeakResult::Clean
        ));
    }

    // ─── Task 1A: typed detect API ───────────────────────────────────────────

    #[test]
    fn detect_structured_key_is_high_confidence_with_correct_span() {
        let detector = LeakDetector::new();
        let content = "config: AKIAIOSFODNN7EXAMPLE trailing";
        let matches = detector.detect(content);
        let aws = matches
            .iter()
            .find(|m| m.label.contains("AWS"))
            .expect("AWS access key id must be detected");
        assert_eq!(aws.confidence, DetectionConfidence::High);
        // The span must cover exactly the credential token.
        assert_eq!(&content[aws.span.clone()], "AKIAIOSFODNN7EXAMPLE");
        // The excerpt must never contain the raw credential.
        assert!(!aws.redacted_excerpt.contains("AKIA"));
    }

    #[test]
    fn detect_keyword_anchored_generics_are_medium_not_high() {
        // Regression: keyword-anchored patterns (`api_key: …`, `aws_secret…`)
        // are weaker signals prone to placeholder false positives; they must be
        // Medium so screening does not raise them to a Denial on their own.
        let detector = LeakDetector::new();

        let api = detector.detect("api_key: your_api_key_here_placeholder123");
        let generic = api
            .iter()
            .find(|m| m.label == "Generic API key")
            .expect("generic api key must be detected");
        assert_eq!(generic.confidence, DetectionConfidence::Medium);

        let aws = detector.detect(
            "aws_secret_access_key=abcdefghijklmnopqrstuvwxyz0123456789ABCD",
        );
        let secret = aws
            .iter()
            .find(|m| m.label == "AWS Secret Access Key")
            .expect("aws secret must be detected");
        assert_eq!(secret.confidence, DetectionConfidence::Medium);

        // A structured, key-shaped credential in the same group stays High.
        let structured = detector.detect("AKIAIOSFODNN7EXAMPLE");
        assert!(
            structured
                .iter()
                .any(|m| m.confidence == DetectionConfidence::High),
            "structured AWS access key id must remain High"
        );
    }

    #[test]
    fn detect_private_key_span_covers_the_block() {
        let detector = LeakDetector::new();
        let content = "before\n-----BEGIN RSA PRIVATE KEY-----\nMIIE...\n-----END RSA PRIVATE KEY-----\nafter";
        let matches = detector.detect(content);
        let key = matches
            .iter()
            .find(|m| m.label.contains("private key"))
            .expect("private key must be detected");
        assert_eq!(key.confidence, DetectionConfidence::High);
        let block = &content[key.span.clone()];
        assert!(block.starts_with("-----BEGIN RSA PRIVATE KEY-----"));
        assert!(block.ends_with("-----END RSA PRIVATE KEY-----"));
    }

    #[test]
    fn detect_generic_secret_is_medium_and_elides_value() {
        let detector = LeakDetector::new();
        let matches = detector.detect("password: hunter2secret");
        let secret = matches
            .iter()
            .find(|m| m.label.contains("Password"))
            .expect("generic password must be detected");
        assert_eq!(secret.confidence, DetectionConfidence::Medium);
        assert!(!secret.redacted_excerpt.contains("hunter2secret"));
    }

    #[test]
    fn detect_entropy_token_is_sub_high_confidence() {
        let detector = LeakDetector::new();
        let content = "credential: aB3xK9mW2pQ7vL4nR8sT1yU6hD0jF5cG";
        let matches = detector.detect(content);
        let entropy = matches
            .iter()
            .find(|m| m.label == "High-entropy token")
            .expect("high-entropy token must be detected");
        assert!(
            entropy.confidence != DetectionConfidence::High,
            "entropy heuristic must stay sub-High so it never forces a denial"
        );
        assert_eq!(
            &content[entropy.span.clone()],
            "aB3xK9mW2pQ7vL4nR8sT1yU6hD0jF5cG"
        );
    }

    #[test]
    fn detect_ignores_url_path_segments() {
        let detector = LeakDetector::new();
        // Same case scan() treats as Clean — the typed API must agree.
        let content =
            "See https://example.org/documents/2024-report-a1b2c3d4e5f6g7h8i9j0.pdf for details";
        assert!(
            detector.detect(content).is_empty(),
            "URL path segments must not be flagged: {:?}",
            detector.detect(content)
        );
    }

    #[test]
    fn detect_clean_text_is_empty() {
        let detector = LeakDetector::new();
        assert!(
            detector
                .detect("A skill that formats JSON nicely.")
                .is_empty()
        );
    }

    #[test]
    fn token_spans_index_the_right_substrings() {
        let content = "foo.bar:baz-qux";
        for span in token_spans(content) {
            let tok = &content[span.clone()];
            assert!(tok.chars().all(is_token_char), "bad token {tok:?}");
        }
        let toks: Vec<&str> = token_spans(content)
            .into_iter()
            .map(|s| &content[s])
            .collect();
        assert_eq!(toks, vec!["foo", "bar", "baz-qux"]);
    }
}
