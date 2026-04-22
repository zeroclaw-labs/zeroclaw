//! Two-stage routing policy — SLM confidence re-weighting (spec, 2026-04-23).
//!
//! The raw keyword-based classifier in `router::classify` gives us a
//! baseline `confidence` number, but it is a coarse "how sure am I
//! that the local SLM *can* handle this" signal. The product spec
//! needs something more nuanced:
//!
//! 1. **Question classification** — BEFORE the SLM runs, decide what
//!    kind of question it is:
//!    - `Simple`           — fact / common knowledge / wiki-like.
//!                           Expect the SLM to solve it via web tools.
//!    - `Specialized`      — medical / legal / scientific / math /
//!                           coding / finance / engineering. A single
//!                           domain expertise is required; the SLM
//!                           may know surface answers but rarely
//!                           deep-correct ones.
//!    - `ComplexReasoning` — multi-knowledge composition, chain-of-
//!                           thought reasoning, long plans.
//!
//! 2. **Answer quality evaluation** — AFTER the SLM produces a draft
//!    answer, score it on:
//!    - `length_ok`       — not empty, not one-liner for a non-trivial
//!                          question.
//!    - `has_hedging`     — contains "probably", "I'm not sure",
//!                          "아마도", "확실하지 않" markers. A
//!                          hedged answer signals the SLM itself
//!                          doesn't trust the output.
//!    - `tool_success`    — when the SLM invoked a web / search /
//!                          retrieval tool, did the tool report
//!                          success? Failed tools mean the raw
//!                          answer is probably fabricated.
//!    - `coherence_score` — 0.0..1.0 heuristic blending the above.
//!
//! 3. **Weighted confidence** — combine the two into a single score
//!    the gatekeeper compares to the threshold:
//!
//!        simple:              base_conf × quality × (1 + 0.20)
//!        specialized:         base_conf × quality × (1 - 0.20)
//!        complex_reasoning:   base_conf × quality × (1 - 0.30)
//!
//!    Plus small penalties for hedging (-0.10) and tool failure
//!    (-0.25) that stack with the difficulty bias. This encodes the
//!    spec: a simple question with a clean tool answer sails through
//!    to SLM-final; a complex or specialized question needs the SLM
//!    to really shine to beat the threshold, otherwise the chat
//!    handler escalates to the cloud LLM.

use serde::{Deserialize, Serialize};

// ── Difficulty ───────────────────────────────────────────────────

/// Macro-category of the question for routing purposes.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum QuestionDifficulty {
    Simple,
    Specialized,
    ComplexReasoning,
}

/// Professional domain the question touches. A question can touch
/// more than one (e.g. "patent on a genome sequencing algorithm" =
/// legal + scientific + coding).
#[derive(Debug, Clone, Copy, PartialEq, Eq, Hash, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum ProfessionalDomain {
    Medical,
    Legal,
    Scientific,
    Mathematical,
    Coding,
    Financial,
    Engineering,
    Other,
}

// ── Classification output ────────────────────────────────────────

#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct QuestionClassification {
    pub difficulty: QuestionDifficulty,
    pub domains: Vec<ProfessionalDomain>,
    /// Whether the SLM would benefit from a web/search tool to
    /// answer this. Used as a signal when evaluating answer quality:
    /// if tools are expected but none were invoked, quality drops.
    pub requires_web_tool: bool,
    /// How sure the classifier itself is about this categorisation,
    /// 0.0..1.0. Mostly informational — the weighting math doesn't
    /// use it directly.
    pub classifier_confidence: f64,
}

#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct AnswerQuality {
    pub length_ok: bool,
    pub has_hedging: bool,
    /// `Some(true)` = tool was invoked and reported success.
    /// `Some(false)` = tool invoked but failed / returned error /
    /// empty result. `None` = no tool was used.
    pub tool_success: Option<bool>,
    /// Blended 0.0..1.0 score derived from the other fields.
    pub coherence_score: f64,
}

