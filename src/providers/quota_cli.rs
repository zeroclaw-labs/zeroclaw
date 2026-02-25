//! CLI for displaying provider quota and rate limit status.

use super::health::ProviderHealthTracker;
use super::quota_types::{ProfileQuotaInfo, ProviderQuotaInfo, QuotaStatus, QuotaSummary};
use crate::auth::profiles::{AuthProfilesData, AuthProfilesStore};
use crate::config::Config;
use anyhow::{bail, Context, Result};
use chrono::Utc;
use std::collections::HashMap;
use std::time::Duration;

/// Run the `providers-quota` CLI command.
///
/// Combines provider health state from circuit breaker with OAuth profile metadata
/// to show comprehensive quota information.
pub async fn run(config: &Config, provider_filter: Option<&str>, format: &str) -> Result<()> {
    // 1. Initialize health tracker (same default settings as used in reliable.rs)
    let failure_threshold = 3; // Default from reliable.rs
    let cooldown_secs = 60; // Default from reliable.rs
    let health_tracker = ProviderHealthTracker::new(
        failure_threshold,
        Duration::from_secs(cooldown_secs),
        100, // max tracked providers
    );

    // 2. Load OAuth profiles
    let auth_store = AuthProfilesStore::new(&config.workspace_dir, config.secrets.encrypt);
    let profiles_data = auth_store
        .load()
        .await
        .context("Failed to load auth profiles")?;

    // 3. Build quota summary
    let summary = build_quota_summary(&health_tracker, &profiles_data, provider_filter)?;

    // 4. Format and print output
    match format {
        "json" => print_json(&summary)?,
        "text" => print_text(&summary)?,
        other => bail!("Invalid format: {other}. Use 'text' or 'json'."),
    }

    Ok(())
}

/// Build quota summary by combining health tracker state and OAuth profile metadata.
pub fn build_quota_summary(
    health_tracker: &ProviderHealthTracker,
    profiles_data: &AuthProfilesData,
    provider_filter: Option<&str>,
) -> Result<QuotaSummary> {
    let health_states = health_tracker.get_all_states();

    // Group profiles by provider
    let mut profiles_by_provider: HashMap<String, Vec<ProfileQuotaInfo>> = HashMap::new();
    for profile in profiles_data.profiles.values() {
        let provider = &profile.provider;

        // Skip if filter doesn't match
        if let Some(filter) = provider_filter {
            if provider != filter {
                continue;
            }
        }

        // Extract quota metadata from profile.metadata
        let rate_limit_remaining = profile
            .metadata
            .get("rate_limit_remaining")
            .and_then(|v| v.parse::<u64>().ok());

        let rate_limit_reset_at = profile
            .metadata
            .get("rate_limit_reset_at")
            .and_then(|v| chrono::DateTime::parse_from_rfc3339(v).ok())
            .map(|dt| dt.with_timezone(&Utc));

        let rate_limit_total = profile
            .metadata
            .get("rate_limit_total")
            .and_then(|v| v.parse::<u64>().ok());

        // Determine profile status
        let profile_status = if let Some(remaining) = rate_limit_remaining {
            if remaining == 0 {
                QuotaStatus::QuotaExhausted
            } else if remaining < 10 {
                QuotaStatus::RateLimited
            } else {
                QuotaStatus::Ok
            }
        } else {
            // No quota metadata available - assume OK
            QuotaStatus::Ok
        };

        profiles_by_provider
            .entry(provider.clone())
            .or_default()
            .push(ProfileQuotaInfo {
                profile_name: profile.profile_name.clone(),
                status: profile_status,
                rate_limit_remaining,
                rate_limit_reset_at,
                rate_limit_total,
            });
    }

    // Build provider quota info combining health + profiles
    let mut providers_info = Vec::new();

    // First, add providers that have health state
    for (provider_name, health_state) in health_states {
        // Skip if filter doesn't match
        if let Some(filter) = provider_filter {
            if provider_name != filter {
                continue;
            }
        }

        let profiles = profiles_by_provider
            .remove(&provider_name)
            .unwrap_or_default();

        // Determine overall provider status
        let status = if health_state.failure_count >= 3 {
            QuotaStatus::CircuitOpen
        } else if profiles
            .iter()
            .any(|p| p.status == QuotaStatus::QuotaExhausted)
        {
            QuotaStatus::QuotaExhausted
        } else if profiles
            .iter()
            .any(|p| p.status == QuotaStatus::RateLimited)
        {
            QuotaStatus::RateLimited
        } else {
            QuotaStatus::Ok
        };

        // Parse retry-after from last error (if available)
        let retry_after_seconds = health_state
            .last_error
            .as_ref()
            .and_then(|err| parse_retry_after_from_error(err));

        // Estimate circuit reset time based on cooldown
        let circuit_resets_at = if health_state.failure_count >= 3 {
            // Circuit is likely open; estimate reset time
            // (Note: actual reset time depends on when last failure occurred)
            Some(Utc::now() + chrono::Duration::seconds(60)) // Assume 60s default
        } else {
            None
        };

        providers_info.push(ProviderQuotaInfo {
            provider: provider_name.clone(),
            status,
            failure_count: health_state.failure_count,
            last_error: health_state.last_error,
            retry_after_seconds,
            circuit_resets_at,
            profiles,
        });
    }

    // Add remaining providers that have profiles but no health state
    for (provider_name, profiles) in profiles_by_provider {
        let status = if profiles
            .iter()
            .any(|p| p.status == QuotaStatus::QuotaExhausted)
        {
            QuotaStatus::QuotaExhausted
        } else if profiles
            .iter()
            .any(|p| p.status == QuotaStatus::RateLimited)
        {
            QuotaStatus::RateLimited
        } else {
            QuotaStatus::Ok
        };

        providers_info.push(ProviderQuotaInfo {
            provider: provider_name,
            status,
            failure_count: 0,
            last_error: None,
            retry_after_seconds: None,
            circuit_resets_at: None,
            profiles,
        });
    }

    // Add Qwen OAuth static quota info if configured and not already present
    add_qwen_oauth_static_quota(&mut providers_info, provider_filter)?;

    // Sort by provider name
    providers_info.sort_by(|a, b| a.provider.cmp(&b.provider));

    Ok(QuotaSummary {
        timestamp: Utc::now(),
        providers: providers_info,
    })
}

