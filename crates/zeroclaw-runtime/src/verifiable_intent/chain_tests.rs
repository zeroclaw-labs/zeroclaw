//! End-to-end tests for the chain verifier, exercising the full issuance →
//! verification round trip plus the attack surface the verifier must close.

use ring::signature::EcdsaKeyPair;

use crate::verifiable_intent::chain::{verify_chain, ChainVerifyRequest};
use crate::verifiable_intent::crypto::{generate_ec_p256, jwk_to_public_bytes, load_key_pair, sd_hash};
use crate::verifiable_intent::error::ViErrorKind;
use crate::verifiable_intent::issuance::{
    create_layer2_autonomous, create_layer3_checkout, create_layer3_payment,
};
use crate::verifiable_intent::types::{
    CheckoutL3Mandate, Constraint, Cnf, Entity, FulfillmentLineItem, Jwk, OpenCheckoutMandate,
    OpenPaymentMandate, PaymentAmount, PaymentInstrument, PaymentL3Mandate,
};
use crate::verifiable_intent::verification::StrictnessMode;

/// Minimal L1 SD-JWT: issuer-signed JWS (ES256, typ sd+jwt) with a payload
/// matching the Layer1 schema and the subject's key binding.
fn make_l1(issuer_key: &EcdsaKeyPair, subject_jwk: &Jwk, iat: i64, exp: i64) -> (String, Cnf) {
    let cnf = Cnf {
        jwk: subject_jwk.clone(),
        kid: Some("user-key-1".into()),
    };
    let header = serde_json::json!({ "alg": "ES256", "typ": "sd+jwt" });
    let payload = serde_json::json!({
        "iss": "https://issuer.example.com",
        "sub": "user-123",
        "iat": iat,
        "exp": exp,
        "vct": "https://example.com/credential/card",
        "cnf": cnf,
        "pan_last_four": "1234",
        "scheme": "card",
    });
    let jws = crate::verifiable_intent::crypto::jws_sign(
        header.to_string().as_bytes(),
        payload.to_string().as_bytes(),
        issuer_key,
    )
    .unwrap();
    let serialized = crate::verifiable_intent::crypto::serialize_sd_jwt(&jws, &[], None);
    (serialized, cnf)
}

/// Build a full Autonomous chain: L1 + L2 (open mandates with constraints)
/// + L3a payment + L3b checkout.
struct FullChain {
    serialized_l1: String,
    serialized_l2: String,
    serialized_l3a: String,
    serialized_l3b: String,
    issuer_jwk: Jwk,
    agent_jwk: Jwk,
}

fn build_autonomous_chain(
    constraints: Vec<Constraint>,
    payee: Entity,
    amount: i64,
    checkout_jwt: &str,
) -> FullChain {
    let now = chrono::Utc::now().timestamp();
    let (issuer_pkcs8, issuer_jwk) = generate_ec_p256().unwrap();
    let issuer_key = load_key_pair(&issuer_pkcs8).unwrap();
    let (user_pkcs8, user_jwk) = generate_ec_p256().unwrap();
    let user_key = load_key_pair(&user_pkcs8).unwrap();
    let (agent_pkcs8, agent_jwk) = generate_ec_p256().unwrap();
    let agent_key = load_key_pair(&agent_pkcs8).unwrap();

    let (serialized_l1, _) = make_l1(&issuer_key, &user_jwk, now - 60, now + 3600);

    let cnf = Cnf {
        jwk: agent_jwk.clone(),
        kid: Some("agent-key-1".into()),
    };
    let checkout = OpenCheckoutMandate {
        vct: "mandate.checkout.open".into(),
        cnf: cnf.clone(),
        constraints: vec![],
        prompt_summary: Some("test".into()),
    };
    let payment = OpenPaymentMandate {
        vct: "mandate.payment.open".into(),
        cnf,
        payment_instrument: PaymentInstrument {
            instrument_type: "token".into(),
            id: "USDC".into(),
            description: None,
        },
        constraints,
    };
    let l2 = create_layer2_autonomous(
        &serialized_l1,
        &checkout,
        &payment,
        "https://network.example.com",
        "nonce-abc",
        &user_key,
        now - 60,
        now + 3600,
    )
    .unwrap();

    let checkout_hash = sd_hash(checkout_jwt);
    let l3a_mandate = PaymentL3Mandate {
        vct: "mandate.payment".into(),
        payment_instrument: PaymentInstrument {
            instrument_type: "token".into(),
            id: "USDC".into(),
            description: None,
        },
        payment_amount: PaymentAmount {
            currency: "USDC".into(),
            amount,
        },
        payee,
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
        &l2.serialized,
        &l3a_mandate,
        &agent_key,
        &agent_jwk,
        now - 60,
        now + 3600,
    )
    .unwrap();
    let l3b = create_layer3_checkout(
        &l2.serialized,
        &l3b_mandate,
        &agent_key,
        &agent_jwk,
        now - 60,
        now + 3600,
    )
    .unwrap();

    FullChain {
        serialized_l1,
        serialized_l2: l2.serialized,
        serialized_l3a: l3a.serialized,
        serialized_l3b: l3b.serialized,
        issuer_jwk,
        agent_jwk,
    }
}