// ── Classifier ───────────────────────────────────────────────────

/// Keyword-heuristic question classifier. Looks at the lowercased
/// message + a handful of Korean keyword buckets, votes each domain,
/// and decides a macro-difficulty bucket from the strongest votes.
///
/// This is intentionally a rule-based classifier, not an SLM prompt:
///
/// 1. It runs BEFORE the SLM call, so using an SLM here would
///    double the latency on every message.
/// 2. It needs to be deterministic + unit-testable so the weighting
///    math has a stable input.
/// 3. It's cheap to extend — new keywords = new coverage.
///
/// False positives are cheap (the quality evaluator catches bad
/// answers downstream). False negatives on Specialized/Complex
/// questions are the real risk, so keyword lists err on the side of
/// recall: any legal / medical / math / coding signal flips the
/// difficulty, even if the question is short.
pub fn classify_question(message: &str) -> QuestionClassification {
    let text = message.to_lowercase();

    let mut domains: Vec<ProfessionalDomain> = Vec::new();
    // Hint slot reserved for a future sub-classifier; nothing writes
    // it yet, so keep it immutable to silence the `unused_mut` lint.
    let _difficulty_hint: Option<QuestionDifficulty> = None;

    // 1) Professional-domain keyword buckets (Korean + English mix).
    let buckets: &[(ProfessionalDomain, &[&str])] = &[
        (
            ProfessionalDomain::Medical,
            &[
                "증상", "진단", "치료", "수술", "처방", "약물", "부작용",
                "질환", "질병", "감염", "통증", "골절", "혈압", "당뇨",
                "암", "종양", "골다공증", "알레르기", "약", "의학", "의료", "병원", "한의학", "편두통",
                "symptom", "diagnosis", "treatment", "surgery", "prescription",
                "medication", "disease", "infection", "fracture", "tumor",
            ],
        ),
        (
            ProfessionalDomain::Legal,
            &[
                "법", "법률", "판례", "소송", "계약", "형량", "양형",
                "고소", "고발", "민법", "형법", "상법", "특허", "저작권",
                "손해배상", "위법", "불법",
                "law", "legal", "lawsuit", "contract", "lawsuit", "litigation",
                "patent", "copyright", "statute", "precedent", "liability",
            ],
        ),
        (
            ProfessionalDomain::Scientific,
            &[
                "분자", "원자", "화학", "물리", "생물", "유전자", "세포",
                "열역학", "양자", "진화", "화학반응", "전자기",
                "molecule", "atom", "quantum", "thermodynamic", "evolution",
                "genetics", "cell", "physics", "chemistry", "biology",
            ],
        ),
        (
            ProfessionalDomain::Mathematical,
            &[
                "방정식", "증명", "미분", "적분", "행렬", "확률", "통계",
                "극한", "수열", "기하", "벡터", "수학", "수학적", "수식",
                "equation", "proof", "derivative", "integral", "matrix",
                "probability", "statistics", "geometry", "theorem", "algebra", "p-value", "pvalue", "p값",
            ],
        ),
        (
            ProfessionalDomain::Coding,
            &[
                "코드", "코딩", "알고리즘", "버그", "디버그", "함수", "변수",
                "컴파일", "런타임", "테스트", "자료구조",
                "code", "coding", "algorithm", "bug", "debug", "function",
                "variable", "compile", "runtime", "refactor", "typescript",
                "javascript", "python", "rust", "golang",
            ],
        ),
        (
            ProfessionalDomain::Financial,
            &[
                "세금", "세무", "회계", "투자", "주식", "채권", "환율",
                "이자", "대출", "파생", "재무제표",
                "tax", "accounting", "investment", "equity", "bond",
                "interest rate", "loan", "derivative", "balance sheet",
            ],
        ),
        (
            ProfessionalDomain::Engineering,
            &[
                "설계", "회로", "구조", "역학", "제어", "하드웨어", "공정",
                "circuit", "mechanics", "control", "hardware", "process",
            ],
        ),
    ];
    for (domain, keywords) in buckets {
        if keywords.iter().any(|k| text.contains(k)) {
            domains.push(*domain);
        }
    }

    // 2) Complexity markers — multi-step reasoning, creative, analytic.
    let complex_markers = [
        "왜냐하면", "왜", "비교", "분석", "추론", "논증", "설계해", "제안해",
        "에세이", "보고서", "정리해", "설명해", "근거", "논리적으로",
        "compare", "analyze", "reason", "derive", "synthesize", "propose",
        "argue", "essay", "justify", "evaluate", "design",
    ];
    let has_complex_marker = complex_markers.iter().any(|k| text.contains(k));

    // 3) Simple-fact markers — wiki-level questions.
    let simple_fact_markers = [
        "뭐야", "뭔가", "무엇인가", "언제", "어디", "누가", "몇",
        "무슨 날", "날짜", "시간이", "수도", "인구",
        "what is", "when did", "when was", "where is", "who is",
        "how many", "how much", "capital of",
    ];
    let has_simple_fact_marker = simple_fact_markers.iter().any(|k| text.contains(k));

    // 4) Decide macro-difficulty.
    let difficulty = if !domains.is_empty() || has_complex_marker {
        // Specialized knowledge OR reasoning required. Split the two:
        // a composite question with BOTH a domain hit AND a complex
        // marker is ComplexReasoning; single-domain without a
        // reasoning marker is Specialized.
        if has_complex_marker && (domains.len() >= 2 || has_complex_marker) {
            if domains.len() >= 2 {
                QuestionDifficulty::ComplexReasoning
            } else if domains.is_empty() {
                QuestionDifficulty::ComplexReasoning
            } else {
                QuestionDifficulty::Specialized
            }
        } else {
            QuestionDifficulty::Specialized
        }
    } else if has_simple_fact_marker {
        QuestionDifficulty::Simple
    } else {
        // No strong signal — treat as Simple unless the message is
        // long (> 120 chars) which usually implies multi-part ask.
        if text.chars().count() > 120 {
            QuestionDifficulty::ComplexReasoning
        } else {
            QuestionDifficulty::Simple
        }
    };
    let _ = &_difficulty_hint; // hint slot reserved for future sub-classifier

    // 5) Web-tool requirement: Simple fact questions almost always
    //    benefit from a web search. Specialized questions might or
    //    might not; we still mark them as "benefits from tools" so
    //    the quality evaluator penalises bare answers.
    let requires_web_tool = matches!(
        difficulty,
        QuestionDifficulty::Simple | QuestionDifficulty::Specialized
    );

    // 6) Classifier confidence. Higher when we have strong signals.
    let classifier_confidence = match (
        !domains.is_empty(),
        has_complex_marker,
        has_simple_fact_marker,
    ) {
        (true, true, _) => 0.90,
        (true, false, _) => 0.80,
        (false, true, false) => 0.75,
        (false, false, true) => 0.80,
        (false, false, false) => 0.55,
        _ => 0.70,
    };

    QuestionClassification {
        difficulty,
        domains,
        requires_web_tool,
        classifier_confidence,
    }
}

