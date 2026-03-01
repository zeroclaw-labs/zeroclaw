//! Quota-aware agent loop helpers.
//!
//! This module provides utilities for the agent loop to:
//! - Check provider quota status before expensive operations
//! - Warn users when quota is running low
//! - Switch providers mid-conversation when requested via tools
//! - Handle rate limit errors with automatic fallback

use crate::auth::profiles::AuthProfilesStore;
use crate::config::Config;
use crate::providers::health::ProviderHealthTracker;
use crate::providers::quota_types::QuotaStatus;
use anyhow::Result;
use std::time::Duration;

/// Check if we should warn about low quota before an operation.
///
/// Returns `Some(warning_message)` if quota is running low (< 10% remaining).
pub async fn check_quota_warning(
    config: &Config,
    provider_name: &str,
    parallel_count: usize,
) -> Result<Option<String>> {
    if parallel_count < 5 {
        // Only warn for operations with 5+ parallel calls
        return Ok(None);
    }

    let health_tracker = ProviderHealthTracker::new(
        3,                       // failure_threshold
        Duration::from_secs(60), // cooldown
        100,                     // max tracked providers
    );

    let auth_store = AuthProfilesStore::new(&config.workspace_dir, config.secrets.encrypt);
    let profiles_data = auth_store.load().await?;

    let summary = crate::providers::quota_cli::build_quota_summary(
        &health_tracker,
        &profiles_data,
        Some(provider_name),
    )?;

    // Find the provider in summary
    if let Some(provider_info) = summary
        .providers
        .iter()
        .find(|p| p.provider == provider_name)
    {
        // Check circuit breaker status
        if provider_info.status == QuotaStatus::CircuitOpen {
            let reset_str = if let Some(resets_at) = provider_info.circuit_resets_at {
                format!(" (resets {})", format_relative_time(resets_at))
            } else {
                String::new()
            };

            return Ok(Some(format!(
                "⚠️ **Provider Unavailable**: {} is circuit-open{}. \
                 Consider switching to an alternative provider using the `check_provider_quota` tool.",
                provider_name, reset_str
            )));
        }

        // Check rate limit status
        if provider_info.status == QuotaStatus::RateLimited
            || provider_info.status == QuotaStatus::QuotaExhausted
        {
            return Ok(Some(format!(
                "⚠️ **Rate Limit Warning**: {} is rate-limited. \
                 Your parallel operation ({} calls) may fail. \
                 Consider switching to another provider using `check_provider_quota` and `switch_provider` tools.",
                provider_name, parallel_count
            )));
        }

        // Check individual profile quotas
        for profile in &provider_info.profiles {
            if let (Some(remaining), Some(total)) =
                (profile.rate_limit_remaining, profile.rate_limit_total)
            {
                let quota_pct = (remaining as f64 / total as f64) * 100.0;
                if quota_pct < 10.0 && remaining < parallel_count as u64 {
                    let reset_str = if let Some(reset_at) = profile.rate_limit_reset_at {
                        format!(" (resets {})", format_relative_time(reset_at))
                    } else {
                        String::new()
                    };

                    return Ok(Some(format!(
                        "⚠️ **Low Quota Warning**: {} profile '{}' has only {}/{} requests remaining ({:.0}%){}. \
                         Your operation requires {} calls. \
                         Consider: (1) reducing parallel operations, (2) switching providers, or (3) waiting for quota reset.",
                        provider_name,
                        profile.profile_name,
                        remaining,
                        total,
                        quota_pct,
                        reset_str,
                        parallel_count
                    )));
                }
            }
        }
    }

    Ok(None)
}

