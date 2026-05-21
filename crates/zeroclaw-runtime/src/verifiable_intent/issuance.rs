//! L2 and L3 credential issuance.
//!
//! Provides builders for constructing VI credentials with proper SD-JWT
//! serialization and key binding. L1 issuance is out of scope (performed by
//! external credential model_providers / issuers).

use ring::signature::EcdsaKeyPair;
use serde_json::json;

use crate::verifiable_intent::crypto::{create_disclosure, jws_sign, sd_hash, serialize_sd_jwt};
use crate::verifiable_intent::error::{ViError, ViErrorKind};
use crate::verifiable_intent::types::{
    CheckoutL3Mandate, FinalCheckoutMandate, FinalPaymentMandate, Jwk, OpenCheckoutMandate,
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
///
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
    /// Disclosure hash of the checkout mandate (needed for `payment.reference`).
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
pub fn create_layer3_payment(
    serialized_l2: &str,
    mandate: &PaymentL3Mandate,
    agent_key: &EcdsaKeyPair,
    agent_jwk: &Jwk,
    iat: i64,
    exp: i64,
) -> Result<L3PaymentResult, ViError> {
    let l2_hash = sd_hash(serialized_l2);

    let header = json!({
        "alg": "ES256",
        "typ": "kb-sd-jwt",
        "jwk": agent_jwk,
        "kid": agent_jwk.x
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
pub fn create_layer3_checkout(
    serialized_l2: &str,
    mandate: &CheckoutL3Mandate,
    agent_key: &EcdsaKeyPair,
    agent_jwk: &Jwk,
    iat: i64,
    exp: i64,
) -> Result<L3CheckoutResult, ViError> {
    let l2_hash = sd_hash(serialized_l2);

    let header = json!({
        "alg": "ES256",
        "typ": "kb-sd-jwt",
        "jwk": agent_jwk,
        "kid": agent_jwk.x
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
    use crate::verifiable_intent::crypto::{generate_ec_p256, load_key_pair};
    use crate::verifiable_intent::types::{
        Cnf, Constraint, Entity, FulfillmentLineItem, PaymentAmount, PaymentInstrument,
    };

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
            jwk: agent_jwk,
            kid: Some("agent-key-1".into()),
        };

        let checkout = OpenCheckoutMandate {
            vct: "mandate.checkout.open".into(),
            cnf: cnf.clone(),
            constraints: vec![Constraint::AllowedMerchant {
                allowed_merchants: vec![Entity {
                    id: None,
                    name: "Test Store".into(),
                    website: "https://store.example.com".into(),
                }],
            }],
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
            constraints: vec![Constraint::PaymentAmount {
                currency: "USD".into(),
                min: Some(10000),
                max: Some(40000),
            }],
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
                jwk: agent_jwk1,
                kid: Some("key-1".into()),
            },
            constraints: vec![],
            prompt_summary: None,
        };
        let payment = OpenPaymentMandate {
            vct: "mandate.payment.open".into(),
            cnf: Cnf {
                jwk: agent_jwk2,
                kid: Some("key-2".into()),
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

    #[test]
    fn create_l3_payment_and_checkout() {
        let (agent_pkcs8, agent_jwk) = generate_ec_p256().unwrap();
        let agent_key = load_key_pair(&agent_pkcs8).unwrap();
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
            &agent_jwk,
            1_700_000_000,
            1_700_000_300,
        )
        .unwrap();
        assert!(!l3a.serialized.is_empty());

        let l3b = create_layer3_checkout(
            l2_serialized,
            &l3b_mandate,
            &agent_key,
            &agent_jwk,
            1_700_000_000,
            1_700_000_300,
        )
        .unwrap();
        assert!(!l3b.serialized.is_empty());
    }
}
