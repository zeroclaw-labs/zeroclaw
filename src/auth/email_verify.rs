//! Email-based verification code service for remote device access.
//!
//! Provides a third authentication factor: after password + device pairing code,
//! a 6-digit verification code is sent to the user's registered email.
//! The code must be confirmed within a configurable TTL (default: 5 minutes).
//!
//! ## Flow
//!
//! 1. User authenticates with username + password + device pairing code
//! 2. Server generates a 6-digit code, stores it with expiry
//! 3. Server sends code via SMTP to user's registered email
//! 4. User enters code in chat/web UI within 5 minutes
//! 5. Server validates code → grants session token
//!
//! ## Security
//!
//! - Codes are hashed (SHA-256) at rest — plaintext never stored
//! - Per-user attempt counter with lockout after max failures
//! - Codes auto-expire after TTL
//! - SMTP credentials are kept in config, never logged

use anyhow::{bail, Result};
use lettre::message::header::ContentType;
use lettre::transport::smtp::authentication::Credentials;
use lettre::{Message, SmtpTransport, Transport};
use parking_lot::Mutex;
use sha2::{Digest, Sha256};
use std::collections::HashMap;
use std::time::{SystemTime, UNIX_EPOCH};

use crate::config::schema::EmailVerificationConfig;

// ── Constants ─────────────────────────────────────────────────────

/// Default code TTL in seconds (5 minutes).
const DEFAULT_CODE_TTL_SECS: u64 = 300;

/// Default maximum verification attempts per pending code.
const DEFAULT_MAX_ATTEMPTS: u32 = 5;

// ── Types ─────────────────────────────────────────────────────────

/// A pending email verification entry.
struct PendingVerification {
    /// SHA-256 hash of the 6-digit code.
    code_hash: String,
    /// User ID this verification belongs to.
    user_id: String,
    /// Device ID being accessed.
    device_id: String,
    /// Unix timestamp when this code expires.
    expires_at: u64,
    /// Number of failed verification attempts.
    failed_attempts: u32,
    /// Maximum allowed attempts.
    max_attempts: u32,
}

/// Email verification service.
///
/// Manages pending verification codes and sends emails via SMTP.
/// Codes are stored in memory (process-scoped) — they are short-lived
/// and do not need persistence across restarts.
pub struct EmailVerifyService {
    /// SMTP configuration.
    config: EmailVerificationConfig,
    /// Pending verifications keyed by user_id.
    /// Only one pending code per user at a time (latest wins).
    pending: Mutex<HashMap<String, PendingVerification>>,
}

impl EmailVerifyService {
    /// Create a new email verification service.
    pub fn new(config: EmailVerificationConfig) -> Self {
        Self {
            config,
            pending: Mutex::new(HashMap::new()),
        }
    }

    /// Whether email verification is enabled and properly configured.
    pub fn is_enabled(&self) -> bool {
        self.config.enabled
            && self.config.smtp_host.is_some()
            && self.config.from_email.is_some()
    }

    /// Generate and send a verification code to the user's email.
    ///
    /// Returns the verification ID (user_id) for later confirmation.
    /// The plaintext code is never stored — only its hash.
    pub fn send_verification_code(
        &self,
        user_id: &str,
        device_id: &str,
        email: &str,
        username: &str,
    ) -> Result<()> {
        if !self.is_enabled() {
            bail!("Email verification is not configured");
        }

        let code = generate_verification_code();
        let code_hash = hash_code(&code);
        let ttl = if self.config.code_ttl_secs > 0 {
            self.config.code_ttl_secs
        } else {
            DEFAULT_CODE_TTL_SECS
        };
        let max_attempts = if self.config.max_attempts > 0 {
            self.config.max_attempts
        } else {
            DEFAULT_MAX_ATTEMPTS
        };
        let expires_at = epoch_secs() + ttl;

        // Store the pending verification (replaces any existing for this user)
        {
            let mut pending = self.pending.lock();
            // Cleanup expired entries opportunistically
            let now = epoch_secs();
            pending.retain(|_, v| v.expires_at > now);

            pending.insert(
                user_id.to_string(),
                PendingVerification {
                    code_hash,
                    user_id: user_id.to_string(),
                    device_id: device_id.to_string(),
                    expires_at,
                    failed_attempts: 0,
                    max_attempts,
                },
            );
        }

        // Send the email
        self.send_email(email, username, &code)?;

        tracing::info!(
            user_id = user_id,
            device_id = device_id,
            ttl_secs = ttl,
            "Email verification code sent"
        );

        Ok(())
    }

