//! Prompt builder functions for STM, MTM, and LTM fact extraction agents.
//!
//! Each function assembles a self-contained prompt string that instructs an LLM
//! to extract structured facts from conversation text and return them as JSON.
//! The prompts encode the full output schema so the downstream
//! [`super::extractor::parse_extraction_response`] parser can deserialise the
//! result without additional context.

/// Maximum number of prior conversation turns included in the STM prompt for
/// recency context.  When the caller supplies more, the oldest turns are
/// silently dropped.
pub const MAX_PRIOR_TURNS: usize = 5;

// ── Valid categories (shared across prompts) ────────────────────────────────

const VALID_CATEGORIES: &str = "\
personal, technical, preference, project, relationship, location, temporal, financial, organizational";

// ── STM extraction ──────────────────────────────────────────────────────────

/// Build a prompt that instructs the LLM to extract **all** facts from a
/// single user/agent exchange, optionally preceded by recent prior turns for
/// context.
///
/// The output schema matches [`super::extractor::FactEntryDraft`].
///
/// # Arguments
///
/// * `user_message`  - The latest user message.
/// * `agent_response` - The agent's reply to that message.
/// * `prior_turns`    - Up to [`MAX_PRIOR_TURNS`] recent conversation strings
///                      (newest last).  If more are supplied the oldest are
///                      truncated.
pub fn build_stm_extraction_prompt(
    user_message: &str,
    agent_response: &str,
    prior_turns: &[&str],
) -> String {
    let mut prompt = String::with_capacity(2048);

    // ── System instructions ──────────────────────────────────────────────
    prompt.push_str(
        "You are a fact-extraction agent for a memory system.\n\
         Your task is to extract ALL facts, numbers, names, dates, preferences, \
         opinions, relationships, locations, and any other concrete information \
         from the conversation exchange below.\n\n",
    );

    // ── Output schema ────────────────────────────────────────────────────
    prompt.push_str("Output a JSON array where each element has these fields:\n");
    prompt.push_str("  - \"category\": string — one of: ");
    prompt.push_str(VALID_CATEGORIES);
    prompt.push('\n');
    prompt.push_str("  - \"subject\": string — the entity the fact is about\n");
    prompt.push_str("  - \"attribute\": string — the specific property or trait\n");
    prompt.push_str("  - \"value\": string — the concrete value or assertion\n");
    prompt.push_str(
        "  - \"context_narrative\": string — brief sentence describing where/how the fact emerged\n",
    );
    prompt.push_str("  - \"confidence\": string — one of \"low\", \"medium\", \"high\"\n");
    prompt.push_str(
        "  - \"related_facts\": array of strings — keys of related facts (may be empty)\n",
    );
    prompt.push_str(
        "  - \"volatility_class\": string — one of \"stable\", \"semi_stable\", \"volatile\"\n\n",
    );

    // ── Prior context ────────────────────────────────────────────────────
    let start = if prior_turns.len() > MAX_PRIOR_TURNS {
        prior_turns.len() - MAX_PRIOR_TURNS
    } else {
        0
    };
    let included = &prior_turns[start..];

    if !included.is_empty() {
        prompt.push_str("=== Recent conversation context ===\n");
        for (i, turn) in included.iter().enumerate() {
            prompt.push_str(&format!("Turn {}: {}\n", start + i, turn));
        }
        prompt.push('\n');
    }

    // ── Current exchange ─────────────────────────────────────────────────
    prompt.push_str("=== Current exchange ===\n");
    prompt.push_str(&format!("[User]: {}\n", user_message));
    prompt.push_str(&format!("[Agent]: {}\n\n", agent_response));

    // ── Closing instruction ──────────────────────────────────────────────
    prompt.push_str(
        "Return ONLY a JSON array. No markdown, no explanation. If no facts found, return [].",
    );

    prompt
}

// ── MTM extraction ──────────────────────────────────────────────────────────

