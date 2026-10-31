//! Ed25519 plugin signature verification.

use base64::Engine;
use base64::engine::general_purpose::URL_SAFE_NO_PAD;
use ring::signature::{self, Ed25519KeyPair, KeyPair};

use super::error::PluginError;

/// Signature mode controls how unsigned/unverified plugins are handled.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Default)]
pub enum SignatureMode {
    /// Reject plugins that are unsigned or fail verification.
    Strict,
    /// Warn but allow plugins that are unsigned or fail verification.
    Permissive,
    /// Do not check signatures at all.
    #[default]
    Disabled,
}

/// Result of verifying a plugin's signature.
#[derive(Debug, Clone, PartialEq, Eq)]
pub enum VerificationResult {
    /// Signature is valid and matches a trusted publisher key.
    Valid { publisher_key: String },
    /// Plugin has no signature field.
    Unsigned,
    /// Signature is present but does not match any trusted key.
    Untrusted,
    /// Signature is present but cryptographically invalid.
    Invalid { reason: String },
}

impl VerificationResult {
    /// Returns true if the signature is valid.
    pub fn is_valid(&self) -> bool {
        matches!(self, Self::Valid { .. })
    }
}

// ── Base64url helpers (reused from verifiable_intent but kept local to avoid coupling) ──

fn b64u_encode(data: &[u8]) -> String {
    URL_SAFE_NO_PAD.encode(data)
}

fn b64u_decode(s: &str) -> Result<Vec<u8>, PluginError> {
    URL_SAFE_NO_PAD
        .decode(s)
        .map_err(|e| PluginError::SignatureInvalid(format!("base64url decode error: {e}")))
}

// ── Hex helpers ──

fn hex_decode(s: &str) -> Result<Vec<u8>, PluginError> {
    // Simple hex decoder
    let s = s.trim();
    if !s.len().is_multiple_of(2) {
        return Err(PluginError::SignatureInvalid(
            "hex string must have even length".into(),
        ));
    }
    (0..s.len())
        .step_by(2)
        .map(|i| {
            u8::from_str_radix(&s[i..i + 2], 16)
                .map_err(|e| PluginError::SignatureInvalid(format!("hex decode: {e}")))
        })
        .collect()
}

fn hex_encode(data: &[u8]) -> String {
    data.iter().map(|b| format!("{b:02x}")).collect()
}

/// Compute a lowercase hexadecimal SHA-256 digest.
#[must_use]
pub fn sha256_hex(data: &[u8]) -> String {
    let digest = ring::digest::digest(&ring::digest::SHA256, data);
    hex_encode(digest.as_ref())
}

/// Validate the manifest representation of a SHA-256 digest.
pub fn validate_sha256_hex(expected: &str) -> Result<(), PluginError> {
    if expected.len() != 64 || !expected.bytes().all(|byte| byte.is_ascii_hexdigit()) {
        return Err(PluginError::PayloadDigestInvalid(expected.to_string()));
    }
    Ok(())
}

/// Verify bytes against a manifest-provided SHA-256 digest.
pub fn verify_payload_digest(data: &[u8], expected: &str) -> Result<(), PluginError> {
    validate_sha256_hex(expected)?;
    let expected = expected.to_ascii_lowercase();
    let actual = sha256_hex(data);
    if actual != expected {
        return Err(PluginError::PayloadDigestMismatch { expected, actual });
    }
    Ok(())
}

// ── Canonical manifest bytes ──

/// Compute the canonical bytes of a manifest for signing/verification.
///
/// This strips the exact root `signature` and `publisher_key` fields from the
/// TOML document and returns the remaining bytes. Nested fields with those
/// names remain signed, including fields in a plugin config schema.
pub fn canonical_manifest_bytes(manifest_toml: &str) -> Result<Vec<u8>, PluginError> {
    let mut document = manifest_toml
        .parse::<toml_edit::DocumentMut>()
        .map_err(|error| PluginError::InvalidManifest(format!("invalid TOML: {error}")))?;
    document.as_table_mut().remove("signature");
    document.as_table_mut().remove("publisher_key");
    let rendered = document.to_string();
    let mut lines: Vec<&str> = rendered.lines().collect();
    // Remove trailing empty lines to normalize
    while lines.last().is_some_and(|l| l.trim().is_empty()) {
        lines.pop();
    }
    let canonical = lines.join("\n");
    Ok(canonical.into_bytes())
}

