use std::fmt;

use chrono::{DateTime, Utc};
use hmac::{Hmac, Mac};
use serde::{Deserialize, Serialize};
use sha2::Sha256;
use zeroize::{Zeroize, ZeroizeOnDrop};

type HmacSha256 = Hmac<Sha256>;

// ── Security levels for gating rules ─────────────────────────

/// Determines what verification is required before a command executes.
#[derive(Debug, Clone, Copy, PartialEq, Eq, PartialOrd, Ord, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum SecurityLevel {
    /// No gating — command executes freely.
    None,
    /// User must confirm ("yes/no") but no TOTP code needed.
    Confirm,
    /// TOTP code required.
    TotpRequired,
    /// TOTP code required AND explicit confirmation.
    TotpAndConfirm,
}

impl Default for SecurityLevel {
    fn default() -> Self {
        Self::None
    }
}

// ── Execution context ────────────────────────────────────────

/// Who or what is requesting the command execution.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum ExecutionContext {
    /// A human user triggered this action interactively.
    Human,
    /// A scheduled cron job triggered this action.
    Cron { job_name: String },
    /// The agent's self-healing/self-improvement triggered this.
    SelfHeal { component: String },
}

// ── Gate decision (unsigned) ─────────────────────────────────

/// The raw decision from the gate engine, before HMAC signing.
#[derive(Debug, Clone, PartialEq, Eq)]
pub enum GateDecision {
    /// Command is allowed without any verification.
    Allowed,
    /// Command requires user confirmation (no TOTP).
    ConfirmRequired { reason: String },
    /// Command requires a valid TOTP code.
    TotpRequired { reason: String },
    /// Command requires TOTP + confirmation.
    TotpAndConfirmRequired { reason: String },
    /// Command is blocked for this user's role (before TOTP check).
    Blocked { reason: String },
    /// Command is queued for later human approval (autonomous context).
    QueuedForApproval { reason: String },
}

impl GateDecision {
    /// Returns true if the command can proceed without human interaction.
    pub fn is_allowed(&self) -> bool {
        matches!(self, GateDecision::Allowed)
    }

    /// Returns the string label for audit logging.
    pub fn as_str(&self) -> &str {
        match self {
            GateDecision::Allowed => "allowed",
            GateDecision::ConfirmRequired { .. } => "confirm_required",
            GateDecision::TotpRequired { .. } => "totp_required",
            GateDecision::TotpAndConfirmRequired { .. } => "totp_and_confirm_required",
            GateDecision::Blocked { .. } => "blocked",
            GateDecision::QueuedForApproval { .. } => "queued_for_approval",
        }
    }
}

// ── Signed decision (TOCTOU protection, Finding F15) ─────────

/// A gate decision cryptographically bound to the exact command that was
/// evaluated. The execution engine MUST verify the HMAC before running
/// the command. If the command was modified between gate evaluation and
/// execution, the HMAC will not match.
#[derive(Clone)]
pub struct SignedDecision {
    pub decision: GateDecision,
    pub command: String,
    pub timestamp: DateTime<Utc>,
    pub hmac_tag: Vec<u8>,
}

impl SignedDecision {
    /// Create a new signed decision. The HMAC covers (command || timestamp).
    pub fn new(decision: GateDecision, command: &str, signing_key: &[u8]) -> Self {
        let timestamp = Utc::now();
        let mut mac =
            HmacSha256::new_from_slice(signing_key).expect("HMAC accepts any key length");
        mac.update(command.as_bytes());
        mac.update(timestamp.to_rfc3339().as_bytes());
        let tag = mac.finalize().into_bytes().to_vec();

        Self {
            decision,
            command: command.to_string(),
            timestamp,
            hmac_tag: tag,
        }
    }

    /// Verify that the command has not been modified since the gate decision.
    /// Returns true if the HMAC matches — safe to execute.
    pub fn verify(&self, command: &str, signing_key: &[u8]) -> bool {
        let mut mac =
            HmacSha256::new_from_slice(signing_key).expect("HMAC accepts any key length");
        mac.update(command.as_bytes());
        mac.update(self.timestamp.to_rfc3339().as_bytes());
        mac.verify_slice(&self.hmac_tag).is_ok()
    }
}

