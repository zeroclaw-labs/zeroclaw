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

/// 3-tier provider access mode.
///
/// Determines how LLM calls are routed, billed, and which models are used.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum ProviderAccessMode {
    /// User provided their own API key → direct device→provider call,
    /// no credit deduction. User chooses model freely.
    UserKey,
    /// No API key, but user explicitly selected a model →
    /// Railway relay with operator key, credits deducted at 2.2×.
    PlatformSelected,
    /// No API key, no model selection (new/default users) →
    /// Railway relay with operator key, task-based auto-routing,
    /// credits deducted at 2.2×.
    PlatformDefault,
}

/// Determine the provider access mode for a request.
pub fn determine_access_mode(user_has_key: bool, user_selected_model: bool) -> ProviderAccessMode {
    if user_has_key {
        ProviderAccessMode::UserKey
    } else if user_selected_model {
        ProviderAccessMode::PlatformSelected
    } else {
        ProviderAccessMode::PlatformDefault
    }
}

/// Task category for default model routing.
///
/// MoA uses a 3-tier model strategy to optimize cost:
/// - **Economy** (Gemini Flash Lite): cron jobs, compaction, simple tasks — cost-effective
/// - **Standard** (Gemini Flash Lite): general chat, search, media — 1/15 of Opus cost
/// - **Premium** (Opus 4.6 / Gemini Pro): coding, legal docs, reasoning — highest quality
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum TaskCategory {
    /// 일반 채팅 (General chat / web search)
    GeneralChat,
    /// 추론/문서 (Reasoning / document analysis / legal writing)
    ReasoningDocument,
    /// 코딩 (Coding)
    Coding,
    /// 코드 리뷰 (Code review)
    CodeReview,
    /// 이미지 (Image generation/analysis)
    Image,
    /// 음악 (Music)
    Music,
    /// 비디오 (Video)
    Video,
    /// 통역 (Voice interpretation)
    Interpretation,
    /// Cron 작업 / 반복 알림 (Economy tier — routine scheduled tasks)
    CronRoutine,
    /// 히스토리 요약 (Economy tier — compaction summarization)
    Compaction,
    /// 메모리 정리/분류 (Economy tier — memory housekeeping)
    MemoryHousekeeping,
}

/// Default model assignment for each task category (Platform Default mode).
///
/// These are used when the user has no API key and has not selected a model.
// Tier members are grouped for cost documentation. Arms intentionally share
// bodies today; diverging per-category models is an expected future change,
// so keep one arm per TaskCategory rather than collapsing them.
#[allow(clippy::match_same_arms)]
pub fn default_model_for_task(task: TaskCategory) -> (&'static str, &'static str) {
    // Returns (provider, model_id)
    //
    // 3-Tier cost optimization:
    //   Economy  (Gemini Flash Lite):   cron, compaction, memory — cost-effective
    //   Standard (Gemini Flash Lite):  chat, search, media     — ~$0.05/M input
    //   Premium  (Opus 4.6 / Pro):     coding, legal, reasoning — ~$0.60/M input
    match task {
        // ── Economy tier (Gemini 3.1 Flash Lite — cost-effective for routine tasks) ──
        // Replaces MiniMax M2.7 which had multi-language mixing issues.
        TaskCategory::CronRoutine => ("gemini", "gemini-3.1-flash-lite-preview"),
        TaskCategory::Compaction => ("gemini", "gemini-3.1-flash-lite-preview"),
        TaskCategory::MemoryHousekeeping => ("gemini", "gemini-3.1-flash-lite-preview"),

        // ── Standard tier (Gemini Flash Lite — same model, general tasks) ──
        TaskCategory::GeneralChat => ("gemini", "gemini-3.1-flash-lite-preview"),
        TaskCategory::Image => ("gemini", "gemini-3.1-flash-lite-preview"),
        TaskCategory::Music => ("gemini", "gemini-3.1-flash-lite-preview"),
        TaskCategory::Video => ("gemini", "gemini-3.1-flash-lite-preview"),

        // ── Premium tier (highest quality) ──
        TaskCategory::ReasoningDocument => ("gemini", "gemini-3.1-pro-preview"),
        TaskCategory::Coding => ("anthropic", "claude-opus-4-6"),
        TaskCategory::CodeReview => ("gemini", "gemini-3.1-pro-preview"),
        TaskCategory::Interpretation => ("gemini", "gemini-2.5-flash"),
    }
}

/// Low-balance warning threshold in credits.
///
/// Fallback low-balance warning threshold when the user has not chosen
/// an explicit preference (see `billing_preferences.low_balance_threshold`).
///
/// Under the 1:1000 USD-to-credit ratio this is equivalent to $5 of
/// remaining headroom at 1× billing. Spec allows the user to narrow it
/// to 3,000 credits in Settings; the default of 5,000 is chosen to give
/// the auto-recharge path time to succeed before the balance hits zero.
pub const LOW_BALANCE_WARNING_THRESHOLD: u32 = 5_000;