// ── Signing ──

/// Sign manifest bytes with an Ed25519 private key (PKCS#8 DER).
/// Returns the base64url-encoded signature.
pub fn sign_manifest(manifest_toml: &str, pkcs8_der: &[u8]) -> Result<String, PluginError> {
    let key_pair = Ed25519KeyPair::from_pkcs8(pkcs8_der)
        .map_err(|e| PluginError::SignatureInvalid(format!("invalid signing key: {e}")))?;
    let canonical = canonical_manifest_bytes(manifest_toml)?;
    let sig = key_pair.sign(&canonical);
    Ok(b64u_encode(sig.as_ref()))
}

/// Get the hex-encoded public key from a PKCS#8 Ed25519 private key.
pub fn public_key_hex(pkcs8_der: &[u8]) -> Result<String, PluginError> {
    let key_pair = Ed25519KeyPair::from_pkcs8(pkcs8_der)
        .map_err(|e| PluginError::SignatureInvalid(format!("invalid signing key: {e}")))?;
    Ok(hex_encode(key_pair.public_key().as_ref()))
}

/// Sign a manifest and return the signed document, with the root `signature`
/// and `publisher_key` entries embedded.
///
/// This is the publisher-side inverse of [`canonical_manifest_bytes`]. The
/// entries are placed in the document before it is signed, so the signed bytes
/// are exactly what the host reconstructs when it removes them again.
///
/// Embedding by hand after [`sign_manifest`] is equivalent only while nothing
/// that was signed moves. The TOML editor the host canonicalizes with attaches
/// a blank line or comment to the entry or table header below it. Entries
/// inserted directly under decoration that was already in the signed manifest
/// take it with them when the host removes them, and decoration added below
/// the entries attaches to the next signed item. Either way the signature no
/// longer verifies.
///
/// Root `signature` and `publisher_key` entries already present are replaced.
pub fn sign_manifest_document(
    manifest_toml: &str,
    pkcs8_der: &[u8],
) -> Result<String, PluginError> {
    let publisher_key = public_key_hex(pkcs8_der)?;
    let mut document = manifest_toml
        .parse::<toml_edit::DocumentMut>()
        .map_err(|error| PluginError::InvalidManifest(format!("invalid TOML: {error}")))?;
    let root = document.as_table_mut();
    root.insert("publisher_key", toml_edit::value(publisher_key));
    root.insert("signature", toml_edit::value(""));
    let signature = sign_manifest(&document.to_string(), pkcs8_der)?;
    document
        .as_table_mut()
        .insert("signature", toml_edit::value(signature));
    Ok(document.to_string())
}

// ── Verification ──

pub fn verify_manifest(
    manifest_toml: &str,
    signature_b64: &str,
    publisher_key_hex: &str,
    trusted_keys: &[String],
) -> VerificationResult {
    // Check if the publisher key is in the trusted set
    let normalized_key = publisher_key_hex.trim().to_lowercase();
    let is_trusted = trusted_keys
        .iter()
        .any(|k| k.trim().to_lowercase() == normalized_key);

    if !is_trusted {
        return VerificationResult::Untrusted;
    }

    // Decode the public key
    let pub_key_bytes = match hex_decode(publisher_key_hex) {
        Ok(bytes) => bytes,
        Err(e) => {
            return VerificationResult::Invalid {
                reason: format!("invalid publisher key: {e}"),
            };
        }
    };

    // Decode the signature
    let sig_bytes = match b64u_decode(signature_b64) {
        Ok(bytes) => bytes,
        Err(e) => {
            return VerificationResult::Invalid {
                reason: format!("invalid signature encoding: {e}"),
            };
        }
    };

    // Compute canonical bytes
    let canonical = match canonical_manifest_bytes(manifest_toml) {
        Ok(canonical) => canonical,
        Err(error) => {
            return VerificationResult::Invalid {
                reason: error.to_string(),
            };
        }
    };

    // Verify
    let peer_public_key = signature::UnparsedPublicKey::new(&signature::ED25519, &pub_key_bytes);
    match peer_public_key.verify(&canonical, &sig_bytes) {
        Ok(()) => VerificationResult::Valid {
            publisher_key: normalized_key,
        },
        Err(_) => VerificationResult::Invalid {
            reason: "Ed25519 signature verification failed".into(),
        },
    }
}

