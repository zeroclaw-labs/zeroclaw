//! Data masking and PII detection for ZeroClaw.
//!
//! Provides automatic detection and masking of personally identifiable
//! information (PII) in text before it is stored, logged, or transmitted.
//!
//! ## Supported Pattern Categories
//! - Email addresses
//! - Phone numbers (international, Korean, US)
//! - Credit card numbers (Luhn-validated)
//! - Korean resident registration numbers (주민등록번호)
//! - IP addresses (IPv4)
//! - API keys and tokens (common patterns)

use regex::Regex;
use std::sync::LazyLock;

/// Default mask replacement string.
const DEFAULT_MASK: &str = "***";

/// Compiled regex patterns for PII detection.
struct PiiPatterns {
    email: Regex,
    phone_international: Regex,
    phone_korean: Regex,
    phone_us: Regex,
    credit_card: Regex,
    korean_rrn: Regex,
    ipv4: Regex,
    api_key_bearer: Regex,
    api_key_prefix: Regex,
}

static PII_PATTERNS: LazyLock<PiiPatterns> = LazyLock::new(|| PiiPatterns {
    email: Regex::new(r"[a-zA-Z0-9._%+-]+@[a-zA-Z0-9.-]+\.[a-zA-Z]{2,}").unwrap(),
    phone_international: Regex::new(r"\+\d{1,3}[-.\s]?\d{2,4}[-.\s]?\d{3,4}[-.\s]?\d{3,4}")
        .unwrap(),
    phone_korean: Regex::new(r"0\d{1,2}-\d{3,4}-\d{4}").unwrap(),
    phone_us: Regex::new(r"\(\d{3}\)\s?\d{3}-\d{4}").unwrap(),
    credit_card: Regex::new(r"\b\d{4}[-\s]?\d{4}[-\s]?\d{4}[-\s]?\d{4}\b").unwrap(),
    korean_rrn: Regex::new(r"\b\d{6}[-\s]?\d{7}\b").unwrap(),
    ipv4: Regex::new(r"\b\d{1,3}\.\d{1,3}\.\d{1,3}\.\d{1,3}\b").unwrap(),
    api_key_bearer: Regex::new(r"(?i)bearer\s+[a-zA-Z0-9\-._~+/]+=*").unwrap(),
    api_key_prefix: Regex::new(r"(?i)(sk|pk|api[_-]?key|token|secret)[_-]?[a-zA-Z0-9]{16,}")
        .unwrap(),
});

/// Category of detected PII.
#[derive(Debug, Clone, PartialEq, Eq)]
pub enum PiiCategory {
    Email,
    Phone,
    CreditCard,
    KoreanRrn,
    IpAddress,
    ApiKey,
}

/// A detected PII match in text.
#[derive(Debug, Clone)]
pub struct PiiMatch {
    /// Category of the PII.
    pub category: PiiCategory,
    /// Start byte offset in the original text.
    pub start: usize,
    /// End byte offset in the original text.
    pub end: usize,
    /// The matched text (for display/logging, should be masked).
    pub masked: String,
}

/// Data masking engine for PII detection and redaction.
pub struct DataMasker {
    /// Custom mask string (default: "***").
    mask: String,
    /// Whether masking is enabled.
    enabled: bool,
}

impl DataMasker {
    /// Create a new data masker with default settings.
    pub fn new(enabled: bool) -> Self {
        Self {
            mask: DEFAULT_MASK.to_string(),
            enabled,
        }
    }

    /// Create a new data masker with a custom mask string.
    pub fn with_mask(enabled: bool, mask: String) -> Self {
        Self { mask, enabled }
    }

