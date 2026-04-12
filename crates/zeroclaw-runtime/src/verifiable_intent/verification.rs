//! Chain verification, constraint checking, and binding integrity validation.
//!
//! Implements the normative verification algorithms from the VI specification:
//! - Full credential chain verification (L1 → L2 → L3)
//! - Per-constraint validation against fulfillment data
//! - Cross-reference and hash binding integrity checks

use crate::verifiable_intent::error::{ViError, ViErrorKind};
use crate::verifiable_intent::types::{
    CheckoutL3Mandate, Constraint, Entity, Fulfillment, LineItemEntry, MandateMode,
    PaymentL3Mandate,
};

// ── Strictness mode ──────────────────────────────────────────────────

/// Controls behavior when an unknown constraint type is encountered.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum StrictnessMode {
    /// Unknown constraint types cause a violation (fail-closed).
    Strict,
    /// Unknown constraint types are skipped with a warning (fail-open).
    Permissive,
}

// ── Chain verification result ────────────────────────────────────────

/// Result of verifying the credential chain (L1 → L2 → optional L3).
#[derive(Debug, Clone)]
pub struct ChainVerificationResult {
    pub valid: bool,
    pub mode: Option<MandateMode>,
    pub errors: Vec<ViError>,
}

impl ChainVerificationResult {
    pub fn ok(mode: MandateMode) -> Self {
        Self {
            valid: true,
            mode: Some(mode),
            errors: vec![],
        }
    }

    pub fn fail(errors: Vec<ViError>) -> Self {
        Self {
            valid: false,
            mode: None,
            errors,
        }
    }
}

// ── Constraint check result ──────────────────────────────────────────

/// Result of evaluating a single constraint against fulfillment data.
#[derive(Debug, Clone)]
pub struct ConstraintCheckResult {
    pub satisfied: bool,
    pub constraint_type: String,
    pub violations: Vec<ViError>,
}

impl ConstraintCheckResult {
    pub fn ok(constraint_type: &str) -> Self {
        Self {
            satisfied: true,
            constraint_type: constraint_type.into(),
            violations: vec![],
        }
    }

    pub fn violation(constraint_type: &str, err: ViError) -> Self {
        Self {
            satisfied: false,
            constraint_type: constraint_type.into(),
            violations: vec![err],
        }
    }
}

// ── Time validation ──────────────────────────────────────────────────

const CLOCK_SKEW_SECS: i64 = 300;

fn current_timestamp() -> i64 {
    chrono::Utc::now().timestamp()
}

/// Verify `iat` and `exp` claims with a 300-second clock skew tolerance.
pub fn verify_timestamps(iat: i64, exp: i64) -> Result<(), ViError> {
    let now = current_timestamp();
    if exp + CLOCK_SKEW_SECS < now {
        return Err(ViError::new(
            ViErrorKind::Expired,
            format!("credential expired at {exp}, now {now}"),
        ));
    }
    if iat - CLOCK_SKEW_SECS > now {
        return Err(ViError::new(
            ViErrorKind::NotYetValid,
            format!("credential not valid until {iat}, now {now}"),
        ));
    }
    Ok(())
}

// ── sd_hash binding ──────────────────────────────────────────────────

/// Verify that `expected_hash` equals `B64U(SHA-256(ASCII(serialized_parent)))`.
pub fn verify_sd_hash_binding(expected_hash: &str, serialized_parent: &str) -> Result<(), ViError> {
    let computed = crate::verifiable_intent::crypto::sd_hash(serialized_parent);
    if computed != expected_hash {
        return Err(ViError::new(
            ViErrorKind::SdHashMismatch,
            format!("sd_hash mismatch: expected {expected_hash}, computed {computed}"),
        ));
    }
    Ok(())
}

// ── L3 cross-reference binding ───────────────────────────────────────

/// Verify that L3a `transaction_id` equals L3b `checkout_hash`.
pub fn verify_l3_cross_reference(
    l3a: &PaymentL3Mandate,
    l3b: &CheckoutL3Mandate,
) -> Result<(), ViError> {
    if l3a.transaction_id != l3b.checkout_hash {
        return Err(ViError::new(
            ViErrorKind::CrossReferenceMismatch,
            format!(
                "L3a transaction_id ({}) != L3b checkout_hash ({})",
                l3a.transaction_id, l3b.checkout_hash
            ),
        ));
    }
    Ok(())
}

