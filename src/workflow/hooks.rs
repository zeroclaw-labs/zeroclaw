// Security Hooks for Shopping Category (v3.0 Section B + Shopping)
//
// Pre/post step hooks enforced by the workflow engine when a workflow's
// parent_category is `shopping` or `phone` (or any category using them).
//
// Hook inventory:
//   consent_gate     — enforce L0/L1/L2/L3 consent before high-risk steps
//   amount_guard     — per-tx / daily / monthly caps, trip to pause
//   pii_masker       — mask PII before LLM system prompts
//   payment_trace    — audit log entry around Layer C calls (Phase 2)
//   device_integrity — Play Integrity / DeviceCheck gate (Phase 2)
//
// Phase 1 implements consent_gate + amount_guard as standalone primitives;
// Phase 2 wires them to the engine's step dispatcher.

use std::collections::HashMap;

use anyhow::{bail, Result};
use serde::{Deserialize, Serialize};

/// Consent levels per the shopping plan rev.2 §1.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "lowercase")]
pub enum ConsentLevel {
    /// Browsing / search. No confirmation needed.
    L0,
    /// Per-transaction: card last-4 + amount + biometric/OTP required.
    L1,
    /// Pre-approved routine (e.g. recurring purchase).
    L2,
    /// High-risk re-confirmation (tickets, limited items, >1M KRW).
    L3,
}

impl ConsentLevel {
    pub fn as_str(self) -> &'static str {
        match self {
            Self::L0 => "L0",
            Self::L1 => "L1",
            Self::L2 => "L2",
            Self::L3 => "L3",
        }
    }
}

/// Per-user transaction caps (KRW).
#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct AmountCaps {
    pub per_transaction: u64,
    pub daily: u64,
    pub monthly: u64,
}

impl Default for AmountCaps {
    fn default() -> Self {
        Self {
            per_transaction: 100_000,
            daily: 300_000,
            monthly: 2_000_000,
        }
    }
}

/// Cumulative spend counters (reset daily/monthly by the caller).
#[derive(Debug, Clone, Default, Serialize, Deserialize)]
pub struct SpendCounter {
    pub today: u64,
    pub this_month: u64,
}

/// Hook execution context.
#[derive(Debug, Clone)]
pub struct HookContext {
    pub caps: AmountCaps,
    pub spend: SpendCounter,
    pub consent_levels: HashMap<String, ConsentLevel>,
    pub device_trusted: bool,
    pub user_confirmed_this_run: bool,
}

impl Default for HookContext {
    fn default() -> Self {
        Self {
            caps: AmountCaps::default(),
            spend: SpendCounter::default(),
            consent_levels: HashMap::new(),
            device_trusted: true,
            user_confirmed_this_run: false,
        }
    }
}

/// Result of a hook check.
#[derive(Debug, Clone, PartialEq, Eq)]
pub enum HookDecision {
    Allow,
    RequireConfirm(String),
    Deny(String),
}

/// Security hook registry attached to the workflow ExecContext.
#[derive(Debug, Clone, Default)]
pub struct SecurityHooks;

impl SecurityHooks {
    /// consent_gate: verify that the required consent level is satisfied.
    ///
    /// L0: always allow.
    /// L1: must have user_confirmed_this_run = true.
    /// L2: must have a stored consent level L2 for the scope.
    /// L3: requires both L2 (stored) and L1 (this run) plus device_trusted.
    pub fn consent_gate(
        required: ConsentLevel,
        scope: &str,
        ctx: &HookContext,
    ) -> HookDecision {
        match required {
            ConsentLevel::L0 => HookDecision::Allow,
            ConsentLevel::L1 => {
                if ctx.user_confirmed_this_run {
                    HookDecision::Allow
                } else {
                    HookDecision::RequireConfirm(format!(
                        "L1 consent required for '{scope}'"
                    ))
                }
            }
            ConsentLevel::L2 => {
                match ctx.consent_levels.get(scope) {
                    Some(level) if *level >= ConsentLevel::L2 => HookDecision::Allow,
                    _ => HookDecision::Deny(format!(
                        "L2 pre-approval missing for '{scope}'"
                    )),
                }
            }
            ConsentLevel::L3 => {
                let has_l2 = matches!(
                    ctx.consent_levels.get(scope),
                    Some(l) if *l >= ConsentLevel::L2
                );
                if !has_l2 {
                    return HookDecision::Deny(format!(
                        "L3 requires L2 pre-approval for '{scope}'"
                    ));
                }
                if !ctx.device_trusted {
                    return HookDecision::Deny(
                        "L3 requires trusted device".to_string(),
                    );
                }
                if !ctx.user_confirmed_this_run {
                    return HookDecision::RequireConfirm(format!(
                        "L3 requires fresh per-run confirmation for '{scope}'"
                    ));
                }
                HookDecision::Allow
            }
        }
    }