/// Parse switch_provider metadata from tool result output.
///
/// The `switch_provider` tool embeds JSON metadata in its output as:
/// `<!-- metadata: {...} -->`
///
/// Returns `Some((provider, model))` if a provider switch was requested.
pub fn parse_switch_provider_metadata(tool_output: &str) -> Option<(String, Option<String>)> {
    // Look for <!-- metadata: {...} --> pattern
    if let Some(start) = tool_output.find("<!-- metadata:") {
        if let Some(end) = tool_output[start..].find("-->") {
            let json_str = &tool_output[start + 14..start + end].trim();
            if let Ok(metadata) = serde_json::from_str::<serde_json::Value>(json_str) {
                if metadata.get("action").and_then(|v| v.as_str()) == Some("switch_provider") {
                    let provider = metadata
                        .get("provider")
                        .and_then(|v| v.as_str())
                        .map(String::from);
                    let model = metadata
                        .get("model")
                        .and_then(|v| v.as_str())
                        .map(String::from);

                    if let Some(p) = provider {
                        return Some((p, model));
                    }
                }
            }
        }
    }

    None
}

/// Format relative time (e.g., "in 2h 30m" or "5 minutes ago").
fn format_relative_time(dt: chrono::DateTime<chrono::Utc>) -> String {
    let now = chrono::Utc::now();
    let diff = dt.signed_duration_since(now);

    if diff.num_seconds() < 0 {
        // In the past
        let abs_diff = -diff;
        if abs_diff.num_hours() > 0 {
            format!("{}h ago", abs_diff.num_hours())
        } else if abs_diff.num_minutes() > 0 {
            format!("{}m ago", abs_diff.num_minutes())
        } else {
            format!("{}s ago", abs_diff.num_seconds())
        }
    } else {
        // In the future
        if diff.num_hours() > 0 {
            format!("in {}h {}m", diff.num_hours(), diff.num_minutes() % 60)
        } else if diff.num_minutes() > 0 {
            format!("in {}m", diff.num_minutes())
        } else {
            format!("in {}s", diff.num_seconds())
        }
    }
}

/// Find an available alternative provider when current provider is unavailable.
///
/// Returns the name of a healthy provider with available quota, or None if all are unavailable.
pub async fn find_available_provider(
    config: &Config,
    current_provider: &str,
) -> Result<Option<String>> {
    let health_tracker = ProviderHealthTracker::new(
        3,                       // failure_threshold
        Duration::from_secs(60), // cooldown
        100,                     // max tracked providers
    );

    let auth_store = AuthProfilesStore::new(&config.workspace_dir, config.secrets.encrypt);
    let profiles_data = auth_store.load().await?;

    let summary =
        crate::providers::quota_cli::build_quota_summary(&health_tracker, &profiles_data, None)?;

    // Find providers with Ok status (not current provider)
    for provider_info in &summary.providers {
        if provider_info.provider != current_provider && provider_info.status == QuotaStatus::Ok {
            return Ok(Some(provider_info.provider.clone()));
        }
    }

    Ok(None)
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn test_parse_switch_provider_metadata() {
        let output = "Switching to gemini.\n\n<!-- metadata: {\"action\":\"switch_provider\",\"provider\":\"gemini\",\"model\":null,\"reason\":\"user request\"} -->";
        let result = parse_switch_provider_metadata(output);
        assert_eq!(result, Some(("gemini".to_string(), None)));

        let output_with_model = "Switching to openai.\n\n<!-- metadata: {\"action\":\"switch_provider\",\"provider\":\"openai\",\"model\":\"gpt-4\",\"reason\":\"rate limit\"} -->";
        let result = parse_switch_provider_metadata(output_with_model);
        assert_eq!(
            result,
            Some(("openai".to_string(), Some("gpt-4".to_string())))
        );

        let no_metadata = "Just some regular tool output";
        assert_eq!(parse_switch_provider_metadata(no_metadata), None);
    }

    #[test]
    fn test_format_relative_time() {
        use chrono::{Duration, Utc};

        let future = Utc::now() + Duration::seconds(3700);
        let formatted = format_relative_time(future);
        assert!(formatted.contains("in"));
        assert!(formatted.contains('h'));

        let past = Utc::now() - Duration::seconds(300);
        let formatted = format_relative_time(past);
        assert!(formatted.contains("ago"));
    }
}
