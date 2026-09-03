//! Credential leak detection for outbound content.

use regex::Regex;
use std::ops::Range;
use std::sync::OnceLock;
use zeroclaw_config::schema::LeakDetectionConfig;

/// Minimum token length considered for high-entropy detection.
const ENTROPY_TOKEN_MIN_LEN: usize = 24;

/// Keys that announce a credential value, paired with the value length their
/// detector needs before it matches. Word parts join with an optional `_` or
/// `-`, mirroring the `api[_-]?key` and `aws[_-]?secret[_-]?access[_-]?key`
/// forms. Longest first, so a key that contains a shorter one is measured
/// against its own threshold.
///
/// The regex sets in `check_api_keys`, `check_aws_credentials` and
/// `check_generic_secrets` remain the source of truth for what is a credential.
/// This table answers a different question those regexes cannot: whether a
/// match is *still possible* in text that has not finished arriving.
/// `withhold_thresholds_match_the_detector_patterns` fails if the two drift.
const CREDENTIAL_KEY_THRESHOLDS: &[(&[&str], usize)] = &[
    (&["aws", "secret", "access", "key"], 40),
    (&["api", "key"], 20),
    (&["token"], 20),
    (&["secret"], 16),
    (&["password"], 8),
];

/// How far back from the end a partial credential can begin: the longest key,
/// its separator and quotes, and the largest threshold, with headroom.
const MAX_INCOMPLETE_CREDENTIAL_LEN: usize = 128;

/// Whether the tail of `content` could still become a credential once more of
/// the stream arrives, and if so the byte offset where it begins.
///
/// A streaming surface publishes text before the response is complete, and a
/// detector can only recognise a credential once enough of the value is
/// present. The text published before that point is not redacted, and a
/// rendered frame cannot be retracted — a later edit replaces what is on
/// screen, but a reader has already seen it. A caller that withholds from this
/// offset publishes only text no later delta can turn into a credential, which
/// also keeps successive frames monotonic: the withheld region is republished
/// as the detector's replacement, which extends what was already shown rather
/// than contradicting it.
///
/// Returns `None` when nothing is pending, including when a value has already
/// reached its threshold — the detector redacts that itself, and withholding it
/// would stall the surface for the rest of the turn.
///
/// Scope: keys announce these credentials, so a value with no key before it —
/// a bare high-entropy token, a JWT — is not covered. Withholding every long
/// unbroken run of characters would hold back ordinary text such as a URL or a
/// hash, and those detectors are heuristic rather than keyed.
pub fn incomplete_credential_tail(content: &str) -> Option<usize> {
    let window_start = content.len().saturating_sub(MAX_INCOMPLETE_CREDENTIAL_LEN);
    let bytes = content.as_bytes();
    for start in window_start..content.len() {
        if !content.is_char_boundary(start) {
            continue;
        }
        for (parts, threshold) in CREDENTIAL_KEY_THRESHOLDS {
            if credential_could_begin_at(bytes, start, parts, *threshold) {
                return Some(start);
            }
        }
    }
    None
}

/// Whether a credential written with this key could begin at `start`, counting
/// a key that is itself still arriving.
///
/// The key matters as much as the value. Deltas split wherever the provider
/// happens to break, so `token` can arrive as `to` and then `ken=`. Waiting for
/// the whole key before withholding would publish `to`, then retract it one
/// frame later — a contradiction, and on Teams a rejected frame. Running out of
/// input mid-key is therefore pending, like running out mid-value.
///
/// The cost is a few trailing characters held whenever text ends on a prefix of
/// one of these keys, which is bounded by the longest of them and resolves on
/// the next delta.
fn credential_could_begin_at(bytes: &[u8], start: usize, parts: &[&str], threshold: usize) -> bool {
    let mut pos = start;
    for (index, part) in parts.iter().enumerate() {
        // The separator between word parts is optional, so a byte that is not
        // one simply belongs to the next part.
        if index > 0 {
            match bytes.get(pos) {
                None => return true,
                Some(b'_' | b'-') => pos += 1,
                Some(_) => {}
            }
        }
        for (offset, expected) in part.as_bytes().iter().enumerate() {
            match bytes.get(pos + offset) {
                None => return true,
                Some(actual) if actual.eq_ignore_ascii_case(expected) => {}
                Some(_) => return false,
            }
        }
        pos += part.len();
    }
    value_can_still_complete(bytes, pos, threshold)
}

/// Whether `[=:]` `\s*` `['"]*` and a value of `threshold` characters could
/// still be completed by text that has not arrived, starting at `pos`.
///
/// Running out of input while the shape is still intact is the pending case.
/// A character that cannot appear where it does ends it, and so does a value
/// that has already reached the threshold.
fn value_can_still_complete(bytes: &[u8], pos: usize, threshold: usize) -> bool {
    let mut pos = pos;
    match bytes.get(pos) {
        None => return true,
        Some(b'=' | b':') => pos += 1,
        Some(_) => return false,
    }
    while matches!(bytes.get(pos), Some(byte) if byte.is_ascii_whitespace()) {
        pos += 1;
    }
    while matches!(bytes.get(pos), Some(b'"' | b'\'')) {
        pos += 1;
    }
    // Deliberately wider than any single pattern's value class, since a
    // narrower one would stop counting early and call a pending value
    // complete. Counting past a character the real pattern rejects only
    // publishes text that pattern would never have redacted.
    let mut value_len = 0;
    while let Some(byte) = bytes.get(pos) {
        if byte.is_ascii_whitespace() || matches!(byte, b'"' | b'\'') {
            return false;
        }
        value_len += 1;
        if value_len >= threshold {
            return false;
        }
        pos += 1;
    }
    true
}

#[derive(Debug, Clone)]
struct CandidateToken<'a> {
    value: &'a str,
    span: Range<usize>,
}

#[derive(Debug, Clone)]
struct Redaction {
    span: Range<usize>,
    replacement: &'static str,
}

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