/// Check a manifest's signature and enforce the configured signature mode.
/// Returns `Ok(VerificationResult)` on success (or warning in permissive mode),
/// or `Err(PluginError)` if the plugin should be rejected.
pub fn enforce_signature_policy(
    plugin_name: &str,
    manifest_toml: &str,
    signature: Option<&str>,
    publisher_key: Option<&str>,
    trusted_keys: &[String],
    mode: SignatureMode,
) -> Result<VerificationResult, PluginError> {
    if mode == SignatureMode::Disabled {
        return Ok(VerificationResult::Unsigned);
    }

    match (signature, publisher_key) {
        (None, _) | (_, None) => {
            // Plugin is unsigned
            match mode {
                SignatureMode::Strict => Err(PluginError::UnsignedPlugin(plugin_name.to_string())),
                SignatureMode::Permissive => {
                    ::zeroclaw_log::record!(
                        WARN,
                        ::zeroclaw_log::Event::new(module_path!(), ::zeroclaw_log::Action::Note)
                            .with_outcome(::zeroclaw_log::EventOutcome::Unknown)
                            .with_attrs(::serde_json::json!({"plugin": plugin_name})),
                        "plugin is unsigned; loading in permissive mode"
                    );
                    Ok(VerificationResult::Unsigned)
                }
                SignatureMode::Disabled => Ok(VerificationResult::Unsigned),
            }
        }
        (Some(sig), Some(pub_key)) => {
            let result = verify_manifest(manifest_toml, sig, pub_key, trusted_keys);
            match &result {
                VerificationResult::Valid { publisher_key } => {
                    ::zeroclaw_log::record!(INFO, ::zeroclaw_log::Event::new(module_path!(), ::zeroclaw_log::Action::Note).with_attrs(::serde_json::json!({"plugin": plugin_name, "publisher_key": publisher_key.as_str()})), "plugin signature verified");
                    Ok(result)
                }
                VerificationResult::Untrusted => match mode {
                    SignatureMode::Strict => Err(PluginError::UntrustedPublisher {
                        plugin: plugin_name.to_string(),
                        publisher_key: pub_key.to_string(),
                    }),
                    SignatureMode::Permissive => {
                        ::zeroclaw_log::record!(WARN, ::zeroclaw_log::Event::new(module_path!(), ::zeroclaw_log::Action::Note).with_outcome(::zeroclaw_log::EventOutcome::Unknown).with_attrs(::serde_json::json!({"plugin": plugin_name, "publisher_key": pub_key})), "plugin publisher key not trusted; loading in permissive mode");
                        Ok(result)
                    }
                    SignatureMode::Disabled => Ok(result),
                },
                VerificationResult::Invalid { reason } => match mode {
                    SignatureMode::Strict => Err(PluginError::SignatureInvalid(format!(
                        "plugin '{}': {}",
                        plugin_name, reason
                    ))),
                    SignatureMode::Permissive => {
                        ::zeroclaw_log::record!(WARN, ::zeroclaw_log::Event::new(module_path!(), ::zeroclaw_log::Action::Note).with_outcome(::zeroclaw_log::EventOutcome::Unknown).with_attrs(::serde_json::json!({"plugin": plugin_name, "reason": reason.as_str()})), "plugin signature invalid; loading in permissive mode");
                        Ok(result)
                    }
                    SignatureMode::Disabled => Ok(result),
                },
                VerificationResult::Unsigned => Ok(result),
            }
        }
    }
}

// ── Key Generation ──