/// Signup bonus credits granted once to each new user at registration.
///
/// Under the 1:1000 USD-to-credit ratio this is equivalent to $2 worth
/// of headroom at 1× billing, or roughly $0.91 at the 2.2× operator
/// markup. Enough for the user to try a handful of premium-cloud chats
/// before deciding whether to subscribe, paste a BYOK key, or stay on
/// the free Gemma 4 base gun.
pub const SIGNUP_BONUS_CREDITS: u32 = 2_000;

/// Grant signup bonus credits to a new user.
///
/// Called once during user registration. The bonus is enough for
/// initial exploration of general chat (Gemini 3.1 Flash Lite is very
/// cost-effective) and a few coding/document tasks.
pub fn grant_signup_bonus(payment_manager: &PaymentManager, user_id: &str) -> anyhow::Result<u32> {
    payment_manager.add_bonus_credits(user_id, SIGNUP_BONUS_CREDITS)?;
    // Log the grant in the per-grant ledger with the standard 30-day TTL
    // so the monthly sweep can expire any unused portion on schedule.
    payment_manager.record_grant(
        "",
        user_id,
        SIGNUP_BONUS_CREDITS,
        "signup",
        crate::billing::GRANT_TTL_SECS_30D,
    )?;
    tracing::info!(
        user_id,
        credits = SIGNUP_BONUS_CREDITS,
        "Signup bonus credits granted"
    );
    Ok(SIGNUP_BONUS_CREDITS)
}

