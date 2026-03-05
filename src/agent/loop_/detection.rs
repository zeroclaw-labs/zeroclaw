//! Loop detection for the agent tool-call loop.
//!
//! Detects three patterns of unproductive looping:
//! 1. **No-progress repeat** — same tool + same args + same output hash.
//! 2. **Ping-pong** — two calls alternating (A→B→A→B) with no progress.
//! 3. **Consecutive failure streak** — same tool failing repeatedly.
//!
//! On first detection an `InjectWarning` verdict gives the LLM a chance to
//! self-correct.  If the pattern persists the next check returns `HardStop`.

use std::collections::HashMap;
use std::fmt;
use std::hash::{DefaultHasher, Hash, Hasher};

/// Maximum bytes of tool output considered when hashing results.
/// Keeps hashing fast and bounded for large outputs.
const OUTPUT_HASH_PREFIX_BYTES: usize = 4096;

// ─── Failure Classification ─────────────────────────────────────────────────

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub(crate) enum FailureCategory {
    MissingBinary,
    PermissionDenied,
    Timeout,
    RateLimit,
    NetworkError,
    InvalidArguments,
    EnvironmentMissing,
    Unknown,
}

impl fmt::Display for FailureCategory {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        let s = match self {
            Self::MissingBinary => "missing_binary",
            Self::PermissionDenied => "permission_denied",
            Self::Timeout => "timeout",
            Self::RateLimit => "rate_limit",
            Self::NetworkError => "network_error",
            Self::InvalidArguments => "invalid_arguments",
            Self::EnvironmentMissing => "environment_missing",
            Self::Unknown => "unknown",
        };
        f.write_str(s)
    }
}

pub(crate) fn classify_error(output: &str) -> FailureCategory {
    let lower = output.to_ascii_lowercase();
    let lower = if lower.len() > 512 { &lower[..512] } else { &lower };
    if lower.contains("command not found")
        || lower.contains("unknown command")
        || lower.contains("not recognized as")
        || lower.contains("no such file or directory")
    {
        FailureCategory::MissingBinary
    } else if lower.contains("permission denied")
        || lower.contains("not permitted")
        || lower.contains("not allowed")
        || lower.contains("access denied")
    {
        FailureCategory::PermissionDenied
    } else if lower.contains("timed out") || lower.contains("timeout") {
        FailureCategory::Timeout
    } else if lower.contains("rate limit")
        || lower.contains("too many requests")
        || lower.contains("budget exhausted")
    {
        FailureCategory::RateLimit
    } else if lower.contains("connection refused")
        || lower.contains("could not resolve")
        || lower.contains("network unreachable")
        || lower.contains("dns resolution")
    {
        FailureCategory::NetworkError
    } else if lower.contains("missing required")
        || lower.contains("invalid argument")
        || lower.contains("expected ")
    {
        FailureCategory::InvalidArguments
    } else if lower.contains("env var") || lower.contains("environment variable") {
        FailureCategory::EnvironmentMissing
    } else {
        FailureCategory::Unknown
    }
}

#[derive(Debug, Clone)]
pub(crate) struct LoopDiagnostics {
    pub tool: String,
    pub category: Option<FailureCategory>,
    pub consecutive_failures: usize,
    pub last_errors: Vec<String>,
    pub strategy: &'static str,
}

// ─── Configuration ───────────────────────────────────────────────────────────

/// Tuning knobs for each detection strategy.
#[derive(Debug, Clone)]
pub(crate) struct LoopDetectionConfig {
    /// Identical (tool + args + output) repetitions before triggering.
    /// `0` = disabled.  Default: `3`.
    pub no_progress_threshold: usize,
    /// Full A-B cycles before triggering ping-pong detection.
    /// `0` = disabled.  Default: `2`.
    pub ping_pong_cycles: usize,
    /// Consecutive failures of the *same* tool before triggering.
    /// `0` = disabled.  Default: `3`.
    pub failure_streak_threshold: usize,
}

impl Default for LoopDetectionConfig {
    fn default() -> Self {
        Self {
            no_progress_threshold: 3,
            ping_pong_cycles: 2,
            failure_streak_threshold: 3,
        }
    }
}

// ─── Verdict ─────────────────────────────────────────────────────────────────