fn payee(name: &str) -> Entity {
    Entity {
        id: Some(format!("solana:{name}")),
        name: name.into(),
        website: format!("https://{name}.example.com"),
    }
}

#[test]
fn valid_autonomous_chain_verifies() {
    let allowed = payee("contractor-alice");
    let chain = build_autonomous_chain(
        vec![
            Constraint::AllowedPayee {
                allowed_payees: vec![allowed.clone()],
            },
            Constraint::PaymentAmount {
                currency: "USDC".into(),
                min: Some(1),
                max: Some(10_000_000),
            },
        ],
        allowed,
        250,
        "merchant-checkout-jwt-1",
    );
    let req = ChainVerifyRequest {
        serialized_l1: &chain.serialized_l1,
        serialized_l2: &chain.serialized_l2,
        serialized_l3a: Some(&chain.serialized_l3a),
        serialized_l3b: Some(&chain.serialized_l3b),
    };
    let result = verify_chain(&req, &chain.issuer_jwk, StrictnessMode::Strict).unwrap();
    assert_eq!(result.fulfillment.payee.as_ref().unwrap().id, Some("solana:contractor-alice".into()));
    assert_eq!(result.fulfillment.amount, Some(250));
    assert_eq!(result.constraints.len(), 2);
    let _ = jwk_to_public_bytes(&chain.agent_jwk).unwrap();
}

#[test]
fn tampered_l3a_signature_is_rejected() {
    let allowed = payee("contractor-alice");
    let mut chain = build_autonomous_chain(
        vec![Constraint::AllowedPayee {
            allowed_payees: vec![allowed.clone()],
        }],
        allowed,
        250,
        "merchant-checkout-jwt-2",
    );
    // Flip a character in the L3a signature segment.
    let idx = chain.serialized_l3a.rfind('.').unwrap();
    let (head, tail) = chain.serialized_l3a.split_at(idx + 1);
    let mutated: String = tail
        .chars()
        .enumerate()
        .map(|(i, c)| if i == 0 { if c == 'A' { 'B' } else { 'A' } } else { c })
        .collect();
    chain.serialized_l3a = format!("{head}{mutated}");
    let req = ChainVerifyRequest {
        serialized_l1: &chain.serialized_l1,
        serialized_l2: &chain.serialized_l2,
        serialized_l3a: Some(&chain.serialized_l3a),
        serialized_l3b: Some(&chain.serialized_l3b),
    };
    let errs = verify_chain(&req, &chain.issuer_jwk, StrictnessMode::Strict).unwrap_err();
    assert!(
        errs.iter().any(|e| e.kind == ViErrorKind::SignatureInvalid),
        "expected SignatureInvalid, got {errs:?}"
    );
}

