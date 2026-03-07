//! Error parsing utilities for extracting structured quota information from provider errors.

use chrono::{DateTime, Utc};
use serde::{Deserialize, Serialize};

/// OpenAI Codex error structure with quota information.
#[derive(Debug, Deserialize, Serialize)]
pub struct OpenAiCodexError {
    pub error: OpenAiCodexErrorDetail,
}

/// Detailed error information from OpenAI Codex.
#[derive(Debug, Deserialize, Serialize)]
pub struct OpenAiCodexErrorDetail {
    #[serde(rename = "type")]
    pub error_type: String, // "usage_limit_reached"
    pub message: String,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub plan_type: Option<String>, // "enterprise", "free", "pro"
    #[serde(skip_serializing_if = "Option::is_none")]
    pub resets_at: Option<i64>, // Unix timestamp
}

/// Extracted quota reset information from error responses.
#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct ErrorQuotaInfo {
    pub provider: String,
    pub error_type: String,
    pub plan_type: Option<String>,
    pub resets_at: Option<DateTime<Utc>>,
    pub message: String,
}

/// Try to parse structured error from OpenAI Codex.
pub fn parse_openai_codex_error(body: &str) -> Option<ErrorQuotaInfo> {
    if let Ok(error) = serde_json::from_str::<OpenAiCodexError>(body) {
        let resets_at = error
            .error
            .resets_at
            .and_then(|ts| DateTime::from_timestamp(ts, 0));

        return Some(ErrorQuotaInfo {
            provider: "openai-codex".to_string(),
            error_type: error.error.error_type,
            plan_type: error.error.plan_type,
            resets_at,
            message: error.error.message,
        });
    }
    None
}

/// Generic error parser that tries multiple provider formats.
pub fn parse_provider_error(provider: &str, body: &str) -> Option<ErrorQuotaInfo> {
    // Try OpenAI Codex format
    if provider.contains("codex") || provider.contains("openai") {
        if let Some(info) = parse_openai_codex_error(body) {
            return Some(info);
        }
    }

    // Future: add parsers for other providers (Anthropic, Gemini, etc.)
    None
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn test_parse_openai_codex_usage_limit() {
        let json = r#"{
            "error": {
                "type": "usage_limit_reached",
                "message": "The usage limit has been reached",
                "plan_type": "enterprise",
                "resets_at": 1772087057
            }
        }"#;

        let info = parse_openai_codex_error(json).expect("Failed to parse");
        assert_eq!(info.error_type, "usage_limit_reached");
        assert_eq!(info.plan_type, Some("enterprise".to_string()));
        assert!(info.resets_at.is_some());
        assert_eq!(info.message, "The usage limit has been reached");
    }

    #[test]
    fn test_parse_openai_codex_minimal() {
        let json = r#"{
            "error": {
                "type": "rate_limit_exceeded",
                "message": "Too many requests"
            }
        }"#;

        let info = parse_openai_codex_error(json).expect("Failed to parse");
        assert_eq!(info.error_type, "rate_limit_exceeded");
        assert!(info.plan_type.is_none());
        assert!(info.resets_at.is_none());
    }

    #[test]
    fn test_parse_invalid_json() {
        let result = parse_openai_codex_error("not json");
        assert!(result.is_none());
    }
}
