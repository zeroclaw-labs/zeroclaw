//! x402 payment protocol — HEAD→402→sign→retry flow.
//!
//! When an HTTP resource returns 402 Payment Required, the agent:
//! 1. Extracts the payment details from the response headers
//! 2. Signs a payment authorization via EIP-712
//! 3. Retries the request with the signed payment header

use super::keypair::WalletKeypair;
use super::signing::Eip712Signer;
use alloy_primitives::{Address, U256};
use serde::{Deserialize, Serialize};

/// Result of an x402 payment attempt.
#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct X402PaymentResult {
    pub success: bool,
    pub status_code: u16,
    pub payment_amount: Option<String>,
    pub recipient: Option<String>,
    pub error: Option<String>,
}

/// Treasury limits for x402 payments.
#[derive(Debug, Clone)]
pub struct TreasuryLimits {
    pub max_payment_cents: u64,
    pub allowed_domains: Vec<String>,
    pub max_daily_spend_cents: u64,
    pub max_monthly_spend_cents: u64,
    pub daily_spent_cents: u64,
    pub monthly_spent_cents: u64,
}

impl TreasuryLimits {
    /// Check if a payment is within treasury limits.
    pub fn can_pay(&self, amount_cents: u64, domain: &str) -> Result<(), String> {
        if amount_cents > self.max_payment_cents {
            return Err(format!(
                "Payment {amount_cents}c exceeds max per-payment limit of {}c",
                self.max_payment_cents
            ));
        }

        if self.daily_spent_cents + amount_cents > self.max_daily_spend_cents {
            return Err(format!(
                "Payment would exceed daily spend limit of {}c (already spent {}c)",
                self.max_daily_spend_cents, self.daily_spent_cents
            ));
        }

        if self.monthly_spent_cents + amount_cents > self.max_monthly_spend_cents {
            return Err(format!(
                "Payment would exceed monthly spend limit of {}c (already spent {}c)",
                self.max_monthly_spend_cents, self.monthly_spent_cents
            ));
        }

        if !self.allowed_domains.is_empty()
            && !self.allowed_domains.iter().any(|d| domain.ends_with(d))
        {
            return Err(format!(
                "Domain '{domain}' not in allowed x402 domains: {:?}",
                self.allowed_domains
            ));
        }

        Ok(())
    }
}

/// Client for the x402 payment protocol.
pub struct X402Client;