/// Verify checkout_hash is `B64U(SHA-256(ASCII(checkout_jwt)))`.
pub fn verify_checkout_hash_binding(
    checkout_hash: &str,
    checkout_jwt: &str,
) -> Result<(), ViError> {
    let computed = crate::verifiable_intent::crypto::sd_hash(checkout_jwt);
    if computed != checkout_hash {
        return Err(ViError::new(
            ViErrorKind::CrossReferenceMismatch,
            format!("checkout_hash mismatch: expected {checkout_hash}, computed {computed}"),
        ));
    }
    Ok(())
}

// ── Mandate mode inference ───────────────────────────────────────────

/// Infer the execution mode from mandate VCT values.
pub fn infer_mode_from_vct(vct: &str) -> Result<MandateMode, ViError> {
    match vct {
        "mandate.checkout" | "mandate.payment" => Ok(MandateMode::Immediate),
        "mandate.checkout.open" | "mandate.payment.open" => Ok(MandateMode::Autonomous),
        _ => Err(ViError::new(
            ViErrorKind::UnknownMandateType,
            format!("unrecognized mandate VCT: {vct}"),
        )),
    }
}

// ── Constraint validation ────────────────────────────────────────────

/// Evaluate all constraints against fulfillment data.
pub fn check_constraints(
    constraints: &[Constraint],
    fulfillment: &Fulfillment,
    strictness: StrictnessMode,
) -> Vec<ConstraintCheckResult> {
    constraints
        .iter()
        .map(|c| check_single_constraint(c, fulfillment, strictness))
        .collect()
}

fn check_single_constraint(
    constraint: &Constraint,
    fulfillment: &Fulfillment,
    _strictness: StrictnessMode,
) -> ConstraintCheckResult {
    match constraint {
        Constraint::AllowedMerchant { allowed_merchants } => {
            check_allowed_merchant(allowed_merchants, fulfillment)
        }
        Constraint::LineItems { items } => check_line_items(items, fulfillment),
        Constraint::AllowedPayee { allowed_payees } => {
            check_allowed_payee(allowed_payees, fulfillment)
        }
        Constraint::PaymentAmount { currency, min, max } => {
            check_payment_amount(currency, *min, *max, fulfillment)
        }
        Constraint::PaymentBudget { currency, max } => {
            check_payment_budget(currency, *max, fulfillment)
        }
        Constraint::PaymentReference {
            conditional_transaction_id,
        } => {
            // Reference binding is verified structurally, not against fulfillment.
            ConstraintCheckResult::ok(&format!(
                "payment.reference({})",
                &conditional_transaction_id[..8.min(conditional_transaction_id.len())]
            ))
        }
        Constraint::PaymentRecurrence { .. } | Constraint::AgentRecurrence { .. } => {
            // Recurrence constraints are informational for the payment network
            // to enforce statefulness. Pass-through at the agent level.
            ConstraintCheckResult::ok("recurrence")
        }
    }
}

// ── Individual constraint checkers ───────────────────────────────────

fn check_allowed_merchant(
    allowed_merchants: &[Entity],
    fulfillment: &Fulfillment,
) -> ConstraintCheckResult {
    let ct = "mandate.checkout.allowed_merchant";
    if allowed_merchants.is_empty() {
        return ConstraintCheckResult::violation(
            ct,
            ViError::new(
                ViErrorKind::MerchantNotAllowed,
                "empty merchant allowlist is unsatisfiable",
            ),
        );
    }
    let Some(merchant) = &fulfillment.merchant else {
        // No merchant info in fulfillment — cannot validate, skip per spec.
        return ConstraintCheckResult::ok(ct);
    };
    if allowed_merchants.iter().any(|m| m.matches(merchant)) {
        ConstraintCheckResult::ok(ct)
    } else {
        ConstraintCheckResult::violation(
            ct,
            ViError::new(
                ViErrorKind::MerchantNotAllowed,
                format!("merchant '{}' not in allowed list", merchant.name),
            ),
        )
    }
}

fn check_allowed_payee(
    allowed_payees: &[Entity],
    fulfillment: &Fulfillment,
) -> ConstraintCheckResult {
    let ct = "payment.allowed_payee";
    if allowed_payees.is_empty() {
        return ConstraintCheckResult::violation(
            ct,
            ViError::new(
                ViErrorKind::PayeeNotAllowed,
                "empty payee allowlist is unsatisfiable",
            ),
        );
    }
    let Some(payee) = &fulfillment.payee else {
        return ConstraintCheckResult::ok(ct);
    };
    if allowed_payees.iter().any(|p| p.matches(payee)) {
        ConstraintCheckResult::ok(ct)
    } else {
        ConstraintCheckResult::violation(
            ct,
            ViError::new(
                ViErrorKind::PayeeNotAllowed,
                format!("payee '{}' not in allowed list", payee.name),
            ),
        )
    }
}