impl fmt::Debug for SignedDecision {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        f.debug_struct("SignedDecision")
            .field("decision", &self.decision)
            .field("command", &self.command)
            .field("timestamp", &self.timestamp)
            .field("hmac_tag", &"[REDACTED]")
            .finish()
    }
}

// ── TOTP secret (zeroized on drop) ───────────────────────────

/// The TOTP shared secret for a single user. Zeroized when dropped
/// to prevent secret leakage in memory dumps (Finding F1).
#[derive(Zeroize, ZeroizeOnDrop)]
pub struct TotpSecret {
    /// Base32-encoded secret (e.g., "JBSWY3DPEHPK3PXP").
    pub secret_base32: String,
    /// Raw bytes decoded from base32.
    pub secret_bytes: Vec<u8>,
}

// Custom Debug to prevent secret leakage in logs (Finding F5).
impl fmt::Debug for TotpSecret {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        f.debug_struct("TotpSecret")
            .field("secret_base32", &"[REDACTED]")
            .field("secret_bytes_len", &self.secret_bytes.len())
            .finish()
    }
}

// ── Recovery code ────────────────────────────────────────────

/// A single-use recovery code (Finding F2). Zeroized on drop.
#[derive(Clone, Zeroize, ZeroizeOnDrop)]
pub struct RecoveryCode {
    pub code: String,
    pub used: bool,
}

impl fmt::Debug for RecoveryCode {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        f.debug_struct("RecoveryCode")
            .field("code", &"[REDACTED]")
            .field("used", &self.used)
            .finish()
    }
}

// ── Per-user TOTP data ───────────────────────────────────────

/// All TOTP-related data for a single user. Stored encrypted on disk.
#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct UserTotpData {
    pub user_id: String,
    /// Base32-encoded TOTP secret.
    pub secret_base32: String,
    /// Whether setup has been verified (user scanned QR + entered first code).
    pub verified: bool,
    /// Last TOTP time step that was successfully used (replay protection, D4).
    pub last_used_step: u64,
    /// Recovery codes (Finding F2).
    pub recovery_codes: Vec<RecoveryCodeEntry>,
    /// Lockout state (Finding F8 — persistent).
    pub lockout: LockoutState,
    /// Clock drift compensation in time steps (Finding F7, D19).
    pub clock_drift_steps: i64,
    /// Number of consecutive verifications with the same drift offset.
    pub drift_consistency_count: u32,
    /// When the user was enrolled.
    pub enrolled_at: Option<DateTime<Utc>>,
}

impl Default for UserTotpData {
    fn default() -> Self {
        Self {
            user_id: String::new(),
            secret_base32: String::new(),
            verified: false,
            last_used_step: 0,
            recovery_codes: Vec::new(),
            lockout: LockoutState::default(),
            clock_drift_steps: 0,
            drift_consistency_count: 0,
            enrolled_at: None,
        }
    }
}

// ── Recovery code entry (serializable) ───────────────────────

#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct RecoveryCodeEntry {
    pub code_hash: String, // SHA-256 hash, not plaintext
    pub used: bool,
    pub used_at: Option<DateTime<Utc>>,
}

// ── Lockout state (persistent, Finding F8) ───────────────────

#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct LockoutState {
    pub failed_attempts: u32,
    pub locked_until: Option<DateTime<Utc>>,
    /// Session ID that caused the lockout (Finding F18 — session-bound).
    pub locked_by_session: Option<String>,
}

impl Default for LockoutState {
    fn default() -> Self {
        Self {
            failed_attempts: 0,
            locked_until: None,
            locked_by_session: None,
        }
    }
}

impl LockoutState {
    /// Returns true if the user is currently locked out.
    pub fn is_locked(&self) -> bool {
        match self.locked_until {
            Some(until) => Utc::now() < until,
            None => false,
        }
    }