/// Generate a new Ed25519 key pair for plugin signing.
/// Returns `(pkcs8_der_bytes, public_key_hex)`.
pub fn generate_signing_key() -> Result<(Vec<u8>, String), PluginError> {
    let rng = ring::rand::SystemRandom::new();
    let pkcs8 = Ed25519KeyPair::generate_pkcs8(&rng)
        .map_err(|e| PluginError::SignatureInvalid(format!("keygen failed: {e}")))?;
    let key_pair = Ed25519KeyPair::from_pkcs8(pkcs8.as_ref())
        .map_err(|e| PluginError::SignatureInvalid(format!("parse pkcs8: {e}")))?;
    let pub_hex = hex_encode(key_pair.public_key().as_ref());
    Ok((pkcs8.as_ref().to_vec(), pub_hex))
}

#[cfg(test)]
mod tests {
    use super::*;

    const TEST_MANIFEST: &str = r#"
name = "test-plugin"
version = "0.1.0"
description = "A test plugin"
wasm_path = "plugin.wasm"
capabilities = ["tool"]
permissions = []
"#;

    const SCHEMA_MANIFEST: &str = r#"
name = "schema-plugin"
version = "0.1.0"
description = "A plugin with signed config schema fields"
signature_algorithm = "ed25519"
wasm_path = "plugin.wasm"
capabilities = ["tool"]
permissions = ["config_read"]

[config_schema]
type = "object"
additionalProperties = false

[config_schema.properties.signature]
type = "string"

[config_schema.properties.publisher_key]
type = "string"
"#;

    fn generate_test_keypair() -> (Vec<u8>, String) {
        generate_signing_key().expect("keygen should succeed")
    }

    #[test]
    fn test_canonical_manifest_strips_only_exact_root_signature_fields() {
        let manifest_with_signature = SCHEMA_MANIFEST.replacen(
            "wasm_path = \"plugin.wasm\"",
            "signature = \"abc123\"\npublisher_key = \"deadbeef\"\nwasm_path = \"plugin.wasm\"",
            1,
        );
        let canonical = canonical_manifest_bytes(&manifest_with_signature).unwrap();
        assert_eq!(
            canonical,
            canonical_manifest_bytes(SCHEMA_MANIFEST).unwrap()
        );

        let canonical_str = String::from_utf8(canonical).unwrap();
        let canonical_table: toml::Table = toml::from_str(&canonical_str).unwrap();
        assert!(!canonical_table.contains_key("signature"));
        assert!(!canonical_table.contains_key("publisher_key"));
        assert_eq!(
            canonical_table
                .get("signature_algorithm")
                .and_then(toml::Value::as_str),
            Some("ed25519")
        );

        let properties = canonical_table
            .get("config_schema")
            .and_then(toml::Value::as_table)
            .and_then(|schema| schema.get("properties"))
            .and_then(toml::Value::as_table)
            .unwrap();
        assert!(properties.contains_key("signature"));
        assert!(properties.contains_key("publisher_key"));
    }

    #[test]
    fn payload_digest_verification_accepts_exact_bytes() {
        let bytes = b"signed component bytes";
        let digest = sha256_hex(bytes);
        verify_payload_digest(bytes, &digest).expect("exact payload digest matches");
        verify_payload_digest(bytes, &digest.to_ascii_uppercase())
            .expect("hex digest comparison is case-insensitive");
    }

    #[test]
    fn payload_digest_verification_rejects_tampering_and_invalid_shape() {
        let digest = sha256_hex(b"original");
        assert!(matches!(
            verify_payload_digest(b"tampered", &digest),
            Err(PluginError::PayloadDigestMismatch { .. })
        ));
        assert!(matches!(
            verify_payload_digest(b"original", "not-a-sha256"),
            Err(PluginError::PayloadDigestInvalid(_))
        ));
    }

    #[test]
    fn test_canonical_manifest_without_signature_fields() {
        let canonical = canonical_manifest_bytes(TEST_MANIFEST).unwrap();
        let canonical_str = String::from_utf8(canonical).unwrap();
        assert!(canonical_str.contains("name = \"test-plugin\""));
    }

