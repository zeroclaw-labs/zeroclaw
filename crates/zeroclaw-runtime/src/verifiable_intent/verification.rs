//! Chain verification, constraint checking, and binding integrity validation.

use crate::verifiable_intent::error::{ViError, ViErrorKind};
use crate::verifiable_intent::types::{
    CheckoutL3Mandate, Constraint, CredentialChain, DisclosableEntry, Entity, Fulfillment,
    KnownConstraint, LineItemEntry, MandateMode, PaymentL3Mandate,
};

// ── Strictness mode ──────────────────────────────────────────────────

/// Controls behavior when an unknown constraint type is encountered.
///
/// This applies only to mandates that are not open. An open mandate rejects an
/// unrecognized constraint in either mode, so the setting cannot widen agent
/// authority. See `check_single_constraint`.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum StrictnessMode {
    /// An unrecognized constraint type is a violation.
    Strict,
    /// An unrecognized constraint type is recorded as skipped and the
    /// recognized constraints decide the outcome.
    Permissive,
}

// ── Verified values ──────────────────────────────────────────────────

/// A credential chain whose signatures, bindings and headers have been
/// verified.
///
/// The fields are private and there is no constructor, no `Default`, and no
/// conversion from unverified data, so a value of this type cannot be produced
/// by any current code path. The chain verifier that constructs it is a later
/// stage of the work tracked upstream; until it lands, holding one of these is
/// impossible rather than merely discouraged.
pub struct VerifiedCredentialChain {
    chain: CredentialChain,
    mode: MandateMode,
}

impl VerifiedCredentialChain {
    /// The verified chain layers.
    pub fn chain(&self) -> &CredentialChain {
        &self.chain
    }

    /// The execution mode established during verification.
    pub fn mode(&self) -> MandateMode {
        self.mode
    }
}

/// A checkout and payment mandate pair, paired and verified together, with the
/// fulfillment derived from the verified L3 layers.
///
/// Same construction rules as `VerifiedCredentialChain`. Constraint evaluation
/// consumes a value of this type once the verifier exists, which is what stops
/// a caller supplying both the constraints and the values checked against them.
pub struct VerifiedMandatePair {
    constraints: Vec<Constraint>,
    fulfillment: Fulfillment,
    mode: MandateMode,
}

impl VerifiedMandatePair {
    /// Constraints taken from the verified L2 mandates.
    pub fn constraints(&self) -> &[Constraint] {
        &self.constraints
    }

    /// Fulfillment derived from the verified L3 mandates.
    pub fn fulfillment(&self) -> &Fulfillment {
        &self.fulfillment
    }

    /// The execution mode established during verification.
    pub fn mode(&self) -> MandateMode {
        self.mode
    }
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
    /// Set when the constraint was not evaluated at all.
    ///
    /// A skipped result reports `satisfied: true` so that recognized
    /// constraints decide the outcome, which is the behavior the reference
    /// implementation records in its `skipped` list. The flag is what separates
    /// "checked and passed" from "never checked", and a caller that treats the
    /// two as the same is drawing a stronger conclusion than the data supports.
    pub skipped: bool,
}

impl ConstraintCheckResult {
    pub fn ok(constraint_type: &str) -> Self {
        Self {
            satisfied: true,
            constraint_type: constraint_type.into(),
            violations: vec![],
            skipped: false,
        }
    }

    pub fn violation(constraint_type: &str, err: ViError) -> Self {
        Self {
            satisfied: false,
            constraint_type: constraint_type.into(),
            violations: vec![err],
            skipped: false,
        }
    }