    /// Verify a code submitted by the user.
    ///
    /// Returns `Ok(device_id)` if the code is valid and not expired.
    /// Returns `Err` if the code is invalid, expired, or attempts exhausted.
    pub fn verify_code(&self, user_id: &str, code: &str) -> Result<String> {
        let mut pending = self.pending.lock();

        let entry = match pending.get_mut(user_id) {
            Some(e) => e,
            None => bail!("No pending verification. Please request a new code."),
        };

        // Check expiry
        let now = epoch_secs();
        if now > entry.expires_at {
            pending.remove(user_id);
            bail!("Verification code has expired. Please request a new code.");
        }

        // Check attempt limit
        if entry.failed_attempts >= entry.max_attempts {
            pending.remove(user_id);
            bail!(
                "Too many failed attempts. Please request a new code."
            );
        }

        // Verify code hash
        let attempt_hash = hash_code(code.trim());
        if !constant_time_eq(entry.code_hash.as_bytes(), attempt_hash.as_bytes()) {
            entry.failed_attempts += 1;
            let remaining = entry.max_attempts - entry.failed_attempts;
            if remaining == 0 {
                pending.remove(user_id);
                tracing::warn!(
                    user_id = user_id,
                    "Email verification locked out after max attempts"
                );
                bail!("Too many failed attempts. Please request a new code.");
            }
            bail!(
                "Invalid verification code. {} attempt(s) remaining.",
                remaining
            );
        }

        // Code is valid — consume it
        let device_id = entry.device_id.clone();
        pending.remove(user_id);

        tracing::info!(
            user_id = user_id,
            device_id = %device_id,
            "Email verification successful"
        );

        Ok(device_id)
    }

    /// Check if a user has a pending verification.
    pub fn has_pending(&self, user_id: &str) -> bool {
        let pending = self.pending.lock();
        if let Some(entry) = pending.get(user_id) {
            epoch_secs() <= entry.expires_at
        } else {
            false
        }
    }

    /// Cancel a pending verification for a user.
    pub fn cancel(&self, user_id: &str) {
        let mut pending = self.pending.lock();
        pending.remove(user_id);
    }

    /// Send the verification email via SMTP.
    fn send_email(&self, to_email: &str, username: &str, code: &str) -> Result<()> {
        let smtp_host = self
            .config
            .smtp_host
            .as_deref()
            .ok_or_else(|| anyhow::anyhow!("SMTP host not configured"))?;
        let from_email = self
            .config
            .from_email
            .as_deref()
            .ok_or_else(|| anyhow::anyhow!("From email not configured"))?;

        let from = format!("{} <{}>", self.config.from_name, from_email);

        let ttl_minutes = self.config.code_ttl_secs / 60;

        let body = format!(
            "안녕하세요 {username}님,\n\n\
             MoA 원격 디바이스 접속을 위한 인증코드입니다:\n\n\
             인증코드: {code}\n\n\
             이 코드는 {ttl_minutes}분간 유효합니다.\n\
             본인이 요청하지 않은 경우, 이 이메일을 무시하고 즉시 비밀번호를 변경해주세요.\n\n\
             ---\n\n\
             Hello {username},\n\n\
             Your MoA remote device access verification code is:\n\n\
             Verification Code: {code}\n\n\
             This code is valid for {ttl_minutes} minutes.\n\
             If you did not request this, please ignore this email and change your password immediately.\n\n\
             — MoA Security"
        );

        let email = Message::builder()
            .from(from.parse()?)
            .to(to_email.parse()?)
            .subject(format!(
                "[MoA] 원격접속 인증코드 / Remote Access Verification Code: {}",
                code
            ))
            .header(ContentType::TEXT_PLAIN)
            .body(body)?;

        // Build SMTP transport
        let mut transport_builder = SmtpTransport::starttls_relay(smtp_host)?;

        if let (Some(user), Some(pass)) = (
            self.config.smtp_username.as_deref(),
            self.config.smtp_password.as_deref(),
        ) {
            transport_builder =
                transport_builder.credentials(Credentials::new(user.to_string(), pass.to_string()));
        }

        let transport = transport_builder
            .port(self.config.smtp_port)
            .build();

        transport.send(&email)?;

        Ok(())
    }
}

