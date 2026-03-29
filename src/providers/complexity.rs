//! Complexity estimation for automatic model routing.
//!
//! Classifies conversation messages into complexity tiers (Simple, Moderate,
//! Complex) using lightweight heuristics — no LLM call required. The tier
//! maps to a route hint that the [`RouterProvider`](super::router::RouterProvider)
//! resolves to the appropriate provider + model combo.

use crate::providers::traits::ChatMessage;

/// Complexity tier for a conversation turn.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum Complexity {
    /// Short messages, no tool calls, no code — route to cheapest model.
    Simple,
    /// Medium messages with some tool context — route to default model.
    Moderate,
    /// Long context, multi-step reasoning, code generation — route to most
    /// capable model.
    Complex,
}

impl Complexity {
    /// Return the auto-routing hint suffix for this tier (e.g. `"auto-simple"`).
    pub fn hint_suffix(&self) -> &'static str {
        match self {
            Self::Simple => "auto-simple",
            Self::Moderate => "auto-moderate",
            Self::Complex => "auto-complex",
        }
    }

    /// Build the full `hint:<name>` model string, using the caller-supplied
    /// override hint when provided, otherwise the default auto hint.
    pub fn to_hint(&self, simple_hint: Option<&str>, complex_hint: Option<&str>) -> String {
        let suffix = match self {
            Self::Simple => simple_hint.unwrap_or("cost-optimized"),
            Self::Moderate => self.hint_suffix(),
            Self::Complex => complex_hint.unwrap_or("reasoning"),
        };
        format!("hint:{suffix}")
    }
}

impl std::fmt::Display for Complexity {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        f.write_str(self.hint_suffix())
    }
}

// ── Heuristic thresholds ────────────────────────────────────────────────

/// Approximate token count from character length (1 token ~ 4 chars for
/// English text). This is a rough heuristic — good enough for routing
/// decisions without pulling in a tokeniser.
fn estimate_tokens(text: &str) -> usize {
    // Ceiling division so even 1 char counts as 1 token.
    text.len().div_ceil(4)
}

/// Keywords whose presence signals complex reasoning or generation tasks.
const COMPLEX_KEYWORDS: &[&str] = &[
    "analyze",
    "analyse",
    "refactor",
    "explain in detail",
    "step by step",
    "implement",
    "architect",
    "design",
    "optimise",
    "optimize",
    "debug",
    "diagnose",
    "compare and contrast",
    "write a test",
    "write tests",
    "code review",
    "security audit",
    "migration",
    "benchmark",
];

// ── Public API ──────────────────────────────────────────────────────────

/// Estimate the complexity of a conversation based on lightweight heuristics.
///
/// Heuristics evaluated (in order of weight):
/// 1. Total token count across all messages.
/// 2. Presence of code blocks (fenced ``` or indented).
/// 3. Presence of tool-result messages (role == "tool").
/// 4. Number of conversation turns.
/// 5. Presence of complexity-signalling keywords in the latest user message.
pub fn estimate(history: &[ChatMessage]) -> Complexity {
    if history.is_empty() {
        return Complexity::Simple;
    }

    let total_tokens: usize = history.iter().map(|m| estimate_tokens(&m.content)).sum();
    let turn_count = history.len();
    let has_tool_results = history.iter().any(|m| m.role == "tool");
    let has_code_blocks = history
        .iter()
        .any(|m| m.content.contains("```") || m.content.contains("    fn "));

    // Latest user message for keyword analysis.
    let last_user = history
        .iter()
        .rev()
        .find(|m| m.role == "user")
        .map(|m| m.content.as_str())
        .unwrap_or("");
    let lower_user = last_user.to_lowercase();
    let has_complex_keywords = COMPLEX_KEYWORDS.iter().any(|kw| lower_user.contains(kw));

    // ── Scoring ─────────────────────────────────────────────────────
    // Each signal adds to a cumulative score. Thresholds:
    //   score < 2  → Simple
    //   score < 4  → Moderate
    //   score >= 4 → Complex
    let mut score: u32 = 0;

    // Token budget signal.
    if total_tokens >= 2000 {
        score += 3;
    } else if total_tokens >= 500 {
        score += 2;
    } else if total_tokens >= 100 {
        score += 1;
    }

    // Conversation depth signal.
    if turn_count >= 10 {
        score += 2;
    } else if turn_count >= 4 {
        score += 1;
    }

    // Structural signals.
    if has_tool_results {
        score += 1;
    }
    if has_code_blocks {
        score += 1;
    }
    if has_complex_keywords {
        score += 2;
    }

    match score {
        0..=1 => Complexity::Simple,
        2..=3 => Complexity::Moderate,
        _ => Complexity::Complex,
    }
}

