//! Core data models for the Verifiable Intent credential chain.
//!
//! These types mirror the normative specification (credential-format.md,
//! constraints.md) while staying idiomatic Rust.  Monetary amounts use integer
//! minor-units (cents) per ISO 4217 throughout to eliminate decimal ambiguity.

use serde::{Deserialize, Serialize};

// ── JWK / Key material ───────────────────────────────────────────────

/// A JSON Web Key (EC P-256) used for signing and key confirmation.
#[derive(Debug, Clone, Serialize, Deserialize, PartialEq, Eq)]
pub struct Jwk {
    pub kty: String,
    pub crv: String,
    /// Base64url-encoded x coordinate.
    pub x: String,
    /// Base64url-encoded y coordinate.
    pub y: String,
    /// Base64url-encoded private key (only present for signing keys, never serialized to verifiers).
    #[serde(skip_serializing_if = "Option::is_none")]
    pub d: Option<String>,
}

/// Confirmation claim (`cnf`) binding a credential to a public key.
#[derive(Debug, Clone, Serialize, Deserialize, PartialEq, Eq)]
pub struct Cnf {
    pub jwk: Jwk,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub kid: Option<String>,
}

// ── Execution mode ───────────────────────────────────────────────────

/// Whether the VI credential chain uses 2-layer (Immediate) or 3-layer (Autonomous) flow.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum MandateMode {
    /// User confirms final values; no agent delegation.
    Immediate,
    /// User sets constraints; agent acts independently.
    Autonomous,
}

// ── Payment instrument / payee / merchant ────────────────────────────

/// Payment instrument descriptor.
#[derive(Debug, Clone, Serialize, Deserialize, PartialEq, Eq)]
pub struct PaymentInstrument {
    #[serde(rename = "type")]
    pub instrument_type: String,
    pub id: String,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub description: Option<String>,
}

/// Merchant or payee descriptor — used in allowlists and fulfillment.
#[derive(Debug, Clone, Serialize, Deserialize, PartialEq, Eq)]
pub struct Entity {
    #[serde(skip_serializing_if = "Option::is_none")]
    pub id: Option<String>,
    pub name: String,
    pub website: String,
}

impl Entity {
    /// Match two entities by the spec-defined precedence: `id` first, then
    /// (`name`, `website`).
    pub fn matches(&self, other: &Entity) -> bool {
        match (&self.id, &other.id) {
            (Some(a), Some(b)) => a == b,
            _ => self.name == other.name && self.website == other.website,
        }
    }
}

// ── Line items ───────────────────────────────────────────────────────

/// A single item option within a line-item constraint.
#[derive(Debug, Clone, Serialize, Deserialize, PartialEq, Eq)]
pub struct AcceptableItem {
    pub id: String,
    pub title: String,
}

/// A line-item entry in a checkout constraint.
#[derive(Debug, Clone, Serialize, Deserialize, PartialEq, Eq)]
pub struct LineItemEntry {
    pub id: String,
    pub acceptable_items: Vec<AcceptableItem>,
    pub quantity: u32,
}

/// A resolved line item from L3b checkout (fulfillment side).
#[derive(Debug, Clone, Serialize, Deserialize, PartialEq, Eq)]
pub struct FulfillmentLineItem {
    pub item_id: String,
    pub quantity: u32,
}

// ── Constraints ──────────────────────────────────────────────────────

/// Constraint types embedded in L2 Autonomous mandates.
#[derive(Debug, Clone, Serialize, Deserialize, PartialEq, Eq)]
#[serde(tag = "type")]
pub enum Constraint {
    /// Merchant allowlist for checkout mandates.
    #[serde(rename = "mandate.checkout.allowed_merchant")]
    AllowedMerchant { allowed_merchants: Vec<Entity> },

    /// Product selection constraints for checkout mandates.
    #[serde(rename = "mandate.checkout.line_items")]
    LineItems { items: Vec<LineItemEntry> },

    /// Payee allowlist for payment mandates.
    #[serde(rename = "payment.allowed_payee")]
    AllowedPayee { allowed_payees: Vec<Entity> },

    /// Per-transaction amount range.
    #[serde(rename = "payment.amount")]
    PaymentAmount {
        currency: String,
        #[serde(skip_serializing_if = "Option::is_none")]
        min: Option<i64>,
        #[serde(skip_serializing_if = "Option::is_none")]
        max: Option<i64>,
    },

    /// Cumulative budget cap.
    #[serde(rename = "payment.budget")]
    PaymentBudget { currency: String, max: i64 },

    /// Merchant-managed recurring payment.
    #[serde(rename = "payment.recurrence")]
    PaymentRecurrence {
        frequency: String,
        start_date: String,
        #[serde(skip_serializing_if = "Option::is_none")]
        end_date: Option<String>,
        #[serde(skip_serializing_if = "Option::is_none")]
        number: Option<u32>,
    },

    /// Agent-managed recurring purchase.
    #[serde(rename = "payment.agent_recurrence")]
    AgentRecurrence {
        frequency: String,
        start_date: String,
        #[serde(skip_serializing_if = "Option::is_none")]
        end_date: Option<String>,
        #[serde(skip_serializing_if = "Option::is_none")]
        max_occurrences: Option<u32>,
    },

    /// Cross-reference between checkout and payment mandates.
    #[serde(rename = "payment.reference")]
    PaymentReference { conditional_transaction_id: String },
}

// ── Mandate payloads ─────────────────────────────────────────────────

/// Checkout mandate — Immediate mode (final values).
#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct FinalCheckoutMandate {
    pub vct: String, // "mandate.checkout"
    pub checkout_jwt: String,
    pub checkout_hash: String,
}

