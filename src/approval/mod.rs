//! Interactive approval workflow for supervised mode.
//!
//! Provides a pre-execution hook that prompts the user before tool calls,
//! with session-scoped "Always" allowlists and audit logging.

use crate::config::AutonomyConfig;
use crate::security::AutonomyLevel;
use chrono::Utc;
use parking_lot::Mutex;
use serde::{Deserialize, Serialize};
use std::collections::{HashMap, HashSet};
use std::io::{self, BufRead, Write};
use std::sync::Arc;
use std::time::Duration;

// ── Types ────────────────────────────────────────────────────────

/// A request to approve a tool call before execution.
#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct ApprovalRequest {
    pub tool_name: String,
    pub arguments: serde_json::Value,
}

/// The user's response to an approval request.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "lowercase")]
pub enum ApprovalResponse {
    /// Execute this one call.
    Yes,
    /// Deny this call.
    No,
    /// Execute and add tool to session-scoped allowlist.
    Always,
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

/// A pending approval request awaiting channel response.
#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct PendingApproval {
    pub request_id: String,
    pub tool_name: String,
    pub arguments: serde_json::Value,
    pub sender: String,
    pub channel: String,
    pub created_at: String,
    pub timeout_at: String,
}

// ── ApprovalManager ──────────────────────────────────────────────

/// Manages the approval workflow for tool calls.
///
/// - Checks config-level `auto_approve` / `always_ask` lists
/// - Maintains a session-scoped "always" allowlist
/// - Records an audit trail of all decisions
///
/// Two modes:
/// - **Interactive** (CLI): tools needing approval trigger a stdin prompt.
/// - **Non-interactive** (channels): tools needing approval are auto-denied
///   because there is no interactive operator to approve them. `auto_approve`
///   policy is still enforced, and `always_ask` / supervised-default tools are
///   denied rather than silently allowed.
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
    /// Session-scoped allowlist built from "Always" responses.
    session_allowlist: Arc<Mutex<HashSet<String>>>,
    /// Audit trail of approval decisions.
    audit_log: Arc<Mutex<Vec<ApprovalLogEntry>>>,
    /// Pending approval requests awaiting channel response.
    pending_requests: Arc<Mutex<HashMap<String, PendingApproval>>>,
    /// Resolved decisions from channel responses.
    resolved_decisions: Arc<Mutex<HashMap<String, ApprovalResponse>>>,
    /// Notifiers for async wait on approval resolution.
    notifiers: Arc<Mutex<HashMap<String, tokio::sync::oneshot::Sender<ApprovalResponse>>>>,
    /// When `true`, tools requiring approval prompt via channel.
    /// When `false`, auto-deny (existing behavior).
    channel_interactive: bool,
}

impl ApprovalManager {
    /// Create an interactive (CLI) approval manager from autonomy config.
    pub fn from_config(config: &AutonomyConfig) -> Self {
        Self {
            auto_approve: config.auto_approve.iter().cloned().collect(),
            always_ask: config.always_ask.iter().cloned().collect(),
            autonomy_level: config.level,
            non_interactive: false,
            session_allowlist: Arc::new(Mutex::new(HashSet::new())),
            audit_log: Arc::new(Mutex::new(Vec::new())),
            pending_requests: Arc::new(Mutex::new(HashMap::new())),
            resolved_decisions: Arc::new(Mutex::new(HashMap::new())),
            notifiers: Arc::new(Mutex::new(HashMap::new())),
            channel_interactive: false,
        }
    }

    /// Create a non-interactive approval manager for channel-driven runs.
    ///
    /// Enforces the same `auto_approve` / `always_ask` / supervised policies
    /// as the CLI manager, but tools that would require interactive approval
    /// are auto-denied instead of prompting (since there is no operator).
    pub fn for_non_interactive(config: &AutonomyConfig) -> Self {
        Self {
            auto_approve: config.auto_approve.iter().cloned().collect(),
            always_ask: config.always_ask.iter().cloned().collect(),
            autonomy_level: config.level,
            non_interactive: true,
            session_allowlist: Arc::new(Mutex::new(HashSet::new())),
            audit_log: Arc::new(Mutex::new(Vec::new())),
            pending_requests: Arc::new(Mutex::new(HashMap::new())),
            resolved_decisions: Arc::new(Mutex::new(HashMap::new())),
            notifiers: Arc::new(Mutex::new(HashMap::new())),
            channel_interactive: false,
        }
    }

