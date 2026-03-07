//! Built-in tools for quota monitoring and provider management.
//!
//! These tools allow the agent to:
//! - Check quota status conversationally
//! - Switch providers when rate limited
//! - Estimate quota costs before operations
//! - Report usage metrics to the user

use crate::auth::profiles::AuthProfilesStore;
use crate::config::Config;
use crate::cost::tracker::CostTracker;
use crate::providers::health::ProviderHealthTracker;
use crate::providers::quota_types::{QuotaStatus, QuotaSummary};
use crate::tools::{Tool, ToolResult};
use anyhow::Result;
use async_trait::async_trait;
use serde_json::json;
use std::fmt::Write as _;
use std::sync::Arc;
use std::time::Duration;

/// Tool for checking provider quota status.
///
/// Allows agent to query: "какие модели доступны?" or "what providers have quota?"
pub struct CheckProviderQuotaTool {
    config: Arc<Config>,
    cost_tracker: Option<Arc<CostTracker>>,
}

impl CheckProviderQuotaTool {
    pub fn new(config: Arc<Config>) -> Self {
        Self {
            config,
            cost_tracker: None,
        }
    }

    pub fn with_cost_tracker(mut self, tracker: Arc<CostTracker>) -> Self {
        self.cost_tracker = Some(tracker);
        self
    }

    async fn build_quota_summary(&self, provider_filter: Option<&str>) -> Result<QuotaSummary> {
        // Fresh tracker on each call: provides a point-in-time snapshot of
        // provider health, not persistent state. This is intentional — the tool
        // reports quota/profile data from OAuth profiles, not cumulative circuit
        // breaker state (which lives in ReliableProvider's own tracker).
        let health_tracker = ProviderHealthTracker::new(
            3,                       // failure_threshold
            Duration::from_secs(60), // cooldown
            100,                     // max tracked providers
        );

        // Load OAuth profiles (state_dir = config dir parent, where auth-profiles.json lives)
        let state_dir = crate::auth::state_dir_from_config(&self.config);
        let auth_store = AuthProfilesStore::new(&state_dir, self.config.secrets.encrypt);
        let profiles_data = auth_store.load().await?;

        // Build quota summary using quota_cli logic
        crate::providers::quota_cli::build_quota_summary(
            &health_tracker,
            &profiles_data,
            provider_filter,
        )
    }
}

#[async_trait]
impl Tool for CheckProviderQuotaTool {
    fn name(&self) -> &str {
        "check_provider_quota"
    }

    fn description(&self) -> &str {
        "Check current rate limit and quota status for AI providers. \
         Returns available providers, rate-limited providers, quota remaining, \
         and estimated reset time. Use this when user asks about model availability \
         or when you encounter rate limit errors."
    }

    fn parameters_schema(&self) -> serde_json::Value {
        json!({
            "type": "object",
            "properties": {
                "provider": {
                    "type": "string",
                    "description": "Specific provider to check (optional). Examples: openai, gemini, anthropic. If omitted, checks all providers."
                }
            }
        })
    }

