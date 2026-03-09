//! Universal quota extractor for different provider rate limit mechanisms.
//!
//! Each provider has its own rate limit header format:
//! - OpenAI: `X-RateLimit-Remaining`, `X-RateLimit-Reset`
//! - Anthropic: `anthropic-ratelimit-requests-remaining`, `retry-after`
//! - Gemini: `X-Goog-RateLimit-Requests-Remaining`
//! - Custom/generic: May not return headers at all
//!
//! This module provides a universal adapter with provider-specific extractors
//! and fallback chains to handle different mechanisms gracefully.

use super::quota_types::QuotaMetadata;
use chrono::{DateTime, Utc};
use reqwest::header::HeaderMap;
use std::collections::HashMap;

/// Trait for extracting quota metadata from provider responses.
pub trait QuotaExtractor: Send + Sync {
    /// Extract quota metadata from HTTP response headers
    fn extract_from_headers(&self, headers: &HeaderMap) -> Option<QuotaMetadata>;

    /// Extract quota metadata from error messages (fallback)
    fn extract_from_error(&self, error: &anyhow::Error) -> Option<QuotaMetadata>;
}

/// OpenAI-compatible quota extractor
pub struct OpenAIQuotaExtractor;

impl QuotaExtractor for OpenAIQuotaExtractor {
    fn extract_from_headers(&self, headers: &HeaderMap) -> Option<QuotaMetadata> {
        let rate_limit_remaining = headers
            .get("X-RateLimit-Remaining")
            .or_else(|| headers.get("x-ratelimit-remaining"))
            .and_then(|v| v.to_str().ok())
            .and_then(|s| s.parse::<u64>().ok());

        let rate_limit_total = headers
            .get("X-RateLimit-Limit")
            .or_else(|| headers.get("x-ratelimit-limit"))
            .and_then(|v| v.to_str().ok())
            .and_then(|s| s.parse::<u64>().ok());

        let rate_limit_reset_at = headers
            .get("X-RateLimit-Reset")
            .or_else(|| headers.get("x-ratelimit-reset"))
            .and_then(|v| v.to_str().ok())
            .and_then(|s| s.parse::<i64>().ok())
            .and_then(|ts| DateTime::from_timestamp(ts, 0));

        let retry_after_seconds = headers
            .get("Retry-After")
            .or_else(|| headers.get("retry-after"))
            .and_then(|v| v.to_str().ok())
            .and_then(|s| s.parse::<u64>().ok());

        if rate_limit_remaining.is_some()
            || rate_limit_reset_at.is_some()
            || retry_after_seconds.is_some()
        {
            Some(QuotaMetadata {
                rate_limit_remaining,
                rate_limit_reset_at,
                retry_after_seconds,
                rate_limit_total,
            })
        } else {
            None
        }
    }

    fn extract_from_error(&self, error: &anyhow::Error) -> Option<QuotaMetadata> {
        let error_str = error.to_string();

        // Parse "retry after X seconds" from error message
        let retry_after_seconds =
            if error_str.contains("retry after") || error_str.contains("Retry after") {
                error_str
                    .split_whitespace()
                    .find_map(|word| word.parse::<u64>().ok())
            } else {
                None
            };

        if retry_after_seconds.is_some() {
            Some(QuotaMetadata {
                rate_limit_remaining: Some(0),
                rate_limit_reset_at: None,
                retry_after_seconds,
                rate_limit_total: None,
            })
        } else {
            None
        }
    }
}

/// Anthropic Claude API quota extractor
pub struct AnthropicQuotaExtractor;

impl QuotaExtractor for AnthropicQuotaExtractor {
    fn extract_from_headers(&self, headers: &HeaderMap) -> Option<QuotaMetadata> {
        let rate_limit_remaining = headers
            .get("anthropic-ratelimit-requests-remaining")
            .or_else(|| headers.get("Anthropic-RateLimit-Requests-Remaining"))
            .and_then(|v| v.to_str().ok())
            .and_then(|s| s.parse::<u64>().ok());

        let rate_limit_total = headers
            .get("anthropic-ratelimit-requests-limit")
            .or_else(|| headers.get("Anthropic-RateLimit-Requests-Limit"))
            .and_then(|v| v.to_str().ok())
            .and_then(|s| s.parse::<u64>().ok());

        let rate_limit_reset_at = headers
            .get("anthropic-ratelimit-requests-reset")
            .or_else(|| headers.get("Anthropic-RateLimit-Requests-Reset"))
            .and_then(|v| v.to_str().ok())
            .and_then(|s| DateTime::parse_from_rfc3339(s).ok())
            .map(|dt| dt.with_timezone(&Utc));

        let retry_after_seconds = headers
            .get("retry-after")
            .or_else(|| headers.get("Retry-After"))
            .and_then(|v| v.to_str().ok())
            .and_then(|s| s.parse::<u64>().ok());

        if rate_limit_remaining.is_some()
            || rate_limit_reset_at.is_some()
            || retry_after_seconds.is_some()
        {
            Some(QuotaMetadata {
                rate_limit_remaining,
                rate_limit_reset_at,
                retry_after_seconds,
                rate_limit_total,
            })
        } else {
            None
        }
    }

