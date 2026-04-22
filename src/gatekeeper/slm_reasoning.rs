//! SLM-as-meta-evaluator (spec, 2026-04-23).
//!
//! The keyword heuristic in `routing_policy` is intentionally a cheap
//! first-pass filter — it catches obvious buckets but can't reason
//! about whether the SLM's own draft answer actually addresses what
//! the user asked for. This module adds a second, reasoning-based
//! tier: the local SLM is re-invoked twice with structured JSON
//! prompts, first to **decompose** the question into premises and
//! requirements, then to **verify** its own draft answer against
//! that decomposition.
//!
//! Why structured JSON rather than free-form reflection:
//!
//! - We need a *numeric* `final_confidence` the routing gate can
//!   compare against the threshold. Free-form self-reflection is
//!   hard to map to a scalar reliably.
//! - Premise + requirement *enumeration* gives us a per-item audit
//!   trail (e.g. "the user asked for a recipe *method*; the answer
//!   only listed ingredients"). The gate then penalises incomplete
//!   coverage even when the individual items that were answered
//!   look fine.
//!
//! All prompts are written so the SLM answers in the user's
//! language (Korean or English) but the JSON field NAMES stay ASCII
//! — so the Rust-side deserialisation is locale-independent. The
//! `rationale` / `notes` free-text fields can be in any language.
//!
//! Failure modes are non-fatal by design: when the SLM returns
//! malformed JSON, a timeout, or an empty body, the caller falls
//! back to the keyword heuristic score from `routing_policy`. We
//! treat this tier as "best-effort precision", not a hard dependency.

use serde::{Deserialize, Serialize};

use super::routing_policy::{ProfessionalDomain, QuestionDifficulty};

// ── Question decomposition (first SLM call) ──────────────────────

/// Structured decomposition of a user question emitted by the SLM
/// meta-classifier. Mirrors the JSON schema in the system prompt.
///
/// `premises` are facts the question takes for granted. Example: the
/// question "2025년에 나온 가장 좋은 법률 AI가 뭐야?" has the premise
/// that *it is 2025 or later* — an answer citing a 2023 article
/// fails that premise even if the article was otherwise accurate.
///
/// `requirements` are things the user explicitly wants in the
/// answer. Example: "레시피 방법 알려줘" requires the *method*
/// (steps), not just the ingredient list. An answer that returns
/// only ingredients fails the requirement.
#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct SlmQuestionAnalysis {
    pub difficulty: QuestionDifficulty,
    #[serde(default)]
    pub domains: Vec<ProfessionalDomain>,
    #[serde(default)]
    pub premises: Vec<String>,
    #[serde(default)]
    pub requirements: Vec<String>,
    #[serde(default)]
    pub requires_web_tool: bool,
    /// Free-form reasoning from the SLM. Kept for observability,
    /// never used as a numeric input to the gate.
    #[serde(default)]
    pub rationale: String,
}

/// System prompt for the question-decomposition call. Keep the
/// wording stable: prompt drift can flip the JSON shape and the
/// caller's deserialisation will start rejecting responses.
pub const QUESTION_DECOMPOSITION_SYSTEM_PROMPT: &str = r#"You are MoA's local meta-classifier. Your job is to analyse the user's question and emit a STRICT JSON object. Do NOT include any prose before or after the JSON.

Respond in the user's language for the "rationale" field, but keep every JSON KEY in ASCII. Every string inside "premises" and "requirements" must be a full sentence. Never invent premises or requirements — if the question has none, emit an empty array.

Schema (no extra keys):
{
  "difficulty": "simple" | "specialized" | "complex_reasoning",
  "domains": ["medical" | "legal" | "scientific" | "mathematical" | "coding" | "financial" | "engineering" | "other", ...],
  "premises": ["...factual assumptions the question takes for granted..."],
  "requirements": ["...specific things the answer MUST contain..."],
  "requires_web_tool": true | false,
  "rationale": "short free-text explanation"
}

Definitions:
- difficulty.simple            = solvable via web search / common knowledge.
- difficulty.specialized       = requires domain expertise (medical, legal, scientific, math, coding, finance, engineering).
- difficulty.complex_reasoning = multi-step reasoning, multi-domain composition, or open-ended design.
- premises                     = assumed facts the answer must respect (dates, named entities, jurisdictions, versions).
- requirements                 = user-requested deliverables (a method vs ingredients, a proof vs just the result, Korean vs English, code vs pseudocode, brevity vs depth).
- requires_web_tool            = true when the answer needs live facts / current data / site-specific information."#;

// ── Answer evaluation (second SLM call) ──────────────────────────

/// Per-item audit for premise coverage.
#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct PremiseCoverage {
    pub premise: String,
    pub addressed: bool,
    #[serde(default)]
    pub notes: String,
}

