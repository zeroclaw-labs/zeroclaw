//! L2 and L3 credential issuance.

use ring::signature::EcdsaKeyPair;
use serde_json::json;

use crate::verifiable_intent::crypto::{create_disclosure, jws_sign, sd_hash, serialize_sd_jwt};
use crate::verifiable_intent::error::{ViError, ViErrorKind};
use crate::verifiable_intent::types::{
    CheckoutL3Mandate, FinalCheckoutMandate, FinalPaymentMandate, OpenCheckoutMandate,
    OpenPaymentMandate, PaymentL3Mandate,
};

// ── L2 Immediate mode ────────────────────────────────────────────────

/// Result of creating an L2 Immediate credential.
#[derive(Debug)]
pub struct ImmediateL2Result {
    /// The serialized SD-JWT string (L1~disclosures~kb_jwt).
    pub serialized: String,
    /// The SD hash of the L1 that was bound.
    pub sd_hash: String,
}

/// Create an L2 Immediate-mode credential binding final checkout and payment values.
/// The caller must provide the serialized L1 SD-JWT and the user's signing key
/// (the private key corresponding to L1 `cnf.jwk`).
pub fn create_layer2_immediate(
    serialized_l1: &str,
    checkout: &FinalCheckoutMandate,
    payment: &FinalPaymentMandate,
    audience: &str,
    nonce: &str,
    user_key: &EcdsaKeyPair,
    iat: i64,
    exp: i64,
) -> Result<ImmediateL2Result, ViError> {
    let l1_hash = sd_hash(serialized_l1);

    // Create disclosures for mandates
    let checkout_value = serde_json::to_value(checkout).map_err(|e| {
        ViError::new(
            ViErrorKind::IssuanceInputInvalid,
            format!("checkout serialize: {e}"),
        )
    })?;
    let payment_value = serde_json::to_value(payment).map_err(|e| {
        ViError::new(
            ViErrorKind::IssuanceInputInvalid,
            format!("payment serialize: {e}"),
        )
    })?;

    let (checkout_disc, checkout_hash) = create_disclosure("checkout_mandate", &checkout_value)?;
    let (payment_disc, payment_hash) = create_disclosure("payment_mandate", &payment_value)?;

    let header = json!({
        "alg": "ES256",
        "typ": "kb-sd-jwt"
    });

    let payload = json!({
        "nonce": nonce,
        "aud": audience,
        "iat": iat,
        "exp": exp,
        "sd_hash": l1_hash,
        "_sd_alg": "sha-256",
        "_sd": [checkout_hash, payment_hash],
        "delegate_payload": [
            {"...": checkout_hash},
            {"...": payment_hash}
        ]
    });

    let kb_jwt = jws_sign(
        header.to_string().as_bytes(),
        payload.to_string().as_bytes(),
        user_key,
    )?;

    let serialized = serialize_sd_jwt(serialized_l1, &[checkout_disc, payment_disc], Some(&kb_jwt));

    Ok(ImmediateL2Result {
        serialized,
        sd_hash: l1_hash,
    })
}

// ── L2 Autonomous mode ───────────────────────────────────────────────

/// Result of creating an L2 Autonomous credential.
#[derive(Debug)]
pub struct AutonomousL2Result {
    /// The serialized SD-JWT string.
    pub serialized: String,
    /// The SD hash of the L1 that was bound.
    pub sd_hash: String,
    /// Disclosure hash of the checkout mandate (needed for `mandate.payment.reference`).
    pub checkout_disclosure_hash: String,
}