    /// Detect PII patterns in the given text.
    pub fn detect(&self, text: &str) -> Vec<PiiMatch> {
        if !self.enabled {
            return Vec::new();
        }

        let mut matches = Vec::new();

        // Email addresses
        for m in PII_PATTERNS.email.find_iter(text) {
            matches.push(PiiMatch {
                category: PiiCategory::Email,
                start: m.start(),
                end: m.end(),
                masked: mask_email(m.as_str()),
            });
        }

        // Phone numbers (Korean)
        for m in PII_PATTERNS.phone_korean.find_iter(text) {
            matches.push(PiiMatch {
                category: PiiCategory::Phone,
                start: m.start(),
                end: m.end(),
                masked: self.mask.clone(),
            });
        }

        // Phone numbers (international)
        for m in PII_PATTERNS.phone_international.find_iter(text) {
            // Skip if already matched as Korean phone
            if matches
                .iter()
                .any(|existing| existing.start <= m.start() && existing.end >= m.end())
            {
                continue;
            }
            matches.push(PiiMatch {
                category: PiiCategory::Phone,
                start: m.start(),
                end: m.end(),
                masked: self.mask.clone(),
            });
        }

        // Phone numbers (US)
        for m in PII_PATTERNS.phone_us.find_iter(text) {
            matches.push(PiiMatch {
                category: PiiCategory::Phone,
                start: m.start(),
                end: m.end(),
                masked: self.mask.clone(),
            });
        }

        // Credit card numbers
        for m in PII_PATTERNS.credit_card.find_iter(text) {
            let digits: String = m.as_str().chars().filter(|c| c.is_ascii_digit()).collect();
            if is_valid_luhn(&digits) {
                matches.push(PiiMatch {
                    category: PiiCategory::CreditCard,
                    start: m.start(),
                    end: m.end(),
                    masked: mask_credit_card(m.as_str()),
                });
            }
        }

        // Korean RRN (주민등록번호)
        for m in PII_PATTERNS.korean_rrn.find_iter(text) {
            matches.push(PiiMatch {
                category: PiiCategory::KoreanRrn,
                start: m.start(),
                end: m.end(),
                masked: self.mask.clone(),
            });
        }

        // IP addresses
        for m in PII_PATTERNS.ipv4.find_iter(text) {
            if is_valid_ipv4(m.as_str()) {
                matches.push(PiiMatch {
                    category: PiiCategory::IpAddress,
                    start: m.start(),
                    end: m.end(),
                    masked: mask_ipv4(m.as_str()),
                });
            }
        }

        // API keys / tokens
        for m in PII_PATTERNS.api_key_bearer.find_iter(text) {
            matches.push(PiiMatch {
                category: PiiCategory::ApiKey,
                start: m.start(),
                end: m.end(),
                masked: self.mask.clone(),
            });
        }

        for m in PII_PATTERNS.api_key_prefix.find_iter(text) {
            if matches
                .iter()
                .any(|existing| existing.start <= m.start() && existing.end >= m.end())
            {
                continue;
            }
            matches.push(PiiMatch {
                category: PiiCategory::ApiKey,
                start: m.start(),
                end: m.end(),
                masked: self.mask.clone(),
            });
        }

        // Sort by position for correct replacement
        matches.sort_by_key(|m| m.start);
        matches
    }

    /// Mask all PII in the given text, returning the redacted version.
    pub fn mask_text(&self, text: &str) -> String {
        if !self.enabled {
            return text.to_string();
        }

        let detections = self.detect(text);
        if detections.is_empty() {
            return text.to_string();
        }

        let mut result = String::with_capacity(text.len());
        let mut last_end = 0;

        for detection in &detections {
            // Skip overlapping matches
            if detection.start < last_end {
                continue;
            }

            result.push_str(&text[last_end..detection.start]);
            result.push_str(&detection.masked);
            last_end = detection.end;
        }

        result.push_str(&text[last_end..]);
        result
    }

    /// Check if text contains any PII.
    pub fn contains_pii(&self, text: &str) -> bool {
        !self.detect(text).is_empty()
    }
}

/// Mask an email address (show first char + domain).
fn mask_email(email: &str) -> String {
    if let Some(at_pos) = email.find('@') {
        if at_pos > 0 {
            let first_char = &email[..1];
            let domain = &email[at_pos..];
            return format!("{first_char}***{domain}");
        }
    }
    DEFAULT_MASK.to_string()
}

/// Mask a credit card number (show last 4 digits).
fn mask_credit_card(card: &str) -> String {
    let digits: String = card.chars().filter(|c| c.is_ascii_digit()).collect();
    if digits.len() >= 4 {
        let last4 = &digits[digits.len() - 4..];
        format!("****-****-****-{last4}")
    } else {
        DEFAULT_MASK.to_string()
    }
}

/// Mask an IPv4 address (show first octet only).
fn mask_ipv4(ip: &str) -> String {
    if let Some(dot_pos) = ip.find('.') {
        format!("{}.***.***.***", &ip[..dot_pos])
    } else {
        DEFAULT_MASK.to_string()
    }
}

/// Validate a credit card number using the Luhn algorithm.
fn is_valid_luhn(digits: &str) -> bool {
    if digits.len() < 13 || digits.len() > 19 {
        return false;
    }

    let mut sum = 0u32;
    let mut double = false;

    for ch in digits.chars().rev() {
        let Some(digit) = ch.to_digit(10) else {
            return false;
        };

        let value = if double {
            let d = digit * 2;
            if d > 9 {
                d - 9
            } else {
                d
            }
        } else {
            digit
        };

        sum += value;
        double = !double;
    }

    {
        sum % 10 == 0
    }
}

