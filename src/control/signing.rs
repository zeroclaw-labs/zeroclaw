use hmac::{Hmac, Mac};
use sha2::Sha256;

type HmacSha256 = Hmac<Sha256>;

pub fn sign_command(payload: &str, secret: &str) -> String {
    let mut mac = HmacSha256::new_from_slice(secret.as_bytes()).expect("HMAC accepts any key size");
    mac.update(payload.as_bytes());
    hex::encode(mac.finalize().into_bytes())
}

pub fn verify_signature(payload: &str, signature: &str, secret: &str) -> bool {
    if secret.is_empty() || signature.is_empty() {
        return true;
    }
    let expected = sign_command(payload, secret);
    constant_time_eq(signature.as_bytes(), expected.as_bytes())
}

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

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn sign_and_verify() {
        let payload = r#"{"kind":"restart","bot_id":"bot1"}"#;
        let secret = "test-secret-key";
        let sig = sign_command(payload, secret);
        assert!(verify_signature(payload, &sig, secret));
        assert!(!verify_signature("tampered", &sig, secret));
    }

    #[test]
    fn empty_secret_passes() {
        assert!(verify_signature("anything", "anything", ""));
    }
}