    async fn execute(&self, args: serde_json::Value) -> Result<ToolResult> {
        use std::fmt::Write;
        let provider_filter = args.get("provider").and_then(|v| v.as_str());

        let summary = self.build_quota_summary(provider_filter).await?;

        // Format result for agent
        let available = summary.available_providers();
        let rate_limited = summary.rate_limited_providers();
        let circuit_open = summary.circuit_open_providers();

        let mut output = String::new();
        let _ = write!(
            output,
            "Quota Status ({})\n\n",
            summary.timestamp.format("%Y-%m-%d %H:%M UTC")
        );

        if !available.is_empty() {
            let _ = writeln!(output, "Available providers: {}", available.join(", "));
        }
        if !rate_limited.is_empty() {
            let _ = writeln!(
                output,
                "Rate-limited providers: {}",
                rate_limited.join(", ")
            );
        }
        if !circuit_open.is_empty() {
            let _ = writeln!(
                output,
                "Circuit-open providers: {}",
                circuit_open.join(", ")
            );
        }

        if available.is_empty() && rate_limited.is_empty() && circuit_open.is_empty() {
            output
                .push_str("No quota information available. Quota is populated after API calls.\n");
        }

        // Always show per-provider and per-profile details
        for provider_info in &summary.providers {
            let status_label = match &provider_info.status {
                QuotaStatus::Ok => "ok",
                QuotaStatus::RateLimited => "rate-limited",
                QuotaStatus::CircuitOpen => "circuit-open",
                QuotaStatus::QuotaExhausted => "quota-exhausted",
            };
            let _ = write!(
                output,
                "\n{} (status: {})\n",
                provider_info.provider, status_label
            );

            if provider_info.failure_count > 0 {
                let _ = writeln!(output, "   Failures: {}", provider_info.failure_count);
            }
            if let Some(retry_after) = provider_info.retry_after_seconds {
                let _ = writeln!(output, "   Retry after: {}s", retry_after);
            }
            if let Some(ref err) = provider_info.last_error {
                let truncated = if err.len() > 120 { &err[..120] } else { err };
                let _ = writeln!(output, "   Last error: {}", truncated);
            }

            for profile in &provider_info.profiles {
                let _ = write!(output, "   - {}", profile.profile_name);
                if let Some(ref acct) = profile.account_id {
                    let _ = write!(output, " ({})", acct);
                }
                output.push('\n');

                if let Some(remaining) = profile.rate_limit_remaining {
                    if let Some(total) = profile.rate_limit_total {
                        let _ = writeln!(output, "     Quota: {}/{} requests", remaining, total);
                    } else {
                        let _ = writeln!(output, "     Quota: {} remaining", remaining);
                    }
                }
                if let Some(reset_at) = profile.rate_limit_reset_at {
                    let _ = writeln!(
                        output,
                        "     Resets at: {}",
                        reset_at.format("%Y-%m-%d %H:%M UTC")
                    );
                }
                if let Some(expires) = profile.token_expires_at {
                    let now = chrono::Utc::now();
                    if expires < now {
                        let ago = now.signed_duration_since(expires);
                        let _ = writeln!(output, "     Token: EXPIRED ({}h ago)", ago.num_hours());
                    } else {
                        let left = expires.signed_duration_since(now);
                        let _ = writeln!(
                            output,
                            "     Token: valid (expires in {}h {}m)",
                            left.num_hours(),
                            left.num_minutes() % 60
                        );
                    }
                }
                if let Some(ref plan) = profile.plan_type {
                    let _ = writeln!(output, "     Plan: {}", plan);
                }
            }
        }

        // Add cost tracking information if available
        if let Some(tracker) = &self.cost_tracker {
            if let Ok(cost_summary) = tracker.get_summary() {
                let _ = writeln!(output, "\nCost & Usage Summary:");
                let _ = writeln!(
                    output,
                    "   Session:  ${:.4} ({} tokens, {} requests)",
                    cost_summary.session_cost_usd,
                    cost_summary.total_tokens,
                    cost_summary.request_count
                );
                let _ = writeln!(output, "   Today:    ${:.4}", cost_summary.daily_cost_usd);
                let _ = writeln!(output, "   Month:    ${:.4}", cost_summary.monthly_cost_usd);

                if !cost_summary.by_model.is_empty() {
                    let _ = writeln!(output, "\n   Per-model breakdown:");
                    for (model, stats) in &cost_summary.by_model {
                        let _ = writeln!(
                            output,
                            "     {}: ${:.4} ({} tokens)",
                            model, stats.cost_usd, stats.total_tokens
                        );
                    }
                }
            }
        }

        // Add metadata as JSON at the end of output for programmatic parsing
        let _ = write!(
            output,
            "\n\n<!-- metadata: {} -->",
            json!({
                "available_providers": available,
                "rate_limited_providers": rate_limited,
                "circuit_open_providers": circuit_open,
            })
        );

        Ok(ToolResult {
            success: true,
            output,
            error: None,
        })
    }
}

/// Tool for switching the default provider/model in config.toml.
///
/// Writes `default_provider` and `default_model` to config.toml so the
/// change persists across requests. Uses the same Config::save() pattern
/// as ModelRoutingConfigTool.
pub struct SwitchProviderTool {
    config: Arc<Config>,
}

impl SwitchProviderTool {
    pub fn new(config: Arc<Config>) -> Self {
        Self { config }
    }

    fn load_config_without_env(&self) -> Result<Config> {
        let contents = std::fs::read_to_string(&self.config.config_path).map_err(|error| {
            anyhow::anyhow!(
                "Failed to read config file {}: {error}",
                self.config.config_path.display()
            )
        })?;

        let mut parsed: Config = toml::from_str(&contents).map_err(|error| {
            anyhow::anyhow!(
                "Failed to parse config file {}: {error}",
                self.config.config_path.display()
            )
        })?;
        parsed.config_path.clone_from(&self.config.config_path);
        parsed.workspace_dir.clone_from(&self.config.workspace_dir);
        Ok(parsed)
    }
}

