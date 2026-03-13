//! LLM billing router for ZeroClaw.
//!
//! Routes LLM API requests through either a user's own API key or the
//! operator's fallback key, applying appropriate billing:
//!
//! - **User key**: No credit deduction (user pays their own API costs).
//! - **Operator key**: Deduct 2x the estimated cost in credits from the
//!   user's balance (covers operator's API cost + margin).
//!
//! This module does NOT wrap the `Provider` trait — it provides key
//! selection and billing logic that callers use before invoking a provider.

use super::payment::PaymentManager;
use super::tracker::{CostEntry, CostTracker};

/// The source of the API key used for a request.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum KeySource {
    /// User provided their own API key.
    UserKey,
    /// Operator's fallback key was used.
    OperatorKey,
}

/// Operator API keys loaded from environment variables.
///
/// These are set as Railway environment variables by the operator.
/// When a user has no personal key for a provider, the operator key
/// is used and the user is charged 2x credits.
#[derive(Debug, Clone, Default)]
pub struct AdminKeys {
    pub anthropic: Option<String>,
    pub openai: Option<String>,
    pub gemini: Option<String>,
    pub perplexity: Option<String>,
    /// Upstage Document Parse API key (for image PDF OCR).
    pub upstage: Option<String>,
}

impl AdminKeys {
    /// Load operator keys from environment variables.
    ///
    /// These are set as Railway environment variables by the operator.
    /// In the hybrid architecture, these keys NEVER leave the server —
    /// clients access LLM/document services through the proxy endpoints
    /// or via temporary upload tokens.
    pub fn from_env() -> Self {
        Self {
            anthropic: std::env::var("ADMIN_ANTHROPIC_API_KEY").ok(),
            openai: std::env::var("ADMIN_OPENAI_API_KEY").ok(),
            gemini: std::env::var("ADMIN_GEMINI_API_KEY").ok(),
            perplexity: std::env::var("ADMIN_PERPLEXITY_API_KEY").ok(),
            upstage: std::env::var("ADMIN_UPSTAGE_API_KEY")
                .or_else(|_| std::env::var("UPSTAGE_API_KEY"))
                .ok(),
        }
    }

    /// Get the operator key for a given provider/service name.
    pub fn get(&self, provider: &str) -> Option<&str> {
        match provider {
            "anthropic" | "claude" => self.anthropic.as_deref(),
            "openai" | "gpt" => self.openai.as_deref(),
            "gemini" | "google" => self.gemini.as_deref(),
            "perplexity" => self.perplexity.as_deref(),
            "upstage" => self.upstage.as_deref(),
            _ => None,
        }
    }
}

/// Result of key resolution for an LLM request.
#[derive(Debug)]
pub struct ResolvedKey {
    /// The API key to use.
    pub api_key: String,
    /// Whether this is the user's own key or operator's fallback.
    pub source: KeySource,
}

/// Credit multiplier applied when using the operator's key.
const OPERATOR_KEY_CREDIT_MULTIPLIER: u32 = 2;

/// Resolve which API key to use for a given provider.
///
/// Priority:
/// 1. User's personal key (from config/settings)
/// 2. Operator's fallback key (from env vars)
/// 3. Error if neither is available
pub fn resolve_key(
    provider: &str,
    user_key: Option<&str>,
    admin_keys: &AdminKeys,
) -> anyhow::Result<ResolvedKey> {
    if let Some(key) = user_key {
        if !key.is_empty() {
            return Ok(ResolvedKey {
                api_key: key.to_string(),
                source: KeySource::UserKey,
            });
        }
    }

    if let Some(admin_key) = admin_keys.get(provider) {
        return Ok(ResolvedKey {
            api_key: admin_key.to_string(),
            source: KeySource::OperatorKey,
        });
    }

    anyhow::bail!(
        "No API key available for provider '{provider}': \
         set your own key in Settings, or contact the operator"
    );
}

/// Record usage and handle billing after an LLM request completes.
///
/// - Always records the cost in the cost tracker.
/// - If the operator key was used, deducts 2x credits from the user's balance.
pub fn record_usage(
    key_source: KeySource,
    user_id: &str,
    provider: &str,
    model: &str,
    input_tokens: i64,
    output_tokens: i64,
    cost_tracker: &CostTracker,
    payment_manager: &PaymentManager,
) -> anyhow::Result<()> {
    let cost_usd = CostTracker::estimate_cost(model, input_tokens, output_tokens);

    // Always record in cost tracker
    let entry = CostEntry {
        provider: provider.to_string(),
        model: model.to_string(),
        input_tokens,
        output_tokens,
        cost_usd,
        channel: None,
        timestamp: chrono::Utc::now().timestamp(),
    };
    cost_tracker.record(&entry)?;

    // Deduct credits only when using operator key
    if key_source == KeySource::OperatorKey {
        // Convert USD cost to credits: 1 credit ≈ ₩10 ≈ $0.007
        // Then multiply by 2 for operator key usage
        let base_credits = ((cost_usd / 0.007) * 1.0).ceil() as u32;
        let credits_to_deduct = base_credits
            .saturating_mul(OPERATOR_KEY_CREDIT_MULTIPLIER)
            .max(1); // Minimum 1 credit

        if let Err(e) = payment_manager.deduct_credits(user_id, credits_to_deduct) {
            tracing::warn!(
                user_id,
                credits = credits_to_deduct,
                cost_usd,
                "Failed to deduct credits for operator key usage: {e}"
            );
            return Err(e);
        }

        tracing::info!(
            user_id,
            provider,
            model,
            cost_usd,
            credits_deducted = credits_to_deduct,
            multiplier = OPERATOR_KEY_CREDIT_MULTIPLIER,
            "Operator key used — credits deducted"
        );
    }

    Ok(())
}