/// Per-item audit for requirement fulfilment.
#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct RequirementCoverage {
    pub requirement: String,
    pub fulfilled: bool,
    #[serde(default)]
    pub notes: String,
}

/// Full evaluation of an SLM draft answer against the earlier
/// decomposition. `final_confidence` is the single scalar the routing
/// gate uses; every other field is audit metadata surfaced for logs
/// and the UI.
///
/// Rationale for having BOTH `coverage_score` and `reasoning_score`:
/// an answer can fully cover every premise + requirement and STILL
/// be wrong about the reasoning path it used to get there (faulty
/// logic masked by correct conclusions). Having two axes lets the
/// gate penalise either failure mode independently before the final
/// `final_confidence` is computed.
#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct SlmAnswerEvaluation {
    #[serde(default)]
    pub premises_addressed: Vec<PremiseCoverage>,
    #[serde(default)]
    pub requirements_fulfilled: Vec<RequirementCoverage>,
    /// 0.0..1.0 — fraction of premises + requirements addressed.
    pub coverage_score: f64,
    /// 0.0..1.0 — SLM's best-effort judgement of reasoning validity
    /// (independent of whether the premises / requirements were
    /// touched).
    pub reasoning_score: f64,
    /// 0.0..1.0 — the final numeric the routing gate compares.
    pub final_confidence: f64,
    #[serde(default)]
    pub accuracy_concerns: Vec<String>,
    #[serde(default)]
    pub rationale: String,
}

pub const ANSWER_EVALUATION_SYSTEM_PROMPT: &str = r#"You are MoA's local answer-grader. Rigorously evaluate whether the draft answer addresses every premise and requirement of the question. Be strict: if a requirement is only partially addressed, mark fulfilled=false.

Output STRICT JSON only (no prose outside the object). Keep JSON KEYS in ASCII; "notes" and "rationale" can be in the user's language.

Schema:
{
  "premises_addressed": [{"premise": "...", "addressed": true|false, "notes": "..."}, ...],
  "requirements_fulfilled": [{"requirement": "...", "fulfilled": true|false, "notes": "..."}, ...],
  "coverage_score": 0.0..1.0,
  "reasoning_score": 0.0..1.0,
  "final_confidence": 0.0..1.0,
  "accuracy_concerns": ["..."],
  "rationale": "short free-text summary"
}

Rigour guidelines:
- If the draft answer cites a fact that contradicts a premise (for example the question asks about 2025 and the answer cites a 2023 source), add a line to "accuracy_concerns".
- If the user asked for a METHOD (e.g. 요리 방법) but the answer returned only INGREDIENTS, mark the corresponding requirement "fulfilled": false regardless of how detailed the ingredient list is.
- "coverage_score" is the fraction of premises + requirements that were handled correctly. Do not round up.
- "reasoning_score" audits the logic itself, independent of coverage. An answer can cover every item and still reason incorrectly — flag that in rationale + accuracy_concerns.
- "final_confidence" must reflect BOTH axes: never higher than min(coverage_score, reasoning_score + 0.1)."#;

// ── Minimal JSON extraction ──────────────────────────────────────

/// Extract the first `{ … }` JSON block from a mixed-prose response.
///
/// Small open-source SLMs (Gemma 4 E2B / E4B) occasionally prefix the
/// JSON with a chat-style preamble ("Sure! Here is the analysis: "),
/// so we lax-parse by grabbing the first balanced `{...}` block
/// rather than demanding the whole body be valid JSON. Falls back
/// to `Err` when no balanced block is found.
pub fn extract_json_block(raw: &str) -> anyhow::Result<String> {
    let bytes = raw.as_bytes();
    let open = raw
        .find('{')
        .ok_or_else(|| anyhow::anyhow!("no opening `{{` in SLM response"))?;
    let mut depth = 0usize;
    let mut in_string = false;
    let mut escape = false;
    let mut end: Option<usize> = None;
    for (i, &b) in bytes.iter().enumerate().skip(open) {
        let c = b as char;
        if escape {
            escape = false;
            continue;
        }
        if c == '\\' {
            escape = true;
            continue;
        }
        if c == '"' {
            in_string = !in_string;
            continue;
        }
        if in_string {
            continue;
        }
        if c == '{' {
            depth += 1;
        } else if c == '}' {
            depth -= 1;
            if depth == 0 {
                end = Some(i);
                break;
            }
        }
    }
    let end = end.ok_or_else(|| anyhow::anyhow!("unbalanced JSON braces"))?;
    Ok(raw[open..=end].to_string())
}

pub fn parse_question_analysis(raw: &str) -> anyhow::Result<SlmQuestionAnalysis> {
    let json = extract_json_block(raw)?;
    serde_json::from_str(&json)
        .map_err(|e| anyhow::anyhow!("question-analysis JSON parse failed: {e}"))
}