    /// pii_masker: mask sensitive personal data before sending to external LLMs.
    ///
    /// Masks:
    /// - Korean residential numbers (NNNNNN-NNNNNNN)
    /// - Credit card numbers (reveals last 4 digits only)
    /// - Phone numbers (reveals last 4 digits only)
    /// - Email addresses (preserves domain, masks local part)
    pub fn pii_masker(text: &str) -> String {
        let mut out = text.to_string();

        // Order matters: specific patterns first (RRN, phone) before generic cards.
        // Korean RRN: NNNNNN-NNNNNNN → NNNNNN-*******
        out = mask_rrn(&out);
        // Phone: 010/011/070 + digits → ***-****-NNNN
        out = mask_phones(&out);
        // Credit card: 13-19 digit sequences → masked prefix + last 4
        out = mask_cards(&out);
        // Email: local@domain → l***@domain
        out = mask_emails(&out);

        out
    }

    /// payment_trace: build a structured audit entry for Layer C operations.
    /// Returns a JSON string ready for moa-bridge audit-append.
    pub fn payment_trace(
        stage: &str,
        provider: &str,
        amount: u64,
        merchant: &str,
        workflow: &str,
    ) -> String {
        serde_json::json!({
            "stage": stage,
            "provider": provider,
            "amount_krw": amount,
            "merchant": merchant,
            "workflow": workflow,
            "timestamp": std::time::SystemTime::now()
                .duration_since(std::time::UNIX_EPOCH)
                .map(|d| d.as_secs())
                .unwrap_or(0),
        })
        .to_string()
    }

    /// device_integrity: gate that requires the device to be trusted
    /// (Play Integrity / DeviceCheck result stored in HookContext).
    pub fn device_integrity(ctx: &HookContext) -> HookDecision {
        if ctx.device_trusted {
            HookDecision::Allow
        } else {
            HookDecision::Deny(
                "Device integrity check failed (Play Integrity / DeviceCheck)".to_string(),
            )
        }
    }

    /// amount_guard: verify an upcoming transaction fits within caps.
    pub fn amount_guard(amount_krw: u64, ctx: &HookContext) -> HookDecision {
        if amount_krw == 0 {
            return HookDecision::Deny("amount must be > 0".to_string());
        }
        if amount_krw > ctx.caps.per_transaction {
            return HookDecision::Deny(format!(
                "{} KRW exceeds per-transaction cap {}",
                amount_krw, ctx.caps.per_transaction
            ));
        }
        if ctx.spend.today + amount_krw > ctx.caps.daily {
            return HookDecision::Deny(format!(
                "{} KRW would exceed daily cap (today: {}, cap: {})",
                amount_krw, ctx.spend.today, ctx.caps.daily
            ));
        }
        if ctx.spend.this_month + amount_krw > ctx.caps.monthly {
            return HookDecision::Deny(format!(
                "{} KRW would exceed monthly cap (mtd: {}, cap: {})",
                amount_krw, ctx.spend.this_month, ctx.caps.monthly
            ));
        }
        HookDecision::Allow
    }
}

/// Apply a hook decision: Ok if allow, Err if deny or require confirm.
pub fn enforce(decision: HookDecision) -> Result<()> {
    match decision {
        HookDecision::Allow => Ok(()),
        HookDecision::RequireConfirm(msg) => bail!("consent required: {msg}"),
        HookDecision::Deny(msg) => bail!("denied: {msg}"),
    }
}

// ── PII masking helpers ──────────────────────────────────────────

