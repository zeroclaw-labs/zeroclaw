//! SD-JWT / KB-SD-JWT cryptographic primitives.

use std::collections::{HashMap, HashSet};
use std::fmt;

use base64::Engine;
use base64::engine::general_purpose::URL_SAFE_NO_PAD;
use ring::rand::SystemRandom;
use ring::signature::{self, ECDSA_P256_SHA256_FIXED_SIGNING, EcdsaKeyPair, KeyPair};
use serde::de::{self, MapAccess, SeqAccess, Visitor};
use sha2::{Digest, Sha256};

use crate::verifiable_intent::error::{ViError, ViErrorKind};
use crate::verifiable_intent::types::Jwk;

// ── Base64url helpers ────────────────────────────────────────────────

/// Encode bytes as base64url without padding.
pub fn b64u_encode(data: &[u8]) -> String {
    URL_SAFE_NO_PAD.encode(data)
}

/// Decode base64url without padding.
pub fn b64u_decode(s: &str) -> Result<Vec<u8>, ViError> {
    URL_SAFE_NO_PAD.decode(s).map_err(|e| {
        ViError::new(
            ViErrorKind::InvalidPayload,
            format!("base64url decode: {e}"),
        )
    })
}

// ── Strict JSON ──────────────────────────────────────────────────────

/// A JSON value that refuses duplicate object members, at any depth.
///
/// `serde_json::Value` keeps the last of a repeated key, so a signed object
/// carrying two `aud` claims parses cleanly and two verifiers reading the same
/// bytes can disagree about which one they checked. The security model requires
/// refusing that rather than picking a winner, and the ambiguity has to be
/// caught here: once the value exists, the evidence that it was ambiguous is
/// gone.
struct StrictJson(serde_json::Value);

impl<'de> serde::Deserialize<'de> for StrictJson {
    fn deserialize<D: serde::Deserializer<'de>>(deserializer: D) -> Result<Self, D::Error> {
        struct StrictVisitor;

        impl<'de> Visitor<'de> for StrictVisitor {
            type Value = StrictJson;

            fn expecting(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
                f.write_str("JSON with no duplicate object members")
            }

            fn visit_unit<E: de::Error>(self) -> Result<Self::Value, E> {
                Ok(StrictJson(serde_json::Value::Null))
            }
            fn visit_none<E: de::Error>(self) -> Result<Self::Value, E> {
                Ok(StrictJson(serde_json::Value::Null))
            }
            fn visit_bool<E: de::Error>(self, value: bool) -> Result<Self::Value, E> {
                Ok(StrictJson(serde_json::Value::Bool(value)))
            }
            fn visit_i64<E: de::Error>(self, value: i64) -> Result<Self::Value, E> {
                Ok(StrictJson(serde_json::Value::from(value)))
            }
            fn visit_u64<E: de::Error>(self, value: u64) -> Result<Self::Value, E> {
                Ok(StrictJson(serde_json::Value::from(value)))
            }
            fn visit_f64<E: de::Error>(self, value: f64) -> Result<Self::Value, E> {
                Ok(StrictJson(serde_json::Value::from(value)))
            }
            fn visit_str<E: de::Error>(self, value: &str) -> Result<Self::Value, E> {
                Ok(StrictJson(serde_json::Value::String(value.to_owned())))
            }

            fn visit_seq<A: SeqAccess<'de>>(self, mut seq: A) -> Result<Self::Value, A::Error> {
                let mut items = Vec::new();
                while let Some(StrictJson(item)) = seq.next_element()? {
                    items.push(item);
                }
                Ok(StrictJson(serde_json::Value::Array(items)))
            }

            fn visit_map<A: MapAccess<'de>>(self, mut map: A) -> Result<Self::Value, A::Error> {
                let mut object = serde_json::Map::new();
                while let Some(key) = map.next_key::<String>()? {
                    let StrictJson(value) = map.next_value()?;
                    if object.contains_key(&key) {
                        return Err(de::Error::custom(format!("duplicate member `{key}`")));
                    }
                    object.insert(key, value);
                }
                Ok(StrictJson(serde_json::Value::Object(object)))
            }
        }

        deserializer.deserialize_any(StrictVisitor)
    }
}

/// Parse JSON, refusing duplicate object members at any depth.
fn parse_json_strict(bytes: &[u8]) -> Result<serde_json::Value, serde_json::Error> {
    serde_json::from_slice::<StrictJson>(bytes).map(|StrictJson(value)| value)
}

// ── Hashing ──────────────────────────────────────────────────────────

/// Compute `B64U(SHA-256(ASCII(input)))` — used for `sd_hash`, `checkout_hash`,
/// `transaction_id`, disclosure hashes, and `conditional_transaction_id`.
pub fn sd_hash(input: &str) -> String {
    let digest = Sha256::digest(input.as_bytes());
    b64u_encode(&digest)
}

/// Compute raw SHA-256 hash of a byte slice.
pub fn sha256(data: &[u8]) -> Vec<u8> {
    Sha256::digest(data).to_vec()
}

// ── JWS / ES256 signing ─────────────────────────────────────────────

/// Sign a JWS (compact serialization) over the given header and payload JSON.
/// Returns the full `header.payload.signature` string.
pub fn jws_sign(
    header_json: &[u8],
    payload_json: &[u8],
    key_pair: &EcdsaKeyPair,
) -> Result<String, ViError> {
    let header_b64 = b64u_encode(header_json);
    let payload_b64 = b64u_encode(payload_json);
    let signing_input = format!("{header_b64}.{payload_b64}");

    let rng = SystemRandom::new();
    let sig = key_pair.sign(&rng, signing_input.as_bytes()).map_err(|e| {
        ViError::new(
            ViErrorKind::SignatureInvalid,
            format!("signing failed: {e}"),
        )
    })?;

    let sig_b64 = b64u_encode(sig.as_ref());
    Ok(format!("{signing_input}.{sig_b64}"))
}

/// Split a JWS compact serialization into its header, payload and signature.
///
/// The pinned reference refuses anything that is not exactly three
/// dot-separated segments (`signing.py::_jwt_decode_parts`). Validating once
/// here keeps every entry point equally strict; the previous per-function
/// `splitn(3, '.')` accepted a fourth segment by folding it into the signature.
fn jws_parts(compact: &str) -> Result<[&str; 3], ViError> {
    let mut segments = compact.split('.');
    let (Some(header), Some(payload), Some(signature), None) = (
        segments.next(),
        segments.next(),
        segments.next(),
        segments.next(),
    ) else {
        return Err(ViError::new(
            ViErrorKind::InvalidHeader,
            "JWS must have exactly 3 dot-separated parts",
        ));
    };
    Ok([header, payload, signature])
}

