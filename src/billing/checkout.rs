//! Multi-provider checkout module for ZeroClaw.
//!
//! Supports two payment providers:
//! - **Stripe** — For international users (credit/debit cards worldwide)
//! - **TossPayments** — For Korean users (카카오페이, 네이버페이, 간편결제)
//!
//! ## Credit Packages (USD-based)
//!
//! | Package    | Price | Credits |
//! |------------|-------|---------|
//! | Starter    | $10   | 1,500   |
//! | Standard   | $20   | 3,200   |
//! | Power      | $50   | 8,500   |
//!
//! ## Auto-Recharge
//! Users can opt in to auto-recharge: when balance drops below $1 worth
//! of credits (~143 credits), the system charges the saved payment method
//! for the user's selected package.

use serde::{Deserialize, Serialize};
use std::collections::HashMap;

// ── Credit Packages (USD) ────────────────────────────────────────

/// A credit package available for purchase.
#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct UsdCreditPackage {
    pub id: &'static str,
    pub name: &'static str,
    /// Price in USD cents (to avoid floating point).
    pub price_cents: u32,
    /// Credits granted.
    pub credits: u32,
    /// Price in KRW (for TossPayments).
    pub price_krw: u32,
}

/// Available one-off top-up packages (spec, 2026-04-22):
///
/// * Manual-recharge offering: $10 / $25 / $50 / $100 / $200.
/// * Auto-recharge offering (subset): $10 / $25 / $50 — the top three
///   tiers are deliberately absent from auto-recharge so an unattended
///   loop can never silently charge a user >$50 at a time.
///
/// Credit grants follow the 1:1000 USD-to-credit ratio strictly — NO
/// loyalty bonus on one-off purchases. Loyalty bonuses belong on the
/// monthly subscription plan (see `SUBSCRIPTION_PLANS`). The `price_krw`
/// values are baseline hardcodes; the billing page recomputes them
/// from a live FX feed before rendering.
pub const USD_PACKAGES: &[UsdCreditPackage] = &[
    UsdCreditPackage {
        id: "topup_10",
        name: "$10",
        price_cents: 1_000,
        credits: 10_000,
        price_krw: 14_000,
    },
    UsdCreditPackage {
        id: "topup_25",
        name: "$25",
        price_cents: 2_500,
        credits: 25_000,
        price_krw: 35_000,
    },
    UsdCreditPackage {
        id: "topup_50",
        name: "$50",
        price_cents: 5_000,
        credits: 50_000,
        price_krw: 69_000,
    },
    UsdCreditPackage {
        id: "topup_100",
        name: "$100",
        price_cents: 10_000,
        credits: 100_000,
        price_krw: 138_000,
    },
    UsdCreditPackage {
        id: "topup_200",
        name: "$200",
        price_cents: 20_000,
        credits: 200_000,
        price_krw: 276_000,
    },
];

/// Package IDs eligible for auto-recharge (subset of `USD_PACKAGES`).
/// Kept separate so the billing page can render a narrower dropdown
/// without hardcoding the subset there.
pub const AUTO_RECHARGE_PACKAGE_IDS: &[&str] = &["topup_10", "topup_25", "topup_50"];

/// Recurring subscription plan (spec, 2026-04-22).
///
/// Credits are granted every billing cycle with the same 30-day TTL as
/// one-off top-ups: unused balance rolls off at the end of the month
/// rather than accumulating indefinitely. The annual plan charges
/// 12 × monthly × 0.9 up front (10% discount) and grants 12× the
/// monthly credit amount in one shot — each grant still uses the
/// standard 30-day TTL, so an annual subscriber receives fresh credits
/// every 30 days via the subscription renewal hook (not by splitting
/// the up-front grant).
#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct SubscriptionPlan {
    pub id: &'static str,
    pub name: &'static str,
    /// One up-front charge in USD cents. Monthly: 3_000 ($30). Annual:
    /// 3_000 × 12 × 0.9 = 32_400 ($324).
    pub price_cents: u32,
    /// Baseline KRW hardcode. The React billing page overrides this with
    /// a live FX conversion before rendering — keep the fallback in the
    /// same ~1,380 KRW/USD ballpark for offline operation.
    pub price_krw: u32,
    /// Credits granted per billing cycle (monthly plan: 20_000 on each
    /// renewal; annual plan: 20_000 each month, issued by the webhook
    /// renewal hook rather than as a single 240_000 block).
    pub credits_per_cycle: u32,
    /// Number of billing cycles covered by a single charge.
    pub cycles: u32,
    /// `"month"` | `"year"` — display label only.
    pub interval: &'static str,
}