    /// Record a failed attempt. Returns true if lockout threshold reached.
    pub fn record_failure(
        &mut self,
        max_attempts: u32,
        lockout_seconds: i64,
        session_id: Option<&str>,
    ) -> bool {
        self.failed_attempts += 1;
        if self.failed_attempts >= max_attempts {
            self.locked_until =
                Some(Utc::now() + chrono::Duration::seconds(lockout_seconds));
            self.locked_by_session = session_id.map(|s| s.to_string());
            true
        } else {
            false
        }
    }

    /// Reset lockout after successful verification.
    pub fn reset(&mut self) {
        self.failed_attempts = 0;
        self.locked_until = None;
        self.locked_by_session = None;
    }
}

// ── Gating rule (from config) ────────────────────────────────

/// A single rule that maps a command pattern to a security level.
#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct GatingRule {
    /// Substring pattern to match against the command string.
    pub pattern: String,
    /// Required security level when pattern matches.
    pub level: SecurityLevel,
    /// Human-readable reason displayed to the user.
    pub reason: String,
}

// ── User identity type ───────────────────────────────────────

/// How a user is identified across different channels.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct UserIdentity {
    pub identity_type: IdentityType,
    pub identifier: String,
}

#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum IdentityType {
    Pairing,
    OsUser,
    Telegram,
    Matrix,
    Web,
    Discord,
    Slack,
}

impl UserIdentity {
    /// Parse "type:identifier" format from config.
    pub fn parse(s: &str) -> Option<Self> {
        let (type_str, identifier) = s.split_once(':')?;
        let identity_type = match type_str {
            "pairing" => IdentityType::Pairing,
            "os_user" => IdentityType::OsUser,
            "telegram" => IdentityType::Telegram,
            "matrix" => IdentityType::Matrix,
            "web" => IdentityType::Web,
            "discord" => IdentityType::Discord,
            "slack" => IdentityType::Slack,
            _ => return None,
        };
        Some(Self {
            identity_type,
            identifier: identifier.to_string(),
        })
    }

    /// Serialize back to "type:identifier" format.
    pub fn to_string(&self) -> String {
        let prefix = match self.identity_type {
            IdentityType::Pairing => "pairing",
            IdentityType::OsUser => "os_user",
            IdentityType::Telegram => "telegram",
            IdentityType::Matrix => "matrix",
            IdentityType::Web => "web",
            IdentityType::Discord => "discord",
            IdentityType::Slack => "slack",
        };
        format!("{}:{}", prefix, self.identifier)
    }
}

// ── TOTP status for user records ─────────────────────────────

#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum TotpStatus {
    Inactive,
    Pending,
    Active,
}

impl Default for TotpStatus {
    fn default() -> Self {
        Self::Inactive
    }
}

// ── Audit event severity ─────────────────────────────────────

#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum AuditSeverity {
    Info,
    Warning,
    High,
    Critical,
}

// ── Audit action types ───────────────────────────────────────

#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum AuditAction {
    VerifyOk,
    VerifyFail,
    Setup,
    Reset,
    Revoke,
    ConfigReload,
    ConfigDowngradeAttempted,
    ConfigDowngradeApproved,
    BreakGlass,
    RecoveryUsed,
    LockoutTriggered,
    ApprovalQueued,
    ApprovalApproved,
    ApprovalRejected,
    ApprovalExpired,
    EStop,
}