// ── Helpers ──────────────────────────────────────────────────────

/// Generate a 6-digit verification code using CSPRNG.
fn generate_verification_code() -> String {
    const UPPER_BOUND: u32 = 1_000_000;
    const REJECT_THRESHOLD: u32 = (u32::MAX / UPPER_BOUND) * UPPER_BOUND;

    loop {
        let bytes: [u8; 4] = rand::random();
        let raw = u32::from_le_bytes(bytes);
        if raw < REJECT_THRESHOLD {
            return format!("{:06}", raw % UPPER_BOUND);
        }
    }
}

/// Hash a verification code with SHA-256.
fn hash_code(code: &str) -> String {
    let mut h = Sha256::new();
    h.update(code.trim().as_bytes());
    hex::encode(h.finalize())
}

/// Constant-time byte comparison.
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

/// Current Unix epoch in seconds.
fn epoch_secs() -> u64 {
    SystemTime::now()
        .duration_since(UNIX_EPOCH)
        .unwrap_or_default()
        .as_secs()
}

// ── Tests ────────────────────────────────────────────────────────

#[cfg(test)]
mod tests {
    use super::*;

    fn test_config() -> EmailVerificationConfig {
        EmailVerificationConfig {
            enabled: true,
            smtp_host: Some("smtp.example.com".into()),
            smtp_port: 587,
            smtp_username: Some("test_user".into()),
            smtp_password: Some("test_pass".into()),
            from_email: Some("noreply@example.com".into()),
            from_name: "MoA Test".into(),
            code_ttl_secs: 300,
            max_attempts: 5,
        }
    }

    #[test]
    fn service_is_enabled_when_configured() {
        let svc = EmailVerifyService::new(test_config());
        assert!(svc.is_enabled());
    }

    #[test]
    fn service_is_disabled_when_not_configured() {
        let mut cfg = test_config();
        cfg.enabled = false;
        let svc = EmailVerifyService::new(cfg);
        assert!(!svc.is_enabled());
    }

    #[test]
    fn service_is_disabled_without_smtp_host() {
        let mut cfg = test_config();
        cfg.smtp_host = None;
        let svc = EmailVerifyService::new(cfg);
        assert!(!svc.is_enabled());
    }

    #[test]
    fn generate_code_is_6_digits() {
        let code = generate_verification_code();
        assert_eq!(code.len(), 6);
        assert!(code.chars().all(|c| c.is_ascii_digit()));
    }

    #[test]
    fn hash_code_is_deterministic() {
        assert_eq!(hash_code("123456"), hash_code("123456"));
        assert_ne!(hash_code("123456"), hash_code("654321"));
    }

    #[test]
    fn verify_code_without_pending_fails() {
        let svc = EmailVerifyService::new(test_config());
        let result = svc.verify_code("user_a", "123456");
        assert!(result.is_err());
        assert!(result.unwrap_err().to_string().contains("No pending"));
    }