/// Catalog of subscription plans.
pub const SUBSCRIPTION_PLANS: &[SubscriptionPlan] = &[
    SubscriptionPlan {
        id: "sub_monthly_30",
        name: "MoA Monthly",
        price_cents: 3_000,
        price_krw: 41_000,
        credits_per_cycle: 20_000,
        cycles: 1,
        interval: "month",
    },
    SubscriptionPlan {
        id: "sub_annual_324",
        name: "MoA Annual (save 10%)",
        // 3_000 cents × 12 × 0.9 = 32_400 cents ($324).
        price_cents: 32_400,
        price_krw: 447_000,
        credits_per_cycle: 20_000,
        cycles: 12,
        interval: "year",
    },
];

/// Look up a subscription plan by ID.
pub fn find_subscription_plan(id: &str) -> Option<&'static SubscriptionPlan> {
    SUBSCRIPTION_PLANS.iter().find(|p| p.id == id)
}

/// Fallback auto-recharge balance trigger when the user has not saved
/// an explicit preference. Overridden per-user by
/// `billing_preferences.auto_recharge_threshold` (values: 3_000 or 5_000).
pub const AUTO_RECHARGE_THRESHOLD: u32 = 5_000;

pub fn find_usd_package(id: &str) -> Option<&'static UsdCreditPackage> {
    USD_PACKAGES.iter().find(|p| p.id == id)
}

// ── Checkout Session ─────────────────────────────────────────────

/// Payment provider selection.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum CheckoutProvider {
    Stripe,
    Toss,
}

/// Request to create a checkout session.
#[derive(Debug, Deserialize)]
pub struct CheckoutRequest {
    pub user_id: String,
    pub package_id: String,
    pub provider: CheckoutProvider,
    /// If true, save the payment method for auto-recharge.
    #[serde(default)]
    pub save_method: bool,
}

/// Response from creating a checkout session.
#[derive(Debug, Serialize)]
pub struct CheckoutResponse {
    /// URL to redirect the user to for payment.
    pub checkout_url: String,
    /// Our internal transaction ID.
    pub transaction_id: String,
    /// Provider used.
    pub provider: CheckoutProvider,
}

/// Auto-recharge settings for a user.
#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct AutoRechargeSettings {
    pub enabled: bool,
    pub package_id: String,
    pub provider: CheckoutProvider,
    /// Stripe customer ID or Toss billing key.
    pub saved_method_id: Option<String>,
}

// ── Stripe Integration ───────────────────────────────────────────

