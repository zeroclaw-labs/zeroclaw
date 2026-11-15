//! Interactive approval workflow for supervised mode.
//! Provides a pre-execution hook that prompts the user before tool calls,
//! with session-scoped "Always" allowlists and audit logging.
//!
//! Both of those are process-local: the allow-list is keyed on a tool *name*
//! and the audit log is a `Vec`. See [`store`] for the durable half — grants
//! bound to one run, tool, and argument set, and a trail that survives a
//! restart.

pub mod store;

use crate::agent::turn::redact::{scrub_credentials, scrub_credentials_value};
use crate::security::AutonomyLevel;
use chrono::Utc;
use parking_lot::Mutex;
use serde::{Deserialize, Serialize};
use std::collections::HashSet;
#[cfg(unix)]
use std::io::BufReader;
use std::io::{self, BufRead, Write};
use std::path::Path;
use std::sync::{Arc, OnceLock};
use zeroclaw_config::schema::RiskProfileConfig;

// ── Types ────────────────────────────────────────────────────────

/// A request to approve a tool call before execution.
#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct ApprovalRequest {
    pub tool_name: String,
    pub arguments: serde_json::Value,
}

/// The user's response to an approval request.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "lowercase")]
pub enum ApprovalResponse {
    /// Execute this one call.
    Yes,
    /// Deny this call.
    No,
    /// Execute and add tool to session-scoped allowlist.
    Always,
    /// Skip execution; return this as the tool result instead.
    #[serde(rename = "replace_with")]
    ReplaceWith(String),
}

/// Maximum length of an operator-supplied `DenyWithEdit` / `ReplaceWith`
/// replacement, in bytes. The replacement is operator-authored but still
/// untrusted input that becomes a tool result fed back to the model — cap it
/// so a runaway paste can't blow up the context window.
pub const MAX_REPLACEMENT_LEN: usize = 64 * 1024;

/// Sanitize an operator-supplied tool-result replacement before it is fed back
/// to the model: drop control characters (except `\n`, `\r`, `\t`) that could
/// corrupt rendering or smuggle terminal escapes, and truncate to
/// [`MAX_REPLACEMENT_LEN`] on a char boundary.
#[must_use]
pub fn sanitize_tool_replacement(replacement: &str) -> String {
    let cleaned: String = replacement
        .chars()
        .filter(|c| !c.is_control() || matches!(c, '\n' | '\r' | '\t'))
        .collect();
    if cleaned.len() <= MAX_REPLACEMENT_LEN {
        return cleaned;
    }
    let mut end = MAX_REPLACEMENT_LEN;
    while end > 0 && !cleaned.is_char_boundary(end) {
        end -= 1;
    }
    cleaned[..end].to_string()
}

/// A single audit log entry for an approval decision.
#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct ApprovalLogEntry {
    pub timestamp: String,
    pub tool_name: String,
    pub arguments_summary: String,
    pub decision: ApprovalResponse,
    pub channel: String,
}

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum ApprovalRequirement {
    Prompt,
    Approved,
    NotRequired,
}

// ── ApprovalManager ──────────────────────────────────────────────

pub struct ApprovalManager {
    /// Tools that never need approval (from config).
    auto_approve: HashSet<String>,
    /// Tools that always need approval, ignoring session allowlist.
    always_ask: HashSet<String>,
    /// Autonomy level from config.
    autonomy_level: AutonomyLevel,
    /// When `true`, tools that would require interactive approval are
    /// auto-denied instead. Used for channel-driven (non-CLI) runs.
    non_interactive: bool,
    /// When `true`, shell calls in non-interactive mode still enter the outer
    /// approval flow because a real client approval channel exists.
    non_interactive_shell_requires_approval: bool,
    /// Session-scoped allowlist built from "Always" responses.
    session_allowlist: Mutex<HashSet<String>>,
    /// Audit trail of approval decisions. Process-local: a restart erases it.
    audit_log: Mutex<Vec<ApprovalLogEntry>>,
    /// Durable half. Production constructors attach `data_dir/approvals.db`
    /// via [`Self::with_store_at`]; open failure leaves this `None` so
    /// in-memory approval proceeds. When present, every gate outcome is also
    /// appended to a trail that survives a restart, and approvals mint a
    /// one-shot grant bound to the exact call.
    store: Option<Arc<store::ApprovalStore>>,
}

impl ApprovalManager {
    /// Create an interactive (CLI) approval manager from a risk profile.
    pub fn from_risk_profile(risk_profile: &RiskProfileConfig) -> Self {
        Self {
            auto_approve: risk_profile.auto_approve.iter().cloned().collect(),
            always_ask: risk_profile.always_ask.iter().cloned().collect(),
            autonomy_level: risk_profile.level,
            non_interactive: false,
            non_interactive_shell_requires_approval: false,
            session_allowlist: Mutex::new(HashSet::new()),
            audit_log: Mutex::new(Vec::new()),
            store: None,
        }
    }

    pub fn for_non_interactive(risk_profile: &RiskProfileConfig) -> Self {
        Self {
            auto_approve: risk_profile.auto_approve.iter().cloned().collect(),
            always_ask: risk_profile.always_ask.iter().cloned().collect(),
            autonomy_level: risk_profile.level,
            non_interactive: true,
            non_interactive_shell_requires_approval: false,
            session_allowlist: Mutex::new(HashSet::new()),
            audit_log: Mutex::new(Vec::new()),
            store: None,
        }
    }

    pub fn for_non_interactive_backchannel(risk_profile: &RiskProfileConfig) -> Self {
        Self {
            auto_approve: risk_profile.auto_approve.iter().cloned().collect(),
            always_ask: risk_profile.always_ask.iter().cloned().collect(),
            autonomy_level: risk_profile.level,
            non_interactive: true,
            non_interactive_shell_requires_approval: true,
            session_allowlist: Mutex::new(HashSet::new()),
            audit_log: Mutex::new(Vec::new()),
            store: None,
        }
    }

    /// Derive a manager for a different agent's risk profile while preserving
    /// THIS manager's interactivity mode. Used when a delegated execution (an
    /// SOP step naming a different agent) must run under the delegate agent's
    /// own approval policy without losing an operator approval route the
    /// current surface provides: an interactive parent stays interactive, a
    /// back-channel parent keeps routing shell approvals through the client
    /// channel, and a plain non-interactive parent stays auto-deny. Policy
    /// sets (`auto_approve` / `always_ask` / autonomy level) come entirely
    /// from `risk_profile`; the session allowlist and in-memory audit trail
    /// start fresh — "Always" grants to one agent never transfer to another.
    /// The durable store Arc is inherited so a delegated step shares the same
    /// `approvals.db` rather than silently dropping persistence.
    pub fn derive_for_risk_profile(&self, risk_profile: &RiskProfileConfig) -> Self {
        Self {
            auto_approve: risk_profile.auto_approve.iter().cloned().collect(),
            always_ask: risk_profile.always_ask.iter().cloned().collect(),
            autonomy_level: risk_profile.level,
            non_interactive: self.non_interactive,
            non_interactive_shell_requires_approval: self.non_interactive_shell_requires_approval,
            session_allowlist: Mutex::new(HashSet::new()),
            audit_log: Mutex::new(Vec::new()),
            store: self.store.clone(),
        }
    }

    /// Returns `true` when this manager operates in non-interactive mode
    /// (i.e. for channel-driven runs where no operator can approve).
    pub fn is_non_interactive(&self) -> bool {
        self.non_interactive
    }

    /// Check whether a tool call requires interactive approval.
    /// Returns `true` if the call needs a prompt, `false` if it can proceed.
    pub fn needs_approval(&self, tool_name: &str) -> bool {
        self.approval_requirement(tool_name) == ApprovalRequirement::Prompt
    }

    pub fn approval_requirement(&self, tool_name: &str) -> ApprovalRequirement {
        let always_ask = self.always_ask.contains("*") || self.always_ask.contains(tool_name);

        // Full autonomy skips the default prompt, but an explicit operator
        // always_ask rule remains authoritative.
        if self.autonomy_level == AutonomyLevel::Full {
            return if always_ask {
                ApprovalRequirement::Prompt
            } else {
                ApprovalRequirement::Approved
            };
        }

        // ReadOnly blocks everything — handled elsewhere; no prompt needed.
        if self.autonomy_level == AutonomyLevel::ReadOnly {
            return ApprovalRequirement::NotRequired;
        }

        // always_ask overrides every remaining approval shortcut.
        if always_ask {
            return ApprovalRequirement::Prompt;
        }

        if self.non_interactive
            && tool_name == "shell"
            && !self.non_interactive_shell_requires_approval
        {
            return ApprovalRequirement::NotRequired;
        }

        // Supervised delegation reads need no prompt unless the owner explicitly
        // requires one above. Starts and cancellations always need a fresh
        // approval, including with wildcard auto-approval or a session grant.
        match zeroclaw_policy::tachi_tool_operation(tool_name) {
            Some(zeroclaw_policy::ToolOperation::Read) => {
                return ApprovalRequirement::NotRequired;
            }
            Some(zeroclaw_policy::ToolOperation::Act) => {
                return ApprovalRequirement::Prompt;
            }
            None => {}
        }

        // auto_approve skips the prompt.
        if self.auto_approve.contains("*") || self.auto_approve.contains(tool_name) {
            return ApprovalRequirement::Approved;
        }

        // Session allowlist (from prior "Always" responses).
        let allowlist = self.session_allowlist.lock();
        if allowlist.contains(tool_name) {
            return ApprovalRequirement::Approved;
        }

        // Default: supervised mode requires approval.
        ApprovalRequirement::Prompt
    }

