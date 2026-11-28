// src/omemo/crypto.rs
//! Cryptographic primitives for OMEMO
//!
//! This module provides the cryptographic operations needed for OMEMO encryption.

use aes_gcm::{
    Nonce,
    aead::{Aead, KeyInit, Payload},
};
use curve25519_dalek::{edwards::CompressedEdwardsY, montgomery::MontgomeryPoint, scalar::Scalar};
use hkdf::Hkdf;
use hmac::{Hmac, Mac};

use sha2::{Digest, Sha256, Sha512};
use thiserror::Error;
use x25519_dalek::{PublicKey as DalekPublicKey, StaticSecret};

use crate::xmpp_omemo::omemo::keys::{
    AesCbcKey, AesGcmKey, CbcIv, GcmNonce, Ikm, PublicKey, Salt, Secret,
};

/// Errors related to cryptographic operations
#[derive(Debug, Error)]
pub enum CryptoError {
    /// Error during AES-GCM encryption or decryption
    #[error("AES-GCM error: {0}")]
    AesGcm(String),

    /// Error during HMAC operation
    #[error("HMAC error: {0}")]
    Hmac(String),

    /// Error during KDF derivation
    #[error("KDF error: {0}")]
    Kdf(String),

    /// Invalid input data
    #[error("Invalid input: {0}")]
    InvalidInput(String),

    /// Key generation error
    #[error("Key generation error: {0}")]
    KeyGeneration(String),

    /// Invalid IV
    #[error("Invalid IV: {0}")]
    #[cfg(test)]
    InvalidIV(String),
}

/// The size of the IV in bytes for AES-GCM (96 bits)
#[cfg(test)]
pub const AES_IV_SIZE: usize = 12;

// Constants for Dino-compatible AES-GCM
pub const AES_GCM_KEY_SIZE: usize = 16; // 128-bit key for Dino compatibility
pub const AES_GCM_IV_SIZE: usize = 12; // 96-bit IV for AES-GCM

/// Generate a 16-byte AES key for Dino-compatible encryption
pub fn generate_aes_key() -> Result<Vec<u8>, CryptoError> {
    let mut key = vec![0u8; AES_GCM_KEY_SIZE];
    getrandom::fill(&mut key)
        .map_err(|_| CryptoError::KeyGeneration("OS randomness unavailable".into()))?;
    Ok(key)
}

/// Generate a 12-byte IV for AES-GCM (Dino-compatible)
pub fn generate_gcm_iv() -> Result<Vec<u8>, CryptoError> {
    let mut iv = vec![0u8; AES_GCM_IV_SIZE];
    getrandom::fill(&mut iv)
        .map_err(|_| CryptoError::KeyGeneration("OS randomness unavailable".into()))?;
    Ok(iv)
}

/// Encrypt data using AES-128-GCM (Dino-compatible format)
/// Returns ciphertext + auth_tag combined
pub fn aes_gcm_encrypt(
    plaintext: &[u8],
    key: &AesGcmKey,
    iv: &GcmNonce,
) -> Result<Vec<u8>, CryptoError> {
    aes_gcm_encrypt_with_ad(plaintext, key, iv, &[])
}

/// Encrypt data using AES-128-GCM with Associated Data (AD)
/// AD is authenticated but not encrypted — binds ciphertext to session context.
pub fn aes_gcm_encrypt_with_ad(
    plaintext: &[u8],
    key: &AesGcmKey,
    iv: &GcmNonce,
    ad: &[u8],
) -> Result<Vec<u8>, CryptoError> {
    use aes_gcm::Aes128Gcm;

    let cipher = Aes128Gcm::new_from_slice(key.as_bytes())
        .map_err(|e| CryptoError::AesGcm(format!("Failed to create AES-128-GCM cipher: {}", e)))?;

    let nonce = Nonce::from(*iv.as_bytes());

    let payload = Payload {
        msg: plaintext,
        aad: ad,
    };
    let ciphertext = cipher
        .encrypt(&nonce, payload)
        .map_err(|e| CryptoError::AesGcm(format!("AES-128-GCM encryption failed: {}", e)))?;

    Ok(ciphertext)
}

/// Decrypt data using AES-128-GCM (Dino-compatible format)
/// Expects ciphertext + auth_tag combined
pub fn aes_gcm_decrypt(
    ciphertext: &[u8],
    key: &AesGcmKey,
    iv: &GcmNonce,
) -> Result<Vec<u8>, CryptoError> {
    aes_gcm_decrypt_with_ad(ciphertext, key, iv, &[])
}

/// Decrypt data using AES-128-GCM with Associated Data (AD)
pub fn aes_gcm_decrypt_with_ad(
    ciphertext: &[u8],
    key: &AesGcmKey,
    iv: &GcmNonce,
    ad: &[u8],
) -> Result<Vec<u8>, CryptoError> {
    use aes_gcm::Aes128Gcm;

    let cipher = Aes128Gcm::new_from_slice(key.as_bytes())
        .map_err(|e| CryptoError::AesGcm(format!("Failed to create AES-128-GCM cipher: {}", e)))?;

    let nonce = Nonce::from(*iv.as_bytes());

    let payload = Payload {
        msg: ciphertext,
        aad: ad,
    };
    let plaintext = cipher
        .decrypt(&nonce, payload)
        .map_err(|e| CryptoError::AesGcm(format!("AES-128-GCM decryption failed: {}", e)))?;

    Ok(plaintext)
}