    #[test]
    fn test_sign_and_verify_roundtrip() {
        let (pkcs8, pub_hex) = generate_test_keypair();
        let sig = sign_manifest(TEST_MANIFEST, &pkcs8).unwrap();
        let trusted_keys = vec![pub_hex.clone()];
        let result = verify_manifest(TEST_MANIFEST, &sig, &pub_hex, &trusted_keys);
        assert!(result.is_valid());
        assert_eq!(
            result,
            VerificationResult::Valid {
                publisher_key: pub_hex.to_lowercase()
            }
        );
    }

    #[test]
    fn test_verify_rejects_tampered_manifest() {
        let (pkcs8, pub_hex) = generate_test_keypair();
        let sig = sign_manifest(TEST_MANIFEST, &pkcs8).unwrap();
        let tampered = TEST_MANIFEST.replace("0.1.0", "0.2.0");
        let trusted_keys = vec![pub_hex.clone()];
        let result = verify_manifest(&tampered, &sig, &pub_hex, &trusted_keys);
        assert!(matches!(result, VerificationResult::Invalid { .. }));
    }

    /// The `[egress]` declaration must be signature-covered content.
    ///
    /// `canonical_manifest_bytes` strips only the two root signature fields, so
    /// coverage of a new field is automatic rather than opt-in — but "automatic"
    /// is exactly the kind of claim that quietly stops being true if the strip
    /// rule is ever broadened. This pins it: editing a declared destination must
    /// break an existing signature.
    #[test]
    fn egress_declaration_is_signature_covered() {
        const DECLARED: &str = r#"
name = "test-plugin"
version = "0.1.0"
wasm_path = "plugin.wasm"
capabilities = ["tool"]
permissions = ["http_client"]

[egress]
hosts = ["api.example.com"]
"#;
        let (pkcs8, pub_hex) = generate_test_keypair();
        let sig = sign_manifest(DECLARED, &pkcs8).unwrap();
        let trusted_keys = vec![pub_hex.clone()];
        assert!(
            verify_manifest(DECLARED, &sig, &pub_hex, &trusted_keys).is_valid(),
            "the declaration as signed must verify"
        );

        for (label, edited) in [
            (
                "retargeting a declared destination",
                DECLARED.replace("api.example.com", "evil.example.net"),
            ),
            (
                "appending a destination",
                DECLARED.replace(
                    r#"hosts = ["api.example.com"]"#,
                    r#"hosts = ["api.example.com", "evil.example.net"]"#,
                ),
            ),
            (
                "deleting the declaration",
                DECLARED.replace("[egress]\nhosts = [\"api.example.com\"]\n", ""),
            ),
        ] {
            assert!(
                matches!(
                    verify_manifest(&edited, &sig, &pub_hex, &trusted_keys),
                    VerificationResult::Invalid { .. }
                ),
                "{label} must invalidate the publisher's signature"
            );
        }

        // And canonicalization keeps the table, rather than treating `[egress]`
        // or `hosts` as another strippable root field.
        let canonical = String::from_utf8(canonical_manifest_bytes(DECLARED).unwrap()).unwrap();
        assert!(canonical.contains("[egress]"), "{canonical}");
        assert!(canonical.contains("api.example.com"), "{canonical}");
    }

    #[test]
    fn test_verify_rejects_tampered_nested_schema_signature_field() {
        let (pkcs8, pub_hex) = generate_test_keypair();
        let sig = sign_manifest(SCHEMA_MANIFEST, &pkcs8).unwrap();
        let tampered = SCHEMA_MANIFEST.replace(
            "[config_schema.properties.signature]\ntype = \"string\"",
            "[config_schema.properties.signature]\ntype = \"boolean\"",
        );
        assert_ne!(tampered, SCHEMA_MANIFEST);

        let trusted_keys = vec![pub_hex.clone()];
        let result = verify_manifest(&tampered, &sig, &pub_hex, &trusted_keys);
        assert!(matches!(result, VerificationResult::Invalid { .. }));
    }

