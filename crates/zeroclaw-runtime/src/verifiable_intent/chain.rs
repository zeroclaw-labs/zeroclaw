//! End-to-end L1 → L2 → L3 credential chain verification.
//!
//! Implements the missing chain verifier for the Verifiable Intent subsystem
//! (issue #9328): `vi_verify` previously evaluated caller-supplied constraints
//! against a caller-supplied fulfillment, with a model in the caller position.
//! This module establishes the credential chain cryptographically — ES256
//! signatures across L1, L2 and L3, `sd_hash` bindings between layers, mandate
//! pairing for Immediate and Autonomous modes, VCT and `typ` headers, and the
//! `cnf` presence rules — and only then exposes constraint evaluation over the
//! verified chain. An empty or caller-forged fulfillment cannot satisfy any
//! constraint here: every value is derived from verified layer data.

use crate::verifiable_intent::crypto::{
    b64u_decode, jws_decode_header, jws_decode_payload, jws_verify, jwk_to_public_bytes,
    parse_sd_jwt, sd_hash,
};
use crate::verifiable_intent::error::{ViError, ViErrorKind};
use crate::verifiable_intent::types::{
    CheckoutL3Mandate, Constraint, CredentialChain, Cnf, Entity, Fulfillment, FulfillmentLineItem,
    Jwk, Layer1, MandateMode, PaymentL3Mandate, PaymentInstrument,
};
use crate::verifiable_intent::verification::{
    check_constraints, infer_mode_from_vct, verify_checkout_hash_binding,
    verify_l3_cross_reference, verify_sd_hash_binding, verify_timestamps, StrictnessMode,
};

/// A serialized credential chain, layer by layer.
///
/// `serialized_l1` and `serialized_l2` are SD-JWT strings. In Autonomous mode
/// the L3a (payment) and L3b (checkout) JWS strings are required.
#[derive(Debug, Clone)]
pub struct ChainVerifyRequest<'a> {
    pub serialized_l1: &'a str,
    pub serialized_l2: &'a str,
    pub serialized_l3a: Option<&'a str>,
    pub serialized_l3b: Option<&'a str>,
}

/// A fully verified chain: parsed layers, the mode, the constraints recovered
/// from the verified L2 disclosures, and the fulfillment derived from the
/// verified L3 (or final L2) mandates. Constraint evaluation must run against
/// these values — never against caller-supplied ones.
#[derive(Debug, Clone)]
pub struct VerifiedChain {
    pub chain: CredentialChain,
    pub mode: MandateMode,
    pub constraints: Vec<Constraint>,
    pub fulfillment: Fulfillment,
    pub l1_cnf: Cnf,
}

/// One disclosure resolved to its claim: `[salt, claim_name, claim_value]`.
#[derive(Debug, Clone)]
struct Disclosure {
    claim_name: String,
    claim_value: serde_json::Value,
    /// The disclosure's own hash (B64U(SHA-256(ASCII(disclosure_b64)))).
    hash: String,
}