#[test]
fn tampered_l1_signature_is_rejected() {
    let allowed = payee("contractor-alice");
    let mut chain = build_autonomous_chain(
        vec![Constraint::AllowedPayee {
            allowed_payees: vec![allowed.clone()],
        }],
        allowed,
        250,
        "merchant-checkout-jwt-3",
    );
    // Mutate L1's signature segment (keeps payload JSON valid; the signature
    // check must fail with SignatureInvalid).
    let parts: Vec<&str> = chain.serialized_l1.split('~').collect();
    let jws_parts: Vec<&str> = parts[0].split('.').collect();
    let mutated_sig: String = jws_parts[2]
        .chars()
        .enumerate()
        .map(|(i, c)| if i == 1 { if c == 'A' { 'B' } else { 'A' } } else { c })
        .collect();
    let mutated_jws = format!("{}.{}.{}", jws_parts[0], jws_parts[1], mutated_sig);
    chain.serialized_l1 = format!("{mutated_jws}~");
    let req = ChainVerifyRequest {
        serialized_l1: &chain.serialized_l1,
        serialized_l2: &chain.serialized_l2,
        serialized_l3a: Some(&chain.serialized_l3a),
        serialized_l3b: Some(&chain.serialized_l3b),
    };
    let errs = verify_chain(&req, &chain.issuer_jwk, StrictnessMode::Strict).unwrap_err();
    assert!(
        errs.iter().any(|e| e.kind == ViErrorKind::SignatureInvalid),
        "expected SignatureInvalid, got {errs:?}"
    );
}

#[test]
fn wrong_issuer_key_is_rejected() {
    let allowed = payee("contractor-alice");
    let chain = build_autonomous_chain(
        vec![Constraint::AllowedPayee {
            allowed_payees: vec![allowed.clone()],
        }],
        allowed,
        250,
        "merchant-checkout-jwt-4",
    );
    let (_wrong_pkcs8, wrong_jwk) = generate_ec_p256().unwrap();
    let req = ChainVerifyRequest {
        serialized_l1: &chain.serialized_l1,
        serialized_l2: &chain.serialized_l2,
        serialized_l3a: Some(&chain.serialized_l3a),
        serialized_l3b: Some(&chain.serialized_l3b),
    };
    let errs = verify_chain(&req, &wrong_jwk, StrictnessMode::Strict).unwrap_err();
    assert!(
        errs.iter().any(|e| e.kind == ViErrorKind::SignatureInvalid),
        "expected SignatureInvalid, got {errs:?}"
    );
}

#[test]
fn payee_not_on_allowlist_is_rejected() {
    let allowed = payee("contractor-alice");
    let attacker = payee("attacker-evil");
    let chain = build_autonomous_chain(
        vec![Constraint::AllowedPayee {
            allowed_payees: vec![allowed],
        }],
        attacker,
        250,
        "merchant-checkout-jwt-5",
    );
    let req = ChainVerifyRequest {
        serialized_l1: &chain.serialized_l1,
        serialized_l2: &chain.serialized_l2,
        serialized_l3a: Some(&chain.serialized_l3a),
        serialized_l3b: Some(&chain.serialized_l3b),
    };
    let errs = verify_chain(&req, &chain.issuer_jwk, StrictnessMode::Strict).unwrap_err();
    assert!(
        errs.iter().any(|e| e.kind == ViErrorKind::PayeeNotAllowed),
        "expected PayeeNotAllowed, got {errs:?}"
    );
}

#[test]
fn amount_over_cap_is_rejected() {
    let allowed = payee("contractor-alice");
    let chain = build_autonomous_chain(
        vec![
            Constraint::AllowedPayee {
                allowed_payees: vec![allowed.clone()],
            },
            Constraint::PaymentAmount {
                currency: "USDC".into(),
                min: Some(1),
                max: Some(100),
            },
        ],
        allowed,
        50_000, // way over the 100-unit cap
        "merchant-checkout-jwt-6",
    );
    let req = ChainVerifyRequest {
        serialized_l1: &chain.serialized_l1,
        serialized_l2: &chain.serialized_l2,
        serialized_l3a: Some(&chain.serialized_l3a),
        serialized_l3b: Some(&chain.serialized_l3b),
    };
    let errs = verify_chain(&req, &chain.issuer_jwk, StrictnessMode::Strict).unwrap_err();
    assert!(
        errs.iter().any(|e| e.kind == ViErrorKind::AmountOutOfRange),
        "expected AmountOutOfRange, got {errs:?}"
    );
}