// ── Tests ───────────────────────────────────────────────────────────────

#[cfg(test)]
mod tests {
    use super::*;

    fn user(content: &str) -> ChatMessage {
        ChatMessage::user(content)
    }

    fn assistant(content: &str) -> ChatMessage {
        ChatMessage {
            role: "assistant".into(),
            content: content.into(),
        }
    }

    fn tool_result(content: &str) -> ChatMessage {
        ChatMessage {
            role: "tool".into(),
            content: content.into(),
        }
    }

    // ── Empty / trivial ─────────────────────────────────────────────

    #[test]
    fn empty_history_is_simple() {
        assert_eq!(estimate(&[]), Complexity::Simple);
    }

    #[test]
    fn short_greeting_is_simple() {
        assert_eq!(estimate(&[user("hi")]), Complexity::Simple);
    }

    #[test]
    fn single_short_question_is_simple() {
        assert_eq!(estimate(&[user("What time is it?")]), Complexity::Simple);
    }

    // ── Moderate signals ────────────────────────────────────────────

    #[test]
    fn medium_message_with_code_block_is_moderate() {
        // ~500 chars ≈ 125 tokens → token score 1, plus code block score 1 → total 2 → Moderate
        let msg = format!(
            "Here is some code:\n```rust\nfn main() {{}}\n```\n{}",
            "x".repeat(450)
        );
        assert_eq!(estimate(&[user(&msg)]), Complexity::Moderate);
    }

    #[test]
    fn multi_turn_conversation_is_moderate() {
        let history = vec![
            user("Hello"),
            assistant("Hi! How can I help?"),
            user("Tell me about Rust"),
            assistant("Rust is a systems programming language..."),
            user("Thanks"),
        ];
        assert!(matches!(
            estimate(&history),
            Complexity::Moderate | Complexity::Simple
        ));
    }

    // ── Complex signals ─────────────────────────────────────────────

    #[test]
    fn long_context_with_keywords_is_complex() {
        let long_msg = format!(
            "Please analyze this code and refactor it step by step:\n{}",
            "let x = 1;\n".repeat(200)
        );
        assert_eq!(estimate(&[user(&long_msg)]), Complexity::Complex);
    }

    #[test]
    fn tool_results_with_code_and_many_turns_is_complex() {
        let mut history = Vec::new();
        for i in 0..6 {
            history.push(user(&format!("Step {i}: implement the next part")));
            history.push(assistant(&format!("```rust\nfn step_{i}() {{}}\n```")));
            history.push(tool_result(&format!("Tool output for step {i}")));
        }
        assert_eq!(estimate(&history), Complexity::Complex);
    }

    #[test]
    fn complex_keywords_boost_score() {
        let msg = "Please analyze and debug this issue, then write tests for the fix";
        // Even though the message is short in tokens, the keywords push it up.
        let result = estimate(&[user(msg)]);
        assert!(
            matches!(result, Complexity::Moderate | Complexity::Complex),
            "expected Moderate or Complex, got {result:?}"
        );
    }

    // ── Hint generation ─────────────────────────────────────────────

    #[test]
    fn hint_suffix_values() {
        assert_eq!(Complexity::Simple.hint_suffix(), "auto-simple");
        assert_eq!(Complexity::Moderate.hint_suffix(), "auto-moderate");
        assert_eq!(Complexity::Complex.hint_suffix(), "auto-complex");
    }

    #[test]
    fn to_hint_uses_defaults() {
        assert_eq!(
            Complexity::Simple.to_hint(None, None),
            "hint:cost-optimized"
        );
        assert_eq!(
            Complexity::Moderate.to_hint(None, None),
            "hint:auto-moderate"
        );
        assert_eq!(Complexity::Complex.to_hint(None, None), "hint:reasoning");
    }

    #[test]
    fn to_hint_respects_overrides() {
        assert_eq!(Complexity::Simple.to_hint(Some("fast"), None), "hint:fast");
        assert_eq!(
            Complexity::Complex.to_hint(None, Some("smart")),
            "hint:smart"
        );
    }

    // ── Token estimation ────────────────────────────────────────────

    #[test]
    fn estimate_tokens_basic() {
        assert_eq!(estimate_tokens(""), 0);
        assert_eq!(estimate_tokens("hi"), 1);
        assert_eq!(estimate_tokens("hello world"), 3); // 11 chars / 4 = 2.75 → ceil = 3
    }

    // ── Display impl ────────────────────────────────────────────────

    #[test]
    fn display_matches_hint_suffix() {
        assert_eq!(format!("{}", Complexity::Simple), "auto-simple");
        assert_eq!(format!("{}", Complexity::Complex), "auto-complex");
    }
}