/// Credential leak detector for outbound content.
#[derive(Debug, Clone)]
pub struct LeakDetector {
    /// Enable all outbound credential detection.
    enabled: bool,
    /// Sensitivity threshold (0.0-1.0, higher = more aggressive detection).
    sensitivity: f64,
    /// Enable heuristic redaction of standalone high-entropy token candidates.
    high_entropy_tokens: bool,
}

impl Default for LeakDetector {
    fn default() -> Self {
        Self::new()
    }
}

impl LeakDetector {
    /// Create a new leak detector with default sensitivity.
    pub fn new() -> Self {
        Self::with_config(&LeakDetectionConfig::default())
    }

    /// Create a detector with custom sensitivity.
    pub fn with_sensitivity(sensitivity: f64) -> Self {
        Self {
            sensitivity: sensitivity.clamp(0.0, 1.0),
            ..Self::new()
        }
    }

    /// Create a detector from the user-facing config source of truth.
    pub fn with_config(config: &LeakDetectionConfig) -> Self {
        Self {
            enabled: config.enabled,
            sensitivity: config.sensitivity.clamp(0.0, 1.0),
            high_entropy_tokens: config.high_entropy_tokens,
        }
    }

    /// Scan content for potential credential leaks.
    pub fn scan(&self, content: &str) -> LeakResult {
        self.scan_with_protected_spans(content, &[])
    }

    /// Scan content while applying caller-supplied byte ranges to heuristics.
    ///
    /// Protected spans mark ranges that the high-entropy heuristic must not
    /// rewrite. Deterministic credential detectors still scan the full content
    /// and may redact precise credential patterns inside protected ranges. This
    /// keeps format-specific paths, URLs, and references from tripping entropy
    /// detection without letting real secrets hide in functional destinations.
    pub fn scan_with_protected_spans(
        &self,
        content: &str,
        protected_spans: &[Range<usize>],
    ) -> LeakResult {
        if !self.enabled {
            return LeakResult::Clean;
        }

        let mut patterns = Vec::new();
        let protected_spans = merge_spans(
            protected_spans
                .iter()
                .filter(|span| {
                    span.start < span.end
                        && span.end <= content.len()
                        && content.is_char_boundary(span.start)
                        && content.is_char_boundary(span.end)
                })
                .cloned()
                .collect(),
        );
        let mut redactions = Vec::new();

        // Deterministic credential patterns always scan the full, unprotected
        // content. They match precise, low-false-positive shapes (AWS key
        // format, PEM markers, JWT triple-base64, DB URL schemes, bot-token
        // syntax) that ordinary generated file paths do not produce, so the
        // shape-based false-positive problem does not apply to them. A real
        // credential can be placed inside a link destination or file reference
        // exactly as easily as in visible text, and visible text must still
        // be scanned for real secrets -- the same must hold for non-visible
        // functional parts. Only the high-entropy heuristic, which misfires on
        // the *shape* of a path rather than on an actual secret token, honors
        // caller-supplied protected spans.
        let no_protected_spans: &[Range<usize>] = &[];
        self.check_api_keys(content, no_protected_spans, &mut patterns, &mut redactions);
        self.check_aws_credentials(content, no_protected_spans, &mut patterns, &mut redactions);
        self.check_generic_secrets(content, no_protected_spans, &mut patterns, &mut redactions);
        self.check_private_keys(content, no_protected_spans, &mut patterns, &mut redactions);
        self.check_jwt_tokens(content, no_protected_spans, &mut patterns, &mut redactions);
        self.check_database_urls(content, no_protected_spans, &mut patterns, &mut redactions);
        self.check_bot_token(content, no_protected_spans, &mut patterns, &mut redactions);
        if self.high_entropy_tokens {
            self.check_high_entropy_tokens(
                content,
                &protected_spans,
                &mut patterns,
                &mut redactions,
            );
        }

        if patterns.is_empty() {
            LeakResult::Clean
        } else {
            let redacted = apply_redactions(content, &redactions);
            LeakResult::Detected { patterns, redacted }
        }
    }