    /// Record an approval decision and update session state.
    pub fn record_decision(
        &self,
        tool_name: &str,
        args: &serde_json::Value,
        decision: &ApprovalResponse,
        channel: &str,
    ) {
        // If "Always", add to session allowlist.
        if *decision == ApprovalResponse::Always {
            let mut allowlist = self.session_allowlist.lock();
            allowlist.insert(tool_name.to_string());
        }

        // Append to audit log.
        let summary = summarize_args(args);
        let entry = ApprovalLogEntry {
            timestamp: Utc::now().to_rfc3339(),
            tool_name: tool_name.to_string(),
            arguments_summary: summary,
            decision: decision.clone(),
            channel: channel.to_string(),
        };
        let mut log = self.audit_log.lock();
        log.push(entry);
    }

    /// Get a snapshot of the audit log.
    pub fn audit_log(&self) -> Vec<ApprovalLogEntry> {
        self.audit_log.lock().clone()
    }

    /// Get the current session allowlist.
    pub fn session_allowlist(&self) -> HashSet<String> {
        self.session_allowlist.lock().clone()
    }

    /// Prompt the user on the CLI and return their decision.
    /// Only called for interactive (CLI) managers. Non-interactive managers
    /// auto-deny in the tool-call loop before reaching this point.
    pub fn prompt_cli(&self, request: &ApprovalRequest) -> ApprovalResponse {
        prompt_cli_interactive(request)
    }

    // ── Durable half ────────────────────────────────────────────────

    /// Attach a durable store. Without one this manager behaves exactly as
    /// before, which is the point: the store is an addition, never a
    /// precondition for the gate working.
    #[must_use]
    pub fn with_store(mut self, store: Arc<store::ApprovalStore>) -> Self {
        self.store = Some(store);
        self
    }

    /// Try to attach `data_dir/approvals.db` for this process boot.
    ///
    /// Open failure is not fatal: log WARN and keep today's in-memory
    /// proceed-without-durability path. Once a store is attached, grant or
    /// redeem write failure fails closed (`grant_and_claim_one_shot` refuses
    /// without redeeming a leftover row).
    #[must_use]
    pub fn with_store_at(self, data_dir: &Path) -> Self {
        match try_open_store(data_dir, process_boot_id()) {
            Some(store) => self.with_store(store),
            None => self,
        }
    }

    #[must_use]
    pub fn has_store(&self) -> bool {
        self.store.is_some()
    }

    /// Append one gate outcome to the durable trail.
    ///
    /// A store write failure is logged and swallowed. Refusing the tool call
    /// because an audit row could not be written would convert a bookkeeping
    /// fault into an outage; the loud log is the alarm.
    pub fn record_audit(
        &self,
        run_id: &str,
        agent: Option<&str>,
        tool_name: &str,
        args: &serde_json::Value,
        decision: store::AuditDecision,
        approver: Option<&str>,
        channel: Option<&str>,
    ) {
        let Some(store) = self.store.as_ref() else {
            return;
        };
        let summary = summarize_args(args);
        if let Err(err) = store.record(
            Some(run_id),
            agent,
            tool_name,
            args,
            &summary,
            decision,
            approver,
            channel,
        ) {
            ::zeroclaw_log::record!(
                WARN,
                ::zeroclaw_log::Event::new(module_path!(), ::zeroclaw_log::Action::Fail)
                    .with_outcome(::zeroclaw_log::EventOutcome::Failure)
                    .with_attrs(::serde_json::json!({
                        "tool": tool_name,
                        "decision": decision.as_str(),
                        "error": format!("{err}"),
                    })),
                "approval audit write failed — this decision is not on the durable trail"
            );
        }
    }

    /// Mint a one-shot grant for exactly this call. Returns the grant id when
    /// a store is attached and the write succeeds.
    pub fn grant_one_shot(
        &self,
        run_id: &str,
        tool_name: &str,
        args: &serde_json::Value,
        approver: &str,
        channel: &str,
    ) -> Option<String> {
        let store = self.store.as_ref()?;
        match store.grant(
            run_id,
            tool_name,
            args,
            approver,
            channel,
            chrono::Duration::seconds(store::DEFAULT_GRANT_TTL_SECS),
        ) {
            Ok(grant) => Some(grant.approval_id),
            Err(err) => {
                ::zeroclaw_log::record!(
                    WARN,
                    ::zeroclaw_log::Event::new(module_path!(), ::zeroclaw_log::Action::Fail)
                        .with_outcome(::zeroclaw_log::EventOutcome::Failure)
                        .with_attrs(::serde_json::json!({
                            "tool": tool_name,
                            "error": format!("{err}"),
                        })),
                    "could not persist approval grant"
                );
                None
            }
        }
    }

    /// Consume the grant covering this exact call.
    ///
    /// `Ok(())` when there is no store — a caller without durable approvals
    /// must not start failing closed on a feature it never enabled. With a
    /// store, an approval covers one execution of one argument set.
    pub fn redeem_one_shot(
        &self,
        run_id: &str,
        tool_name: &str,
        args: &serde_json::Value,
    ) -> Result<(), store::RedeemFailure> {
        let Some(store) = self.store.as_ref() else {
            return Ok(());
        };
        match store.redeem(run_id, tool_name, args) {
            Ok(Ok(_)) => Ok(()),
            Ok(Err(failure)) => Err(failure),
            Err(err) => {
                ::zeroclaw_log::record!(
                    WARN,
                    ::zeroclaw_log::Event::new(module_path!(), ::zeroclaw_log::Action::Fail)
                        .with_outcome(::zeroclaw_log::EventOutcome::Failure)
                        .with_attrs(::serde_json::json!({
                            "tool": tool_name,
                            "error": format!("{err}"),
                        })),
                    "approval grant lookup failed — treating as no grant"
                );
                Err(store::RedeemFailure::NoGrant)
            }
        }
    }

    /// Persist a one-shot grant for this exact call and claim it.
    ///
    /// When a store is attached, a grant write failure refuses immediately
    /// without calling redeem. Otherwise a leftover unconsumed row for the
    /// same boot/run/tool/args_hash could be spent as if this approval had
    /// persisted.
    pub fn grant_and_claim_one_shot(
        &self,
        run_id: &str,
        tool_name: &str,
        args: &serde_json::Value,
        approver: &str,
        channel: &str,
    ) -> Result<(), store::RedeemFailure> {
        if self.has_store()
            && self
                .grant_one_shot(run_id, tool_name, args, approver, channel)
                .is_none()
        {
            return Err(store::RedeemFailure::NoGrant);
        }
        self.redeem_one_shot(run_id, tool_name, args)
    }
}

/// Resolve the local_tool boot id, freezing the first answer for the process.
///
/// The boot id is a process-local UUID. It was homologous with the durable
/// control-plane's boot id until Wall 4 (issue 197) retired that plane: durable
/// execution truth moved to Tachi (frozen contract annex rows 1 and 6), and the grant
/// namespace never needed the homology — what it needs is one stable id per
/// process so independently opened managers redeem each other's rows.
///
/// The `OnceLock` freeze stays: gateway and channel construction can attach
/// an approval store at different moments of the same process, and flipping
/// boot_id mid-process would split one process across two grant namespaces.
fn resolve_boot_id(slot: &OnceLock<String>) -> String {
    slot.get_or_init(|| uuid::Uuid::new_v4().to_string())
        .clone()
}

fn process_boot_id() -> String {
    static BOOT_ID: OnceLock<String> = OnceLock::new();
    resolve_boot_id(&BOOT_ID)
}