fn mask_cards(input: &str) -> String {
    // Match 13-19 digit sequences (optionally with - or space separators)
    let mut out = String::with_capacity(input.len());
    let chars: Vec<char> = input.chars().collect();
    let mut i = 0;
    while i < chars.len() {
        // Try to match a card-like sequence
        let mut digits = String::new();
        let mut raw_len = 0;
        let mut j = i;
        while j < chars.len() && digits.len() < 20 {
            let c = chars[j];
            if c.is_ascii_digit() {
                digits.push(c);
                raw_len += 1;
            } else if (c == '-' || c == ' ') && !digits.is_empty() && raw_len < 24 {
                raw_len += 1;
            } else {
                break;
            }
            j += 1;
        }
        if digits.len() >= 13 && digits.len() <= 19 {
            // Mask: show last 4 digits only
            let masked_prefix: String = digits.chars().take(digits.len() - 4).map(|_| '*').collect();
            let last4: String = digits.chars().skip(digits.len() - 4).collect();
            out.push_str(&format!("{masked_prefix}{last4}"));
            i += raw_len;
        } else {
            out.push(chars[i]);
            i += 1;
        }
    }
    out
}

fn mask_rrn(input: &str) -> String {
    // Korean RRN: NNNNNN-NNNNNNN → NNNNNN-*******
    let mut out = String::with_capacity(input.len());
    let chars: Vec<char> = input.chars().collect();
    let mut i = 0;
    while i < chars.len() {
        if i + 14 <= chars.len() {
            let window: String = chars[i..i + 14].iter().collect();
            if is_rrn(&window) {
                out.push_str(&window[..7]);
                out.push_str("*******");
                i += 14;
                continue;
            }
        }
        out.push(chars[i]);
        i += 1;
    }
    out
}

fn is_rrn(s: &str) -> bool {
    let bytes = s.as_bytes();
    if bytes.len() != 14 {
        return false;
    }
    for (idx, b) in bytes.iter().enumerate() {
        if idx == 6 {
            if *b != b'-' {
                return false;
            }
        } else if !b.is_ascii_digit() {
            return false;
        }
    }
    true
}

fn mask_phones(input: &str) -> String {
    // 010-NNNN-NNNN or 01012345678 → ***-****-NNNN
    let mut out = String::with_capacity(input.len());
    let chars: Vec<char> = input.chars().collect();
    let mut i = 0;
    while i < chars.len() {
        // Try with separators first
        let tail = &chars[i..].iter().collect::<String>();
        if let Some((raw_len, matched)) = try_phone_match(tail) {
            // Extract digits, show last 4
            let digits: String = matched.chars().filter(|c| c.is_ascii_digit()).collect();
            if digits.len() >= 10 {
                let last4: String = digits.chars().skip(digits.len() - 4).collect();
                out.push_str(&format!("***-****-{last4}"));
                i += raw_len;
                continue;
            }
        }
        out.push(chars[i]);
        i += 1;
    }
    out
}

fn try_phone_match(tail: &str) -> Option<(usize, String)> {
    let prefixes = ["010", "011", "016", "017", "018", "019", "070"];
    for p in &prefixes {
        if tail.starts_with(p) {
            // Consume p + remaining digits with optional separators
            let mut raw_len = p.len();
            let mut matched = p.to_string();
            let chars: Vec<char> = tail.chars().skip(p.len()).collect();
            let mut digit_count = 0;
            for c in chars.iter() {
                if c.is_ascii_digit() {
                    matched.push(*c);
                    raw_len += 1;
                    digit_count += 1;
                    if digit_count >= 8 {
                        return Some((raw_len, matched));
                    }
                } else if (*c == '-' || *c == ' ') && digit_count < 8 {
                    matched.push(*c);
                    raw_len += 1;
                } else {
                    break;
                }
            }
            if digit_count >= 7 {
                return Some((raw_len, matched));
            }
        }
    }
    None
}

fn mask_emails(input: &str) -> String {
    // local@domain → l***@domain (preserves domain for routing debug)
    let mut out = String::with_capacity(input.len());
    for word in input.split_inclusive(|c: char| c.is_whitespace() || c == ',' || c == ';') {
        if let Some(at_pos) = word.find('@') {
            // Extract trailing whitespace/punctuation
            let (email_part, sep) = {
                let end = word.find(|c: char| c.is_whitespace() || c == ',' || c == ';').unwrap_or(word.len());
                (&word[..end], &word[end..])
            };
            if let Some(at) = email_part.find('@') {
                let local = &email_part[..at];
                let domain = &email_part[at..];
                if local.len() >= 2 && domain.contains('.') {
                    let first: String = local.chars().take(1).collect();
                    out.push_str(&format!("{first}***{domain}{sep}"));
                    continue;
                }
            }
            let _ = at_pos;
        }
        out.push_str(word);
    }
    out
}