#[test]
fn missing_l3_in_autonomous_mode_is_rejected() {
    let allowed = payee("contractor-alice");
    let chain = build_autonomous_chain(
        vec![Constraint::AllowedPayee {
            allowed_payees: vec![allowed.clone()],
        }],
        allowed.clone(),
        250,
        "merchant-checkout-jwt-7",
    );
    let req = ChainVerifyRequest {
        serialized_l1: &chain.serialized_l1,
        serialized_l2: &chain.serialized_l2,
        serialized_l3a: None, // attacker drops L3a
        serialized_l3b: Some(&chain.serialized_l3b),
    };
    let errs = verify_chain(&req, &chain.issuer_jwk, StrictnessMode::Strict).unwrap_err();
    assert!(
        errs.iter().any(|e| e.kind == ViErrorKind::IncompleteMandatePair),
        "expected IncompleteMandatePair, got {errs:?}"
    );
}

#[test]
fn expired_chain_is_rejected() {
    let allowed = payee("contractor-alice");
    let now = chrono::Utc::now().timestamp();
    let (issuer_pkcs8, issuer_jwk) = generate_ec_p256().unwrap();
    let issuer_key = load_key_pair(&issuer_pkcs8).unwrap();
    let (user_pkcs8, user_jwk) = generate_ec_p256().unwrap();
    let user_key = load_key_pair(&user_pkcs8).unwrap();
    let (agent_pkcs8, agent_jwk) = generate_ec_p256().unwrap();
    let agent_key = load_key_pair(&agent_pkcs8).unwrap();

    let (serialized_l1, _) = make_l1(&issuer_key, &user_jwk, now - 7200, now - 3600); // expired
    let cnf = Cnf {
        jwk: agent_jwk.clone(),
        kid: Some("agent-key-1".into()),
    };
    let checkout = OpenCheckoutMandate {
        vct: "mandate.checkout.open".into(),
        cnf: cnf.clone(),
        constraints: vec![],
        prompt_summary: None,
    };
    let payment = OpenPaymentMandate {
        vct: "mandate.payment.open".into(),
        cnf,
        payment_instrument: PaymentInstrument {
            instrument_type: "token".into(),
            id: "USDC".into(),
            description: None,
        },
        constraints: vec![Constraint::AllowedPayee {
            allowed_payees: vec![allowed.clone()],
        }],
    };
    let l2 = create_layer2_autonomous(
        &serialized_l1,
        &checkout,
        &payment,
        "https://network.example.com",
        "nonce",
        &user_key,
        now - 7200,
        now - 3600,
    )
    .unwrap();
    let checkout_jwt = "merchant-checkout-jwt-8";
    let checkout_hash = sd_hash(checkout_jwt);
    let l3a = create_layer3_payment(
        &l2.serialized,
        &PaymentL3Mandate {
            vct: "mandate.payment".into(),
            payment_instrument: PaymentInstrument {
                instrument_type: "token".into(),
                id: "USDC".into(),
                description: None,
            },
            payment_amount: PaymentAmount {
                currency: "USDC".into(),
                amount: 250,
            },
            payee: allowed.clone(),
            transaction_id: checkout_hash.clone(),
        },
        &agent_key,
        &agent_jwk,
        now - 7200,
        now - 3600,
    )
    .unwrap();
    let l3b = create_layer3_checkout(
        &l2.serialized,
        &CheckoutL3Mandate {
            vct: "mandate.checkout".into(),
            checkout_jwt: checkout_jwt.into(),
            checkout_hash,
            line_items: None,
        },
        &agent_key,
        &agent_jwk,
        now - 7200,
        now - 3600,
    )
    .unwrap();

    let req = ChainVerifyRequest {
        serialized_l1: &serialized_l1,
        serialized_l2: &l2.serialized,
        serialized_l3a: Some(&l3a.serialized),
        serialized_l3b: Some(&l3b.serialized),
    };
    let errs = verify_chain(&req, &issuer_jwk, StrictnessMode::Strict).unwrap_err();
    assert!(
        errs.iter().any(|e| e.kind == ViErrorKind::Expired),
        "expected Expired, got {errs:?}"
    );
}
