// RFC 6238 TOTP implementation.
//
// Algorithm: HMAC-SHA1 (for maximum authenticator compatibility).
// Digits: 6. Period: 30 seconds. Skew: +/- 1 step.
// Replay protection: last_used_step tracking.
// Clock drift: auto-compensation per Finding F7.

use data_encoding::BASE32_NOPAD;
use hmac::{Hmac, Mac};
use rand::Rng;
use sha1::Sha1;

type HmacSha1 = Hmac<Sha1>;

/// TOTP parameters matching RFC 6238 defaults.
pub const TOTP_DIGITS: u32 = 6;
pub const TOTP_PERIOD: u64 = 30;
pub const TOTP_SKEW: u64 = 1;
pub const TOTP_SECRET_BYTES: usize = 20; // 160 bits, RFC 4226 recommendation

/// Generate a cryptographically random TOTP secret.
/// Returns the raw bytes (20 bytes = 160 bits).
pub fn generate_secret() -> Vec<u8> {
    let mut rng = rand::thread_rng();
    let mut secret = vec![0u8; TOTP_SECRET_BYTES];
    rng.fill(&mut secret[..]);
    secret
}

/// Encode raw secret bytes to Base32 (for QR code / user display).
pub fn secret_to_base32(secret: &[u8]) -> String {
    BASE32_NOPAD.encode(secret)
}

/// Decode Base32-encoded secret back to raw bytes.
pub fn base32_to_secret(base32: &str) -> Result<Vec<u8>, TotpError> {
    // Normalize: uppercase, remove spaces
    let normalized: String = base32
        .to_uppercase()
        .chars()
        .filter(|c| !c.is_whitespace())
        .collect();
    BASE32_NOPAD
        .decode(normalized.as_bytes())
        .map_err(|_| TotpError::InvalidBase32)
}

/// Generate the TOTP code for a given secret and time.
/// Implements RFC 6238 section 4.
pub fn generate_code(secret: &[u8], time: u64) -> String {
    let step = time / TOTP_PERIOD;
    generate_code_for_step(secret, step)
}

/// Generate the TOTP code for a specific time step.
fn generate_code_for_step(secret: &[u8], step: u64) -> String {
    // Step 1: Generate HMAC-SHA1
    let mut mac = HmacSha1::new_from_slice(secret).expect("HMAC accepts any key length");
    mac.update(&step.to_be_bytes());
    let result = mac.finalize().into_bytes();

    // Step 2: Dynamic truncation (RFC 4226 section 5.4)
    let offset = (result[19] & 0x0f) as usize;
    let binary = ((result[offset] as u32 & 0x7f) << 24)
        | ((result[offset + 1] as u32) << 16)
        | ((result[offset + 2] as u32) << 8)
        | (result[offset + 3] as u32);

    // Step 3: Modulo to get the desired number of digits
    let modulus = 10u32.pow(TOTP_DIGITS);
    let code = binary % modulus;

    format!("{:0>width$}", code, width = TOTP_DIGITS as usize)
}

/// Verification result with metadata for drift tracking.
pub struct VerifyResult {
    /// Whether the code was valid.
    pub valid: bool,
    /// Which time step matched (if valid). Used for drift compensation.
    pub matched_step: Option<u64>,
    /// The expected (center) step at verification time.
    pub expected_step: u64,
    /// The drift offset (matched_step - expected_step).
    pub drift_offset: Option<i64>,
}

/// Verify a TOTP code against the secret, with skew window
/// and replay protection.
///
/// `drift_compensation`: persistent clock drift offset (Finding F7).
/// `last_used_step`: last step that was successfully verified (replay protection, D4).
pub fn verify_code(
    secret: &[u8],
    code: &str,
    time: u64,
    drift_compensation: i64,
    last_used_step: u64,
) -> VerifyResult {
    let base_step = time / TOTP_PERIOD;
    // Apply drift compensation
    let center_step = (base_step as i64 + drift_compensation) as u64;

    for offset in 0..=TOTP_SKEW {
        for &direction in &[0i64, 1, -1] {
            if offset == 0 && direction != 0 {
                continue;
            }
            let check_step = if direction >= 0 {
                center_step + offset
            } else {
                center_step.saturating_sub(offset)
            };

            // Replay protection: reject reused steps
            if check_step <= last_used_step {
                continue;
            }

            let expected_code = generate_code_for_step(secret, check_step);
            if constant_time_eq(code.as_bytes(), expected_code.as_bytes()) {
                return VerifyResult {
                    valid: true,
                    matched_step: Some(check_step),
                    expected_step: center_step,
                    drift_offset: Some(check_step as i64 - base_step as i64),
                };
            }
        }
    }

    VerifyResult {
        valid: false,
        matched_step: None,
        expected_step: center_step,
        drift_offset: None,
    }
}

/// Build an otpauth:// URI for QR code generation.
pub fn build_otpauth_uri(secret_base32: &str, issuer: &str, account: &str) -> String {
    format!(
        "otpauth://totp/{}:{}?secret={}&issuer={}&algorithm=SHA1&digits={}&period={}",
        issuer, account, secret_base32, issuer, TOTP_DIGITS, TOTP_PERIOD
    )
}

/// Constant-time comparison to prevent timing attacks.
fn constant_time_eq(a: &[u8], b: &[u8]) -> bool {
    if a.len() != b.len() {
        return false;
    }
    let mut diff = 0u8;
    for (x, y) in a.iter().zip(b.iter()) {
        diff |= x ^ y;
    }
    diff == 0
}