/// Create an L2 Autonomous-mode credential with constraints and agent key binding.
pub fn create_layer2_autonomous(
    serialized_l1: &str,
    checkout: &OpenCheckoutMandate,
    payment: &OpenPaymentMandate,
    audience: &str,
    nonce: &str,
    user_key: &EcdsaKeyPair,
    iat: i64,
    exp: i64,
) -> Result<AutonomousL2Result, ViError> {
    // Validate cnf parity between checkout and payment mandates
    if checkout.cnf != payment.cnf {
        return Err(ViError::new(
            ViErrorKind::ModeMismatch,
            "checkout and payment mandates must bind the same agent key (cnf mismatch)",
        ));
    }

    // An autonomous mandate must name the key it delegates to. §4.6 requires
    // `cnf.jwk` to carry a `kid`, and §5.7 rule 1 is what makes it load-bearing:
    // an L3 verifier resolves the agent's key by matching the L3 header's `kid`
    // against this one, so an L2 without it delegates to a key nothing can
    // resolve.
    //
    // `Jwk.kid` stays optional, because a bare public key and an L1 credential
    // both have legitimate use for one without an identifier. The requirement
    // belongs to this construction boundary, which is where the delegation is
    // actually declared.
    //
    // Checking the checkout side alone is sufficient because the parity check
    // above has already established that the two `cnf` values are equal. An
    // all-whitespace identifier is refused on the same reasoning as an empty
    // constraint currency in `verify_fulfillment_currency`: it is present
    // without naming anything.
    let names_a_key = checkout
        .cnf
        .jwk
        .kid
        .as_deref()
        .is_some_and(|kid| !kid.trim().is_empty());
    if !names_a_key {
        return Err(ViError::new(
            ViErrorKind::IssuanceInputInvalid,
            "an autonomous mandate must bind the agent key by identifier: \
             cnf.jwk.kid is required and must not be blank",
        ));
    }

    let l1_hash = sd_hash(serialized_l1);

    let checkout_value = serde_json::to_value(checkout).map_err(|e| {
        ViError::new(
            ViErrorKind::IssuanceInputInvalid,
            format!("checkout serialize: {e}"),
        )
    })?;
    let payment_value = serde_json::to_value(payment).map_err(|e| {
        ViError::new(
            ViErrorKind::IssuanceInputInvalid,
            format!("payment serialize: {e}"),
        )
    })?;

    let (checkout_disc, checkout_hash) = create_disclosure("checkout_mandate", &checkout_value)?;
    let (payment_disc, payment_hash) = create_disclosure("payment_mandate", &payment_value)?;

    let header = json!({
        "alg": "ES256",
        "typ": "kb-sd-jwt+kb"
    });

    let payload = json!({
        "nonce": nonce,
        "aud": audience,
        "iat": iat,
        "exp": exp,
        "sd_hash": l1_hash,
        "_sd_alg": "sha-256",
        "_sd": [checkout_hash, payment_hash],
        "delegate_payload": [
            {"...": checkout_hash},
            {"...": payment_hash}
        ]
    });

    let kb_jwt = jws_sign(
        header.to_string().as_bytes(),
        payload.to_string().as_bytes(),
        user_key,
    )?;

    let serialized = serialize_sd_jwt(serialized_l1, &[checkout_disc, payment_disc], Some(&kb_jwt));

    Ok(AutonomousL2Result {
        serialized,
        sd_hash: l1_hash,
        checkout_disclosure_hash: checkout_hash,
    })
}

// ── L3 Issuance (Autonomous only) ────────────────────────────────────

/// Result of creating an L3 payment credential.
#[derive(Debug)]
pub struct L3PaymentResult {
    /// The serialized KB-SD-JWT for the payment network.
    pub serialized: String,
}

/// Create an L3a payment mandate signed by the agent's key.
///
/// `agent_kid` names the key in the L2 mandate's `cnf.jwk.kid`. It is a key
/// identifier and not a key: `credential-format.md` §13.3 requires the header to
/// carry `kid` and forbids a `jwk`, and §13.4 rule 7 gives the reason, which is
/// that a verifier resolves the agent's key out of L2 and must never trust one
/// the L3 asserts about itself.
pub fn create_layer3_payment(
    serialized_l2: &str,
    mandate: &PaymentL3Mandate,
    agent_key: &EcdsaKeyPair,
    agent_kid: &str,
    iat: i64,
    exp: i64,
) -> Result<L3PaymentResult, ViError> {
    let l2_hash = sd_hash(serialized_l2);

    let header = json!({
        "alg": "ES256",
        "typ": "kb-sd-jwt",
        "kid": agent_kid
    });

    let mandate_value = serde_json::to_value(mandate).map_err(|e| {
        ViError::new(
            ViErrorKind::IssuanceInputInvalid,
            format!("L3a mandate serialize: {e}"),
        )
    })?;

    let payload = json!({
        "iat": iat,
        "exp": exp,
        "sd_hash": l2_hash,
        "mandate": mandate_value
    });

    let jwt = jws_sign(
        header.to_string().as_bytes(),
        payload.to_string().as_bytes(),
        agent_key,
    )?;

    // L3 has no disclosures in the reference implementation
    let serialized = serialize_sd_jwt(&jwt, &[], None);

    Ok(L3PaymentResult { serialized })
}