/// Parse retry-after duration from error message (fallback heuristic).
fn parse_retry_after_from_error(error: &str) -> Option<u64> {
    if error.contains("retry after") || error.contains("Retry after") {
        // Try to extract number from error message
        error.split_whitespace().find_map(|word| {
            word.trim_matches(|c: char| !c.is_numeric())
                .parse::<u64>()
                .ok()
        })
    } else {
        None
    }
}

/// Print quota summary in JSON format.
fn print_json(summary: &QuotaSummary) -> Result<()> {
    let json = serde_json::to_string_pretty(summary)
        .context("Failed to serialize quota summary to JSON")?;
    println!("{json}");
    Ok(())
}

/// Print quota summary in human-readable text format with a table.
fn print_text(summary: &QuotaSummary) -> Result<()> {
    println!(
        "\nProvider Quota Status ({0})\n",
        summary.timestamp.format("%Y-%m-%d %H:%M:%S UTC")
    );

    if summary.providers.is_empty() {
        println!("No provider quota information available.");
        println!("\nHint: Quota information is populated after API calls or when OAuth profiles are configured.");
        return Ok(());
    }

    // Print table header
    println!(
        "┌────────────────────┬────────────────┬────────┬──────────────────┬─────────────────────┐"
    );
    println!(
        "│ Provider           │ Status         │ Errors │ Retry After (s)  │ Circuit Resets At   │"
    );
    println!(
        "├────────────────────┼────────────────┼────────┼──────────────────┼─────────────────────┤"
    );

    for provider_info in &summary.providers {
        let status_str = format_status(&provider_info.status);
        let retry_str = provider_info
            .retry_after_seconds
            .map(|s| s.to_string())
            .unwrap_or_else(|| "-".to_string());
        let circuit_str = provider_info
            .circuit_resets_at
            .map(format_relative_time)
            .unwrap_or_else(|| "-".to_string());

        println!(
            "│ {:<18} │ {:<14} │ {:>6} │ {:>16} │ {:>19} │",
            truncate(&provider_info.provider, 18),
            truncate(&status_str, 14),
            provider_info.failure_count,
            truncate(&retry_str, 16),
            truncate(&circuit_str, 19)
        );

        // Print profile details (if any)
        for profile in &provider_info.profiles {
            let profile_status = format_status(&profile.status);
            let remaining_str = match (profile.rate_limit_remaining, profile.rate_limit_total) {
                (Some(r), Some(total)) => format!("{r}/{total}"),
                (Some(r), None) => format!("{r}"),
                (None, Some(total)) => format!("?/{total}"), // Static quota (total known, remaining unknown)
                (None, None) => "-".to_string(),
            };

            let reset_str = profile
                .rate_limit_reset_at
                .map(format_relative_time)
                .unwrap_or_else(|| "-".to_string());

            println!(
                "│   ├─ {:<15} │ {:<14} │ {:>6} │ {:>16} │ {:>19} │",
                truncate(&profile.profile_name, 15),
                truncate(&profile_status, 14),
                "-",
                truncate(&remaining_str, 16),
                truncate(&reset_str, 19)
            );
        }
    }

    println!("└────────────────────┴────────────────┴────────┴──────────────────┴─────────────────────┘\n");

    // Print summary footer
    let available = summary.available_providers();
    let rate_limited = summary.rate_limited_providers();
    let circuit_open = summary.circuit_open_providers();

    if !available.is_empty() {
        println!("✅ Available providers: {}", available.join(", "));
    }
    if !rate_limited.is_empty() {
        println!("⚠️  Rate-limited providers: {}", rate_limited.join(", "));
    }
    if !circuit_open.is_empty() {
        println!("❌ Circuit open providers: {}", circuit_open.join(", "));
    }

    if rate_limited.is_empty() && circuit_open.is_empty() {
        println!("\n✅ All providers are healthy.");
    }

    Ok(())
}

