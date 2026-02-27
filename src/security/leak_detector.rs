//! Credential leak detection for outbound content.
//!
//! Scans outbound messages for potential credential leaks before they are sent,
//! preventing accidental exfiltration of API keys, tokens, passwords, and other
//! sensitive values.
//!
//! Contributed from RustyClaw (MIT licensed).

use regex::Regex;
use std::sync::OnceLock;

/// Minimum sensitivity required to activate heuristic (generic) secret rules.
///
/// Structurally identifiable patterns (API keys with known prefixes, AWS keys,
/// JWTs, PEM blocks, database URLs) are always scanned regardless of sensitivity.
/// Generic rules (password=, secret=, token=) only fire when `sensitivity` exceeds
/// this threshold, reducing false positives on technical content.
const GENERIC_SECRET_SENSITIVITY_THRESHOLD: f64 = 0.5;
const ENTROPY_TOKEN_MIN_LEN: usize = 20;
const HIGH_ENTROPY_BASELINE: f64 = 4.2;

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
        self.check_high_entropy_tokens(content, &mut patterns, &mut redacted);

        if patterns.is_empty() {
            LeakResult::Clean
        } else {
            LeakResult::Detected { patterns, redacted }
        }
    }

    /// Check for common API key patterns.
    fn check_api_keys(&self, content: &str, patterns: &mut Vec<String>, redacted: &mut String) {
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
                // Generic
                (
                    Regex::new(r#"api[_-]?key[=:]\s*['"]*[a-zA-Z0-9_-]{20,}"#).unwrap(),
                    "Generic API key",
                ),
            ]
        });

        for (regex, name) in regexes {
            if regex.is_match(content) {
                patterns.push(name.to_string());
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
        static AWS_PATTERNS: OnceLock<Vec<(Regex, &'static str)>> = OnceLock::new();
        let regexes = AWS_PATTERNS.get_or_init(|| {
            vec![
                (
                    Regex::new(r"AKIA[A-Z0-9]{16}").unwrap(),
                    "AWS Access Key ID",
                ),
                (
                    Regex::new(
                        r#"aws[_-]?secret[_-]?access[_-]?key[=:]\s*['"]*[a-zA-Z0-9/+=]{40}"#,
                    )
                    .unwrap(),
                    "AWS Secret Access Key",
                ),
            ]
        });

        for (regex, name) in regexes {
            if regex.is_match(content) {
                patterns.push(name.to_string());
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
            if regex.is_match(content) && self.sensitivity > GENERIC_SECRET_SENSITIVITY_THRESHOLD {
                patterns.push(name.to_string());
                *redacted = regex.replace_all(redacted, "[REDACTED_SECRET]").to_string();
            }
        }
    }

    /// Check for private keys.
    fn check_private_keys(&self, content: &str, patterns: &mut Vec<String>, redacted: &mut String) {
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
            if content.contains(begin) && content.contains(end) {
                patterns.push(name.to_string());
                // Redact the entire key block
                if let Some(start_idx) = content.find(begin) {
                    if let Some(end_idx) = content.find(end) {
                        let key_block = &content[start_idx..end_idx + end.len()];
                        *redacted = redacted.replace(key_block, "[REDACTED_PRIVATE_KEY]");
                    }
                }
            }
        }
    }

    /// Check for JWT tokens.
    fn check_jwt_tokens(&self, content: &str, patterns: &mut Vec<String>, redacted: &mut String) {
        static JWT_PATTERN: OnceLock<Regex> = OnceLock::new();
        let regex = JWT_PATTERN.get_or_init(|| {
            // JWT: three base64url-encoded parts separated by dots
            Regex::new(r"eyJ[a-zA-Z0-9_-]*\.eyJ[a-zA-Z0-9_-]*\.[a-zA-Z0-9_-]*").unwrap()
        });

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
            if regex.is_match(content) {
                patterns.push(name.to_string());
                *redacted = regex
                    .replace_all(redacted, "[REDACTED_DATABASE_URL]")
                    .to_string();
            }
        }
    }

    /// Check for high-entropy tokens that resemble obfuscated secrets.
    fn check_high_entropy_tokens(
        &self,
        content: &str,
        patterns: &mut Vec<String>,
        redacted: &mut String,
    ) {
        let threshold = (HIGH_ENTROPY_BASELINE + (self.sensitivity - 0.5) * 0.6).clamp(3.9, 4.8);
        let mut flagged = false;

        for token in extract_candidate_tokens(content) {
            if token.len() < ENTROPY_TOKEN_MIN_LEN {
                continue;
            }

            // Lower false positives by requiring mixed alphanumerics.
            let has_alpha = token.chars().any(|c| c.is_ascii_alphabetic());
            let has_digit = token.chars().any(|c| c.is_ascii_digit());
            if !(has_alpha && has_digit) {
                continue;
            }

            let entropy = shannon_entropy(token.as_bytes());
            if entropy >= threshold {
                flagged = true;
                let replaced = redacted.replace(token, "[REDACTED_HIGH_ENTROPY_TOKEN]");
                if replaced != *redacted {
                    *redacted = replaced;
                } else if redacted.contains("[REDACTED_SECRET]") {
                    *redacted =
                        redacted.replacen("[REDACTED_SECRET]", "[REDACTED_HIGH_ENTROPY_TOKEN]", 1);
                }
            }
        }

        if flagged {
            patterns.push("High-entropy token (possible encoded secret)".to_string());
        }
    }
}