fn check_payment_amount(
    currency: &str,
    min: Option<i64>,
    max: Option<i64>,
    fulfillment: &Fulfillment,
) -> ConstraintCheckResult {
    let ct = "payment.amount";
    let Some(actual_amount) = fulfillment.amount else {
        return ConstraintCheckResult::violation(
            ct,
            ViError::new(
                ViErrorKind::AmountOutOfRange,
                "missing payment amount in fulfillment",
            ),
        );
    };
    if let Some(actual_currency) = &fulfillment.currency
        && actual_currency != currency
    {
        return ConstraintCheckResult::violation(
            ct,
            ViError::new(
                ViErrorKind::CurrencyMismatch,
                format!("expected {currency}, got {actual_currency}"),
            ),
        );
    }
    if let Some(max_val) = max
        && actual_amount > max_val
    {
        return ConstraintCheckResult::violation(
            ct,
            ViError::new(
                ViErrorKind::AmountOutOfRange,
                format!("amount {actual_amount} > max {max_val} {currency}"),
            ),
        );
    }
    if let Some(min_val) = min
        && actual_amount < min_val
    {
        return ConstraintCheckResult::violation(
            ct,
            ViError::new(
                ViErrorKind::AmountOutOfRange,
                format!("amount {actual_amount} < min {min_val} {currency}"),
            ),
        );
    }
    ConstraintCheckResult::ok(ct)
}

fn check_payment_budget(
    currency: &str,
    max: i64,
    fulfillment: &Fulfillment,
) -> ConstraintCheckResult {
    let ct = "payment.budget";
    let Some(actual_amount) = fulfillment.amount else {
        return ConstraintCheckResult::violation(
            ct,
            ViError::new(
                ViErrorKind::BudgetExceeded,
                "missing payment amount in fulfillment",
            ),
        );
    };
    if let Some(actual_currency) = &fulfillment.currency
        && actual_currency != currency
    {
        return ConstraintCheckResult::violation(
            ct,
            ViError::new(
                ViErrorKind::CurrencyMismatch,
                format!("expected {currency}, got {actual_currency}"),
            ),
        );
    }
    // Single-transaction check: amount must not exceed budget.
    // Cumulative tracking is the payment network's responsibility.
    if actual_amount > max {
        return ConstraintCheckResult::violation(
            ct,
            ViError::new(
                ViErrorKind::BudgetExceeded,
                format!("amount {actual_amount} > budget max {max} {currency}"),
            ),
        );
    }
    ConstraintCheckResult::ok(ct)
}