    #[test]
    fn test_malformed_manifest_cannot_be_signed_or_verified() {
        let malformed = "name = \"unterminated";
        assert!(matches!(
            canonical_manifest_bytes(malformed),
            Err(PluginError::InvalidManifest(_))
        ));

        let (pkcs8, pub_hex) = generate_test_keypair();
        assert!(matches!(
            sign_manifest(malformed, &pkcs8),
            Err(PluginError::InvalidManifest(_))
        ));

        let valid_signature = sign_manifest(TEST_MANIFEST, &pkcs8).unwrap();
        let trusted_keys = vec![pub_hex.clone()];
        let result = verify_manifest(malformed, &valid_signature, &pub_hex, &trusted_keys);
        assert!(matches!(result, VerificationResult::Invalid { .. }));
    }

    #[test]
    fn test_verify_rejects_wrong_key() {
        let (pkcs8, _pub_hex) = generate_test_keypair();
        let (_pkcs8_2, pub_hex_2) = generate_test_keypair();
        let sig = sign_manifest(TEST_MANIFEST, &pkcs8).unwrap();
        let trusted_keys = vec![pub_hex_2.clone()];
        let result = verify_manifest(TEST_MANIFEST, &sig, &pub_hex_2, &trusted_keys);
        assert!(matches!(result, VerificationResult::Invalid { .. }));
    }

    #[test]
    fn test_verify_untrusted_publisher() {
        let (pkcs8, pub_hex) = generate_test_keypair();
        let sig = sign_manifest(TEST_MANIFEST, &pkcs8).unwrap();
        let trusted_keys: Vec<String> = vec![]; // no trusted keys
        let result = verify_manifest(TEST_MANIFEST, &sig, &pub_hex, &trusted_keys);
        assert_eq!(result, VerificationResult::Untrusted);
    }

    #[test]
    fn test_public_key_hex_matches_generate() {
        let (pkcs8, pub_hex) = generate_test_keypair();
        let derived_hex = public_key_hex(&pkcs8).unwrap();
        assert_eq!(pub_hex, derived_hex);
    }

    #[test]
    fn test_hex_roundtrip() {
        let data = vec![0xDE, 0xAD, 0xBE, 0xEF];
        let encoded = hex_encode(&data);
        assert_eq!(encoded, "deadbeef");
        let decoded = hex_decode(&encoded).unwrap();
        assert_eq!(decoded, data);
    }

    #[test]
    fn test_enforce_policy_disabled_mode() {
        let result = enforce_signature_policy(
            "test",
            TEST_MANIFEST,
            None,
            None,
            &[],
            SignatureMode::Disabled,
        )
        .unwrap();
        assert_eq!(result, VerificationResult::Unsigned);
    }

    #[test]
    fn test_enforce_policy_strict_rejects_unsigned() {
        let err = enforce_signature_policy(
            "test",
            TEST_MANIFEST,
            None,
            None,
            &[],
            SignatureMode::Strict,
        )
        .unwrap_err();
        assert!(matches!(err, PluginError::UnsignedPlugin(_)));
    }

    #[test]
    fn test_enforce_policy_permissive_allows_unsigned() {
        let result = enforce_signature_policy(
            "test",
            TEST_MANIFEST,
            None,
            None,
            &[],
            SignatureMode::Permissive,
        )
        .unwrap();
        assert_eq!(result, VerificationResult::Unsigned);
    }

    #[test]
    fn test_enforce_policy_strict_rejects_untrusted() {
        let (pkcs8, pub_hex) = generate_test_keypair();
        let sig = sign_manifest(TEST_MANIFEST, &pkcs8).unwrap();
        let err = enforce_signature_policy(
            "test",
            TEST_MANIFEST,
            Some(&sig),
            Some(&pub_hex),
            &[], // no trusted keys
            SignatureMode::Strict,
        )
        .unwrap_err();
        assert!(matches!(err, PluginError::UntrustedPublisher { .. }));
    }

    #[test]
    fn test_enforce_policy_strict_accepts_valid_signature() {
        let (pkcs8, pub_hex) = generate_test_keypair();
        let sig = sign_manifest(TEST_MANIFEST, &pkcs8).unwrap();
        let trusted_keys = vec![pub_hex.clone()];
        let result = enforce_signature_policy(
            "test",
            TEST_MANIFEST,
            Some(&sig),
            Some(&pub_hex),
            &trusted_keys,
            SignatureMode::Strict,
        )
        .unwrap();
        assert!(result.is_valid());
    }