/// Open `data_dir/approvals.db`. Failure returns `None` so callers keep the
/// in-memory approval path rather than refusing to start.
fn try_open_store(
    data_dir: &Path,
    boot_id: impl Into<String>,
) -> Option<Arc<store::ApprovalStore>> {
    match store::ApprovalStore::open(data_dir, boot_id) {
        Ok(store) => Some(Arc::new(store)),
        Err(err) => {
            ::zeroclaw_log::record!(
                WARN,
                ::zeroclaw_log::Event::new(module_path!(), ::zeroclaw_log::Action::Fail)
                    .with_outcome(::zeroclaw_log::EventOutcome::Failure)
                    .with_attrs(::serde_json::json!({
                        "path": data_dir.display().to_string(),
                        "error": format!("{err}"),
                    })),
                "approval store open failed — continuing without durable grants"
            );
            None
        }
    }
}

#[cfg(test)]
mod approval_precedence_tests {
    use super::{ApprovalManager, ApprovalRequirement, ApprovalResponse, AutonomyLevel};
    use parking_lot::Mutex;
    use serde_json::json;
    use std::collections::HashSet;

    fn manager(
        autonomy_level: AutonomyLevel,
        always_ask: &[&str],
        auto_approve: &[&str],
    ) -> ApprovalManager {
        ApprovalManager {
            auto_approve: auto_approve
                .iter()
                .map(|tool| (*tool).to_string())
                .collect(),
            always_ask: always_ask.iter().map(|tool| (*tool).to_string()).collect(),
            autonomy_level,
            non_interactive: false,
            non_interactive_shell_requires_approval: false,
            session_allowlist: Mutex::new(HashSet::new()),
            audit_log: Mutex::new(Vec::new()),
            store: None,
        }
    }

    #[test]
    fn supervised_tachi_reads_and_side_effects_follow_policy() {
        for auto_approve in [vec![], vec!["*"]] {
            let mgr = manager(AutonomyLevel::Supervised, &[], &auto_approve);
            for name in ["tachi_status", "tachi_result", "tachi_watch"] {
                assert_eq!(
                    mgr.approval_requirement(name),
                    ApprovalRequirement::NotRequired
                );
            }
            for name in ["tachi_start", "tachi_cancel"] {
                mgr.session_allowlist.lock().insert(name.into());
                assert_eq!(mgr.approval_requirement(name), ApprovalRequirement::Prompt);
            }
        }
        for rule in ["tachi_status", "*"] {
            let mgr = manager(AutonomyLevel::Supervised, &[rule], &["*"]);
            assert_eq!(
                mgr.approval_requirement("tachi_status"),
                ApprovalRequirement::Prompt
            );
        }
        let mgr = manager(AutonomyLevel::Supervised, &[], &[]);
        for name in ["", "tachi_staff", "tachi_start_extra"] {
            assert_eq!(mgr.approval_requirement(name), ApprovalRequirement::Prompt);
        }
    }

    #[test]
    fn full_autonomy_prompts_for_exact_always_ask_tool() {
        let manager = manager(AutonomyLevel::Full, &["shell"], &[]);

        assert_eq!(
            manager.approval_requirement("shell"),
            ApprovalRequirement::Prompt
        );
        assert!(manager.needs_approval("shell"));
    }

    #[test]
    fn full_autonomy_prompts_for_wildcard_always_ask() {
        let manager = manager(AutonomyLevel::Full, &["*"], &[]);

        for tool_name in ["shell", "file_write", "http_request"] {
            assert_eq!(
                manager.approval_requirement(tool_name),
                ApprovalRequirement::Prompt,
                "wildcard always_ask must cover {tool_name}"
            );
        }
    }

    #[test]
    fn full_autonomy_approves_tool_not_covered_by_always_ask() {
        let manager = manager(AutonomyLevel::Full, &["shell"], &[]);

        assert_eq!(
            manager.approval_requirement("file_read"),
            ApprovalRequirement::Approved
        );
    }

    #[test]
    fn full_autonomy_always_ask_overrides_auto_approve_and_session_allowlist() {
        let manager = manager(AutonomyLevel::Full, &["shell"], &["shell"]);
        manager.record_decision(
            "shell",
            &json!({"command": "pwd"}),
            &ApprovalResponse::Always,
            "test",
        );

        assert_eq!(
            manager.approval_requirement("shell"),
            ApprovalRequirement::Prompt
        );
    }

    #[test]
    fn read_only_behavior_is_unchanged_when_always_ask_matches() {
        let manager = manager(AutonomyLevel::ReadOnly, &["*"], &[]);

        assert_eq!(
            manager.approval_requirement("shell"),
            ApprovalRequirement::NotRequired
        );
        assert!(!manager.needs_approval("shell"));
    }

    // ── The unattended path ──────────────────────────────────────────
    //
    // A cron `JobType::Agent` run and a channel-driven turn both execute
    // with nobody watching. On that path `ApprovalManager::for_non_interactive`
    // sets `non_interactive_shell_requires_approval: false`, which drops
    // `shell` to `NotRequired` — no approval at all. These tests pin the two
    // things that keep a scheduled trading agent from reaching a shell at
    // 03:00: `always_ask` outranking that bypass, and outranking Full
    // autonomy above it.

    fn unattended(
        autonomy_level: AutonomyLevel,
        always_ask: &[&str],
        auto_approve: &[&str],
    ) -> ApprovalManager {
        ApprovalManager {
            auto_approve: auto_approve
                .iter()
                .map(|tool| (*tool).to_string())
                .collect(),
            always_ask: always_ask.iter().map(|tool| (*tool).to_string()).collect(),
            autonomy_level,
            non_interactive: true,
            // Exactly what `for_non_interactive` builds.
            non_interactive_shell_requires_approval: false,
            session_allowlist: Mutex::new(HashSet::new()),
            audit_log: Mutex::new(Vec::new()),
            store: None,
        }
    }

    /// Without `always_ask`, the unattended path really does hand out `shell`
    /// with no approval. This is the hazard the next test closes — asserting
    /// it here keeps the pair honest: if upstream ever fixes this default,
    /// this test fails and tells us the backstop is no longer load-bearing.
    #[test]
    fn unattended_shell_is_ungated_without_always_ask() {
        let manager = unattended(AutonomyLevel::Supervised, &[], &[]);

        assert_eq!(
            manager.approval_requirement("shell"),
            ApprovalRequirement::NotRequired,
            "documents the hazard: non_interactive drops shell to NotRequired"
        );
    }

    /// The backstop. `always_ask` is consulted before the non-interactive
    /// shell bypass, so a profile that lists `shell` still forces approval —
    /// which, with no operator present, is a denial rather than a free shell.
    #[test]
    fn unattended_always_ask_outranks_the_non_interactive_shell_bypass() {
        let manager = unattended(AutonomyLevel::Supervised, &["shell"], &[]);

        assert_eq!(
            manager.approval_requirement("shell"),
            ApprovalRequirement::Prompt,
            "always_ask must be checked before the non_interactive bypass"
        );
    }

    /// Both hazards at once: a profile mis-set to Full autonomy running
    /// unattended. Upstream returns `Approved` here before ever looking at
    /// `always_ask`; this fork looks first.
    #[test]
    fn unattended_full_autonomy_still_honors_always_ask() {
        let manager = unattended(AutonomyLevel::Full, &["shell", "file_write"], &[]);

        for tool_name in ["shell", "file_write"] {
            assert_eq!(
                manager.approval_requirement(tool_name),
                ApprovalRequirement::Prompt,
                "{tool_name} must still require approval under Full + unattended"
            );
        }
    }

    // ── The durable half, as the gate uses it ────────────────────────

    fn with_store(manager: ApprovalManager, dir: &std::path::Path) -> ApprovalManager {
        let store = super::store::ApprovalStore::open(dir, "boot-1").expect("store opens");
        manager.with_store(std::sync::Arc::new(store))
    }

    /// A manager with no store must behave exactly as before. The durable
    /// half is an addition; a caller that never enabled it must not start
    /// failing closed on it.
    #[test]
    fn without_a_store_redemption_is_a_no_op() {
        let manager = manager(AutonomyLevel::Supervised, &["shell"], &[]);
        assert!(!manager.has_store());
        assert!(
            manager
                .redeem_one_shot("run-1", "shell", &json!({"command": "ls"}))
                .is_ok(),
            "no store must mean no new refusals"
        );
    }

    /// The gate's grant-then-redeem round trip: it succeeds for the call that
    /// was approved.
    #[test]
    fn a_grant_redeems_for_the_call_it_was_issued_for() {
        let dir = tempfile::tempdir().unwrap();
        let manager = with_store(
            manager(AutonomyLevel::Supervised, &["shell"], &[]),
            dir.path(),
        );
        let args = json!({"command": "ls"});

        manager.grant_one_shot("run-1", "shell", &args, "owner", "cli");
        assert!(manager.redeem_one_shot("run-1", "shell", &args).is_ok());
    }