/// Verify a full credential chain. The trust anchor is the issuer's public
/// JWK (the key that signed L1); every downstream key is recovered from the
/// chain itself (L1 `cnf` → L2 KB-JWT, L2 Autonomous `cnf` → L3 agent JWTs).
/// All failures are accumulated and returned; a single failed binding makes
/// the whole chain invalid.
pub fn verify_chain(
    req: &ChainVerifyRequest<'_>,
    issuer_jwk: &Jwk,
    strictness: StrictnessMode,
) -> Result<VerifiedChain, Vec<ViError>> {
    let mut errors: Vec<ViError> = Vec::new();

    // ── L1: issuer-signed SD-JWT ─────────────────────────────────────
    let (l1_jws, l1_disclosures, l1_kb) = parse_sd_jwt(req.serialized_l1).map_err(|e| vec![e])?;
    if l1_kb.is_some() {
        return Err(vec![ViError::new(
            ViErrorKind::ModeMismatch,
            "L1 must not carry a key-binding JWT",
        )]);
    }
    let l1_header = jws_decode_header(l1_jws).map_err(|e| vec![e])?;
    if l1_header.get("typ").and_then(|t| t.as_str()) != Some("sd+jwt") {
        return Err(vec![ViError::new(
            ViErrorKind::InvalidHeader,
            "L1 header typ must be 'sd+jwt'",
        )]);
    }
    let l1_payload = jws_decode_payload(l1_jws).map_err(|e| vec![e])?;
    let l1: Layer1 = serde_json::from_value(l1_payload).map_err(|e| {
        vec![ViError::new(
            ViErrorKind::InvalidPayload,
            format!("L1 payload does not match the Layer1 schema: {e}"),
        )]
    })?;
    // L1 signature against the issuer trust anchor.
    let issuer_pk = jwk_to_public_bytes(issuer_jwk).map_err(|e| vec![e])?;
    jws_verify(l1_jws, &issuer_pk).map_err(|e| vec![e])?;
    // L1 must carry the subject's key binding (used to verify L2).
    if l1.cnf.jwk.kty.is_empty() {
        return Err(vec![ViError::new(
            ViErrorKind::KeyMismatch,
            "L1 cnf.jwk is missing; cannot verify L2 key binding",
        )]);
    }
    verify_timestamps(l1.iat, l1.exp).map_err(|e| vec![e])?;
    let _ = l1_disclosures; // L1 disclosures are not consumed by the verifier.

    // ── L2: user-signed KB-SD-JWT binding the L1 ─────────────────────
    let (l2_issuer, l2_disclosures, l2_kb) = parse_sd_jwt(req.serialized_l2).map_err(|e| vec![e])?;
    // The L2 issuer segment is the L1 JWS (the full serialized L1 is nested
    // inside L2 with its own `~` separators). The authoritative binding
    // between the layers is the `sd_hash` claim, verified below; the issuer
    // segment must at least be non-empty.
    if l2_issuer.is_empty() {
        return Err(vec![ViError::new(
            ViErrorKind::InvalidPayload,
            "L2 issuer segment is empty",
        )]);
    }
    let kb_jwt = l2_kb.ok_or_else(|| {
        vec![ViError::new(
            ViErrorKind::InvalidPayload,
            "L2 must carry a key-binding JWT",
        )]
    })?;
    let l2_header = jws_decode_header(kb_jwt).map_err(|e| vec![e])?;
    let l2_typ = l2_header.get("typ").and_then(|t| t.as_str()).unwrap_or("");
    if l2_typ != "kb-sd-jwt" && l2_typ != "kb-sd-jwt+kb" {
        return Err(vec![ViError::new(
            ViErrorKind::InvalidHeader,
            format!("L2 header typ must be 'kb-sd-jwt' or 'kb-sd-jwt+kb', got '{l2_typ}'"),
        )]);
    }
    let l2_payload = jws_decode_payload(kb_jwt).map_err(|e| vec![e])?;
    // Extract the L2 claims individually; the payload shape (with `_sd` and
    // `delegate_payload`) differs from the internal Layer2 struct.
    let l2_nonce = l2_payload
        .get("nonce")
        .and_then(|v| v.as_str())
        .unwrap_or("")
        .to_string();
    let l2_aud = l2_payload
        .get("aud")
        .and_then(|v| v.as_str())
        .unwrap_or("")
        .to_string();
    let l2_iat = l2_payload.get("iat").and_then(|v| v.as_i64()).unwrap_or(0);
    let l2_exp = l2_payload.get("exp").and_then(|v| v.as_i64()).unwrap_or(0);
    let l2_sd_hash = l2_payload
        .get("sd_hash")
        .and_then(|v| v.as_str())
        .unwrap_or("")
        .to_string();
    let bound_sd_hashes: Vec<String> = l2_payload
        .get("_sd")
        .and_then(|v| v.as_array())
        .map(|arr| {
            arr.iter()
                .filter_map(|h| h.as_str().map(String::from))
                .collect()
        })
        .unwrap_or_default();
    // L2 signature against the L1 subject key (cnf.jwk).
    let l1_subject_pk = jwk_to_public_bytes(&l1.cnf.jwk).map_err(|e| vec![e])?;
    jws_verify(kb_jwt, &l1_subject_pk).map_err(|e| vec![e])?;
    // sd_hash binding: L2 binds the exact serialized L1.
    verify_sd_hash_binding(&l2_sd_hash, req.serialized_l1).map_err(|e| vec![e])?;
    verify_timestamps(l2_iat, l2_exp).map_err(|e| vec![e])?;

    // ── Disclosures → mandates ───────────────────────────────────────
    let resolved = resolve_disclosures(&l2_disclosures).map_err(|e| vec![e])?;
    // Every disclosure hash must be bound in L2 `_sd`.
    for d in &resolved {
        if !bound_sd_hashes.contains(&d.hash) {
            errors.push(ViError::new(
                ViErrorKind::SdHashMismatch,
                format!(
                    "disclosure '{}' hash not bound in L2 _sd",
                    d.claim_name
                ),
            ));
        }
    }

    // Split resolved mandates by claim name (issuance uses the same claim
    // names for both modes; the mode is inferred from the VCT values).
    let mut checkout_mandate: Option<serde_json::Value> = None;
    let mut payment_mandate: Option<serde_json::Value> = None;
    for d in &resolved {
        match d.claim_name.as_str() {
            "checkout_mandate" => checkout_mandate = Some(d.claim_value.clone()),
            "payment_mandate" => payment_mandate = Some(d.claim_value.clone()),
            _ => {}
        }
    }
    let (Some(checkout), Some(payment)) = (checkout_mandate, payment_mandate) else {
        return Err(vec![ViError::new(
            ViErrorKind::IncompleteMandatePair,
            "L2 must disclose both checkout and payment mandates",
        )]);
    };
    let checkout_vct = checkout
        .get("vct")
        .and_then(|v| v.as_str())
        .unwrap_or("")
        .to_string();
    let payment_vct = payment
        .get("vct")
        .and_then(|v| v.as_str())
        .unwrap_or("")
        .to_string();
    let mode = infer_mode_from_vct(&checkout_vct).map_err(|e| vec![e])?;
    let payment_mode = infer_mode_from_vct(&payment_vct).map_err(|e| vec![e])?;
    if mode != payment_mode {
        return Err(vec![ViError::new(
            ViErrorKind::ModeMismatch,
            "checkout and payment mandates disagree on execution mode",
        )]);
    }

    // ── Mode-specific mandate pairing ────────────────────────────────
    let (constraints, l2_cnf) = match mode {
        MandateMode::Autonomous => {
            let c_checkout: Cnf =
                serde_json::from_value(checkout.get("cnf").cloned().unwrap_or_default())
                    .map_err(|e| {
                        vec![ViError::new(
                            ViErrorKind::ModeMismatch,
                            format!("checkout mandate cnf parse: {e}"),
                        )]
                    })?;
            let c_payment: Cnf =
                serde_json::from_value(payment.get("cnf").cloned().unwrap_or_default()).map_err(
                    |e| {
                        vec![ViError::new(
                            ViErrorKind::ModeMismatch,
                            format!("payment mandate cnf parse: {e}"),
                        )]
                    },
                )?;
            if c_checkout != c_payment {
                return Err(vec![ViError::new(
                    ViErrorKind::ModeMismatch,
                    "checkout and payment mandates must bind the same agent key (cnf mismatch)",
                )]);
            }
            let constraints: Vec<Constraint> =
                serde_json::from_value(payment.get("constraints").cloned().unwrap_or_default())
                    .map_err(|e| {
                        vec![ViError::new(
                            ViErrorKind::InvalidPayload,
                            format!("payment constraints parse: {e}"),
                        )]
                    })?;
            (constraints, c_checkout)
        }
        MandateMode::Immediate => {
            // Immediate mode: no cnf in the mandates (per spec).
            if checkout.get("cnf").is_some() || payment.get("cnf").is_some() {
                return Err(vec![ViError::new(
                    ViErrorKind::ModeMismatch,
                    "Immediate mode mandates must not carry cnf",
                )]);
            }
            (Vec::new(), l1.cnf.clone())
        }
    };

    // ── L3 verification (Autonomous only) ────────────────────────────
    let mut l3a: Option<PaymentL3Mandate> = None;
    let mut l3b: Option<CheckoutL3Mandate> = None;
    match mode {
        MandateMode::Autonomous => {
            let (Some(l3a_str), Some(l3b_str)) = (req.serialized_l3a, req.serialized_l3b) else {
                return Err(vec![ViError::new(
                    ViErrorKind::IncompleteMandatePair,
                    "Autonomous mode requires L3a and L3b",
                )]);
            };
            // L3a: agent-signed payment values. The serialized form is
            // `jwt~` (no disclosures, no KB-JWT), so parse out the JWS
            // before decoding/verifying — the trailing `~` is not part of
            // the compact JWS.
            let (l3a_jws, _, _) = parse_sd_jwt(l3a_str).map_err(|e| vec![e])?;
            let l3a_payload = jws_decode_payload(l3a_jws).map_err(|e| vec![e])?;
            let l3a_sd_hash = l3a_payload
                .get("sd_hash")
                .and_then(|v| v.as_str())
                .unwrap_or("");
            verify_sd_hash_binding(l3a_sd_hash, req.serialized_l2).map_err(|e| vec![e])?;
            let l3a_mandate: PaymentL3Mandate =
                serde_json::from_value(l3a_payload.get("mandate").cloned().unwrap_or_default())
                    .map_err(|e| {
                        vec![ViError::new(
                            ViErrorKind::InvalidPayload,
                            format!("L3a mandate parse: {e}"),
                        )]
                    })?;
            let l3a_iat = l3a_payload.get("iat").and_then(|v| v.as_i64()).unwrap_or(0);
            let l3a_exp = l3a_payload.get("exp").and_then(|v| v.as_i64()).unwrap_or(0);
            verify_timestamps(l3a_iat, l3a_exp).map_err(|e| vec![e])?;
            // L3a signature against the agent key from the L2 Autonomous cnf.
            let agent_pk = jwk_to_public_bytes(&l2_cnf.jwk).map_err(|e| vec![e])?;
            jws_verify(l3a_jws, &agent_pk).map_err(|e| vec![e])?;

            // L3b: agent-signed checkout values.
            let (l3b_jws, _, _) = parse_sd_jwt(l3b_str).map_err(|e| vec![e])?;
            let l3b_payload = jws_decode_payload(l3b_jws).map_err(|e| vec![e])?;
            let l3b_sd_hash = l3b_payload
                .get("sd_hash")
                .and_then(|v| v.as_str())
                .unwrap_or("");
            verify_sd_hash_binding(l3b_sd_hash, req.serialized_l2).map_err(|e| vec![e])?;
            let l3b_mandate: CheckoutL3Mandate =
                serde_json::from_value(l3b_payload.get("mandate").cloned().unwrap_or_default())
                    .map_err(|e| {
                        vec![ViError::new(
                            ViErrorKind::InvalidPayload,
                            format!("L3b mandate parse: {e}"),
                        )]
                    })?;
            let l3b_iat = l3b_payload.get("iat").and_then(|v| v.as_i64()).unwrap_or(0);
            let l3b_exp = l3b_payload.get("exp").and_then(|v| v.as_i64()).unwrap_or(0);
            verify_timestamps(l3b_iat, l3b_exp).map_err(|e| vec![e])?;
            jws_verify(l3b_jws, &agent_pk).map_err(|e| vec![e])?;

            // Cross-reference: L3a transaction_id ↔ L3b checkout_hash.
            verify_l3_cross_reference(&l3a_mandate, &l3b_mandate).map_err(|e| vec![e])?;
            verify_checkout_hash_binding(&l3b_mandate.checkout_hash, &l3b_mandate.checkout_jwt)
                .map_err(|e| vec![e])?;
            l3a = Some(l3a_mandate);
            l3b = Some(l3b_mandate);
        }
        MandateMode::Immediate => {
            if req.serialized_l3a.is_some() || req.serialized_l3b.is_some() {
                errors.push(ViError::new(
                    ViErrorKind::ModeMismatch,
                    "Immediate mode does not use L3 mandates",
                ));
            }
        }
    }

    if !errors.is_empty() {
        return Err(errors);
    }

    // ── Fulfillment derived from verified L3 (or final L2) mandates ──
    let fulfillment = build_fulfillment(mode, l3a.as_ref(), l3b.as_ref(), &resolved);

    // Fail-closed guard (issue #9327): allowlist constraints must have a
    // disclosed subject. An absent subject can never satisfy an allowlist.
    if strictness == StrictnessMode::Strict {
        for c in &constraints {
            match c {
                Constraint::AllowedMerchant { .. } if fulfillment.merchant.is_none() => {
                    return Err(vec![ViError::new(
                        ViErrorKind::MerchantNotAllowed,
                        "strict mode: allowed_merchant constraint has no disclosed merchant",
                    )]);
                }
                Constraint::AllowedPayee { .. } if fulfillment.payee.is_none() => {
                    return Err(vec![ViError::new(
                        ViErrorKind::PayeeNotAllowed,
                        "strict mode: allowed_payee constraint has no disclosed payee",
                    )]);
                }
                _ => {}
            }
        }
    }

    // Capture the L1 subject key binding before `l1` is moved into the chain.
    let l1_cnf = l1.cnf.clone();

    let chain = CredentialChain {
        l1,
        l2: crate::verifiable_intent::types::Layer2 {
            nonce: l2_nonce,
            aud: l2_aud,
            iat: l2_iat,
            exp: l2_exp,
            sd_hash: l2_sd_hash,
            mode,
            mandates: resolved.iter().map(|d| d.claim_value.clone()).collect(),
        },
        l3a,
        l3b,
    };

    // Sanity: constraints must evaluate against the derived fulfillment.
    let results = check_constraints(&constraints, &fulfillment, strictness);
    if results.iter().any(|r| !r.satisfied) {
        return Err(results
            .iter()
            .filter(|r| !r.satisfied)
            .flat_map(|r| r.violations.clone())
            .collect());
    }

    Ok(VerifiedChain {
        chain,
        mode,
        constraints,
        fulfillment,
        l1_cnf,
    })
}