/// Encrypt using AES-256-CBC with PKCS7 padding (Signal protocol inner cipher)
pub fn aes_256_cbc_encrypt(
    key: &AesCbcKey,
    iv: &CbcIv,
    plaintext: &[u8],
) -> Result<Vec<u8>, CryptoError> {
    use cbc::cipher::{BlockModeEncrypt, KeyIvInit, block_padding::Pkcs7};
    type Aes256CbcEnc = cbc::Encryptor<aes::Aes256>;

    let cipher = Aes256CbcEnc::new_from_slices(key.as_bytes(), iv.as_bytes())
        .map_err(|e| CryptoError::AesGcm(format!("AES-256-CBC init failed: {}", e)))?;

    let mut buf = vec![0u8; plaintext.len() + 16];
    buf[..plaintext.len()].copy_from_slice(plaintext);
    let ct = cipher
        .encrypt_padded::<Pkcs7>(&mut buf, plaintext.len())
        .map_err(|_| CryptoError::AesGcm("AES-256-CBC padding failed".to_string()))?;
    Ok(ct.to_vec())
}

/// Decrypt using AES-256-CBC with PKCS7 padding (Signal protocol inner cipher)
pub fn aes_256_cbc_decrypt(
    key: &AesCbcKey,
    iv: &CbcIv,
    ciphertext: &[u8],
) -> Result<Vec<u8>, CryptoError> {
    use cbc::cipher::{BlockModeDecrypt, KeyIvInit, block_padding::Pkcs7};
    type Aes256CbcDec = cbc::Decryptor<aes::Aes256>;

    let cipher = Aes256CbcDec::new_from_slices(key.as_bytes(), iv.as_bytes())
        .map_err(|e| CryptoError::AesGcm(format!("AES-256-CBC init failed: {}", e)))?;

    let mut buf = ciphertext.to_vec();
    let pt = cipher
        .decrypt_padded::<Pkcs7>(&mut buf)
        .map_err(|_| CryptoError::AesGcm("AES-256-CBC decryption/unpadding failed".to_string()))?;
    Ok(pt.to_vec())
}

/// Validate an OMEMO initialization vector (IV)
#[cfg(test)]
pub fn validate_iv(iv: &[u8]) -> Result<(), CryptoError> {
    if iv.is_empty() {
        ::zeroclaw_log::record!(
            ERROR,
            ::zeroclaw_log::Event::new(module_path!(), ::zeroclaw_log::Action::Validate),
            "IV is empty"
        );
        return Err(CryptoError::InvalidIV("IV is empty".to_string()));
    }

    if iv.len() != AES_IV_SIZE {
        ::zeroclaw_log::record!(
            ERROR,
            ::zeroclaw_log::Event::new(module_path!(), ::zeroclaw_log::Action::Validate),
            format!(
                "Invalid IV length: {} (expected {} bytes)",
                iv.len(),
                AES_IV_SIZE
            )
        );
        return Err(CryptoError::InvalidIV(format!(
            "Invalid IV length: {} (expected {} bytes)",
            iv.len(),
            AES_IV_SIZE
        )));
    }

    Ok(())
}

/// Decrypt a message using AES-128-GCM
/// Boundary function: validates key/IV lengths before calling the typed inner function.
#[cfg(test)]
pub fn decrypt(
    ciphertext: &[u8],
    key: &[u8],
    iv: &[u8],
    associated_data: &[u8],
) -> Result<Vec<u8>, CryptoError> {
    let k = AesGcmKey::from_slice(key).ok_or_else(|| {
        CryptoError::InvalidInput(format!(
            "Invalid key size for AES-GCM: {} (expected {})",
            key.len(),
            AES_GCM_KEY_SIZE
        ))
    })?;
    let n = GcmNonce::from_slice(iv).ok_or_else(|| {
        CryptoError::InvalidIV(format!(
            "Invalid IV size for AES-GCM: {} (expected {})",
            iv.len(),
            AES_GCM_IV_SIZE
        ))
    })?;
    aes_gcm_decrypt_with_ad(ciphertext, &k, &n, associated_data)
}

/// HMAC-SHA256 for message authentication
pub fn hmac_sha256(key: &[u8], data: &[u8]) -> Result<Vec<u8>, CryptoError> {
    // Create the HMAC instance - using hmac::Mac trait's new_from_slice method
    let mut mac = <Hmac<Sha256> as Mac>::new_from_slice(key).map_err(|e| {
        ::zeroclaw_log::record!(
            ERROR,
            ::zeroclaw_log::Event::new(module_path!(), ::zeroclaw_log::Action::Validate),
            format!("Failed to create HMAC: {}", e)
        );
        CryptoError::Hmac(format!("Failed to create HMAC: {}", e))
    })?;

    // Update with the data
    mac.update(data);

    // Finalize and get the result
    let result = mac.finalize().into_bytes().to_vec();

    Ok(result)
}

/// Generate an ephemeral X25519 key pair for the X3DH key agreement
pub fn generate_x25519_keypair() -> Result<(Vec<u8>, Vec<u8>), CryptoError> {
    // Generate a secure random static secret key using OsRng for cryptographic randomness
    let mut raw = zeroize::Zeroizing::new([0u8; 32]);
    getrandom::fill(raw.as_mut())
        .map_err(|_| CryptoError::KeyGeneration("OS randomness unavailable".into()))?;
    let static_secret = StaticSecret::from(*raw);

    // Derive the public key from the secret key
    let public_key = DalekPublicKey::from(&static_secret);

    // Get the bytes
    let public_key_bytes = public_key.as_bytes().to_vec();
    let private_key_bytes = static_secret.to_bytes().to_vec();

    Ok((private_key_bytes, public_key_bytes))
}