/// Validate that a string is a valid IPv4 address.
fn is_valid_ipv4(ip: &str) -> bool {
    let parts: Vec<&str> = ip.split('.').collect();
    if parts.len() != 4 {
        return false;
    }
    parts.iter().all(|part| part.parse::<u8>().is_ok())
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn detect_email() {
        let masker = DataMasker::new(true);
        let text = "Contact zeroclaw_user@example.com for info";
        let matches = masker.detect(text);
        assert!(matches.iter().any(|m| m.category == PiiCategory::Email));
    }

    #[test]
    fn mask_email_preserves_domain() {
        let masked = mask_email("zeroclaw_user@example.com");
        assert_eq!(masked, "z***@example.com");
    }

    #[test]
    fn detect_korean_phone() {
        let masker = DataMasker::new(true);
        let text = "Call 010-1234-5678 for support";
        let matches = masker.detect(text);
        assert!(matches.iter().any(|m| m.category == PiiCategory::Phone));
    }

    #[test]
    fn detect_credit_card_with_luhn() {
        let masker = DataMasker::new(true);
        // Valid Luhn test number (Visa test card)
        let text = "Card: 4111 1111 1111 1111";
        let matches = masker.detect(text);
        assert!(
            matches
                .iter()
                .any(|m| m.category == PiiCategory::CreditCard),
            "Should detect valid Luhn credit card number"
        );

        // Invalid Luhn number should NOT be detected as credit card
        let text2 = "Number: 1234 5678 9012 3456";
        let matches2 = masker.detect(text2);
        assert!(
            !matches2
                .iter()
                .any(|m| m.category == PiiCategory::CreditCard),
            "Should not detect invalid Luhn number as credit card"
        );
    }

    #[test]
    fn detect_ipv4() {
        let masker = DataMasker::new(true);
        let text = "Server at 192.168.1.100";
        let matches = masker.detect(text);
        assert!(matches.iter().any(|m| m.category == PiiCategory::IpAddress));
    }

    #[test]
    fn mask_ipv4_shows_first_octet() {
        let masked = mask_ipv4("192.168.1.100");
        assert_eq!(masked, "192.***.***.***");
    }

    #[test]
    fn mask_text_replaces_pii() {
        let masker = DataMasker::new(true);
        let text = "Email zeroclaw_user@example.com phone 010-1234-5678";
        let masked = masker.mask_text(text);
        assert!(!masked.contains("zeroclaw_user@example.com"));
        assert!(!masked.contains("010-1234-5678"));
    }

    #[test]
    fn mask_text_preserves_non_pii() {
        let masker = DataMasker::new(true);
        let text = "Hello world, this is just regular text.";
        let masked = masker.mask_text(text);
        assert_eq!(masked, text);
    }

    #[test]
    fn disabled_masker_returns_original() {
        let masker = DataMasker::new(false);
        let text = "Email zeroclaw_user@example.com";
        let masked = masker.mask_text(text);
        assert_eq!(masked, text);
    }

    #[test]
    fn contains_pii_detects() {
        let masker = DataMasker::new(true);
        assert!(masker.contains_pii("Send to zeroclaw_user@example.com"));
        assert!(!masker.contains_pii("Just a regular message"));
    }

    #[test]
    fn luhn_valid_test_numbers() {
        // Standard test card numbers
        assert!(is_valid_luhn("4111111111111111")); // Visa test
        assert!(is_valid_luhn("5500000000000004")); // MC test
    }

    #[test]
    fn luhn_invalid_numbers() {
        assert!(!is_valid_luhn("1234567890123456"));
        assert!(!is_valid_luhn("0000000000000001"));
        assert!(!is_valid_luhn("123")); // Too short
    }

    #[test]
    fn valid_ipv4() {
        assert!(is_valid_ipv4("192.168.1.1"));
        assert!(is_valid_ipv4("10.0.0.1"));
        assert!(is_valid_ipv4("0.0.0.0"));
        assert!(is_valid_ipv4("255.255.255.255"));
    }

    #[test]
    fn invalid_ipv4() {
        assert!(!is_valid_ipv4("256.0.0.1"));
        assert!(!is_valid_ipv4("192.168.1"));
        assert!(!is_valid_ipv4("not.an.ip.addr"));
    }

    #[test]
    fn detect_api_key_bearer() {
        let masker = DataMasker::new(true);
        let text = "Authorization: Bearer sk_test_placeholder_token_value_here";
        let matches = masker.detect(text);
        assert!(matches.iter().any(|m| m.category == PiiCategory::ApiKey));
    }

    #[test]
    fn mask_credit_card_shows_last_4() {
        let masked = mask_credit_card("4111-1111-1111-1111");
        assert_eq!(masked, "****-****-****-1111");
    }

    #[test]
    fn custom_mask_string() {
        let masker = DataMasker::with_mask(true, "[REDACTED]".into());
        let text = "Call 010-1234-5678";
        let masked = masker.mask_text(text);
        assert!(masked.contains("[REDACTED]"));
    }
}