/// Create a Stripe Checkout Session.
///
/// Returns the Stripe checkout URL for the user to complete payment.
/// On success, Stripe calls our webhook to confirm.
pub async fn create_stripe_session(
    secret_key: &str,
    package: &UsdCreditPackage,
    transaction_id: &str,
    user_id: &str,
    callback_base_url: &str,
    save_method: bool,
) -> anyhow::Result<CheckoutResponse> {
    let client = reqwest::Client::new();

    let mode = if save_method { "setup" } else { "payment" };

    let mut params: Vec<(&str, String)> = vec![
        ("mode", mode.to_string()),
        (
            "success_url",
            format!("{callback_base_url}/api/checkout/success?tx={transaction_id}&provider=stripe"),
        ),
        (
            "cancel_url",
            format!("{callback_base_url}/api/checkout/cancel?tx={transaction_id}"),
        ),
        ("client_reference_id", transaction_id.to_string()),
        ("metadata[user_id]", user_id.to_string()),
        ("metadata[package_id]", package.id.to_string()),
        ("metadata[transaction_id]", transaction_id.to_string()),
    ];

    if mode == "payment" {
        params.extend([
            ("line_items[0][price_data][currency]", "usd".to_string()),
            (
                "line_items[0][price_data][unit_amount]",
                package.price_cents.to_string(),
            ),
            (
                "line_items[0][price_data][product_data][name]",
                format!(
                    "MoA Credits — {} ({} credits)",
                    package.name, package.credits
                ),
            ),
            ("line_items[0][quantity]", "1".to_string()),
        ]);
    }

    if save_method {
        params.push((
            "payment_intent_data[setup_future_usage]",
            "off_session".to_string(),
        ));
    }

    let response = client
        .post("https://api.stripe.com/v1/checkout/sessions")
        .basic_auth(secret_key, Option::<&str>::None)
        .form(&params)
        .send()
        .await
        .map_err(|e| anyhow::anyhow!("Stripe API error: {e}"))?;

    let status = response.status();
    let body: serde_json::Value = response
        .json()
        .await
        .map_err(|e| anyhow::anyhow!("Stripe response parse error: {e}"))?;

    if !status.is_success() {
        let msg = body
            .pointer("/error/message")
            .and_then(|v| v.as_str())
            .unwrap_or("Unknown Stripe error");
        anyhow::bail!("Stripe error ({}): {}", status.as_u16(), msg);
    }

    let checkout_url = body
        .get("url")
        .and_then(|v| v.as_str())
        .ok_or_else(|| anyhow::anyhow!("Stripe response missing checkout URL"))?
        .to_string();

    Ok(CheckoutResponse {
        checkout_url,
        transaction_id: transaction_id.to_string(),
        provider: CheckoutProvider::Stripe,
    })
}

/// Verify a Stripe webhook event signature.
pub fn verify_stripe_signature(
    payload: &[u8],
    sig_header: &str,
    webhook_secret: &str,
) -> anyhow::Result<serde_json::Value> {
    // Parse the Stripe-Signature header
    let mut parts: HashMap<&str, &str> = HashMap::new();
    for pair in sig_header.split(',') {
        if let Some((k, v)) = pair.split_once('=') {
            parts.insert(k.trim(), v.trim());
        }
    }

    let timestamp = parts
        .get("t")
        .ok_or_else(|| anyhow::anyhow!("Missing timestamp in Stripe signature"))?;
    let v1_sig = parts
        .get("v1")
        .ok_or_else(|| anyhow::anyhow!("Missing v1 signature in Stripe signature"))?;

    // Compute expected signature: HMAC-SHA256(secret, "{timestamp}.{payload}")
    use hmac::{Hmac, Mac};
    use sha2::Sha256;
    type HmacSha256 = Hmac<Sha256>;

    let signed_payload = format!("{}.{}", timestamp, String::from_utf8_lossy(payload));
    let mut mac = HmacSha256::new_from_slice(webhook_secret.as_bytes())
        .map_err(|e| anyhow::anyhow!("HMAC init failed: {e}"))?;
    mac.update(signed_payload.as_bytes());

    let expected = hex::encode(mac.finalize().into_bytes());
    if expected != *v1_sig {
        anyhow::bail!("Invalid Stripe webhook signature");
    }

    serde_json::from_slice(payload)
        .map_err(|e| anyhow::anyhow!("Invalid Stripe webhook payload: {e}"))
}

// ── TossPayments Integration ─────────────────────────────────────