    /// Check for common API key patterns.
    fn check_api_keys(
        &self,
        content: &str,
        protected_spans: &[Range<usize>],
        patterns: &mut Vec<String>,
        redactions: &mut Vec<Redaction>,
    ) {
        static API_KEY_PATTERNS: OnceLock<Vec<(Regex, &'static str)>> = OnceLock::new();
        let regexes = API_KEY_PATTERNS.get_or_init(|| {
            vec![
                // Stripe
                (
                    Regex::new(r"sk_(live|test)_[a-zA-Z0-9]{24,}").unwrap(),
                    "Stripe secret key",
                ),
                (
                    Regex::new(r"pk_(live|test)_[a-zA-Z0-9]{24,}").unwrap(),
                    "Stripe publishable key",
                ),
                // OpenAI
                (
                    Regex::new(r"sk-[a-zA-Z0-9]{20,}T3BlbkFJ[a-zA-Z0-9]{20,}").unwrap(),
                    "OpenAI API key",
                ),
                (
                    Regex::new(r"sk-[a-zA-Z0-9]{48,}").unwrap(),
                    "OpenAI-style API key",
                ),
                // Anthropic
                (
                    Regex::new(r"sk-ant-[a-zA-Z0-9-_]{32,}").unwrap(),
                    "Anthropic API key",
                ),
                // Groq
                (Regex::new(r"gsk_[a-zA-Z0-9]{20,}").unwrap(), "Groq API key"),
                // Google
                (
                    Regex::new(r"AIza[a-zA-Z0-9_-]{35}").unwrap(),
                    "Google API key",
                ),
                // GitHub
                (
                    Regex::new(r"gh[pousr]_[a-zA-Z0-9]{36,}").unwrap(),
                    "GitHub token",
                ),
                (
                    Regex::new(r"github_pat_[a-zA-Z0-9_]{22,}").unwrap(),
                    "GitHub PAT",
                ),
                // Slack
                (
                    Regex::new(r"xox[baprs]-[0-9A-Za-z-]{10,}")
                        .expect("static Slack token regex must compile"),
                    "Slack token",
                ),
                (
                    Regex::new(r"xapp-[0-9A-Za-z-]{10,}")
                        .expect("static Slack app-level token regex must compile"),
                    "Slack app-level token",
                ),
                (
                    Regex::new(r"xwfp-[0-9A-Za-z-]{10,}")
                        .expect("static Slack workflow token regex must compile"),
                    "Slack workflow token",
                ),
                (
                    // Rotation family: refresh tokens (`xoxe-…`) and rotated
                    // access tokens (`xoxe.xoxb-…`, `xoxe.xoxp-…`). The base
                    // `xox[baprs]-` class excludes `e`, and matching only the
                    // inner `xoxb-`/`xoxp-` would leave the `xoxe.` prefix
                    // unredacted, so cover the whole token explicitly.
                    Regex::new(r"xoxe(?:-[0-9A-Za-z-]{10,}|\.xox[bp]-[0-9A-Za-z-]{10,})")
                        .expect("static Slack rotation token regex must compile"),
                    "Slack refresh/rotated token",
                ),
                // Generic. Case-insensitive on the key, as the `password`,
                // `secret` and `token` patterns are: `API_KEY=` is the
                // conventional spelling in an environment file, and the
                // streaming withholding in the channel layer matches keys
                // without regard to case, so a case-sensitive pattern here
                // would hold a value back and then publish it unredacted.
                (
                    Regex::new(r#"(?i)api[_-]?key[=:]\s*['"]*[a-zA-Z0-9_-]{20,}"#).unwrap(),
                    "Generic API key",
                ),
            ]
        });

        for (regex, name) in regexes {
            collect_regex_redactions(
                content,
                regex,
                protected_spans,
                name,
                "[REDACTED_API_KEY]",
                patterns,
                redactions,
            );
        }
    }

    /// Check for AWS credentials.
    fn check_aws_credentials(
        &self,
        content: &str,
        protected_spans: &[Range<usize>],
        patterns: &mut Vec<String>,
        redactions: &mut Vec<Redaction>,
    ) {
        static AWS_PATTERNS: OnceLock<Vec<(Regex, &'static str)>> = OnceLock::new();
        let regexes = AWS_PATTERNS.get_or_init(|| {
            vec![
                (
                    Regex::new(r"AKIA[A-Z0-9]{16}").unwrap(),
                    "AWS Access Key ID",
                ),
                (
                    // Case-insensitive on the key for the same reason as the
                    // generic API key above; `AWS_SECRET_ACCESS_KEY` is the
                    // spelling the AWS SDKs read from the environment.
                    Regex::new(
                        r#"(?i)aws[_-]?secret[_-]?access[_-]?key[=:]\s*['"]*[a-zA-Z0-9/+=]{40}"#,
                    )
                    .unwrap(),
                    "AWS Secret Access Key",
                ),
            ]
        });

        for (regex, name) in regexes {
            collect_regex_redactions(
                content,
                regex,
                protected_spans,
                name,
                "[REDACTED_AWS_CREDENTIAL]",
                patterns,
                redactions,
            );
        }
    }

    /// Check for generic secret patterns.
    fn check_generic_secrets(
        &self,
        content: &str,
        protected_spans: &[Range<usize>],
        patterns: &mut Vec<String>,
        redactions: &mut Vec<Redaction>,
    ) {
        static SECRET_PATTERNS: OnceLock<Vec<(Regex, &'static str)>> = OnceLock::new();
        let regexes = SECRET_PATTERNS.get_or_init(|| {
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
        });

        for (regex, name) in regexes {
            if self.sensitivity > 0.5 {
                collect_regex_redactions(
                    content,
                    regex,
                    protected_spans,
                    name,
                    "[REDACTED_SECRET]",
                    patterns,
                    redactions,
                );
            }
        }
    }

    /// Check for private keys.
    fn check_private_keys(
        &self,
        content: &str,
        protected_spans: &[Range<usize>],
        patterns: &mut Vec<String>,
        redactions: &mut Vec<Redaction>,
    ) {
        // PEM-encoded private keys
        let key_patterns = [
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

        for (begin, end, name) in key_patterns {
            let mut search_from = 0;
            let mut matched = false;

            while let Some(start_offset) = content[search_from..].find(begin) {
                let start_idx = search_from + start_offset;
                search_from = start_idx + begin.len();
                if is_span_protected(&(start_idx..search_from), protected_spans) {
                    continue;
                }

                let end_search_from = start_idx + begin.len();
                let mut end_scan_from = end_search_from;
                let end_idx = loop {
                    let Some(end_offset) = content[end_scan_from..].find(end) else {
                        break None;
                    };
                    let candidate_end = end_scan_from + end_offset;
                    end_scan_from = candidate_end + end.len();
                    if !is_span_protected(&(candidate_end..end_scan_from), protected_spans) {
                        break Some(candidate_end);
                    }
                };
                let Some(end_idx) = end_idx else {
                    continue;
                };
                let span = start_idx..end_idx + end.len();
                search_from = span.end;

                for unprotected in unprotected_subspans(span, protected_spans) {
                    matched = true;
                    redactions.push(Redaction {
                        span: unprotected,
                        replacement: "[REDACTED_PRIVATE_KEY]",
                    });
                }
            }

            if matched {
                patterns.push(name.to_string());
            }
        }
    }

    /// Check for JWT tokens.
    fn check_jwt_tokens(
        &self,
        content: &str,
        protected_spans: &[Range<usize>],
        patterns: &mut Vec<String>,
        redactions: &mut Vec<Redaction>,
    ) {
        static JWT_PATTERN: OnceLock<Regex> = OnceLock::new();
        let regex = JWT_PATTERN.get_or_init(|| {
            // JWT: three base64url-encoded parts separated by dots
            Regex::new(r"eyJ[a-zA-Z0-9_-]*\.eyJ[a-zA-Z0-9_-]*\.[a-zA-Z0-9_-]*").unwrap()
        });

        collect_regex_redactions(
            content,
            regex,
            protected_spans,
            "JWT token",
            "[REDACTED_JWT]",
            patterns,
            redactions,
        );
    }

    /// Check for database connection URLs.
    fn check_database_urls(
        &self,
        content: &str,
        protected_spans: &[Range<usize>],
        patterns: &mut Vec<String>,
        redactions: &mut Vec<Redaction>,
    ) {
        static DB_PATTERNS: OnceLock<Vec<(Regex, &'static str)>> = OnceLock::new();
        let regexes = DB_PATTERNS.get_or_init(|| {
            vec![
                (
                    Regex::new(r"postgres(ql)?://[^:]+:[^@]+@[^\s]+").unwrap(),
                    "PostgreSQL connection URL",
                ),
                (
                    Regex::new(r"mysql://[^:]+:[^@]+@[^\s]+").unwrap(),
                    "MySQL connection URL",
                ),
                (
                    Regex::new(r"mongodb(\+srv)?://[^:]+:[^@]+@[^\s]+").unwrap(),
                    "MongoDB connection URL",
                ),
                (
                    Regex::new(r"redis://[^:]+:[^@]+@[^\s]+").unwrap(),
                    "Redis connection URL",
                ),
            ]
        });

        for (regex, name) in regexes {
            collect_regex_redactions(
                content,
                regex,
                protected_spans,
                name,
                "[REDACTED_DATABASE_URL]",
                patterns,
                redactions,
            );
        }
    }

    fn check_bot_token(
        &self,
        content: &str,
        protected_spans: &[Range<usize>],
        patterns: &mut Vec<String>,
        redactions: &mut Vec<Redaction>,
    ) {
        static BOT_TOKEN_PATTERN: OnceLock<Regex> = OnceLock::new();
        let regex =
            BOT_TOKEN_PATTERN.get_or_init(|| Regex::new(r"/bot[0-9]+:[A-Za-z0-9_-]+").unwrap());

        collect_regex_redactions(
            content,
            regex,
            protected_spans,
            "Bot token",
            "/bot[REDACTED_BOT_TOKEN]",
            patterns,
            redactions,
        );
    }

    fn check_high_entropy_tokens(
        &self,
        content: &str,
        protected_spans: &[Range<usize>],
        patterns: &mut Vec<String>,
        redactions: &mut Vec<Redaction>,
    ) {
        // Entropy threshold scales with sensitivity: at 0.7 this is ~4.37.
        let entropy_threshold = 3.5 + self.sensitivity * 1.25;

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
        let mut entropy_protected = protected_spans.to_vec();
        collect_regex_spans(content, url_re, &mut entropy_protected);
        collect_regex_spans(content, media_re, &mut entropy_protected);
        collect_regex_spans(content, receipt_re, &mut entropy_protected);
        let entropy_protected = merge_spans(entropy_protected);

        let tokens = extract_candidate_tokens(content);

        for token in tokens {
            if is_span_protected(&token.span, &entropy_protected) {
                continue;
            }

            if is_path_like_token(token.value) {
                if collect_path_segment_entropy_redactions(&token, entropy_threshold, redactions) {
                    patterns.push("High-entropy token".to_string());
                }
            } else if is_high_entropy_candidate(token.value, entropy_threshold) {
                patterns.push("High-entropy token".to_string());
                redactions.push(Redaction {
                    span: token.span,
                    replacement: "[REDACTED_HIGH_ENTROPY_TOKEN]",
                });
            }
        }
    }
}

/// Extract candidate tokens by splitting on characters outside the
/// alphanumeric + common credential character set.
fn extract_candidate_tokens(content: &str) -> Vec<CandidateToken<'_>> {
    let mut tokens = Vec::new();
    let mut start = None;

    for (idx, ch) in content.char_indices() {
        let is_token_char = ch.is_ascii_alphanumeric() || matches!(ch, '_' | '-' | '+' | '/');
        if is_token_char {
            start.get_or_insert(idx);
        } else if let Some(token_start) = start.take() {
            tokens.push(CandidateToken {
                value: &content[token_start..idx],
                span: token_start..idx,
            });
        }
    }

    if let Some(token_start) = start {
        tokens.push(CandidateToken {
            value: &content[token_start..],
            span: token_start..content.len(),
        });
    }

    tokens
}

/// Compute Shannon entropy (bits per character) for the given string.
fn shannon_entropy(s: &str) -> f64 {
    let len = s.len() as f64;
    if len == 0.0 {
        return 0.0;
    }
    let mut freq = [0usize; 256];
    for &b in s.as_bytes() {
        freq[b as usize] += 1;
    }

    freq.into_iter()
        .filter(|&count| count > 0)
        .fold(0.0, |acc, count| {
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

fn is_high_entropy_candidate(s: &str, entropy_threshold: f64) -> bool {
    s.len() >= ENTROPY_TOKEN_MIN_LEN
        && shannon_entropy(s) >= entropy_threshold
        && has_mixed_alpha_digit(s)
}

fn collect_path_segment_entropy_redactions(
    token: &CandidateToken<'_>,
    entropy_threshold: f64,
    redactions: &mut Vec<Redaction>,
) -> bool {
    let mut found = false;
    let mut offset = 0;
    for segment in token.value.split('/') {
        let end = offset + segment.len();
        if is_high_entropy_candidate(segment, entropy_threshold) {
            found = true;
            redactions.push(Redaction {
                span: token.span.start + offset..token.span.start + end,
                replacement: "[REDACTED_HIGH_ENTROPY_TOKEN]",
            });
        }
        offset = end + 1;
    }
    found
}

fn is_path_like_token(s: &str) -> bool {
    if !s.contains('/') {
        return false;
    }
    let mut segments = s.split('/').filter(|segment| !segment.is_empty());
    let Some(first_segment) = segments.next() else {
        return false;
    };

    let mut count = 1;
    let mut has_dated_slug = is_dated_slug_segment(first_segment);
    let mut all_segments_path_like = is_path_segment_like(first_segment);
    for segment in segments {
        count += 1;
        has_dated_slug |= is_dated_slug_segment(segment);
        all_segments_path_like &= is_path_segment_like(segment);
    }

    count >= 3 && has_dated_slug && all_segments_path_like
}

fn is_path_segment_like(segment: &str) -> bool {
    is_dated_slug_segment(segment)
        || is_env_root_segment(segment)
        || is_lower_path_segment(segment)
        || is_upper_path_segment(segment)
        || is_acronym_slug_segment(segment)
}

fn is_dated_slug_segment(segment: &str) -> bool {
    starts_with_iso_date(segment) && segment[10..].bytes().any(|b| b.is_ascii_lowercase())
}

fn is_env_root_segment(segment: &str) -> bool {
    segment.contains('_')
        && segment
            .bytes()
            .all(|b| b.is_ascii_uppercase() || b.is_ascii_digit() || matches!(b, b'_'))
}

fn is_lower_path_segment(segment: &str) -> bool {
    segment
        .bytes()
        .all(|b| b.is_ascii_lowercase() || b.is_ascii_digit() || matches!(b, b'-' | b'_'))
}

fn is_upper_path_segment(segment: &str) -> bool {
    segment
        .bytes()
        .all(|b| b.is_ascii_uppercase() || b.is_ascii_digit() || matches!(b, b'-' | b'_'))
}

fn is_acronym_slug_segment(segment: &str) -> bool {
    segment.contains('-')
        && segment
            .bytes()
            .all(|b| b.is_ascii_alphanumeric() || matches!(b, b'-' | b'_'))
        && segment.split('-').all(|part| {
            part.bytes()
                .all(|b| b.is_ascii_lowercase() || b.is_ascii_digit())
                || part
                    .bytes()
                    .all(|b| b.is_ascii_uppercase() || b.is_ascii_digit())
        })
}

fn starts_with_iso_date(s: &str) -> bool {
    let bytes = s.as_bytes();
    bytes.len() >= 10
        && bytes[0..4].iter().all(u8::is_ascii_digit)
        && bytes[4] == b'-'
        && bytes[5..7].iter().all(u8::is_ascii_digit)
        && bytes[7] == b'-'
        && bytes[8..10].iter().all(u8::is_ascii_digit)
}

fn collect_regex_redactions(
    content: &str,
    regex: &Regex,
    protected_spans: &[Range<usize>],
    pattern_name: &str,
    replacement: &'static str,
    patterns: &mut Vec<String>,
    redactions: &mut Vec<Redaction>,
) {
    let mut matched = false;
    for mat in regex.find_iter(content) {
        let span = mat.start()..mat.end();
        for unprotected in unprotected_subspans(span, protected_spans) {
            if !content[unprotected.clone()]
                .bytes()
                .any(|b| b.is_ascii_alphanumeric())
            {
                continue;
            }
            matched = true;
            redactions.push(Redaction {
                span: unprotected,
                replacement,
            });
        }
    }

    if matched {
        patterns.push(pattern_name.to_string());
    }
}

fn collect_regex_spans(content: &str, regex: &Regex, spans: &mut Vec<Range<usize>>) {
    spans.extend(regex.find_iter(content).map(|mat| mat.start()..mat.end()));
}

fn apply_redactions(content: &str, redactions: &[Redaction]) -> String {
    if redactions.is_empty() {
        return content.to_string();
    }

    let mut sorted = redactions.to_vec();
    sorted.sort_by(|a, b| {
        a.span
            .start
            .cmp(&b.span.start)
            .then_with(|| b.span.end.cmp(&a.span.end))
    });

    let mut non_overlapping = Vec::new();
    let mut last_end = 0;
    for redaction in sorted {
        if redaction.span.start >= last_end {
            last_end = redaction.span.end;
            non_overlapping.push(redaction);
        }
    }

    let mut redacted = content.to_string();
    for redaction in non_overlapping.iter().rev() {
        redacted.replace_range(
            redaction.span.start..redaction.span.end,
            redaction.replacement,
        );
    }
    redacted
}

fn is_span_protected(span: &Range<usize>, protected_spans: &[Range<usize>]) -> bool {
    protected_spans
        .iter()
        .any(|protected| span.start < protected.end && span.end > protected.start)
}

fn unprotected_subspans(span: Range<usize>, protected_spans: &[Range<usize>]) -> Vec<Range<usize>> {
    let mut subspans = Vec::new();
    let mut cursor = span.start;

    for protected in protected_spans {
        if protected.end <= cursor {
            continue;
        }
        if protected.start >= span.end {
            break;
        }
        if cursor < protected.start {
            subspans.push(cursor..protected.start.min(span.end));
        }
        cursor = cursor.max(protected.end);
        if cursor >= span.end {
            break;
        }
    }

    if cursor < span.end {
        subspans.push(cursor..span.end);
    }

    subspans
}

fn merge_spans(mut spans: Vec<Range<usize>>) -> Vec<Range<usize>> {
    if spans.is_empty() {
        return spans;
    }

    spans.sort_by_key(|span| (span.start, span.end));
    let mut merged = Vec::new();
    let mut iter = spans.into_iter();
    let Some(mut current) = iter.next() else {
        return Vec::new();
    };
    for span in iter {
        if span.start <= current.end {
            current.end = current.end.max(span.end);
        } else {
            merged.push(current);
            current = span;
        }
    }
    merged.push(current);
    merged
}

#[cfg(test)]
mod tests {
    use super::*;

    fn has_high_entropy_candidate_without_path_exemption(content: &str) -> bool {
        let entropy_threshold = 3.5 + 0.7 * 1.25;
        extract_candidate_tokens(content).into_iter().any(|token| {
            token.value.len() >= ENTROPY_TOKEN_MIN_LEN
                && shannon_entropy(token.value) >= entropy_threshold
                && has_mixed_alpha_digit(token.value)
        })
    }

    /// The withholding table carries thresholds the detector's regexes own. If
    /// a pattern's length requirement moves and this table does not, a
    /// streaming caller either publishes a value the detector would have
    /// redacted or holds one back forever.
    #[test]
    fn withhold_thresholds_match_the_detector_patterns() {
        // Deterministic patterns only: the entropy heuristic fires on a long
        // enough run on its own, which would mask what this pins.
        let detector = LeakDetector::with_config(&LeakDetectionConfig {
            enabled: true,
            sensitivity: 1.0,
            high_entropy_tokens: false,
        });

        for (parts, threshold) in CREDENTIAL_KEY_THRESHOLDS {
            let lower = parts.join("_");
            // Both spellings, because the withholding matches a key without
            // regard to case: a pattern that does not would hold the value
            // back and then publish it unredacted, which is worse than never
            // having withheld it. `API_KEY` and `AWS_SECRET_ACCESS_KEY` are
            // also the conventional environment-variable spellings, so the
            // uppercase form is the likelier one to arrive.
            for key in [lower.clone(), lower.to_uppercase()] {
                // Values end on a character that starts none of these keys, so
                // the trailing-prefix hold does not stand in for the threshold.
                let short = format!("{key}={}9", "a".repeat(threshold - 2));
                let complete = format!("{key}={}9", "a".repeat(threshold - 1));

                assert!(
                    matches!(detector.scan(&short), LeakResult::Clean),
                    "{key}: a value one short of {threshold} is below the detector, \
                     so the tail must be withheld rather than published"
                );
                assert_eq!(
                    incomplete_credential_tail(&short),
                    Some(0),
                    "{key}: a value one short of {threshold} can still complete"
                );

                assert!(
                    matches!(detector.scan(&complete), LeakResult::Detected { .. }),
                    "{key}: a value of {threshold} must be detected, or the \
                     threshold here is larger than the pattern needs"
                );
                assert_eq!(
                    incomplete_credential_tail(&complete),
                    None,
                    "{key}: the detector redacts a complete value, so withholding \
                     it would stall the surface"
                );
            }
        }
    }

    /// Withholding costs the reader visible text, so it has to end as soon as
    /// a match becomes impossible. These keys are ordinary words for an agent
    /// that talks about credentials, and holding the rest of a turn back
    /// whenever one appears would be worse than the exposure it prevents.
    #[test]
    fn text_that_can_no_longer_become_a_credential_is_published() {
        for text in [
            // A space ends the value, and the pattern requires an unbroken run.
            "the token: is a concept worth explaining",
            // The pattern wants the separator against the key.
            "I stored the password in the vault.",
            // `key` alone announces nothing; `api[_-]?key` does.
            "no key here at all!",
            // Already long enough to be detected and redacted.
            "token=abcdefghijklmnopqrstuvwx",
        ] {
            assert_eq!(
                incomplete_credential_tail(text),
                None,
                "{text:?} has nothing pending and must publish"
            );
        }
    }

    /// The offset is where the key starts, not where the value does: the
    /// detector's replacement covers the key too, so publishing `token=` and
    /// then replacing it would contradict the frame before it.
    #[test]
    fn a_pending_credential_is_withheld_from_the_key() {
        let held = incomplete_credential_tail("Here it is: token=aB3xK9mW2p")
            .expect("a ten-character token value can still reach twenty");
        assert_eq!(&"Here it is: token=aB3xK9mW2p"[..held], "Here it is: ");
    }

    /// The price of covering a key that arrives in pieces: text ending on a
    /// prefix of one waits for the delta that decides the word. Bounded by the
    /// longest key, and released as soon as the word cannot be one.
    #[test]
    fn a_trailing_prefix_of_a_key_waits_for_the_next_delta() {
        assert_eq!(incomplete_credential_tail("the sec"), Some(4));
        assert_eq!(
            incomplete_credential_tail("the section"),
            None,
            "the word resolved to something that is not a key"
        );
    }

    /// What a streaming caller publishes must only ever grow. Teams rejects a
    /// frame that does not contain the one before it, and a reader who saw text
    /// disappear is owed better than a protocol error either way.
    ///
    /// The deltas break mid-key, which is where withholding from a completed
    /// key alone retracts what it already published: `to` goes out, then the
    /// key resolves and the offset moves back behind it.
    #[test]
    fn published_prefixes_only_ever_grow() {
        let detector = LeakDetector::with_config(&LeakDetectionConfig {
            enabled: true,
            sensitivity: 1.0,
            high_entropy_tokens: false,
        });

        let mut accumulated = String::new();
        let mut previous = String::new();
        for delta in [
            "Cron output: file:///tmp/report.md?to",
            "ken=aB3xK9mW2p",
            "Q7vL4nR8sT1yU6hD0jF5cG",
            " Grab it soon.",
        ] {
            accumulated.push_str(delta);
            let publishable = match incomplete_credential_tail(&accumulated) {
                Some(offset) => &accumulated[..offset],
                None => accumulated.as_str(),
            };
            let published = match detector.scan(publishable) {
                LeakResult::Detected { redacted, .. } => redacted,
                LeakResult::Clean => publishable.to_string(),
            };

            assert!(
                published.starts_with(&previous),
                "frame {published:?} does not contain {previous:?}"
            );
            assert!(
                !published.contains("aB3x"),
                "frame {published:?} published a raw credential prefix"
            );
            previous = published;
        }

        assert!(
            previous.contains("[REDACTED") && previous.ends_with(" Grab it soon."),
            "the last frame must carry the redacted value and the text after it: {previous:?}"
        );
    }

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
    fn generated_workspace_paths_not_redacted_as_high_entropy() {
        let detector = LeakDetector::new();
        let cases = [
            "missions/2026-07-02-plan-b-for-something-useful/briefs/ARCH-1-plan-b-useful-direction.md",
            "/home/zeroclaw/.zeroclaw/agents/scribe/workspace/tasks/inbox/2026-07-02-13-20-plan-b-draft-materialization.md",
            "/home/zeroclaw/.zeroclaw/agents/scribe/workspace/drafts/2026-07-02-plan-b-for-something-useful/",
            "$ZC_DIR/agents/scribe/workspace/drafts/2026-07-02-plan-b-for-something-useful/",
            "agents/scribe/workspace/drafts/2026-07-02-plan-b-for-something-useful/",
            "drafts/2026-07-03-v3-delegation-practices-reviewed-source/proposed/shared/skills/core/useful-routing-and-planning-governance/SKILL.md",
        ];

        for path in cases {
            let content = format!("Recorded path: {path}");
            assert!(
                has_high_entropy_candidate_without_path_exemption(&content),
                "fixture should reproduce the old entropy false positive: {path}"
            );
            assert!(
                matches!(detector.scan(&content), LeakResult::Clean),
                "workspace path should not be redacted: {path}"
            );
        }
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
        let tokens = extract_candidate_tokens("foo.bar:baz qux-quux key=val path/segment");
        let values: Vec<_> = tokens.iter().map(|token| token.value).collect();
        assert!(values.contains(&"foo"));
        assert!(values.contains(&"bar"));
        assert!(values.contains(&"baz"));
        assert!(values.contains(&"qux-quux"));
        assert!(values.contains(&"path/segment"));
        // '=' is a delimiter, not part of tokens
        assert!(values.contains(&"key"));
        assert!(values.contains(&"val"));
    }

    // Protected spans are honored only by the high-entropy heuristic, which
    // misfires on the *shape* of ordinary generated paths. Deterministic
    // credential patterns (API keys, AWS creds, private keys, JWTs, DB URLs,
    // bot tokens, generic secrets) are precise, low-false-positive signals that
    // a real credential can trigger just as easily inside a link destination or
    // file reference as in visible text, so they always scan full content and
    // are never suppressed by a caller-supplied protected span.

    #[test]
    fn protected_spans_are_opaque_only_to_the_entropy_heuristic() {
        let detector = LeakDetector::new();
        let content = "link-target aB3xK9mW2pQ7vL4nR8sT1yU6hD0jF5cG";
        let protected = "link-target ".len()..content.len();

        assert!(matches!(
            detector.scan_with_protected_spans(content, std::slice::from_ref(&protected)),
            LeakResult::Clean
        ));
    }

    #[test]
    fn deterministic_secret_syntax_is_still_detected_inside_a_protected_uri() {
        let detector = LeakDetector::new();
        let target = "file:///tmp/report.md?token=aB3xK9mW2pQ7vL4nR8sT1yU6hD0jF5cG";
        let content = format!("Recorded {target}.");
        let start = "Recorded ".len();
        let protected = start..start + target.len();

        match detector.scan_with_protected_spans(&content, std::slice::from_ref(&protected)) {
            LeakResult::Detected { patterns, redacted } => {
                assert!(
                    patterns.iter().any(|p| p == "Token value"),
                    "patterns: {patterns:?}"
                );
                assert!(
                    redacted.contains("[REDACTED_SECRET]"),
                    "redacted: {redacted}"
                );
            }
            LeakResult::Clean => {
                panic!(
                    "a deterministic secret pattern inside a protected span must still be caught"
                )
            }
        }
    }

    #[test]
    fn private_key_markers_are_still_detected_inside_a_protected_span() {
        let detector = LeakDetector::new();
        let target = "file:///tmp/-----BEGIN PRIVATE KEY-----abc-----END PRIVATE KEY-----.pem";
        let content = format!("Recorded {target}.");
        let start = "Recorded ".len();
        let protected = start..start + target.len();

        match detector.scan_with_protected_spans(&content, std::slice::from_ref(&protected)) {
            LeakResult::Detected { redacted, .. } => {
                assert!(
                    redacted.contains("[REDACTED_PRIVATE_KEY]"),
                    "redacted: {redacted}"
                );
            }
            LeakResult::Clean => {
                panic!("private key markers should still be detected regardless of protected spans")
            }
        }
    }

    #[test]
    fn invalid_protected_span_boundaries_are_ignored() {
        let detector = LeakDetector::new();
        let content = "é leaked token=aB3xK9mW2pQ7vL4nR8sT1yU6hD0jF5cG";
        let invalid_utf8_boundary = 0..1;

        match detector.scan_with_protected_spans(content, &[invalid_utf8_boundary]) {
            LeakResult::Detected { redacted, .. } => {
                assert!(redacted.contains("[REDACTED"));
            }
            LeakResult::Clean => panic!("invalid protected span should be ignored"),
        }
    }

    #[test]
    fn private_key_detection_ignores_protected_spans() {
        let detector = LeakDetector::new();
        let leaked_key = "-----BEGIN PRIVATE KEY-----\nrealkeybody\n-----END PRIVATE KEY-----";
        let content = format!("Recorded a reference.\nLeaked:\n{leaked_key}");
        // Marking the whole message as "protected" must not suppress a real
        // leaked key.
        let protected = 0..content.len();

        match detector.scan_with_protected_spans(&content, std::slice::from_ref(&protected)) {
            LeakResult::Detected { redacted, .. } => {
                assert!(!redacted.contains("realkeybody"), "redacted: {redacted}");
                assert!(
                    redacted.contains("[REDACTED_PRIVATE_KEY]"),
                    "redacted: {redacted}"
                );
            }
            LeakResult::Clean => {
                panic!("private key should still be detected even under a protected span")
            }
        }
    }

    #[test]
    fn protected_spans_do_not_hide_unprotected_high_entropy_tokens() {
        let detector = LeakDetector::new();
        let protected_token = "aB3xK9mW2pQ7vL4nR8sT1yU6hD0jF5cG";
        let leaked_token = "zC9vN4mK8pQ2rL7xT5yU1hD6jF0gB3wE";
        let content = format!("safe-target {protected_token}\nactual {leaked_token}");
        let protected = 0.."safe-target ".len() + protected_token.len();

        match detector.scan_with_protected_spans(&content, std::slice::from_ref(&protected)) {
            LeakResult::Detected { redacted, .. } => {
                assert!(redacted.contains(protected_token));
                assert!(!redacted.contains(leaked_token));
            }
            LeakResult::Clean => panic!("unprotected token should still be detected"),
        }
    }

    #[test]
    fn protected_spans_do_not_hide_a_secret_pattern_that_overlaps_them() {
        let detector = LeakDetector::new();
        let target = "file:///tmp/report.md";
        let content = format!("[password=longsecretvalue]({target})");
        let start = "[password=longsecretvalue](".len();
        let protected = start..start + target.len();

        match detector.scan_with_protected_spans(&content, std::slice::from_ref(&protected)) {
            LeakResult::Detected { redacted, .. } => {
                assert!(
                    !redacted.contains("longsecretvalue"),
                    "redacted: {redacted}"
                );
                assert!(
                    redacted.contains("[REDACTED_SECRET]"),
                    "redacted: {redacted}"
                );
            }
            LeakResult::Clean => panic!("unprotected link text secret should still be detected"),
        }
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
    fn slash_containing_high_entropy_token_still_detected() {
        let detector = LeakDetector::new();
        let cases = [
            "/aB3xK9mW2pQ7vL4n/R8sT1yU6hD0jF5cG/zP4qX7vN2mK8rL5s",
            "/2026-07-04/aB3xK9mW2pQ7vL4n/R8sT1yU6hD0jF5cG/zP4qX7vN2mK8rL5s",
            "/2026-07-04-plan/aB3xK9mW2pQ7vL4n/R8sT1yU6hD0jF5cG/zP4qX7vN2mK8rL5s",
        ];

        for token in cases {
            match detector.scan(&format!("Leaked credential: token={token}")) {
                LeakResult::Detected { redacted, .. } => {
                    assert!(redacted.contains("[REDACTED_HIGH_ENTROPY_TOKEN]"));
                }
                LeakResult::Clean => {
                    panic!("slash-containing high-entropy token should be detected: {token}")
                }
            }
        }
    }

    #[test]
    fn disabled_detector_returns_clean_without_redaction() {
        let detector = LeakDetector::with_config(&LeakDetectionConfig {
            enabled: false,
            ..LeakDetectionConfig::default()
        });
        let content = "Leaked credential: aB3xK9mW2pQ7vL4nR8sT1yU6hD0jF5cG";

        let result = detector.scan(content);

        assert!(matches!(result, LeakResult::Clean));
    }

    #[test]
    fn high_entropy_detection_can_be_disabled_without_disabling_specific_patterns() {
        let detector = LeakDetector::with_config(&LeakDetectionConfig {
            high_entropy_tokens: false,
            ..LeakDetectionConfig::default()
        });

        assert!(matches!(
            detector.scan("Leaked credential: aB3xK9mW2pQ7vL4nR8sT1yU6hD0jF5cG"),
            LeakResult::Clean
        ));
        assert!(matches!(
            detector.scan("AWS key: AKIAIOSFODNN7EXAMPLE"),
            LeakResult::Detected { .. }
        ));
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
    fn detects_slack_tokens() {
        // High-entropy scanning is disabled so each case proves the *specific*
        // Slack pattern redacts the token, not the entropy fallback (which a
        // user may turn off while these credential patterns stay enabled).
        // `absent` is the substring that must not survive redaction: for the
        // rotated `xoxe.` forms it is the leading `xoxe` prefix, proving the
        // whole token is redacted rather than only the inner `xoxb-`/`xoxp-`.
        let config = LeakDetectionConfig {
            sensitivity: 0.5,
            high_entropy_tokens: false,
            ..Default::default()
        };
        let detector = LeakDetector::with_config(&config);
        // Assemble rotation-token fixtures at runtime so push protection does not
        // mistake synthetic test data for live Slack credentials.
        let refresh = ["xo", "xe-1-", "xxxxxxxxxxxxxxxxxxxxxxxxxxxxxxxxxx"].concat();
        let rotated_bot = ["xo", "xe.xoxb-1-", "xxxxxxxxxxxxxxxxxxxxxxxxxxxxxx"].concat();
        let rotated_user = ["xo", "xe.xoxp-1-", "xxxxxxxxxxxxxxxxxxxxxxxxxxxxxx"].concat();

        // (label, content, substring that must be gone from the output)
        // Placeholder token bodies are all-`x`; rotation tokens are assembled
        // above so synthetic fixtures are not mistaken for live credentials.
        let cases = [
            (
                "bot",
                "SLACK_BOT_TOKEN=xoxb-xxxxxxxxxxxxxxxxxxxxxxxxxxxxxxxxxxxx",
                "xoxb-xxxxxxxxxxxxxxxxxxxxxxxxxxxxxxxxxxxx",
            ),
            (
                "user",
                "xoxp-xxxxxxxxxxxxxxxxxxxxxxxxxxxxxxxxxxxx",
                "xoxp-xxxxxxxxxxxxxxxxxxxxxxxxxxxxxxxxxxxx",
            ),
            (
                "app-level",
                "xapp-xxxxxxxxxxxxxxxxxxxxxxxxxxxxxxxxxxxx",
                "xapp-xxxxxxxxxxxxxxxxxxxxxxxxxxxxxxxxxxxx",
            ),
            (
                "workflow",
                "xwfp-xxxxxxxxxxxxxxxxxxxxxxxxxxxxxxxxxxxx",
                "xwfp-xxxxxxxxxxxxxxxxxxxxxxxxxxxxxxxxxxxx",
            ),
            ("refresh", refresh.as_str(), refresh.as_str()),
            ("rotated bot", rotated_bot.as_str(), "xoxe"),
            ("rotated user", rotated_user.as_str(), "xoxe"),
        ];

        for (label, content, absent) in cases {
            match detector.scan(content) {
                LeakResult::Detected { patterns, redacted } => {
                    assert!(
                        patterns.iter().any(|p| p.contains("Slack")),
                        "{label}: expected a Slack pattern, got {patterns:?}"
                    );
                    assert!(
                        !redacted.contains(absent),
                        "{label}: `{absent}` survived redaction in `{redacted}`"
                    );
                }
                LeakResult::Clean => panic!("{label}: Slack token not detected"),
            }
        }

        assert!(matches!(
            detector.scan("xoxe.example.com"),
            LeakResult::Clean
        ));
    }

    #[test]
    fn bot_token_leaves_unrelated_text_clean() {
        let detector = LeakDetector::new();
        assert!(matches!(
            detector.scan("connection reset by peer"),
            LeakResult::Clean
        ));
    }
}