/// Check credit balance and return a warning status.
///
/// Returns:
/// - `Ok(None)` — balance is healthy, no warning needed
/// - `Ok(Some(balance))` — balance is low, caller should show warning
/// - `Err(_)` — balance is zero or lookup failed
pub fn check_credit_warning(
    payment_manager: &PaymentManager,
    user_id: &str,
) -> anyhow::Result<Option<u32>> {
    let balance = payment_manager.get_balance(user_id)?;
    if balance == 0 {
        anyhow::bail!(
            "크레딧이 소진되었습니다. 크레딧을 충전하시거나 설정에서 직접 API 키를 입력해 주세요. \
             (Credits exhausted. Please recharge credits or enter your own API keys in Settings.)"
        );
    }
    if balance <= LOW_BALANCE_WARNING_THRESHOLD {
        Ok(Some(balance))
    } else {
        Ok(None)
    }
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
    /// Freepik API key (image generation/editing/upscaling).
    pub freepik: Option<String>,
    /// Suno API key (music generation).
    pub suno: Option<String>,
    /// Runway API key (video generation).
    pub runway: Option<String>,
    /// ElevenLabs API key (premium TTS voices).
    /// When set, users without their own key are charged 2.2× credits.
    pub elevenlabs: Option<String>,
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
            freepik: std::env::var("ADMIN_FREEPIK_API_KEY").ok(),
            suno: std::env::var("ADMIN_SUNO_API_KEY").ok(),
            runway: std::env::var("ADMIN_RUNWAY_API_KEY").ok(),
            elevenlabs: std::env::var("ADMIN_ELEVENLABS_API_KEY").ok(),
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
            "freepik" => self.freepik.as_deref(),
            "suno" => self.suno.as_deref(),
            "runway" => self.runway.as_deref(),
            "elevenlabs" => self.elevenlabs.as_deref(),
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
///
/// 2.0× base operator margin + 10% VAT = 2.2× total.
/// Expressed as a float to preserve the fractional VAT component.
/// Scalar mapping USD billing charges to user-visible credit units.
///
/// Spec (2026-04-22): "달러에 1000배한 것이 MoA에서의 크레딧 가치".
/// Keep this constant in lockstep with the React-side display logic
/// (`clients/tauri/src/lib/billing.ts::CREDITS_PER_USD`) and with the
/// KRW FX conversion that renders prices in the billing page.
pub const CREDITS_PER_USD: f64 = 1_000.0;

const OPERATOR_KEY_CREDIT_MULTIPLIER: f64 = 2.2;

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
        // Spec (2026-04-22): 1 USD billed = 1,000 credits, and the
        // operator's raw API cost is charged at 2.2× (plus VAT applied
        // elsewhere in the pipeline via PlatformRoutingConfig.vat_rate).
        // Example: raw cost $0.91 → 0.91 × 2.2 × 1_000 ≈ 2,002 credits
        // → the user sees ~"$2 of API burn = 2,000 credits" in their
        // history, which matches the onboarding greeting copy.
        let credits_to_deduct =
            (cost_usd * OPERATOR_KEY_CREDIT_MULTIPLIER * CREDITS_PER_USD).ceil() as u32;
        let credits_to_deduct = credits_to_deduct.max(1); // Minimum 1 credit

        // Check balance and warn before deduction
        let current_balance = payment_manager.get_balance(user_id).unwrap_or(0);
        if current_balance <= LOW_BALANCE_WARNING_THRESHOLD && current_balance > 0 {
            tracing::warn!(
                user_id,
                balance = current_balance,
                "Low credit balance — user should recharge or enter own API keys"
            );
        }

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
            "Operator key used — credits deducted (2.2× = 2× margin + VAT)"
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
            upstage: None,
            freepik: None,
            suno: None,
            runway: None,
            elevenlabs: None,
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
    fn record_usage_operator_key_deducts_credits_at_2_2x() {
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

        // Operator key → deduct 2.2x credits (2× margin + 10% VAT)
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

    #[test]
    fn determine_access_mode_user_key() {
        assert_eq!(
            determine_access_mode(true, false),
            ProviderAccessMode::UserKey
        );
        assert_eq!(
            determine_access_mode(true, true),
            ProviderAccessMode::UserKey
        );
    }

    #[test]
    fn determine_access_mode_platform_selected() {
        assert_eq!(
            determine_access_mode(false, true),
            ProviderAccessMode::PlatformSelected
        );
    }

    #[test]
    fn determine_access_mode_platform_default() {
        assert_eq!(
            determine_access_mode(false, false),
            ProviderAccessMode::PlatformDefault
        );
    }

    #[test]
    fn default_model_for_general_chat() {
        let (provider, model) = default_model_for_task(TaskCategory::GeneralChat);
        assert_eq!(provider, "gemini");
        assert_eq!(model, "gemini-3.1-flash-lite-preview");
    }

    #[test]
    fn default_model_for_coding() {
        let (provider, model) = default_model_for_task(TaskCategory::Coding);
        assert_eq!(provider, "anthropic");
        assert_eq!(model, "claude-opus-4-6");
    }

    #[test]
    fn default_model_for_reasoning_document() {
        let (provider, model) = default_model_for_task(TaskCategory::ReasoningDocument);
        assert_eq!(provider, "gemini");
        assert_eq!(model, "gemini-3.1-pro-preview");
    }

    #[test]
    fn default_model_for_code_review() {
        let (provider, model) = default_model_for_task(TaskCategory::CodeReview);
        assert_eq!(provider, "gemini");
        assert_eq!(model, "gemini-3.1-pro-preview");
    }

    #[test]
    fn default_model_for_interpretation() {
        let (provider, model) = default_model_for_task(TaskCategory::Interpretation);
        assert_eq!(provider, "gemini");
        assert_eq!(model, "gemini-2.5-flash");
    }

    #[test]
    fn signup_bonus_grants_credits() {
        let tmp = TempDir::new().unwrap();
        let payment =
            PaymentManager::new(tmp.path(), None, "https://zeroclaw.example.com", true).unwrap();

        let balance_before = payment.get_balance("new_user").unwrap();
        assert_eq!(balance_before, 0);

        let bonus = grant_signup_bonus(&payment, "new_user").unwrap();
        assert_eq!(bonus, SIGNUP_BONUS_CREDITS);

        let balance_after = payment.get_balance("new_user").unwrap();
        assert_eq!(balance_after, SIGNUP_BONUS_CREDITS);
    }

    #[test]
    fn check_credit_warning_healthy_balance() {
        let tmp = TempDir::new().unwrap();
        let payment =
            PaymentManager::new(tmp.path(), None, "https://zeroclaw.example.com", true).unwrap();

        // Give user plenty of credits
        payment.add_bonus_credits("zeroclaw_user", 1000).unwrap();
        let result = check_credit_warning(&payment, "zeroclaw_user").unwrap();
        assert!(result.is_none()); // No warning
    }

    #[test]
    fn check_credit_warning_low_balance() {
        let tmp = TempDir::new().unwrap();
        let payment =
            PaymentManager::new(tmp.path(), None, "https://zeroclaw.example.com", true).unwrap();

        // Give user just above zero but below threshold
        payment.add_bonus_credits("zeroclaw_user", 50).unwrap();
        let result = check_credit_warning(&payment, "zeroclaw_user").unwrap();
        assert_eq!(result, Some(50)); // Warning with balance
    }

    #[test]
    fn check_credit_warning_zero_balance_errors() {
        let tmp = TempDir::new().unwrap();
        let payment =
            PaymentManager::new(tmp.path(), None, "https://zeroclaw.example.com", true).unwrap();

        let result = check_credit_warning(&payment, "zeroclaw_user");
        assert!(result.is_err()); // Error: zero credits
    }
}