/// Create a TossPayments checkout session.
///
/// Uses the "결제 요청" API to create a payment and returns
/// the checkout URL for the user.
pub async fn create_toss_session(
    secret_key: &str,
    package: &UsdCreditPackage,
    transaction_id: &str,
    user_id: &str,
    callback_base_url: &str,
) -> anyhow::Result<CheckoutResponse> {
    let client = reqwest::Client::new();

    // TossPayments uses Base64-encoded "{secret_key}:" for auth
    let auth = base64_encode_key(secret_key);

    let body = serde_json::json!({
        "amount": package.price_krw,
        "orderId": transaction_id,
        "orderName": format!("MoA 크레딧 — {} ({}크레딧)", package.name, package.credits),
        "successUrl": format!("{callback_base_url}/api/checkout/success?tx={transaction_id}&provider=toss"),
        "failUrl": format!("{callback_base_url}/api/checkout/cancel?tx={transaction_id}"),
        "method": "카드",
        "metadata": {
            "user_id": user_id,
            "package_id": package.id,
        }
    });

    let response = client
        .post("https://api.tosspayments.com/v1/payments")
        .header("Authorization", format!("Basic {auth}"))
        .header("Content-Type", "application/json")
        .json(&body)
        .send()
        .await
        .map_err(|e| anyhow::anyhow!("TossPayments API error: {e}"))?;

    let status = response.status();
    let resp_body: serde_json::Value = response
        .json()
        .await
        .map_err(|e| anyhow::anyhow!("TossPayments response parse error: {e}"))?;

    if !status.is_success() {
        let msg = resp_body
            .get("message")
            .and_then(|v| v.as_str())
            .unwrap_or("Unknown TossPayments error");
        anyhow::bail!("TossPayments error ({}): {}", status.as_u16(), msg);
    }

    let checkout_url = resp_body
        .get("checkout")
        .and_then(|v| v.get("url"))
        .and_then(|v| v.as_str())
        .ok_or_else(|| anyhow::anyhow!("TossPayments response missing checkout URL"))?
        .to_string();

    Ok(CheckoutResponse {
        checkout_url,
        transaction_id: transaction_id.to_string(),
        provider: CheckoutProvider::Toss,
    })
}

/// Confirm a TossPayments payment after success callback.
pub async fn confirm_toss_payment(
    secret_key: &str,
    payment_key: &str,
    order_id: &str,
    amount: u32,
) -> anyhow::Result<serde_json::Value> {
    let client = reqwest::Client::new();
    let auth = base64_encode_key(secret_key);

    let body = serde_json::json!({
        "paymentKey": payment_key,
        "orderId": order_id,
        "amount": amount,
    });

    let response = client
        .post("https://api.tosspayments.com/v1/payments/confirm")
        .header("Authorization", format!("Basic {auth}"))
        .header("Content-Type", "application/json")
        .json(&body)
        .send()
        .await
        .map_err(|e| anyhow::anyhow!("TossPayments confirm error: {e}"))?;

    let status = response.status();
    let resp_body: serde_json::Value = response.json().await?;

    if !status.is_success() {
        let msg = resp_body
            .get("message")
            .and_then(|v| v.as_str())
            .unwrap_or("Confirmation failed");
        anyhow::bail!("TossPayments confirm error ({}): {}", status.as_u16(), msg);
    }

    Ok(resp_body)
}

/// Base64-encode a TossPayments secret key for Basic auth.
fn base64_encode_key(secret_key: &str) -> String {
    use base64::Engine;
    base64::engine::general_purpose::STANDARD.encode(format!("{}:", secret_key))
}

// ── Auto-Recharge Logic ──────────────────────────────────────────

