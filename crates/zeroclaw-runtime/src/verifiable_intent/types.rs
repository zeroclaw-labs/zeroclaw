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

        // The placeholder object carries `...` and nothing else, so the key's
        // presence decides what the entry is: either a well-formed reference or
        // malformed input. It must never fall through to `T`.
        //
        // Falling through is not a harmless leniency. Entry types accept
        // unknown fields, so an object carrying the marker beside a complete
        // set of entry fields would deserialize as an ordinary disclosed value
        // with the marker discarded, and an allowlist check would then
        // authorize against fields supplied outside the disclosure the marker
        // names. Testing membership rather than the whole shape, as the
        // reference implementation does, has the same effect by a different
        // route: it keeps the entry as a reference and drops the siblings.
        if let serde_json::Value::Object(object) = &value
            && object.contains_key("...")
        {
            if object.len() != 1 {
                return Err(D::Error::custom(
                    "a disclosure reference must carry `...` as its only member",
                ));
            }
            let Some(serde_json::Value::String(hash)) = object.get("...") else {
                return Err(D::Error::custom(
                    "a disclosure reference hash must be a string",
                ));
            };
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
#[cfg_attr(test, derive(strum_macros::EnumIter))]
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
    Known {
        /// The recognized constraint.
        known: KnownConstraint,
        /// Fields the recognized variant did not consume.
        ///
        /// Preservation is required of every constraint object, not only of
        /// unrecognized types: a newer issuer may add a field to a type this
        /// build already knows, and a stage that reads the constraint later
        /// cannot recover what this parser discarded. The reference
        /// implementation carries the same thing on every constraint class.
        extra: serde_json::Map<String, serde_json::Value>,
    },
    /// A constraint type this build does not recognize, preserved in full.
    Unknown {
        constraint_type: String,
        fields: serde_json::Map<String, serde_json::Value>,
    },
}

impl From<KnownConstraint> for Constraint {
    /// Wrap a recognized constraint that carries no additional fields.
    ///
    /// This is a convenience over the recognized representation and asserts
    /// nothing about verification. The opaque verified types deliberately have
    /// no such conversion.
    fn from(known: KnownConstraint) -> Self {
        Self::Known {
            known,
            extra: serde_json::Map::new(),
        }
    }
}

/// The object form of a recognized constraint.
///
/// Serialization rebuilds the constraint from this, and deserialization uses it
/// to decide which input keys the variant consumed. Deriving the consumed set
/// from serde itself is what keeps a second list of field names from existing
/// beside the variants, where it could drift.
///
/// `serde_json::Error` is returned rather than a generic serde error because
/// both directions need this and the two error traits are distinct; each caller
/// maps it to its own.
fn known_constraint_object(
    known: &KnownConstraint,
) -> Result<serde_json::Map<String, serde_json::Value>, serde_json::Error> {
    match serde_json::to_value(known)? {
        serde_json::Value::Object(object) => Ok(object),
        other => Err(<serde_json::Error as serde::ser::Error>::custom(format!(
            "a recognized constraint must serialize to an object, got {other}"
        ))),
    }
}

/// Add preserved fields to a constraint object without displacing what the
/// constraint has already written.
///
/// Both arms write their authoritative keys first and then carry the fields the
/// parser did not consume. A preserved field colliding with one of those keys is
/// dropped rather than applied, because the authoritative keys are what a
/// checker evaluates: for a recognized constraint they are the variant's own
/// fields, and for an unrecognized one the `type` tag. Letting a preserved copy
/// win would serialize a constraint that reparses into a value nobody
/// constructed, and on the unrecognized arm it would put a different type in the
/// signed mandate from the one the checker acted on.
///
/// Neither collision can be reached from parsing, which removes the consumed
/// keys before preserving the rest. Both can be reached from a value built by
/// hand, and the public issuance path accepts such a value.
fn insert_preserved_fields(
    object: &mut serde_json::Map<String, serde_json::Value>,
    preserved: &serde_json::Map<String, serde_json::Value>,
) {
    for (key, value) in preserved {
        if !object.contains_key(key) {
            object.insert(key.clone(), value.clone());
        }
    }
}

impl Constraint {
    /// The object form this constraint serializes to.
    ///
    /// Shared by serialization and by the check below, so what is inspected is
    /// what is emitted rather than a reconstruction of it.
    fn to_object(&self) -> Result<serde_json::Map<String, serde_json::Value>, serde_json::Error> {
        match self {
            Constraint::Known { known, extra } => {
                let mut object = known_constraint_object(known)?;
                insert_preserved_fields(&mut object, extra);
                Ok(object)
            }
            Constraint::Unknown {
                constraint_type,
                fields,
            } => {
                let mut object = serde_json::Map::with_capacity(fields.len() + 1);
                object.insert(
                    "type".to_owned(),
                    serde_json::Value::String(constraint_type.clone()),
                );
                insert_preserved_fields(&mut object, fields);
                Ok(object)
            }
        }
    }