    /// The point of the round trip. If the arguments change between the
    /// approval and the execution, the approval does not cover the call —
    /// an approved `ls` must not become an approved `rm -rf /`.
    #[test]
    fn a_grant_does_not_redeem_for_mutated_arguments() {
        let dir = tempfile::tempdir().unwrap();
        let manager = with_store(
            manager(AutonomyLevel::Supervised, &["shell"], &[]),
            dir.path(),
        );

        manager.grant_one_shot("run-1", "shell", &json!({"command": "ls"}), "owner", "cli");

        assert_eq!(
            manager.redeem_one_shot("run-1", "shell", &json!({"command": "rm -rf /"})),
            Err(super::store::RedeemFailure::NoGrant),
            "an approval must not carry over to different arguments"
        );
    }

    /// One approval, one execution. A second identical call in the same run
    /// needs its own approval rather than riding the first.
    #[test]
    fn a_grant_does_not_cover_a_repeat_of_the_same_call() {
        let dir = tempfile::tempdir().unwrap();
        let manager = with_store(
            manager(AutonomyLevel::Supervised, &["shell"], &[]),
            dir.path(),
        );
        let args = json!({"command": "ls"});

        manager.grant_one_shot("run-1", "shell", &args, "owner", "cli");
        assert!(manager.redeem_one_shot("run-1", "shell", &args).is_ok());
        assert_eq!(
            manager.redeem_one_shot("run-1", "shell", &args),
            Err(super::store::RedeemFailure::AlreadyConsumed)
        );
    }

    /// An unattended auto-approval reaches the durable trail even though no
    /// human ever saw it. This is the row that answers "who approved the
    /// 03:00 call" with "nobody, and here is the proof".
    #[test]
    fn an_unattended_decision_reaches_the_durable_trail() {
        let dir = tempfile::tempdir().unwrap();
        let manager = with_store(unattended(AutonomyLevel::Full, &[], &[]), dir.path());

        manager.record_audit(
            "run-1",
            Some("trader"),
            "shell",
            &json!({"command": "ls"}),
            super::store::AuditDecision::NotRequired,
            None,
            Some("cron"),
        );

        let store = super::store::ApprovalStore::open(dir.path(), "boot-2").unwrap();
        let rows = store.audit_for_run("run-1").unwrap();
        assert_eq!(rows.len(), 1);
        assert_eq!(rows[0].2, "not_required");
    }

    /// Production constructors attach via `with_store_at`. A grant must
    /// round-trip, land on `approval_audit`, and still be there after a new
    /// manager opens the same `data_dir`.
    #[test]
    fn with_store_at_round_trips_and_audit_survives_a_new_manager() {
        let dir = tempfile::tempdir().unwrap();
        let args = json!({"command": "ls"});
        let first = manager(AutonomyLevel::Supervised, &["shell"], &[]).with_store_at(dir.path());
        assert!(
            first.has_store(),
            "a writable data_dir must attach the store"
        );

        assert!(
            first
                .grant_one_shot("run-1", "shell", &args, "owner", "cli")
                .is_some()
        );
        assert!(first.redeem_one_shot("run-1", "shell", &args).is_ok());
        first.record_audit(
            "run-1",
            Some("trader"),
            "shell",
            &args,
            super::store::AuditDecision::Granted,
            Some("owner"),
            Some("cli"),
        );
        drop(first);

        let restarted =
            manager(AutonomyLevel::Supervised, &["shell"], &[]).with_store_at(dir.path());
        assert!(
            restarted.has_store(),
            "reopening the same data_dir must attach again"
        );
        let store = super::store::ApprovalStore::open(dir.path(), "boot-restart").unwrap();
        let rows = store.audit_for_run("run-1").unwrap();
        assert_eq!(rows.len(), 1, "the audit row must survive the new manager");
        assert_eq!(rows[0].2, "granted");
    }

    /// Open failure must not change today's proceed-without-durability path.
    #[test]
    fn store_open_failure_keeps_in_memory_proceed() {
        let tmp = tempfile::tempdir().unwrap();
        let blocked = tmp.path().join("not-a-directory");
        std::fs::write(&blocked, b"this is a file").unwrap();

        let manager = manager(AutonomyLevel::Supervised, &["shell"], &[]).with_store_at(&blocked);
        assert!(
            !manager.has_store(),
            "a path that cannot hold approvals.db must not attach a store"
        );
        assert!(
            manager
                .redeem_one_shot("run-1", "shell", &json!({"command": "ls"}))
                .is_ok(),
            "open failure must keep redemption as a no-op"
        );
    }

    #[cfg(unix)]
    #[test]
    fn store_open_failure_on_readonly_dir_keeps_in_memory_proceed() {
        use std::os::unix::fs::PermissionsExt;

        let tmp = tempfile::tempdir().unwrap();
        let readonly = tmp.path().join("readonly");
        std::fs::create_dir(&readonly).unwrap();
        std::fs::set_permissions(&readonly, std::fs::Permissions::from_mode(0o555)).unwrap();

        let (has_store, redeem_ok) = {
            let manager =
                manager(AutonomyLevel::Supervised, &["shell"], &[]).with_store_at(&readonly);
            (
                manager.has_store(),
                manager
                    .redeem_one_shot("run-1", "shell", &json!({"command": "ls"}))
                    .is_ok(),
            )
        };
        std::fs::set_permissions(&readonly, std::fs::Permissions::from_mode(0o755)).unwrap();

        assert!(!has_store, "a read-only data_dir must not attach a store");
        assert!(redeem_ok, "open failure must keep redemption as a no-op");
    }

    /// Once a store is attached, a grant that cannot be written must not
    /// look like success — redeem then fails closed, which is how the gate
    /// refuses execution.
    #[test]
    fn grant_write_failure_fails_closed() {
        let dir = tempfile::tempdir().unwrap();
        let manager = with_store(
            manager(AutonomyLevel::Supervised, &["shell"], &[]),
            dir.path(),
        );
        let conn = rusqlite::Connection::open(dir.path().join("approvals.db")).unwrap();
        conn.execute_batch(
            "CREATE TRIGGER deny_insert BEFORE INSERT ON approval_grants
             BEGIN SELECT RAISE(ABORT, 'injected write failure'); END;",
        )
        .unwrap();
        drop(conn);

        let args = json!({"command": "ls"});
        assert!(
            manager
                .grant_one_shot("run-1", "shell", &args, "owner", "cli")
                .is_none(),
            "a grant that cannot be persisted must not return an id"
        );
        assert_eq!(
            manager.redeem_one_shot("run-1", "shell", &args),
            Err(super::store::RedeemFailure::NoGrant),
            "write failure must fail closed: no persisted grant, no execution"
        );
    }

    /// The gate used to ignore `grant_one_shot` returning `None` and still
    /// redeem. A leftover unconsumed row for the same tuple would then be
    /// spent as if this approval had been persisted.
    #[test]
    fn grant_write_failure_does_not_spend_a_prior_matching_grant() {
        let dir = tempfile::tempdir().unwrap();
        let manager = with_store(
            manager(AutonomyLevel::Supervised, &["shell"], &[]),
            dir.path(),
        );
        let args = json!({"command": "ls"});
        assert!(
            manager
                .grant_one_shot("run-1", "shell", &args, "owner", "cli")
                .is_some()
        );

        let conn = rusqlite::Connection::open(dir.path().join("approvals.db")).unwrap();
        conn.execute_batch(
            "CREATE TRIGGER deny_insert BEFORE INSERT ON approval_grants
             BEGIN SELECT RAISE(ABORT, 'injected write failure'); END;",
        )
        .unwrap();
        drop(conn);

        assert_eq!(
            manager.grant_and_claim_one_shot("run-1", "shell", &args, "owner", "cli"),
            Err(super::store::RedeemFailure::NoGrant),
            "a grant write failure must refuse without redeeming"
        );

        let consumed: Option<String> = rusqlite::Connection::open(dir.path().join("approvals.db"))
            .unwrap()
            .query_row(
                "SELECT consumed_at FROM approval_grants
                  WHERE run_id = ?1 AND tool_name = ?2",
                rusqlite::params!["run-1", "shell"],
                |row| row.get(0),
            )
            .unwrap();
        assert!(
            consumed.is_none(),
            "the prior matching grant must still be unconsumed"
        );
        assert!(
            manager.redeem_one_shot("run-1", "shell", &args).is_ok(),
            "the leftover grant must remain redeemable after the refused write"
        );
    }