/// Check if a user's balance is below the auto-recharge threshold
/// and trigger a charge if auto-recharge is enabled.
///
/// This is called after every credit deduction.
pub async fn maybe_auto_recharge(
    user_id: &str,
    current_balance: u32,
    settings: &AutoRechargeSettings,
    stripe_key: Option<&str>,
    toss_key: Option<&str>,
    _callback_base_url: &str,
) -> anyhow::Result<Option<String>> {
    if !settings.enabled || current_balance > AUTO_RECHARGE_THRESHOLD {
        return Ok(None);
    }

    let package = find_usd_package(&settings.package_id).ok_or_else(|| {
        anyhow::anyhow!("Auto-recharge package not found: {}", settings.package_id)
    })?;

    let saved_method = settings
        .saved_method_id
        .as_deref()
        .ok_or_else(|| anyhow::anyhow!("Auto-recharge enabled but no saved payment method"))?;

    let transaction_id = uuid::Uuid::new_v4().to_string();

    match settings.provider {
        CheckoutProvider::Stripe => {
            let key = stripe_key
                .ok_or_else(|| anyhow::anyhow!("Stripe key not configured for auto-recharge"))?;

            // Create a PaymentIntent using the saved payment method
            let client = reqwest::Client::new();
            let params = [
                ("amount", package.price_cents.to_string()),
                ("currency", "usd".to_string()),
                ("customer", saved_method.to_string()),
                ("off_session", "true".to_string()),
                ("confirm", "true".to_string()),
                ("metadata[user_id]", user_id.to_string()),
                ("metadata[package_id]", package.id.to_string()),
                ("metadata[transaction_id]", transaction_id.to_string()),
                ("metadata[auto_recharge]", "true".to_string()),
            ];

            let response = client
                .post("https://api.stripe.com/v1/payment_intents")
                .basic_auth(key, Option::<&str>::None)
                .form(&params)
                .send()
                .await?;

            let body: serde_json::Value = response.json().await?;
            let status_str = body.get("status").and_then(|v| v.as_str()).unwrap_or("");

            if status_str == "succeeded" {
                tracing::info!(
                    user_id,
                    package_id = package.id,
                    credits = package.credits,
                    "Auto-recharge succeeded via Stripe"
                );
                Ok(Some(transaction_id))
            } else {
                anyhow::bail!("Auto-recharge Stripe payment status: {}", status_str);
            }
        }
        CheckoutProvider::Toss => {
            // TossPayments recurring charge against a previously-issued
            // billing key (issued via `exchange_toss_auth_key`). The
            // prior draft called `/v1/billing/authorizations/card` which
            // is actually the *issue* endpoint, not the charge endpoint
            // — that would silently fail on every retry. Using
            // `charge_toss_billing_key` here keeps the auth and charge
            // paths sharing one well-tested call.
            let key = toss_key
                .ok_or_else(|| anyhow::anyhow!("Toss key not configured for auto-recharge"))?;
            let order_name =
                format!("MoA 크레딧 자동충전 — {} ({}크레딧)", package.name, package.credits);
            charge_toss_billing_key(
                key,
                saved_method,
                user_id,
                package.price_krw,
                &transaction_id,
                &order_name,
            )
            .await?;
            tracing::info!(
                user_id,
                package_id = package.id,
                credits = package.credits,
                "Auto-recharge succeeded via TossPayments billing key"
            );
            Ok(Some(transaction_id))
        }
    }
}

// ── Stripe subscription (recurring) — spec, 2026-04-22 ───────────
//
// The earlier `create_stripe_session` creates a one-shot Checkout Session
// in `mode=payment`, which is the right shape for one-off credit
// top-ups. Subscriptions need `mode=subscription` and a recurring price
// so Stripe itself drives monthly / annual renewal, emitting
// `invoice.paid` every cycle. We price the subscription inline via
// `price_data` + `recurring[interval]` so the operator does not have to
// pre-create Stripe Price objects for the two plans — one less config
// step, and the price stays in lockstep with `SUBSCRIPTION_PLANS`.
//
// The gateway scheduler used to poke a synthetic renewal when the
// `subscriptions.renewal_at` clock expired, but once a subscription
// goes through this path Stripe becomes the source of truth — see
// the `invoice.paid` branch in `handle_api_checkout_webhook_stripe`.