    #[test]
    fn verify_code_direct_insertion_and_verification() {
        let svc = EmailVerifyService::new(test_config());
        let code = "654321";
        let code_hash = hash_code(code);

        // Manually insert a pending verification (bypass SMTP send)
        {
            let mut pending = svc.pending.lock();
            pending.insert(
                "user_a".to_string(),
                PendingVerification {
                    code_hash,
                    user_id: "user_a".to_string(),
                    device_id: "dev_1".to_string(),
                    expires_at: epoch_secs() + 300,
                    failed_attempts: 0,
                    max_attempts: 5,
                },
            );
        }

        // Verify with correct code
        let device_id = svc.verify_code("user_a", code).unwrap();
        assert_eq!(device_id, "dev_1");

        // Code is consumed — second attempt should fail
        assert!(svc.verify_code("user_a", code).is_err());
    }

    #[test]
    fn verify_code_wrong_code_decrements_attempts() {
        let svc = EmailVerifyService::new(test_config());
        let code_hash = hash_code("111111");

        {
            let mut pending = svc.pending.lock();
            pending.insert(
                "user_b".to_string(),
                PendingVerification {
                    code_hash,
                    user_id: "user_b".to_string(),
                    device_id: "dev_2".to_string(),
                    expires_at: epoch_secs() + 300,
                    failed_attempts: 0,
                    max_attempts: 3,
                },
            );
        }

        // Wrong code — 3 attempts allowed
        let r1 = svc.verify_code("user_b", "000000");
        assert!(r1.is_err());
        assert!(r1.unwrap_err().to_string().contains("2 attempt(s)"));

        let r2 = svc.verify_code("user_b", "000000");
        assert!(r2.is_err());
        assert!(r2.unwrap_err().to_string().contains("1 attempt(s)"));

        let r3 = svc.verify_code("user_b", "000000");
        assert!(r3.is_err());
        assert!(r3.unwrap_err().to_string().contains("Too many"));

        // Subsequent attempts fail with "no pending"
        let r4 = svc.verify_code("user_b", "111111");
        assert!(r4.is_err());
        assert!(r4.unwrap_err().to_string().contains("No pending"));
    }

    #[test]
    fn verify_code_expired_fails() {
        let svc = EmailVerifyService::new(test_config());
        let code_hash = hash_code("222222");

        {
            let mut pending = svc.pending.lock();
            pending.insert(
                "user_c".to_string(),
                PendingVerification {
                    code_hash,
                    user_id: "user_c".to_string(),
                    device_id: "dev_3".to_string(),
                    expires_at: epoch_secs().saturating_sub(1), // Already expired
                    failed_attempts: 0,
                    max_attempts: 5,
                },
            );
        }

        let result = svc.verify_code("user_c", "222222");
        assert!(result.is_err());
        assert!(result.unwrap_err().to_string().contains("expired"));
    }

    #[test]
    fn has_pending_tracks_state() {
        let svc = EmailVerifyService::new(test_config());
        assert!(!svc.has_pending("user_d"));

        {
            let mut pending = svc.pending.lock();
            pending.insert(
                "user_d".to_string(),
                PendingVerification {
                    code_hash: hash_code("333333"),
                    user_id: "user_d".to_string(),
                    device_id: "dev_4".to_string(),
                    expires_at: epoch_secs() + 300,
                    failed_attempts: 0,
                    max_attempts: 5,
                },
            );
        }

        assert!(svc.has_pending("user_d"));
    }

    #[test]
    fn cancel_removes_pending() {
        let svc = EmailVerifyService::new(test_config());

        {
            let mut pending = svc.pending.lock();
            pending.insert(
                "user_e".to_string(),
                PendingVerification {
                    code_hash: hash_code("444444"),
                    user_id: "user_e".to_string(),
                    device_id: "dev_5".to_string(),
                    expires_at: epoch_secs() + 300,
                    failed_attempts: 0,
                    max_attempts: 5,
                },
            );
        }

        assert!(svc.has_pending("user_e"));
        svc.cancel("user_e");
        assert!(!svc.has_pending("user_e"));
    }

    #[test]
    fn constant_time_eq_works() {
        assert!(constant_time_eq(b"abc", b"abc"));
        assert!(!constant_time_eq(b"abc", b"abd"));
        assert!(!constant_time_eq(b"ab", b"abc"));
    }
}