    /// First manager freezes a boot id. A second manager on the same DB —
    /// opened independently, as the gateway and channel construction do —
    /// resolves the same frozen id and can redeem the first manager's grant.
    #[tokio::test]
    async fn frozen_boot_id_cross_redeems_across_independently_opened_managers() {
        let slot = std::sync::OnceLock::new();
        let dir = tempfile::tempdir().unwrap();
        let args = json!({"command": "ls"});

        let first_boot = super::resolve_boot_id(&slot);
        let first =
            manager(AutonomyLevel::Supervised, &["shell"], &[]).with_store(std::sync::Arc::new(
                super::store::ApprovalStore::open(dir.path(), first_boot.as_str())
                    .expect("store opens"),
            ));

        let after = super::resolve_boot_id(&slot);
        assert_eq!(
            first_boot, after,
            "the first boot choice must stay frozen for the whole process"
        );

        let second =
            manager(AutonomyLevel::Supervised, &["shell"], &[]).with_store(std::sync::Arc::new(
                super::store::ApprovalStore::open(dir.path(), after.as_str()).expect("store opens"),
            ));
        assert_eq!(
            first.store.as_ref().unwrap().boot_id(),
            second.store.as_ref().unwrap().boot_id()
        );
        assert!(
            first
                .grant_one_shot("run-1", "shell", &args, "owner", "cli")
                .is_some()
        );
        assert!(
            second.redeem_one_shot("run-1", "shell", &args).is_ok(),
            "independently opened managers sharing the frozen boot must cross-redeem"
        );
        assert!(
            first
                .grant_one_shot("run-2", "shell", &args, "owner", "cli")
                .is_some()
        );
        assert!(
            first.redeem_one_shot("run-2", "shell", &args).is_ok(),
            "the first manager must also redeem a grant minted by itself afterwards"
        );
    }

    /// Two managers, two SQLite connections, one data_dir: the consume
    /// UPDATE is atomic, so a racing redeem of the same grant succeeds
    /// exactly once.
    #[test]
    fn two_managers_racing_redeem_consume_a_grant_exactly_once() {
        let dir = tempfile::tempdir().unwrap();
        let args = json!({"command": "ls"});
        let a = std::sync::Arc::new(with_store(
            manager(AutonomyLevel::Supervised, &["shell"], &[]),
            dir.path(),
        ));
        let b = std::sync::Arc::new(with_store(
            manager(AutonomyLevel::Supervised, &["shell"], &[]),
            dir.path(),
        ));
        assert!(
            a.grant_one_shot("run-1", "shell", &args, "owner", "cli")
                .is_some()
        );

        let barrier = std::sync::Arc::new(std::sync::Barrier::new(2));
        let (left, right) = std::thread::scope(|scope| {
            let barrier_a = std::sync::Arc::clone(&barrier);
            let manager_a = std::sync::Arc::clone(&a);
            let args_a = args.clone();
            let left = scope.spawn(move || {
                barrier_a.wait();
                manager_a.redeem_one_shot("run-1", "shell", &args_a)
            });
            let barrier_b = std::sync::Arc::clone(&barrier);
            let manager_b = std::sync::Arc::clone(&b);
            let args_b = args.clone();
            let right = scope.spawn(move || {
                barrier_b.wait();
                manager_b.redeem_one_shot("run-1", "shell", &args_b)
            });
            (left.join().unwrap(), right.join().unwrap())
        });

        let wins = [&left, &right].iter().filter(|r| r.is_ok()).count();
        let losses = [&left, &right]
            .iter()
            .filter(|r| {
                matches!(
                    r,
                    Err(super::store::RedeemFailure::AlreadyConsumed)
                        | Err(super::store::RedeemFailure::NoGrant)
                )
            })
            .count();
        assert_eq!(wins, 1, "exactly one racing redeem must succeed");
        assert_eq!(losses, 1, "the other racing redeem must fail closed");
    }

    /// Delegated SOP steps derive a new manager; dropping the store there
    /// would silently revert them to memory-only.
    #[test]
    fn derive_for_risk_profile_keeps_the_store() {
        let dir = tempfile::tempdir().unwrap();
        let parent = manager(AutonomyLevel::Supervised, &["shell"], &[]).with_store_at(dir.path());
        let derived =
            parent.derive_for_risk_profile(&zeroclaw_config::schema::RiskProfileConfig::default());
        assert!(parent.has_store());
        assert!(
            derived.has_store(),
            "derive must inherit the parent's durable store"
        );

        let args = json!({"command": "ls"});
        assert!(
            derived
                .grant_one_shot("run-1", "shell", &args, "owner", "cli")
                .is_some()
        );
        assert!(derived.redeem_one_shot("run-1", "shell", &args).is_ok());
    }

    /// A prior "Always" answer must not survive into an unattended run for a
    /// tool the profile marks `always_ask`.
    #[test]
    fn unattended_session_allowlist_cannot_unlock_an_always_ask_tool() {
        let manager = unattended(AutonomyLevel::Full, &["shell"], &["shell"]);
        manager.record_decision(
            "shell",
            &json!({"command": "rm -rf /"}),
            &ApprovalResponse::Always,
            "test",
        );

        assert_eq!(
            manager.approval_requirement("shell"),
            ApprovalRequirement::Prompt
        );
    }
}

// ── CLI prompt ───────────────────────────────────────────────────

/// Display the approval prompt and read user input from the controlling
/// terminal when available, falling back to stdin otherwise.
fn prompt_cli_interactive(request: &ApprovalRequest) -> ApprovalResponse {
    let summary = summarize_args(&request.arguments);
    eprintln!();
    eprintln!("🔧 Agent wants to execute: {}", request.tool_name);
    eprintln!("   {summary}");
    eprint!("   [Y]es / [N]o / [A]lways for {}: ", request.tool_name);
    let _ = io::stderr().flush();

    let Ok(line) = read_cli_approval_line() else {
        return ApprovalResponse::No;
    };

    parse_cli_approval_response(&line)
}

fn parse_cli_approval_response(line: &str) -> ApprovalResponse {
    match line.trim().to_ascii_lowercase().as_str() {
        "y" | "yes" => ApprovalResponse::Yes,
        "a" | "always" => ApprovalResponse::Always,
        _ => ApprovalResponse::No,
    }
}

#[cfg(unix)]
fn read_cli_approval_line() -> io::Result<String> {
    read_cli_approval_line_with(
        || std::fs::File::open("/dev/tty").map(BufReader::new),
        read_stdin_approval_line,
    )
}

#[cfg(unix)]
fn read_cli_approval_line_with<Tty, OpenTty, ReadStdin>(
    open_tty: OpenTty,
    read_stdin: ReadStdin,
) -> io::Result<String>
where
    Tty: BufRead,
    OpenTty: FnOnce() -> io::Result<Tty>,
    ReadStdin: FnOnce() -> io::Result<String>,
{
    match open_tty() {
        Ok(tty) => read_approval_line_from(tty),
        Err(_) => read_stdin(),
    }
}

#[cfg(not(unix))]
fn read_cli_approval_line() -> io::Result<String> {
    read_stdin_approval_line()
}

fn read_stdin_approval_line() -> io::Result<String> {
    let stdin = io::stdin();
    read_approval_line_from(stdin.lock())
}

fn read_approval_line_from<R: BufRead>(mut reader: R) -> io::Result<String> {
    let mut line = String::new();
    reader.read_line(&mut line)?;
    Ok(line)
}

pub fn summarize_args(args: &serde_json::Value) -> String {
    match args {
        serde_json::Value::Object(map) => {
            let mut parts: Vec<String> = Vec::with_capacity(map.len());

            // Prioritize "path" (used by file_write/file_edit etc.) so approval
            // popups and audit logs always surface the target file first.
            if let Some(v) = map.get("path") {
                let val = if looks_like_secret_key("path") {
                    "[redacted]".to_string()
                } else {
                    match v {
                        // Same scrub-before-truncate treatment as the general
                        // loop below: a signed or tokenized path
                        // (`https://host/file?token=...`) is still a render
                        // surface, and truncating first could cut the value
                        // below the scrubber's match length.
                        serde_json::Value::String(s) => {
                            truncate_for_summary(&scrub_credentials(s), 80)
                        }
                        other => {
                            let s = scrub_credentials_value(other.clone()).to_string();
                            truncate_for_summary(&s, 80)
                        }
                    }
                };
                parts.push(format!("path: {val}"));
            }

            for (k, v) in map.iter() {
                if k == "path" {
                    continue;
                }
                let val = if looks_like_secret_key(k) {
                    "[redacted]".to_string()
                } else {
                    match v {
                        // Plain strings can still carry inline credential
                        // shapes (note: "token=..."); scrub before truncation
                        // so truncation cannot cut the value below the
                        // scrubber's match length and smuggle a prefix
                        // through, matching the nested-value treatment.
                        serde_json::Value::String(s) => {
                            truncate_for_summary(&scrub_credentials(s), 80)
                        }
                        // Nested objects/arrays are rendered to the operator
                        // (and persisted in the approval audit trail); run
                        // them through the rendering-boundary scrub so a
                        // credential hidden inside a nested argument (e.g.
                        // metadata.api_key) is not echoed verbatim.
                        other => {
                            let s = scrub_credentials_value(other.clone()).to_string();
                            truncate_for_summary(&s, 80)
                        }
                    }
                };
                parts.push(format!("{k}: {val}"));
            }
            parts.join(", ")
        }
        other => {
            // Non-object top-level args (e.g. an array of calls) get the same
            // rendering-boundary scrub before they reach the operator.
            let s = scrub_credentials_value(other.clone()).to_string();
            truncate_for_summary(&s, 120)
        }
    }
}