/// Build a prompt for medium-term (daily) fact extraction.
///
/// The LLM receives the full day's transcript together with any previously
/// known facts and is instructed to perform deep analysis, flag
/// contradictions, and output enriched fact objects that include correction
/// metadata.
///
/// # Arguments
///
/// * `day_transcript` - Concatenated conversation text for the entire day.
/// * `existing_facts` - Stringified representations of facts already stored
///                      in the memory system.
pub fn build_mtm_extraction_prompt(day_transcript: &str, existing_facts: &[&str]) -> String {
    let mut prompt = String::with_capacity(4096);

    // ── System instructions ──────────────────────────────────────────────
    prompt.push_str(
        "You are a deep-analysis memory agent performing end-of-day fact consolidation.\n\
         Analyse the full day's conversation transcript below. Extract ALL facts, \
         numbers, names, dates, preferences, opinions, relationships, locations, \
         and any other concrete information.\n\n\
         Cross-reference each extracted fact against the existing facts provided. \
         If a new fact contradicts or updates an existing one, mark it as a correction.\n\n",
    );

    // ── Output schema ────────────────────────────────────────────────────
    prompt.push_str("Output a JSON array where each element has these fields:\n");
    prompt.push_str("  - \"category\": string — one of: ");
    prompt.push_str(VALID_CATEGORIES);
    prompt.push('\n');
    prompt.push_str("  - \"subject\": string — the entity the fact is about\n");
    prompt.push_str("  - \"attribute\": string — the specific property or trait\n");
    prompt.push_str("  - \"value\": string — the concrete value or assertion\n");
    prompt.push_str(
        "  - \"context_narrative\": string — brief sentence describing where/how the fact emerged\n",
    );
    prompt.push_str("  - \"confidence\": string — one of \"low\", \"medium\", \"high\"\n");
    prompt.push_str(
        "  - \"related_facts\": array of strings — keys of related facts (may be empty)\n",
    );
    prompt.push_str(
        "  - \"volatility_class\": string — one of \"stable\", \"semi_stable\", \"volatile\"\n",
    );
    prompt.push_str(
        "  - \"is_correction\": bool — true if this fact corrects/supersedes an existing one\n",
    );
    prompt.push_str(
        "  - \"corrects_key\": string — the key of the existing fact being corrected, or \"\" if not a correction\n\n",
    );

    // ── Existing facts ───────────────────────────────────────────────────
    prompt.push_str("=== Existing facts ===\n");
    if existing_facts.is_empty() {
        prompt.push_str("(none)\n");
    } else {
        for fact in existing_facts {
            prompt.push_str("- ");
            prompt.push_str(fact);
            prompt.push('\n');
        }
    }
    prompt.push('\n');

    // ── Day transcript ───────────────────────────────────────────────────
    prompt.push_str("=== Day transcript ===\n");
    prompt.push_str(day_transcript);
    prompt.push_str("\n\n");

    // ── Closing instruction ──────────────────────────────────────────────
    prompt.push_str(
        "Return ONLY a JSON array. No markdown, no explanation. If no facts found, return [].",
    );

    prompt
}

// ── LTM compression ─────────────────────────────────────────────────────────