    /// Create a channel-interactive approval manager.
    ///
    /// When enabled, tools requiring approval will send a prompt via the
    /// messaging channel and await user response instead of auto-denying.
    pub fn for_channel_interactive(config: &AutonomyConfig) -> Self {
        Self {
            auto_approve: config.auto_approve.iter().cloned().collect(),
            always_ask: config.always_ask.iter().cloned().collect(),
            autonomy_level: config.level,
            non_interactive: false,
            session_allowlist: Arc::new(Mutex::new(HashSet::new())),
            audit_log: Arc::new(Mutex::new(Vec::new())),
            pending_requests: Arc::new(Mutex::new(HashMap::new())),
            resolved_decisions: Arc::new(Mutex::new(HashMap::new())),
            notifiers: Arc::new(Mutex::new(HashMap::new())),
            channel_interactive: true,
        }
    }

    /// Returns `true` when this manager operates in non-interactive mode
    /// (i.e. for channel-driven runs where no operator can approve).
    pub fn is_non_interactive(&self) -> bool {
        self.non_interactive
    }

    /// Returns `true` when this manager operates in channel-interactive mode.
    pub fn is_channel_interactive(&self) -> bool {
        self.channel_interactive
    }

    /// Check whether a tool call requires interactive approval.
    ///
    /// Returns `true` if the call needs a prompt, `false` if it can proceed.
    pub fn needs_approval(&self, tool_name: &str) -> bool {
        // Full autonomy never prompts.
        if self.autonomy_level == AutonomyLevel::Full {
            return false;
        }

        // ReadOnly blocks everything — handled elsewhere; no prompt needed.
        if self.autonomy_level == AutonomyLevel::ReadOnly {
            return false;
        }

        // always_ask overrides everything.
        if self.always_ask.contains("*") || self.always_ask.contains(tool_name) {
            return true;
        }

        // Channel-driven shell execution is still guarded by the shell tool's
        // own command allowlist and risk policy. Skipping the outer approval
        // gate here lets low-risk allowlisted commands (e.g. `ls`) work in
        // non-interactive channels without silently allowing medium/high-risk
        // commands.
        if self.non_interactive && tool_name == "shell" {
            return false;
        }

        // auto_approve skips the prompt.
        if self.auto_approve.contains("*") || self.auto_approve.contains(tool_name) {
            return false;
        }

        // Session allowlist (from prior "Always" responses).
        let allowlist = self.session_allowlist.lock();
        if allowlist.contains(tool_name) {
            return false;
        }

        // Default: supervised mode requires approval.
        true
    }