/// Action the caller should take after `LoopDetector::check()`.
#[derive(Debug, PartialEq, Eq)]
pub(crate) enum DetectionVerdict {
    /// No loop detected — proceed normally.
    Continue,
    /// First detection — inject this self-correction prompt, then continue.
    InjectWarning(String),
    /// Pattern persisted after warning — terminate the loop.
    HardStop(String),
}

// ─── Internal record ─────────────────────────────────────────────────────────

struct CallRecord {
    tool_name: String,
    args_sig: String,
    result_hash: u64,
    success: bool,
    error_output: Option<String>,
}

// ─── Detector ────────────────────────────────────────────────────────────────

pub(crate) struct LoopDetector {
    config: LoopDetectionConfig,
    history: Vec<CallRecord>,
    consecutive_failures: HashMap<String, usize>,
    warning_injected: bool,
    last_diagnostics: Option<LoopDiagnostics>,
}

impl LoopDetector {
    pub fn new(config: LoopDetectionConfig) -> Self {
        Self {
            config,
            history: Vec::new(),
            consecutive_failures: HashMap::new(),
            warning_injected: false,
            last_diagnostics: None,
        }
    }

    /// Return the diagnostics from the most recent detection event.
    pub fn diagnostics(&self) -> Option<&LoopDiagnostics> {
        self.last_diagnostics.as_ref()
    }

    /// Record a completed tool invocation.
    ///
    /// * `tool_name` — canonical tool name (lowercased by caller).
    /// * `args_sig`  — canonical JSON args string from `tool_call_signature()`.
    /// * `output`    — raw tool output text.
    /// * `success`   — whether the tool reported success.
    pub fn record_call(&mut self, tool_name: &str, args_sig: &str, output: &str, success: bool) {
        let result_hash = hash_output(output);
        let error_output = if !success {
            let trunc = if output.len() > 512 {
                let boundary = crate::util::floor_utf8_char_boundary(output, 512);
                &output[..boundary]
            } else {
                output
            };
            Some(trunc.to_owned())
        } else {
            None
        };
        self.history.push(CallRecord {
            tool_name: tool_name.to_owned(),
            args_sig: args_sig.to_owned(),
            result_hash,
            success,
            error_output,
        });

        if success {
            self.consecutive_failures.remove(tool_name);
        } else {
            *self
                .consecutive_failures
                .entry(tool_name.to_owned())
                .or_insert(0) += 1;
        }
    }

    /// Evaluate the current history and return a verdict.
    pub fn check(&mut self) -> DetectionVerdict {
        // Try each strategy, collecting diagnostics
        let detection = self
            .check_no_progress_repeat()
            .or_else(|| self.check_ping_pong())
            .or_else(|| self.check_failure_streak());

        match detection {
            None => {
                self.last_diagnostics = None;
                DetectionVerdict::Continue
            }
            Some((msg, diag)) => {
                let category = diag.category;
                self.last_diagnostics = Some(diag);
                if self.warning_injected {
                    DetectionVerdict::HardStop(msg)
                } else {
                    self.warning_injected = true;
                    DetectionVerdict::InjectWarning(format_warning(&msg, category))
                }
            }
        }
    }

    // ── Strategy 1: no-progress repeat ───────────────────────────────────

    fn check_no_progress_repeat(&self) -> Option<(String, LoopDiagnostics)> {
        let threshold = self.config.no_progress_threshold;
        if threshold == 0 || self.history.is_empty() {
            return None;
        }
        let last = self.history.last().unwrap();
        let streak = self
            .history
            .iter()
            .rev()
            .take_while(|r| {
                r.tool_name == last.tool_name
                    && r.args_sig == last.args_sig
                    && r.result_hash == last.result_hash
            })
            .count();
        if streak >= threshold {
            let msg = format!(
                "Tool '{}' called {} times with identical arguments and identical results \
                 — no progress detected",
                last.tool_name, streak
            );
            let diag = LoopDiagnostics {
                tool: last.tool_name.clone(),
                category: None,
                consecutive_failures: 0,
                last_errors: Vec::new(),
                strategy: "no_progress_repeat",
            };
            Some((msg, diag))
        } else {
            None
        }
    }

    // ── Strategy 2: ping-pong ────────────────────────────────────────────