impl X402Client {
    /// Execute the x402 payment flow for a URL.
    ///
    /// 1. HEAD request to check if 402 is required
    /// 2. Extract payment parameters from response
    /// 3. Sign payment authorization
    /// 4. Retry with signed payment header
    pub async fn pay_and_fetch(
        http_client: &reqwest::Client,
        url: &str,
        keypair: &WalletKeypair,
        treasury: &TreasuryLimits,
    ) -> anyhow::Result<X402PaymentResult> {
        // Step 1: HEAD request to check for 402
        let head_resp = http_client
            .head(url)
            .send()
            .await
            .map_err(|e| anyhow::anyhow!("HEAD request failed: {e}"))?;

        if head_resp.status() != reqwest::StatusCode::PAYMENT_REQUIRED {
            return Ok(X402PaymentResult {
                success: true,
                status_code: head_resp.status().as_u16(),
                payment_amount: None,
                recipient: None,
                error: None,
            });
        }

        // Step 2: Extract payment parameters from headers
        let recipient_str = head_resp
            .headers()
            .get("x-payment-recipient")
            .and_then(|v| v.to_str().ok())
            .ok_or_else(|| anyhow::anyhow!("402 response missing x-payment-recipient header"))?;

        let amount_str = head_resp
            .headers()
            .get("x-payment-amount")
            .and_then(|v| v.to_str().ok())
            .ok_or_else(|| anyhow::anyhow!("402 response missing x-payment-amount header"))?;

        let chain_id: u64 = head_resp
            .headers()
            .get("x-payment-chain-id")
            .and_then(|v| v.to_str().ok())
            .and_then(|v| v.parse().ok())
            .unwrap_or(8453); // Default to Base

        let nonce: u64 = head_resp
            .headers()
            .get("x-payment-nonce")
            .and_then(|v| v.to_str().ok())
            .and_then(|v| v.parse().ok())
            .unwrap_or(1);

        let expiry: u64 = head_resp
            .headers()
            .get("x-payment-expiry")
            .and_then(|v| v.to_str().ok())
            .and_then(|v| v.parse().ok())
            .unwrap_or(u64::MAX);

        // Parse amounts
        let recipient: Address = recipient_str
            .parse()
            .map_err(|e| anyhow::anyhow!("Invalid recipient address: {e}"))?;
        let amount: U256 = amount_str
            .parse()
            .map_err(|e| anyhow::anyhow!("Invalid payment amount: {e}"))?;

        // Extract domain for treasury check
        let domain = reqwest::Url::parse(url)
            .ok()
            .and_then(|u| u.host_str().map(|h| h.to_string()))
            .unwrap_or_default();

        // Step 3: Treasury enforcement
        // Convert wei to cents approximation (1 ETH ≈ $3000, 1 cent = 1e16 wei)
        let amount_cents = amount
            .checked_div(U256::from(10_000_000_000_000_000u64))
            .map(|v| v.to::<u64>())
            .unwrap_or(u64::MAX);

        if let Err(e) = treasury.can_pay(amount_cents, &domain) {
            return Ok(X402PaymentResult {
                success: false,
                status_code: 402,
                payment_amount: Some(amount_str.to_string()),
                recipient: Some(recipient_str.to_string()),
                error: Some(format!("Treasury rejected: {e}")),
            });
        }

        // Step 4: Sign payment
        let signed =
            Eip712Signer::sign_payment(keypair, recipient, amount, nonce, expiry, chain_id).await?;

        // Step 5: Retry with payment header
        let payment_json = serde_json::to_string(&signed)?;
        let retry_resp = http_client
            .get(url)
            .header("x-payment", &payment_json)
            .send()
            .await
            .map_err(|e| anyhow::anyhow!("Paid request failed: {e}"))?;

        Ok(X402PaymentResult {
            success: retry_resp.status().is_success(),
            status_code: retry_resp.status().as_u16(),
            payment_amount: Some(amount_str.to_string()),
            recipient: Some(recipient_str.to_string()),
            error: if retry_resp.status().is_success() {
                None
            } else {
                Some(format!("Server returned {}", retry_resp.status()))
            },
        })
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    fn test_limits() -> TreasuryLimits {
        TreasuryLimits {
            max_payment_cents: 100,
            allowed_domains: vec!["example.com".into(), "api.test.io".into()],
            max_daily_spend_cents: 500,
            max_monthly_spend_cents: 5000,
            daily_spent_cents: 0,
            monthly_spent_cents: 0,
        }
    }

    #[test]
    fn treasury_allows_within_limits() {
        let limits = test_limits();
        assert!(limits.can_pay(50, "example.com").is_ok());
        assert!(limits.can_pay(100, "sub.example.com").is_ok());
        assert!(limits.can_pay(1, "api.test.io").is_ok());
    }

    #[test]
    fn treasury_rejects_over_per_payment_limit() {
        let limits = test_limits();
        let err = limits.can_pay(101, "example.com").unwrap_err();
        assert!(err.contains("exceeds max per-payment"));
    }

    #[test]
    fn treasury_rejects_over_daily_limit() {
        let limits = TreasuryLimits {
            daily_spent_cents: 450,
            ..test_limits()
        };
        let err = limits.can_pay(51, "example.com").unwrap_err();
        assert!(err.contains("daily spend limit"));
    }

    #[test]
    fn treasury_rejects_over_monthly_limit() {
        let limits = TreasuryLimits {
            monthly_spent_cents: 4950,
            ..test_limits()
        };
        let err = limits.can_pay(51, "example.com").unwrap_err();
        assert!(err.contains("monthly spend limit"));
    }

    #[test]
    fn treasury_rejects_disallowed_domain() {
        let limits = test_limits();
        let err = limits.can_pay(10, "evil.com").unwrap_err();
        assert!(err.contains("not in allowed x402 domains"));
    }

    #[test]
    fn treasury_allows_any_domain_when_list_empty() {
        let limits = TreasuryLimits {
            allowed_domains: vec![],
            ..test_limits()
        };
        assert!(limits.can_pay(50, "any-domain.com").is_ok());
    }

    #[test]
    fn x402_payment_result_serde_roundtrip() {
        let result = X402PaymentResult {
            success: true,
            status_code: 200,
            payment_amount: Some("1000000".into()),
            recipient: Some("0xaabb".into()),
            error: None,
        };
        let json = serde_json::to_string(&result).unwrap();
        let parsed: X402PaymentResult = serde_json::from_str(&json).unwrap();
        assert!(parsed.success);
        assert_eq!(parsed.status_code, 200);
    }

    #[test]
    fn treasury_boundary_exact_daily_limit() {
        let limits = TreasuryLimits {
            daily_spent_cents: 400,
            ..test_limits()
        };
        // Exactly at limit — allowed
        assert!(limits.can_pay(100, "example.com").is_ok());
        // One cent over — rejected
        let err = limits.can_pay(101, "example.com").unwrap_err();
        assert!(err.contains("exceeds max per-payment"));
    }
}