/// Encode a Curve25519 public key to 33 bytes with 0x05 type prefix.
/// This is the format expected by libsignal (Conversations, Dino, etc.)
/// for bundle XML, PreKeySignalMessage, and SignalMessage wire formats.
pub fn encode_public_key_with_prefix(key: &[u8]) -> Vec<u8> {
    match key.len() {
        32 => {
            let mut prefixed = Vec::with_capacity(33);
            prefixed.push(0x05);
            prefixed.extend_from_slice(key);
            prefixed
        }
        33 if key[0] == 0x05 => {
            // Already has 0x05 prefix
            key.to_vec()
        }
        _ => {
            // Best effort: prepend 0x05 regardless
            let mut prefixed = Vec::with_capacity(key.len() + 1);
            prefixed.push(0x05);
            prefixed.extend_from_slice(key);
            prefixed
        }
    }
}

/// Perform a Diffie-Hellman key exchange with X25519.
/// Both arguments are typed: `private_key` must be a `Secret<32>` (access via
/// `.expose_secret()` is done here), and `public_key` must be a `PublicKey`
/// (already normalized — no 0x05-prefix stripping needed).
pub fn x25519_diffie_hellman(
    private_key: &Secret<32>,
    public_key: &PublicKey,
) -> Result<Vec<u8>, CryptoError> {
    // Reject known low-order Curve25519 points (small-subgroup attack vectors).
    // DH against these points produces a predictable (often all-zero) shared
    // secret and leaks no key material.
    const LOW_ORDER_POINTS: &[[u8; 32]] = &[
        [
            0x00, 0x00, 0x00, 0x00, 0x00, 0x00, 0x00, 0x00, 0x00, 0x00, 0x00, 0x00, 0x00, 0x00,
            0x00, 0x00, 0x00, 0x00, 0x00, 0x00, 0x00, 0x00, 0x00, 0x00, 0x00, 0x00, 0x00, 0x00,
            0x00, 0x00, 0x00, 0x00,
        ],
        [
            0x01, 0x00, 0x00, 0x00, 0x00, 0x00, 0x00, 0x00, 0x00, 0x00, 0x00, 0x00, 0x00, 0x00,
            0x00, 0x00, 0x00, 0x00, 0x00, 0x00, 0x00, 0x00, 0x00, 0x00, 0x00, 0x00, 0x00, 0x00,
            0x00, 0x00, 0x00, 0x00,
        ],
        [
            0xe0, 0xeb, 0x7a, 0x7c, 0x3b, 0x41, 0xb8, 0xae, 0x16, 0x56, 0xe3, 0xfa, 0xf1, 0x9f,
            0xc4, 0x6a, 0xda, 0x09, 0x8d, 0xeb, 0x9c, 0x32, 0xb1, 0xfd, 0x86, 0x62, 0x05, 0x16,
            0x5f, 0x49, 0xb8, 0x00,
        ],
        [
            0x5f, 0x9c, 0x95, 0xbc, 0xa3, 0x50, 0x8c, 0x24, 0xb1, 0xd0, 0xb1, 0x55, 0x9c, 0x83,
            0xef, 0x5b, 0x04, 0x44, 0x5c, 0xc4, 0x58, 0x1c, 0x8e, 0x86, 0xd8, 0x22, 0x4e, 0xdd,
            0xd0, 0x9f, 0x11, 0x57,
        ],
        [
            0xec, 0xff, 0xff, 0xff, 0xff, 0xff, 0xff, 0xff, 0xff, 0xff, 0xff, 0xff, 0xff, 0xff,
            0xff, 0xff, 0xff, 0xff, 0xff, 0xff, 0xff, 0xff, 0xff, 0xff, 0xff, 0xff, 0xff, 0xff,
            0xff, 0xff, 0xff, 0x7f,
        ],
        [
            0xed, 0xff, 0xff, 0xff, 0xff, 0xff, 0xff, 0xff, 0xff, 0xff, 0xff, 0xff, 0xff, 0xff,
            0xff, 0xff, 0xff, 0xff, 0xff, 0xff, 0xff, 0xff, 0xff, 0xff, 0xff, 0xff, 0xff, 0xff,
            0xff, 0xff, 0xff, 0x7f,
        ],
        [
            0xee, 0xff, 0xff, 0xff, 0xff, 0xff, 0xff, 0xff, 0xff, 0xff, 0xff, 0xff, 0xff, 0xff,
            0xff, 0xff, 0xff, 0xff, 0xff, 0xff, 0xff, 0xff, 0xff, 0xff, 0xff, 0xff, 0xff, 0xff,
            0xff, 0xff, 0xff, 0x7f,
        ],
        [
            0xcd, 0xeb, 0x7a, 0x7c, 0x3b, 0x41, 0xb8, 0xae, 0x16, 0x56, 0xe3, 0xfa, 0xf1, 0x9f,
            0xc4, 0x6a, 0xda, 0x09, 0x8d, 0xeb, 0x9c, 0x32, 0xb1, 0xfd, 0x86, 0x62, 0x05, 0x16,
            0x5f, 0x49, 0xb8, 0x80,
        ],
    ];
    if LOW_ORDER_POINTS.contains(public_key.as_raw()) {
        return Err(CryptoError::InvalidInput(
            "Low-order Curve25519 point rejected".to_string(),
        ));
    }

    // Create the StaticSecret from the typed private key bytes.
    let static_secret = StaticSecret::from(*private_key.expose_secret());
    let dalek_public = DalekPublicKey::from(*public_key.as_raw());

    // Compute the DH shared secret
    let shared_secret = static_secret.diffie_hellman(&dalek_public);
    let shared_bytes = shared_secret.as_bytes().to_vec();

    // Reject an all-zero shared secret — produced by low-order points not
    // caught above, or by a degenerate key.
    if shared_bytes.iter().all(|&b| b == 0) {
        return Err(CryptoError::InvalidInput(
            "All-zero DH shared secret rejected".to_string(),
        ));
    }

    Ok(shared_bytes)
}