    /// A constraint that was left unevaluated under a permissive policy.
    pub fn skipped(constraint_type: &str) -> Self {
        Self {
            satisfied: true,
            constraint_type: constraint_type.into(),
            violations: vec![],
            skipped: true,
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
///
/// `mode` decides how an unrecognized constraint is treated. `Autonomous` is
/// the open-mandate case, where the agent acts on its own and an unevaluable
/// constraint would leave its authority unbounded, so such a constraint is a
/// violation whatever the strictness setting says.
pub fn check_constraints(
    constraints: &[Constraint],
    fulfillment: &Fulfillment,
    strictness: StrictnessMode,
    mode: MandateMode,
) -> Vec<ConstraintCheckResult> {
    constraints
        .iter()
        .map(|c| check_single_constraint(c, fulfillment, strictness, mode))
        .collect()
}

fn check_single_constraint(
    constraint: &Constraint,
    fulfillment: &Fulfillment,
    strictness: StrictnessMode,
    mode: MandateMode,
) -> ConstraintCheckResult {
    let known = match constraint {
        // Fields the recognized variant did not consume are carried for later
        // stages rather than evaluated here. A checker that acted on a field it
        // does not recognize would be inventing a rule the issuer never wrote.
        Constraint::Known { known, .. } => known,
        Constraint::Unknown {
            constraint_type, ..
        } => return check_unknown_constraint(constraint_type, strictness, mode),
    };

    match known {
        KnownConstraint::AllowedMerchant { allowed_merchants } => {
            check_allowed_merchant(allowed_merchants, fulfillment)
        }
        KnownConstraint::LineItems { items } => check_line_items(items, fulfillment),
        KnownConstraint::AllowedPayee { allowed_payees } => {
            check_allowed_payee(allowed_payees, fulfillment)
        }
        KnownConstraint::PaymentAmount { currency, min, max } => {
            check_payment_amount(currency, *min, *max, fulfillment)
        }
        KnownConstraint::PaymentBudget { currency, max } => {
            check_payment_budget(currency, *max, fulfillment)
        }
        KnownConstraint::PaymentReference {
            conditional_transaction_id,
        } => {
            // Reference binding is verified structurally, not against fulfillment.
            ConstraintCheckResult::ok(&format!(
                "payment.reference({})",
                &conditional_transaction_id[..8.min(conditional_transaction_id.len())]
            ))
        }
        KnownConstraint::PaymentRecurrence { .. } | KnownConstraint::AgentRecurrence { .. } => {
            // Recurrence constraints are informational for the payment network
            // to enforce statefulness. Pass-through at the agent level.
            ConstraintCheckResult::ok("recurrence")
        }
    }
}

/// Decide what an unrecognized constraint type means.
///
/// An open mandate rejects it in either strictness mode: the agent acts without
/// a further confirmation step, and a constraint nothing can evaluate places no
/// bound on what it may do. Outside that case the configured mode decides, and
/// a permissive result is recorded as skipped rather than as a pass.
fn check_unknown_constraint(
    constraint_type: &str,
    strictness: StrictnessMode,
    mode: MandateMode,
) -> ConstraintCheckResult {
    let open_mandate = matches!(mode, MandateMode::Autonomous);
    if open_mandate || matches!(strictness, StrictnessMode::Strict) {
        let detail = if open_mandate {
            format!("unknown constraint type in open mandate: {constraint_type}")
        } else {
            format!("unknown constraint type: {constraint_type}")
        };
        return ConstraintCheckResult::violation(
            constraint_type,
            ViError::new(ViErrorKind::UnknownConstraintType, detail),
        );
    }
    ConstraintCheckResult::skipped(constraint_type)
}

// ── Individual constraint checkers ───────────────────────────────────

fn check_allowed_merchant(
    allowed_merchants: &[DisclosableEntry<Entity>],
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
        // Fail closed: the constraint is present but the fulfillment discloses
        // no merchant to check against it, and an absent subject cannot satisfy
        // an allowlist. Matches the VI reference implementation, which reports
        // "Missing or invalid merchant in fulfillment" as a violation.
        return ConstraintCheckResult::violation(
            ct,
            ViError::new(
                ViErrorKind::MerchantNotAllowed,
                "fulfillment discloses no merchant to check against the allowlist",
            ),
        );
    };

    // An entry withheld from this presentation cannot take part in the
    // decision, so only disclosed entries are candidates. The reference skips
    // the constraint when every entry is withheld; failing closed is the
    // conservative reading, and the policy belongs to the constraint-checker
    // stage that owns unresolved-entry behavior.
    let disclosed: Vec<&Entity> = allowed_merchants
        .iter()
        .filter_map(DisclosableEntry::disclosed)
        .collect();
    if disclosed.is_empty() {
        return ConstraintCheckResult::violation(
            ct,
            ViError::new(
                ViErrorKind::MerchantNotAllowed,
                "merchant allowlist discloses no entries, so no merchant can be matched",
            ),
        );
    }

    if disclosed.iter().any(|allowed| allowed.matches(merchant)) {
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
    allowed_payees: &[DisclosableEntry<Entity>],
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
        // Fail closed, as in `check_allowed_merchant`: an absent payee cannot
        // satisfy an allowlist.
        return ConstraintCheckResult::violation(
            ct,
            ViError::new(
                ViErrorKind::PayeeNotAllowed,
                "fulfillment discloses no payee to check against the allowlist",
            ),
        );
    };

    // As in `check_allowed_merchant`: withheld entries are not candidates.
    let disclosed: Vec<&Entity> = allowed_payees
        .iter()
        .filter_map(DisclosableEntry::disclosed)
        .collect();
    if disclosed.is_empty() {
        return ConstraintCheckResult::violation(
            ct,
            ViError::new(
                ViErrorKind::PayeeNotAllowed,
                "payee allowlist discloses no entries, so no payee can be matched",
            ),
        );
    }

    if disclosed.iter().any(|allowed| allowed.matches(payee)) {
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
    if let Err(err) = verify_fulfillment_currency(currency, fulfillment.currency.as_deref()) {
        return ConstraintCheckResult::violation(ct, err);
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
    if let Err(err) = verify_fulfillment_currency(currency, fulfillment.currency.as_deref()) {
        return ConstraintCheckResult::violation(ct, err);
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

fn verify_fulfillment_currency(expected: &str, actual: Option<&str>) -> Result<(), ViError> {
    if expected.trim().is_empty() {
        return Err(ViError::new(
            ViErrorKind::CurrencyMismatch,
            "payment constraint currency is empty",
        ));
    }
    match actual {
        Some(actual) if actual.trim().is_empty() => Err(ViError::new(
            ViErrorKind::CurrencyMismatch,
            format!("missing payment currency; expected {expected}"),
        )),
        Some(actual) if actual == expected => Ok(()),
        Some(actual) => Err(ViError::new(
            ViErrorKind::CurrencyMismatch,
            format!("expected {expected}, got {actual}"),
        )),
        None => Err(ViError::new(
            ViErrorKind::CurrencyMismatch,
            format!("missing payment currency; expected {expected}"),
        )),
    }
}

fn check_line_items(
    constraint_items: &[DisclosableEntry<LineItemEntry>],
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

    // A withheld entry states neither a quantity nor an acceptable-item list,
    // so it cannot widen what the cart is allowed to contain. Counting only
    // disclosed entries understates the allowance, which is the safe direction.
    let disclosed: Vec<&LineItemEntry> = constraint_items
        .iter()
        .filter_map(DisclosableEntry::disclosed)
        .collect();
    if disclosed.is_empty() {
        return ConstraintCheckResult::violation(
            ct,
            ViError::new(
                ViErrorKind::LineItemViolation,
                "line_items constraint discloses no entries, so no cart can be checked against it",
            ),
        );
    }

    // Total quantity check
    let total_allowed: u128 = disclosed.iter().map(|item| u128::from(item.quantity)).sum();
    let total_actual: u128 = fulfillment_items
        .iter()
        .map(|item| u128::from(item.quantity))
        .sum();
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
        let allowed_by_any = disclosed.iter().any(|entry| {
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

    /// An allowlist entry the presentation actually disclosed.
    fn disclosed<T>(value: T) -> DisclosableEntry<T> {
        DisclosableEntry::Disclosed(value)
    }

    /// An entry withheld from the presentation, carrying only its hash.
    fn withheld<T>(hash: &str) -> DisclosableEntry<T> {
        DisclosableEntry::Reference { hash: hash.into() }
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
            disclosed(merchant("Store A", "https://store-a.example.com")),
            disclosed(merchant("Store B", "https://store-b.example.com")),
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
        let allowed = vec![disclosed(merchant(
            "Store A",
            "https://store-a.example.com",
        ))];
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
        let allowed = vec![disclosed(merchant(
            "Payee A",
            "https://payee-a.example.com",
        ))];
        let f = Fulfillment {
            payee: Some(merchant("Payee A", "https://payee-a.example.com")),
            ..Default::default()
        };
        let result = check_allowed_payee(&allowed, &f);
        assert!(result.satisfied);
    }

    #[test]
    fn payee_not_in_allowlist_fails() {
        let allowed = vec![disclosed(merchant(
            "Payee A",
            "https://payee-a.example.com",
        ))];
        let f = Fulfillment {
            payee: Some(merchant("Payee B", "https://payee-b.example.com")),
            ..Default::default()
        };
        let result = check_allowed_payee(&allowed, &f);
        assert!(!result.satisfied);
    }

    /// A fulfillment that omits the merchant must not satisfy a merchant
    /// allowlist. Every `Fulfillment` field is `Option` with `Default` derived,
    /// so a caller-supplied `{}` deserializes to all-`None` and would otherwise
    /// clear the constraint it is being checked against.
    #[test]
    fn missing_merchant_does_not_satisfy_allowed_merchant() {
        let allowed = vec![disclosed(merchant(
            "Store A",
            "https://store-a.example.com",
        ))];
        let f = Fulfillment::default();
        let result = check_allowed_merchant(&allowed, &f);
        assert!(
            !result.satisfied,
            "an absent merchant must not satisfy an allowed-merchant constraint"
        );
        assert_eq!(result.violations[0].kind, ViErrorKind::MerchantNotAllowed);
    }

    /// The same for the payee allowlist.
    #[test]
    fn missing_payee_does_not_satisfy_allowed_payee() {
        let allowed = vec![disclosed(merchant(
            "Payee A",
            "https://payee-a.example.com",
        ))];
        let f = Fulfillment::default();
        let result = check_allowed_payee(&allowed, &f);
        assert!(
            !result.satisfied,
            "an absent payee must not satisfy an allowed-payee constraint"
        );
        assert_eq!(result.violations[0].kind, ViErrorKind::PayeeNotAllowed);
    }

    /// An allowlist whose entries were all withheld from the presentation says
    /// nothing about who is permitted, so nothing can satisfy it. The reference
    /// skips the constraint here; failing closed is the conservative reading,
    /// and the policy belongs to the checker-parity stage.
    #[test]
    fn an_allowlist_of_withheld_entries_matches_nothing() {
        let allowed: Vec<DisclosableEntry<Entity>> = vec![withheld("hash-a"), withheld("hash-b")];
        let f = Fulfillment {
            merchant: Some(merchant("Store A", "https://store-a.example.com")),
            ..Default::default()
        };

        let result = check_allowed_merchant(&allowed, &f);
        assert!(
            !result.satisfied,
            "an entry whose contents are unknown must not authorize a merchant"
        );
        assert_eq!(result.violations[0].kind, ViErrorKind::MerchantNotAllowed);
    }

    /// The common Autonomous case: the agent discloses the one merchant it is
    /// transacting with and withholds the rest. The disclosed entry still
    /// decides the outcome.
    #[test]
    fn a_disclosed_entry_matches_beside_withheld_ones() {
        let allowed = vec![
            withheld("hash-a"),
            disclosed(merchant("Store B", "https://store-b.example.com")),
            withheld("hash-c"),
        ];
        let f = Fulfillment {
            merchant: Some(merchant("Store B", "https://store-b.example.com")),
            ..Default::default()
        };
        assert!(check_allowed_merchant(&allowed, &f).satisfied);

        let other = Fulfillment {
            merchant: Some(merchant("Store Z", "https://store-z.example.com")),
            ..Default::default()
        };
        assert!(
            !check_allowed_merchant(&allowed, &other).satisfied,
            "a withheld entry must not vouch for a merchant it never named"
        );
    }

    /// A withheld line item states no quantity, so it cannot raise the cap.
    /// Counting only disclosed entries understates the allowance, which is the
    /// direction that fails closed.
    #[test]
    fn a_withheld_line_item_does_not_widen_the_allowance() {
        let constraint_items = vec![
            disclosed(LineItemEntry {
                id: "line-1".into(),
                acceptable_items: vec![],
                quantity: 1,
            }),
            withheld("hash-of-a-second-line-item"),
        ];

        let within = Fulfillment {
            line_items: Some(vec![FulfillmentLineItem {
                item_id: "SKU001".into(),
                quantity: 1,
            }]),
            ..Default::default()
        };
        assert!(check_line_items(&constraint_items, &within).satisfied);

        let over = Fulfillment {
            line_items: Some(vec![FulfillmentLineItem {
                item_id: "SKU001".into(),
                quantity: 2,
            }]),
            ..Default::default()
        };
        let result = check_line_items(&constraint_items, &over);
        assert!(
            !result.satisfied,
            "the withheld entry must not contribute quantity it never stated"
        );
        assert_eq!(result.violations[0].kind, ViErrorKind::LineItemViolation);
    }

    #[test]
    fn line_items_valid() {
        let constraint_items = vec![disclosed(LineItemEntry {
            id: "line-1".into(),
            acceptable_items: vec![AcceptableItem {
                id: "SKU001".into(),
                title: "Test Product".into(),
            }],
            quantity: 2,
        })];
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
        let constraint_items = vec![disclosed(LineItemEntry {
            id: "line-1".into(),
            acceptable_items: vec![AcceptableItem {
                id: "SKU001".into(),
                title: "Test Product".into(),
            }],
            quantity: 2,
        })];
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
        let constraint_items = vec![disclosed(LineItemEntry {
            id: "line-1".into(),
            acceptable_items: vec![AcceptableItem {
                id: "SKU001".into(),
                title: "Test Product".into(),
            }],
            quantity: 1,
        })];
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
    fn allowed_line_item_quantity_total_does_not_wrap() {
        let constraint_items = vec![
            disclosed(LineItemEntry {
                id: "line-1".into(),
                acceptable_items: vec![],
                quantity: u32::MAX,
            }),
            disclosed(LineItemEntry {
                id: "line-2".into(),
                acceptable_items: vec![],
                quantity: 1,
            }),
        ];
        let f = Fulfillment {
            line_items: Some(vec![FulfillmentLineItem {
                item_id: "SKU001".into(),
                quantity: u32::MAX,
            }]),
            ..Default::default()
        };

        assert!(check_line_items(&constraint_items, &f).satisfied);
    }

    #[test]
    fn fulfillment_line_item_quantity_total_does_not_wrap() {
        let constraint_items = vec![disclosed(LineItemEntry {
            id: "line-1".into(),
            acceptable_items: vec![],
            quantity: u32::MAX,
        })];
        let f = Fulfillment {
            line_items: Some(vec![
                FulfillmentLineItem {
                    item_id: "SKU001".into(),
                    quantity: u32::MAX,
                },
                FulfillmentLineItem {
                    item_id: "SKU002".into(),
                    quantity: 1,
                },
            ]),
            ..Default::default()
        };

        let result = check_line_items(&constraint_items, &f);
        assert!(!result.satisfied);
        assert_eq!(result.violations[0].kind, ViErrorKind::LineItemViolation);
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
            KnownConstraint::PaymentAmount {
                currency: "USD".into(),
                min: Some(10000),
                max: Some(40000),
            }
            .into(),
            KnownConstraint::AllowedPayee {
                allowed_payees: vec![disclosed(merchant("Store", "https://store.example.com"))],
            }
            .into(),
        ];
        let f = Fulfillment {
            amount: Some(25000),
            currency: Some("USD".into()),
            payee: Some(merchant("Store", "https://store.example.com")),
            ..Default::default()
        };
        let results = check_constraints(
            &constraints,
            &f,
            StrictnessMode::Strict,
            MandateMode::Autonomous,
        );
        assert_eq!(results.len(), 2);
        assert!(results.iter().all(|r| r.satisfied));
        assert!(results.iter().all(|r| !r.skipped));
    }

    #[test]
    fn missing_or_empty_currency_fails_currency_bound_constraints() {
        for (expected, currency) in [
            ("USD", None),
            ("USD", Some(String::new())),
            ("USD", Some(" ".into())),
            ("", Some(String::new())),
            (" ", Some(" ".into())),
        ] {
            let constraints: Vec<Constraint> = vec![
                KnownConstraint::PaymentAmount {
                    currency: expected.into(),
                    min: None,
                    max: Some(40000),
                }
                .into(),
                KnownConstraint::PaymentBudget {
                    currency: expected.into(),
                    max: 50000,
                }
                .into(),
            ];
            let fulfillment = Fulfillment {
                amount: Some(20000),
                currency,
                ..Default::default()
            };
            let results = check_constraints(
                &constraints,
                &fulfillment,
                StrictnessMode::Strict,
                MandateMode::Autonomous,
            );

            assert_eq!(results.len(), 2);
            assert_eq!(results[0].constraint_type, "payment.amount");
            assert_eq!(results[1].constraint_type, "payment.budget");
            for result in results {
                assert!(!result.satisfied);
                assert_eq!(result.violations.len(), 1);
                assert_eq!(result.violations[0].kind, ViErrorKind::CurrencyMismatch);
            }
        }
    }

    fn unknown_constraint() -> Constraint {
        serde_json::from_str(r#"{"type":"urn:example:experimental","scope":"wide"}"#).unwrap()
    }

    /// An open mandate rejects an unrecognized constraint whatever the
    /// strictness setting says. The agent acts on its own in this mode, so a
    /// constraint nothing can evaluate places no bound on what it may do.
    #[test]
    fn open_mandate_rejects_unknown_constraint_in_either_mode() {
        for strictness in [StrictnessMode::Strict, StrictnessMode::Permissive] {
            let results = check_constraints(
                &[unknown_constraint()],
                &Fulfillment::default(),
                strictness,
                MandateMode::Autonomous,
            );

            assert_eq!(results.len(), 1);
            assert!(
                !results[0].satisfied,
                "an open mandate must reject an unknown constraint under {strictness:?}"
            );
            assert!(!results[0].skipped);
            assert_eq!(
                results[0].violations[0].kind,
                ViErrorKind::UnknownConstraintType
            );
            assert_eq!(results[0].constraint_type, "urn:example:experimental");
        }
    }

    /// Outside an open mandate the configured mode decides, which is the half
    /// of the setting that had no effect while the representation was closed.
    #[test]
    fn strict_mode_rejects_unknown_constraint() {
        let results = check_constraints(
            &[unknown_constraint()],
            &Fulfillment::default(),
            StrictnessMode::Strict,
            MandateMode::Immediate,
        );

        assert!(!results[0].satisfied);
        assert!(!results[0].skipped);
        assert_eq!(
            results[0].violations[0].kind,
            ViErrorKind::UnknownConstraintType
        );
    }

    /// The permissive result is recorded as skipped rather than as a pass. A
    /// caller that reads only `satisfied` would otherwise conclude the
    /// constraint had been checked.
    #[test]
    fn permissive_mode_records_unknown_constraint_as_skipped() {
        let results = check_constraints(
            &[unknown_constraint()],
            &Fulfillment::default(),
            StrictnessMode::Permissive,
            MandateMode::Immediate,
        );

        assert!(results[0].satisfied);
        assert!(
            results[0].skipped,
            "a permissive result must record the skip"
        );
        assert!(results[0].violations.is_empty());
        assert_eq!(results[0].constraint_type, "urn:example:experimental");
    }

    /// A recognized constraint is unaffected by either input, and its result is
    /// never marked skipped.
    #[test]
    fn known_constraint_is_unaffected_by_strictness_and_mode() {
        let constraints: Vec<Constraint> = vec![
            KnownConstraint::PaymentAmount {
                currency: "USD".into(),
                min: None,
                max: Some(40000),
            }
            .into(),
        ];
        let fulfillment = Fulfillment {
            amount: Some(20000),
            currency: Some("USD".into()),
            ..Default::default()
        };

        for strictness in [StrictnessMode::Strict, StrictnessMode::Permissive] {
            for mode in [MandateMode::Immediate, MandateMode::Autonomous] {
                let results = check_constraints(&constraints, &fulfillment, strictness, mode);
                assert!(results[0].satisfied, "{strictness:?} / {mode:?}");
                assert!(!results[0].skipped, "{strictness:?} / {mode:?}");
            }
        }
    }

    /// A mixed list evaluates the recognized constraint and rejects the
    /// unrecognized one rather than failing the whole list at parse time.
    #[test]
    fn unknown_constraint_does_not_prevent_evaluating_the_rest() {
        let constraints: Vec<Constraint> = serde_json::from_str(
            r#"[
                {"type":"payment.amount","currency":"USD","max":40000},
                {"type":"urn:example:experimental","scope":"wide"}
            ]"#,
        )
        .unwrap();
        let fulfillment = Fulfillment {
            amount: Some(20000),
            currency: Some("USD".into()),
            ..Default::default()
        };

        let results = check_constraints(
            &constraints,
            &fulfillment,
            StrictnessMode::Strict,
            MandateMode::Autonomous,
        );

        assert_eq!(results.len(), 2);
        assert!(results[0].satisfied);
        assert_eq!(results[0].constraint_type, "payment.amount");
        assert!(!results[1].satisfied);
        assert_eq!(
            results[1].violations[0].kind,
            ViErrorKind::UnknownConstraintType
        );
    }
}
