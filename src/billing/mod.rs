//! Billing and credit tracking module for ZeroClaw.
//!
//! Tracks API usage costs across providers and channels, enforces
//! spending limits, and provides usage reporting.
//!
//! ## Design
//! - SQLite-based local cost ledger
//! - Per-provider, per-model cost tracking with token counts
//! - Configurable spending limits (daily/monthly) with alerts
//! - Usage summary export for billing reconciliation
//! - TOML-backed model pricing registry with admin API

pub mod checkout;
pub mod llm_router;
pub mod payment;
pub mod pricing;
pub mod tracker;

#[allow(unused_imports)]
pub mod alerts;

pub use checkout::{
    find_subscription_plan, AUTO_RECHARGE_PACKAGE_IDS, SUBSCRIPTION_PLANS,
    AutoRechargeSettings, CheckoutProvider,
};
#[allow(unused_imports)]
pub use llm_router::{
    grant_signup_bonus, AdminKeys, KeySource, ProviderAccessMode, ResolvedKey, TaskCategory,
    CREDITS_PER_USD, LOW_BALANCE_WARNING_THRESHOLD, SIGNUP_BONUS_CREDITS,
};
#[allow(unused_imports)]
pub use payment::{
    BillingPreferences, CreditPackage, PaymentManager, PaymentRecord, PaymentStatus,
    PendingAutoRecharge, SubscriptionRecord, GRANT_TTL_SECS_30D,
    PENDING_AUTO_RECHARGE_TIMEOUT_SECS,
};
#[allow(unused_imports)]
pub use pricing::{ModelPrice, PricingRegistry, SharedPricingRegistry};
#[allow(unused_imports)]
pub use tracker::{CostEntry, CostTracker, UsageSummary};