    #[test]
    fn test_enforce_policy_strict_rejects_invalid_signature() {
        let (pkcs8, pub_hex) = generate_test_keypair();
        let _sig = sign_manifest(TEST_MANIFEST, &pkcs8).unwrap();
        let trusted_keys = vec![pub_hex.clone()];
        let err = enforce_signature_policy(
            "test",
            TEST_MANIFEST,
            Some("badsignature"),
            Some(&pub_hex),
            &trusted_keys,
            SignatureMode::Strict,
        )
        .unwrap_err();
        assert!(matches!(err, PluginError::SignatureInvalid(_)));
    }

    #[test]
    fn test_signature_mode_default_is_disabled() {
        assert_eq!(SignatureMode::default(), SignatureMode::Disabled);
    }

    #[test]
    fn test_manifest_with_signature_fields_verifies() {
        let (pkcs8, pub_hex) = generate_test_keypair();
        // Sign the manifest without signature fields
        let sig = sign_manifest(TEST_MANIFEST, &pkcs8).unwrap();

        // Now create a manifest that includes the signature fields
        let manifest_with_sig = format!(
            r#"
name = "test-plugin"
version = "0.1.0"
description = "A test plugin"
signature = "{sig}"
publisher_key = "{pub_hex}"
wasm_path = "plugin.wasm"
capabilities = ["tool"]
permissions = []
"#
        );

        // Verification should still work because canonical bytes strip sig fields
        let trusted_keys = vec![pub_hex.clone()];
        let result = verify_manifest(&manifest_with_sig, &sig, &pub_hex, &trusted_keys);
        assert!(result.is_valid());
    }

    /// Read the two embedded root entries back the way the host does, from the
    /// parsed document, and enforce strict policy against the signed text.
    fn enforce_strict_on_document(
        signed: &str,
        trusted_keys: &[String],
    ) -> Result<VerificationResult, PluginError> {
        let root: toml::Table = toml::from_str(signed).expect("signed manifest parses");
        enforce_signature_policy(
            "document",
            signed,
            root.get("signature").and_then(toml::Value::as_str),
            root.get("publisher_key").and_then(toml::Value::as_str),
            trusted_keys,
            SignatureMode::Strict,
        )
    }

    #[test]
    fn signed_document_verifies_under_strict_policy() {
        let (pkcs8, pub_hex) = generate_test_keypair();
        for manifest in [TEST_MANIFEST, SCHEMA_MANIFEST] {
            let signed = sign_manifest_document(manifest, &pkcs8).unwrap();
            let result = enforce_strict_on_document(&signed, std::slice::from_ref(&pub_hex))
                .expect("a document signed by a trusted key is accepted");
            assert_eq!(
                result,
                VerificationResult::Valid {
                    publisher_key: pub_hex.clone()
                }
            );
        }
    }

    /// A root entry written after a table header belongs to that table, and
    /// the host then sees an unsigned manifest. The embedded entries have to
    /// land among the root entries even when the manifest ends in tables.
    #[test]
    fn signed_document_keeps_embedded_entries_at_the_root() {
        let (pkcs8, pub_hex) = generate_test_keypair();
        let signed = sign_manifest_document(SCHEMA_MANIFEST, &pkcs8).unwrap();

        let first_table = signed.find("[config_schema]").unwrap();
        assert!(signed.find("\nsignature = ").unwrap() < first_table);
        assert!(signed.find("\npublisher_key = ").unwrap() < first_table);

        let root: toml::Table = toml::from_str(&signed).unwrap();
        assert_eq!(
            root.get("publisher_key").and_then(toml::Value::as_str),
            Some(pub_hex.as_str())
        );
        let schema_properties = root["config_schema"]["properties"].as_table().unwrap();
        assert!(schema_properties["signature"].is_table());
        assert_eq!(
            canonical_manifest_bytes(&signed).unwrap(),
            canonical_manifest_bytes(SCHEMA_MANIFEST).unwrap(),
            "embedding must leave the signed content untouched"
        );
    }