// ── Answer quality evaluator ─────────────────────────────────────

/// Hedging markers that signal the SLM itself is uncertain.
const HEDGING_MARKERS: &[&str] = &[
    "아마도", "아마", "잘 모르", "확실하지 않", "~인 것 같",
    "추측", "어쩌면", "~일지도", "판단이 어렵",
    "i'm not sure", "not entirely sure", "probably", "might be",
    "possibly", "i think", "i believe", "cannot say for certain",
    "hard to tell",
];

/// Evaluate the SLM's draft answer against the question.
///
/// `tool_was_used` + `tool_reported_success` come from the chat
/// handler, which knows whether the agent loop invoked any
/// tools during the response generation.
pub fn evaluate_answer_quality(
    question: &str,
    answer: &str,
    tool_was_used: bool,
    tool_reported_success: Option<bool>,
) -> AnswerQuality {
    let trimmed = answer.trim();
    let answer_chars = trimmed.chars().count();
    let question_chars = question.chars().count();

    // Length heuristic: a non-trivial question expects more than a
    // single sentence. Very short answers to very short questions
    // (greetings) are fine.
    let length_ok = if question_chars < 20 {
        !trimmed.is_empty()
    } else {
        answer_chars >= 20
    };

    let lowered = trimmed.to_lowercase();
    let has_hedging = HEDGING_MARKERS.iter().any(|m| lowered.contains(m));

    let tool_success = if tool_was_used {
        Some(tool_reported_success.unwrap_or(false))
    } else {
        None
    };

    // Coherence score: blend the above into a 0..1 number. Starts at
    // 1.0 and subtracts penalties for each negative signal.
    let mut score: f64 = 1.0;
    if !length_ok {
        score -= 0.30;
    }
    if has_hedging {
        score -= 0.20;
    }
    if matches!(tool_success, Some(false)) {
        score -= 0.40;
    }
    // Empty answer is fatal.
    if trimmed.is_empty() {
        score = 0.0;
    }
    let coherence_score = score.clamp(0.0, 1.0);

    AnswerQuality {
        length_ok,
        has_hedging,
        tool_success,
        coherence_score,
    }
}