    /// Record an approval decision and update session state.
    pub fn record_decision(
        &self,
        tool_name: &str,
        args: &serde_json::Value,
        decision: ApprovalResponse,
        channel: &str,
    ) {
        // If "Always", add to session allowlist.
        if decision == ApprovalResponse::Always {
            let mut allowlist = self.session_allowlist.lock();
            allowlist.insert(tool_name.to_string());
        }

        // Append to audit log.
        let summary = summarize_args(args);
        let entry = ApprovalLogEntry {
            timestamp: Utc::now().to_rfc3339(),
            tool_name: tool_name.to_string(),
            arguments_summary: summary,
            decision,
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
    ///
    /// Only called for interactive (CLI) managers. Non-interactive managers
    /// auto-deny in the tool-call loop before reaching this point.
    pub fn prompt_cli(&self, request: &ApprovalRequest) -> ApprovalResponse {
        prompt_cli_interactive(request)
    }

    /// Create a pending approval request and return its unique ID.
    pub fn create_pending_request(
        &self,
        tool_name: String,
        arguments: serde_json::Value,
        sender: String,
        channel: String,
        timeout: Duration,
    ) -> String {
        use uuid::Uuid;

        let request_id = Uuid::new_v4().to_string();
        let now = Utc::now();
        let timeout_time = now + chrono::Duration::from_std(timeout).unwrap();

        let pending = PendingApproval {
            request_id: request_id.clone(),
            tool_name,
            arguments,
            sender,
            channel,
            created_at: now.to_rfc3339(),
            timeout_at: timeout_time.to_rfc3339(),
        };

        let mut requests = self.pending_requests.lock();
        requests.insert(request_id.clone(), pending);

        request_id
    }

    /// Resolve a pending approval request with a decision.
    ///
    /// Validates that the sender matches the original request sender.
    pub fn resolve_pending_request(
        &self,
        request_id: &str,
        sender: &str,
        decision: ApprovalResponse,
    ) -> anyhow::Result<()> {
        let mut requests = self.pending_requests.lock();

        let pending = requests
            .get(request_id)
            .ok_or_else(|| anyhow::anyhow!("Unknown approval request ID: {}", request_id))?;

        if pending.sender != sender {
            anyhow::bail!(
                "Sender mismatch: expected '{}', got '{}'",
                pending.sender,
                sender
            );
        }

        let tool_name = pending.tool_name.clone();
        requests.remove(request_id);
        drop(requests);

        // If "Always", add to session allowlist.
        if decision == ApprovalResponse::Always {
            let mut allowlist = self.session_allowlist.lock();
            allowlist.insert(tool_name);
        }

        // Store resolved decision.
        let mut resolved = self.resolved_decisions.lock();
        resolved.insert(request_id.to_string(), decision);

        // Notify waiter if present.
        let mut notifiers = self.notifiers.lock();
        if let Some(tx) = notifiers.remove(request_id) {
            let _ = tx.send(decision);
        }

        Ok(())
    }

    /// Wait for an approval decision, with timeout.
    pub async fn wait_for_approval(&self, request_id: &str, timeout: Duration) -> ApprovalResponse {
        let (tx, rx) = tokio::sync::oneshot::channel();

        {
            let mut notifiers = self.notifiers.lock();
            notifiers.insert(request_id.to_string(), tx);
        }

        match tokio::time::timeout(timeout, rx).await {
            Ok(Ok(decision)) => decision,
            _ => {
                // Timeout or channel closed - cleanup and return No.
                self.cleanup_request(request_id);
                ApprovalResponse::No
            }
        }
    }

    /// Clean up a single pending request (e.g., on timeout).
    fn cleanup_request(&self, request_id: &str) {
        let mut requests = self.pending_requests.lock();
        requests.remove(request_id);
        drop(requests);

        let mut resolved = self.resolved_decisions.lock();
        resolved.remove(request_id);
        drop(resolved);

        let mut notifiers = self.notifiers.lock();
        notifiers.remove(request_id);
    }

    /// Remove expired pending requests and notify waiters with No.
    pub fn cleanup_expired(&self) {
        let now = Utc::now();
        let mut requests = self.pending_requests.lock();
        let mut notifiers = self.notifiers.lock();

        let expired: Vec<String> = requests
            .iter()
            .filter_map(|(id, req)| {
                if let Ok(timeout_time) = chrono::DateTime::parse_from_rfc3339(&req.timeout_at) {
                    if timeout_time.with_timezone(&Utc) < now {
                        return Some(id.clone());
                    }
                }
                None
            })
            .collect();

        for id in expired {
            requests.remove(&id);
            if let Some(tx) = notifiers.remove(&id) {
                let _ = tx.send(ApprovalResponse::No);
            }
        }
    }
}

impl Clone for ApprovalManager {
    fn clone(&self) -> Self {
        Self {
            auto_approve: self.auto_approve.clone(),
            always_ask: self.always_ask.clone(),
            autonomy_level: self.autonomy_level,
            non_interactive: self.non_interactive,
            session_allowlist: Arc::clone(&self.session_allowlist),
            audit_log: Arc::clone(&self.audit_log),
            pending_requests: Arc::clone(&self.pending_requests),
            resolved_decisions: Arc::clone(&self.resolved_decisions),
            notifiers: Arc::clone(&self.notifiers),
            channel_interactive: self.channel_interactive,
        }
    }
}

// ── CLI prompt ───────────────────────────────────────────────────

/// Display the approval prompt and read user input from stdin.
fn prompt_cli_interactive(request: &ApprovalRequest) -> ApprovalResponse {
    let summary = summarize_args(&request.arguments);
    eprintln!();
    eprintln!("🔧 Agent wants to execute: {}", request.tool_name);
    eprintln!("   {summary}");
    eprint!("   [Y]es / [N]o / [A]lways for {}: ", request.tool_name);
    let _ = io::stderr().flush();

    let stdin = io::stdin();
    let mut line = String::new();
    if stdin.lock().read_line(&mut line).is_err() {
        return ApprovalResponse::No;
    }

    match line.trim().to_ascii_lowercase().as_str() {
        "y" | "yes" => ApprovalResponse::Yes,
        "a" | "always" => ApprovalResponse::Always,
        _ => ApprovalResponse::No,
    }
}

/// Produce a short human-readable summary of tool arguments.
fn summarize_args(args: &serde_json::Value) -> String {
    match args {
        serde_json::Value::Object(map) => {
            let parts: Vec<String> = map
                .iter()
                .map(|(k, v)| {
                    let val = match v {
                        serde_json::Value::String(s) => truncate_for_summary(s, 80),
                        other => {
                            let s = other.to_string();
                            truncate_for_summary(&s, 80)
                        }
                    };
                    format!("{k}: {val}")
                })
                .collect();
            parts.join(", ")
        }
        other => {
            let s = other.to_string();
            truncate_for_summary(&s, 120)
        }
    }
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
    use crate::config::AutonomyConfig;

    fn supervised_config() -> AutonomyConfig {
        AutonomyConfig {
            level: AutonomyLevel::Supervised,
            auto_approve: vec!["file_read".into(), "memory_recall".into()],
            always_ask: vec!["shell".into()],
            ..AutonomyConfig::default()
        }
    }

    fn full_config() -> AutonomyConfig {
        AutonomyConfig {
            level: AutonomyLevel::Full,
            ..AutonomyConfig::default()
        }
    }

    // ── needs_approval ───────────────────────────────────────

    #[test]
    fn auto_approve_tools_skip_prompt() {
        let mgr = ApprovalManager::from_config(&supervised_config());
        assert!(!mgr.needs_approval("file_read"));
        assert!(!mgr.needs_approval("memory_recall"));
    }

    #[test]
    fn always_ask_tools_always_prompt() {
        let mgr = ApprovalManager::from_config(&supervised_config());
        assert!(mgr.needs_approval("shell"));
    }

    #[test]
    fn unknown_tool_needs_approval_in_supervised() {
        let mgr = ApprovalManager::from_config(&supervised_config());
        assert!(mgr.needs_approval("file_write"));
        assert!(mgr.needs_approval("http_request"));
    }

    #[test]
    fn full_autonomy_never_prompts() {
        let mgr = ApprovalManager::from_config(&full_config());
        assert!(!mgr.needs_approval("shell"));
        assert!(!mgr.needs_approval("file_write"));
        assert!(!mgr.needs_approval("anything"));
    }

    #[test]
    fn readonly_never_prompts() {
        let config = AutonomyConfig {
            level: AutonomyLevel::ReadOnly,
            ..AutonomyConfig::default()
        };
        let mgr = ApprovalManager::from_config(&config);
        assert!(!mgr.needs_approval("shell"));
    }

    // ── session allowlist ────────────────────────────────────

    #[test]
    fn always_response_adds_to_session_allowlist() {
        let mgr = ApprovalManager::from_config(&supervised_config());
        assert!(mgr.needs_approval("file_write"));

        mgr.record_decision(
            "file_write",
            &serde_json::json!({"path": "test.txt"}),
            ApprovalResponse::Always,
            "cli",
        );

        // Now file_write should be in session allowlist.
        assert!(!mgr.needs_approval("file_write"));
    }

    #[test]
    fn always_ask_overrides_session_allowlist() {
        let mgr = ApprovalManager::from_config(&supervised_config());

        // Even after "Always" for shell, it should still prompt.
        mgr.record_decision(
            "shell",
            &serde_json::json!({"command": "ls"}),
            ApprovalResponse::Always,
            "cli",
        );

        // shell is in always_ask, so it still needs approval.
        assert!(mgr.needs_approval("shell"));
    }

    #[test]
    fn yes_response_does_not_add_to_allowlist() {
        let mgr = ApprovalManager::from_config(&supervised_config());
        mgr.record_decision(
            "file_write",
            &serde_json::json!({}),
            ApprovalResponse::Yes,
            "cli",
        );
        assert!(mgr.needs_approval("file_write"));
    }

    // ── audit log ────────────────────────────────────────────

    #[test]
    fn audit_log_records_decisions() {
        let mgr = ApprovalManager::from_config(&supervised_config());

        mgr.record_decision(
            "shell",
            &serde_json::json!({"command": "rm -rf ./build/"}),
            ApprovalResponse::No,
            "cli",
        );
        mgr.record_decision(
            "file_write",
            &serde_json::json!({"path": "out.txt", "content": "hello"}),
            ApprovalResponse::Yes,
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
        let mgr = ApprovalManager::from_config(&supervised_config());
        mgr.record_decision(
            "shell",
            &serde_json::json!({"command": "ls"}),
            ApprovalResponse::Yes,
            "telegram",
        );

        let log = mgr.audit_log();
        assert_eq!(log.len(), 1);
        assert!(!log[0].timestamp.is_empty());
        assert_eq!(log[0].channel, "telegram");
    }

    // ── summarize_args ───────────────────────────────────────

    #[test]
    fn summarize_args_object() {
        let args = serde_json::json!({"command": "ls -la", "cwd": "/tmp"});
        let summary = summarize_args(&args);
        assert!(summary.contains("command: ls -la"));
        assert!(summary.contains("cwd: /tmp"));
    }

    #[test]
    fn summarize_args_truncates_long_values() {
        let long_val = "x".repeat(200);
        let args = serde_json::json!({ "content": long_val });
        let summary = summarize_args(&args);
        assert!(summary.contains('…'));
        assert!(summary.len() < 200);
    }

    #[test]
    fn summarize_args_unicode_safe_truncation() {
        let long_val = "🦀".repeat(120);
        let args = serde_json::json!({ "content": long_val });
        let summary = summarize_args(&args);
        assert!(summary.contains("content:"));
        assert!(summary.contains('…'));
    }

    #[test]
    fn summarize_args_non_object() {
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
        let mgr = ApprovalManager::from_config(&supervised_config());
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
        let mgr = ApprovalManager::for_non_interactive(&AutonomyConfig::default());
        assert!(!mgr.needs_approval("shell"));
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
    fn non_interactive_full_autonomy_never_needs_approval() {
        let mgr = ApprovalManager::for_non_interactive(&full_config());
        // Full autonomy means no approval needed, even in non-interactive mode.
        assert!(!mgr.needs_approval("shell"));
        assert!(!mgr.needs_approval("file_write"));
        assert!(!mgr.needs_approval("anything"));
    }

    #[test]
    fn non_interactive_readonly_never_needs_approval() {
        let config = AutonomyConfig {
            level: AutonomyLevel::ReadOnly,
            ..AutonomyConfig::default()
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
            ApprovalResponse::Always,
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
            ApprovalResponse::Always,
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

    // ── Regression: #4247 default approved tools in channels ──

    #[test]
    fn non_interactive_allows_default_auto_approve_tools() {
        let config = AutonomyConfig::default();
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
        let config = AutonomyConfig::default();
        let mgr = ApprovalManager::for_non_interactive(&config);
        assert!(
            mgr.needs_approval("some_unknown_tool"),
            "unknown tool should need approval"
        );
    }

    #[test]
    fn non_interactive_weather_is_auto_approved() {
        let config = AutonomyConfig::default();
        let mgr = ApprovalManager::for_non_interactive(&config);
        assert!(
            !mgr.needs_approval("weather"),
            "weather tool must not need approval — it is in the default auto_approve list"
        );
    }

    #[test]
    fn always_ask_overrides_auto_approve() {
        let mut config = AutonomyConfig::default();
        config.always_ask = vec!["weather".into()];
        let mgr = ApprovalManager::for_non_interactive(&config);
        assert!(
            mgr.needs_approval("weather"),
            "always_ask must override auto_approve"
        );
    }

    // ── PendingApproval ──────────────────────────────────────────

    #[test]
    fn pending_approval_serde_roundtrip() {
        let pending = PendingApproval {
            request_id: "test-123".into(),
            tool_name: "shell".into(),
            arguments: serde_json::json!({"command": "ls"}),
            sender: "alice".into(),
            channel: "telegram".into(),
            created_at: "2024-01-01T00:00:00Z".into(),
            timeout_at: "2024-01-01T00:01:00Z".into(),
        };

        let json = serde_json::to_string(&pending).unwrap();
        let parsed: PendingApproval = serde_json::from_str(&json).unwrap();

        assert_eq!(parsed.request_id, "test-123");
        assert_eq!(parsed.tool_name, "shell");
        assert_eq!(parsed.sender, "alice");
        assert_eq!(parsed.channel, "telegram");
        assert_eq!(parsed.created_at, "2024-01-01T00:00:00Z");
        assert_eq!(parsed.timeout_at, "2024-01-01T00:01:00Z");
    }

    // ── Channel-interactive mode ─────────────────────────────────

    #[test]
    fn approval_manager_for_channel_interactive_initializes_empty() {
        let mgr = ApprovalManager::for_channel_interactive(&supervised_config());
        assert_eq!(mgr.pending_requests.lock().len(), 0);
        assert_eq!(mgr.resolved_decisions.lock().len(), 0);
        assert_eq!(mgr.notifiers.lock().len(), 0);
    }

    #[test]
    fn approval_manager_for_channel_interactive_sets_flag() {
        let mgr = ApprovalManager::for_channel_interactive(&supervised_config());
        assert!(mgr.is_channel_interactive());
        assert!(!mgr.is_non_interactive());
    }

    #[test]
    fn approval_manager_is_channel_interactive_returns_flag() {
        let interactive = ApprovalManager::for_channel_interactive(&supervised_config());
        let non_interactive = ApprovalManager::for_non_interactive(&supervised_config());
        let cli = ApprovalManager::from_config(&supervised_config());

        assert!(interactive.is_channel_interactive());
        assert!(!non_interactive.is_channel_interactive());
        assert!(!cli.is_channel_interactive());
    }

    #[test]
    fn approval_manager_create_pending_request_generates_id() {
        let mgr = ApprovalManager::for_channel_interactive(&supervised_config());
        let id1 = mgr.create_pending_request(
            "shell".into(),
            serde_json::json!({"command": "ls"}),
            "alice".into(),
            "telegram".into(),
            std::time::Duration::from_secs(60),
        );
        let id2 = mgr.create_pending_request(
            "shell".into(),
            serde_json::json!({"command": "pwd"}),
            "alice".into(),
            "telegram".into(),
            std::time::Duration::from_secs(60),
        );

        assert_ne!(id1, id2);
        assert!(!id1.is_empty());
        assert!(!id2.is_empty());
    }

    #[test]
    fn approval_manager_create_pending_request_stores() {
        let mgr = ApprovalManager::for_channel_interactive(&supervised_config());
        let id = mgr.create_pending_request(
            "shell".into(),
            serde_json::json!({"command": "ls"}),
            "alice".into(),
            "telegram".into(),
            std::time::Duration::from_secs(60),
        );

        let requests = mgr.pending_requests.lock();
        assert_eq!(requests.len(), 1);
        let pending = requests.get(&id).unwrap();
        assert_eq!(pending.tool_name, "shell");
        assert_eq!(pending.sender, "alice");
        assert_eq!(pending.channel, "telegram");
    }

    #[test]
    fn approval_manager_create_pending_request_sets_timeout() {
        let mgr = ApprovalManager::for_channel_interactive(&supervised_config());
        let id = mgr.create_pending_request(
            "shell".into(),
            serde_json::json!({"command": "ls"}),
            "alice".into(),
            "telegram".into(),
            std::time::Duration::from_secs(60),
        );

        let requests = mgr.pending_requests.lock();
        let pending = requests.get(&id).unwrap();

        let created = chrono::DateTime::parse_from_rfc3339(&pending.created_at).unwrap();
        let timeout = chrono::DateTime::parse_from_rfc3339(&pending.timeout_at).unwrap();

        let diff = (timeout - created).num_seconds();
        assert_eq!(diff, 60);
    }

    #[test]
    fn approval_manager_resolve_pending_request_moves_to_resolved() {
        let mgr = ApprovalManager::for_channel_interactive(&supervised_config());
        let id = mgr.create_pending_request(
            "shell".into(),
            serde_json::json!({"command": "ls"}),
            "alice".into(),
            "telegram".into(),
            std::time::Duration::from_secs(60),
        );

        mgr.resolve_pending_request(&id, "alice", ApprovalResponse::Yes)
            .unwrap();

        assert_eq!(mgr.pending_requests.lock().len(), 0);
        assert_eq!(mgr.resolved_decisions.lock().len(), 1);
        assert_eq!(
            *mgr.resolved_decisions.lock().get(&id).unwrap(),
            ApprovalResponse::Yes
        );
    }

    #[test]
    fn approval_manager_resolve_pending_request_unknown_id_is_noop() {
        let mgr = ApprovalManager::for_channel_interactive(&supervised_config());
        let result = mgr.resolve_pending_request("unknown", "alice", ApprovalResponse::Yes);
        assert!(result.is_err());
    }

    #[test]
    fn approval_manager_resolve_pending_request_enforces_sender_match() {
        let mgr = ApprovalManager::for_channel_interactive(&supervised_config());
        let id = mgr.create_pending_request(
            "shell".into(),
            serde_json::json!({"command": "ls"}),
            "alice".into(),
            "telegram".into(),
            std::time::Duration::from_secs(60),
        );

        let result = mgr.resolve_pending_request(&id, "bob", ApprovalResponse::Yes);
        assert!(result.is_err());
        assert!(result.unwrap_err().to_string().contains("Sender mismatch"));

        // Request should still be pending.
        assert_eq!(mgr.pending_requests.lock().len(), 1);
    }

    #[tokio::test]
    async fn approval_manager_wait_for_approval_returns_decision() {
        let mgr = ApprovalManager::for_channel_interactive(&supervised_config());
        let id = mgr.create_pending_request(
            "shell".into(),
            serde_json::json!({"command": "ls"}),
            "alice".into(),
            "telegram".into(),
            std::time::Duration::from_secs(60),
        );

        let mgr_clone = mgr.clone();
        let id_clone = id.clone();
        tokio::spawn(async move {
            tokio::time::sleep(tokio::time::Duration::from_millis(100)).await;
            mgr_clone
                .resolve_pending_request(&id_clone, "alice", ApprovalResponse::Yes)
                .unwrap();
        });

        let decision = mgr
            .wait_for_approval(&id, std::time::Duration::from_secs(5))
            .await;
        assert_eq!(decision, ApprovalResponse::Yes);
    }

    #[tokio::test]
    async fn approval_manager_wait_for_approval_timeout_returns_no() {
        let mgr = ApprovalManager::for_channel_interactive(&supervised_config());
        let id = mgr.create_pending_request(
            "shell".into(),
            serde_json::json!({"command": "ls"}),
            "alice".into(),
            "telegram".into(),
            std::time::Duration::from_secs(60),
        );

        let decision = mgr
            .wait_for_approval(&id, std::time::Duration::from_millis(100))
            .await;
        assert_eq!(decision, ApprovalResponse::No);
    }

    #[test]
    fn approval_manager_cleanup_expired_removes_old() {
        let mgr = ApprovalManager::for_channel_interactive(&supervised_config());

        // Create request with very short timeout (already expired).
        let _id = mgr.create_pending_request(
            "shell".into(),
            serde_json::json!({"command": "ls"}),
            "alice".into(),
            "telegram".into(),
            std::time::Duration::from_millis(1),
        );

        std::thread::sleep(std::time::Duration::from_millis(10));

        mgr.cleanup_expired();

        assert_eq!(mgr.pending_requests.lock().len(), 0);
    }

    #[test]
    fn approval_manager_cleanup_expired_preserves_active() {
        let mgr = ApprovalManager::for_channel_interactive(&supervised_config());

        let _id = mgr.create_pending_request(
            "shell".into(),
            serde_json::json!({"command": "ls"}),
            "alice".into(),
            "telegram".into(),
            std::time::Duration::from_secs(60),
        );

        mgr.cleanup_expired();

        assert_eq!(mgr.pending_requests.lock().len(), 1);
    }

    #[tokio::test]
    async fn approval_manager_cleanup_expired_notifies_timed_out() {
        let mgr = ApprovalManager::for_channel_interactive(&supervised_config());
        let id = mgr.create_pending_request(
            "shell".into(),
            serde_json::json!({"command": "ls"}),
            "alice".into(),
            "telegram".into(),
            std::time::Duration::from_millis(1),
        );

        let mgr_clone = mgr.clone();
        let id_clone = id.clone();
        let handle = tokio::spawn(async move {
            mgr_clone
                .wait_for_approval(&id_clone, std::time::Duration::from_secs(5))
                .await
        });

        tokio::time::sleep(tokio::time::Duration::from_millis(10)).await;
        mgr.cleanup_expired();

        let decision = handle.await.unwrap();
        assert_eq!(decision, ApprovalResponse::No);
    }

    #[test]
    fn approval_manager_needs_approval_skips_when_channel_interactive_false() {
        let mgr = ApprovalManager::for_non_interactive(&supervised_config());
        assert!(!mgr.is_channel_interactive());
        // Should follow existing logic - unknown tools need approval in supervised mode.
        assert!(mgr.needs_approval("file_write"));
    }

    #[test]
    fn approval_manager_needs_approval_requires_when_channel_interactive_true() {
        let mgr = ApprovalManager::for_channel_interactive(&supervised_config());
        assert!(mgr.is_channel_interactive());
        // Should still require approval - channel_interactive doesn't bypass approval.
        assert!(mgr.needs_approval("file_write"));
    }

    // ── Integration tests ────────────────────────────────────────

    #[tokio::test]
    async fn approval_manager_channel_flow_end_to_end_allow() {
        let mgr = ApprovalManager::for_channel_interactive(&supervised_config());
        let id = mgr.create_pending_request(
            "shell".into(),
            serde_json::json!({"command": "ls"}),
            "alice".into(),
            "telegram".into(),
            std::time::Duration::from_secs(60),
        );

        let mgr_clone = mgr.clone();
        let id_clone = id.clone();
        tokio::spawn(async move {
            tokio::time::sleep(tokio::time::Duration::from_millis(50)).await;
            mgr_clone
                .resolve_pending_request(&id_clone, "alice", ApprovalResponse::Yes)
                .unwrap();
        });

        let decision = mgr
            .wait_for_approval(&id, std::time::Duration::from_secs(5))
            .await;
        assert_eq!(decision, ApprovalResponse::Yes);
    }

    #[tokio::test]
    async fn approval_manager_channel_flow_end_to_end_deny() {
        let mgr = ApprovalManager::for_channel_interactive(&supervised_config());
        let id = mgr.create_pending_request(
            "shell".into(),
            serde_json::json!({"command": "rm -rf"}),
            "alice".into(),
            "telegram".into(),
            std::time::Duration::from_secs(60),
        );

        let mgr_clone = mgr.clone();
        let id_clone = id.clone();
        tokio::spawn(async move {
            tokio::time::sleep(tokio::time::Duration::from_millis(50)).await;
            mgr_clone
                .resolve_pending_request(&id_clone, "alice", ApprovalResponse::No)
                .unwrap();
        });

        let decision = mgr
            .wait_for_approval(&id, std::time::Duration::from_secs(5))
            .await;
        assert_eq!(decision, ApprovalResponse::No);
    }

    #[tokio::test]
    async fn approval_manager_channel_flow_end_to_end_always() {
        let mgr = ApprovalManager::for_channel_interactive(&supervised_config());
        let id = mgr.create_pending_request(
            "file_write".into(),
            serde_json::json!({"path": "test.txt"}),
            "alice".into(),
            "telegram".into(),
            std::time::Duration::from_secs(60),
        );

        let id_clone = id.clone();
        tokio::spawn({
            let mgr = mgr.clone();
            async move {
                tokio::time::sleep(tokio::time::Duration::from_millis(50)).await;
                mgr.resolve_pending_request(&id_clone, "alice", ApprovalResponse::Always)
                    .unwrap();
            }
        });

        let decision = mgr
            .wait_for_approval(&id, std::time::Duration::from_secs(5))
            .await;
        assert_eq!(decision, ApprovalResponse::Always);

        // Verify tool was added to session allowlist.
        // Give a small delay to ensure the resolve completed.
        tokio::time::sleep(tokio::time::Duration::from_millis(10)).await;
        let allowlist = mgr.session_allowlist();
        assert!(allowlist.contains("file_write"));
    }

    #[tokio::test]
    async fn approval_manager_channel_flow_concurrent_requests() {
        let mgr = ApprovalManager::for_channel_interactive(&supervised_config());

        let id1 = mgr.create_pending_request(
            "shell".into(),
            serde_json::json!({"command": "ls"}),
            "alice".into(),
            "telegram".into(),
            std::time::Duration::from_secs(60),
        );

        let id2 = mgr.create_pending_request(
            "file_write".into(),
            serde_json::json!({"path": "test.txt"}),
            "bob".into(),
            "discord".into(),
            std::time::Duration::from_secs(60),
        );

        let mgr1 = mgr.clone();
        let id1_clone = id1.clone();
        let handle1 = tokio::spawn(async move {
            mgr1.wait_for_approval(&id1_clone, std::time::Duration::from_secs(5))
                .await
        });

        let mgr2 = mgr.clone();
        let id2_clone = id2.clone();
        let handle2 = tokio::spawn(async move {
            mgr2.wait_for_approval(&id2_clone, std::time::Duration::from_secs(5))
                .await
        });

        tokio::time::sleep(tokio::time::Duration::from_millis(50)).await;
        mgr.resolve_pending_request(&id1, "alice", ApprovalResponse::Yes)
            .unwrap();
        mgr.resolve_pending_request(&id2, "bob", ApprovalResponse::No)
            .unwrap();

        let decision1 = handle1.await.unwrap();
        let decision2 = handle2.await.unwrap();

        assert_eq!(decision1, ApprovalResponse::Yes);
        assert_eq!(decision2, ApprovalResponse::No);
    }

    #[tokio::test]
    async fn approval_manager_channel_flow_timeout_before_resolve() {
        let mgr = ApprovalManager::for_channel_interactive(&supervised_config());
        let id = mgr.create_pending_request(
            "shell".into(),
            serde_json::json!({"command": "ls"}),
            "alice".into(),
            "telegram".into(),
            std::time::Duration::from_secs(60),
        );

        let decision = mgr
            .wait_for_approval(&id, std::time::Duration::from_millis(100))
            .await;
        assert_eq!(decision, ApprovalResponse::No);

        // Subsequent resolve should fail since request is cleaned up.
        let result = mgr.resolve_pending_request(&id, "alice", ApprovalResponse::Yes);
        assert!(result.is_err());
    }

    #[tokio::test]
    async fn approval_manager_channel_flow_cleanup_after_timeout() {
        let mgr = ApprovalManager::for_channel_interactive(&supervised_config());
        let id = mgr.create_pending_request(
            "shell".into(),
            serde_json::json!({"command": "ls"}),
            "alice".into(),
            "telegram".into(),
            std::time::Duration::from_millis(1),
        );

        tokio::time::sleep(tokio::time::Duration::from_millis(10)).await;
        mgr.cleanup_expired();

        let result = mgr.resolve_pending_request(&id, "alice", ApprovalResponse::Yes);
        assert!(result.is_err());
    }
}
