//! Machine-readable error taxonomy for Verifiable Intent operations.
//!
//! Every VI error carries a [`ViErrorKind`] discriminant so policy engines and
//! tool gates can branch deterministically on failure reason without parsing
//! human-readable messages.

use std::fmt;

/// Discriminant for VI error classification — used by policy engines to decide
/// whether a transaction should be blocked, retried, or escalated.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Hash)]
pub enum ViErrorKind {
    // ── Credential structure errors ───────────────────────────────────
    /// JWT header is malformed or missing required fields.
    InvalidHeader,
    /// JWT payload cannot be decoded or is missing required claims.
    InvalidPayload,
    /// SD-JWT disclosure is malformed or cannot be resolved.
    InvalidDisclosure,
    /// Credential has expired (`exp` < now).
    Expired,
    /// Credential is not yet valid (`iat` > now).
    NotYetValid,

    // ── Signature / key errors ────────────────────────────────────────
    /// Cryptographic signature verification failed.
    SignatureInvalid,
    /// The signing key does not match the expected `cnf.jwk` binding.
    KeyMismatch,
    /// Key material is missing or in an unsupported format.
    KeyUnsupported,

    // ── Chain binding errors ──────────────────────────────────────────
    /// `sd_hash` in L2/L3 does not match the hash of the parent layer.
    SdHashMismatch,
    /// `checkout_hash` / `transaction_id` cross-reference between L3a and L3b failed.
    CrossReferenceMismatch,
    /// `conditional_transaction_id` binding between payment and checkout mandates failed.
    ReferenceBindingMismatch,

    // ── Constraint violations ─────────────────────────────────────────
    /// Transaction amount is outside the permitted range.
    AmountOutOfRange,
    /// Cumulative budget cap exceeded.
    BudgetExceeded,
    /// Currency in L3 does not match the constraint currency.
    CurrencyMismatch,
    /// Merchant is not in the allowed merchant list.
    MerchantNotAllowed,
    /// Payee is not in the allowed payee list.
    PayeeNotAllowed,
    /// Line items violate product selection or quantity constraints.
    LineItemViolation,
    /// Recurrence constraint violated.
    RecurrenceViolation,
    /// An unknown constraint type was encountered in strict mode.
    UnknownConstraintType,

    // ── Mode / structural mismatch ────────────────────────────────────
    /// L2 contains `cnf` in Immediate mode (forbidden) or lacks it in Autonomous mode.
    ModeMismatch,
    /// Mandate VCT value is not recognized.
    UnknownMandateType,
    /// Mandate pair is incomplete (missing checkout or payment mandate).
    IncompleteMandatePair,

    // ── Issuance errors ───────────────────────────────────────────────
    /// Issuance failed due to missing or invalid input parameters.
    IssuanceInputInvalid,
}

/// A Verifiable Intent error with a machine-readable kind and human-readable context.
#[derive(Debug, Clone)]
pub struct ViError {
    pub kind: ViErrorKind,
    pub message: String,
}

impl ViError {
    pub fn new(kind: ViErrorKind, message: impl Into<String>) -> Self {
        Self {
            kind,
            message: message.into(),
        }
    }
}

impl fmt::Display for ViError {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        write!(f, "VI/{:?}: {}", self.kind, self.message)
    }
}

impl std::error::Error for ViError {}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn error_display_includes_kind_and_message() {
        let err = ViError::new(ViErrorKind::AmountOutOfRange, "50000 > 40000 USD");
        let s = format!("{err}");
        assert!(s.contains("AmountOutOfRange"));
        assert!(s.contains("50000 > 40000 USD"));
    }

    #[test]
    fn error_kind_equality() {
        assert_eq!(ViErrorKind::Expired, ViErrorKind::Expired);
        assert_ne!(ViErrorKind::Expired, ViErrorKind::SignatureInvalid);
    }
}
