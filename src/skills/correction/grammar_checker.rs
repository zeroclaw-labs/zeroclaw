//! Grammar validation gate — filter out user edits that are linguistically wrong.
//!
//! This module provides heuristic validation only. For deeper validation,
//! the caller should pipe through an LLM using `VALIDATION_SYSTEM_PROMPT`.

use super::observer::CorrectionObservation;
use serde::{Deserialize, Serialize};

/// Verdict classification for a correction.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize)]
pub enum ValidationVerdict {
    /// Grammar valid — correction is linguistically sound
    GrammarValid,
    /// Style change — both forms grammatical, user preference
    StyleChange,
    /// Grammar invalid — correction is wrong, do NOT learn
    GrammarInvalid,
    /// Domain-specific — non-standard but domain-acceptable
    DomainSpecific,
}

/// Result of validating a correction.
#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct ValidationResult {
    pub verdict: ValidationVerdict,
    pub should_learn: bool,
    pub notes: Option<String>,
}

impl ValidationResult {
    pub fn valid() -> Self {
        Self {
            verdict: ValidationVerdict::GrammarValid,
            should_learn: true,
            notes: None,
        }
    }

    pub fn style_change() -> Self {
        Self {
            verdict: ValidationVerdict::StyleChange,
            should_learn: true,
            notes: None,
        }
    }

    pub fn invalid(notes: impl Into<String>) -> Self {
        Self {
            verdict: ValidationVerdict::GrammarInvalid,
            should_learn: false,
            notes: Some(notes.into()),
        }
    }

    pub fn domain_specific(notes: impl Into<String>) -> Self {
        Self {
            verdict: ValidationVerdict::DomainSpecific,
            should_learn: true,
            notes: Some(notes.into()),
        }
    }
}

/// System prompt for LLM-based validation.
pub const VALIDATION_SYSTEM_PROMPT: &str = r#"A user edited a document and changed text.
Classify whether the correction is linguistically sound:

- "grammar_valid": the correction fixes a real grammatical error
- "style_change": both forms are grammatical but the user prefers one style
- "grammar_invalid": the correction introduces an error (do NOT learn this)
- "domain_specific": non-standard but accepted in the domain (legal, medical, code)

Respond in JSON format:
{"verdict": "...", "reason": "..."}"#;

/// Heuristic validation — fast preliminary check.
///
/// Returns `GrammarValid` for clearly OK edits, `GrammarInvalid` for obvious
/// regressions (empty, too short, pure whitespace). Ambiguous cases return
/// `StyleChange` so the LLM validator can make the final call.
pub fn validate_correction(obs: &CorrectionObservation) -> ValidationResult {
    let orig_trimmed = obs.original_text.trim();
    let corr_trimmed = obs.corrected_text.trim();

    // Empty corrections are invalid
    if corr_trimmed.is_empty() {
        return ValidationResult::invalid("correction is empty");
    }

    // No actual change
    if orig_trimmed == corr_trimmed {
        return ValidationResult::invalid("no change after trimming");
    }

    // Adding obvious typos like duplicate letters without context
    if has_obvious_typo_pattern(corr_trimmed) && !has_obvious_typo_pattern(orig_trimmed) {
        return ValidationResult::invalid("correction introduces obvious typo pattern");
    }

    // Ratio check — if one is drastically longer it may be a wholesale rewrite
    let ratio = corr_trimmed.chars().count() as f64 / orig_trimmed.chars().count().max(1) as f64;
    if !(0.3..=3.5).contains(&ratio) {
        return ValidationResult::style_change(); // Ambiguous — let LLM decide
    }

    // Default: treat as valid style change pending LLM confirmation
    ValidationResult::style_change()
}

/// Detect trivially bad patterns (3+ consecutive duplicate chars, etc.).
fn has_obvious_typo_pattern(s: &str) -> bool {
    let mut prev: Option<char> = None;
    let mut run = 1;
    for ch in s.chars() {
        if Some(ch) == prev {
            run += 1;
            if run >= 3 && ch.is_alphabetic() {
                return true;
            }
        } else {
            run = 1;
            prev = Some(ch);
        }
    }
    false
}

#[cfg(test)]
mod tests {
    use super::*;

    fn obs(before: &str, after: &str) -> CorrectionObservation {
        CorrectionObservation {
            id: None,
            uuid: "test".into(),
            original_text: before.into(),
            corrected_text: after.into(),
            context_before: None,
            context_after: None,
            document_type: None,
            category: None,
            source: "user_edit".into(),
            grammar_valid: true,
            observed_at: 0,
            session_id: None,
        }
    }

    #[test]
    fn empty_correction_invalid() {
        let r = validate_correction(&obs("text", ""));
        assert_eq!(r.verdict, ValidationVerdict::GrammarInvalid);
        assert!(!r.should_learn);
    }

    #[test]
    fn no_change_invalid() {
        let r = validate_correction(&obs("  text  ", "text"));
        assert_eq!(r.verdict, ValidationVerdict::GrammarInvalid);
    }

    #[test]
    fn obvious_typo_rejected() {
        let r = validate_correction(&obs("correct", "corrrrect"));
        assert_eq!(r.verdict, ValidationVerdict::GrammarInvalid);
    }

    #[test]
    fn normal_edit_passes_to_llm() {
        let r = validate_correction(&obs("하였다", "합니다"));
        assert_eq!(r.verdict, ValidationVerdict::StyleChange);
        assert!(r.should_learn);
    }

    #[test]
    fn extreme_length_change_flagged() {
        let r = validate_correction(&obs("a", "this is a totally different rewrite"));
        assert_eq!(r.verdict, ValidationVerdict::StyleChange);
    }
}
