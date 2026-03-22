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
pub use checkout::{
    AutoRechargeSettings, CheckoutProvider, CheckoutRequest, CheckoutResponse, UsdCreditPackage,
    USD_PACKAGES,
};
#[allow(unused_imports)]
pub use llm_router::{
    AdminKeys, KeySource, ProviderAccessMode, ResolvedKey, TaskCategory,
    LOW_BALANCE_WARNING_THRESHOLD, SIGNUP_BONUS_CREDITS,
};
#[allow(unused_imports)]
pub use payment::{CreditPackage, PaymentManager, PaymentRecord, PaymentStatus};
#[allow(unused_imports)]
pub use pricing::{ModelPrice, PricingRegistry, SharedPricingRegistry};
#[allow(unused_imports)]
pub use tracker::{CostEntry, CostTracker, UsageSummary};