/// Verify an ES256 JWS compact-serialization string against a public key.
pub fn jws_verify(compact: &str, public_key_bytes: &[u8]) -> Result<(), ViError> {
    let [header, payload, signature] = jws_parts(compact)?;

    let signing_input = format!("{header}.{payload}");
    let sig_bytes = b64u_decode(signature)?;

    let peer_public_key =
        signature::UnparsedPublicKey::new(&signature::ECDSA_P256_SHA256_FIXED, public_key_bytes);

    peer_public_key
        .verify(signing_input.as_bytes(), &sig_bytes)
        .map_err(|_| {
            ViError::new(
                ViErrorKind::SignatureInvalid,
                "ES256 signature verification failed",
            )
        })
}

/// Decode the payload segment of a JWS compact string (the middle part).
pub fn jws_decode_payload(compact: &str) -> Result<serde_json::Value, ViError> {
    let [_, payload, _] = jws_parts(compact)?;
    let bytes = b64u_decode(payload)?;
    parse_json_strict(&bytes)
        .map_err(|e| ViError::new(ViErrorKind::InvalidPayload, format!("payload JSON: {e}")))
}

/// Decode the header segment of a JWS compact string (the first part).
pub fn jws_decode_header(compact: &str) -> Result<serde_json::Value, ViError> {
    let [header, _, _] = jws_parts(compact)?;
    let bytes = b64u_decode(header)?;
    parse_json_strict(&bytes)
        .map_err(|e| ViError::new(ViErrorKind::InvalidHeader, format!("header JSON: {e}")))
}

// ── EC P-256 key utilities ──────────────────────────────────────────

/// Generate a fresh EC P-256 key pair.  Returns (pkcs8_document, Jwk_public).
pub fn generate_ec_p256() -> Result<(Vec<u8>, Jwk), ViError> {
    let rng = SystemRandom::new();
    let pkcs8 = EcdsaKeyPair::generate_pkcs8(&ECDSA_P256_SHA256_FIXED_SIGNING, &rng)
        .map_err(|e| ViError::new(ViErrorKind::KeyUnsupported, format!("keygen: {e}")))?;

    let key_pair = EcdsaKeyPair::from_pkcs8(&ECDSA_P256_SHA256_FIXED_SIGNING, pkcs8.as_ref(), &rng)
        .map_err(|e| ViError::new(ViErrorKind::KeyUnsupported, format!("parse pkcs8: {e}")))?;

    let pub_bytes = key_pair.public_key().as_ref();
    let jwk = ec_public_bytes_to_jwk(pub_bytes)?;

    Ok((pkcs8.as_ref().to_vec(), jwk))
}

/// Load an `EcdsaKeyPair` from PKCS#8 DER bytes.
pub fn load_key_pair(pkcs8_der: &[u8]) -> Result<EcdsaKeyPair, ViError> {
    let rng = SystemRandom::new();
    EcdsaKeyPair::from_pkcs8(&ECDSA_P256_SHA256_FIXED_SIGNING, pkcs8_der, &rng)
        .map_err(|e| ViError::new(ViErrorKind::KeyUnsupported, format!("load pkcs8: {e}")))
}

/// Convert the raw uncompressed public key bytes (65 bytes: 0x04 || x || y)
/// into a [`Jwk`].
pub fn ec_public_bytes_to_jwk(pub_bytes: &[u8]) -> Result<Jwk, ViError> {
    if pub_bytes.len() != 65 || pub_bytes[0] != 0x04 {
        return Err(ViError::new(
            ViErrorKind::KeyUnsupported,
            "expected 65-byte uncompressed EC point (0x04 || x || y)",
        ));
    }
    Ok(Jwk {
        kty: "EC".into(),
        crv: "P-256".into(),
        x: b64u_encode(&pub_bytes[1..33]),
        y: b64u_encode(&pub_bytes[33..65]),
        d: None,
    })
}

/// Convert a [`Jwk`] (public) back to raw uncompressed bytes (65 bytes).
pub fn jwk_to_public_bytes(jwk: &Jwk) -> Result<Vec<u8>, ViError> {
    if jwk.kty != "EC" || jwk.crv != "P-256" {
        return Err(ViError::new(
            ViErrorKind::KeyUnsupported,
            format!("unsupported key type: {}:{}", jwk.kty, jwk.crv),
        ));
    }
    let x = b64u_decode(&jwk.x)?;
    let y = b64u_decode(&jwk.y)?;
    if x.len() != 32 || y.len() != 32 {
        return Err(ViError::new(
            ViErrorKind::KeyUnsupported,
            "x/y coordinates must be 32 bytes each",
        ));
    }
    let mut bytes = Vec::with_capacity(65);
    bytes.push(0x04);
    bytes.extend_from_slice(&x);
    bytes.extend_from_slice(&y);
    Ok(bytes)
}

// ── SD-JWT disclosure helpers ────────────────────────────────────────

/// The decoded contents of an SD-JWT disclosure.
///
/// Only two shapes exist, and both resolve to their array's final element —
/// which is what makes the spec's §9.1 `delegate_payload` references work
/// uniformly across them.
#[derive(Debug, Clone, PartialEq, Eq)]
pub enum Disclosure {
    /// `[salt, claim_name, claim_value]` — discloses an object property.
    ObjectProperty {
        salt: String,
        claim_name: String,
        claim_value: serde_json::Value,
    },
    /// `[salt, claim_value]` — discloses an array element.
    ArrayElement {
        salt: String,
        claim_value: serde_json::Value,
    },
}

impl Disclosure {
    /// The salt that makes the disclosure unguessable.
    pub fn salt(&self) -> &str {
        match self {
            Self::ObjectProperty { salt, .. } | Self::ArrayElement { salt, .. } => salt,
        }
    }

    /// The disclosed property name, absent for array-element disclosures.
    pub fn claim_name(&self) -> Option<&str> {
        match self {
            Self::ObjectProperty { claim_name, .. } => Some(claim_name),
            Self::ArrayElement { .. } => None,
        }
    }

    /// The disclosed value — the array's last element in either shape.
    pub fn claim_value(&self) -> &serde_json::Value {
        match self {
            Self::ObjectProperty { claim_value, .. } | Self::ArrayElement { claim_value, .. } => {
                claim_value
            }
        }
    }
}