    /// Whether a checker would read these two constraints the same way.
    ///
    /// `check_single_constraint` destructures the recognized value for a known
    /// type and the tag for an unrecognized one, and evaluates nothing else.
    /// Preserved fields sit outside this comparison deliberately: dropping one
    /// is a preservation question, while changing what is compared here is an
    /// authorization question.
    fn evaluates_the_same_as(&self, other: &Self) -> bool {
        match (self, other) {
            (Constraint::Known { known: a, .. }, Constraint::Known { known: b, .. }) => a == b,
            (
                Constraint::Unknown {
                    constraint_type: a, ..
                },
                Constraint::Unknown {
                    constraint_type: b, ..
                },
            ) => a == b,
            _ => false,
        }
    }
}

impl Serialize for Constraint {
    /// Emit the constraint, refusing any value whose serialized form is a
    /// different constraint from the one in hand.
    ///
    /// The recognized value comes from the parser and the preserved fields come
    /// from the caller, so the two can disagree. There are four ways, and all
    /// of them are reachable only from a hand-built value, since the parser
    /// derives its preserved set from whatever the recognized value did not
    /// consume:
    ///
    /// - a preserved field named after one the variant emits,
    /// - a preserved `type` on the unrecognized arm,
    /// - a preserved field named after one the variant *omits*, which
    ///   `skip_serializing_if` keeps out of the emitted object entirely,
    /// - an unrecognized constraint carrying a tag this build does recognize,
    ///   whose bytes parse back as the recognized variant.
    ///
    /// The first two are refused by name while the object is built. The last
    /// two are invisible to a name check, so the rule is applied to the result
    /// instead: parse the emitted bytes back and compare what a checker would
    /// read. Stating it once over the whole type is what keeps the next
    /// variation from having to be foreseen.
    ///
    /// This matters because the issuance path is public and signs what it is
    /// handed. A mismatch here puts a constraint into a signed mandate that no
    /// checker ever evaluated, and the fourth case does so in the direction
    /// that fails open.
    fn serialize<S: Serializer>(&self, serializer: S) -> Result<S::Ok, S::Error> {
        use serde::ser::Error as _;

        let object = self.to_object().map_err(S::Error::custom)?;

        let reparsed = Constraint::deserialize(serde_json::Value::Object(object.clone())).map_err(
            |error| {
                S::Error::custom(format!(
                    "a constraint must serialize into something that parses back: {error}"
                ))
            },
        )?;
        if !self.evaluates_the_same_as(&reparsed) {
            return Err(S::Error::custom(format!(
                "the serialized form is a different constraint: a checker reads {self:?}, \
                 while the emitted bytes parse as {reparsed:?}"
            )));
        }

        object.serialize(serializer)
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
            Ok(known) => {
                let consumed = known_constraint_object(&known).map_err(D::Error::custom)?;
                let extra = object
                    .into_iter()
                    .filter(|(key, _)| !consumed.contains_key(key))
                    .collect();
                Ok(Constraint::Known { known, extra })
            }
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
        let c: Constraint = KnownConstraint::PaymentAmount {
            currency: "USD".into(),
            min: Some(10000),
            max: Some(40000),
        }
        .into();
        let json = serde_json::to_string(&c).unwrap();
        assert!(json.contains("payment.amount"));
        let back: Constraint = serde_json::from_str(&json).unwrap();
        assert_eq!(c, back);
    }