/// Create a Stripe Checkout Session in subscription mode.
///
/// On success the returned `checkout_url` leads the user through card
/// entry + first-cycle charge. Stripe then invokes our webhook with
/// `checkout.session.completed` (first activation) and
/// `invoice.paid` on every subsequent renewal cycle.
pub async fn create_stripe_subscription_session(
    secret_key: &str,
    plan: &SubscriptionPlan,
    transaction_id: &str,
    user_id: &str,
    callback_base_url: &str,
) -> anyhow::Result<CheckoutResponse> {
    let client = reqwest::Client::new();
    // Interval: monthly plan → month, annual plan → year. Stripe allows
    // interval_count but spec plans cover exactly one interval unit each,
    // so we keep the request minimal.
    let interval = if plan.interval == "year" { "year" } else { "month" };

    let params: Vec<(&str, String)> = vec![
        ("mode", "subscription".to_string()),
        (
            "success_url",
            format!(
                "{callback_base_url}/api/checkout/success?tx={transaction_id}&provider=stripe&plan={plan_id}",
                plan_id = plan.id,
            ),
        ),
        (
            "cancel_url",
            format!("{callback_base_url}/api/checkout/cancel?tx={transaction_id}"),
        ),
        ("client_reference_id", transaction_id.to_string()),
        ("metadata[user_id]", user_id.to_string()),
        ("metadata[plan_id]", plan.id.to_string()),
        ("metadata[transaction_id]", transaction_id.to_string()),
        ("subscription_data[metadata][user_id]", user_id.to_string()),
        ("subscription_data[metadata][plan_id]", plan.id.to_string()),
        ("line_items[0][quantity]", "1".to_string()),
        ("line_items[0][price_data][currency]", "usd".to_string()),
        (
            "line_items[0][price_data][unit_amount]",
            // Stripe charges the full plan price each billing cycle.
            // Monthly plan: 3_000 cents per cycle. Annual plan: 32_400
            // cents per cycle (every 12 months).
            (plan.price_cents / plan.cycles).to_string(),
        ),
        (
            "line_items[0][price_data][product_data][name]",
            format!("MoA Subscription — {}", plan.name),
        ),
        (
            "line_items[0][price_data][recurring][interval]",
            interval.to_string(),
        ),
    ];

    let response = client
        .post("https://api.stripe.com/v1/checkout/sessions")
        .basic_auth(secret_key, Option::<&str>::None)
        .form(&params)
        .send()
        .await
        .map_err(|e| anyhow::anyhow!("Stripe API error: {e}"))?;

    let status = response.status();
    let body: serde_json::Value = response
        .json()
        .await
        .map_err(|e| anyhow::anyhow!("Stripe response parse error: {e}"))?;

    if !status.is_success() {
        let msg = body
            .pointer("/error/message")
            .and_then(|v| v.as_str())
            .unwrap_or("Unknown Stripe error");
        anyhow::bail!("Stripe subscription error ({}): {}", status.as_u16(), msg);
    }

    let checkout_url = body
        .get("url")
        .and_then(|v| v.as_str())
        .ok_or_else(|| anyhow::anyhow!("Stripe response missing checkout URL"))?
        .to_string();

    Ok(CheckoutResponse {
        checkout_url,
        transaction_id: transaction_id.to_string(),
        provider: CheckoutProvider::Stripe,
    })
}