/// Generate the 16-byte salt the spec requires, base64url-encoded.
fn generate_salt() -> Result<String, ViError> {
    let rng = SystemRandom::new();
    let mut salt_bytes = [0u8; 16];
    ring::rand::SecureRandom::fill(&rng, &mut salt_bytes)
        .map_err(|e| ViError::new(ViErrorKind::IssuanceInputInvalid, format!("rng: {e}")))?;
    Ok(b64u_encode(&salt_bytes))
}

/// Create a disclosure with a caller-supplied salt.
///
/// `claim_name` selects the shape: `Some` produces the object-property form,
/// `None` the array-element form. Supplying the salt is what makes output
/// comparable against the reference implementation's pinned test vectors.
///
/// Returns `(disclosure_b64, disclosure_hash)`.
pub fn create_disclosure_with_salt(
    claim_name: Option<&str>,
    claim_value: &serde_json::Value,
    salt: &str,
) -> Result<(String, String), ViError> {
    let disclosure_json = match claim_name {
        Some(name) => serde_json::json!([salt, name, claim_value]),
        None => serde_json::json!([salt, claim_value]),
    };
    let disclosure_str = serde_json::to_string(&disclosure_json).map_err(|e| {
        ViError::new(
            ViErrorKind::IssuanceInputInvalid,
            format!("disclosure JSON: {e}"),
        )
    })?;
    let disclosure_b64 = b64u_encode(disclosure_str.as_bytes());
    let hash = sd_hash(&disclosure_b64);
    Ok((disclosure_b64, hash))
}

/// Create a single SD-JWT disclosure: `[salt, claim_name, claim_value]`.
/// Returns `(disclosure_b64, disclosure_hash)`.
pub fn create_disclosure(
    claim_name: &str,
    claim_value: &serde_json::Value,
) -> Result<(String, String), ViError> {
    create_disclosure_with_salt(Some(claim_name), claim_value, &generate_salt()?)
}

/// Create an array-element disclosure: `[salt, claim_value]`.
///
/// Used for entries that are individually disclosable rather than named
/// properties — `delegate_payload` members and, per §9.2, the entries inside
/// `allowed` and `line_items` constraints.
pub fn create_array_element_disclosure(
    claim_value: &serde_json::Value,
) -> Result<(String, String), ViError> {
    create_disclosure_with_salt(None, claim_value, &generate_salt()?)
}

/// Decode a disclosure into its typed form.
///
/// Stricter than the reference, which returns whatever JSON the disclosure
/// happens to hold: anything that is not a 2- or 3-element array with a string
/// salt is refused here rather than reaching a caller obliged to re-check it.
///
/// The bytes are parsed with the same duplicate-refusing reader the signed
/// header and payload use. A disclosure is bound by a digest the issuer signed,
/// and resolution merges its value into the claim set, so a repeated member
/// inside one is a repeated claim name in the payload a verifier acts on.
/// Reading it with a parser that silently keeps one of the two would leave the
/// ambiguity undetectable by the time anything could act on it.
pub fn decode_disclosure(disclosure_b64: &str) -> Result<Disclosure, ViError> {
    let bytes = b64u_decode(disclosure_b64)?;
    let decoded = parse_json_strict(&bytes).map_err(|e| {
        ViError::new(
            ViErrorKind::InvalidDisclosure,
            format!("disclosure JSON: {e}"),
        )
    })?;

    let serde_json::Value::Array(elements) = decoded else {
        return Err(ViError::new(
            ViErrorKind::InvalidDisclosure,
            "disclosure must be a JSON array",
        ));
    };

    let mut elements = elements.into_iter();
    let (Some(salt), Some(second)) = (elements.next(), elements.next()) else {
        return Err(ViError::new(
            ViErrorKind::InvalidDisclosure,
            "disclosure must have 2 or 3 elements",
        ));
    };
    let third = elements.next();
    if elements.next().is_some() {
        return Err(ViError::new(
            ViErrorKind::InvalidDisclosure,
            "disclosure must have 2 or 3 elements",
        ));
    }

    let serde_json::Value::String(salt) = salt else {
        return Err(ViError::new(
            ViErrorKind::InvalidDisclosure,
            "disclosure salt must be a string",
        ));
    };

    match third {
        Some(claim_value) => {
            let serde_json::Value::String(claim_name) = second else {
                return Err(ViError::new(
                    ViErrorKind::InvalidDisclosure,
                    "object-property disclosure name must be a string",
                ));
            };
            Ok(Disclosure::ObjectProperty {
                salt,
                claim_name,
                claim_value,
            })
        }
        None => Ok(Disclosure::ArrayElement {
            salt,
            claim_value: second,
        }),
    }
}

/// Serialize an SD-JWT: `issuer_jwt~disclosure1~disclosure2~...~kb_jwt`
/// (omit `kb_jwt` for L1 which has no key-binding JWT).
pub fn serialize_sd_jwt(issuer_jwt: &str, disclosures: &[String], kb_jwt: Option<&str>) -> String {
    let mut result = issuer_jwt.to_string();
    for d in disclosures {
        result.push('~');
        result.push_str(d);
    }
    result.push('~');
    if let Some(kb) = kb_jwt {
        result.push_str(kb);
    }
    result
}

/// Parse a serialized SD-JWT into (issuer_jwt, disclosures, optional_kb_jwt).
///
/// The two forms are distinguished by the final `~`: a presentation without key
/// binding keeps it, and one with key binding ends in the key-binding JWT. That
/// distinction is the only thing separating `issuer~disclosure` from
/// `issuer~disclosure~`, and reading the first as the second silently drops the
/// last disclosure while inventing a key binding. So the final component is
/// required to look like a JWT before it is treated as one.
///
/// Structure only. Nothing here checks a key-binding JWT's audience, nonce,
/// `sd_hash` or signature; those are chain-verification concerns above this
/// layer.
pub fn parse_sd_jwt(serialized: &str) -> Result<(&str, Vec<&str>, Option<&str>), ViError> {
    let parts: Vec<&str> = serialized.split('~').collect();
    if parts.len() < 2 {
        return Err(ViError::new(
            ViErrorKind::InvalidDisclosure,
            "SD-JWT must have at least issuer JWT and trailing ~",
        ));
    }
    let issuer_jwt = parts[0];
    let last = *parts.last().unwrap();

    let kb_jwt = if last.is_empty() {
        None
    } else {
        jws_decode_header(last).and_then(|header| {
            if header.is_object() {
                Ok(())
            } else {
                Err(ViError::new(
                    ViErrorKind::InvalidHeader,
                    "key-binding JWT header must be a JSON object",
                ))
            }
        })?;
        jws_decode_payload(last).and_then(|payload| {
            if payload.is_object() {
                Ok(())
            } else {
                Err(ViError::new(
                    ViErrorKind::InvalidPayload,
                    "key-binding JWT payload must be a JSON object",
                ))
            }
        })?;
        Some(last)
    };

    let disclosures = parts[1..parts.len() - 1].to_vec();

    // An empty segment is not a disclosure. It appears when a serialized SD-JWT
    // that already ends in `~` is joined as though it were a bare JWT, which
    // produces `jwt~~disclosure` and would otherwise be carried down to the
    // disclosure decoder as an empty string.
    if disclosures.iter().any(|disclosure| disclosure.is_empty()) {
        return Err(ViError::new(
            ViErrorKind::InvalidDisclosure,
            "SD-JWT contains an empty disclosure segment",
        ));
    }

    Ok((issuer_jwt, disclosures, kb_jwt))
}

