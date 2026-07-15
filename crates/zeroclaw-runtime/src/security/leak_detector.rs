//! Credential leak detection for outbound content.
//!
//! Scans outbound messages for potential credential leaks before they are sent,
//! preventing accidental exfiltration of API keys, tokens, passwords, and other
//! sensitive values.
//!
//! Contributed from RustyClaw (MIT licensed).

use regex::Regex;
use std::ops::Range;
use std::sync::OnceLock;
use zeroclaw_config::schema::LeakDetectionConfig;

use super::detection::{DetectionConfidence, DetectionMatch, sanitize_excerpt};

/// Minimum token length considered for high-entropy detection.
const ENTROPY_TOKEN_MIN_LEN: usize = 24;

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
        // syntax) that ordinary generated file paths do not produce, so #8722's
        // false-positive problem does not apply to them. A real credential can
        // be placed inside a link destination or file reference exactly as
        // easily as in visible text, and #8722 requires visible text to still
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
        // Patterns are defined once in `api_key_patterns()` (shared with the
        // typed `detect` API); scan ignores the per-pattern confidence.
        for (regex, name, _confidence) in api_key_patterns() {
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
        for (regex, name, _confidence) in aws_patterns() {
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
        for (regex, name) in generic_secret_patterns() {
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
        // PEM markers are defined once in `PRIVATE_KEY_MARKERS` (shared with the
        // typed `detect` API).
        for &(begin, end, name) in PRIVATE_KEY_MARKERS {
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
        collect_regex_redactions(
            content,
            jwt_pattern(),
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
        for (regex, name, _confidence) in db_url_patterns() {
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

    /// Check for messaging bot tokens embedded in API URLs.
    ///
    /// Telegram bot tokens appear in request URLs as `/bot<id>:<token>` and
    /// would otherwise reach error logs verbatim. The token half is not
    /// guaranteed high-entropy, so it needs an explicit pattern rather than
    /// relying on the entropy scan.
    fn check_bot_token(
        &self,
        content: &str,
        protected_spans: &[Range<usize>],
        patterns: &mut Vec<String>,
        redactions: &mut Vec<Redaction>,
    ) {
        collect_regex_redactions(
            content,
            bot_token_pattern(),
            protected_spans,
            "Bot token",
            "/bot[REDACTED_BOT_TOKEN]",
            patterns,
            redactions,
        );
    }

    /// Check for high-entropy tokens that may be leaked credentials.
    ///
    /// Extracts candidate tokens from content (after stripping URLs to avoid
    /// false-positives on path segments) and flags any that exceed the Shannon
    /// entropy threshold derived from the detector's sensitivity.
    fn check_high_entropy_tokens(
        &self,
        content: &str,
        protected_spans: &[Range<usize>],
        patterns: &mut Vec<String>,
        redactions: &mut Vec<Redaction>,
    ) {
        // Entropy threshold scales with sensitivity: at 0.7 this is ~4.37.
        let entropy_threshold = 3.5 + self.sensitivity * 1.25;

        // Protect URLs and media markers before extracting tokens so that path
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
    // misfires on the *shape* of ordinary generated paths (#8722). Deterministic
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

        let aws = detector.detect("aws_secret_access_key=abcdefghijklmnopqrstuvwxyz0123456789ABCD");
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
