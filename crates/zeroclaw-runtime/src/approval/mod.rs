//! Interactive approval workflow for supervised mode.
//! Provides a pre-execution hook that prompts the user before tool calls,
//! with session-scoped "Always" allowlists and audit logging.

use crate::security::AutonomyLevel;
use chrono::Utc;
use parking_lot::Mutex;
use serde::{Deserialize, Serialize};
use std::collections::HashSet;
#[cfg(unix)]
use std::io::BufReader;
use std::io::{self, BufRead, Write};
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
    /// Audit trail of approval decisions.
    audit_log: Mutex<Vec<ApprovalLogEntry>>,
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
    /// from `risk_profile`; the session allowlist and audit trail start
    /// fresh — "Always" grants to one agent never transfer to another.
    pub fn derive_for_risk_profile(&self, risk_profile: &RiskProfileConfig) -> Self {
        Self {
            auto_approve: risk_profile.auto_approve.iter().cloned().collect(),
            always_ask: risk_profile.always_ask.iter().cloned().collect(),
            autonomy_level: risk_profile.level,
            non_interactive: self.non_interactive,
            non_interactive_shell_requires_approval: self.non_interactive_shell_requires_approval,
            session_allowlist: Mutex::new(HashSet::new()),
            audit_log: Mutex::new(Vec::new()),
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
        // Full autonomy never prompts.
        if self.autonomy_level == AutonomyLevel::Full {
            return ApprovalRequirement::Approved;
        }

        // ReadOnly blocks everything — handled elsewhere; no prompt needed.
        if self.autonomy_level == AutonomyLevel::ReadOnly {
            return ApprovalRequirement::NotRequired;
        }

        // always_ask overrides everything.
        if self.always_ask.contains("*") || self.always_ask.contains(tool_name) {
            return ApprovalRequirement::Prompt;
        }

        if self.non_interactive
            && tool_name == "shell"
            && !self.non_interactive_shell_requires_approval
        {
            return ApprovalRequirement::NotRequired;
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
                        serde_json::Value::String(s) => truncate_for_summary(s, 80),
                        other => {
                            let s = other.to_string();
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
                        serde_json::Value::String(s) => truncate_for_summary(s, 80),
                        other => {
                            let s = other.to_string();
                            truncate_for_summary(&s, 80)
                        }
                    }
                };
                parts.push(format!("{k}: {val}"));
            }
            parts.join(", ")
        }
        other => {
            let s = other.to_string();
            truncate_for_summary(&s, 120)
        }
    }
}

/// Heuristic for argument keys that should have their value redacted in
/// human-readable summaries. Matches anywhere in the (lowercased) key:
/// covers `api_key`, `api-key`, `apiKey`, `oauth_token`, `secret`,
/// `password`, `auth_token`, `bearer`, `client_secret`, `private_key`, etc.
fn looks_like_secret_key(key: &str) -> bool {
    let lower = key.to_ascii_lowercase();
    [
        "secret",
        "password",
        "passwd",
        "token",
        "api_key",
        "api-key",
        "apikey",
        "auth",
        "bearer",
        "private_key",
        "private-key",
        "privatekey",
        "credential",
    ]
    .iter()
    .any(|needle| lower.contains(needle))
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
    fn full_autonomy_never_prompts() {
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
    fn non_interactive_full_autonomy_never_needs_approval() {
        let mgr = ApprovalManager::for_non_interactive(&full_config());
        // Full autonomy means no approval needed, even in non-interactive mode.
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
    fn non_interactive_weather_is_auto_approved() {
        let config = RiskProfileConfig::default();
        let mgr = ApprovalManager::for_non_interactive(&config);
        assert!(
            !mgr.needs_approval("weather"),
            "weather tool must not need approval — it is in the default auto_approve list"
        );
    }

    #[test]
    fn always_ask_overrides_auto_approve() {
        let config = RiskProfileConfig {
            always_ask: vec!["weather".into()],
            ..RiskProfileConfig::default()
        };
        let mgr = ApprovalManager::for_non_interactive(&config);
        assert!(
            mgr.needs_approval("weather"),
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