// ── Parsed SD-JWT ────────────────────────────────────────────────────

/// A parsed SD-JWT presentation.
///
/// Keeps the exact strings it was parsed from alongside the decoded values.
/// Spec §6.1 binds each layer to "the serialized form as received", so
/// re-encoding from the decoded header and payload would not reproduce the
/// bytes a binding covers.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct SdJwt {
    issuer_jwt: String,
    header: serde_json::Value,
    payload: serde_json::Value,
    signature: Vec<u8>,
    disclosures: Vec<String>,
    disclosure_values: Vec<Disclosure>,
    key_binding_jwt: Option<String>,
}

impl SdJwt {
    /// Parse a serialized SD-JWT, decoding the JWT segments and every disclosure.
    pub fn parse(serialized: &str) -> Result<Self, ViError> {
        let (issuer_jwt, disclosures, key_binding_jwt) = parse_sd_jwt(serialized)?;
        let [_, _, signature_b64] = jws_parts(issuer_jwt)?;

        let disclosure_values = disclosures
            .iter()
            .map(|disclosure| decode_disclosure(disclosure))
            .collect::<Result<Vec<_>, _>>()?;

        Ok(Self {
            issuer_jwt: issuer_jwt.to_string(),
            header: jws_decode_header(issuer_jwt)?,
            payload: jws_decode_payload(issuer_jwt)?,
            signature: b64u_decode(signature_b64)?,
            disclosures: disclosures.into_iter().map(str::to_string).collect(),
            disclosure_values,
            key_binding_jwt: key_binding_jwt.map(str::to_string),
        })
    }

    /// The issuer-signed JWT, exactly as received.
    pub fn issuer_jwt(&self) -> &str {
        &self.issuer_jwt
    }

    /// The decoded JWT header.
    pub fn header(&self) -> &serde_json::Value {
        &self.header
    }

    /// The decoded JWT payload, before any disclosure is resolved into it.
    pub fn payload(&self) -> &serde_json::Value {
        &self.payload
    }

    /// The raw ES256 signature bytes.
    pub fn signature(&self) -> &[u8] {
        &self.signature
    }

    /// The presented disclosures, in the order they were received.
    pub fn disclosures(&self) -> &[String] {
        &self.disclosures
    }

    /// The decoded disclosures, positionally matching [`Self::disclosures`].
    pub fn disclosure_values(&self) -> &[Disclosure] {
        &self.disclosure_values
    }

    /// The key-binding JWT, when the presentation carried one.
    pub fn key_binding_jwt(&self) -> Option<&str> {
        self.key_binding_jwt.as_deref()
    }

    /// Re-serialize exactly what was parsed, key-binding JWT included.
    pub fn serialize(&self) -> String {
        serialize_sd_jwt(
            &self.issuer_jwt,
            &self.disclosures,
            self.key_binding_jwt.as_deref(),
        )
    }

    /// The presentation a hash binding covers: every disclosure, a trailing
    /// `~`, and no key-binding JWT.
    ///
    /// §6.1 computes `sd_hash` over the SD-JWT excluding the KB-JWT, so this
    /// is a named operation rather than a convention a caller has to remember.
    pub fn presentation(&self) -> String {
        serialize_sd_jwt(&self.issuer_jwt, &self.disclosures, None)
    }

    /// The presentation containing only the disclosures at `indices`.
    ///
    /// §5.4 and §6.1.2: an L3 binds to the L2 subset forwarded to its own
    /// recipient, not to everything the agent holds.
    pub fn selective_presentation(&self, indices: &[usize]) -> Result<String, ViError> {
        let mut selected = Vec::with_capacity(indices.len());
        for &index in indices {
            let disclosure = self.disclosures.get(index).ok_or_else(|| {
                ViError::new(
                    ViErrorKind::InvalidDisclosure,
                    format!("disclosure index {index} is out of range"),
                )
            })?;
            selected.push(disclosure.clone());
        }
        Ok(serialize_sd_jwt(&self.issuer_jwt, &selected, None))
    }

