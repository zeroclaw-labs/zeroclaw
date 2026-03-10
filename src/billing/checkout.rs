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

/// Available credit packages.
pub const USD_PACKAGES: &[UsdCreditPackage] = &[
    UsdCreditPackage {
        id: "starter_10",
        name: "Starter",
        price_cents: 1000,  // $10
        credits: 1_500,
        price_krw: 14_000,
    },
    UsdCreditPackage {
        id: "standard_20",
        name: "Standard",
        price_cents: 2000,  // $20
        credits: 3_200,
        price_krw: 28_000,
    },
    UsdCreditPackage {
        id: "power_50",
        name: "Power",
        price_cents: 5000,  // $50
        credits: 8_500,
        price_krw: 69_000,
    },
];

/// Auto-recharge threshold: recharge when balance drops below this.
/// ~$1 worth of credits (1 credit ≈ $0.007).
pub const AUTO_RECHARGE_THRESHOLD: u32 = 143;

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
        ("success_url", format!("{callback_base_url}/api/checkout/success?tx={transaction_id}&provider=stripe")),
        ("cancel_url", format!("{callback_base_url}/api/checkout/cancel?tx={transaction_id}")),
        ("client_reference_id", transaction_id.to_string()),
        ("metadata[user_id]", user_id.to_string()),
        ("metadata[package_id]", package.id.to_string()),
        ("metadata[transaction_id]", transaction_id.to_string()),
    ];

    if mode == "payment" {
        params.extend([
            ("line_items[0][price_data][currency]", "usd".to_string()),
            ("line_items[0][price_data][unit_amount]", package.price_cents.to_string()),
            ("line_items[0][price_data][product_data][name]", format!("MoA Credits — {} ({} credits)", package.name, package.credits)),
            ("line_items[0][quantity]", "1".to_string()),
        ]);
    }

    if save_method {
        params.push(("payment_intent_data[setup_future_usage]", "off_session".to_string()));
    }

    let response = client
        .post("https://api.stripe.com/v1/checkout/sessions")
        .basic_auth(secret_key, Option::<&str>::None)
        .form(&params)
        .send()
        .await
        .map_err(|e| anyhow::anyhow!("Stripe API error: {e}"))?;

    let status = response.status();
    let body: serde_json::Value = response.json().await
        .map_err(|e| anyhow::anyhow!("Stripe response parse error: {e}"))?;

    if !status.is_success() {
        let msg = body.pointer("/error/message")
            .and_then(|v| v.as_str())
            .unwrap_or("Unknown Stripe error");
        anyhow::bail!("Stripe error ({}): {}", status.as_u16(), msg);
    }

    let checkout_url = body.get("url")
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

    let timestamp = parts.get("t")
        .ok_or_else(|| anyhow::anyhow!("Missing timestamp in Stripe signature"))?;
    let v1_sig = parts.get("v1")
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
    let resp_body: serde_json::Value = response.json().await
        .map_err(|e| anyhow::anyhow!("TossPayments response parse error: {e}"))?;

    if !status.is_success() {
        let msg = resp_body.get("message")
            .and_then(|v| v.as_str())
            .unwrap_or("Unknown TossPayments error");
        anyhow::bail!("TossPayments error ({}): {}", status.as_u16(), msg);
    }

    let checkout_url = resp_body.get("checkout")
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
        let msg = resp_body.get("message")
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

    let package = find_usd_package(&settings.package_id)
        .ok_or_else(|| anyhow::anyhow!("Auto-recharge package not found: {}", settings.package_id))?;

    let saved_method = settings.saved_method_id.as_deref()
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
            let status_str = body.get("status")
                .and_then(|v| v.as_str())
                .unwrap_or("");

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
            // TossPayments billing key auto-charge
            let key = toss_key
                .ok_or_else(|| anyhow::anyhow!("Toss key not configured for auto-recharge"))?;

            let auth = base64_encode_key(key);
            let client = reqwest::Client::new();

            let body = serde_json::json!({
                "billingKey": saved_method,
                "customerKey": user_id,
                "amount": package.price_krw,
                "orderId": transaction_id,
                "orderName": format!("MoA 크레딧 자동충전 — {} ({}크레딧)", package.name, package.credits),
            });

            let response = client
                .post("https://api.tosspayments.com/v1/billing/authorizations/card")
                .header("Authorization", format!("Basic {auth}"))
                .header("Content-Type", "application/json")
                .json(&body)
                .send()
                .await?;

            let resp: serde_json::Value = response.json().await?;
            let toss_status = resp.get("status")
                .and_then(|v| v.as_str())
                .unwrap_or("");

            if toss_status == "DONE" {
                tracing::info!(
                    user_id,
                    package_id = package.id,
                    credits = package.credits,
                    "Auto-recharge succeeded via TossPayments"
                );
                Ok(Some(transaction_id))
            } else {
                anyhow::bail!("Auto-recharge TossPayments status: {}", toss_status);
            }
        }
    }
}