/// Format quota status with emoji.
fn format_status(status: &QuotaStatus) -> String {
    match status {
        QuotaStatus::Ok => "✅ ok".to_string(),
        QuotaStatus::RateLimited => "⚠️  rate_limited".to_string(),
        QuotaStatus::CircuitOpen => "❌ circuit_open".to_string(),
        QuotaStatus::QuotaExhausted => "🚫 quota_exhausted".to_string(),
    }
}

/// Format relative time (e.g., "in 2h 30m" or "5 minutes ago").
fn format_relative_time(dt: chrono::DateTime<Utc>) -> String {
    let now = Utc::now();
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

/// Truncate string to max length with ellipsis.
fn truncate(s: &str, max_len: usize) -> String {
    if s.len() <= max_len {
        s.to_string()
    } else if max_len >= 3 {
        format!("{}...", &s[..max_len - 3])
    } else {
        s[..max_len].to_string()
    }
}

/// Add Qwen OAuth static quota information if ~/.qwen/oauth_creds.json exists.
///
/// Qwen OAuth free tier has a known limit of 1000 requests/day, but the API
/// doesn't return rate limit headers. This function provides static quota info.
fn add_qwen_oauth_static_quota(
    providers_info: &mut Vec<ProviderQuotaInfo>,
    provider_filter: Option<&str>,
) -> Result<()> {
    // Check if qwen-code or qwen-oauth is requested
    let qwen_aliases = ["qwen", "qwen-code", "qwen-oauth", "qwen_oauth", "dashscope"];
    let should_add_qwen = provider_filter
        .map(|f| qwen_aliases.contains(&f))
        .unwrap_or(true); // If no filter, always try to add

    if !should_add_qwen {
        return Ok(());
    }

    // Check if Qwen OAuth credentials exist
    let home_dir = std::env::var("HOME").context("HOME environment variable not set")?;
    let qwen_creds_path = std::path::Path::new(&home_dir).join(".qwen/oauth_creds.json");

    if !qwen_creds_path.exists() {
        return Ok(()); // No Qwen OAuth configured
    }

    // Check if qwen provider already exists in providers_info
    let qwen_exists = providers_info
        .iter()
        .any(|p| qwen_aliases.contains(&p.provider.as_str()));

    if qwen_exists {
        return Ok(()); // Already added
    }

    // Add static quota info for Qwen OAuth
    providers_info.push(ProviderQuotaInfo {
        provider: "qwen-code".to_string(),
        status: QuotaStatus::Ok,
        failure_count: 0,
        last_error: None,
        retry_after_seconds: None,
        circuit_resets_at: None,
        profiles: vec![ProfileQuotaInfo {
            profile_name: "OAuth (portal.qwen.ai)".to_string(),
            status: QuotaStatus::Ok,
            rate_limit_remaining: None, // Unknown without local tracking
            rate_limit_reset_at: None,  // Daily reset (exact time unknown)
            rate_limit_total: Some(1000), // OAuth free tier limit
        }],
    });

    Ok(())
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn test_format_relative_time_future() {
        let future = Utc::now() + chrono::Duration::seconds(3700);
        let formatted = format_relative_time(future);
        assert!(formatted.contains("in"));
        assert!(formatted.contains('h'));
    }

    #[test]
    fn test_format_relative_time_past() {
        let past = Utc::now() - chrono::Duration::seconds(300);
        let formatted = format_relative_time(past);
        assert!(formatted.contains("ago"));
    }

    #[test]
    fn test_truncate() {
        assert_eq!(truncate("hello", 10), "hello");
        assert_eq!(truncate("hello world", 8), "hello...");
        assert_eq!(truncate("hi", 5), "hi");
    }

    #[test]
    fn test_parse_retry_after() {
        assert_eq!(
            parse_retry_after_from_error("retry after 60 seconds"),
            Some(60)
        );
        assert_eq!(
            parse_retry_after_from_error("Please retry after 120s"),
            Some(120)
        );
        assert_eq!(parse_retry_after_from_error("No retry info"), None);
    }
}