    #[test]
    fn signed_document_rejects_edits_made_after_signing() {
        let (pkcs8, pub_hex) = generate_test_keypair();
        let signed = sign_manifest_document(SCHEMA_MANIFEST, &pkcs8).unwrap();
        let tampered = signed.replace(
            "additionalProperties = false",
            "additionalProperties = true",
        );
        assert_ne!(signed, tampered);

        let err = enforce_strict_on_document(&tampered, &[pub_hex]).unwrap_err();
        assert!(matches!(err, PluginError::SignatureInvalid(_)));
    }

    #[test]
    fn signing_a_signed_document_replaces_the_embedded_entries() {
        let (first_key, first_hex) = generate_test_keypair();
        let (second_key, second_hex) = generate_test_keypair();
        let once = sign_manifest_document(TEST_MANIFEST, &first_key).unwrap();
        let twice = sign_manifest_document(&once, &second_key).unwrap();

        assert_eq!(twice.matches("\nsignature = ").count(), 1);
        assert_eq!(twice.matches("\npublisher_key = ").count(), 1);
        enforce_strict_on_document(&twice, std::slice::from_ref(&second_hex))
            .expect("the replacement signature verifies under the new key");
        let err = enforce_strict_on_document(&twice, &[first_hex]).unwrap_err();
        assert!(matches!(err, PluginError::UntrustedPublisher { .. }));
    }

    /// The rule the distribution guide gives publishers who embed by hand:
    /// where the entries go decides whether signed content moves with them.
    #[test]
    fn hand_embedding_verifies_only_when_no_signed_decoration_moves() {
        let (pkcs8, pub_hex) = generate_test_keypair();
        let signature = sign_manifest(SCHEMA_MANIFEST, &pkcs8).unwrap();
        let entries = format!("signature = \"{signature}\"\npublisher_key = \"{pub_hex}\"\n");
        let trusted = std::slice::from_ref(&pub_hex);

        let after_an_entry = SCHEMA_MANIFEST.replacen(
            "permissions = [\"config_read\"]\n",
            &format!("permissions = [\"config_read\"]\n{entries}"),
            1,
        );
        assert!(after_an_entry.contains(&entries));
        enforce_strict_on_document(&after_an_entry, trusted)
            .expect("entries placed directly after a root entry leave the signed bytes alone");

        let with_their_own_comment = SCHEMA_MANIFEST.replacen(
            "permissions = [\"config_read\"]\n",
            &format!("permissions = [\"config_read\"]\n\n# Publisher signature\n{entries}"),
            1,
        );
        enforce_strict_on_document(&with_their_own_comment, trusted)
            .expect("decoration added together with the entries is removed together with them");

        let under_the_signed_blank_line = SCHEMA_MANIFEST.replacen(
            "\n\n[config_schema]\n",
            &format!("\n\n{entries}[config_schema]\n"),
            1,
        );
        assert!(under_the_signed_blank_line.contains(&entries));
        let err = enforce_strict_on_document(&under_the_signed_blank_line, trusted).unwrap_err();
        assert!(
            matches!(err, PluginError::SignatureInvalid(_)),
            "the signed blank line now belongs to an embedded entry: {err}"
        );

        let with_a_comment_below = SCHEMA_MANIFEST.replacen(
            "permissions = [\"config_read\"]\n",
            &format!("permissions = [\"config_read\"]\n{entries}# Signed above\n"),
            1,
        );
        let err = enforce_strict_on_document(&with_a_comment_below, trusted).unwrap_err();
        assert!(
            matches!(err, PluginError::SignatureInvalid(_)),
            "decoration added below the entries belongs to the next signed item: {err}"
        );
    }

    #[test]
    fn signed_document_requires_valid_toml_and_a_valid_key() {
        let (pkcs8, _) = generate_test_keypair();
        assert!(matches!(
            sign_manifest_document("name = ", &pkcs8),
            Err(PluginError::InvalidManifest(_))
        ));
        assert!(matches!(
            sign_manifest_document(TEST_MANIFEST, b"not a key"),
            Err(PluginError::SignatureInvalid(_))
        ));
    }
}