#[async_trait]
impl Tool for SwitchProviderTool {
    fn name(&self) -> &str {
        "switch_provider"
    }

    fn description(&self) -> &str {
        "Switch to a different AI provider/model by updating config.toml. \
         Use when current provider is rate-limited or when user explicitly \
         requests a specific provider for a task. The change persists across requests."
    }

    fn parameters_schema(&self) -> serde_json::Value {
        json!({
            "type": "object",
            "properties": {
                "provider": {
                    "type": "string",
                    "description": "Provider name (e.g., 'gemini', 'openai', 'anthropic')",
                },
                "model": {
                    "type": "string",
                    "description": "Specific model (optional, e.g., 'gemini-2.5-flash', 'claude-opus-4')"
                },
                "reason": {
                    "type": "string",
                    "description": "Reason for switching (for logging and user notification)"
                }
            },
            "required": ["provider"]
        })
    }

    async fn execute(&self, args: serde_json::Value) -> Result<ToolResult> {
        let provider = args["provider"]
            .as_str()
            .ok_or_else(|| anyhow::anyhow!("Missing provider"))?;
        let model = args.get("model").and_then(|v| v.as_str());
        let reason = args
            .get("reason")
            .and_then(|v| v.as_str())
            .unwrap_or("user request");

        // Load config from disk (without env overrides), update, and save
        let save_result = async {
            let mut cfg = self.load_config_without_env()?;
            let previous_provider = cfg.default_provider.clone();
            let previous_model = cfg.default_model.clone();

            cfg.default_provider = Some(provider.to_string());
            if let Some(m) = model {
                cfg.default_model = Some(m.to_string());
            }

            cfg.save().await?;
            Ok::<_, anyhow::Error>((previous_provider, previous_model))
        }
        .await;

        match save_result {
            Ok((prev_provider, prev_model)) => {
                let mut output = format!(
                    "Switched provider to '{provider}'{}. Reason: {reason}",
                    model.map(|m| format!(" (model: {m})")).unwrap_or_default(),
                );

                if let Some(pp) = &prev_provider {
                    let _ = write!(output, "\nPrevious: {pp}");
                    if let Some(pm) = &prev_model {
                        let _ = write!(output, " ({pm})");
                    }
                }

                let _ = write!(
                    output,
                    "\n\n<!-- metadata: {} -->",
                    json!({
                        "action": "switch_provider",
                        "provider": provider,
                        "model": model,
                        "reason": reason,
                        "previous_provider": prev_provider,
                        "previous_model": prev_model,
                        "persisted": true,
                    })
                );

                Ok(ToolResult {
                    success: true,
                    output,
                    error: None,
                })
            }
            Err(e) => Ok(ToolResult {
                success: false,
                output: String::new(),
                error: Some(format!("Failed to update config: {e}")),
            }),
        }
    }
}

/// Tool for estimating quota cost before expensive operations.
///
/// Allows agent to predict: "это займет ~100 токенов"
pub struct EstimateQuotaCostTool;

#[async_trait]
impl Tool for EstimateQuotaCostTool {
    fn name(&self) -> &str {
        "estimate_quota_cost"
    }

    fn description(&self) -> &str {
        "Estimate quota cost (tokens, requests) for an operation before executing it. \
         Useful for warning user if operation may exhaust quota or when planning \
         parallel tool calls."
    }

    fn parameters_schema(&self) -> serde_json::Value {
        json!({
            "type": "object",
            "properties": {
                "operation": {
                    "type": "string",
                    "description": "Operation type",
                    "enum": ["tool_call", "chat_response", "parallel_tools", "file_analysis"]
                },
                "estimated_tokens": {
                    "type": "integer",
                    "description": "Estimated input+output tokens (optional, default: 1000)"
                },
                "parallel_count": {
                    "type": "integer",
                    "description": "Number of parallel operations (if applicable, default: 1)"
                }
            },
            "required": ["operation"]
        })
    }