impl AuditAction {
    pub fn severity(&self) -> AuditSeverity {
        match self {
            AuditAction::VerifyOk | AuditAction::Setup => AuditSeverity::Info,
            AuditAction::VerifyFail | AuditAction::ApprovalQueued => AuditSeverity::Warning,
            AuditAction::Reset
            | AuditAction::Revoke
            | AuditAction::ConfigReload
            | AuditAction::RecoveryUsed
            | AuditAction::ApprovalApproved
            | AuditAction::ApprovalRejected
            | AuditAction::ApprovalExpired => AuditSeverity::High,
            AuditAction::BreakGlass
            | AuditAction::LockoutTriggered
            | AuditAction::ConfigDowngradeAttempted
            | AuditAction::ConfigDowngradeApproved
            | AuditAction::EStop => AuditSeverity::Critical,
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn security_level_ordering() {
        assert!(SecurityLevel::None < SecurityLevel::Confirm);
        assert!(SecurityLevel::Confirm < SecurityLevel::TotpRequired);
        assert!(SecurityLevel::TotpRequired < SecurityLevel::TotpAndConfirm);
    }

    #[test]
    fn signed_decision_verify_unchanged_command() {
        let key = b"test-signing-key-32-bytes-long!!";
        let sd = SignedDecision::new(GateDecision::Allowed, "rm -rf /tmp/test", key);
        assert!(sd.verify("rm -rf /tmp/test", key));
    }

    #[test]
    fn signed_decision_rejects_modified_command() {
        let key = b"test-signing-key-32-bytes-long!!";
        let sd = SignedDecision::new(GateDecision::Allowed, "rm -rf /tmp/test", key);
        // Attacker changes the command between gate and execution
        assert!(!sd.verify("rm -rf /", key));
    }

    #[test]
    fn signed_decision_rejects_wrong_key() {
        let key1 = b"test-signing-key-32-bytes-long!!";
        let key2 = b"different-key-also-32-bytes-long";
        let sd = SignedDecision::new(GateDecision::Allowed, "ls -la", key1);
        assert!(!sd.verify("ls -la", key2));
    }

    #[test]
    fn totp_secret_debug_redacts() {
        let secret = TotpSecret {
            secret_base32: "JBSWY3DPEHPK3PXP".to_string(),
            secret_bytes: vec![1, 2, 3],
        };
        let debug_output = format!("{:?}", secret);
        assert!(debug_output.contains("[REDACTED]"));
        assert!(!debug_output.contains("JBSWY3DPEHPK3PXP"));
    }

    #[test]
    fn lockout_state_tracks_failures() {
        let mut lockout = LockoutState::default();
        assert!(!lockout.is_locked());
        assert!(!lockout.record_failure(3, 300, None));
        assert!(!lockout.record_failure(3, 300, None));
        assert!(lockout.record_failure(3, 300, None)); // 3rd attempt triggers lockout
        assert!(lockout.is_locked());
    }

    #[test]
    fn lockout_resets_on_success() {
        let mut lockout = LockoutState::default();
        lockout.record_failure(3, 300, None);
        lockout.record_failure(3, 300, None);
        lockout.reset();
        assert_eq!(lockout.failed_attempts, 0);
        assert!(!lockout.is_locked());
    }

    #[test]
    fn user_identity_parse_roundtrip() {
        let cases = [
            "pairing:abc123",
            "os_user:mueller",
            "telegram:123456789",
            "matrix:@mueller:kanzlei.at",
            "web:mueller@kanzlei.at",
        ];
        for input in cases {
            let parsed = UserIdentity::parse(input).unwrap();
            assert_eq!(parsed.to_string(), input);
        }
    }

    #[test]
    fn user_identity_parse_invalid() {
        assert!(UserIdentity::parse("invalid").is_none());
        assert!(UserIdentity::parse("unknown_type:id").is_none());
        assert!(UserIdentity::parse("").is_none());
    }

    #[test]
    fn gate_decision_as_str() {
        assert_eq!(GateDecision::Allowed.as_str(), "allowed");
        assert_eq!(
            GateDecision::TotpRequired {
                reason: "test".into()
            }
            .as_str(),
            "totp_required"
        );
        assert_eq!(
            GateDecision::Blocked {
                reason: "test".into()
            }
            .as_str(),
            "blocked"
        );
    }

    #[test]
    fn audit_action_severity_classification() {
        assert_eq!(AuditAction::VerifyOk.severity(), AuditSeverity::Info);
        assert_eq!(AuditAction::VerifyFail.severity(), AuditSeverity::Warning);
        assert_eq!(AuditAction::Reset.severity(), AuditSeverity::High);
        assert_eq!(AuditAction::BreakGlass.severity(), AuditSeverity::Critical);
        assert_eq!(AuditAction::EStop.severity(), AuditSeverity::Critical);
    }
}
