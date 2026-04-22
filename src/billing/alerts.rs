//! Low-balance alert dispatcher.
//!
//! Spec (2026-04-22): when a user's credit balance crosses their
//! chosen low-balance threshold (3,000 or 5,000) on a *downward* edge
//! — i.e., the most recent observed balance was above the threshold
//! and the new balance is at or below — fire an alert through every
//! channel the user has opted into. The in-app banner
//! (`LowBalanceBanner` on the chat view) is always the primary
//! surface; email and SMS are additive.
//!
//! Design:
//! - Per-user "last observed above threshold" state lives in memory
//!   (Mutex). No persistence needed: on restart we simply re-arm, so
//!   the worst case is one duplicate email after a deploy.
//! - Email uses the same SMTP credentials as
//!   `auth::email_verify::EmailVerifyService` (via
//!   `EmailVerificationConfig`) so operators don't configure SMTP
//!   twice.
//! - SMS goes through Twilio's classic REST API. Env vars
//!   `TWILIO_ACCOUNT_SID`, `TWILIO_AUTH_TOKEN`, `TWILIO_FROM_NUMBER`
//!   gate whether SMS is available at all. Future work: swap Twilio
//!   for a Korean carrier (Aligo, CoolSMS) with the same trait.
//!
//! Non-goals:
//! - Push notifications (handled by the Tauri frontend).
//! - Rate limiting beyond "one alert per downward edge": if the user
//!   recharges above the threshold and drops below again, we fire
//!   again — that is the intended behaviour.

use std::collections::HashMap;
use std::sync::Arc;

use parking_lot::Mutex;
use serde::{Deserialize, Serialize};

use crate::config::schema::EmailVerificationConfig;

/// Tracks which users have already been notified since they last went
/// above-threshold, so we don't spam them on every balance poll.
///
/// Keyed by `user_id`. Value `true` = "user is currently below the
/// threshold and has already received the alert on this descent".
#[derive(Default)]
pub struct LowBalanceAlertState {
    notified: Mutex<HashMap<String, bool>>,
}

impl LowBalanceAlertState {
    pub fn new() -> Arc<Self> {
        Arc::new(Self::default())
    }

    /// Decide whether to fire an alert for `user_id` given their
    /// current balance and configured threshold. Returns `true` when
    /// this is the first observation at-or-below the threshold since
    /// the last time the balance was above it.
    pub fn should_fire(&self, user_id: &str, balance: u32, threshold: u32) -> bool {
        let mut guard = self.notified.lock();
        let already_notified = guard.get(user_id).copied().unwrap_or(false);
        if balance > threshold {
            // Back above threshold → arm for the next descent.
            guard.remove(user_id);
            return false;
        }
        if already_notified {
            return false;
        }
        guard.insert(user_id.to_string(), true);
        true
    }
}

/// Preferences specific to alert routing. Stored inside the broader
/// `BillingPreferences` struct in `payment.rs` via additive columns.
#[derive(Debug, Clone, Default, Serialize, Deserialize)]
pub struct AlertChannels {
    pub email_enabled: bool,
    pub email_address: Option<String>,
    pub sms_enabled: bool,
    pub sms_phone: Option<String>,
}

/// Send an email low-balance alert via the same SMTP credentials the
/// email-verify subsystem already uses. Non-fatal on failure — the
/// in-app banner is the primary surface, and we don't want a flaky
/// SMTP server to block billing writes.
pub fn send_low_balance_email(
    smtp: &EmailVerificationConfig,
    to_email: &str,
    balance: u32,
    threshold: u32,
) -> anyhow::Result<()> {
    use lettre::message::header::ContentType;
    use lettre::transport::smtp::authentication::Credentials;
    use lettre::{Message, SmtpTransport, Transport};

    let (Some(host), Some(from)) = (smtp.smtp_host.as_ref(), smtp.from_email.as_ref()) else {
        anyhow::bail!("SMTP host / from_email not configured");
    };

    let subject = "MoA credits running low";
    let body = format!(
        "Your MoA credit balance has fallen to {balance} (your alert threshold is {threshold}).\n\n\
         Open the billing page to top up or tune the auto-recharge settings:\n\
         https://mymoa.app/billing\n\n\
         This is an automated message — no action is required if you already tuned auto-recharge."
    );

    let email = Message::builder()
        .from(from.parse()?)
        .to(to_email.parse()?)
        .subject(subject)
        .header(ContentType::TEXT_PLAIN)
        .body(body)?;

    let mut builder = SmtpTransport::relay(host)?.port(smtp.smtp_port);
    if let (Some(user), Some(pass)) = (smtp.smtp_username.as_ref(), smtp.smtp_password.as_ref()) {
        builder = builder.credentials(Credentials::new(user.clone(), pass.clone()));
    }
    let transport = builder.build();
    transport.send(&email)?;
    Ok(())
}