/// Derive a key using HKDF.
/// `salt` and `ikm` are distinct types so callers cannot silently swap them.
pub fn hkdf_derive(
    salt: Salt,
    ikm: Ikm,
    info: &[u8],
    output_len: usize,
) -> Result<Vec<u8>, CryptoError> {
    let hk = Hkdf::<sha2_11::Sha256>::new(Some(salt.0), ikm.0);
    let mut okm = vec![0u8; output_len];

    if let Err(e) = hk.expand(info, &mut okm) {
        ::zeroclaw_log::record!(
            ERROR,
            ::zeroclaw_log::Event::new(module_path!(), ::zeroclaw_log::Action::Validate),
            format!("HKDF expansion failed: {}", e)
        );
        return Err(CryptoError::Kdf(format!("HKDF expansion failed: {}", e)));
    }

    Ok(okm)
}

/// XEdDSA: Sign a message using an X25519 private key.
///
/// This implements the XEdDSA signing algorithm used by Signal/OMEMO:
/// 1. Clamp the X25519 private key to get the scalar
/// 2. Compute the Edwards public key from the scalar
/// 3. If the Edwards Y coordinate's sign bit is set, negate the scalar
/// 4. Sign using Ed25519 with the adjusted scalar
///
/// This produces signatures compatible with libsignal's Curve25519.calculateSignature().
pub fn xeddsa_sign(x25519_private_key: &[u8], message: &[u8]) -> Result<Vec<u8>, CryptoError> {
    if x25519_private_key.len() != 32 {
        return Err(CryptoError::InvalidInput(format!(
            "X25519 private key must be 32 bytes, got {}",
            x25519_private_key.len()
        )));
    }

    // Step 1: Clamp the private key (X25519 clamping)
    let mut clamped = [0u8; 32];
    clamped.copy_from_slice(x25519_private_key);
    clamped[0] &= 248;
    clamped[31] &= 127;
    clamped[31] |= 64;

    // Step 2: Convert to a Scalar
    let scalar = Scalar::from_bytes_mod_order(clamped);

    // Step 3: Compute the Edwards public key (with its natural sign)
    let edwards_point = curve25519_dalek::constants::ED25519_BASEPOINT_TABLE * &scalar;
    let compressed = edwards_point.compress();
    let edwards_bytes = compressed.to_bytes();

    // Step 4: Record the sign bit (but do NOT negate the scalar)
    // libsignal-protocol-c's curve25519_verify reconstructs the Edwards point
    // from the Montgomery u-coordinate + this sign bit. The signer must use
    // the SAME Edwards point (with its natural sign) in the challenge hash.
    let sign_bit = edwards_bytes[31] & 0x80;

    // Step 5: Use the original public key (with its natural sign) for signing
    let public_bytes = edwards_bytes;

    // Step 6: Generate a random nonce (64 bytes)
    let mut random = [0u8; 64];
    getrandom::fill(&mut random)
        .map_err(|_| CryptoError::KeyGeneration("OS randomness unavailable".into()))?;

    // Step 7: Compute nonce: SHA-512(random || message)
    let mut nonce_hash = Sha512::new();
    nonce_hash.update(random);
    nonce_hash.update(message);
    let nonce_digest = nonce_hash.finalize();
    let nonce_scalar = Scalar::from_bytes_mod_order_wide(&nonce_digest.into());

    // Step 8: R = nonce_scalar * B
    let r_point = curve25519_dalek::constants::ED25519_BASEPOINT_TABLE * &nonce_scalar;
    let r_bytes = r_point.compress().to_bytes();

    // Step 9: Compute challenge: SHA-512(R || public_key || message)
    let mut challenge_hash = Sha512::new();
    challenge_hash.update(r_bytes);
    challenge_hash.update(public_bytes);
    challenge_hash.update(message);
    let challenge_digest = challenge_hash.finalize();
    let challenge = Scalar::from_bytes_mod_order_wide(&challenge_digest.into());

    // Step 10: s = nonce_scalar + challenge * scalar (using original, unnegated scalar)
    let s = nonce_scalar + challenge * scalar;

    // Step 11: Signature = R || s, with sign bit encoded in s[31] high bit
    // The verifier extracts this sign bit to reconstruct the correct Edwards point
    let mut signature = [0u8; 64];
    signature[..32].copy_from_slice(&r_bytes);
    signature[32..].copy_from_slice(&s.to_bytes());
    signature[63] &= 0x7F; // Clear high bit of s (reserved for sign bit)
    signature[63] |= sign_bit; // Encode sign bit so verifier can reconstruct A

    Ok(signature.to_vec())
}