    /// Resolve the presented disclosures into the payload's claim set.
    ///
    /// Two separate rules, matching the reference:
    ///
    /// - Object-property disclosures resolve only when their hash appears in
    ///   `_sd`. An array-element disclosure listed there resolves to nothing,
    ///   because it has no name to bind to.
    /// - `delegate_payload` references resolve against every presented
    ///   disclosure, ungated by `_sd`. A reference whose disclosure is absent
    ///   stays a `{"...": "<hash>"}` object so the caller can tell resolved
    ///   from unresolved.
    ///
    /// Nested references inside constraint objects are deliberately left
    /// alone; the reference does not recurse into them either.
    pub fn resolve_disclosures(
        &self,
    ) -> Result<serde_json::Map<String, serde_json::Value>, ViError> {
        let serde_json::Value::Object(mut claims) = self.payload.clone() else {
            return Err(ViError::new(
                ViErrorKind::InvalidPayload,
                "SD-JWT payload must be a JSON object",
            ));
        };

        // A digest listed twice makes the graph ambiguous about which disclosure
        // satisfies it, so it is refused rather than deduplicated. The scope is
        // deliberately the `_sd` array alone: the credential format's own worked
        // example carries a mandate's digest in both `_sd` and
        // `delegate_payload`, so rejecting repeats across the whole payload
        // would reject the shape the specification documents.
        let mut sd_hashes: HashSet<String> = HashSet::new();
        if let Some(serde_json::Value::Array(entries)) = claims.get("_sd") {
            for digest in entries.iter().filter_map(serde_json::Value::as_str) {
                if !sd_hashes.insert(digest.to_owned()) {
                    return Err(ViError::new(
                        ViErrorKind::InvalidDisclosure,
                        format!("digest {digest} is listed more than once in `_sd`"),
                    ));
                }
            }
        }

        for (encoded, decoded) in self.disclosures.iter().zip(&self.disclosure_values) {
            if let Disclosure::ObjectProperty {
                claim_name,
                claim_value,
                ..
            } = decoded
                && sd_hashes.contains(&sd_hash(encoded))
            {
                // A disclosure may not name a structural member, and may not
                // redefine a claim the issuer already signed in the clear.
                // Either would let a selectively disclosed value overwrite what
                // the verifier believes it read from the signed payload.
                if claim_name == "_sd" || claim_name == "..." {
                    return Err(ViError::new(
                        ViErrorKind::InvalidDisclosure,
                        format!("a disclosure may not be named `{claim_name}`"),
                    ));
                }
                if claims.contains_key(claim_name) {
                    return Err(ViError::new(
                        ViErrorKind::InvalidDisclosure,
                        format!("disclosure would redefine the existing claim `{claim_name}`"),
                    ));
                }
                claims.insert(claim_name.clone(), claim_value.clone());
            }
        }

        let Some(serde_json::Value::Array(entries)) = claims.get("delegate_payload") else {
            return Ok(claims);
        };
        if entries.is_empty() {
            return Ok(claims);
        }

        let by_hash: HashMap<String, &serde_json::Value> = self
            .disclosures
            .iter()
            .zip(&self.disclosure_values)
            .map(|(encoded, decoded)| (sd_hash(encoded), decoded.claim_value()))
            .collect();

        let mut resolved: Vec<serde_json::Value> = Vec::with_capacity(entries.len());
        for entry in entries {
            // A placeholder carries `...` and nothing else. Recognising one by
            // membership instead would replace `{"...": h, "id": "x"}` wholesale
            // and discard the sibling, which is silent data loss exactly where
            // the caller believes it received a disclosed value.
            let Some(object) = entry.as_object() else {
                resolved.push(entry.clone());
                continue;
            };
            if !object.contains_key("...") {
                resolved.push(entry.clone());
                continue;
            }
            if object.len() != 1 {
                return Err(ViError::new(
                    ViErrorKind::InvalidDisclosure,
                    "a disclosure reference must carry `...` as its only member",
                ));
            }
            let Some(serde_json::Value::String(reference)) = object.get("...") else {
                return Err(ViError::new(
                    ViErrorKind::InvalidDisclosure,
                    "a disclosure reference hash must be a string",
                ));
            };

            // An unresolved reference stays in place rather than being removed.
            // RFC 9901 drops an array element whose digest has no disclosure;
            // here the surviving reference is what lets a verifier see that
            // undisclosed mandates exist, which is the defence the security
            // model builds on `delegate_payload`.
            match by_hash.get(reference.as_str()) {
                Some(value) => resolved.push((*value).clone()),
                None => resolved.push(entry.clone()),
            }
        }
        claims.insert(
            "delegate_payload".to_string(),
            serde_json::Value::Array(resolved),
        );

        Ok(claims)
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn sd_hash_deterministic() {
        let h1 = sd_hash("hello");
        let h2 = sd_hash("hello");
        assert_eq!(h1, h2);
        assert!(!h1.is_empty());
    }

    #[test]
    fn b64u_roundtrip() {
        let data = b"test data";
        let encoded = b64u_encode(data);
        let decoded = b64u_decode(&encoded).unwrap();
        assert_eq!(decoded, data);
    }

    #[test]
    fn generate_key_and_convert_roundtrip() {
        let (_pkcs8, jwk) = generate_ec_p256().unwrap();
        assert_eq!(jwk.kty, "EC");
        assert_eq!(jwk.crv, "P-256");
        assert!(jwk.d.is_none());
        let bytes = jwk_to_public_bytes(&jwk).unwrap();
        assert_eq!(bytes.len(), 65);
        assert_eq!(bytes[0], 0x04);
        let jwk2 = ec_public_bytes_to_jwk(&bytes).unwrap();
        assert_eq!(jwk, jwk2);
    }

    #[test]
    fn jws_sign_and_verify() {
        let (pkcs8, jwk) = generate_ec_p256().unwrap();
        let key_pair = load_key_pair(&pkcs8).unwrap();
        let header = serde_json::json!({"alg": "ES256", "typ": "sd+jwt"});
        let payload = serde_json::json!({"sub": "test"});
        let compact = jws_sign(
            header.to_string().as_bytes(),
            payload.to_string().as_bytes(),
            &key_pair,
        )
        .unwrap();

        let pub_bytes = jwk_to_public_bytes(&jwk).unwrap();
        jws_verify(&compact, &pub_bytes).unwrap();
    }

    #[test]
    fn jws_verify_rejects_tampered() {
        let (pkcs8, jwk) = generate_ec_p256().unwrap();
        let key_pair = load_key_pair(&pkcs8).unwrap();
        let header = serde_json::json!({"alg": "ES256"});
        let payload = serde_json::json!({"sub": "test"});
        let mut compact = jws_sign(
            header.to_string().as_bytes(),
            payload.to_string().as_bytes(),
            &key_pair,
        )
        .unwrap();
        // Tamper with payload
        compact = compact.replacen('.', ".AAAA", 1);
        let pub_bytes = jwk_to_public_bytes(&jwk).unwrap();
        assert!(jws_verify(&compact, &pub_bytes).is_err());
    }

    #[test]
    fn disclosure_creation() {
        let (b64, hash) =
            create_disclosure("email", &serde_json::json!("user@example.com")).unwrap();
        assert!(!b64.is_empty());
        assert!(!hash.is_empty());
        // Verify hash matches
        assert_eq!(sd_hash(&b64), hash);
    }

    #[test]
    fn sd_jwt_serialize_parse_roundtrip() {
        let jwt = "eyJhbGciOiJFUzI1NiJ9.eyJzdWIiOiJ0ZXN0In0.sig";
        let disclosures = vec!["disc1".to_string(), "disc2".to_string()];
        let serialized = serialize_sd_jwt(jwt, &disclosures, None);
        let (parsed_jwt, parsed_disc, parsed_kb) = parse_sd_jwt(&serialized).unwrap();
        assert_eq!(parsed_jwt, jwt);
        assert_eq!(parsed_disc, vec!["disc1", "disc2"]);
        assert!(parsed_kb.is_none());
    }

    #[test]
    fn sd_jwt_serialize_with_kb_jwt() {
        let jwt = "header.payload.sig";
        let disclosures = vec!["d1".to_string()];
        // A key-binding component has to be a JWT to be treated as one, so this
        // carries decodable segments rather than three arbitrary words.
        let kb = key_binding_jwt();
        let serialized = serialize_sd_jwt(jwt, &disclosures, Some(&kb));
        let (parsed_jwt, parsed_disc, parsed_kb) = parse_sd_jwt(&serialized).unwrap();
        assert_eq!(parsed_jwt, jwt);
        assert_eq!(parsed_disc, vec!["d1"]);
        assert_eq!(parsed_kb, Some(kb.as_str()));
    }

    #[test]
    fn jws_decode_payload_works() {
        let header = b64u_encode(b"{\"alg\":\"ES256\"}");
        let payload = b64u_encode(b"{\"sub\":\"test\"}");
        let compact = format!("{header}.{payload}.fake-sig");
        let decoded = jws_decode_payload(&compact).unwrap();
        assert_eq!(decoded["sub"], "test");
    }

    // ── Conformance against the pinned reference implementation ──────
    //
    // Expected values come from agent-intent/verifiable-intent at
    // 356c29635f1c44df7de02edb58699ca9f29bece6, captured by
    // scripts/dev/generate-vi-reference-vectors.py. A ZeroClaw round trip
    // cannot show that both sides preserved the same local deviation, so
    // these assert against output ZeroClaw did not produce.

    /// The one vector whose claim value contains non-ASCII text.
    const NON_ASCII_CASE: &str = "object_nested";

    fn vectors() -> serde_json::Value {
        serde_json::from_str(include_str!(
            "../../tests/fixtures/vi-reference-vectors.json"
        ))
        .expect("reference vectors must be valid JSON")
    }

    fn reference_public_key() -> Vec<u8> {
        let vectors = vectors();
        let jwk = &vectors["sd_jwt"]["public_jwk"];
        let field = |name: &str| jwk[name].as_str().expect("jwk field").to_string();
        jwk_to_public_bytes(&Jwk {
            kty: field("kty"),
            crv: field("crv"),
            x: field("x"),
            y: field("y"),
            d: None,
        })
        .expect("reference JWK must convert")
    }

    #[test]
    fn disclosure_encoding_matches_the_reference() {
        let vectors = vectors();
        let mut checked = 0;
        for case in vectors["disclosures"].as_array().expect("disclosures") {
            let name = case["case"].as_str().expect("case name");
            if name == NON_ASCII_CASE {
                continue;
            }
            let (disclosure, hash) = create_disclosure_with_salt(
                case["claim_name"].as_str(),
                &case["claim_value"],
                case["salt"].as_str().expect("salt"),
            )
            .expect("disclosure must encode");

            assert_eq!(
                disclosure,
                case["disclosure"].as_str().expect("expected"),
                "disclosure bytes for {name}"
            );
            assert_eq!(
                hash,
                case["hash"].as_str().expect("expected hash"),
                "disclosure hash for {name}"
            );
            checked += 1;
        }
        assert_eq!(checked, 5, "every ASCII vector must be exercised");
    }

    /// Python's `json.dumps` escapes non-ASCII to `\uXXXX` by default;
    /// `serde_json` emits UTF-8 directly. The values are equal, the bytes are
    /// not, so the disclosure hashes differ. RFC 8259 permits both and the
    /// spec does not choose between them, so this pins the divergence for a
    /// maintainer decision rather than silently resolving it either way.
    #[test]
    fn non_ascii_disclosure_encoding_diverges_from_the_reference() {
        let vectors = vectors();
        let case = vectors["disclosures"]
            .as_array()
            .expect("disclosures")
            .iter()
            .find(|case| case["case"] == NON_ASCII_CASE)
            .expect("the non-ASCII vector must exist");

        let (disclosure, hash) = create_disclosure_with_salt(
            case["claim_name"].as_str(),
            &case["claim_value"],
            case["salt"].as_str().expect("salt"),
        )
        .expect("disclosure must encode");

        let reference_disclosure = case["disclosure"].as_str().expect("expected");
        assert_ne!(disclosure, reference_disclosure);
        assert_ne!(hash, case["hash"].as_str().expect("expected hash"));

        // The divergence is serialization only: both decode to the same value.
        assert_eq!(
            decode_disclosure(&disclosure).expect("ours").claim_value(),
            decode_disclosure(reference_disclosure)
                .expect("reference")
                .claim_value(),
        );
    }

    #[test]
    fn decode_disclosure_reads_both_reference_shapes() {
        let vectors = vectors();
        for case in vectors["disclosures"].as_array().expect("disclosures") {
            let name = case["case"].as_str().expect("case name");
            let decoded = decode_disclosure(case["disclosure"].as_str().expect("disclosure"))
                .expect("reference disclosure must decode");

            assert_eq!(
                decoded.salt(),
                case["salt"].as_str().expect("salt"),
                "salt for {name}"
            );
            assert_eq!(
                decoded.claim_name(),
                case["claim_name"].as_str(),
                "name for {name}"
            );
            assert_eq!(
                decoded.claim_value(),
                &case["claim_value"],
                "value for {name}"
            );
        }
    }

    #[test]
    fn decode_disclosure_rejects_malformed_input() {
        let cases = [
            ("not an array", &br#"{"salt":"x"}"#[..]),
            ("one element", &br#"["salt"]"#[..]),
            ("four elements", &br#"["salt","name","value","extra"]"#[..]),
            ("non-string salt", &br#"[1,"name","value"]"#[..]),
            ("non-string claim name", &br#"["salt",2,"value"]"#[..]),
        ];
        for (label, raw) in cases {
            let encoded = b64u_encode(raw);
            assert!(decode_disclosure(&encoded).is_err(), "must reject: {label}");
        }
    }

    #[test]
    fn resolve_disclosures_matches_the_reference() {
        let vectors = vectors();
        let parsed = SdJwt::parse(
            vectors["sd_jwt"]["serialized"]
                .as_str()
                .expect("serialized"),
        )
        .expect("reference SD-JWT must parse");

        let resolved = parsed
            .resolve_disclosures()
            .expect("resolution must succeed");
        let expected = vectors["sd_jwt"]["resolved_claims"]
            .as_object()
            .expect("resolved_claims");

        assert_eq!(&resolved, expected);
    }

    #[test]
    fn selective_presentation_matches_the_reference() {
        let vectors = vectors();
        let selective = &vectors["selective_presentation"];
        let base = selective["base_jwt"].as_str().expect("base_jwt");
        let subset: Vec<String> = selective["subset_disclosures"]
            .as_array()
            .expect("subset")
            .iter()
            .map(|entry| entry.as_str().expect("disclosure").to_string())
            .collect();

        let presentation = serialize_sd_jwt(base, &subset, None);
        assert_eq!(
            presentation,
            selective["presentation"].as_str().expect("presentation")
        );
        assert_eq!(
            sd_hash(&presentation),
            selective["binding_hash"].as_str().expect("binding_hash"),
        );

        // The same subset selected through the parsed type, by index.
        let parsed = SdJwt::parse(
            vectors["sd_jwt"]["serialized"]
                .as_str()
                .expect("serialized"),
        )
        .expect("parse");
        let by_index = parsed.selective_presentation(&[0, 2]).expect("select");
        assert_eq!(
            by_index.strip_prefix(parsed.issuer_jwt()),
            presentation.strip_prefix(base),
        );
    }

    #[test]
    fn selective_presentation_rejects_an_out_of_range_index() {
        let vectors = vectors();
        let parsed = SdJwt::parse(
            vectors["sd_jwt"]["serialized"]
                .as_str()
                .expect("serialized"),
        )
        .expect("parse");
        assert!(parsed.selective_presentation(&[0, 99]).is_err());
    }

    #[test]
    fn verifies_a_signature_the_reference_produced() {
        let vectors = vectors();
        let parsed = SdJwt::parse(
            vectors["sd_jwt"]["serialized"]
                .as_str()
                .expect("serialized"),
        )
        .expect("parse");
        let public_key = reference_public_key();

        jws_verify(parsed.issuer_jwt(), &public_key)
            .expect("must verify a signature produced by the reference");

        // Control: the same key must reject a mutated header.
        let tampered = parsed.issuer_jwt().replacen("eyJ", "eyK", 1);
        assert!(jws_verify(&tampered, &public_key).is_err());
    }

    #[test]
    fn parse_retains_the_key_binding_jwt_without_binding_over_it() {
        let vectors = vectors();
        let serialized = vectors["sd_jwt"]["serialized"]
            .as_str()
            .expect("serialized");
        let kb = key_binding_jwt();
        let with_kb = format!("{serialized}{kb}");

        let parsed = SdJwt::parse(&with_kb).expect("parse");
        assert_eq!(parsed.key_binding_jwt(), Some(kb.as_str()));
        assert_eq!(parsed.disclosures().len(), 4);
        assert_eq!(parsed.serialize(), with_kb);
        // §6.1: what a binding hashes excludes the KB-JWT and keeps the `~`.
        assert_eq!(parsed.presentation(), serialized);
    }

    // ── Wire shapes the parser must refuse ───────────────────────────

    const PROBE_SALT: &str = "AAAAAAAAAAAAAAAAAAAAAA";

    /// A three-segment JWT over the given header and payload. Unsigned: these
    /// tests exercise parsing and resolution, which never inspect a signature.
    fn unsigned_jwt(header: &serde_json::Value, payload: &serde_json::Value) -> String {
        format!(
            "{}.{}.c2ln",
            b64u_encode(header.to_string().as_bytes()),
            b64u_encode(payload.to_string().as_bytes())
        )
    }

    fn sd_jwt_over(payload: &serde_json::Value, disclosures: &[String]) -> String {
        let header = serde_json::json!({"alg": "ES256", "typ": "kb-sd-jwt"});
        serialize_sd_jwt(&unsigned_jwt(&header, payload), disclosures, None)
    }

    /// A structurally valid key-binding JWT. Nothing verifies its signature or
    /// its claims at this layer; it exists so that tests about the *shape* of a
    /// presentation are not accidentally testing the KB rule.
    fn key_binding_jwt() -> String {
        unsigned_jwt(
            &serde_json::json!({"alg": "ES256", "typ": "kb+jwt"}),
            &serde_json::json!({"aud": "https://verifier.example", "nonce": "n-1"}),
        )
    }

    /// A placeholder object carries `...` and nothing else. Replacing one that
    /// has siblings discards those siblings, which is silent data loss at a
    /// point where the caller believes it received a disclosed value.
    #[test]
    fn a_delegate_payload_placeholder_must_carry_nothing_but_the_hash() {
        let (disclosure, hash) =
            create_disclosure_with_salt(None, &serde_json::json!({"id": "agent-1"}), PROBE_SALT)
                .unwrap();
        let payload = serde_json::json!({
            "delegate_payload": [ { "...": hash, "id": "dropped-on-the-floor" } ]
        });

        let parsed = SdJwt::parse(&sd_jwt_over(&payload, &[disclosure])).expect("parses");
        assert!(
            parsed.resolve_disclosures().is_err(),
            "a placeholder with sibling keys must be refused, not silently replaced"
        );
    }

    /// A digest appearing twice makes the disclosure graph ambiguous.
    #[test]
    fn a_repeated_sd_digest_is_refused() {
        let (disclosure, hash) =
            create_disclosure_with_salt(Some("a"), &serde_json::json!(1), PROBE_SALT).unwrap();
        let payload = serde_json::json!({ "_sd": [hash, hash] });

        let parsed = SdJwt::parse(&sd_jwt_over(&payload, &[disclosure])).expect("parses");
        assert!(
            parsed.resolve_disclosures().is_err(),
            "the same digest listed twice must be refused rather than deduplicated"
        );
    }

    /// A disclosure may not introduce a claim named `_sd` or `...`.
    #[test]
    fn a_disclosure_may_not_be_named_after_a_reserved_claim() {
        for reserved in ["_sd", "..."] {
            let (disclosure, hash) = create_disclosure_with_salt(
                Some(reserved),
                &serde_json::json!("smuggled"),
                PROBE_SALT,
            )
            .unwrap();
            let payload = serde_json::json!({ "_sd": [hash] });

            let parsed = SdJwt::parse(&sd_jwt_over(&payload, &[disclosure])).expect("parses");
            assert!(
                parsed.resolve_disclosures().is_err(),
                "a disclosure named `{reserved}` must be refused"
            );
        }
    }

    /// A disclosure may not overwrite a claim the issuer signed in the clear.
    #[test]
    fn a_disclosure_may_not_overwrite_an_existing_claim() {
        let (disclosure, hash) = create_disclosure_with_salt(
            Some("aud"),
            &serde_json::json!("https://attacker.example"),
            PROBE_SALT,
        )
        .unwrap();
        let payload = serde_json::json!({
            "aud": "https://network.example",
            "_sd": [hash]
        });

        let parsed = SdJwt::parse(&sd_jwt_over(&payload, &[disclosure])).expect("parses");
        assert!(
            parsed.resolve_disclosures().is_err(),
            "a disclosure must not redefine a permanently disclosed claim"
        );
    }

    /// Without a trailing `~` the final component is a key-binding JWT, so a
    /// presentation that simply omits the separator must not be read as one
    /// with its last disclosure silently removed.
    #[test]
    fn a_presentation_missing_its_trailing_tilde_is_refused() {
        let issuer = unsigned_jwt(
            &serde_json::json!({"alg": "ES256"}),
            &serde_json::json!({"iss": "https://issuer.example"}),
        );
        let (disclosure, _) =
            create_disclosure_with_salt(Some("a"), &serde_json::json!(1), PROBE_SALT).unwrap();

        let malformed = format!("{issuer}~{disclosure}");
        assert!(
            SdJwt::parse(&malformed).is_err(),
            "a disclosure must not be accepted as a key-binding JWT"
        );
    }

    /// The final component, when present, has to be a JWT.
    #[test]
    fn a_key_binding_component_must_be_a_jwt() {
        let issuer = unsigned_jwt(
            &serde_json::json!({"alg": "ES256"}),
            &serde_json::json!({"iss": "https://issuer.example"}),
        );
        for bogus in ["kb.jwt.here", "two.segments", "!!!.!!!.!!!"] {
            let malformed = format!("{issuer}~{bogus}");
            assert!(
                SdJwt::parse(&malformed).is_err(),
                "a key-binding component that is not a JWT must be refused: {bogus}"
            );
        }
    }

    /// An empty segment is not a disclosure. It arises when a serialized SD-JWT
    /// that already ends in `~` is composed as though it were a bare JWT.
    #[test]
    fn an_empty_disclosure_segment_is_refused() {
        let issuer = unsigned_jwt(
            &serde_json::json!({"alg": "ES256"}),
            &serde_json::json!({"iss": "https://issuer.example"}),
        );
        let (disclosure, _) =
            create_disclosure_with_salt(Some("a"), &serde_json::json!(1), PROBE_SALT).unwrap();

        let malformed = format!("{issuer}~~{disclosure}~");
        assert!(
            parse_sd_jwt(&malformed).is_err(),
            "an empty segment must be refused rather than passed on as a disclosure"
        );
    }

    /// Duplicate members make a signed JSON object ambiguous: parsers differ on
    /// first-wins versus last-wins, so two verifiers can reach opposite results
    /// from identical bytes.
    #[test]
    fn duplicate_members_in_signed_json_are_refused() {
        let header_ok = b64u_encode(br#"{"alg":"ES256"}"#);
        let payload_ok = b64u_encode(br#"{"iss":"a"}"#);
        let header_dup = b64u_encode(br#"{"alg":"ES256","alg":"none"}"#);
        let payload_dup = b64u_encode(br#"{"iss":"a","iss":"b"}"#);

        let dup_payload = format!("{header_ok}.{payload_dup}.c2ln");
        assert!(
            jws_decode_payload(&dup_payload).is_err(),
            "a duplicate payload claim must be refused"
        );

        let dup_header = format!("{header_dup}.{payload_ok}.c2ln");
        assert!(
            jws_decode_header(&dup_header).is_err(),
            "a duplicate header parameter must be refused"
        );

        // Control: the same helpers accept the unambiguous forms.
        let clean = format!("{header_ok}.{payload_ok}.c2ln");
        assert!(jws_decode_payload(&clean).is_ok());
        assert!(jws_decode_header(&clean).is_ok());
    }

    /// A disclosure is bound by a digest the issuer signed, and resolution
    /// merges its value straight into the claim set. A duplicate member inside
    /// one is therefore a duplicate claim name in the payload a verifier acts
    /// on, which the security model requires refusing rather than resolving to
    /// whichever member the parser happened to keep.
    ///
    /// Both spec shapes are covered because a duplicate can hide in either, and
    /// nesting is not an escape hatch.
    #[test]
    fn duplicate_members_inside_a_disclosure_are_refused() {
        let three_element =
            b64u_encode(br#"["AAAAAAAAAAAAAAAAAAAAAA","checkout_mandate",{"vct":"a","vct":"b"}]"#);
        let two_element = b64u_encode(br#"["AAAAAAAAAAAAAAAAAAAAAA",{"vct":"a","vct":"b"}]"#);
        let nested = b64u_encode(br#"["AAAAAAAAAAAAAAAAAAAAAA","m",{"outer":{"k":1,"k":2}}]"#);

        for (label, encoded) in [
            ("three-element", &three_element),
            ("two-element", &two_element),
            ("nested", &nested),
        ] {
            assert!(
                decode_disclosure(encoded).is_err(),
                "an ambiguous disclosure must be refused: {label}"
            );
        }

        // Controls over the same shapes, so a rejection is attributable to the
        // duplicate rather than to the shape being unsupported.
        let three_ok = b64u_encode(br#"["AAAAAAAAAAAAAAAAAAAAAA","checkout_mandate",{"vct":"a"}]"#);
        let two_ok = b64u_encode(br#"["AAAAAAAAAAAAAAAAAAAAAA",{"vct":"a"}]"#);
        assert!(
            decode_disclosure(&three_ok).is_ok(),
            "control three-element"
        );
        assert!(decode_disclosure(&two_ok).is_ok(), "control two-element");
    }

    #[test]
    fn jws_helpers_require_exactly_three_segments() {
        let header = b64u_encode(b"{\"alg\":\"ES256\"}");
        let payload = b64u_encode(b"{\"sub\":\"test\"}");
        let two = format!("{header}.{payload}");
        let four = format!("{header}.{payload}.sig.extra");

        for malformed in [&two, &four] {
            assert!(jws_decode_header(malformed).is_err(), "header: {malformed}");
            assert!(
                jws_decode_payload(malformed).is_err(),
                "payload: {malformed}"
            );
            assert!(jws_verify(malformed, &[]).is_err(), "verify: {malformed}");
        }
    }
}