// ── Weighted confidence ──────────────────────────────────────────

/// Difficulty bias applied multiplicatively (`1.0 + bias`) to the
/// raw SLM confidence. Simple questions get a boost, specialised a
/// drag, complex reasoning a steep drag.
fn difficulty_bias(d: QuestionDifficulty) -> f64 {
    match d {
        QuestionDifficulty::Simple => 0.20,
        QuestionDifficulty::Specialized => -0.20,
        QuestionDifficulty::ComplexReasoning => -0.30,
    }
}

/// Additional penalties that stack with the difficulty bias.
///
/// - hedging in the draft answer                : −0.10
/// - tool invoked but reported failure          : −0.25
/// - tool expected (`requires_web_tool`) but
///   none was invoked                           : −0.15
/// - tool invoked + succeeded                   : +0.05
fn penalties(classification: &QuestionClassification, quality: &AnswerQuality) -> f64 {
    let mut p = 0.0;
    if quality.has_hedging {
        p -= 0.10;
    }
    match quality.tool_success {
        Some(false) => p -= 0.25,
        Some(true) => p += 0.05,
        None if classification.requires_web_tool => p -= 0.15,
        None => {}
    }
    p
}

/// Final weighted confidence the gatekeeper compares against the
/// configured threshold.
///
///     weighted = clamp(
///         (base + bias + penalties) × coherence,
///         0.0, 1.0,
///     )
///
/// The difficulty bias is additive (not multiplicative on `base`) so
/// the magnitudes stay intuitive: "+20%" on a 0.7 base becomes 0.9
/// before the quality multiplier, not 0.84.
pub fn weighted_confidence(
    base_slm_confidence: f64,
    classification: &QuestionClassification,
    answer_quality: &AnswerQuality,
) -> f64 {
    let bias = difficulty_bias(classification.difficulty);
    let p = penalties(classification, answer_quality);
    let adjusted = (base_slm_confidence + bias + p) * answer_quality.coherence_score;
    adjusted.clamp(0.0, 1.0)
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn simple_fact_question_gets_simple_difficulty() {
        let c = classify_question("대한민국의 수도는 어디야?");
        assert!(matches!(c.difficulty, QuestionDifficulty::Simple));
        assert!(c.requires_web_tool);
    }

    #[test]
    fn medical_question_is_specialized() {
        let c = classify_question("편두통에 먹으면 좋은 약이 뭐야?");
        assert!(matches!(c.difficulty, QuestionDifficulty::Specialized));
        assert!(c.domains.contains(&ProfessionalDomain::Medical));
    }

    #[test]
    fn legal_reasoning_question_is_complex() {
        let c = classify_question(
            "이 계약서를 어떻게 분석해서 위약금 조항이 정당한지 논리적으로 설명해줘",
        );
        assert!(c.domains.contains(&ProfessionalDomain::Legal));
        // Has a complex marker AND a domain → specialized OR complex
        // depending on the multi-domain rule.
        assert!(matches!(
            c.difficulty,
            QuestionDifficulty::Specialized | QuestionDifficulty::ComplexReasoning
        ));
    }

    #[test]
    fn multi_domain_reasoning_is_complex_reasoning() {
        let c = classify_question(
            "의학 논문의 통계 해석에서 p-value 오용 사례를 수학적으로 증명해서 분석해줘",
        );
        // Hits medical + mathematical + complex marker.
        assert!(c.domains.len() >= 2);
        assert!(matches!(c.difficulty, QuestionDifficulty::ComplexReasoning));
    }

    #[test]
    fn weighted_boost_on_simple_tool_success() {
        let c = QuestionClassification {
            difficulty: QuestionDifficulty::Simple,
            domains: vec![],
            requires_web_tool: true,
            classifier_confidence: 0.9,
        };
        let q = AnswerQuality {
            length_ok: true,
            has_hedging: false,
            tool_success: Some(true),
            coherence_score: 1.0,
        };
        let w = weighted_confidence(0.7, &c, &q);
        // Base 0.7 + Simple bias 0.20 + tool-success bonus 0.05
        // = 0.95, clamped. Multiplier 1.0 → 0.95.
        assert!((w - 0.95).abs() < 0.01, "got {w}");
    }

    #[test]
    fn weighted_drop_on_specialized_hedged_toolfail() {
        let c = QuestionClassification {
            difficulty: QuestionDifficulty::Specialized,
            domains: vec![ProfessionalDomain::Legal],
            requires_web_tool: true,
            classifier_confidence: 0.8,
        };
        let q = AnswerQuality {
            length_ok: true,
            has_hedging: true,
            tool_success: Some(false),
            coherence_score: 0.5, // already low because hedge + no length
        };
        let w = weighted_confidence(0.7, &c, &q);
        // Base 0.7 + Specialized -0.20 + hedging -0.10 + tool fail -0.25
        // = 0.15; × 0.5 coherence = 0.075.
        assert!(w < 0.15, "got {w}");
    }

    #[test]
    fn complex_reasoning_no_tool_expected_no_penalty() {
        let c = QuestionClassification {
            difficulty: QuestionDifficulty::ComplexReasoning,
            domains: vec![],
            requires_web_tool: false,
            classifier_confidence: 0.75,
        };
        let q = AnswerQuality {
            length_ok: true,
            has_hedging: false,
            tool_success: None,
            coherence_score: 1.0,
        };
        let w = weighted_confidence(0.8, &c, &q);
        // 0.8 + ComplexReasoning -0.30 = 0.5; × 1.0 = 0.5.
        assert!((w - 0.5).abs() < 0.01, "got {w}");
    }

    #[test]
    fn empty_answer_drives_quality_to_zero() {
        let q = evaluate_answer_quality("some question", "", false, None);
        assert_eq!(q.coherence_score, 0.0);
    }

    #[test]
    fn hedged_answer_is_detected() {
        let q = evaluate_answer_quality(
            "편두통 약 추천해줘",
            "아마도 타이레놀이 좋을 것 같지만 확실하지 않습니다.",
            false,
            None,
        );
        assert!(q.has_hedging);
        assert!(q.coherence_score < 1.0);
    }

    #[test]
    fn tool_failure_heavily_penalises_quality() {
        let q = evaluate_answer_quality(
            "오늘 날씨 알려줘",
            "서울은 맑고 기온은 22도입니다.",
            true,
            Some(false),
        );
        assert_eq!(q.tool_success, Some(false));
        assert!(q.coherence_score <= 0.7);
    }
}
