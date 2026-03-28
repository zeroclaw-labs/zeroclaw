//! Shared types for quota and rate limit tracking.

use chrono::{DateTime, Utc};
use serde::{Deserialize, Serialize};

/// Quota metadata extracted from provider responses (HTTP headers or errors).
#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct QuotaMetadata {
    /// Number of requests remaining in current quota window
    pub rate_limit_remaining: Option<u64>,
    /// Timestamp when the rate limit resets (UTC)
    pub rate_limit_reset_at: Option<DateTime<Utc>>,
    /// Number of seconds to wait before retry (from Retry-After header)
    pub retry_after_seconds: Option<u64>,
    /// Maximum requests allowed in quota window (if available)
    pub rate_limit_total: Option<u64>,
}

/// Status of a provider's quota and circuit breaker state.
#[derive(Debug, Clone, Serialize, Deserialize, PartialEq, Eq)]
#[serde(rename_all = "snake_case")]
pub enum QuotaStatus {
    /// Provider is healthy and available
    Ok,
    /// Provider is rate-limited but circuit is still closed
    RateLimited,
    /// Circuit breaker is open (too many failures)
    CircuitOpen,
    /// OAuth profile quota exhausted
    QuotaExhausted,
}

/// Per-provider quota information combining health state and OAuth profile metadata.
#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct ProviderQuotaInfo {
    pub provider: String,
    pub status: QuotaStatus,
    pub failure_count: u32,
    pub last_error: Option<String>,
    pub retry_after_seconds: Option<u64>,
    pub circuit_resets_at: Option<DateTime<Utc>>,
    pub profiles: Vec<ProfileQuotaInfo>,
}

/// Per-OAuth-profile quota information.
#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct ProfileQuotaInfo {
    pub profile_name: String,
    pub status: QuotaStatus,
    pub rate_limit_remaining: Option<u64>,
    pub rate_limit_reset_at: Option<DateTime<Utc>>,
    pub rate_limit_total: Option<u64>,
    /// Account identifier (email, workspace ID, etc.)
    #[serde(skip_serializing_if = "Option::is_none")]
    pub account_id: Option<String>,
    /// When the OAuth token / subscription expires
    #[serde(skip_serializing_if = "Option::is_none")]
    pub token_expires_at: Option<DateTime<Utc>>,
    /// Plan type (free, pro, enterprise) if known
    #[serde(skip_serializing_if = "Option::is_none")]
    pub plan_type: Option<String>,
}

/// Summary of all providers' quota status.
#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct QuotaSummary {
    pub timestamp: DateTime<Utc>,
    pub providers: Vec<ProviderQuotaInfo>,
}

impl QuotaSummary {
    /// Get available (healthy) providers
    pub fn available_providers(&self) -> Vec<&str> {
        self.providers
            .iter()
            .filter(|p| p.status == QuotaStatus::Ok)
            .map(|p| p.provider.as_str())
            .collect()
    }

    /// Get rate-limited providers
    pub fn rate_limited_providers(&self) -> Vec<&str> {
        self.providers
            .iter()
            .filter(|p| {
                p.status == QuotaStatus::RateLimited || p.status == QuotaStatus::QuotaExhausted
            })
            .map(|p| p.provider.as_str())
            .collect()
    }

    /// Get circuit-open providers
    pub fn circuit_open_providers(&self) -> Vec<&str> {
        self.providers
            .iter()
            .filter(|p| p.status == QuotaStatus::CircuitOpen)
            .map(|p| p.provider.as_str())
            .collect()
    }
}

/// Provider usage metrics (tracked per request).
#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct ProviderUsageMetrics {
    pub provider: String,
    pub requests_today: u64,
    pub requests_session: u64,
    pub tokens_input_today: u64,
    pub tokens_output_today: u64,
    pub tokens_input_session: u64,
    pub tokens_output_session: u64,
    pub cost_usd_today: f64,
    pub cost_usd_session: f64,
    pub daily_request_limit: u64,
    pub daily_token_limit: u64,
    pub last_reset_at: DateTime<Utc>,
}

impl Default for ProviderUsageMetrics {
    fn default() -> Self {
        Self {
            provider: String::new(),
            requests_today: 0,
            requests_session: 0,
            tokens_input_today: 0,
            tokens_output_today: 0,
            tokens_input_session: 0,
            tokens_output_session: 0,
            cost_usd_today: 0.0,
            cost_usd_session: 0.0,
            daily_request_limit: 0,
            daily_token_limit: 0,
            last_reset_at: Utc::now(),
        }
    }
}

impl ProviderUsageMetrics {
    pub fn new(provider: &str) -> Self {
        Self {
            provider: provider.to_string(),
            ..Default::default()
        }
    }
}
