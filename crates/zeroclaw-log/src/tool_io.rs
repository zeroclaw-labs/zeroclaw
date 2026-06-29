//! Tool input/output capture: leak-scan + truncation + denylist.
//!
//! The actual `LeakDetector` lives in `zeroclaw-runtime::security` (it
//! depends on regex tables that themselves depend on other runtime types).
//! This crate is upstream of runtime, so we can't reach the detector
//! directly. Instead, callers in runtime invoke
//! [`capture_tool_input`] / [`capture_tool_output`] with the post-scan
//! string (the runtime side runs `LeakDetector::scan` first and passes
//! the redacted output here for truncation + size-flagging).

use crate::config::{LlmRequestPayloadPolicy, ResolvedPolicy, ToolIoPolicy};

/// Result of a tool-io capture pass. The string in `text` is what should
/// land in the `attributes.tool_input` (or `tool_output`) field. Metadata
/// goes into `original_bytes` / `truncated` so the dashboard can render
/// a "truncated" badge.
#[derive(Debug, Clone)]
pub struct ToolIoCapture {
    pub text: String,
    pub original_bytes: usize,
    pub truncated: bool,
}

impl ToolIoCapture {
    fn empty() -> Self {
        Self {
            text: String::new(),
            original_bytes: 0,
            truncated: false,
        }
    }
}

/// Capture redacted tool input.
///
/// `redacted` is the input string AFTER the runtime has scanned it for
/// credential leaks (using `zeroclaw_runtime::security::LeakDetector`).
/// This function only handles truncation + denylist enforcement.
///
/// Returns `None` when policy/denylist says to skip capture entirely.
#[must_use]
pub fn capture_tool_input(
    policy: &ResolvedPolicy,
    tool: &str,
    redacted: &str,
) -> Option<ToolIoCapture> {
    capture_with_policy(policy, tool, redacted)
}

/// Capture redacted tool output. Same shape as [`capture_tool_input`].
#[must_use]
pub fn capture_tool_output(
    policy: &ResolvedPolicy,
    tool: &str,
    redacted: &str,
) -> Option<ToolIoCapture> {
    capture_with_policy(policy, tool, redacted)
}

fn capture_with_policy(
    policy: &ResolvedPolicy,
    tool: &str,
    redacted: &str,
) -> Option<ToolIoCapture> {
    if !policy.tool_io.captures_io() {
        return None;
    }
    if policy.is_tool_denylisted(tool) {
        return None;
    }
    let original_bytes = redacted.len();
    match policy.tool_io {
        ToolIoPolicy::Off => None,
        ToolIoPolicy::Full => Some(ToolIoCapture {
            text: redacted.to_string(),
            original_bytes,
            truncated: false,
        }),
        ToolIoPolicy::Redacted => Some(truncate_to_cap(redacted, policy.tool_io_truncate_bytes)),
    }
}

/// Capture the (already leak-scanned) LLM request payload per the
/// request-payload policy. Unlike tool I/O there is no per-tool denylist:
/// the whole payload is gated by the policy alone, and `truncate_bytes`
/// reuses the shared tool-io truncate cap. Returns `None` when the policy is
/// `off` (the default) so the prompt is never persisted unless opted in.
///
/// Takes the policy + cap directly (not a [`ResolvedPolicy`]) so the call
/// site can use [`crate::llm_request_payload_policy`] without holding the
/// full resolved bundle.
#[must_use]
pub fn capture_llm_request(
    policy: LlmRequestPayloadPolicy,
    truncate_bytes: usize,
    redacted: &str,
) -> Option<ToolIoCapture> {
    match policy {
        LlmRequestPayloadPolicy::Off => None,
        LlmRequestPayloadPolicy::Full => Some(ToolIoCapture {
            text: redacted.to_string(),
            original_bytes: redacted.len(),
            truncated: false,
        }),
        LlmRequestPayloadPolicy::Redacted => Some(truncate_to_cap(redacted, truncate_bytes)),
    }
}

/// Truncate `redacted` to at most `cap` bytes on a char boundary, flagging
/// whether truncation occurred and the original byte length.
fn truncate_to_cap(redacted: &str, cap: usize) -> ToolIoCapture {
    let original_bytes = redacted.len();
    if original_bytes <= cap {
        return ToolIoCapture {
            text: redacted.to_string(),
            original_bytes,
            truncated: false,
        };
    }
    let mut acc = String::with_capacity(cap);
    for ch in redacted.chars() {
        if acc.len() + ch.len_utf8() > cap {
            break;
        }
        acc.push(ch);
    }
    ToolIoCapture {
        text: acc,
        original_bytes,
        truncated: true,
    }
}

#[allow(dead_code)]
fn empty_unused_marker() {
    // Suppress unused-import false positives for `ToolIoCapture::empty`
    // (kept around for future "explicit empty capture" call sites).
    let _ = ToolIoCapture::empty();
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::config::LogConfig;

    fn make_policy(io: &str, cap: usize, denylist: Vec<String>) -> ResolvedPolicy {
        let cfg = LogConfig {
            log_tool_io: io.into(),
            log_tool_io_truncate_bytes: cap,
            log_tool_io_denylist: denylist,
            ..LogConfig::default()
        };
        ResolvedPolicy::from_config(&cfg, std::path::Path::new("/"))
    }

    #[test]
    fn off_policy_returns_none() {
        let p = make_policy("off", 8192, vec![]);
        assert!(capture_tool_input(&p, "shell", "hello").is_none());
    }

    #[test]
    fn denylist_skips_capture() {
        let p = make_policy("redacted", 8192, vec!["memory_recall".into()]);
        assert!(capture_tool_input(&p, "memory_recall", "hello").is_none());
        assert!(capture_tool_input(&p, "shell", "hello").is_some());
    }

    #[test]
    fn redacted_truncates_when_over_cap() {
        let p = make_policy("redacted", 4, vec![]);
        let cap = capture_tool_input(&p, "shell", "hello world").unwrap();
        assert_eq!(cap.text, "hell");
        assert_eq!(cap.original_bytes, 11);
        assert!(cap.truncated);
    }

    #[test]
    fn full_policy_keeps_everything() {
        let p = make_policy("full", 4, vec![]);
        let cap = capture_tool_output(&p, "shell", "hello world").unwrap();
        assert_eq!(cap.text, "hello world");
        assert!(!cap.truncated);
    }

    #[test]
    fn llm_request_off_is_default_and_returns_none() {
        // Default config resolves to Off => no capture.
        let default = ResolvedPolicy::from_config(&LogConfig::default(), std::path::Path::new("/"));
        assert_eq!(default.llm_request_payload, LlmRequestPayloadPolicy::Off);
        assert!(capture_llm_request(LlmRequestPayloadPolicy::Off, 8192, "system prompt").is_none());
    }

    #[test]
    fn llm_request_redacted_truncates_at_cap() {
        let cap = capture_llm_request(LlmRequestPayloadPolicy::Redacted, 4, "hello world").unwrap();
        assert_eq!(cap.text, "hell");
        assert_eq!(cap.original_bytes, 11);
        assert!(cap.truncated);
    }

    #[test]
    fn llm_request_full_keeps_everything() {
        let cap = capture_llm_request(LlmRequestPayloadPolicy::Full, 4, "hello world").unwrap();
        assert_eq!(cap.text, "hello world");
        assert!(!cap.truncated);
    }
}