    #[test]
    fn constraint_merchant_serde_roundtrip() {
        let c: Constraint = KnownConstraint::AllowedMerchant {
            allowed_merchants: vec![DisclosableEntry::Disclosed(Entity {
                id: None,
                name: "Test Store".into(),
                website: "https://test.example.com".into(),
            })],
        }
        .into();
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
        let Constraint::Known {
            known: KnownConstraint::AllowedMerchant { allowed_merchants },
            ..
        } = &parsed
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

    /// A reference marker is decisive: an entry carrying `...` alongside a
    /// complete set of entry fields is malformed, not a disclosed value. The
    /// entry types ignore unknown fields, so without this the marker is
    /// discarded and the checker authorizes on fields presented outside the
    /// disclosure the marker names.
    #[test]
    fn a_complete_hybrid_entry_is_rejected_rather_than_silently_disclosed() {
        let merchant = r#"{
            "type": "mandate.checkout.allowed_merchant",
            "allowed_merchants": [
                {"...": "HASH", "name": "Store X", "website": "https://store-x.example"}
            ]
        }"#;
        assert!(
            serde_json::from_str::<Constraint>(merchant).is_err(),
            "a merchant entry carrying a reference marker beside a complete entity must not parse"
        );

        let line_items = r#"{
            "type": "mandate.checkout.line_items",
            "items": [
                {"...": "HASH", "id": "line-1", "acceptable_items": [], "quantity": 3}
            ]
        }"#;
        assert!(
            serde_json::from_str::<Constraint>(line_items).is_err(),
            "a line item carrying a reference marker beside a complete entry must not parse"
        );
    }

    /// The specification requires a parser to preserve fields it does not
    /// recognize, and says so for constraint objects generally rather than only
    /// for unrecognized types. A recognized variant that drops an extension
    /// field erases data no later stage can recover.
    #[test]
    fn known_constraint_preserves_unrecognized_fields() {
        let json = r#"{"type":"payment.amount","currency":"USD","max":40000,"acme_tier":"gold"}"#;
        let parsed: Constraint = serde_json::from_str(json).unwrap();

        let before: serde_json::Value = serde_json::from_str(json).unwrap();
        let after: serde_json::Value =
            serde_json::from_str(&serde_json::to_string(&parsed).unwrap()).unwrap();
        assert_eq!(
            before, after,
            "an unrecognized field on a recognized constraint must survive a round trip"
        );
    }

    /// Preservation has to hold for every recognized type, not for the one that
    /// happened to be tested. This walks the same variant array the tag-drift
    /// test uses, so a new variant is carried in here too rather than being
    /// covered by assertion.
    #[test]
    fn every_known_variant_preserves_an_unrecognized_field() {
        for variant in every_known_variant() {
            let mut object = match serde_json::to_value(&variant).unwrap() {
                serde_json::Value::Object(object) => object,
                other => panic!("a recognized constraint must serialize to an object: {other}"),
            };
            object.insert(
                "acme_extension".to_owned(),
                serde_json::Value::String("kept".to_owned()),
            );
            let before = serde_json::Value::Object(object);

            let parsed: Constraint = serde_json::from_value(before.clone()).unwrap();
            let Constraint::Known { extra, .. } = &parsed else {
                panic!("expected the known arm for {before}");
            };
            assert_eq!(
                extra.get("acme_extension").and_then(|v| v.as_str()),
                Some("kept"),
                "the extension field was dropped from {before}"
            );

            let after: serde_json::Value =
                serde_json::from_str(&serde_json::to_string(&parsed).unwrap()).unwrap();
            assert_eq!(before, after, "round trip lost data for {before}");
        }
    }

    /// An explicit `null` for a field that is skipped when absent is not part of
    /// the recognized serialization, so it is carried as an extra and survives.
    /// Preserving it is the specification's requirement; the wrinkle is only
    /// that it arrives through `extra` rather than through the variant.
    #[test]
    fn an_explicit_null_for_an_optional_field_is_preserved() {
        let json = r#"{"type":"payment.amount","currency":"USD","min":null}"#;
        let parsed: Constraint = serde_json::from_str(json).unwrap();

        let Constraint::Known {
            known: KnownConstraint::PaymentAmount { min, .. },
            extra,
        } = &parsed
        else {
            panic!("expected a payment amount, got {parsed:?}");
        };
        assert!(min.is_none(), "an explicit null parses as absent");
        assert_eq!(extra.get("min"), Some(&serde_json::Value::Null));

        let before: serde_json::Value = serde_json::from_str(json).unwrap();
        let after: serde_json::Value =
            serde_json::from_str(&serde_json::to_string(&parsed).unwrap()).unwrap();
        assert_eq!(before, after);
    }

    /// A recognized field outranks an extra of the same name. The parser cannot
    /// build that collision, but a caller can, and an extra that overwrote the
    /// variant would serialize a constraint that reparses into different values
    /// than the one in hand.
    #[test]
    fn a_recognized_field_is_not_overwritten_by_a_colliding_extra() {
        let mut extra = serde_json::Map::new();
        extra.insert(
            "currency".to_owned(),
            serde_json::Value::String("EUR".to_owned()),
        );
        let hand_built = Constraint::Known {
            known: KnownConstraint::PaymentAmount {
                currency: "USD".into(),
                min: None,
                max: Some(40000),
            },
            extra,
        };

        let serialized = serde_json::to_value(&hand_built).unwrap();
        assert_eq!(serialized["currency"], "USD");

        let reparsed: Constraint = serde_json::from_value(serialized).unwrap();
        let Constraint::Known {
            known: KnownConstraint::PaymentAmount { currency, .. },
            extra,
        } = &reparsed
        else {
            panic!("expected a payment amount, got {reparsed:?}");
        };
        assert_eq!(currency, "USD");
        assert!(extra.is_empty(), "the colliding extra must not be emitted");
    }

    /// The tag a checker acts on and the tag an issuer signs have to be the
    /// same string.
    ///
    /// An unrecognized constraint holds its authoritative tag in
    /// `constraint_type`, while the fields it preserves can also carry a `type`
    /// key once a caller builds the value by hand rather than parsing it. If
    /// the preserved key were written over the authoritative one, the checker
    /// would evaluate one constraint type while the issued mandate carried
    /// another, and the signature would cover the second.
    #[test]
    fn a_preserved_type_field_cannot_displace_the_unknown_constraint_tag() {
        use crate::verifiable_intent::verification::{StrictnessMode, check_constraints};

        let mut fields = serde_json::Map::new();
        fields.insert(
            "type".to_owned(),
            serde_json::Value::String("com.acme.other".to_owned()),
        );
        let hand_built = Constraint::Unknown {
            constraint_type: "com.acme.safe".to_owned(),
            fields,
        };

        let checked = check_constraints(
            std::slice::from_ref(&hand_built),
            &Fulfillment::default(),
            StrictnessMode::Strict,
            MandateMode::Autonomous,
        );
        let serialized = serde_json::to_value(&hand_built).unwrap();

        assert_eq!(
            Some(checked[0].constraint_type.as_str()),
            serialized["type"].as_str(),
            "the checker and the serialized form must report the same tag"
        );
        assert_eq!(serialized["type"], "com.acme.safe");

        let reparsed: Constraint = serde_json::from_value(serialized).unwrap();
        let Constraint::Unknown {
            constraint_type,
            fields,
        } = &reparsed
        else {
            panic!("expected the unknown arm, got {reparsed:?}");
        };
        assert_eq!(constraint_type, "com.acme.safe");
        assert!(fields.is_empty(), "the colliding field must not be emitted");
    }

    /// The invariant three review rounds have circled: the constraint a checker
    /// evaluates and the constraint that gets signed are the same one.
    ///
    /// A value satisfies it either by refusing to serialize at all or by
    /// emitting bytes that parse back to something a checker reads identically.
    /// Preserved fields are outside the comparison on purpose: nothing
    /// evaluates them, and `check_single_constraint` destructures exactly the
    /// recognized value or the unrecognized tag.
    ///
    /// Stating it as a property rather than as a particular refusal keeps these
    /// tests true if the enforcement strategy is ever replaced.
    fn signed_form_matches_the_evaluated_value(constraint: &Constraint) -> bool {
        let Ok(value) = serde_json::to_value(constraint) else {
            return true;
        };
        let Ok(reparsed) = serde_json::from_value::<Constraint>(value) else {
            return false;
        };
        match (constraint, &reparsed) {
            (Constraint::Known { known: a, .. }, Constraint::Known { known: b, .. }) => a == b,
            (
                Constraint::Unknown {
                    constraint_type: a, ..
                },
                Constraint::Unknown {
                    constraint_type: b, ..
                },
            ) => a == b,
            _ => false,
        }
    }

    /// A recognized field that serde omits is still authoritative.
    ///
    /// `skip_serializing_if` keeps an absent optional field out of the
    /// recognized object entirely, so the guard that protects emitted fields
    /// cannot see its name. A preserved entry of the same name then reaches the
    /// wire, and the constraint that gets signed carries a bound the checker
    /// never evaluated.
    #[test]
    fn a_preserved_field_cannot_introduce_a_bound_the_checker_did_not_evaluate() {
        let mut extra = serde_json::Map::new();
        extra.insert("min".to_owned(), serde_json::json!(100));
        let smuggled_bound = Constraint::Known {
            known: KnownConstraint::PaymentAmount {
                currency: "USD".into(),
                min: None,
                max: Some(40000),
            },
            extra,
        };
        assert!(
            signed_form_matches_the_evaluated_value(&smuggled_bound),
            "a preserved `min` must not add a bound the checker never evaluated"
        );

        // Control: a genuine extension field over the same variant is preserved
        // and changes nothing a checker reads.
        let mut extra = serde_json::Map::new();
        extra.insert("acme_tier".to_owned(), serde_json::json!("gold"));
        let extension = Constraint::Known {
            known: KnownConstraint::PaymentAmount {
                currency: "USD".into(),
                min: None,
                max: Some(40000),
            },
            extra,
        };
        assert!(
            signed_form_matches_the_evaluated_value(&extension),
            "a genuine extension field must still serialize"
        );
        assert!(serde_json::to_value(&extension).is_ok());
    }

    /// An unrecognized constraint may not serialize into a recognized one.
    ///
    /// `Constraint::Unknown` accepts any tag, including one this build knows.
    /// Such a value is refused by the checker, because an unevaluable
    /// constraint cannot bound an open mandate, while the bytes it serializes
    /// to parse back as the recognized variant that a verifier will evaluate
    /// normally. Alone among this class it fails open rather than closed: the
    /// checker says no and the signed mandate says yes.
    #[test]
    fn a_hand_built_unknown_may_not_become_a_recognized_constraint() {
        let mut fields = serde_json::Map::new();
        fields.insert("currency".to_owned(), serde_json::json!("USD"));
        let disguised = Constraint::Unknown {
            constraint_type: "payment.amount".to_owned(),
            fields,
        };
        assert!(
            signed_form_matches_the_evaluated_value(&disguised),
            "an unrecognized constraint must not be signed as a recognized one"
        );

        // Control: a genuinely unrecognized tag round-trips as itself.
        let mut fields = serde_json::Map::new();
        fields.insert("scope".to_owned(), serde_json::json!("wide"));
        let genuine = Constraint::Unknown {
            constraint_type: "urn:example:experimental".to_owned(),
            fields,
        };
        assert!(
            signed_form_matches_the_evaluated_value(&genuine),
            "an unrecognized constraint must still serialize"
        );
        assert!(serde_json::to_value(&genuine).is_ok());
    }

    /// The same rule on the recognized arm, measured rather than reasoned.
    ///
    /// A recognized variant emits its own tag through serde, so a preserved
    /// field named `type` collides with a key the recognized value has already
    /// written and the existing guard skips it. That follows from the tag
    /// attribute, and following from an attribute is not the same as having
    /// been observed, which is why it is asserted here.
    #[test]
    fn a_preserved_type_field_cannot_displace_a_known_constraint_tag() {
        let mut extra = serde_json::Map::new();
        extra.insert(
            "type".to_owned(),
            serde_json::Value::String("com.acme.other".to_owned()),
        );
        let hand_built = Constraint::Known {
            known: KnownConstraint::PaymentAmount {
                currency: "USD".into(),
                min: None,
                max: Some(40000),
            },
            extra,
        };

        let serialized = serde_json::to_value(&hand_built).unwrap();
        assert_eq!(serialized["type"], "payment.amount");

        let reparsed: Constraint = serde_json::from_value(serialized).unwrap();
        let Constraint::Known { known, extra } = &reparsed else {
            panic!("expected the known arm, got {reparsed:?}");
        };
        assert!(matches!(known, KnownConstraint::PaymentAmount { .. }));
        assert!(extra.is_empty(), "the colliding field must not be emitted");
    }

    /// Every `KnownConstraint` variant, enumerated from the enum itself.
    ///
    /// A hand-written list is what these tests used to walk, and it could not
    /// see a variant nobody added to it. The exhaustive `match` in
    /// `check_single_constraint` forces a new variant to be *handled*, but
    /// handling it there and leaving `KNOWN_CONSTRAINT_TYPES` alone compiles
    /// and passes, which is the fail-open that table exists to prevent.
    /// Deriving the iteration removes the hand-written step, so the tag test
    /// compares the enum against the table rather than one list against
    /// another. Verified by adding a ninth variant and watching this fail.
    fn every_known_variant() -> Vec<KnownConstraint> {
        use strum::IntoEnumIterator as _;
        KnownConstraint::iter().collect()
    }

    /// `KNOWN_CONSTRAINT_TYPES` decides whether a failed parse of a recognized
    /// tag surfaces as an error or degrades into `Unknown`. Adding a variant
    /// and forgetting the list would make that degradation silent, so pin one
    /// against the other: every variant's serialized tag must appear in the
    /// list, and the list must carry nothing the enum does not emit.
    #[test]
    fn known_constraint_tags_are_complete() {
        let mut emitted: Vec<String> = every_known_variant()
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
            Constraint::Known {
                known: KnownConstraint::PaymentAmount { .. },
                ..
            }
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
        assert!(matches!(parsed[0], Constraint::Known { .. }));
        assert!(matches!(parsed[1], Constraint::Unknown { .. }));
    }
}