/// Send an SMS low-balance alert via Twilio's REST API. Requires the
/// three Twilio env vars to be present — any missing var aborts the
/// call with an `Err`. Returns `Ok(())` on `201 Created`.
pub async fn send_low_balance_sms(
    to_phone: &str,
    balance: u32,
    threshold: u32,
) -> anyhow::Result<()> {
    let account_sid = std::env::var("TWILIO_ACCOUNT_SID")
        .map_err(|_| anyhow::anyhow!("TWILIO_ACCOUNT_SID not set"))?;
    let auth_token = std::env::var("TWILIO_AUTH_TOKEN")
        .map_err(|_| anyhow::anyhow!("TWILIO_AUTH_TOKEN not set"))?;
    let from_number = std::env::var("TWILIO_FROM_NUMBER")
        .map_err(|_| anyhow::anyhow!("TWILIO_FROM_NUMBER not set"))?;

    let client = reqwest::Client::new();
    let url = format!(
        "https://api.twilio.com/2010-04-01/Accounts/{account_sid}/Messages.json"
    );
    let body = format!(
        "[MoA] Credits low: {balance}/{threshold}. Top up: mymoa.app/billing"
    );
    let params = [
        ("From", from_number.as_str()),
        ("To", to_phone),
        ("Body", body.as_str()),
    ];

    let resp = client
        .post(url)
        .basic_auth(account_sid, Some(auth_token))
        .form(&params)
        .send()
        .await?;
    let status = resp.status();
    if !status.is_success() {
        let body: serde_json::Value = resp.json().await.unwrap_or_default();
        let msg = body.get("message").and_then(|v| v.as_str()).unwrap_or("unknown");
        anyhow::bail!("Twilio {}: {}", status.as_u16(), msg);
    }
    Ok(())
}

/// Orchestrate the per-user alert decision. Called from the chat /
/// usage-recording path after each `deduct_credits` round-trip.
///
/// - `state`         : shared edge-detection state (one per gateway).
/// - `balance`       : balance after this deduction.
/// - `channels`      : user's opt-in channel preferences.
/// - `threshold`     : user's chosen low-balance threshold.
/// - `smtp_cfg`      : SMTP config (may be disabled; function is a no-op then).
///
/// Returns a struct describing what fired so the caller can log it.
#[derive(Debug, Default, Serialize)]
pub struct AlertDispatchOutcome {
    pub email_sent: bool,
    pub sms_sent: bool,
    pub errors: Vec<String>,
}

pub async fn maybe_fire_low_balance_alert(
    state: &LowBalanceAlertState,
    user_id: &str,
    balance: u32,
    threshold: u32,
    channels: &AlertChannels,
    smtp_cfg: &EmailVerificationConfig,
) -> AlertDispatchOutcome {
    if !state.should_fire(user_id, balance, threshold) {
        return AlertDispatchOutcome::default();
    }
    let mut outcome = AlertDispatchOutcome::default();

    if channels.email_enabled {
        if let Some(addr) = channels.email_address.as_deref() {
            match send_low_balance_email(smtp_cfg, addr, balance, threshold) {
                Ok(()) => outcome.email_sent = true,
                Err(e) => outcome.errors.push(format!("email: {e}")),
            }
        } else {
            outcome.errors.push("email enabled but no address".into());
        }
    }

    if channels.sms_enabled {
        if let Some(phone) = channels.sms_phone.as_deref() {
            match send_low_balance_sms(phone, balance, threshold).await {
                Ok(()) => outcome.sms_sent = true,
                Err(e) => outcome.errors.push(format!("sms: {e}")),
            }
        } else {
            outcome.errors.push("sms enabled but no phone".into());
        }
    }

    outcome
}

/// Convenience entry point for the chat handler: load the user's
/// alert channel preferences from `PaymentManager`, then dispatch
/// the low-balance email + SMS via `maybe_fire_low_balance_alert`.
///
/// Returns the same `AlertDispatchOutcome` so the caller can log /
/// surface results. Non-fatal on every failure path: if preferences
/// can't be loaded or SMTP/Twilio credentials are missing, the
/// outcome simply records the error and the in-app banner remains
/// the primary surface.
pub async fn dispatch_for_user(
    state: &LowBalanceAlertState,
    payment_manager: &crate::billing::PaymentManager,
    user_id: &str,
    balance: u32,
    smtp_cfg: &crate::config::schema::EmailVerificationConfig,
) -> AlertDispatchOutcome {
    let prefs = match payment_manager.get_billing_preferences(user_id) {
        Ok(p) => p,
        Err(e) => {
            return AlertDispatchOutcome {
                errors: vec![format!("preferences lookup failed: {e}")],
                ..Default::default()
            };
        }
    };
    let channels = AlertChannels {
        email_enabled: prefs.alert_email_enabled,
        email_address: prefs.alert_email_address.clone(),
        sms_enabled: prefs.alert_sms_enabled,
        sms_phone: prefs.alert_sms_phone.clone(),
    };
    maybe_fire_low_balance_alert(
        state,
        user_id,
        balance,
        prefs.low_balance_threshold,
        &channels,
        smtp_cfg,
    )
    .await
}