#[cfg(test)]
mod tests {
    use super::*;
    use tempfile::TempDir;

    fn make_admin_keys() -> AdminKeys {
        AdminKeys {
            anthropic: Some("admin-anthropic-key".to_string()),
            openai: Some("admin-openai-key".to_string()),
            gemini: None,
            perplexity: None,
        }
    }

    #[test]
    fn resolve_key_prefers_user_key() {
        let admin = make_admin_keys();
        let resolved = resolve_key("anthropic", Some("user-key"), &admin).unwrap();
        assert_eq!(resolved.api_key, "user-key");
        assert_eq!(resolved.source, KeySource::UserKey);
    }

    #[test]
    fn resolve_key_falls_back_to_admin() {
        let admin = make_admin_keys();
        let resolved = resolve_key("anthropic", None, &admin).unwrap();
        assert_eq!(resolved.api_key, "admin-anthropic-key");
        assert_eq!(resolved.source, KeySource::OperatorKey);
    }

    #[test]
    fn resolve_key_empty_user_key_uses_admin() {
        let admin = make_admin_keys();
        let resolved = resolve_key("openai", Some(""), &admin).unwrap();
        assert_eq!(resolved.api_key, "admin-openai-key");
        assert_eq!(resolved.source, KeySource::OperatorKey);
    }

    #[test]
    fn resolve_key_no_keys_available_fails() {
        let admin = make_admin_keys();
        let result = resolve_key("gemini", None, &admin);
        assert!(result.is_err());
        assert!(result.unwrap_err().to_string().contains("No API key"));
    }

    #[test]
    fn resolve_key_provider_aliases() {
        let admin = make_admin_keys();

        let r1 = resolve_key("claude", None, &admin).unwrap();
        assert_eq!(r1.api_key, "admin-anthropic-key");

        let r2 = resolve_key("gpt", None, &admin).unwrap();
        assert_eq!(r2.api_key, "admin-openai-key");
    }

    #[test]
    fn admin_keys_from_env_reads_vars() {
        // Just verify the method doesn't panic
        let keys = AdminKeys::from_env();
        // In test env, these are likely None
        let _ = keys.anthropic;
        let _ = keys.openai;
        let _ = keys.gemini;
    }

    #[test]
    fn record_usage_user_key_no_credit_deduction() {
        let tmp = TempDir::new().unwrap();
        let tracker = CostTracker::new(tmp.path(), true).unwrap();
        let payment =
            PaymentManager::new(tmp.path(), None, "https://zeroclaw.example.com", true).unwrap();

        // User key → no credit deduction even without balance
        let result = record_usage(
            KeySource::UserKey,
            "zeroclaw_user",
            "anthropic",
            "claude-sonnet-4",
            1000,
            500,
            &tracker,
            &payment,
        );
        assert!(result.is_ok());

        // Cost should be recorded
        let today = tracker.today_total().unwrap();
        assert!(today > 0.0);
    }

    #[test]
    fn record_usage_operator_key_deducts_credits() {
        let tmp = TempDir::new().unwrap();
        let tracker = CostTracker::new(tmp.path(), true).unwrap();
        let payment =
            PaymentManager::new(tmp.path(), None, "https://zeroclaw.example.com", true).unwrap();

        // Give user some credits first
        let (record, _) = payment
            .initiate_payment("zeroclaw_user", "pro_10000")
            .unwrap();
        payment.complete_payment(&record.transaction_id).unwrap();

        let balance_before = payment.get_balance("zeroclaw_user").unwrap();
        assert_eq!(balance_before, 1500);

        // Operator key → deduct 2x credits
        let result = record_usage(
            KeySource::OperatorKey,
            "zeroclaw_user",
            "anthropic",
            "claude-sonnet-4",
            1000,
            500,
            &tracker,
            &payment,
        );
        assert!(result.is_ok());

        let balance_after = payment.get_balance("zeroclaw_user").unwrap();
        assert!(balance_after < balance_before);
    }

    #[test]
    fn record_usage_operator_key_insufficient_credits_fails() {
        let tmp = TempDir::new().unwrap();
        let tracker = CostTracker::new(tmp.path(), true).unwrap();
        let payment =
            PaymentManager::new(tmp.path(), None, "https://zeroclaw.example.com", true).unwrap();

        // No credits → should fail
        let result = record_usage(
            KeySource::OperatorKey,
            "zeroclaw_user",
            "anthropic",
            "claude-sonnet-4",
            1000,
            500,
            &tracker,
            &payment,
        );
        assert!(result.is_err());
    }
}