/// Resolve SD-JWT disclosures into (claim_name, claim_value, hash) triples.
fn resolve_disclosures(raw: &[&str]) -> Result<Vec<Disclosure>, ViError> {
    let mut out = Vec::new();
    for d in raw {
        if d.is_empty() {
            continue;
        }
        let bytes = b64u_decode(d)?;
        let arr: Vec<serde_json::Value> =
            serde_json::from_slice(&bytes).map_err(|e| {
                ViError::new(
                    ViErrorKind::InvalidDisclosure,
                    format!("disclosure JSON: {e}"),
                )
            })?;
        if arr.len() != 3 {
            return Err(ViError::new(
                ViErrorKind::InvalidDisclosure,
                "disclosure must be [salt, claim_name, claim_value]",
            ));
        }
        let claim_name = arr[1].as_str().unwrap_or("").to_string();
        out.push(Disclosure {
            claim_name,
            claim_value: arr[2].clone(),
            hash: sd_hash(d),
        });
    }
    Ok(out)
}

/// Build the fulfillment from verified L3 mandates (Autonomous) or from the
/// final L2 mandates (Immediate).
fn build_fulfillment(
    mode: MandateMode,
    l3a: Option<&PaymentL3Mandate>,
    l3b: Option<&CheckoutL3Mandate>,
    resolved: &[Disclosure],
) -> Fulfillment {
    match mode {
        MandateMode::Autonomous => {
            let mut f = Fulfillment::default();
            if let Some(a) = l3a {
                f.payee = Some(a.payee.clone());
                f.payment_instrument = Some(a.payment_instrument.clone());
                f.currency = Some(a.payment_amount.currency.clone());
                f.amount = Some(a.payment_amount.amount);
            }
            if let Some(b) = l3b {
                f.line_items = b.line_items.clone();
            }
            f
        }
        MandateMode::Immediate => {
            let mut f = Fulfillment::default();
            for d in resolved {
                if d.claim_name == "payment_mandate" {
                    let payee: Option<Entity> =
                        serde_json::from_value(d.claim_value.get("payee").cloned().unwrap_or_default())
                            .ok();
                    let instrument: Option<PaymentInstrument> = serde_json::from_value(
                        d.claim_value
                            .get("payment_instrument")
                            .cloned()
                            .unwrap_or_default(),
                    )
                    .ok();
                    let amount = d.claim_value.get("amount").and_then(|v| v.as_i64());
                    let currency = d
                        .claim_value
                        .get("currency")
                        .and_then(|v| v.as_str())
                        .map(String::from);
                    f.payee = payee;
                    f.payment_instrument = instrument;
                    f.currency = currency;
                    f.amount = amount;
                }
                if d.claim_name == "checkout_mandate" {
                    let line_items: Option<Vec<FulfillmentLineItem>> = serde_json::from_value(
                        d.claim_value
                            .get("line_items")
                            .cloned()
                            .unwrap_or_default(),
                    )
                    .ok();
                    f.line_items = line_items;
                }
            }
            f
        }
    }
}