// Allow ConsentLevel comparison via <, >= for hierarchy checks.
impl PartialOrd for ConsentLevel {
    fn partial_cmp(&self, other: &Self) -> Option<std::cmp::Ordering> {
        Some(self.rank().cmp(&other.rank()))
    }
}
impl Ord for ConsentLevel {
    fn cmp(&self, other: &Self) -> std::cmp::Ordering {
        self.rank().cmp(&other.rank())
    }
}
impl ConsentLevel {
    fn rank(self) -> u8 {
        match self {
            Self::L0 => 0,
            Self::L1 => 1,
            Self::L2 => 2,
            Self::L3 => 3,
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    fn ctx_base() -> HookContext {
        HookContext::default()
    }

    // ── consent_gate tests ──────────────────────────────────────

    #[test]
    fn l0_always_allowed() {
        let ctx = ctx_base();
        assert_eq!(
            SecurityHooks::consent_gate(ConsentLevel::L0, "any", &ctx),
            HookDecision::Allow
        );
    }

    #[test]
    fn l1_requires_per_run_confirm() {
        let mut ctx = ctx_base();
        ctx.user_confirmed_this_run = false;
        assert!(matches!(
            SecurityHooks::consent_gate(ConsentLevel::L1, "scope", &ctx),
            HookDecision::RequireConfirm(_)
        ));
        ctx.user_confirmed_this_run = true;
        assert_eq!(
            SecurityHooks::consent_gate(ConsentLevel::L1, "scope", &ctx),
            HookDecision::Allow
        );
    }

    #[test]
    fn l2_requires_stored_approval() {
        let mut ctx = ctx_base();
        assert!(matches!(
            SecurityHooks::consent_gate(ConsentLevel::L2, "recurring_1", &ctx),
            HookDecision::Deny(_)
        ));
        ctx.consent_levels
            .insert("recurring_1".to_string(), ConsentLevel::L2);
        assert_eq!(
            SecurityHooks::consent_gate(ConsentLevel::L2, "recurring_1", &ctx),
            HookDecision::Allow
        );
    }

    #[test]
    fn l3_requires_l2_plus_l1_plus_device() {
        let mut ctx = ctx_base();
        ctx.consent_levels
            .insert("ticket_1".to_string(), ConsentLevel::L2);
        ctx.user_confirmed_this_run = true;
        ctx.device_trusted = true;
        assert_eq!(
            SecurityHooks::consent_gate(ConsentLevel::L3, "ticket_1", &ctx),
            HookDecision::Allow
        );

        ctx.device_trusted = false;
        assert!(matches!(
            SecurityHooks::consent_gate(ConsentLevel::L3, "ticket_1", &ctx),
            HookDecision::Deny(_)
        ));
    }

    #[test]
    fn l3_denies_without_l2() {
        let mut ctx = ctx_base();
        ctx.user_confirmed_this_run = true;
        ctx.device_trusted = true;
        assert!(matches!(
            SecurityHooks::consent_gate(ConsentLevel::L3, "no_pre_approval", &ctx),
            HookDecision::Deny(_)
        ));
    }

    // ── amount_guard tests ──────────────────────────────────────

    #[test]
    fn amount_zero_denied() {
        let ctx = ctx_base();
        assert!(matches!(
            SecurityHooks::amount_guard(0, &ctx),
            HookDecision::Deny(_)
        ));
    }

    #[test]
    fn amount_within_all_caps() {
        let ctx = ctx_base();
        assert_eq!(
            SecurityHooks::amount_guard(50_000, &ctx),
            HookDecision::Allow
        );
    }

    #[test]
    fn amount_exceeds_per_transaction() {
        let ctx = ctx_base();
        assert!(matches!(
            SecurityHooks::amount_guard(200_000, &ctx),
            HookDecision::Deny(_)
        ));
    }

    #[test]
    fn amount_exceeds_daily_cumulative() {
        let mut ctx = ctx_base();
        ctx.spend.today = 250_000;
        assert!(matches!(
            SecurityHooks::amount_guard(80_000, &ctx),
            HookDecision::Deny(_)
        ));
    }

    #[test]
    fn amount_exceeds_monthly_cumulative() {
        let mut ctx = ctx_base();
        ctx.spend.this_month = 1_950_000;
        assert!(matches!(
            SecurityHooks::amount_guard(80_000, &ctx),
            HookDecision::Deny(_)
        ));
    }

    #[test]
    fn amount_custom_caps() {
        let ctx = HookContext {
            caps: AmountCaps {
                per_transaction: 10_000,
                daily: 30_000,
                monthly: 100_000,
            },
            ..Default::default()
        };
        assert_eq!(
            SecurityHooks::amount_guard(9_999, &ctx),
            HookDecision::Allow
        );
        assert!(matches!(
            SecurityHooks::amount_guard(10_001, &ctx),
            HookDecision::Deny(_)
        ));
    }

    // ── pii_masker tests ────────────────────────────────────────

    #[test]
    fn mask_credit_card_number() {
        let out = SecurityHooks::pii_masker("카드: 1234-5678-9012-3456 사용");
        assert!(!out.contains("1234-5678"));
        assert!(out.contains("3456"));
        assert!(out.contains("****"));
    }

    #[test]
    fn mask_plain_card_digits() {
        let out = SecurityHooks::pii_masker("card 1234567890123456 end");
        assert!(out.contains("3456"));
        assert!(!out.contains("1234567890123456"));
    }

    #[test]
    fn mask_rrn() {
        let out = SecurityHooks::pii_masker("주민: 901231-1234567 입니다");
        assert!(out.contains("901231-*******"));
        assert!(!out.contains("1234567"));
    }

    #[test]
    fn mask_phone_dashed() {
        let out = SecurityHooks::pii_masker("010-1234-5678 연락");
        assert!(out.contains("5678"));
        assert!(!out.contains("1234-5678"));
    }

    #[test]
    fn mask_email() {
        let out = SecurityHooks::pii_masker("contact alice@example.com pls");
        assert!(out.contains("@example.com"));
        assert!(out.contains("***"));
        assert!(!out.contains("alice@"));
    }

    #[test]
    fn mask_preserves_non_pii() {
        let out = SecurityHooks::pii_masker("Hello world, nothing sensitive");
        assert_eq!(out, "Hello world, nothing sensitive");
    }

    // ── payment_trace tests ─────────────────────────────────────

    #[test]
    fn payment_trace_returns_json() {
        let trace = SecurityHooks::payment_trace(
            "pre_invoke",
            "passkey",
            50_000,
            "coupang",
            "one_time",
        );
        let parsed: serde_json::Value = serde_json::from_str(&trace).unwrap();
        assert_eq!(parsed["stage"], "pre_invoke");
        assert_eq!(parsed["provider"], "passkey");
        assert_eq!(parsed["amount_krw"], 50_000);
        assert_eq!(parsed["merchant"], "coupang");
    }

    // ── device_integrity tests ──────────────────────────────────

    #[test]
    fn device_integrity_allows_trusted() {
        let ctx = ctx_base();
        assert_eq!(
            SecurityHooks::device_integrity(&ctx),
            HookDecision::Allow
        );
    }

    #[test]
    fn device_integrity_denies_untrusted() {
        let mut ctx = ctx_base();
        ctx.device_trusted = false;
        assert!(matches!(
            SecurityHooks::device_integrity(&ctx),
            HookDecision::Deny(_)
        ));
    }

    // ── enforce ─────────────────────────────────────────────────

    #[test]
    fn enforce_allow_is_ok() {
        assert!(enforce(HookDecision::Allow).is_ok());
    }

    #[test]
    fn enforce_deny_is_err() {
        assert!(enforce(HookDecision::Deny("test".into())).is_err());
    }

    #[test]
    fn enforce_require_is_err() {
        assert!(enforce(HookDecision::RequireConfirm("test".into())).is_err());
    }

    // ── ConsentLevel ordering ───────────────────────────────────

    #[test]
    fn consent_level_hierarchy() {
        assert!(ConsentLevel::L3 > ConsentLevel::L2);
        assert!(ConsentLevel::L2 > ConsentLevel::L1);
        assert!(ConsentLevel::L1 > ConsentLevel::L0);
    }

    #[test]
    fn consent_level_as_str() {
        assert_eq!(ConsentLevel::L0.as_str(), "L0");
        assert_eq!(ConsentLevel::L3.as_str(), "L3");
    }
}