// ── Errors ───────────────────────────────────────────────────

#[derive(Debug, thiserror::Error)]
pub enum TotpError {
    #[error("invalid base32 encoding")]
    InvalidBase32,
    #[error("secret too short (minimum {TOTP_SECRET_BYTES} bytes)")]
    SecretTooShort,
}

#[cfg(test)]
mod tests {
    use super::*;

    // RFC 6238 Appendix B test vectors (SHA-1).
    // Secret: "12345678901234567890" (ASCII) = 20 bytes.
    const TEST_SECRET: &[u8] = b"12345678901234567890";

    #[test]
    fn rfc6238_test_vector_t59() {
        // T = 59, step = 59/30 = 1
        let code = generate_code(TEST_SECRET, 59);
        assert_eq!(code, "287082");
    }

    #[test]
    fn rfc6238_test_vector_t1111111109() {
        // T = 1111111109, step = 37037036
        let code = generate_code(TEST_SECRET, 1111111109);
        assert_eq!(code, "081804");
    }

    #[test]
    fn rfc6238_test_vector_t1111111111() {
        // T = 1111111111, step = 37037037
        let code = generate_code(TEST_SECRET, 1111111111);
        assert_eq!(code, "050471");
    }

    #[test]
    fn rfc6238_test_vector_t1234567890() {
        // T = 1234567890, step = 41152263
        let code = generate_code(TEST_SECRET, 1234567890);
        assert_eq!(code, "005924");
    }

    #[test]
    fn rfc6238_test_vector_t2000000000() {
        // T = 2000000000, step = 66666666
        let code = generate_code(TEST_SECRET, 2000000000);
        assert_eq!(code, "279037");
    }

    #[test]
    fn verify_accepts_correct_code() {
        let time = 59u64;
        let code = generate_code(TEST_SECRET, time);
        let result = verify_code(TEST_SECRET, &code, time, 0, 0);
        assert!(result.valid);
        assert!(result.matched_step.is_some());
    }

    #[test]
    fn verify_rejects_wrong_code() {
        let result = verify_code(TEST_SECRET, "000000", 59, 0, 0);
        assert!(!result.valid);
        assert!(result.matched_step.is_none());
    }

    #[test]
    fn verify_accepts_adjacent_window() {
        // Generate code for step N, verify at step N+1 (within skew=1)
        let step_n_time = 30; // step = 1
        let code = generate_code(TEST_SECRET, step_n_time);
        // Verify at step 2 (time = 60). Skew allows checking step 1.
        let result = verify_code(TEST_SECRET, &code, 60, 0, 0);
        assert!(result.valid);
    }

    #[test]
    fn verify_replay_protection() {
        let time = 59u64;
        let code = generate_code(TEST_SECRET, time);
        let step = time / TOTP_PERIOD; // step = 1

        // First verification succeeds
        let r1 = verify_code(TEST_SECRET, &code, time, 0, 0);
        assert!(r1.valid);

        // Second verification with same code fails (replay)
        let r2 = verify_code(TEST_SECRET, &code, time, 0, step);
        assert!(!r2.valid);
    }

    #[test]
    fn generate_and_verify_roundtrip() {
        let secret = generate_secret();
        let now = std::time::SystemTime::now()
            .duration_since(std::time::UNIX_EPOCH)
            .unwrap()
            .as_secs();

        let code = generate_code(&secret, now);
        assert_eq!(code.len(), TOTP_DIGITS as usize);

        let result = verify_code(&secret, &code, now, 0, 0);
        assert!(result.valid);
    }

    #[test]
    fn base32_roundtrip() {
        let secret = generate_secret();
        let base32 = secret_to_base32(&secret);
        let decoded = base32_to_secret(&base32).unwrap();
        assert_eq!(secret, decoded);
    }

    #[test]
    fn base32_handles_spaces_and_lowercase() {
        let secret = b"Hello!";
        let base32 = secret_to_base32(secret);
        // Add spaces and lowercase
        let messy = base32.to_lowercase().chars().enumerate()
            .flat_map(|(i, c)| {
                if i > 0 && i % 4 == 0 { vec![' ', c] } else { vec![c] }
            })
            .collect::<String>();
        let decoded = base32_to_secret(&messy).unwrap();
        assert_eq!(decoded, secret);
    }

    #[test]
    fn otpauth_uri_format() {
        let uri = build_otpauth_uri("JBSWY3DPEHPK3PXP", "ZeroClaw", "user@example.com");
        assert!(uri.starts_with("otpauth://totp/"));
        assert!(uri.contains("secret=JBSWY3DPEHPK3PXP"));
        assert!(uri.contains("issuer=ZeroClaw"));
        assert!(uri.contains("digits=6"));
        assert!(uri.contains("period=30"));
    }

    #[test]
    fn code_is_always_6_digits() {
        // Ensure leading zeros are preserved
        for t in [0u64, 1, 100, 1000, 99999, 1000000] {
            let code = generate_code(TEST_SECRET, t);
            assert_eq!(code.len(), 6, "Code at time {t} has wrong length: {code}");
        }
    }

    #[test]
    fn constant_time_eq_works() {
        assert!(constant_time_eq(b"123456", b"123456"));
        assert!(!constant_time_eq(b"123456", b"123457"));
        assert!(!constant_time_eq(b"123456", b"12345"));
    }
}
