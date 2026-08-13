//! Core data models for the Verifiable Intent credential chain.

use serde::{Deserialize, Deserializer, Serialize, Serializer};
use std::fmt;

// ── JWK / Key material ───────────────────────────────────────────────

/// A JSON Web Key (EC P-256) used for signing and key confirmation.
#[derive(Clone, Serialize, Deserialize, PartialEq, Eq)]
pub struct Jwk {
    pub kty: String,
    pub crv: String,
    /// Base64url-encoded x coordinate.
    pub x: String,
    /// Base64url-encoded y coordinate.
    pub y: String,
    /// Base64url-encoded private key (only present for signing keys, never serialized to verifiers).
    #[serde(skip_serializing)]
    pub d: Option<String>,
}

impl fmt::Debug for Jwk {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        let private_scalar = self.d.as_ref().map(|_| "[REDACTED]");
        f.debug_struct("Jwk")
            .field("kty", &self.kty)
            .field("crv", &self.crv)
            .field("x", &self.x)
            .field("y", &self.y)
            .field("d", &private_scalar)
            .finish()
    }
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

// ── Selectively disclosable constraint entries ───────────────────────

/// An entry inside an `allowed` or `line_items` constraint.
///
/// In Autonomous mode the specification makes these entries individually
/// disclosable: each is its own SD-JWT disclosure, referenced from the
/// constraint object by hash. An agent presents only the entries a given
/// verifier needs, so the remainder arrive as `{"...": "<hash>"}`.
///
/// Both forms have to survive parsing. A reference is not an error, and it is
/// not a value either — it is an entry this presentation deliberately withheld,
/// and a verifier that cannot tell the two apart cannot reason about either.
/// Nothing here resolves a reference; resolution needs the presentation's
/// disclosure set, which lives above this layer.
#[derive(Debug, Clone, PartialEq, Eq)]
pub enum DisclosableEntry<T> {
    /// The entry's value, disclosed in this presentation.
    Disclosed(T),
    /// An entry withheld from this presentation, kept as its disclosure hash.
    Reference { hash: String },
}

impl<T> DisclosableEntry<T> {
    /// The entry's value, or `None` when it was withheld.
    ///
    /// Callers that evaluate a constraint use this: an undisclosed entry cannot
    /// take part in a decision, because its contents are unknown.
    pub fn disclosed(&self) -> Option<&T> {
        match self {
            Self::Disclosed(value) => Some(value),
            Self::Reference { .. } => None,
        }
    }
}

impl<T: Serialize> Serialize for DisclosableEntry<T> {
    fn serialize<S: Serializer>(&self, serializer: S) -> Result<S::Ok, S::Error> {
        match self {
            Self::Disclosed(value) => value.serialize(serializer),
            Self::Reference { hash } => {
                let mut object = serde_json::Map::with_capacity(1);
                object.insert("...".to_owned(), serde_json::Value::String(hash.clone()));
                object.serialize(serializer)
            }
        }
    }
}

impl<'de, T: Deserialize<'de>> Deserialize<'de> for DisclosableEntry<T> {
    fn deserialize<D: Deserializer<'de>>(deserializer: D) -> Result<Self, D::Error> {
        use serde::de::Error as _;

        let value = serde_json::Value::deserialize(deserializer)?;

        // A reference is an object whose *only* member is `...` carrying a
        // string. The reference implementation tests membership instead, which
        // reads `{"...": h, "id": "m-1"}` as a reference and silently discards
        // the `id`. Requiring exclusivity turns that malformed entry into a
        // parse failure rather than quiet data loss.
        if let serde_json::Value::Object(object) = &value
            && object.len() == 1
            && let Some(serde_json::Value::String(hash)) = object.get("...")
        {
            return Ok(Self::Reference { hash: hash.clone() });
        }

        T::deserialize(value)
            .map(Self::Disclosed)
            .map_err(D::Error::custom)
    }
}

// ── Constraints ──────────────────────────────────────────────────────

/// Constraint types this build recognizes and can evaluate.
#[derive(Debug, Clone, Serialize, Deserialize, PartialEq, Eq)]
#[serde(tag = "type")]
pub enum KnownConstraint {
    /// Merchant allowlist for checkout mandates.
    #[serde(rename = "mandate.checkout.allowed_merchant")]
    AllowedMerchant {
        allowed_merchants: Vec<DisclosableEntry<Entity>>,
    },

    /// Product selection constraints for checkout mandates.
    #[serde(rename = "mandate.checkout.line_items")]
    LineItems {
        items: Vec<DisclosableEntry<LineItemEntry>>,
    },