    fn extract_from_error(&self, error: &anyhow::Error) -> Option<QuotaMetadata> {
        // Anthropic errors may contain retry-after info in message
        let error_str = error.to_string().to_lowercase();

        if error_str.contains("overloaded") || error_str.contains("rate limit") {
            Some(QuotaMetadata {
                rate_limit_remaining: Some(0),
                rate_limit_reset_at: None,
                retry_after_seconds: Some(60), // Default 60s backoff
                rate_limit_total: None,
            })
        } else {
            None
        }
    }
}

/// Google Gemini API quota extractor
pub struct GeminiQuotaExtractor;

impl QuotaExtractor for GeminiQuotaExtractor {
    fn extract_from_headers(&self, headers: &HeaderMap) -> Option<QuotaMetadata> {
        let rate_limit_remaining = headers
            .get("X-Goog-RateLimit-Requests-Remaining")
            .or_else(|| headers.get("x-goog-ratelimit-requests-remaining"))
            .and_then(|v| v.to_str().ok())
            .and_then(|s| s.parse::<u64>().ok());

        let rate_limit_total = headers
            .get("X-Goog-RateLimit-Requests-Limit")
            .or_else(|| headers.get("x-goog-ratelimit-requests-limit"))
            .and_then(|v| v.to_str().ok())
            .and_then(|s| s.parse::<u64>().ok());

        let retry_after_seconds = headers
            .get("Retry-After")
            .or_else(|| headers.get("retry-after"))
            .and_then(|v| v.to_str().ok())
            .and_then(|s| s.parse::<u64>().ok());

        if rate_limit_remaining.is_some() || retry_after_seconds.is_some() {
            Some(QuotaMetadata {
                rate_limit_remaining,
                rate_limit_reset_at: None, // Gemini may not provide reset timestamp
                retry_after_seconds,
                rate_limit_total,
            })
        } else {
            None
        }
    }

    fn extract_from_error(&self, error: &anyhow::Error) -> Option<QuotaMetadata> {
        let error_str = error.to_string();

        // Gemini may return "RESOURCE_EXHAUSTED" or "insufficient quota"
        if error_str.contains("RESOURCE_EXHAUSTED") || error_str.contains("insufficient quota") {
            Some(QuotaMetadata {
                rate_limit_remaining: Some(0),
                rate_limit_reset_at: None,
                retry_after_seconds: Some(3600), // 1 hour default for quota exhaustion
                rate_limit_total: None,
            })
        } else {
            None
        }
    }
}

/// Qwen OAuth API quota extractor
///
/// Qwen OAuth API (portal.qwen.ai) does not return rate limit headers.
/// OAuth free tier has a known limit of 1000 requests/day.
/// This extractor provides error parsing for rate limit detection.
pub struct QwenQuotaExtractor;

impl QuotaExtractor for QwenQuotaExtractor {
    fn extract_from_headers(&self, _headers: &HeaderMap) -> Option<QuotaMetadata> {
        // Qwen OAuth API doesn't return rate limit headers
        // Return None to avoid breaking fallback chain
        // Static quota info is handled by quota CLI separately
        None
    }

    fn extract_from_error(&self, error: &anyhow::Error) -> Option<QuotaMetadata> {
        let error_str = error.to_string().to_lowercase();

        // Qwen may return rate limit errors with "too many requests" or "rate limit"
        if error_str.contains("too many requests")
            || error_str.contains("rate limit")
            || error_str.contains("quota")
        {
            Some(QuotaMetadata {
                rate_limit_remaining: Some(0),
                rate_limit_reset_at: None,
                retry_after_seconds: Some(3600), // 1 hour default backoff
                rate_limit_total: Some(1000),    // OAuth free tier limit
            })
        } else {
            None
        }
    }
}