    fn check_ping_pong(&self) -> Option<(String, LoopDiagnostics)> {
        let cycles = self.config.ping_pong_cycles;
        if cycles == 0 || self.history.len() < 4 {
            return None;
        }
        let len = self.history.len();
        let a = &self.history[len - 2];
        let b = &self.history[len - 1];

        if a.tool_name == b.tool_name && a.args_sig == b.args_sig {
            return None;
        }

        let min_entries = cycles * 2;
        if len < min_entries {
            return None;
        }
        let tail = &self.history[len - min_entries..];
        let is_ping_pong = tail.chunks(2).all(|pair| {
            pair.len() == 2
                && pair[0].tool_name == a.tool_name
                && pair[0].args_sig == a.args_sig
                && pair[0].result_hash == a.result_hash
                && pair[1].tool_name == b.tool_name
                && pair[1].args_sig == b.args_sig
                && pair[1].result_hash == b.result_hash
        });

        if is_ping_pong {
            let msg = format!(
                "Ping-pong loop detected: '{}' and '{}' alternating {} times with no progress",
                a.tool_name, b.tool_name, cycles
            );
            let diag = LoopDiagnostics {
                tool: format!("{},{}", a.tool_name, b.tool_name),
                category: None,
                consecutive_failures: 0,
                last_errors: Vec::new(),
                strategy: "ping_pong",
            };
            Some((msg, diag))
        } else {
            None
        }
    }

    // ── Strategy 3: consecutive failure streak ───────────────────────────

    fn check_failure_streak(&self) -> Option<(String, LoopDiagnostics)> {
        let threshold = self.config.failure_streak_threshold;
        if threshold == 0 {
            return None;
        }
        for (tool, count) in &self.consecutive_failures {
            if *count >= threshold {
                let last_errors: Vec<String> = self
                    .history
                    .iter()
                    .rev()
                    .filter(|r| &r.tool_name == tool && !r.success)
                    .take(3)
                    .filter_map(|r| r.error_output.clone())
                    .collect();
                let category = last_errors
                    .first()
                    .map(|e| classify_error(e))
                    .unwrap_or(FailureCategory::Unknown);
                let msg = format!(
                    "Tool '{}' failed {} consecutive times",
                    tool, count
                );
                let diag = LoopDiagnostics {
                    tool: tool.clone(),
                    category: Some(category),
                    consecutive_failures: *count,
                    last_errors,
                    strategy: "failure_streak",
                };
                return Some((msg, diag));
            }
        }
        None
    }
}

// ─── Helpers ─────────────────────────────────────────────────────────────────

fn hash_output(output: &str) -> u64 {
    let prefix = if output.len() > OUTPUT_HASH_PREFIX_BYTES {
        // Use floor_utf8_char_boundary to avoid panic on multi-byte UTF-8 characters
        let boundary = crate::util::floor_utf8_char_boundary(output, OUTPUT_HASH_PREFIX_BYTES);
        &output[..boundary]
    } else {
        output
    };
    let mut hasher = DefaultHasher::new();
    prefix.hash(&mut hasher);
    hasher.finish()
}

fn format_warning(reason: &str, category: Option<FailureCategory>) -> String {
    let recovery_hint = match category {
        Some(FailureCategory::MissingBinary) =>
            "The command/binary is NOT available in this environment. \
             Do NOT retry it. Instead: check what IS available (e.g. `which`, `ls /usr/bin`), \
             or use a completely different approach that does not require this binary.",
        Some(FailureCategory::PermissionDenied) =>
            "This action is blocked by security policy or filesystem permissions. \
             Do NOT retry the same path/command. Work within the allowed workspace \
             and use only approved commands.",
        Some(FailureCategory::Timeout) =>
            "The command timed out — it may be hanging or waiting for interactive input. \
             Try: add --no-input or --non-interactive flags, pipe from /dev/null, \
             or break into smaller steps.",
        Some(FailureCategory::RateLimit) =>
            "You have hit a rate limit or action budget. Stop calling this tool \
             and complete the task with what you have, or wait and retry later.",
        Some(FailureCategory::NetworkError) =>
            "Network connectivity failed. Do NOT retry network-dependent commands. \
             Use local alternatives or inform the user that network access is needed.",
        Some(FailureCategory::InvalidArguments) =>
            "The tool arguments are missing or malformed. Review the tool schema \
             and provide all required parameters with correct types.",
        Some(FailureCategory::EnvironmentMissing) =>
            "A required environment variable or runtime dependency is missing. \
             Do NOT retry. Check what environment is available and adapt your approach.",
        Some(FailureCategory::Unknown) | None =>
            "Try a different tool or different arguments. \
             If polling a process, increase wait time or check if it's stuck. \
             If the task cannot be completed, explain why and stop.",
    };
    format!(
        "IMPORTANT: A loop pattern has been detected in your tool usage. {reason}. \
         You MUST change your approach — {recovery_hint} \
         Do NOT repeat the same tool call with the same arguments."
    )
}

