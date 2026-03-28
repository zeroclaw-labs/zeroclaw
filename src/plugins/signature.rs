//! Ed25519 plugin signature verification.
//!
//! Uses `ring` (already a dependency) for Ed25519 signing and verification.
//! Plugin manifests may include a base64url-encoded Ed25519 signature over
//! the canonical manifest bytes (TOML content without the `signature` field).
//! Publisher public keys are stored in the config as hex-encoded strings.

use base64::Engine;
use base64::engine::general_purpose::URL_SAFE_NO_PAD;
use ring::signature::{self, Ed25519KeyPair, KeyPair};

use super::error::PluginError;

/// Signature mode controls how unsigned/unverified plugins are handled.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum SignatureMode {
    /// Reject plugins that are unsigned or fail verification.
    Strict,
    /// Warn but allow plugins that are unsigned or fail verification.
    Permissive,
    /// Do not check signatures at all.
    Disabled,
}

impl Default for SignatureMode {
    fn default() -> Self {
        Self::Disabled
    }
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
    if s.len() % 2 != 0 {
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

// ── Canonical manifest bytes ──

/// Compute the canonical bytes of a manifest for signing/verification.
///
/// This strips the `signature` and `publisher_key` fields from the TOML content
/// and returns the remaining bytes. The stripping is line-based: any line
/// starting with `signature` or `publisher_key` followed by `=` is removed.
pub fn canonical_manifest_bytes(manifest_toml: &str) -> Vec<u8> {
    let mut lines: Vec<&str> = Vec::new();
    for line in manifest_toml.lines() {
        let trimmed = line.trim();
        if trimmed.starts_with("signature") && trimmed.contains('=') {
            continue;
        }
        if trimmed.starts_with("publisher_key") && trimmed.contains('=') {
            continue;
        }
        lines.push(line);
    }
    // Remove trailing empty lines to normalize
    while lines.last().map_or(false, |l| l.trim().is_empty()) {
        lines.pop();
    }
    let canonical = lines.join("\n");
    canonical.into_bytes()
}

// ── Signing ──

/// Sign manifest bytes with an Ed25519 private key (PKCS#8 DER).
/// Returns the base64url-encoded signature.
pub fn sign_manifest(manifest_toml: &str, pkcs8_der: &[u8]) -> Result<String, PluginError> {
    let key_pair = Ed25519KeyPair::from_pkcs8(pkcs8_der)
        .map_err(|e| PluginError::SignatureInvalid(format!("invalid signing key: {e}")))?;
    let canonical = canonical_manifest_bytes(manifest_toml);
    let sig = key_pair.sign(&canonical);
    Ok(b64u_encode(sig.as_ref()))
}

/// Get the hex-encoded public key from a PKCS#8 Ed25519 private key.
pub fn public_key_hex(pkcs8_der: &[u8]) -> Result<String, PluginError> {
    let key_pair = Ed25519KeyPair::from_pkcs8(pkcs8_der)
        .map_err(|e| PluginError::SignatureInvalid(format!("invalid signing key: {e}")))?;
    Ok(hex_encode(key_pair.public_key().as_ref()))
}

// ── Verification ──

/// Verify a plugin manifest signature against a set of trusted publisher keys.
///
/// # Arguments
/// - `manifest_toml`: The raw TOML content of the manifest file.
/// - `signature_b64`: The base64url-encoded Ed25519 signature from the manifest.
/// - `publisher_key_hex`: The hex-encoded publisher public key from the manifest.
/// - `trusted_keys`: Set of hex-encoded trusted publisher public keys from config.
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
    let canonical = canonical_manifest_bytes(manifest_toml);

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
///
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
                    tracing::warn!(
                        plugin = plugin_name,
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
                    tracing::info!(
                        plugin = plugin_name,
                        publisher_key = publisher_key.as_str(),
                        "plugin signature verified"
                    );
                    Ok(result)
                }
                VerificationResult::Untrusted => match mode {
                    SignatureMode::Strict => Err(PluginError::UntrustedPublisher {
                        plugin: plugin_name.to_string(),
                        publisher_key: pub_key.to_string(),
                    }),
                    SignatureMode::Permissive => {
                        tracing::warn!(
                            plugin = plugin_name,
                            publisher_key = pub_key,
                            "plugin publisher key not trusted; loading in permissive mode"
                        );
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
                        tracing::warn!(
                            plugin = plugin_name,
                            reason = reason.as_str(),
                            "plugin signature invalid; loading in permissive mode"
                        );
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

    fn generate_test_keypair() -> (Vec<u8>, String) {
        generate_signing_key().expect("keygen should succeed")
    }

    #[test]
    fn test_canonical_manifest_strips_signature_fields() {
        let manifest_with_sig = r#"
name = "test-plugin"
version = "0.1.0"
signature = "abc123"
publisher_key = "deadbeef"
wasm_path = "plugin.wasm"
capabilities = ["tool"]
"#;
        let canonical = canonical_manifest_bytes(manifest_with_sig);
        let canonical_str = String::from_utf8(canonical).unwrap();
        assert!(!canonical_str.contains("signature"));
        assert!(!canonical_str.contains("publisher_key"));
        assert!(canonical_str.contains("name = \"test-plugin\""));
        assert!(canonical_str.contains("wasm_path = \"plugin.wasm\""));
    }

    #[test]
    fn test_canonical_manifest_without_signature_fields() {
        let canonical = canonical_manifest_bytes(TEST_MANIFEST);
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
}