/// XEdDSA: Verify a signature using an X25519 public key (Montgomery point).
///
/// Supports two protocol variants:
/// 1. Standard XEdDSA (convert_mont with sign=0)
/// 2. libsignal-protocol-c variant (sign bit encoded in signature[63] high bit)
///
/// Also handles identity keys published in Edwards form by converting to Montgomery first.
pub fn xeddsa_verify(
    x25519_public_key: &[u8],
    message: &[u8],
    signature: &[u8],
) -> Result<(), CryptoError> {
    if x25519_public_key.len() != 32 {
        return Err(CryptoError::InvalidInput(format!(
            "X25519 public key must be 32 bytes, got {}",
            x25519_public_key.len()
        )));
    }
    if signature.len() != 64 {
        return Err(CryptoError::InvalidInput(format!(
            "Signature must be 64 bytes, got {}",
            signature.len()
        )));
    }

    let key_array = <&[u8; 32]>::try_from(x25519_public_key).unwrap();

    // Determine the Montgomery u-coordinate for XEdDSA verification
    let montgomery = MontgomeryPoint(*key_array);
    let montgomery_key = if montgomery.to_edwards(0).is_some() || montgomery.to_edwards(1).is_some()
    {
        // Key is already in Montgomery form
        montgomery
    } else {
        // Key might be in Edwards form — convert to Montgomery
        let compressed = CompressedEdwardsY(*key_array);
        match compressed.decompress() {
            Some(edwards_point) => edwards_point.to_montgomery(),
            None => {
                return Err(CryptoError::InvalidInput(
                    "Key is neither a valid Montgomery nor Edwards point".to_string(),
                ));
            }
        }
    };

    // Extract R from signature (first 32 bytes, unchanged in both protocols)
    let r_bytes: [u8; 32] = signature[..32].try_into().unwrap();

    // Decompress R
    let r_compressed = CompressedEdwardsY(r_bytes);
    let r_point = match r_compressed.decompress() {
        Some(point) => point,
        None => {
            return Err(CryptoError::InvalidInput(
                "XEdDSA signature verification failed".to_string(),
            ));
        }
    };

    // libsignal protocol: sign bit is encoded in signature[63] high bit
    // Extract sign bit and clean s
    let sign_bit = (signature[63] & 0x80) >> 7;
    let mut s_bytes: [u8; 32] = signature[32..].try_into().unwrap();
    s_bytes[31] &= 0x7F; // Clear sign bit from s

    // Convert s to scalar
    let s = match Scalar::from_canonical_bytes(s_bytes).into() {
        Some(s) => s,
        None => {
            return Err(CryptoError::InvalidInput(
                "XEdDSA: non-canonical scalar".to_string(),
            ));
        }
    };

    // s*B (common to all attempts)
    let sb = curve25519_dalek::constants::ED25519_BASEPOINT_TABLE * &s;

    // Try verification with the sign from the signature (libsignal protocol)
    if let Some(point) = montgomery_key.to_edwards(sign_bit) {
        let public_bytes = point.compress().to_bytes();
        let mut challenge_hash = Sha512::new();
        challenge_hash.update(r_bytes);
        challenge_hash.update(public_bytes);
        challenge_hash.update(message);
        let challenge_digest = challenge_hash.finalize();
        let challenge = Scalar::from_bytes_mod_order_wide(&challenge_digest.into());
        let ca = point * challenge;
        let expected = r_point + ca;
        if sb == expected {
            return Ok(());
        }
    }

    // Try with the opposite sign (in case sign bit wasn't set by signer)
    let other_sign = 1 - sign_bit;
    if let Some(point) = montgomery_key.to_edwards(other_sign) {
        let public_bytes = point.compress().to_bytes();
        let mut challenge_hash = Sha512::new();
        challenge_hash.update(r_bytes);
        challenge_hash.update(public_bytes);
        challenge_hash.update(message);
        let challenge_digest = challenge_hash.finalize();
        let challenge = Scalar::from_bytes_mod_order_wide(&challenge_digest.into());
        let ca = point * challenge;
        let expected = r_point + ca;
        if sb == expected {
            return Ok(());
        }
    }

    // Also try with the original s (without clearing sign bit) in case the
    // signer didn't use the libsignal sign-encoding convention
    let s_orig = match Scalar::from_canonical_bytes(signature[32..].try_into().unwrap()).into() {
        Some(s) => s,
        None => {
            return Err(CryptoError::InvalidInput(
                "XEdDSA signature verification failed".to_string(),
            ));
        }
    };
    if s_orig != s {
        let sb_orig = curve25519_dalek::constants::ED25519_BASEPOINT_TABLE * &s_orig;
        for sign in [0u8, 1u8] {
            if let Some(point) = montgomery_key.to_edwards(sign) {
                let public_bytes = point.compress().to_bytes();
                let mut challenge_hash = Sha512::new();
                challenge_hash.update(r_bytes);
                challenge_hash.update(public_bytes);
                challenge_hash.update(message);
                let challenge_digest = challenge_hash.finalize();
                let challenge = Scalar::from_bytes_mod_order_wide(&challenge_digest.into());
                let ca = point * challenge;
                let expected = r_point + ca;
                if sb_orig == expected {
                    return Ok(());
                }
            }
        }
    }

    Err(CryptoError::InvalidInput(
        "XEdDSA signature verification failed".to_string(),
    ))
}