/// Universal quota extractor with provider-specific extractors and fallback chain.
pub struct UniversalQuotaExtractor {
    extractors: HashMap<String, Box<dyn QuotaExtractor>>,
}

impl UniversalQuotaExtractor {
    /// Create a new universal extractor with built-in provider support.
    pub fn new() -> Self {
        let mut extractors: HashMap<String, Box<dyn QuotaExtractor>> = HashMap::new();

        // Register provider-specific extractors
        extractors.insert("openai".to_string(), Box::new(OpenAIQuotaExtractor));
        extractors.insert("openai-codex".to_string(), Box::new(OpenAIQuotaExtractor));
        extractors.insert("anthropic".to_string(), Box::new(AnthropicQuotaExtractor));
        extractors.insert("gemini".to_string(), Box::new(GeminiQuotaExtractor));
        extractors.insert("openrouter".to_string(), Box::new(OpenAIQuotaExtractor)); // OpenRouter uses OpenAI format
        extractors.insert("qwen".to_string(), Box::new(QwenQuotaExtractor));
        extractors.insert("qwen-coding-plan".to_string(), Box::new(QwenQuotaExtractor));
        extractors.insert("qwen-code".to_string(), Box::new(QwenQuotaExtractor)); // OAuth alias
        extractors.insert("qwen-oauth".to_string(), Box::new(QwenQuotaExtractor)); // OAuth alias
        extractors.insert("dashscope".to_string(), Box::new(QwenQuotaExtractor)); // DashScope API key

        Self { extractors }
    }

    /// Extract quota metadata with provider-specific extractor and fallback chain.
    ///
    /// Tries:
    /// 1. Provider-specific extractor on headers
    /// 2. All extractors on headers (some providers use OpenAI-compatible format)
    /// 3. Provider-specific extractor on error message (if error provided)
    /// 4. All extractors on error message
    pub fn extract(
        &self,
        provider: &str,
        headers: &HeaderMap,
        error: Option<&anyhow::Error>,
    ) -> Option<QuotaMetadata> {
        // Try provider-specific extractor first
        if let Some(extractor) = self.extractors.get(provider) {
            if let Some(quota) = extractor.extract_from_headers(headers) {
                tracing::debug!(
                    provider = provider,
                    remaining = ?quota.rate_limit_remaining,
                    "Extracted quota from headers using provider-specific extractor"
                );
                return Some(quota);
            }
        }

        // Fallback: try all extractors (some providers use OpenAI-compatible headers)
        for (name, extractor) in &self.extractors {
            if name != provider {
                if let Some(quota) = extractor.extract_from_headers(headers) {
                    tracing::debug!(
                        provider = provider,
                        extractor = name,
                        remaining = ?quota.rate_limit_remaining,
                        "Extracted quota from headers using fallback extractor"
                    );
                    return Some(quota);
                }
            }
        }

        // Last resort: parse from error message
        if let Some(err) = error {
            if let Some(extractor) = self.extractors.get(provider) {
                if let Some(quota) = extractor.extract_from_error(err) {
                    tracing::debug!(
                        provider = provider,
                        "Extracted quota from error message using provider-specific extractor"
                    );
                    return Some(quota);
                }
            }

            // Try all error extractors
            for (name, extractor) in &self.extractors {
                if name != provider {
                    if let Some(quota) = extractor.extract_from_error(err) {
                        tracing::debug!(
                            provider = provider,
                            extractor = name,
                            "Extracted quota from error message using fallback extractor"
                        );
                        return Some(quota);
                    }
                }
            }
        }

        None
    }

    /// Register a custom quota extractor for a provider
    pub fn register_extractor(&mut self, provider: String, extractor: Box<dyn QuotaExtractor>) {
        self.extractors.insert(provider, extractor);
    }
}