    /// Payee allowlist for payment mandates.
    #[serde(rename = "payment.allowed_payee")]
    AllowedPayee {
        allowed_payees: Vec<DisclosableEntry<Entity>>,
    },

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

/// Every `type` tag `KnownConstraint` can deserialize.
///
/// This list decides what happens when a constraint carries a recognized tag
/// but fails to deserialize. Such a value must surface the parse error. A tag
/// missing here would instead let a malformed recognized constraint fall
/// through to `Constraint::Unknown`, where a permissive policy skips it and the
/// malformed input is silently ignored. `known_constraint_tags_are_complete`
/// pins the list to the enum.
const KNOWN_CONSTRAINT_TYPES: &[&str] = &[
    "mandate.checkout.allowed_merchant",
    "mandate.checkout.line_items",
    "payment.allowed_payee",
    "payment.amount",
    "payment.budget",
    "payment.recurrence",
    "payment.agent_recurrence",
    "payment.reference",
];

/// A constraint carried by an L2 open mandate.
///
/// The constraint model is extensible: implementations define their own types,
/// and the registered set grows over time, so a verifier will meet tags newer
/// than itself. An unrecognized constraint is kept verbatim rather than failing
/// the whole evaluation at parse time, so that a strictness policy can decide
/// what it means. The specification requires that every field of such a
/// constraint survive parsing.
#[derive(Debug, Clone, PartialEq, Eq)]
pub enum Constraint {
    /// A constraint type this build can evaluate.
    Known(KnownConstraint),
    /// A constraint type this build does not recognize, preserved in full.
    Unknown {
        constraint_type: String,
        fields: serde_json::Map<String, serde_json::Value>,
    },
}

impl Serialize for Constraint {
    fn serialize<S: Serializer>(&self, serializer: S) -> Result<S::Ok, S::Error> {
        match self {
            Constraint::Known(known) => known.serialize(serializer),
            Constraint::Unknown {
                constraint_type,
                fields,
            } => {
                let mut object = serde_json::Map::with_capacity(fields.len() + 1);
                object.insert(
                    "type".to_owned(),
                    serde_json::Value::String(constraint_type.clone()),
                );
                for (key, value) in fields {
                    object.insert(key.clone(), value.clone());
                }
                object.serialize(serializer)
            }
        }
    }
}

impl<'de> Deserialize<'de> for Constraint {
    fn deserialize<D: Deserializer<'de>>(deserializer: D) -> Result<Self, D::Error> {
        use serde::de::Error as _;

        let serde_json::Value::Object(mut object) = serde_json::Value::deserialize(deserializer)?
        else {
            return Err(D::Error::custom("constraint must be a JSON object"));
        };
        let Some(serde_json::Value::String(constraint_type)) = object.get("type").cloned() else {
            return Err(D::Error::custom("constraint must carry a string `type`"));
        };