/// Returns true if two encoded Curve25519 identity keys represent *different*
/// keys. A leading `0x05` type-prefix (the 33-byte libsignal encoding) is
/// ignored, so the 32-byte and 33-byte encodings of the same key compare equal.
///
/// Used for identity-key pinning: a change here for an already-known device is a
/// possible MITM and must reset trust.
#[cfg(test)]
pub fn identity_key_changed(old: &[u8], new: &[u8]) -> bool {
    fn strip(k: &[u8]) -> &[u8] {
        if k.len() == 33 && k[0] == 0x05 {
            &k[1..]
        } else {
            k
        }
    }
    strip(old) != strip(new)
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn test_identity_key_changed() {
        let a = vec![0x11u8; 32];
        let b = vec![0x22u8; 32];

        // Same raw key → not changed.
        assert!(!identity_key_changed(&a, &a));

        // Same key, one with 0x05 prefix (33 bytes) → not changed.
        let a_prefixed = encode_public_key_with_prefix(&a);
        assert_eq!(a_prefixed.len(), 33);
        assert!(!identity_key_changed(&a, &a_prefixed));
        assert!(!identity_key_changed(&a_prefixed, &a));

        // Different keys → changed (raw and prefixed forms).
        assert!(identity_key_changed(&a, &b));
        assert!(identity_key_changed(
            &encode_public_key_with_prefix(&a),
            &encode_public_key_with_prefix(&b)
        ));
    }

    #[test]
    fn test_dino_key_montgomery_to_edwards() {
        // This is device 1019198589's identity key from storage — it's an Ed25519 key,
        // not an X25519 key. Montgomery conversion fails, but Edwards decompression works.
        let key_bytes: [u8; 32] = [
            0x25, 0x09, 0x39, 0xf2, 0x0f, 0xb4, 0xa2, 0x6c, 0x23, 0x1b, 0xc2, 0x8e, 0xf2, 0x8f,
            0xdc, 0xdc, 0xe3, 0x0a, 0xed, 0x6e, 0xef, 0x16, 0x52, 0x42, 0x63, 0xd1, 0x1a, 0xfd,
            0x36, 0xb8, 0x1c, 0x55,
        ];
        let montgomery = MontgomeryPoint(key_bytes);
        // Montgomery conversion returns None for this key
        assert!(montgomery.to_edwards(0).is_none());
        assert!(montgomery.to_edwards(1).is_none());

        // But it IS a valid Edwards point
        let compressed = CompressedEdwardsY(key_bytes);
        assert!(
            compressed.decompress().is_some(),
            "Key should be valid as Edwards point"
        );

        // And we can convert Edwards -> Montgomery -> Edwards(sign=0) for XEdDSA
        let edwards_point = compressed.decompress().unwrap();
        let montgomery_point = edwards_point.to_montgomery();
        // The Montgomery form should be convertible back to Edwards
        assert!(
            montgomery_point.to_edwards(0).is_some() || montgomery_point.to_edwards(1).is_some()
        );
    }

    #[test]
    fn test_xeddsa_verify_with_montgomery_key() {
        // Test verification with proper Montgomery key (the format used in OMEMO bundles)
        let (private_key, _) = generate_x25519_keypair().unwrap();
        let message = b"test signed prekey data";

        let signature = xeddsa_sign(&private_key, message).unwrap();

        // Derive the Montgomery public key (as it appears in OMEMO bundles)
        let public_key = {
            let secret = x25519_dalek::StaticSecret::from(
                <[u8; 32]>::try_from(private_key.as_slice()).unwrap(),
            );
            x25519_dalek::PublicKey::from(&secret).as_bytes().to_vec()
        };

        // Verify using Montgomery form of the key
        xeddsa_verify(&public_key, message, &signature)
            .expect("XEdDSA verify should succeed with Montgomery key");
    }

    #[test]
    fn test_xeddsa_verify_prefixed_message() {
        // Test with the 0x05-prefixed message format used by libsignal
        let (private_key, _) = generate_x25519_keypair().unwrap();
        let (_, spk_pub) = generate_x25519_keypair().unwrap();

        // Sign the prefixed SPK (as libsignal does)
        let mut message = vec![0x05];
        message.extend_from_slice(&spk_pub);

        let signature = xeddsa_sign(&private_key, &message).unwrap();

        // Verify with Montgomery key
        let public_key = {
            let secret = x25519_dalek::StaticSecret::from(
                <[u8; 32]>::try_from(private_key.as_slice()).unwrap(),
            );
            x25519_dalek::PublicKey::from(&secret).as_bytes().to_vec()
        };
        xeddsa_verify(&public_key, &message, &signature)
            .expect("XEdDSA verify with 0x05-prefixed SPK should succeed");
    }

    /// RFC 5869 §A.1 Test Case 1 — HKDF-SHA256
    /// Verifies the extract-then-expand key derivation against a published reference.
    #[test]
    fn hkdf_rfc5869_test_case_1() {
        let ikm = &[0x0bu8; 22];
        let salt = &[
            0x00u8, 0x01, 0x02, 0x03, 0x04, 0x05, 0x06, 0x07, 0x08, 0x09, 0x0a, 0x0b, 0x0c,
        ];
        let info = &[0xf0u8, 0xf1, 0xf2, 0xf3, 0xf4, 0xf5, 0xf6, 0xf7, 0xf8, 0xf9];
        // Expected OKM from RFC 5869 Appendix A.1
        let expected = [
            0x3c, 0xb2, 0x5f, 0x25, 0xfa, 0xac, 0xd5, 0x7a, 0x90, 0x43, 0x4f, 0x64, 0xd0, 0x36,
            0x2f, 0x2a, 0x2d, 0x2d, 0x0a, 0x90, 0xcf, 0x1a, 0x5a, 0x4c, 0x5d, 0xb0, 0x2d, 0x56,
            0xec, 0xc4, 0xc5, 0xbf, 0x34, 0x00, 0x72, 0x08, 0xd5, 0xb8, 0x87, 0x18, 0x58, 0x65,
        ];
        let okm = hkdf_derive(Salt(salt), Ikm(ikm), info, 42).unwrap();
        assert_eq!(
            &okm[..],
            &expected[..],
            "HKDF output must match RFC 5869 §A.1 vector"
        );
    }

    #[test]
    fn test_decrypt_binds_aad() {
        // crypto::decrypt must reject ciphertext authenticated under different AAD.
        // Previously the _associated_data parameter was silently discarded, so
        // decryption with wrong AAD would succeed — concealing an AEAD mismatch.
        let key = generate_aes_key().unwrap();
        let iv = generate_gcm_iv().unwrap();
        let plaintext = b"sensitive payload";
        let aad_a = b"correct context";
        let aad_b = b"wrong context";

        let aes_key = AesGcmKey::from_slice(&key).unwrap();
        let gcm_iv = GcmNonce::from_slice(&iv).unwrap();

        let ciphertext = aes_gcm_encrypt_with_ad(plaintext, &aes_key, &gcm_iv, aad_a).unwrap();

        // Correct AAD must decrypt successfully.
        assert!(decrypt(&ciphertext, &key, &iv, aad_a).is_ok());

        // Wrong AAD must fail with an authentication error.
        assert!(
            decrypt(&ciphertext, &key, &iv, aad_b).is_err(),
            "decryption with wrong AAD must fail — AAD was not bound"
        );
    }

    #[test]
    fn test_dh() {
        let (priv_a_bytes, pub_a_bytes) = generate_x25519_keypair().unwrap();
        let (priv_b_bytes, pub_b_bytes) = generate_x25519_keypair().unwrap();
        let priv_a = Secret::<32>::from_slice(&priv_a_bytes).unwrap();
        let priv_b = Secret::<32>::from_slice(&priv_b_bytes).unwrap();
        let pub_a = PublicKey::from_wire(&pub_a_bytes).unwrap();
        let pub_b = PublicKey::from_wire(&pub_b_bytes).unwrap();

        let secret_a = x25519_diffie_hellman(&priv_a, &pub_b).unwrap();
        let secret_b = x25519_diffie_hellman(&priv_b, &pub_a).unwrap();

        assert_eq!(secret_a, secret_b);
    }

    #[test]
    fn test_dh_low_order_points_rejected() {
        // All 8 small-subgroup (low-order) Curve25519 points in little-endian
        // Montgomery form.  DH against any of them leaks no key material and
        // must be rejected to prevent small-subgroup attacks.
        let low_order_points: &[[u8; 32]] = &[
            [
                0x00, 0x00, 0x00, 0x00, 0x00, 0x00, 0x00, 0x00, 0x00, 0x00, 0x00, 0x00, 0x00, 0x00,
                0x00, 0x00, 0x00, 0x00, 0x00, 0x00, 0x00, 0x00, 0x00, 0x00, 0x00, 0x00, 0x00, 0x00,
                0x00, 0x00, 0x00, 0x00,
            ],
            [
                0x01, 0x00, 0x00, 0x00, 0x00, 0x00, 0x00, 0x00, 0x00, 0x00, 0x00, 0x00, 0x00, 0x00,
                0x00, 0x00, 0x00, 0x00, 0x00, 0x00, 0x00, 0x00, 0x00, 0x00, 0x00, 0x00, 0x00, 0x00,
                0x00, 0x00, 0x00, 0x00,
            ],
            [
                0xe0, 0xeb, 0x7a, 0x7c, 0x3b, 0x41, 0xb8, 0xae, 0x16, 0x56, 0xe3, 0xfa, 0xf1, 0x9f,
                0xc4, 0x6a, 0xda, 0x09, 0x8d, 0xeb, 0x9c, 0x32, 0xb1, 0xfd, 0x86, 0x62, 0x05, 0x16,
                0x5f, 0x49, 0xb8, 0x00,
            ],
            [
                0x5f, 0x9c, 0x95, 0xbc, 0xa3, 0x50, 0x8c, 0x24, 0xb1, 0xd0, 0xb1, 0x55, 0x9c, 0x83,
                0xef, 0x5b, 0x04, 0x44, 0x5c, 0xc4, 0x58, 0x1c, 0x8e, 0x86, 0xd8, 0x22, 0x4e, 0xdd,
                0xd0, 0x9f, 0x11, 0x57,
            ],
            [
                0xec, 0xff, 0xff, 0xff, 0xff, 0xff, 0xff, 0xff, 0xff, 0xff, 0xff, 0xff, 0xff, 0xff,
                0xff, 0xff, 0xff, 0xff, 0xff, 0xff, 0xff, 0xff, 0xff, 0xff, 0xff, 0xff, 0xff, 0xff,
                0xff, 0xff, 0xff, 0x7f,
            ],
            [
                0xed, 0xff, 0xff, 0xff, 0xff, 0xff, 0xff, 0xff, 0xff, 0xff, 0xff, 0xff, 0xff, 0xff,
                0xff, 0xff, 0xff, 0xff, 0xff, 0xff, 0xff, 0xff, 0xff, 0xff, 0xff, 0xff, 0xff, 0xff,
                0xff, 0xff, 0xff, 0x7f,
            ],
            [
                0xee, 0xff, 0xff, 0xff, 0xff, 0xff, 0xff, 0xff, 0xff, 0xff, 0xff, 0xff, 0xff, 0xff,
                0xff, 0xff, 0xff, 0xff, 0xff, 0xff, 0xff, 0xff, 0xff, 0xff, 0xff, 0xff, 0xff, 0xff,
                0xff, 0xff, 0xff, 0x7f,
            ],
            [
                0xcd, 0xeb, 0x7a, 0x7c, 0x3b, 0x41, 0xb8, 0xae, 0x16, 0x56, 0xe3, 0xfa, 0xf1, 0x9f,
                0xc4, 0x6a, 0xda, 0x09, 0x8d, 0xeb, 0x9c, 0x32, 0xb1, 0xfd, 0x86, 0x62, 0x05, 0x16,
                0x5f, 0x49, 0xb8, 0x80,
            ],
        ];

        let (priv_key_bytes, _) = generate_x25519_keypair().unwrap();
        let priv_key = Secret::<32>::from_slice(&priv_key_bytes).unwrap();
        for (i, low_order) in low_order_points.iter().enumerate() {
            let result = x25519_diffie_hellman(&priv_key, &PublicKey::new(*low_order));
            assert!(result.is_err(), "low-order point {} must be rejected", i);
        }
    }

    /// RFC 4231 §4.2 Test Case 1 — HMAC-SHA256
    /// Verifies the MAC computation against a published reference.
    #[test]
    fn hmac_sha256_rfc4231_test_case_1() {
        let key = &[0x0bu8; 20];
        let data = b"Hi There";
        // Expected HMAC from RFC 4231 Section 4.2
        let expected = [
            0xb0, 0x34, 0x4c, 0x61, 0xd8, 0xdb, 0x38, 0x53, 0x5c, 0xa8, 0xaf, 0xce, 0xaf, 0x0b,
            0xf1, 0x2b, 0x88, 0x1d, 0xc2, 0x00, 0xc9, 0x83, 0x3d, 0xa7, 0x26, 0xe9, 0x37, 0x6c,
            0x2e, 0x32, 0xcf, 0xf7,
        ];
        let mac = hmac_sha256(key, data).unwrap();
        assert_eq!(
            &mac[..],
            &expected[..],
            "HMAC-SHA256 output must match RFC 4231 §4.2 vector"
        );
    }

    #[test]
    fn test_validate_iv() {
        let valid_iv = generate_gcm_iv().unwrap();
        assert!(validate_iv(&valid_iv).is_ok());

        let empty_iv: Vec<u8> = Vec::new();
        assert!(validate_iv(&empty_iv).is_err());

        let invalid_length_iv = vec![0; 16];
        assert!(validate_iv(&invalid_length_iv).is_err());
    }

    #[test]
    fn test_normalize_curve25519_public_key() {
        // PublicKey::from_wire replaces the old normalize_curve25519_public_key function.
        use crate::xmpp_omemo::omemo::keys::PublicKey;

        // 32-byte key: accepted as-is
        let key_32 = [0x01u8; 32];
        assert!(PublicKey::from_wire(&key_32).is_some());

        // 33-byte key with 0x05 prefix: strip prefix
        let mut key_33 = vec![0x05u8];
        key_33.extend_from_slice(&[0x02u8; 32]);
        assert!(PublicKey::from_wire(&key_33).is_some());
        assert_eq!(
            PublicKey::from_wire(&key_33).unwrap().as_raw(),
            &[0x02u8; 32]
        );

        // 33-byte key with wrong prefix: rejected
        let mut key_bad = vec![0x04u8];
        key_bad.extend_from_slice(&[0x03u8; 32]);
        assert!(PublicKey::from_wire(&key_bad).is_none());

        // Wrong length: rejected
        assert!(PublicKey::from_wire(&[0x01u8; 31]).is_none());
    }

    #[test]
    fn test_x25519_with_33_byte_public_key() {
        // Generate a test key pair
        let (private_key_bytes, public_key_32_bytes) = generate_x25519_keypair().unwrap();
        let private_key = Secret::<32>::from_slice(&private_key_bytes).unwrap();
        let public_key_32 = PublicKey::from_wire(&public_key_32_bytes).unwrap();

        // Create a 33-byte version with 0x05 prefix
        let mut prefixed = vec![0x05u8];
        prefixed.extend_from_slice(&public_key_32_bytes);
        let public_key_33 = PublicKey::from_wire(&prefixed).unwrap();

        // Both should produce the same result (same normalized key)
        let result_32 = x25519_diffie_hellman(&private_key, &public_key_32).unwrap();
        let result_33 = x25519_diffie_hellman(&private_key, &public_key_33).unwrap();

        assert_eq!(result_32, result_33);
    }

    #[test]
    fn test_xeddsa_sign_verify_roundtrip() {
        // Generate an X25519 key pair
        let (private_key, public_key) = generate_x25519_keypair().unwrap();
        let message = b"test message for XEdDSA";

        // Sign with private key
        let signature = xeddsa_sign(&private_key, message).unwrap();
        assert_eq!(signature.len(), 64);

        // Verify with public key
        let valid = xeddsa_verify(&public_key, message, &signature).is_ok();
        assert!(
            valid,
            "XEdDSA signature should verify with matching public key"
        );

        // Verify fails with wrong message
        let valid = xeddsa_verify(&public_key, b"wrong message", &signature).is_ok();
        assert!(
            !valid,
            "XEdDSA signature should not verify with wrong message"
        );

        // Verify fails with wrong key
        let (_, other_public) = generate_x25519_keypair().unwrap();
        let valid = xeddsa_verify(&other_public, message, &signature).is_ok();
        assert!(!valid, "XEdDSA signature should not verify with wrong key");
    }
}