pub fn parse_answer_evaluation(raw: &str) -> anyhow::Result<SlmAnswerEvaluation> {
    let json = extract_json_block(raw)?;
    serde_json::from_str(&json)
        .map_err(|e| anyhow::anyhow!("answer-evaluation JSON parse failed: {e}"))
}

/// Build the user-message body for the decomposition call.
pub fn build_decomposition_user_prompt(message: &str) -> String {
    format!(
        "User question:\n\"{}\"\n\nReturn only the JSON object. Do NOT wrap the JSON in a Markdown code block.",
        message.replace('\n', " ")
    )
}

/// Build the user-message body for the evaluation call.
pub fn build_evaluation_user_prompt(
    question: &str,
    analysis: &SlmQuestionAnalysis,
    draft_answer: &str,
) -> String {
    let premises = if analysis.premises.is_empty() {
        "(none)".to_string()
    } else {
        analysis
            .premises
            .iter()
            .enumerate()
            .map(|(i, p)| format!("  {}. {}", i + 1, p))
            .collect::<Vec<_>>()
            .join("\n")
    };
    let requirements = if analysis.requirements.is_empty() {
        "(none)".to_string()
    } else {
        analysis
            .requirements
            .iter()
            .enumerate()
            .map(|(i, r)| format!("  {}. {}", i + 1, r))
            .collect::<Vec<_>>()
            .join("\n")
    };
    format!(
        "Question:\n\"{q}\"\n\nExtracted premises:\n{premises}\n\nExtracted requirements:\n{requirements}\n\nDraft answer:\n\"{a}\"\n\nReturn only the JSON object described in the system prompt.",
        q = question.replace('\n', " "),
        a = draft_answer.replace('\n', " "),
    )
}

#[cfg(test)]
mod tests {
    use super::*;

    const ANALYSIS_JSON: &str = r#"Sure, here is the JSON:
{
  "difficulty": "specialized",
  "domains": ["medical"],
  "premises": ["The user is asking about migraine treatment."],
  "requirements": ["Recommend an over-the-counter option"],
  "requires_web_tool": true,
  "rationale": "약 추천은 의학 전문영역"
}
Happy to help!"#;

    const EVALUATION_JSON: &str = r#"{
  "premises_addressed": [
    {"premise": "The user is asking about migraine treatment.", "addressed": true, "notes": "OK"}
  ],
  "requirements_fulfilled": [
    {"requirement": "Recommend an OTC option", "fulfilled": false, "notes": "재료만 답변함"}
  ],
  "coverage_score": 0.5,
  "reasoning_score": 0.6,
  "final_confidence": 0.45,
  "accuracy_concerns": ["재료 리스트만 돌려주어 요구사항 미충족"],
  "rationale": "답변이 방법을 빠뜨림"
}"#;

    #[test]
    fn extract_json_block_strips_preamble_and_postamble() {
        let extracted = extract_json_block(ANALYSIS_JSON).unwrap();
        assert!(extracted.starts_with('{'));
        assert!(extracted.ends_with('}'));
    }

    #[test]
    fn question_analysis_parses() {
        let a = parse_question_analysis(ANALYSIS_JSON).unwrap();
        assert!(matches!(a.difficulty, QuestionDifficulty::Specialized));
        assert_eq!(a.domains.len(), 1);
        assert!(a.requires_web_tool);
    }

    #[test]
    fn answer_evaluation_parses_with_zero_confidence_on_unmet_requirement() {
        let e = parse_answer_evaluation(EVALUATION_JSON).unwrap();
        assert!(e.coverage_score < 0.6);
        assert!(e.final_confidence < 0.5);
        assert!(!e.requirements_fulfilled.is_empty());
        assert!(!e.requirements_fulfilled[0].fulfilled);
    }

    #[test]
    fn extract_json_block_handles_nested_braces_in_strings() {
        let raw = r#"{"rationale": "use {foo} template", "difficulty": "simple", "requires_web_tool": false}"#;
        let extracted = extract_json_block(raw).unwrap();
        assert!(extracted.contains("{foo}"));
    }

    #[test]
    fn evaluation_prompt_enumerates_premises_and_requirements() {
        let analysis = SlmQuestionAnalysis {
            difficulty: QuestionDifficulty::Specialized,
            domains: vec![ProfessionalDomain::Medical],
            premises: vec!["전제1".into(), "전제2".into()],
            requirements: vec!["요구1".into()],
            requires_web_tool: false,
            rationale: "".into(),
        };
        let prompt = build_evaluation_user_prompt("질문", &analysis, "답변");
        assert!(prompt.contains("1. 전제1"));
        assert!(prompt.contains("2. 전제2"));
        assert!(prompt.contains("1. 요구1"));
    }
}