/// Cancel an active Stripe Subscription + optionally issue a prorated
/// refund for unused months on the annual plan.
///
/// `subscription_id` is the `sub_…` identifier Stripe hands us in
/// `subscription_data.metadata` on checkout completion. For the annual
/// plan we compute the refund as `remaining_full_months × (plan.price /
/// plan.cycles)` — i.e. unused whole months only. Partial months are
/// forfeited; users who want a cleaner exit can wait until the month
/// tick before cancelling.
///
/// Returns `(cancelled_at, refund_cents)` — `refund_cents = 0` if no
/// refund was issued (monthly plan, or no full months remaining).
pub async fn cancel_stripe_subscription(
    secret_key: &str,
    subscription_id: &str,
    plan: &SubscriptionPlan,
    renewal_at_unix: i64,
) -> anyhow::Result<(i64, u32)> {
    let client = reqwest::Client::new();

    // 1) Cancel the subscription immediately (not at period end) so
    //    Stripe stops billing in the same round-trip.
    let cancel_resp = client
        .delete(format!(
            "https://api.stripe.com/v1/subscriptions/{subscription_id}"
        ))
        .basic_auth(secret_key, Option::<&str>::None)
        .send()
        .await
        .map_err(|e| anyhow::anyhow!("Stripe cancel error: {e}"))?;
    if !cancel_resp.status().is_success() {
        let status = cancel_resp.status();
        let body: serde_json::Value = cancel_resp.json().await.unwrap_or_default();
        let msg = body
            .pointer("/error/message")
            .and_then(|v| v.as_str())
            .unwrap_or("Unknown Stripe error");
        anyhow::bail!("Stripe cancel failed ({}): {}", status.as_u16(), msg);
    }
    let now = chrono::Utc::now().timestamp();

    // 2) No refund on the monthly plan — a user who cancels a $30 month
    //    has already received the 20_000 credits for that cycle. Partial
    //    refund only makes sense on the annual plan.
    if plan.interval != "year" {
        return Ok((now, 0));
    }

    // 3) Compute unused whole months. `renewal_at_unix` points at the
    //    NEXT yearly renewal (start + 365d). Months remaining =
    //    ceil((renewal - now) / 30d) rounded DOWN to whole months so
    //    partial months are forfeited.
    let seconds_remaining = (renewal_at_unix - now).max(0);
    let month_secs: i64 = 30 * 24 * 60 * 60;
    let months_remaining = (seconds_remaining / month_secs) as u32;
    if months_remaining == 0 {
        return Ok((now, 0));
    }

    // Annual plan charge per month = 32_400 / 12 = 2_700 cents ($27).
    let per_month_cents = plan.price_cents / plan.cycles.max(1);
    let refund_cents = months_remaining.saturating_mul(per_month_cents);
    if refund_cents == 0 {
        return Ok((now, 0));
    }

    // 4) Fetch the latest invoice attached to this subscription to grab
    //    its payment_intent — Stripe Refunds API refunds a PaymentIntent
    //    (or Charge), not a subscription directly.
    let invoice_resp = client
        .get("https://api.stripe.com/v1/invoices")
        .basic_auth(secret_key, Option::<&str>::None)
        .query(&[("subscription", subscription_id), ("limit", "1")])
        .send()
        .await
        .map_err(|e| anyhow::anyhow!("Stripe invoice fetch: {e}"))?;
    let invoice_body: serde_json::Value = invoice_resp.json().await.unwrap_or_default();
    let payment_intent_id = invoice_body
        .pointer("/data/0/payment_intent")
        .and_then(|v| v.as_str());
    let Some(pi_id) = payment_intent_id else {
        // Cancellation succeeded but we cannot issue the refund without
        // the payment intent reference. Surface this to the caller as
        // a refund amount of 0 — operator can reconcile manually.
        tracing::warn!(
            subscription_id,
            "subscription cancelled but no payment_intent found for refund"
        );
        return Ok((now, 0));
    };

    let refund_params: Vec<(&str, String)> = vec![
        ("payment_intent", pi_id.to_string()),
        ("amount", refund_cents.to_string()),
        ("reason", "requested_by_customer".to_string()),
        ("metadata[reason]", "subscription_cancel_prorated".to_string()),
        ("metadata[months_remaining]", months_remaining.to_string()),
    ];

    let refund_resp = client
        .post("https://api.stripe.com/v1/refunds")
        .basic_auth(secret_key, Option::<&str>::None)
        .form(&refund_params)
        .send()
        .await
        .map_err(|e| anyhow::anyhow!("Stripe refund error: {e}"))?;
    if !refund_resp.status().is_success() {
        let status = refund_resp.status();
        let body: serde_json::Value = refund_resp.json().await.unwrap_or_default();
        let msg = body
            .pointer("/error/message")
            .and_then(|v| v.as_str())
            .unwrap_or("Unknown Stripe error");
        anyhow::bail!("Stripe refund failed ({}): {}", status.as_u16(), msg);
    }

    Ok((now, refund_cents))
}

// ── Toss 빌링키 (recurring) — spec, 2026-04-22 / 2026-04-23 ─────────
//
// Stripe does not issue merchant accounts to Korean entities, so Toss
// becomes the primary rail for Korean subscribers. The Toss flow is:
//
//   1. Frontend invokes the Toss JS widget with a freshly generated
//      `customerKey` (opaque UUID we own) + a success/fail callback URL.
//   2. User authorises the card inside the Toss popup.
//   3. Toss redirects to the success URL with `authKey` + `customerKey`.
//   4. Our callback endpoint calls `exchange_toss_auth_key` to swap the
//      `authKey` for a reusable `billingKey`, which we persist in
//      `billing_preferences.saved_method_id` (provider = "toss").
//   5. Every subsequent charge (first-cycle + renewals + auto-recharge)
//      calls `charge_toss_billing_key` with the stored `billingKey`.
//
// We never handle raw card data — Toss does. Our DB only ever sees
// the opaque billingKey.