        match KnownConstraint::deserialize(serde_json::Value::Object(object.clone())) {
            Ok(known) => Ok(Constraint::Known(known)),
            Err(error) => {
                if KNOWN_CONSTRAINT_TYPES.contains(&constraint_type.as_str()) {
                    return Err(D::Error::custom(error));
                }
                object.remove("type");
                Ok(Constraint::Unknown {
                    constraint_type,
                    fields: object,
                })
            }
        }
    }
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

/// Parsed representation of an L1 SD-JWT (credential model_provider → user).
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
    fn jwk_private_scalar_is_not_serialized_or_debugged() {
        let secret = "private-scalar-sentinel";
        let jwk: Jwk = serde_json::from_value(serde_json::json!({
            "kty": "EC",
            "crv": "P-256",
            "x": "x-coordinate",
            "y": "y-coordinate",
            "d": secret,
        }))
        .unwrap();

        assert_eq!(jwk.d.as_deref(), Some(secret));

        let serialized = serde_json::to_string(&jwk).unwrap();
        assert!(!serialized.contains(secret));
        assert!(!serialized.contains("\"d\""));

        let debug = format!("{jwk:?}");
        assert!(!debug.contains(secret));
        assert!(debug.contains("[REDACTED]"));
    }

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
        let c = Constraint::Known(KnownConstraint::PaymentAmount {
            currency: "USD".into(),
            min: Some(10000),
            max: Some(40000),
        });
        let json = serde_json::to_string(&c).unwrap();
        assert!(json.contains("payment.amount"));
        let back: Constraint = serde_json::from_str(&json).unwrap();
        assert_eq!(c, back);
    }

    #[test]
    fn constraint_merchant_serde_roundtrip() {
        let c = Constraint::Known(KnownConstraint::AllowedMerchant {
            allowed_merchants: vec![DisclosableEntry::Disclosed(Entity {
                id: None,
                name: "Test Store".into(),
                website: "https://test.example.com".into(),
            })],
        });
        let json = serde_json::to_string(&c).unwrap();
        assert!(json.contains("mandate.checkout.allowed_merchant"));
        let back: Constraint = serde_json::from_str(&json).unwrap();
        assert_eq!(c, back);
    }

    /// An Autonomous presentation discloses only the entries a given verifier
    /// needs, so a constraint routinely carries a mix of values and hashes.
    /// Both have to survive parsing, and a round trip must not turn one into
    /// the other.
    #[test]
    fn constraint_entries_keep_disclosed_and_withheld_forms() {
        let json = r#"{
            "type": "mandate.checkout.allowed_merchant",
            "allowed_merchants": [
                {"id": "m-1", "name": "Audioshop", "website": "https://audioshop.example"},
                {"...": "hrPPJ7L3t5KDOjA04PIL08z0_6UyW8finU53nPf-sCU"}
            ]
        }"#;

        let parsed: Constraint = serde_json::from_str(json).expect("mixed entries must parse");
        let Constraint::Known(KnownConstraint::AllowedMerchant { allowed_merchants }) = &parsed
        else {
            panic!("expected a merchant allowlist, got {parsed:?}");
        };

        assert_eq!(allowed_merchants.len(), 2);
        assert_eq!(
            allowed_merchants[0].disclosed().map(|e| e.name.as_str()),
            Some("Audioshop"),
        );
        assert_eq!(
            allowed_merchants[1],
            DisclosableEntry::Reference {
                hash: "hrPPJ7L3t5KDOjA04PIL08z0_6UyW8finU53nPf-sCU".to_owned(),
            },
        );
        assert!(allowed_merchants[1].disclosed().is_none());

        let round_tripped: Constraint =
            serde_json::from_str(&serde_json::to_string(&parsed).unwrap()).unwrap();
        assert_eq!(parsed, round_tripped);
    }

    /// An entry is a reference only when `...` is its sole member. The
    /// reference implementation tests membership, which would read a hybrid
    /// object as a reference and silently drop its other fields; refusing it
    /// turns malformed input into a parse error instead of quiet data loss.
    #[test]
    fn a_reference_entry_must_carry_nothing_but_the_hash() {
        let hybrid = r#"{
            "type": "mandate.checkout.allowed_merchant",
            "allowed_merchants": [{"...": "abc", "id": "m-1"}]
        }"#;
        assert!(
            serde_json::from_str::<Constraint>(hybrid).is_err(),
            "an object mixing a reference with entry fields must not parse"
        );

        let non_string_hash = r#"{
            "type": "mandate.checkout.allowed_merchant",
            "allowed_merchants": [{"...": 7}]
        }"#;
        assert!(
            serde_json::from_str::<Constraint>(non_string_hash).is_err(),
            "a reference hash must be a string"
        );
    }

    /// `KNOWN_CONSTRAINT_TYPES` decides whether a failed parse of a recognized
    /// tag surfaces as an error or degrades into `Unknown`. Adding a variant
    /// and forgetting the list would make that degradation silent, so pin one
    /// against the other: every variant's serialized tag must appear in the
    /// list, and the list must carry nothing the enum does not emit.
    #[test]
    fn known_constraint_tags_are_complete() {
        let entity = || Entity {
            id: None,
            name: "n".into(),
            website: "w".into(),
        };
        let every_variant = [
            KnownConstraint::AllowedMerchant {
                allowed_merchants: vec![DisclosableEntry::Disclosed(entity())],
            },
            KnownConstraint::LineItems { items: vec![] },
            KnownConstraint::AllowedPayee {
                allowed_payees: vec![DisclosableEntry::Disclosed(entity())],
            },
            KnownConstraint::PaymentAmount {
                currency: "USD".into(),
                min: None,
                max: None,
            },
            KnownConstraint::PaymentBudget {
                currency: "USD".into(),
                max: 1,
            },
            KnownConstraint::PaymentRecurrence {
                frequency: "MNTH".into(),
                start_date: "2026-01-01".into(),
                end_date: None,
                number: None,
            },
            KnownConstraint::AgentRecurrence {
                frequency: "MNTH".into(),
                start_date: "2026-01-01".into(),
                end_date: None,
                max_occurrences: None,
            },
            KnownConstraint::PaymentReference {
                conditional_transaction_id: "t".into(),
            },
        ];

        let mut emitted: Vec<String> = every_variant
            .iter()
            .map(|variant| {
                let value = serde_json::to_value(variant).unwrap();
                value["type"].as_str().unwrap().to_owned()
            })
            .collect();
        emitted.sort();

        let mut declared: Vec<String> = KNOWN_CONSTRAINT_TYPES
            .iter()
            .map(|t| (*t).to_owned())
            .collect();
        declared.sort();

        assert_eq!(
            emitted, declared,
            "KNOWN_CONSTRAINT_TYPES has drifted from the KnownConstraint variants"
        );
    }

    #[test]
    fn mandate_mode_serde() {
        let m = MandateMode::Autonomous;
        let json = serde_json::to_string(&m).unwrap();
        assert_eq!(json, r#""autonomous""#);
        let back: MandateMode = serde_json::from_str(&json).unwrap();
        assert_eq!(m, back);
    }

    /// The constraint model is extensible and its registered set grows, so a
    /// verifier will meet tags newer than itself. A representation that cannot
    /// hold one has no way to apply a strictness policy to it.
    #[test]
    fn unknown_constraint_type_is_representable() {
        let json = r#"{"type":"urn:example:loyalty-points","tier":"gold","points":42}"#;
        let parsed: Constraint =
            serde_json::from_str(json).expect("an unrecognized constraint type must parse");

        let Constraint::Unknown {
            constraint_type,
            fields,
        } = &parsed
        else {
            panic!("expected the unknown arm, got {parsed:?}");
        };
        assert_eq!(constraint_type, "urn:example:loyalty-points");
        assert_eq!(fields.get("tier").and_then(|v| v.as_str()), Some("gold"));
        assert_eq!(fields.get("points").and_then(|v| v.as_i64()), Some(42));
    }

    /// Preserving the payload is only worth anything if it survives a round
    /// trip, which the specification requires.
    #[test]
    fn unknown_constraint_round_trips_with_every_field() {
        let json = r#"{"type":"com.acme.shipping","nested":{"a":[1,2]},"flag":true}"#;
        let parsed: Constraint = serde_json::from_str(json).unwrap();
        let reserialized = serde_json::to_string(&parsed).unwrap();

        let before: serde_json::Value = serde_json::from_str(json).unwrap();
        let after: serde_json::Value = serde_json::from_str(&reserialized).unwrap();
        assert_eq!(before, after);
    }

    /// An unrecognized type carrying nothing but `type` is still a constraint.
    #[test]
    fn bare_unknown_constraint_parses() {
        let parsed: Constraint = serde_json::from_str(r#"{"type":"com.acme.bare"}"#).unwrap();
        let Constraint::Unknown { fields, .. } = &parsed else {
            panic!("expected the unknown arm, got {parsed:?}");
        };
        assert!(fields.is_empty());
    }

    /// The dangerous case, and the reason `KNOWN_CONSTRAINT_TYPES` exists. A
    /// recognized tag that fails to deserialize must surface that error. If it
    /// degraded into the unknown arm, a permissive policy would skip it and a
    /// malformed recognized constraint would pass unreported.
    #[test]
    fn malformed_known_constraint_errors_rather_than_becoming_unknown() {
        // `payment.amount` requires `currency`.
        let err = serde_json::from_str::<Constraint>(r#"{"type":"payment.amount"}"#).unwrap_err();
        assert!(
            err.to_string().contains("currency"),
            "expected a missing-field error, got: {err}"
        );
    }

    /// A constraint has to be an object carrying a string `type`.
    #[test]
    fn malformed_constraint_shapes_are_rejected() {
        assert!(serde_json::from_str::<Constraint>("[1,2]").is_err());
        assert!(serde_json::from_str::<Constraint>(r#"{"currency":"USD"}"#).is_err());
        assert!(serde_json::from_str::<Constraint>(r#"{"type":7}"#).is_err());
    }

    /// Recognized types are unaffected by the open representation.
    #[test]
    fn known_constraint_still_parses_into_its_variant() {
        let json = r#"{"type":"payment.amount","currency":"USD","min":10000,"max":40000}"#;
        let parsed: Constraint = serde_json::from_str(json).unwrap();
        assert!(matches!(
            parsed,
            Constraint::Known(KnownConstraint::PaymentAmount { .. })
        ));
    }

    /// The realistic shape: a vendor extension alongside registered types.
    #[test]
    fn mixed_constraint_list_parses_both_arms() {
        let json = r#"[
            {"type":"payment.amount","currency":"USD","max":40000},
            {"type":"urn:example:experimental","scope":"wide"}
        ]"#;
        let parsed: Vec<Constraint> = serde_json::from_str(json).unwrap();
        assert_eq!(parsed.len(), 2);
        assert!(matches!(parsed[0], Constraint::Known(_)));
        assert!(matches!(parsed[1], Constraint::Unknown { .. }));
    }
}
