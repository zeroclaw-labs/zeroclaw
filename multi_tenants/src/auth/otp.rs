use rand::RngExt;
use sha2::{Digest, Sha256};
use subtle::ConstantTimeEq;

/// Generate a 6-digit OTP code.
pub fn generate_otp() -> String {
    let code = rand::rng().random_range(100_000u32..1_000_000u32);
    code.to_string()
}

/// Generate a 16-byte random salt, hex-encoded.
pub fn generate_salt() -> String {
    let mut buf = [0u8; 16];
    rand::rng().fill(&mut buf);
    hex::encode(buf)
}

/// Hash an OTP: SHA-256(salt || otp), returned as hex string.
pub fn hash_otp(salt: &str, otp: &str) -> String {
    let mut hasher = Sha256::new();
    hasher.update(salt.as_bytes());
    hasher.update(otp.as_bytes());
    hex::encode(hasher.finalize())
}

/// Verify an OTP candidate against a stored hash using constant-time comparison.
pub fn verify_otp(salt: &str, candidate: &str, stored_hash: &str) -> bool {
    let candidate_hash = hash_otp(salt, candidate);
    let candidate_bytes = candidate_hash.as_bytes();
    let stored_bytes = stored_hash.as_bytes();
    candidate_bytes.ct_eq(stored_bytes).into()
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn test_otp_is_6_digits() {
        for _ in 0..20 {
            let otp = generate_otp();
            assert_eq!(otp.len(), 6, "OTP must be 6 characters");
            assert!(
                otp.chars().all(|c| c.is_ascii_digit()),
                "OTP must be all digits"
            );
        }
    }

    #[test]
    fn test_hash_verify_roundtrip() {
        let salt = generate_salt();
        let otp = generate_otp();
        let hash = hash_otp(&salt, &otp);
        assert!(
            verify_otp(&salt, &otp, &hash),
            "verify must succeed for correct otp"
        );
    }

    #[test]
    fn test_verify_wrong_code_fails() {
        let salt = generate_salt();
        let otp = generate_otp();
        let hash = hash_otp(&salt, &otp);
        // Use a clearly wrong code
        let wrong = if otp == "123456" { "654321" } else { "123456" };
        assert!(
            !verify_otp(&salt, wrong, &hash),
            "verify must fail for wrong otp"
        );
    }

    #[test]
    fn test_different_salts_produce_different_hashes() {
        let otp = "123456";
        let salt1 = generate_salt();
        let salt2 = generate_salt();
        // In the astronomically unlikely case salts are equal, skip
        if salt1 == salt2 {
            return;
        }
        let hash1 = hash_otp(&salt1, otp);
        let hash2 = hash_otp(&salt2, otp);
        assert_ne!(
            hash1, hash2,
            "different salts must produce different hashes"
        );
    }
}