fn extract_candidate_tokens(content: &str) -> Vec<&str> {
    content
        .split(|c: char| {
            !(c.is_ascii_alphanumeric() || c == '_' || c == '-' || c == '+' || c == '/' || c == '=')
        })
        .filter(|token| !token.is_empty())
        .collect()
}

fn shannon_entropy(bytes: &[u8]) -> f64 {
    if bytes.is_empty() {
        return 0.0;
    }
    let mut counts = [0_u32; 256];
    for &b in bytes {
        counts[b as usize] += 1;
    }
    let len = bytes.len() as f64;
    counts
        .iter()
        .filter(|&&count| count > 0)
        .map(|&count| {
            let p = count as f64 / len;
            -p * p.log2()
        })
        .sum()
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
            _ => panic!("Should detect Stripe key"),
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
            _ => panic!("Should detect AWS key"),
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
            _ => panic!("Should detect private key"),
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
            _ => panic!("Should detect JWT"),
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
            _ => panic!("Should detect database URL"),
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
    fn sensitivity_at_threshold_does_not_fire_generic() {
        // The condition is strict `>`, so exactly 0.5 must NOT trigger generic rules.
        let detector = LeakDetector::with_sensitivity(GENERIC_SECRET_SENSITIVITY_THRESHOLD);
        let content = "password=hunter2isasecret";
        let result = detector.scan(content);
        assert!(
            matches!(result, LeakResult::Clean),
            "sensitivity == threshold (0.5) should NOT activate generic-secret rules"
        );
    }

    #[test]
    fn sensitivity_just_above_threshold_fires_generic() {
        let detector = LeakDetector::with_sensitivity(GENERIC_SECRET_SENSITIVITY_THRESHOLD + 0.01);
        let content = "password=hunter2isasecret";
        let result = detector.scan(content);
        assert!(
            matches!(result, LeakResult::Detected { .. }),
            "sensitivity just above threshold should activate generic-secret rules"
        );
    }

    #[test]
    fn structural_api_key_detected_regardless_of_sensitivity() {
        // Stripe key is structurally identifiable — must be caught even at zero sensitivity.
        let detector = LeakDetector::with_sensitivity(0.0);
        let content = "key: sk_test_1234567890abcdefghijklmnop";
        let result = detector.scan(content);
        assert!(
            matches!(result, LeakResult::Detected { .. }),
            "structural API key patterns must fire at any sensitivity level"
        );
    }

    #[test]
    fn high_entropy_token_is_detected_and_redacted() {
        let detector = LeakDetector::with_sensitivity(0.9);
        let content = "token: A9sD2kL0zQ1xW8vN3mR7tY6uI4oP2qS9dF1gH5jK";
        let result = detector.scan(content);
        match result {
            LeakResult::Detected { patterns, redacted } => {
                assert!(patterns.iter().any(|p| p.contains("High-entropy token")));
                assert!(redacted.contains("[REDACTED_HIGH_ENTROPY_TOKEN]"));
            }
            _ => panic!("expected high-entropy detection"),
        }
    }

    #[test]
    fn natural_language_text_is_not_flagged_as_high_entropy() {
        let detector = LeakDetector::with_sensitivity(0.9);
        let content = "the quick brown fox jumps over the lazy dog";
        let result = detector.scan(content);
        assert!(matches!(result, LeakResult::Clean));
    }

    #[test]
    fn shannon_entropy_distinguishes_repetitive_from_random_tokens() {
        let low = shannon_entropy(b"aaaaaaaaaaaaaaaaaaaaaaaaaaaa");
        let high = shannon_entropy(b"aB3f9K1mP0qX8vT2nR6sW4yZ7uH5");
        assert!(high > low);
    }
}