/// Result of creating an L3 checkout credential.
#[derive(Debug)]
pub struct L3CheckoutResult {
    /// The serialized KB-SD-JWT for the merchant.
    pub serialized: String,
}

/// Create an L3b checkout mandate signed by the agent's key.
///
/// `agent_kid` names the key in the L2 mandate's `cnf.jwk.kid`, on the same
/// rule as [`create_layer3_payment`].
pub fn create_layer3_checkout(
    serialized_l2: &str,
    mandate: &CheckoutL3Mandate,
    agent_key: &EcdsaKeyPair,
    agent_kid: &str,
    iat: i64,
    exp: i64,
) -> Result<L3CheckoutResult, ViError> {
    let l2_hash = sd_hash(serialized_l2);

    let header = json!({
        "alg": "ES256",
        "typ": "kb-sd-jwt",
        "kid": agent_kid
    });

    let mandate_value = serde_json::to_value(mandate).map_err(|e| {
        ViError::new(
            ViErrorKind::IssuanceInputInvalid,
            format!("L3b mandate serialize: {e}"),
        )
    })?;

    let payload = json!({
        "iat": iat,
        "exp": exp,
        "sd_hash": l2_hash,
        "mandate": mandate_value
    });

    let jwt = jws_sign(
        header.to_string().as_bytes(),
        payload.to_string().as_bytes(),
        agent_key,
    )?;

    let serialized = serialize_sd_jwt(&jwt, &[], None);

    Ok(L3CheckoutResult { serialized })
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::verifiable_intent::crypto::{
        decode_disclosure, generate_ec_p256, jws_decode_header, load_key_pair,
    };
    use crate::verifiable_intent::types::{
        Cnf, DisclosableEntry, Entity, FulfillmentLineItem, Jwk, KnownConstraint, MandateMode,
        PaymentAmount, PaymentInstrument,
    };
    use crate::verifiable_intent::verification::infer_mode_from_vct;

    fn test_issuer_l1() -> String {
        // Minimal L1 SD-JWT for testing (not cryptographically valid, just structural)
        "eyJhbGciOiJFUzI1NiIsInR5cCI6InNkK2p3dCJ9.eyJpc3MiOiJodHRwczovL2lzc3Vlci5leGFtcGxlLmNvbSJ9.sig~".to_string()
    }

    #[test]
    fn create_immediate_l2() {
        let (pkcs8, _jwk) = generate_ec_p256().unwrap();
        let user_key = load_key_pair(&pkcs8).unwrap();
        let l1 = test_issuer_l1();

        let checkout = FinalCheckoutMandate {
            vct: "mandate.checkout".into(),
            checkout_jwt: "merchant.jwt.here".into(),
            checkout_hash: sd_hash("merchant.jwt.here"),
        };
        let payment = FinalPaymentMandate {
            vct: "mandate.payment".into(),
            payment_instrument: PaymentInstrument {
                instrument_type: "card".into(),
                id: "tok-1".into(),
                description: None,
            },
            currency: "USD".into(),
            amount: 27999,
            payee: Entity {
                id: None,
                name: "Test Store".into(),
                website: "https://store.example.com".into(),
            },
            transaction_id: sd_hash("merchant.jwt.here"),
        };

        let result = create_layer2_immediate(
            &l1,
            &checkout,
            &payment,
            "https://network.example.com",
            "nonce-123",
            &user_key,
            1_700_000_000,
            1_700_000_900,
        )
        .unwrap();

        assert!(!result.serialized.is_empty());
        assert!(!result.sd_hash.is_empty());
        // The serialized form should contain the L1 as prefix
        assert!(result.serialized.starts_with(&l1));
    }

    #[test]
    fn create_autonomous_l2() {
        let (user_pkcs8, _user_jwk) = generate_ec_p256().unwrap();
        let user_key = load_key_pair(&user_pkcs8).unwrap();
        let (_agent_pkcs8, agent_jwk) = generate_ec_p256().unwrap();
        let l1 = test_issuer_l1();

        let cnf = Cnf {
            jwk: Jwk {
                kid: Some("agent-key-1".into()),
                ..agent_jwk
            },
        };

        let checkout = OpenCheckoutMandate {
            vct: "mandate.checkout.open".into(),
            cnf: cnf.clone(),
            constraints: vec![
                KnownConstraint::AllowedMerchant {
                    allowed_merchants: vec![DisclosableEntry::Disclosed(Entity {
                        id: None,
                        name: "Test Store".into(),
                        website: "https://store.example.com".into(),
                    })],
                }
                .into(),
            ],
            prompt_summary: Some("Buy a test product".into()),
        };
        let payment = OpenPaymentMandate {
            vct: "mandate.payment.open".into(),
            cnf,
            payment_instrument: PaymentInstrument {
                instrument_type: "card".into(),
                id: "tok-1".into(),
                description: None,
            },
            constraints: vec![
                KnownConstraint::PaymentAmount {
                    currency: "USD".into(),
                    min: Some(10000),
                    max: Some(40000),
                }
                .into(),
            ],
        };

        let result = create_layer2_autonomous(
            &l1,
            &checkout,
            &payment,
            "https://network.example.com",
            "nonce-456",
            &user_key,
            1_700_000_000,
            1_700_086_400,
        )
        .unwrap();

        assert!(!result.serialized.is_empty());
        assert!(!result.checkout_disclosure_hash.is_empty());
    }

    #[test]
    fn create_autonomous_l2_cnf_mismatch_fails() {
        let (user_pkcs8, _user_jwk) = generate_ec_p256().unwrap();
        let user_key = load_key_pair(&user_pkcs8).unwrap();
        let (_a1, agent_jwk1) = generate_ec_p256().unwrap();
        let (_a2, agent_jwk2) = generate_ec_p256().unwrap();
        let l1 = test_issuer_l1();

        let checkout = OpenCheckoutMandate {
            vct: "mandate.checkout.open".into(),
            cnf: Cnf {
                jwk: Jwk {
                    kid: Some("key-1".into()),
                    ..agent_jwk1
                },
            },
            constraints: vec![],
            prompt_summary: None,
        };
        let payment = OpenPaymentMandate {
            vct: "mandate.payment.open".into(),
            cnf: Cnf {
                jwk: Jwk {
                    kid: Some("key-2".into()),
                    ..agent_jwk2
                },
            },
            payment_instrument: PaymentInstrument {
                instrument_type: "card".into(),
                id: "tok-1".into(),
                description: None,
            },
            constraints: vec![],
        };

        let err = create_layer2_autonomous(
            &l1,
            &checkout,
            &payment,
            "https://network.example.com",
            "nonce",
            &user_key,
            1_700_000_000,
            1_700_086_400,
        )
        .unwrap_err();

        assert_eq!(err.kind, ViErrorKind::ModeMismatch);
    }

    /// An autonomous L2 must not issue without `cnf.jwk.kid`.
    ///
    /// `credential-format.md` §4.6 requires every autonomous mandate to carry
    /// `cnf.jwk` with a `kid` member, and §5.7 rule 1 is what makes it
    /// load-bearing: an L3 verifier resolves the delegated key by matching the
    /// L3 header's `kid` against this value. An L2 issued without one leaves the
    /// L3 credentials no conformant key-binding path.
    ///
    /// Parity alone does not catch this. Two mandates whose nested JWK is
    /// missing the identifier are equal to each other, so the pair check passes
    /// and a structurally invalid credential gets signed. `generate_ec_p256`
    /// returns `kid: None`, so that is the state a caller starts from.
    #[test]
    fn autonomous_issuance_requires_a_nested_kid() {
        let (user_pkcs8, _user_jwk) = generate_ec_p256().unwrap();
        let user_key = load_key_pair(&user_pkcs8).unwrap();
        let (_agent_pkcs8, agent_jwk) = generate_ec_p256().unwrap();

        let instrument = || PaymentInstrument {
            instrument_type: "card".into(),
            id: "tok-1".into(),
            description: None,
        };
        let issue = |cnf: Cnf| {
            create_layer2_autonomous(
                &test_issuer_l1(),
                &OpenCheckoutMandate {
                    vct: "mandate.checkout.open".into(),
                    cnf: cnf.clone(),
                    constraints: vec![],
                    prompt_summary: None,
                },
                &OpenPaymentMandate {
                    vct: "mandate.payment.open".into(),
                    cnf,
                    payment_instrument: instrument(),
                    constraints: vec![],
                },
                "https://network.example.com",
                "nonce-kid",
                &user_key,
                1_700_000_000,
                1_700_086_400,
            )
        };

        // The pair matches, and the shared JWK carries no identifier.
        let err = issue(Cnf {
            jwk: Jwk {
                kid: None,
                ..agent_jwk.clone()
            },
        })
        .expect_err("an autonomous mandate pair without `cnf.jwk.kid` must be refused");
        assert_eq!(err.kind, ViErrorKind::IssuanceInputInvalid);
        assert!(
            err.message.contains("kid"),
            "the refusal must name the missing member: {}",
            err.message
        );

        // Control: the same pair issues once the identifier is present, so the
        // refusal above is attributable to the missing `kid` and not to the
        // empty constraint lists or anything else in the fixture.
        issue(Cnf {
            jwk: Jwk {
                kid: Some("agent-key-1".into()),
                ..agent_jwk
            },
        })
        .expect("a pair carrying `cnf.jwk.kid` must still issue");
    }

    #[test]
    fn create_l3_payment_and_checkout() {
        let (agent_pkcs8, _agent_jwk) = generate_ec_p256().unwrap();
        let agent_key = load_key_pair(&agent_pkcs8).unwrap();
        let agent_kid = "agent-key-1";
        let l2_serialized = "l2.serialized.form~disc1~disc2~kb.jwt";

        let checkout_jwt = "merchant.checkout.jwt";
        let checkout_hash = sd_hash(checkout_jwt);

        let l3a_mandate = PaymentL3Mandate {
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
            payee: Entity {
                id: None,
                name: "Test Store".into(),
                website: "https://store.example.com".into(),
            },
            transaction_id: checkout_hash.clone(),
        };

        let l3b_mandate = CheckoutL3Mandate {
            vct: "mandate.checkout".into(),
            checkout_jwt: checkout_jwt.into(),
            checkout_hash,
            line_items: Some(vec![FulfillmentLineItem {
                item_id: "SKU001".into(),
                quantity: 1,
            }]),
        };

        let l3a = create_layer3_payment(
            l2_serialized,
            &l3a_mandate,
            &agent_key,
            agent_kid,
            1_700_000_000,
            1_700_000_300,
        )
        .unwrap();
        assert!(!l3a.serialized.is_empty());

        let l3b = create_layer3_checkout(
            l2_serialized,
            &l3b_mandate,
            &agent_key,
            agent_kid,
            1_700_000_000,
            1_700_000_300,
        )
        .unwrap();
        assert!(!l3b.serialized.is_empty());
    }

    /// An L3 header names the agent's key and never carries it.
    ///
    /// `credential-format.md` §13.3 rule 3 requires `kid` and forbids `jwk`, and
    /// §13.4 rule 7 says why: a verifier resolves the agent's key from the L2
    /// mandate's `cnf.jwk` by matching this `kid`, so a `jwk` in the header is a
    /// key the credential asserts about itself. Emitting one invites a verifier
    /// to trust it.
    ///
    /// Both L3 constructors are walked, because the header was built twice and
    /// a fix applied to one of them would leave the other emitting the
    /// forbidden parameter.
    #[test]
    fn an_l3_header_carries_the_kid_and_never_the_agent_jwk() {
        let (agent_pkcs8, _agent_jwk) = generate_ec_p256().unwrap();
        let agent_key = load_key_pair(&agent_pkcs8).unwrap();
        let agent_kid = "agent-key-1";
        let l2 = "l2.serialized.form~disc1~kb.jwt";
        let checkout_jwt = "merchant.checkout.jwt";

        let payment = create_layer3_payment(
            l2,
            &PaymentL3Mandate {
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
                payee: Entity {
                    id: None,
                    name: "Test Store".into(),
                    website: "https://store.example.com".into(),
                },
                transaction_id: sd_hash(checkout_jwt),
            },
            &agent_key,
            agent_kid,
            1_700_000_000,
            1_700_000_300,
        )
        .unwrap();

        let checkout = create_layer3_checkout(
            l2,
            &CheckoutL3Mandate {
                vct: "mandate.checkout".into(),
                checkout_jwt: checkout_jwt.into(),
                checkout_hash: sd_hash(checkout_jwt),
                line_items: None,
            },
            &agent_key,
            agent_kid,
            1_700_000_000,
            1_700_000_300,
        )
        .unwrap();

        for (label, serialized) in [("L3a", &payment.serialized), ("L3b", &checkout.serialized)] {
            let jwt = serialized
                .split('~')
                .next()
                .expect("a serialized L3 starts with its JWT");
            let header = jws_decode_header(jwt).expect("the L3 header must decode");

            assert_eq!(
                header.get("kid").and_then(serde_json::Value::as_str),
                Some(agent_kid),
                "{label} must name the L2-bound key"
            );
            assert!(
                header.get("jwk").is_none(),
                "{label} header carries a forbidden `jwk`: {header}"
            );
            assert_eq!(
                header.get("typ").and_then(serde_json::Value::as_str),
                Some("kb-sd-jwt"),
                "{label} typ"
            );
        }
    }

    /// Issuance passes a mandate's `vct` through into the disclosure it signs,
    /// and until now nothing read the emitted value back: every assertion in
    /// this module checks that a serialized form exists, never what VCT it
    /// carries. The registry could therefore move on the verification side
    /// while issuance kept emitting the old string, with the whole suite still
    /// green.
    ///
    /// Asserting both halves together is what stops that. The first assertion
    /// pins what issuance emits; the second requires verification to recognize
    /// that exact string, so the two cannot drift apart.
    #[test]
    fn issuance_emits_mandate_vcts_that_verification_recognizes() {
        const CHECKOUT_VCT: &str = "mandate.checkout.open";
        const PAYMENT_VCT: &str = "mandate.payment.open";

        let (user_pkcs8, _user_jwk) = generate_ec_p256().unwrap();
        let user_key = load_key_pair(&user_pkcs8).unwrap();
        let (_agent_pkcs8, agent_jwk) = generate_ec_p256().unwrap();
        let cnf = Cnf {
            jwk: Jwk {
                kid: Some("agent-key-1".into()),
                ..agent_jwk
            },
        };

        let checkout = OpenCheckoutMandate {
            vct: CHECKOUT_VCT.into(),
            cnf: cnf.clone(),
            constraints: vec![],
            prompt_summary: None,
        };
        let payment = OpenPaymentMandate {
            vct: PAYMENT_VCT.into(),
            cnf,
            payment_instrument: PaymentInstrument {
                instrument_type: "card".into(),
                id: "tok-1".into(),
                description: None,
            },
            constraints: vec![],
        };

        let result = create_layer2_autonomous(
            &test_issuer_l1(),
            &checkout,
            &payment,
            "https://network.example.com",
            "nonce-vct",
            &user_key,
            1_700_000_000,
            1_700_086_400,
        )
        .unwrap();

        // Read the VCTs back out of the disclosures issuance actually produced.
        // The segments are split by hand because the serialized L1 this helper
        // supplies already ends in `~`, which `parse_sd_jwt` refuses. That
        // defect is stage 3's to fix and is deliberately not worked around
        // here beyond reaching the disclosures.
        let emitted: Vec<String> = result
            .serialized
            .split('~')
            .filter_map(|segment| decode_disclosure(segment).ok())
            .filter_map(|disclosure| {
                disclosure
                    .claim_value()
                    .get("vct")?
                    .as_str()
                    .map(str::to_owned)
            })
            .collect();

        assert_eq!(
            emitted,
            vec![CHECKOUT_VCT.to_owned(), PAYMENT_VCT.to_owned()],
            "issuance must emit the mandate VCTs it was given, in order"
        );

        for vct in &emitted {
            assert_eq!(
                infer_mode_from_vct(vct).expect("verification must recognize an emitted VCT"),
                MandateMode::Autonomous,
                "verification must read `{vct}` as an open mandate"
            );
        }
    }
}