/// Heuristic for argument keys that should have their value redacted in
/// human-readable summaries. Matches anywhere in the (case-insensitive) key:
/// covers `api_key`, `api-key`, `apiKey`, `oauth_token`, `secret`,
/// `password`, `passwd`, `auth`, `auth_token`, `bearer`, `client_secret`,
/// `private_key`, `credential`, cookie headers, etc. The predicate lives in
/// `agent::turn::redact` and is shared with the structured credential walk
/// so the two surfaces cannot drift apart.
fn looks_like_secret_key(key: &str) -> bool {
    crate::agent::turn::redact::is_sensitive_key(key)
}

fn truncate_for_summary(input: &str, max_chars: usize) -> String {
    let mut chars = input.chars();
    let truncated: String = chars.by_ref().take(max_chars).collect();
    if chars.next().is_some() {
        format!("{truncated}…")
    } else {
        input.to_string()
    }
}

// ── Tests ────────────────────────────────────────────────────────

#[cfg(test)]
mod tests {
    use super::*;
    use zeroclaw_config::schema::RiskProfileConfig;

    #[test]
    fn sanitize_replacement_strips_control_chars_keeps_whitespace() {
        let dirty = "ok\u{0007}line\nnext\ttab\u{001b}[31m";
        let clean = sanitize_tool_replacement(dirty);
        assert_eq!(clean, "okline\nnext\ttab[31m");
    }

    #[test]
    fn sanitize_replacement_truncates_on_char_boundary() {
        let big = "é".repeat(MAX_REPLACEMENT_LEN); // 2 bytes each
        let clean = sanitize_tool_replacement(&big);
        assert!(clean.len() <= MAX_REPLACEMENT_LEN);
        // Truncation must land on a char boundary (no panic, valid UTF-8).
        assert!(clean.chars().all(|c| c == 'é'));
    }

    fn supervised_config() -> RiskProfileConfig {
        RiskProfileConfig {
            level: AutonomyLevel::Supervised,
            auto_approve: vec!["file_read".into(), "memory_recall".into()],
            always_ask: vec!["shell".into()],
            ..RiskProfileConfig::default()
        }
    }

    fn full_config() -> RiskProfileConfig {
        RiskProfileConfig {
            level: AutonomyLevel::Full,
            ..RiskProfileConfig::default()
        }
    }

    // ── CLI prompt input ────────────────────────────────────

    #[test]
    fn cli_approval_parser_accepts_yes_and_always() {
        assert_eq!(parse_cli_approval_response("y\n"), ApprovalResponse::Yes);
        assert_eq!(parse_cli_approval_response("YES\n"), ApprovalResponse::Yes);
        assert_eq!(
            parse_cli_approval_response(" always \n"),
            ApprovalResponse::Always
        );
        assert_eq!(
            parse_cli_approval_response("A\r\n"),
            ApprovalResponse::Always
        );
    }

    #[test]
    fn cli_approval_parser_denies_empty_eof_and_unknown_input() {
        assert_eq!(parse_cli_approval_response(""), ApprovalResponse::No);
        assert_eq!(parse_cli_approval_response("\n"), ApprovalResponse::No);
        assert_eq!(parse_cli_approval_response("maybe\n"), ApprovalResponse::No);
        assert_eq!(parse_cli_approval_response("[Y]\n"), ApprovalResponse::No);
    }

    #[test]
    fn approval_line_reader_preserves_existing_stdin_eof_semantics() {
        let line = read_approval_line_from(std::io::Cursor::new("yes\n")).unwrap();
        assert_eq!(line, "yes\n");

        let eof = read_approval_line_from(std::io::Cursor::new(Vec::<u8>::new())).unwrap();
        assert_eq!(eof, "");
        assert_eq!(parse_cli_approval_response(&eof), ApprovalResponse::No);
    }

    #[cfg(unix)]
    #[test]
    fn cli_approval_reader_prefers_tty_over_stdin_eof() {
        let line =
            read_cli_approval_line_with(|| Ok(std::io::Cursor::new("yes\n")), || Ok(String::new()))
                .unwrap();

        assert_eq!(line, "yes\n");
        assert_eq!(parse_cli_approval_response(&line), ApprovalResponse::Yes);
    }

    #[cfg(unix)]
    #[test]
    fn cli_approval_reader_falls_back_to_stdin_when_tty_unavailable() {
        let line = read_cli_approval_line_with(
            || -> io::Result<std::io::Cursor<&'static str>> {
                Err(io::Error::new(io::ErrorKind::NotFound, "no tty"))
            },
            || Ok("always\n".to_string()),
        )
        .unwrap();

