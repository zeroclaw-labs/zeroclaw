//! Agent behavior hooks for One2X.
//!
//! ## Planning-without-execution detection
//!
//! Detects when the LLM outputs plans without taking action and injects
//! a nudge to execute immediately. This prevents wasted turns where the
//! agent describes what it "will do" without actually doing it.
//!
//! ## Upstream integration
//!
//! Called from `agent/loop_.rs` after each assistant response:
//! ```ignore
//! #[cfg(feature = "one2x")]
//! crate::one2x::agent_hooks::check_planning_without_execution(&mut messages);
//! ```

use zeroclaw_api::provider::ChatMessage;

/// Maximum number of retries when an LLM inference step times out.
///
/// On each retry, the backoff doubles: 2s → 4s → 8s.
/// Setting to 0 preserves the original bail-immediately behaviour.
pub const STEP_TIMEOUT_MAX_RETRIES: u32 = 2;

/// Planning-only indicator phrases (case-insensitive matching).
const PLANNING_PHRASES: &[&str] = &[
    "i will ",
    "i'll ",
    "i would ",
    "i'd recommend ",
    "here's my plan",
    "here is my plan",
    "let me outline",
    "the steps are",
    "the approach would be",
    "i'm going to ",
    "my plan is to",
    "first, i'll ",
    "step 1:",
    "i can help you by",
    "here's what i'll do",
    "here's what we need to do",
    "i propose ",
    "the strategy is",
];

/// Phrases that indicate the assistant actually executed something.
const EXECUTION_INDICATORS: &[&str] = &[
    "```",
    "tool_use",
    "tool_call",
    "i ran ",
    "i executed ",
    "the result is",
    "here's the output",
    "the output shows",
    "done.",
    "completed.",
    "created successfully",
    "updated successfully",
    "error:",
    "warning:",
];

const NUDGE_MESSAGE: &str = "\
Do not describe what you will do — execute it now. \
Use your tools to take the first concrete action immediately. \
If multiple steps are needed, execute the first step now and report the result.";

/// Check if the latest assistant message is planning-only (no execution).
/// If so, append a system nudge to push the model toward action.
///
/// Returns `true` if a nudge was injected.
pub fn check_planning_without_execution(messages: &mut Vec<ChatMessage>) -> bool {
    let last = match messages.last() {
        Some(m) if m.role == "assistant" => m,
        _ => return false,
    };

    let content_lower = last.content.to_lowercase();

    // Check for execution indicators — if any found, not planning-only
    for indicator in EXECUTION_INDICATORS {
        if content_lower.contains(indicator) {
            return false;
        }
    }

    // Check for planning phrases
    let has_planning = PLANNING_PHRASES
        .iter()
        .any(|phrase| content_lower.contains(phrase));

    if !has_planning {
        return false;
    }

    // Short responses are likely not planning-only
    if last.content.len() < 100 {
        return false;
    }

    tracing::info!("Detected planning-without-execution, injecting execution nudge");
    messages.push(ChatMessage {
        role: "user".to_string(),
        content: NUDGE_MESSAGE.to_string(),
    });

    true
}

/// Fast-path detection for simple user approvals.
/// Returns an optimized instruction if the user message is a short approval.
pub fn detect_fast_approval(user_message: &str) -> Option<&'static str> {
    let trimmed = user_message.trim().to_lowercase();
    let approvals = [
        "ok", "好", "好的", "可以", "行", "执行", "做吧", "开始", "确认",
        "go", "yes", "do it", "proceed", "continue", "sure", "yep", "yeah",
        "confirmed", "approve", "agreed", "lgtm",
    ];

    if approvals.iter().any(|a| trimmed == *a) {
        Some(
            "The user approved. Execute the previously discussed plan immediately. \
             Do not re-explain the plan — take the first action now.",
        )
    } else {
        None
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    fn msg(role: &str, content: &str) -> ChatMessage {
        ChatMessage {
            role: role.to_string(),
            content: content.to_string(),
        }
    }

    #[test]
    fn test_detects_planning() {
        let mut messages = vec![
            msg("user", "Fix the bug in auth.rs"),
            msg("assistant", "I'll fix this bug. Here's my plan: First, I'll read the file. Then I'll identify the issue. Step 1: Read auth.rs. Step 2: Fix the null check. Step 3: Test the fix."),
        ];
        assert!(check_planning_without_execution(&mut messages));
        assert_eq!(messages.len(), 3);
        assert_eq!(messages[2].role, "user");
        assert!(messages[2].content.contains("execute it now"));
    }

    #[test]
    fn test_no_nudge_when_executing() {
        let mut messages = vec![
            msg("user", "Fix the bug"),
            msg("assistant", "I'll fix this. Here's the output:\n```\nfixed\n```"),
        ];
        assert!(!check_planning_without_execution(&mut messages));
        assert_eq!(messages.len(), 2);
    }

    #[test]
    fn test_no_nudge_for_short_messages() {
        let mut messages = vec![
            msg("user", "What's up?"),
            msg("assistant", "I'll check."),
        ];
        assert!(!check_planning_without_execution(&mut messages));
    }

    #[test]
    fn test_no_nudge_for_user_message() {
        let mut messages = vec![msg("user", "I will do something later")];
        assert!(!check_planning_without_execution(&mut messages));
    }

    #[test]
    fn test_fast_approval_detection() {
        assert!(detect_fast_approval("ok").is_some());
        assert!(detect_fast_approval("好的").is_some());
        assert!(detect_fast_approval("Do it").is_some());
        assert!(detect_fast_approval("Yes").is_some());
        assert!(detect_fast_approval("  确认  ").is_some());
        assert!(detect_fast_approval("Tell me more about it").is_none());
        assert!(detect_fast_approval("No, I changed my mind").is_none());
    }
}