/// Exchange an `authKey` (returned by the Toss billing widget) for a
/// persistent `billingKey` that can charge the user's card on demand.
///
/// `customer_key` must be the same value passed to the widget in step
/// (1). Toss rejects the exchange if they disagree.
pub async fn exchange_toss_auth_key(
    secret_key: &str,
    auth_key: &str,
    customer_key: &str,
) -> anyhow::Result<String> {
    let client = reqwest::Client::new();
    let auth = base64_encode_key(secret_key);
    let body = serde_json::json!({
        "authKey": auth_key,
        "customerKey": customer_key,
    });
    let response = client
        .post("https://api.tosspayments.com/v1/billing/authorizations/issue")
        .header("Authorization", format!("Basic {auth}"))
        .header("Content-Type", "application/json")
        .json(&body)
        .send()
        .await
        .map_err(|e| anyhow::anyhow!("TossPayments auth exchange network error: {e}"))?;
    let status = response.status();
    let resp: serde_json::Value = response
        .json()
        .await
        .map_err(|e| anyhow::anyhow!("TossPayments auth exchange parse error: {e}"))?;
    if !status.is_success() {
        let msg = resp.get("message").and_then(|v| v.as_str()).unwrap_or("unknown");
        anyhow::bail!("TossPayments auth exchange failed ({}): {}", status.as_u16(), msg);
    }
    let billing_key = resp
        .get("billingKey")
        .and_then(|v| v.as_str())
        .ok_or_else(|| anyhow::anyhow!("Toss response missing billingKey"))?
        .to_string();
    Ok(billing_key)
}

/// Charge a previously-issued Toss billing key. Returns the Toss
/// `paymentKey` (handy for refunds) on `DONE` / success. The amount
/// is in KRW because Toss is a Korean rail; USD → KRW conversion
/// happens on the gateway side before this call.
pub async fn charge_toss_billing_key(
    secret_key: &str,
    billing_key: &str,
    customer_key: &str,
    amount_krw: u32,
    order_id: &str,
    order_name: &str,
) -> anyhow::Result<String> {
    let client = reqwest::Client::new();
    let auth = base64_encode_key(secret_key);
    let body = serde_json::json!({
        "customerKey": customer_key,
        "amount": amount_krw,
        "orderId": order_id,
        "orderName": order_name,
    });
    let response = client
        .post(format!(
            "https://api.tosspayments.com/v1/billing/{billing_key}"
        ))
        .header("Authorization", format!("Basic {auth}"))
        .header("Content-Type", "application/json")
        .json(&body)
        .send()
        .await
        .map_err(|e| anyhow::anyhow!("TossPayments charge network error: {e}"))?;
    let status = response.status();
    let resp: serde_json::Value = response
        .json()
        .await
        .map_err(|e| anyhow::anyhow!("TossPayments charge parse error: {e}"))?;
    if !status.is_success() {
        let msg = resp.get("message").and_then(|v| v.as_str()).unwrap_or("unknown");
        anyhow::bail!("TossPayments charge failed ({}): {}", status.as_u16(), msg);
    }
    let toss_status = resp.get("status").and_then(|v| v.as_str()).unwrap_or("");
    if toss_status != "DONE" {
        anyhow::bail!("TossPayments charge status: {}", toss_status);
    }
    Ok(resp
        .get("paymentKey")
        .and_then(|v| v.as_str())
        .unwrap_or("")
        .to_string())
}

/// Ask Toss to cancel a paid order (partial or full). Used for the
/// prorated refund on annual-plan subscription cancellation when the
/// subscriber paid via Toss. `payment_key` is what `charge_toss_billing_key`
/// returned at charge time. `cancel_amount` in KRW; omit for a full cancel.
pub async fn cancel_toss_payment(
    secret_key: &str,
    payment_key: &str,
    cancel_reason: &str,
    cancel_amount_krw: Option<u32>,
) -> anyhow::Result<()> {
    let client = reqwest::Client::new();
    let auth = base64_encode_key(secret_key);
    let mut body = serde_json::json!({ "cancelReason": cancel_reason });
    if let Some(amount) = cancel_amount_krw {
        body["cancelAmount"] = serde_json::Value::from(amount);
    }
    let response = client
        .post(format!(
            "https://api.tosspayments.com/v1/payments/{payment_key}/cancel"
        ))
        .header("Authorization", format!("Basic {auth}"))
        .header("Content-Type", "application/json")
        .json(&body)
        .send()
        .await
        .map_err(|e| anyhow::anyhow!("TossPayments cancel network error: {e}"))?;
    let status = response.status();
    if !status.is_success() {
        let resp: serde_json::Value = response.json().await.unwrap_or_default();
        let msg = resp.get("message").and_then(|v| v.as_str()).unwrap_or("unknown");
        anyhow::bail!("TossPayments cancel failed ({}): {}", status.as_u16(), msg);
    }
    Ok(())
}