impl Default for UniversalQuotaExtractor {
    fn default() -> Self {
        Self::new()
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn test_openai_extractor_headers() {
        let extractor = OpenAIQuotaExtractor;
        let mut headers = HeaderMap::new();
        headers.insert("X-RateLimit-Remaining", "10".parse().unwrap());
        headers.insert("X-RateLimit-Limit", "100".parse().unwrap());
        headers.insert("X-RateLimit-Reset", "1708718400".parse().unwrap());

        let quota = extractor.extract_from_headers(&headers).unwrap();
        assert_eq!(quota.rate_limit_remaining, Some(10));
        assert_eq!(quota.rate_limit_total, Some(100));
        assert!(quota.rate_limit_reset_at.is_some());
    }

    #[test]
    fn test_anthropic_extractor_headers() {
        let extractor = AnthropicQuotaExtractor;
        let mut headers = HeaderMap::new();
        headers.insert(
            "anthropic-ratelimit-requests-remaining",
            "50".parse().unwrap(),
        );
        headers.insert("retry-after", "30".parse().unwrap());

        let quota = extractor.extract_from_headers(&headers).unwrap();
        assert_eq!(quota.rate_limit_remaining, Some(50));
        assert_eq!(quota.retry_after_seconds, Some(30));
    }

    #[test]
    fn test_gemini_extractor_headers() {
        let extractor = GeminiQuotaExtractor;
        let mut headers = HeaderMap::new();
        headers.insert("X-Goog-RateLimit-Requests-Remaining", "20".parse().unwrap());
        headers.insert("X-Goog-RateLimit-Requests-Limit", "100".parse().unwrap());

        let quota = extractor.extract_from_headers(&headers).unwrap();
        assert_eq!(quota.rate_limit_remaining, Some(20));
        assert_eq!(quota.rate_limit_total, Some(100));
    }

    #[test]
    fn test_gemini_extractor_error() {
        let extractor = GeminiQuotaExtractor;
        let error = anyhow::anyhow!("gemini API error (429): RESOURCE_EXHAUSTED");

        let quota = extractor.extract_from_error(&error).unwrap();
        assert_eq!(quota.rate_limit_remaining, Some(0));
        assert_eq!(quota.retry_after_seconds, Some(3600));
    }

    #[test]
    fn test_universal_extractor_provider_specific() {
        let extractor = UniversalQuotaExtractor::new();
        let mut headers = HeaderMap::new();
        headers.insert("X-RateLimit-Remaining", "15".parse().unwrap());

        let quota = extractor.extract("openai", &headers, None).unwrap();
        assert_eq!(quota.rate_limit_remaining, Some(15));
    }

    #[test]
    fn test_universal_extractor_fallback() {
        let extractor = UniversalQuotaExtractor::new();
        let mut headers = HeaderMap::new();
        headers.insert("X-RateLimit-Remaining", "25".parse().unwrap());

        // Request for "custom-provider" should fallback to OpenAI extractor
        let quota = extractor
            .extract("custom-provider", &headers, None)
            .unwrap();
        assert_eq!(quota.rate_limit_remaining, Some(25));
    }

    #[test]
    fn test_universal_extractor_error_fallback() {
        let extractor = UniversalQuotaExtractor::new();
        let headers = HeaderMap::new();
        let error = anyhow::anyhow!("gemini API error: insufficient quota");

        let quota = extractor.extract("gemini", &headers, Some(&error)).unwrap();
        assert_eq!(quota.rate_limit_remaining, Some(0));
        assert!(quota.retry_after_seconds.is_some());
    }

    #[test]
    fn test_universal_extractor_no_match() {
        let extractor = UniversalQuotaExtractor::new();
        let headers = HeaderMap::new();

        let quota = extractor.extract("unknown-provider", &headers, None);
        assert!(quota.is_none());
    }

    #[test]
    fn test_qwen_extractor_headers() {
        let extractor = QwenQuotaExtractor;
        let headers = HeaderMap::new();

        // Qwen doesn't return rate limit headers
        let quota = extractor.extract_from_headers(&headers);
        assert!(quota.is_none());
    }

    #[test]
    fn test_qwen_extractor_error() {
        let extractor = QwenQuotaExtractor;
        let error = anyhow::anyhow!("qwen API error (429): Too many requests");

        let quota = extractor.extract_from_error(&error).unwrap();
        assert_eq!(quota.rate_limit_remaining, Some(0));
        assert_eq!(quota.rate_limit_total, Some(1000));
        assert!(quota.retry_after_seconds.is_some());
    }

    #[test]
    fn test_universal_extractor_qwen_error() {
        let extractor = UniversalQuotaExtractor::new();
        let headers = HeaderMap::new();
        let error = anyhow::anyhow!("qwen rate limit exceeded");

        // Test qwen-code alias with error
        let quota = extractor
            .extract("qwen-code", &headers, Some(&error))
            .unwrap();
        assert_eq!(quota.rate_limit_remaining, Some(0));
        assert_eq!(quota.rate_limit_total, Some(1000));
    }
}