// ─── Unit tests ──────────────────────────────────────────────────────────────

#[cfg(test)]
mod tests {
    use super::*;

    fn default_config() -> LoopDetectionConfig {
        LoopDetectionConfig::default()
    }

    fn disabled_config() -> LoopDetectionConfig {
        LoopDetectionConfig {
            no_progress_threshold: 0,
            ping_pong_cycles: 0,
            failure_streak_threshold: 0,
        }
    }

    // 1. Below threshold → Continue
    #[test]
    fn below_threshold_does_not_trigger() {
        let mut det = LoopDetector::new(default_config());
        det.record_call("echo", r#"{"msg":"hi"}"#, "hello", true);
        det.record_call("echo", r#"{"msg":"hi"}"#, "hello", true);
        assert_eq!(det.check(), DetectionVerdict::Continue);
    }

    // 2. No-progress repeat triggers warning at threshold
    #[test]
    fn no_progress_repeat_triggers_warning() {
        let mut det = LoopDetector::new(default_config());
        for _ in 0..3 {
            det.record_call("echo", r#"{"msg":"hi"}"#, "hello", true);
        }
        match det.check() {
            DetectionVerdict::InjectWarning(msg) => {
                assert!(msg.contains("no progress"), "msg: {msg}");
            }
            other => panic!("expected InjectWarning, got {other:?}"),
        }
    }

    // 3. Same input but different output → no trigger (progress detected)
    #[test]
    fn same_input_different_output_does_not_trigger() {
        let mut det = LoopDetector::new(default_config());
        det.record_call("echo", r#"{"msg":"hi"}"#, "result_1", true);
        det.record_call("echo", r#"{"msg":"hi"}"#, "result_2", true);
        det.record_call("echo", r#"{"msg":"hi"}"#, "result_3", true);
        assert_eq!(det.check(), DetectionVerdict::Continue);
    }

    // 4. Warning then continued loop → HardStop
    #[test]
    fn warning_then_continued_loop_triggers_hard_stop() {
        let mut det = LoopDetector::new(default_config());
        for _ in 0..3 {
            det.record_call("echo", r#"{"msg":"hi"}"#, "same", true);
        }
        assert!(matches!(det.check(), DetectionVerdict::InjectWarning(_)));
        // One more identical call
        det.record_call("echo", r#"{"msg":"hi"}"#, "same", true);
        match det.check() {
            DetectionVerdict::HardStop(msg) => {
                assert!(msg.contains("no progress"), "msg: {msg}");
            }
            other => panic!("expected HardStop, got {other:?}"),
        }
    }

    // 5. Ping-pong detection
    #[test]
    fn ping_pong_triggers_warning() {
        let mut det = LoopDetector::new(default_config());
        // 2 cycles: A-B-A-B
        det.record_call("tool_a", r#"{"x":1}"#, "out_a", true);
        det.record_call("tool_b", r#"{"y":2}"#, "out_b", true);
        det.record_call("tool_a", r#"{"x":1}"#, "out_a", true);
        det.record_call("tool_b", r#"{"y":2}"#, "out_b", true);
        match det.check() {
            DetectionVerdict::InjectWarning(msg) => {
                assert!(msg.contains("Ping-pong"), "msg: {msg}");
            }
            other => panic!("expected InjectWarning, got {other:?}"),
        }
    }

    // 6. Ping-pong with progress does not trigger
    #[test]
    fn ping_pong_with_progress_does_not_trigger() {
        let mut det = LoopDetector::new(default_config());
        det.record_call("tool_a", r#"{"x":1}"#, "out_a_1", true);
        det.record_call("tool_b", r#"{"y":2}"#, "out_b_1", true);
        det.record_call("tool_a", r#"{"x":1}"#, "out_a_2", true); // different output
        det.record_call("tool_b", r#"{"y":2}"#, "out_b_2", true); // different output
        assert_eq!(det.check(), DetectionVerdict::Continue);
    }

    // 7. Consecutive failure streak (different args each time to avoid no-progress trigger)
    #[test]
    fn failure_streak_triggers_warning() {
        let mut det = LoopDetector::new(default_config());
        det.record_call("shell", r#"{"cmd":"bad1"}"#, "error: not found 1", false);
        det.record_call("shell", r#"{"cmd":"bad2"}"#, "error: not found 2", false);
        det.record_call("shell", r#"{"cmd":"bad3"}"#, "error: not found 3", false);
        match det.check() {
            DetectionVerdict::InjectWarning(msg) => {
                assert!(msg.contains("failed 3 consecutive"), "msg: {msg}");
            }
            other => panic!("expected InjectWarning, got {other:?}"),
        }
    }

    // 8. Failure streak resets on success
    #[test]
    fn failure_streak_resets_on_success() {
        let mut det = LoopDetector::new(default_config());
        det.record_call("shell", r#"{"cmd":"bad"}"#, "err", false);
        det.record_call("shell", r#"{"cmd":"bad"}"#, "err", false);
        det.record_call("shell", r#"{"cmd":"good"}"#, "ok", true); // resets
        det.record_call("shell", r#"{"cmd":"bad"}"#, "err", false);
        det.record_call("shell", r#"{"cmd":"bad"}"#, "err", false);
        assert_eq!(det.check(), DetectionVerdict::Continue);
    }

    // 9. All thresholds zero → disabled
    #[test]
    fn all_disabled_never_triggers() {
        let mut det = LoopDetector::new(disabled_config());
        for _ in 0..20 {
            det.record_call("echo", r#"{"msg":"hi"}"#, "same", true);
        }
        assert_eq!(det.check(), DetectionVerdict::Continue);
    }

    // 10. Mixed tools → no false positive
    #[test]
    fn mixed_tools_no_false_positive() {
        let mut det = LoopDetector::new(default_config());
        det.record_call("file_read", r#"{"path":"a.rs"}"#, "content_a", true);
        det.record_call("shell", r#"{"cmd":"ls"}"#, "file_list", true);
        det.record_call("memory_store", r#"{"key":"x"}"#, "stored", true);
        det.record_call("file_read", r#"{"path":"b.rs"}"#, "content_b", true);
        det.record_call("shell", r#"{"cmd":"cargo test"}"#, "ok", true);
        assert_eq!(det.check(), DetectionVerdict::Continue);
    }

    // 11. UTF-8 boundary safety: hash_output must not panic on CJK text
    #[test]
    fn hash_output_utf8_boundary_safe() {
        // Create a string where byte 4096 lands inside a multi-byte char
        // Chinese chars are 3 bytes each, so 1366 chars = 4098 bytes
        let cjk_text: String = "文".repeat(1366); // 4098 bytes
        assert!(cjk_text.len() > super::OUTPUT_HASH_PREFIX_BYTES);

        // This should NOT panic
        let hash1 = super::hash_output(&cjk_text);

        // Different content should produce different hash
        let cjk_text2: String = "字".repeat(1366);
        let hash2 = super::hash_output(&cjk_text2);
        assert_ne!(hash1, hash2);

        // Mixed ASCII + CJK at boundary
        let mixed = "a".repeat(4094) + "文文"; // 4094 + 6 = 4100 bytes, boundary at 4096
        let hash3 = super::hash_output(&mixed);
        assert!(hash3 != 0); // Just verify it runs
    }

    #[test]
    fn classify_error_categories() {
        assert_eq!(classify_error("command not found"), FailureCategory::MissingBinary);
        assert_eq!(classify_error("permission denied"), FailureCategory::PermissionDenied);
        assert_eq!(classify_error("timed out"), FailureCategory::Timeout);
        assert_eq!(classify_error("rate limit"), FailureCategory::RateLimit);
        assert_eq!(classify_error("connection refused"), FailureCategory::NetworkError);
        assert_eq!(classify_error("missing required"), FailureCategory::InvalidArguments);
        assert_eq!(classify_error("env var X"), FailureCategory::EnvironmentMissing);
        assert_eq!(classify_error("weird"), FailureCategory::Unknown);
    }

    #[test]
    fn diagnostics_on_failure_streak() {
        let mut det = LoopDetector::new(default_config());
        for i in 0..3 {
            det.record_call("sh", &format!("{{\"c\":\"{i}\"}}"), "command not found", false);
        }
        assert!(matches!(det.check(), DetectionVerdict::InjectWarning(_)));
        let d = det.diagnostics().unwrap();
        assert_eq!(d.strategy, "failure_streak");
        assert_eq!(d.category, Some(FailureCategory::MissingBinary));
        assert_eq!(d.consecutive_failures, 3);
    }
}