        assert_eq!(line, "always\n");
        assert_eq!(parse_cli_approval_response(&line), ApprovalResponse::Always);
    }

    #[cfg(unix)]
    #[test]
    fn cli_approval_reader_tty_read_error_fails_without_stdin_fallback() {
        struct FailingReader;

        impl std::io::Read for FailingReader {
            fn read(&mut self, _buf: &mut [u8]) -> io::Result<usize> {
                Err(io::Error::new(io::ErrorKind::PermissionDenied, "tty read"))
            }
        }

        let result = read_cli_approval_line_with(
            || Ok(std::io::BufReader::new(FailingReader)),
            || panic!("stdin fallback should not run after tty read errors"),
        );

        assert!(result.is_err());
    }

    // ── needs_approval ───────────────────────────────────────

    #[test]
    fn auto_approve_tools_skip_prompt() {
        let mgr = ApprovalManager::from_risk_profile(&supervised_config());
        assert!(!mgr.needs_approval("file_read"));
        assert!(!mgr.needs_approval("memory_recall"));
    }

    #[test]
    fn always_ask_tools_always_prompt() {
        let mgr = ApprovalManager::from_risk_profile(&supervised_config());
        assert!(mgr.needs_approval("shell"));
    }

    #[test]
    fn unknown_tool_needs_approval_in_supervised() {
        let mgr = ApprovalManager::from_risk_profile(&supervised_config());
        assert!(mgr.needs_approval("file_write"));
        assert!(mgr.needs_approval("http_request"));
    }

    #[test]
    fn full_autonomy_never_prompts_without_always_ask() {
        // full_config() has empty always_ask; Full auto-approves those tools.
        let mgr = ApprovalManager::from_risk_profile(&full_config());
        assert!(!mgr.needs_approval("shell"));
        assert!(!mgr.needs_approval("file_write"));
        assert!(!mgr.needs_approval("anything"));
    }

    #[test]
    fn readonly_never_prompts() {
        let config = RiskProfileConfig {
            level: AutonomyLevel::ReadOnly,
            ..RiskProfileConfig::default()
        };
        let mgr = ApprovalManager::from_risk_profile(&config);
        assert!(!mgr.needs_approval("shell"));
    }

    // ── session allowlist ────────────────────────────────────

    #[test]
    fn always_response_adds_to_session_allowlist() {
        let mgr = ApprovalManager::from_risk_profile(&supervised_config());
        assert!(mgr.needs_approval("file_write"));

        mgr.record_decision(
            "file_write",
            &serde_json::json!({"path": "test.txt"}),
            &ApprovalResponse::Always,
            "cli",
        );

        // Now file_write should be in session allowlist.
        assert!(!mgr.needs_approval("file_write"));
    }

    #[test]
    fn always_ask_overrides_session_allowlist() {
        let mgr = ApprovalManager::from_risk_profile(&supervised_config());

        // Even after "Always" for shell, it should still prompt.
        mgr.record_decision(
            "shell",
            &serde_json::json!({"command": "ls"}),
            &ApprovalResponse::Always,
            "cli",
        );

        // shell is in always_ask, so it still needs approval.
        assert!(mgr.needs_approval("shell"));
    }

    #[test]
    fn yes_response_does_not_add_to_allowlist() {
        let mgr = ApprovalManager::from_risk_profile(&supervised_config());
        mgr.record_decision(
            "file_write",
            &serde_json::json!({}),
            &ApprovalResponse::Yes,
            "cli",
        );
        assert!(mgr.needs_approval("file_write"));
    }

    // ── audit log ────────────────────────────────────────────

    #[test]
    fn audit_log_records_decisions() {
        let mgr = ApprovalManager::from_risk_profile(&supervised_config());

        mgr.record_decision(
            "shell",
            &serde_json::json!({"command": "rm -rf ./build/"}),
            &ApprovalResponse::No,
            "cli",
        );
        mgr.record_decision(
            "file_write",
            &serde_json::json!({"path": "out.txt", "content": "hello"}),
            &ApprovalResponse::Yes,
            "cli",
        );

        let log = mgr.audit_log();
        assert_eq!(log.len(), 2);
        assert_eq!(log[0].tool_name, "shell");
        assert_eq!(log[0].decision, ApprovalResponse::No);
        assert_eq!(log[1].tool_name, "file_write");
        assert_eq!(log[1].decision, ApprovalResponse::Yes);
    }

    #[test]
    fn audit_log_contains_timestamp_and_channel() {
        let mgr = ApprovalManager::from_risk_profile(&supervised_config());
        mgr.record_decision(
            "shell",
            &serde_json::json!({"command": "ls"}),
            &ApprovalResponse::Yes,
            "telegram",
        );

        let log = mgr.audit_log();
        assert_eq!(log.len(), 1);
        assert!(!log[0].timestamp.is_empty());
        assert_eq!(log[0].channel, "telegram");
    }

    // ── summarize_args ───────────────────────────────────────

    #[test]
    pub fn summarize_args_object() {
        let args = serde_json::json!({"command": "ls -la", "cwd": "/tmp"});
        let summary = summarize_args(&args);
        assert!(summary.contains("command: ls -la"));
        assert!(summary.contains("cwd: /tmp"));
    }

    #[test]
    pub fn summarize_args_puts_path_first_for_file_tools() {
        let args = serde_json::json!({
            "path": "src/main.rs",
            "old_string": "foo",
            "new_string": "bar"
        });
        let summary = summarize_args(&args);
        assert!(summary.starts_with("path: src/main.rs"));
        assert!(summary.contains("old_string: foo"));
        assert!(summary.contains("new_string: bar"));
    }

    #[test]
    pub fn summarize_args_truncates_long_values() {
        let long_val = "x".repeat(200);
        let args = serde_json::json!({ "content": long_val });
        let summary = summarize_args(&args);
        assert!(summary.contains('…'));
        assert!(summary.len() < 200);
    }

    #[test]
    pub fn summarize_args_redacts_credential_nested_in_object_value() {
        // A credential smuggled inside a nested argument object must not be
        // echoed verbatim into the approval prompt or the audit trail.
        let args = serde_json::json!({
            "action": "upsert_agent",
            "metadata": {"api_key": "sk-test-raw-secret-material"},
            "model": "gpt-5.3-codex"
        });
        let summary = summarize_args(&args);
        assert!(
            !summary.contains("sk-test-raw-secret-material"),
            "nested credential must not be echoed: {summary}"
        );
        assert!(
            summary.contains("[REDACTED]"),
            "nested credential should show the redaction marker: {summary}"
        );
        assert!(summary.contains("model: gpt-5.3-codex"));
    }

    #[test]
    pub fn summarize_args_redacts_credential_nested_in_path_value() {
        let args = serde_json::json!({
            "path": {"api_key": "sk-test-raw-secret-material", "hint": "coding"}
        });
        let summary = summarize_args(&args);
        assert!(
            !summary.contains("sk-test-raw-secret-material"),
            "nested credential under 'path' must not be echoed: {summary}"
        );
    }

    #[test]
    pub fn summarize_args_redacts_inline_credential_in_plain_string_value() {
        let args = serde_json::json!({
            "note": "auth via token=aaaaaaaaaaaa99 then retry"
        });
        let summary = summarize_args(&args);
        assert!(
            !summary.contains("aaaaaaaaaaaa99"),
            "inline credential inside a plain string must not be echoed: {summary}"
        );
        assert!(
            summary.contains("token=[REDACTED]"),
            "inline credential should show the full mask: {summary}"
        );
        assert!(summary.contains("then retry"));
    }

    #[test]
    pub fn summarize_args_redacts_inline_credential_in_string_path_value() {
        let args = serde_json::json!({
            "path": "https://internal.host/fetch?token=aaaaaaaaaaaa99&mode=raw"
        });
        let summary = summarize_args(&args);
        assert!(
            !summary.contains("aaaaaaaaaaaa99"),
            "inline credential inside a string path must not be echoed: {summary}"
        );
        assert!(
            summary.contains("token=[REDACTED]"),
            "inline credential in a path should show the full mask: {summary}"
        );
        assert!(summary.contains("internal.host/fetch"));
    }

    #[test]
    pub fn summarize_args_unicode_safe_truncation() {
        let long_val = "🦀".repeat(120);
        let args = serde_json::json!({ "content": long_val });
        let summary = summarize_args(&args);
        assert!(summary.contains("content:"));
        assert!(summary.contains('…'));
    }

    #[test]
    pub fn summarize_args_non_object() {
        let args = serde_json::json!("just a string");
        let summary = summarize_args(&args);
        assert!(summary.contains("just a string"));
    }

    // ── non-interactive (channel) mode ────────────────────────

    #[test]
    fn non_interactive_manager_reports_non_interactive() {
        let mgr = ApprovalManager::for_non_interactive(&supervised_config());
        assert!(mgr.is_non_interactive());
    }

    #[test]
    fn interactive_manager_reports_interactive() {
        let mgr = ApprovalManager::from_risk_profile(&supervised_config());
        assert!(!mgr.is_non_interactive());
    }

    #[test]
    fn non_interactive_auto_approve_tools_skip_approval() {
        let mgr = ApprovalManager::for_non_interactive(&supervised_config());
        // auto_approve tools (file_read, memory_recall) should not need approval.
        assert!(!mgr.needs_approval("file_read"));
        assert!(!mgr.needs_approval("memory_recall"));
    }

    #[test]
    fn non_interactive_shell_skips_outer_approval_by_default() {
        let mgr = ApprovalManager::for_non_interactive(&RiskProfileConfig::default());
        assert!(!mgr.needs_approval("shell"));
    }

    #[test]
    fn non_interactive_backchannel_shell_requires_outer_approval() {
        let mgr = ApprovalManager::for_non_interactive_backchannel(&RiskProfileConfig::default());
        assert!(mgr.is_non_interactive());
        assert!(mgr.needs_approval("shell"));
    }

    #[test]
    fn non_interactive_always_ask_tools_need_approval() {
        let mgr = ApprovalManager::for_non_interactive(&supervised_config());
        // always_ask tools (shell) still report as needing approval,
        // so the tool-call loop will auto-deny them in non-interactive mode.
        assert!(mgr.needs_approval("shell"));
    }

    #[test]
    fn non_interactive_unknown_tools_need_approval_in_supervised() {
        let mgr = ApprovalManager::for_non_interactive(&supervised_config());
        // Unknown tools in supervised mode need approval (will be auto-denied
        // by the tool-call loop for non-interactive managers).
        assert!(mgr.needs_approval("file_write"));
        assert!(mgr.needs_approval("http_request"));
    }

    #[test]
    fn non_interactive_full_autonomy_never_needs_approval_without_always_ask() {
        let mgr = ApprovalManager::for_non_interactive(&full_config());
        // Empty always_ask: Full means no approval needed, even non-interactive.
        assert!(!mgr.needs_approval("shell"));
        assert!(!mgr.needs_approval("file_write"));
        assert!(!mgr.needs_approval("anything"));
    }

    #[test]
    fn non_interactive_readonly_never_needs_approval() {
        let config = RiskProfileConfig {
            level: AutonomyLevel::ReadOnly,
            ..RiskProfileConfig::default()
        };
        let mgr = ApprovalManager::for_non_interactive(&config);
        // ReadOnly blocks execution elsewhere; approval manager does not prompt.
        assert!(!mgr.needs_approval("shell"));
    }

    #[test]
    fn non_interactive_session_allowlist_still_works() {
        let mgr = ApprovalManager::for_non_interactive(&supervised_config());
        assert!(mgr.needs_approval("file_write"));

        // Simulate an "Always" decision (would come from a prior channel run
        // if the tool was auto-approved somehow, e.g. via config change).
        mgr.record_decision(
            "file_write",
            &serde_json::json!({"path": "test.txt"}),
            &ApprovalResponse::Always,
            "telegram",
        );

        assert!(!mgr.needs_approval("file_write"));
    }

    #[test]
    fn non_interactive_always_ask_overrides_session_allowlist() {
        let mgr = ApprovalManager::for_non_interactive(&supervised_config());

        mgr.record_decision(
            "shell",
            &serde_json::json!({"command": "ls"}),
            &ApprovalResponse::Always,
            "telegram",
        );

        // shell is in always_ask, so it still needs approval even after "Always".
        assert!(mgr.needs_approval("shell"));
    }

    // ── ApprovalResponse serde ───────────────────────────────

    #[test]
    fn approval_response_serde_roundtrip() {
        let json = serde_json::to_string(&ApprovalResponse::Always).unwrap();
        assert_eq!(json, "\"always\"");
        let parsed: ApprovalResponse = serde_json::from_str("\"no\"").unwrap();
        assert_eq!(parsed, ApprovalResponse::No);
        let json =
            serde_json::to_string(&ApprovalResponse::ReplaceWith("foo".to_string())).unwrap();
        let parsed: ApprovalResponse = serde_json::from_str(&json).unwrap();
        assert_eq!(parsed, ApprovalResponse::ReplaceWith("foo".to_string()));
    }

    // ── ApprovalRequest ──────────────────────────────────────

    #[test]
    fn approval_request_serde() {
        let req = ApprovalRequest {
            tool_name: "shell".into(),
            arguments: serde_json::json!({"command": "echo hi"}),
        };
        let json = serde_json::to_string(&req).unwrap();
        let parsed: ApprovalRequest = serde_json::from_str(&json).unwrap();
        assert_eq!(parsed.tool_name, "shell");
    }

    // ──default approved tools in channels ──

    #[test]
    fn non_interactive_allows_default_auto_approve_tools() {
        let config = RiskProfileConfig::default();
        let mgr = ApprovalManager::for_non_interactive(&config);

        for tool in &config.auto_approve {
            assert!(
                !mgr.needs_approval(tool),
                "default auto_approve tool '{tool}' should not need approval in non-interactive mode"
            );
        }
    }

    #[test]
    fn non_interactive_denies_unknown_tools() {
        let config = RiskProfileConfig::default();
        let mgr = ApprovalManager::for_non_interactive(&config);
        assert!(
            mgr.needs_approval("some_unknown_tool"),
            "unknown tool should need approval"
        );
    }

    #[test]
    fn non_interactive_tool_search_is_auto_approved() {
        let config = RiskProfileConfig::default();
        let mgr = ApprovalManager::for_non_interactive(&config);
        assert!(
            !mgr.needs_approval("tool_search"),
            "tool_search discovery must not need approval in non-interactive mode"
        );
    }

    #[test]
    fn non_interactive_glob_search_is_auto_approved() {
        let config = RiskProfileConfig::default();
        let mgr = ApprovalManager::for_non_interactive(&config);
        assert!(
            !mgr.needs_approval("glob_search"),
            "glob_search tool must not need approval — it is in the default auto_approve list"
        );
    }

    #[test]
    fn always_ask_overrides_auto_approve() {
        let config = RiskProfileConfig {
            always_ask: vec!["glob_search".into()],
            ..RiskProfileConfig::default()
        };
        let mgr = ApprovalManager::for_non_interactive(&config);
        assert!(
            mgr.needs_approval("glob_search"),
            "always_ask must override auto_approve"
        );
    }

    // ── ChannelApprovalResponse → ApprovalResponse mapping ──────

    #[test]
    fn channel_approve_maps_to_yes() {
        use zeroclaw_api::channel::ChannelApprovalResponse;
        let mapped = match ChannelApprovalResponse::Approve {
            ChannelApprovalResponse::Approve => ApprovalResponse::Yes,
            ChannelApprovalResponse::AlwaysApprove => ApprovalResponse::Always,
            ChannelApprovalResponse::Deny => ApprovalResponse::No,
            ChannelApprovalResponse::DenyWithEdit { replacement } => {
                ApprovalResponse::ReplaceWith(replacement)
            }
        };
        assert_eq!(mapped, ApprovalResponse::Yes);
    }

    #[test]
    fn channel_always_approve_maps_to_always() {
        use zeroclaw_api::channel::ChannelApprovalResponse;
        let mapped = match ChannelApprovalResponse::AlwaysApprove {
            ChannelApprovalResponse::Approve => ApprovalResponse::Yes,
            ChannelApprovalResponse::AlwaysApprove => ApprovalResponse::Always,
            ChannelApprovalResponse::Deny => ApprovalResponse::No,
            ChannelApprovalResponse::DenyWithEdit { replacement } => {
                ApprovalResponse::ReplaceWith(replacement)
            }
        };
        assert_eq!(mapped, ApprovalResponse::Always);
    }

    #[test]
    fn channel_deny_maps_to_no() {
        use zeroclaw_api::channel::ChannelApprovalResponse;
        let mapped = match ChannelApprovalResponse::Deny {
            ChannelApprovalResponse::Approve => ApprovalResponse::Yes,
            ChannelApprovalResponse::AlwaysApprove => ApprovalResponse::Always,
            ChannelApprovalResponse::Deny => ApprovalResponse::No,
            ChannelApprovalResponse::DenyWithEdit { replacement } => {
                ApprovalResponse::ReplaceWith(replacement)
            }
        };
        assert_eq!(mapped, ApprovalResponse::No);
    }

    #[test]
    fn channel_deny_with_edit_maps_to_replace_with() {
        use zeroclaw_api::channel::ChannelApprovalResponse;
        let mapped = match (ChannelApprovalResponse::DenyWithEdit {
            replacement: "x".to_string(),
        }) {
            ChannelApprovalResponse::Approve => ApprovalResponse::Yes,
            ChannelApprovalResponse::AlwaysApprove => ApprovalResponse::Always,
            ChannelApprovalResponse::Deny => ApprovalResponse::No,
            ChannelApprovalResponse::DenyWithEdit { replacement } => {
                ApprovalResponse::ReplaceWith(replacement)
            }
        };
        assert!(matches!(mapped, ApprovalResponse::ReplaceWith(s) if s == "x"));
    }

    #[test]
    fn replace_with_is_not_yes_or_no() {
        let r = ApprovalResponse::ReplaceWith("new text".to_string());
        assert_ne!(r, ApprovalResponse::Yes);
        assert_ne!(r, ApprovalResponse::No);
    }

    #[test]
    fn channel_approval_request_serde_roundtrip() {
        use zeroclaw_api::channel::ChannelApprovalRequest;
        let req = ChannelApprovalRequest {
            tool_name: "shell".into(),
            arguments_summary: "command: ls -la".into(),
            raw_arguments: None,
        };
        let json = serde_json::to_string(&req).unwrap();
        let parsed: ChannelApprovalRequest = serde_json::from_str(&json).unwrap();
        assert_eq!(parsed.tool_name, "shell");
        assert_eq!(parsed.arguments_summary, "command: ls -la");
    }

    #[test]
    fn channel_approval_response_serde_roundtrip() {
        use zeroclaw_api::channel::ChannelApprovalResponse;
        // AlwaysApprove serializes to "always" to match the CLI-side
        // ApprovalResponse::Always and keep audit logs consistent.
        let json = serde_json::to_string(&ChannelApprovalResponse::AlwaysApprove).unwrap();
        assert_eq!(json, "\"always\"");
        let parsed: ChannelApprovalResponse = serde_json::from_str("\"always\"").unwrap();
        assert_eq!(parsed, ChannelApprovalResponse::AlwaysApprove);
        let parsed: ChannelApprovalResponse = serde_json::from_str("\"deny\"").unwrap();
        assert_eq!(parsed, ChannelApprovalResponse::Deny);
    }

    // ── summarize_args secret-key redaction ────────────────────

    #[test]
    fn summarize_args_redacts_known_secret_key_names() {
        let args = serde_json::json!({
            "endpoint": "https://api.example.com",
            "api_key": "sk-very-secret-key-value",
            "oauth_token": "oauth-secret",
            "client_secret": "client-secret",
            "password": "hunter2",
            "private_key": "-----BEGIN PRIVATE KEY-----abc",
            "bearer_token": "bearer-thing",
        });
        let summary = summarize_args(&args);
        for needle in [
            "sk-very-secret-key-value",
            "oauth-secret",
            "client-secret",
            "hunter2",
            "-----BEGIN PRIVATE KEY-----",
            "bearer-thing",
        ] {
            assert!(
                !summary.contains(needle),
                "summary leaked secret value {needle:?}: {summary}"
            );
        }
        assert!(summary.contains("endpoint:"));
        assert!(summary.contains("api.example.com"));
    }

    #[test]
    fn summarize_args_keeps_non_secret_values() {
        let args = serde_json::json!({
            "path": "/tmp/file.txt",
            "limit": 42,
        });
        let summary = summarize_args(&args);
        assert!(summary.contains("/tmp/file.txt"));
        assert!(summary.contains("42"));
    }

    #[test]
    fn summarize_args_redaction_is_case_insensitive_and_substring_aware() {
        let args = serde_json::json!({
            "X-API-Key": "hdrsecret",
            "DBPassword": "dbpw",
            "AuthHeader": "auth-thing",
        });
        let summary = summarize_args(&args);
        for leaked in ["hdrsecret", "dbpw", "auth-thing"] {
            assert!(
                !summary.contains(leaked),
                "redaction missed {leaked:?}: {summary}"
            );
        }
    }
}