    async fn execute(&self, args: serde_json::Value) -> Result<ToolResult> {
        let operation = args["operation"]
            .as_str()
            .ok_or_else(|| anyhow::anyhow!("Missing operation"))?;
        let estimated_tokens = args
            .get("estimated_tokens")
            .and_then(|v| v.as_u64())
            .unwrap_or(1000);
        let parallel_count = args
            .get("parallel_count")
            .and_then(|v| v.as_u64())
            .unwrap_or(1);

        // Simple cost estimation (can be improved with provider-specific pricing)
        let total_tokens = estimated_tokens * parallel_count;
        let total_requests = parallel_count;

        // Rough cost estimate (based on average pricing)
        let cost_per_1k_tokens = 0.015; // Average across providers
        let estimated_cost_usd = (total_tokens as f64 / 1000.0) * cost_per_1k_tokens;

        let output = format!(
            "Estimated cost for {operation}:\n\
             - Requests: {total_requests}\n\
             - Tokens: {total_tokens}\n\
             - Cost: ${estimated_cost_usd:.4} USD (estimate)\n\
             \n\
             Note: Actual cost may vary by provider and model."
        );

        Ok(ToolResult {
            success: true,
            output,
            error: None,
        })
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn test_check_provider_quota_schema() {
        let tool = CheckProviderQuotaTool::new(Arc::new(Config::default()));
        let schema = tool.parameters_schema();
        assert!(schema["properties"]["provider"].is_object());
    }

    #[test]
    fn test_switch_provider_schema() {
        let tool = SwitchProviderTool::new(Arc::new(Config::default()));
        let schema = tool.parameters_schema();
        assert!(schema["required"]
            .as_array()
            .unwrap()
            .contains(&json!("provider")));
    }

    #[test]
    fn test_estimate_quota_schema() {
        let tool = EstimateQuotaCostTool;
        let schema = tool.parameters_schema();
        assert!(schema["properties"]["operation"]["enum"].is_array());
    }

    #[test]
    fn test_check_provider_quota_name_and_description() {
        let tool = CheckProviderQuotaTool::new(Arc::new(Config::default()));
        assert_eq!(tool.name(), "check_provider_quota");
        assert!(tool.description().contains("quota"));
        assert!(tool.description().contains("rate limit"));
    }

    #[test]
    fn test_switch_provider_name_and_description() {
        let tool = SwitchProviderTool::new(Arc::new(Config::default()));
        assert_eq!(tool.name(), "switch_provider");
        assert!(tool.description().contains("Switch"));
    }

    #[test]
    fn test_estimate_quota_cost_name_and_description() {
        let tool = EstimateQuotaCostTool;
        assert_eq!(tool.name(), "estimate_quota_cost");
        assert!(tool.description().contains("cost"));
    }

    #[tokio::test]
    async fn test_switch_provider_execute() {
        let tmp = tempfile::TempDir::new().unwrap();
        let config = Config {
            workspace_dir: tmp.path().to_path_buf(),
            config_path: tmp.path().join("config.toml"),
            ..Config::default()
        };
        config.save().await.unwrap();
        let tool = SwitchProviderTool::new(Arc::new(config));
        let result = tool
            .execute(json!({"provider": "gemini", "model": "gemini-2.5-flash", "reason": "rate limited"}))
            .await
            .unwrap();
        assert!(result.success);
        assert!(result.output.contains("gemini"));
        assert!(result.output.contains("rate limited"));
        // Verify config was actually updated
        let saved = std::fs::read_to_string(tmp.path().join("config.toml")).unwrap();
        assert!(saved.contains("gemini"));
    }

    #[tokio::test]
    async fn test_estimate_quota_cost_execute() {
        let tool = EstimateQuotaCostTool;
        let result = tool
            .execute(json!({"operation": "chat_response", "estimated_tokens": 5000}))
            .await
            .unwrap();
        assert!(result.success);
        assert!(result.output.contains("5000"));
        assert!(result.output.contains('$'));
    }

    #[tokio::test]
    async fn test_check_provider_quota_execute_no_profiles() {
        // Test with default config (no real auth profiles)
        let tmp = tempfile::TempDir::new().unwrap();
        let config = Config {
            workspace_dir: tmp.path().to_path_buf(),
            config_path: tmp.path().join("config.toml"),
            ..Config::default()
        };
        let tool = CheckProviderQuotaTool::new(Arc::new(config));
        let result = tool.execute(json!({})).await.unwrap();
        assert!(result.success);
        // Should contain quota status header
        assert!(result.output.contains("Quota Status"));
    }

    #[tokio::test]
    async fn test_check_provider_quota_with_filter() {
        let tmp = tempfile::TempDir::new().unwrap();
        let config = Config {
            workspace_dir: tmp.path().to_path_buf(),
            config_path: tmp.path().join("config.toml"),
            ..Config::default()
        };
        let tool = CheckProviderQuotaTool::new(Arc::new(config));
        let result = tool.execute(json!({"provider": "gemini"})).await.unwrap();
        assert!(result.success);
    }
}