fn check_line_items(
    constraint_items: &[LineItemEntry],
    fulfillment: &Fulfillment,
) -> ConstraintCheckResult {
    let ct = "mandate.checkout.line_items";
    if constraint_items.is_empty() {
        return ConstraintCheckResult::violation(
            ct,
            ViError::new(
                ViErrorKind::LineItemViolation,
                "empty items allowlist is unsatisfiable",
            ),
        );
    }
    let Some(fulfillment_items) = &fulfillment.line_items else {
        return ConstraintCheckResult::violation(
            ct,
            ViError::new(
                ViErrorKind::LineItemViolation,
                "empty cart does not satisfy line_items constraint",
            ),
        );
    };
    if fulfillment_items.is_empty() {
        return ConstraintCheckResult::violation(
            ct,
            ViError::new(
                ViErrorKind::LineItemViolation,
                "empty cart does not satisfy line_items constraint",
            ),
        );
    }

    // Total quantity check
    let total_allowed: u32 = constraint_items.iter().map(|l| l.quantity).sum();
    let total_actual: u32 = fulfillment_items.iter().map(|f| f.quantity).sum();
    if total_actual > total_allowed {
        return ConstraintCheckResult::violation(
            ct,
            ViError::new(
                ViErrorKind::LineItemViolation,
                format!("total quantity {total_actual} > allowed {total_allowed}"),
            ),
        );
    }

    // Per-item validation: each fulfillment item must be in at least one
    // constraint entry's acceptable_items (unless acceptable_items is empty = wildcard).
    for fi in fulfillment_items {
        let allowed_by_any = constraint_items.iter().any(|entry| {
            if entry.acceptable_items.is_empty() {
                return true; // wildcard
            }
            entry.acceptable_items.iter().any(|ai| ai.id == fi.item_id)
        });
        if !allowed_by_any {
            return ConstraintCheckResult::violation(
                ct,
                ViError::new(
                    ViErrorKind::LineItemViolation,
                    format!("item '{}' not in any acceptable_items list", fi.item_id),
                ),
            );
        }
    }

    ConstraintCheckResult::ok(ct)
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::verifiable_intent::types::{
        AcceptableItem, FulfillmentLineItem, PaymentAmount, PaymentInstrument,
    };

    fn merchant(name: &str, website: &str) -> Entity {
        Entity {
            id: None,
            name: name.into(),
            website: website.into(),
        }
    }

    #[test]
    fn amount_in_range_passes() {
        let f = Fulfillment {
            amount: Some(27999),
            currency: Some("USD".into()),
            ..Default::default()
        };
        let result = check_payment_amount("USD", Some(10000), Some(40000), &f);
        assert!(result.satisfied);
    }

    #[test]
    fn amount_exceeds_max() {
        let f = Fulfillment {
            amount: Some(50000),
            currency: Some("USD".into()),
            ..Default::default()
        };
        let result = check_payment_amount("USD", Some(10000), Some(40000), &f);
        assert!(!result.satisfied);
        assert_eq!(result.violations[0].kind, ViErrorKind::AmountOutOfRange);
    }

    #[test]
    fn amount_below_min() {
        let f = Fulfillment {
            amount: Some(5000),
            currency: Some("USD".into()),
            ..Default::default()
        };
        let result = check_payment_amount("USD", Some(10000), Some(40000), &f);
        assert!(!result.satisfied);
    }

    #[test]
    fn currency_mismatch_fails() {
        let f = Fulfillment {
            amount: Some(20000),
            currency: Some("EUR".into()),
            ..Default::default()
        };
        let result = check_payment_amount("USD", None, Some(40000), &f);
        assert!(!result.satisfied);
        assert_eq!(result.violations[0].kind, ViErrorKind::CurrencyMismatch);
    }

    #[test]
    fn merchant_in_allowlist_passes() {
        let allowed = vec![
            merchant("Store A", "https://store-a.example.com"),
            merchant("Store B", "https://store-b.example.com"),
        ];
        let f = Fulfillment {
            merchant: Some(merchant("Store A", "https://store-a.example.com")),
            ..Default::default()
        };
        let result = check_allowed_merchant(&allowed, &f);
        assert!(result.satisfied);
    }

    #[test]
    fn merchant_not_in_allowlist_fails() {
        let allowed = vec![merchant("Store A", "https://store-a.example.com")];
        let f = Fulfillment {
            merchant: Some(merchant("Store C", "https://store-c.example.com")),
            ..Default::default()
        };
        let result = check_allowed_merchant(&allowed, &f);
        assert!(!result.satisfied);
        assert_eq!(result.violations[0].kind, ViErrorKind::MerchantNotAllowed);
    }

    #[test]
    fn payee_in_allowlist_passes() {
        let allowed = vec![merchant("Payee A", "https://payee-a.example.com")];
        let f = Fulfillment {
            payee: Some(merchant("Payee A", "https://payee-a.example.com")),
            ..Default::default()
        };
        let result = check_allowed_payee(&allowed, &f);
        assert!(result.satisfied);
    }

    #[test]
    fn payee_not_in_allowlist_fails() {
        let allowed = vec![merchant("Payee A", "https://payee-a.example.com")];
        let f = Fulfillment {
            payee: Some(merchant("Payee B", "https://payee-b.example.com")),
            ..Default::default()
        };
        let result = check_allowed_payee(&allowed, &f);
        assert!(!result.satisfied);
    }

    #[test]
    fn line_items_valid() {
        let constraint_items = vec![LineItemEntry {
            id: "line-1".into(),
            acceptable_items: vec![AcceptableItem {
                id: "SKU001".into(),
                title: "Test Product".into(),
            }],
            quantity: 2,
        }];
        let f = Fulfillment {
            line_items: Some(vec![FulfillmentLineItem {
                item_id: "SKU001".into(),
                quantity: 1,
            }]),
            ..Default::default()
        };
        let result = check_line_items(&constraint_items, &f);
        assert!(result.satisfied);
    }

    #[test]
    fn line_items_unknown_sku_fails() {
        let constraint_items = vec![LineItemEntry {
            id: "line-1".into(),
            acceptable_items: vec![AcceptableItem {
                id: "SKU001".into(),
                title: "Test Product".into(),
            }],
            quantity: 2,
        }];
        let f = Fulfillment {
            line_items: Some(vec![FulfillmentLineItem {
                item_id: "SKU999".into(),
                quantity: 1,
            }]),
            ..Default::default()
        };
        let result = check_line_items(&constraint_items, &f);
        assert!(!result.satisfied);
        assert_eq!(result.violations[0].kind, ViErrorKind::LineItemViolation);
    }

    #[test]
    fn line_items_quantity_exceeded() {
        let constraint_items = vec![LineItemEntry {
            id: "line-1".into(),
            acceptable_items: vec![AcceptableItem {
                id: "SKU001".into(),
                title: "Test Product".into(),
            }],
            quantity: 1,
        }];
        let f = Fulfillment {
            line_items: Some(vec![FulfillmentLineItem {
                item_id: "SKU001".into(),
                quantity: 5,
            }]),
            ..Default::default()
        };
        let result = check_line_items(&constraint_items, &f);
        assert!(!result.satisfied);
    }

    #[test]
    fn budget_within_limit_passes() {
        let f = Fulfillment {
            amount: Some(30000),
            currency: Some("USD".into()),
            ..Default::default()
        };
        let result = check_payment_budget("USD", 50000, &f);
        assert!(result.satisfied);
    }

    #[test]
    fn budget_exceeded_fails() {
        let f = Fulfillment {
            amount: Some(60000),
            currency: Some("USD".into()),
            ..Default::default()
        };
        let result = check_payment_budget("USD", 50000, &f);
        assert!(!result.satisfied);
        assert_eq!(result.violations[0].kind, ViErrorKind::BudgetExceeded);
    }

    #[test]
    fn l3_cross_reference_valid() {
        let hash = "abc123";
        let l3a = PaymentL3Mandate {
            vct: "mandate.payment".into(),
            payment_instrument: PaymentInstrument {
                instrument_type: "card".into(),
                id: "tok-1".into(),
                description: None,
            },
            payment_amount: PaymentAmount {
                currency: "USD".into(),
                amount: 27999,
            },
            payee: merchant("Store", "https://store.example.com"),
            transaction_id: hash.into(),
        };
        let l3b = CheckoutL3Mandate {
            vct: "mandate.checkout".into(),
            checkout_jwt: "jwt".into(),
            checkout_hash: hash.into(),
            line_items: None,
        };
        assert!(verify_l3_cross_reference(&l3a, &l3b).is_ok());
    }

    #[test]
    fn l3_cross_reference_mismatch() {
        let l3a = PaymentL3Mandate {
            vct: "mandate.payment".into(),
            payment_instrument: PaymentInstrument {
                instrument_type: "card".into(),
                id: "tok-1".into(),
                description: None,
            },
            payment_amount: PaymentAmount {
                currency: "USD".into(),
                amount: 27999,
            },
            payee: merchant("Store", "https://store.example.com"),
            transaction_id: "hash-a".into(),
        };
        let l3b = CheckoutL3Mandate {
            vct: "mandate.checkout".into(),
            checkout_jwt: "jwt".into(),
            checkout_hash: "hash-b".into(),
            line_items: None,
        };
        let err = verify_l3_cross_reference(&l3a, &l3b).unwrap_err();
        assert_eq!(err.kind, ViErrorKind::CrossReferenceMismatch);
    }

    #[test]
    fn infer_mode_immediate() {
        assert_eq!(
            infer_mode_from_vct("mandate.checkout").unwrap(),
            MandateMode::Immediate
        );
        assert_eq!(
            infer_mode_from_vct("mandate.payment").unwrap(),
            MandateMode::Immediate
        );
    }

    #[test]
    fn infer_mode_autonomous() {
        assert_eq!(
            infer_mode_from_vct("mandate.checkout.open").unwrap(),
            MandateMode::Autonomous
        );
    }

    #[test]
    fn infer_mode_unknown_fails() {
        assert!(infer_mode_from_vct("mandate.unknown").is_err());
    }

    #[test]
    fn check_constraints_multiple() {
        let constraints = vec![
            Constraint::PaymentAmount {
                currency: "USD".into(),
                min: Some(10000),
                max: Some(40000),
            },
            Constraint::AllowedPayee {
                allowed_payees: vec![merchant("Store", "https://store.example.com")],
            },
        ];
        let f = Fulfillment {
            amount: Some(25000),
            currency: Some("USD".into()),
            payee: Some(merchant("Store", "https://store.example.com")),
            ..Default::default()
        };
        let results = check_constraints(&constraints, &f, StrictnessMode::Strict);
        assert_eq!(results.len(), 2);
        assert!(results.iter().all(|r| r.satisfied));
    }
}