/// Build a prompt for long-term memory consolidation.
///
/// The LLM receives numbered daily MTM summaries and existing LTM facts and
/// is asked to merge them into a single durable summary, preserving stable
/// facts and discarding expired volatile ones.
///
/// The expected output is a JSON **object** (not an array) with:
/// - `"summary"`: string — consolidated narrative summary.
/// - `"facts"`: array of fact objects (same schema as STM).
/// - `"expired_keys"`: array of strings — keys that should be removed.
///
/// # Arguments
///
/// * `mtm_summaries`  - One string per day, in chronological order.
/// * `existing_facts` - Stringified representations of facts currently in LTM.
pub fn build_ltm_compression_prompt(mtm_summaries: &[&str], existing_facts: &[&str]) -> String {
    let mut prompt = String::with_capacity(4096);

    // ── System instructions ──────────────────────────────────────────────
    prompt.push_str(
        "You are a long-term memory consolidation agent.\n\
         Your task is to merge the daily summaries below into a single durable \
         long-term memory record. Preserve facts that are stable and still relevant. \
         Discard volatile facts that have expired or been superseded. \
         Resolve any contradictions by keeping the most recent value.\n\n",
    );

    // ── Output schema ────────────────────────────────────────────────────
    prompt.push_str("Output a JSON object with these top-level keys:\n");
    prompt.push_str("  - \"summary\": string — a consolidated narrative summary\n");
    prompt.push_str("  - \"facts\": array — each element has: category, subject, attribute, value, context_narrative, confidence, related_facts, volatility_class\n");
    prompt.push_str(
        "  - \"expired_keys\": array of strings — fact keys that should be removed from LTM\n\n",
    );

    // ── Existing LTM facts ───────────────────────────────────────────────
    prompt.push_str("=== Existing LTM facts ===\n");
    if existing_facts.is_empty() {
        prompt.push_str("(none)\n");
    } else {
        for fact in existing_facts {
            prompt.push_str("- ");
            prompt.push_str(fact);
            prompt.push('\n');
        }
    }
    prompt.push('\n');

    // ── Daily summaries ──────────────────────────────────────────────────
    prompt.push_str("=== Daily summaries ===\n");
    for (i, summary) in mtm_summaries.iter().enumerate() {
        prompt.push_str(&format!("[Day {}]: {}\n", i + 1, summary));
    }
    prompt.push('\n');

    // ── Closing instruction ──────────────────────────────────────────────
    prompt.push_str("Return ONLY a JSON object. No markdown, no explanation.");

    prompt
}

// ── Tests ────────────────────────────────────────────────────────────────────

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn stm_prompt_includes_messages() {
        let prompt =
            build_stm_extraction_prompt("My name is Alice", "Nice to meet you, Alice!", &[]);

        assert!(
            prompt.contains("My name is Alice"),
            "prompt should contain user message"
        );
        assert!(
            prompt.contains("Nice to meet you, Alice!"),
            "prompt should contain agent response"
        );
        assert!(
            prompt.contains("JSON"),
            "prompt should mention JSON output format"
        );
    }

    #[test]
    fn stm_prompt_truncates_context() {
        // Create 20 prior turns numbered 0..19.
        let turns: Vec<String> = (0..20).map(|i| format!("Turn {} content", i)).collect();
        let turn_refs: Vec<&str> = turns.iter().map(|s| s.as_str()).collect();

        let prompt = build_stm_extraction_prompt("hello", "hi there", &turn_refs);

        // Only the last MAX_PRIOR_TURNS (5) should be included: turns 15..19.
        // Turn 0 must NOT appear.
        assert!(
            !prompt.contains("Turn 0 content"),
            "prompt should NOT include Turn 0 (oldest turns truncated)"
        );
        // Turn 19 (the newest) should be present.
        assert!(
            prompt.contains("Turn 19 content"),
            "prompt should include Turn 19 (newest)"
        );
    }

    #[test]
    fn mtm_prompt_includes_transcript_and_facts() {
        let transcript = "User asked about Rust lifetimes. Agent explained borrowing.";
        let facts = vec![
            "fact:personal:user:name = Alice",
            "fact:preference:user:language = Rust",
        ];

        let prompt = build_mtm_extraction_prompt(transcript, &facts);

        assert!(
            prompt.contains(transcript),
            "prompt should contain day transcript"
        );
        assert!(
            prompt.contains("fact:personal:user:name = Alice"),
            "prompt should contain existing fact 1"
        );
        assert!(
            prompt.contains("fact:preference:user:language = Rust"),
            "prompt should contain existing fact 2"
        );
    }

    #[test]
    fn ltm_prompt_includes_summaries() {
        let summaries = vec![
            "Day 1: User onboarded, set preferences.",
            "Day 2: User asked about deployment.",
        ];
        let facts = vec!["fact:project:zeroclaw:status = active"];

        let prompt = build_ltm_compression_prompt(&summaries, &facts);

        assert!(
            prompt.contains("Day 1: User onboarded, set preferences."),
            "prompt should contain first summary"
        );
        assert!(
            prompt.contains("Day 2: User asked about deployment."),
            "prompt should contain second summary"
        );
        assert!(
            prompt.contains("fact:project:zeroclaw:status = active"),
            "prompt should contain existing fact"
        );
    }
}