/// Payment mandate — Immediate mode (final values).
#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct FinalPaymentMandate {
    pub vct: String, // "mandate.payment"
    pub payment_instrument: PaymentInstrument,
    pub currency: String,
    pub amount: i64,
    pub payee: Entity,
    pub transaction_id: String,
}

/// Checkout mandate — Autonomous mode (constraints + agent key binding).
#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct OpenCheckoutMandate {
    pub vct: String, // "mandate.checkout.open"
    pub cnf: Cnf,
    pub constraints: Vec<Constraint>,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub prompt_summary: Option<String>,
}

/// Payment mandate — Autonomous mode (constraints + agent key binding).
#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct OpenPaymentMandate {
    pub vct: String, // "mandate.payment.open"
    pub cnf: Cnf,
    pub payment_instrument: PaymentInstrument,
    pub constraints: Vec<Constraint>,
}

/// L3a — agent-signed final payment values sent to the payment network.
#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct PaymentL3Mandate {
    pub vct: String, // "mandate.payment"
    pub payment_instrument: PaymentInstrument,
    pub payment_amount: PaymentAmount,
    pub payee: Entity,
    pub transaction_id: String,
}

/// L3b — agent-signed final checkout values sent to the merchant.
#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct CheckoutL3Mandate {
    pub vct: String, // "mandate.checkout"
    pub checkout_jwt: String,
    pub checkout_hash: String,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub line_items: Option<Vec<FulfillmentLineItem>>,
}

/// Nested amount object for L3a payment mandates.
#[derive(Debug, Clone, Serialize, Deserialize, PartialEq, Eq)]
pub struct PaymentAmount {
    pub currency: String,
    pub amount: i64,
}

// ── Fulfillment (verifier-constructed from L3) ───────────────────────

/// Verifier-constructed fulfillment object derived from L3 mandates.
/// Used as the input to constraint validation.
#[derive(Debug, Clone, Serialize, Deserialize, Default)]
pub struct Fulfillment {
    #[serde(skip_serializing_if = "Option::is_none")]
    pub line_items: Option<Vec<FulfillmentLineItem>>,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub merchant: Option<Entity>,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub payee: Option<Entity>,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub payment_instrument: Option<PaymentInstrument>,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub currency: Option<String>,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub amount: Option<i64>,
}

// ── Credential chain layers (serialized form) ────────────────────────

/// Parsed representation of an L1 SD-JWT (credential provider → user).
#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct Layer1 {
    pub iss: String,
    pub sub: String,
    pub iat: i64,
    pub exp: i64,
    pub vct: String,
    pub cnf: Cnf,
    pub pan_last_four: String,
    pub scheme: String,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub card_id: Option<String>,
}

/// Parsed representation of an L2 KB-SD-JWT (user → agent/verifier).
#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct Layer2 {
    pub nonce: String,
    pub aud: String,
    pub iat: i64,
    pub exp: i64,
    pub sd_hash: String,
    pub mode: MandateMode,
    /// In Immediate mode: contains `FinalCheckoutMandate` + `FinalPaymentMandate`.
    /// In Autonomous mode: contains `OpenCheckoutMandate` + `OpenPaymentMandate`.
    pub mandates: Vec<serde_json::Value>,
}

/// Parsed representation of the full credential chain (L1 + L2 + optional L3).
#[derive(Debug, Clone)]
pub struct CredentialChain {
    pub l1: Layer1,
    pub l2: Layer2,
    /// Only present in Autonomous mode.
    pub l3a: Option<PaymentL3Mandate>,
    /// Only present in Autonomous mode.
    pub l3b: Option<CheckoutL3Mandate>,
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn entity_matches_by_id() {
        let a = Entity {
            id: Some("m-1".into()),
            name: "Merchant A".into(),
            website: "https://a.example.com".into(),
        };
        let b = Entity {
            id: Some("m-1".into()),
            name: "Different Name".into(),
            website: "https://different.example.com".into(),
        };
        assert!(a.matches(&b));
    }

    #[test]
    fn entity_matches_by_name_website_when_no_id() {
        let a = Entity {
            id: None,
            name: "Merchant A".into(),
            website: "https://a.example.com".into(),
        };
        let b = Entity {
            id: None,
            name: "Merchant A".into(),
            website: "https://a.example.com".into(),
        };
        assert!(a.matches(&b));
    }

    #[test]
    fn entity_no_match() {
        let a = Entity {
            id: None,
            name: "Merchant A".into(),
            website: "https://a.example.com".into(),
        };
        let b = Entity {
            id: None,
            name: "Merchant B".into(),
            website: "https://b.example.com".into(),
        };
        assert!(!a.matches(&b));
    }

    #[test]
    fn constraint_serde_roundtrip() {
        let c = Constraint::PaymentAmount {
            currency: "USD".into(),
            min: Some(10000),
            max: Some(40000),
        };
        let json = serde_json::to_string(&c).unwrap();
        assert!(json.contains("payment.amount"));
        let back: Constraint = serde_json::from_str(&json).unwrap();
        assert_eq!(c, back);
    }

    #[test]
    fn constraint_merchant_serde_roundtrip() {
        let c = Constraint::AllowedMerchant {
            allowed_merchants: vec![Entity {
                id: None,
                name: "Test Store".into(),
                website: "https://test.example.com".into(),
            }],
        };
        let json = serde_json::to_string(&c).unwrap();
        assert!(json.contains("mandate.checkout.allowed_merchant"));
        let back: Constraint = serde_json::from_str(&json).unwrap();
        assert_eq!(c, back);
    }

    #[test]
    fn mandate_mode_serde() {
        let m = MandateMode::Autonomous;
        let json = serde_json::to_string(&m).unwrap();
        assert_eq!(json, r#""autonomous""#);
        let back: MandateMode = serde_json::from_str(&json).unwrap();
        assert_eq!(m, back);
    }
}
