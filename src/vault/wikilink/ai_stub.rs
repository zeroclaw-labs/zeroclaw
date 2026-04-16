// @Ref: SUMMARY §3 Steps 2a + 4 — AIEngine trait + provider-free heuristic impl.
//
// Production path: `LlmAIEngine` wraps a provider (Haiku/Opus). Tests and
// offline environments use `HeuristicAIEngine` which applies rule-based
// extraction only — no network, no flakes, deterministic output.

use super::tokens::{CompoundToken, CompoundTokenKind};
use async_trait::async_trait;

#[derive(Debug, Clone, PartialEq)]
pub struct KeyConcept {
    pub term: String,
    /// 1–10, where 10 = this document's raison d'être.
    pub importance: u8,
}

#[derive(Debug, Clone, Default)]
pub struct GatekeepVerdict {
    pub kept: Vec<String>,
    /// Pairs of (representative, alias) that the gatekeeper identified as
    /// synonyms within this document. Fed to Step 5 for `[[rep|alias]]`
    /// and to Step 6 for long-term vocabulary_relations learning.
    pub synonym_pairs: Vec<(String, String)>,
}

/// Structured briefing narrative produced by `AIEngine::narrate_briefing`.
/// Each field is a markdown-ready block. Empty strings are permitted for
/// sections the AI had no evidence for.
#[derive(Debug, Clone, Default)]
pub struct BriefingNarrative {
    /// 사건 경과 (timeline)
    pub timeline: String,
    /// 양측 주장 대비
    pub contentions: String,
    /// 핵심 쟁점
    pub issues: String,
    /// 증거 현황 및 미비
    pub evidence: String,
    /// 관련 판례 요약
    pub precedents: String,
    /// 다음 기일 준비 체크리스트
    pub checklist: String,
    /// 전략 제안 (강점 / 약점)
    pub strategy: String,
}

/// Pluggable AI driver — production uses LLM, tests use heuristic.
#[async_trait]
pub trait AIEngine: Send + Sync {
    async fn extract_key_concepts(
        &self,
        markdown: &str,
        compounds: &[CompoundToken],
    ) -> anyhow::Result<Vec<KeyConcept>>;

    async fn gatekeep(
        &self,
        candidates: &[String],
        doc_preview: &str,
    ) -> anyhow::Result<GatekeepVerdict>;

    /// Synthesize a 7-section case briefing from the supplied context.
    /// Default implementation returns an empty narrative so existing
    /// engines remain valid without overriding.
    async fn narrate_briefing(
        &self,
        _case_number: &str,
        _primary_docs: &[(i64, String, String)], // (doc_id, title, content_preview)
        _related_docs: &[(i64, String)],
    ) -> anyhow::Result<BriefingNarrative> {
        Ok(BriefingNarrative::default())
    }

    /// Assign each backlinked document to one or more skeleton sections
    /// of the hub note. Returns a vector aligned with `docs` where each
    /// entry is the list of section indices (0-based) the doc belongs to.
    /// Empty vectors are permitted (doc not pinned to any section).
    ///
    /// Default: hash-mod distribution (`doc_id mod section_count`) so
    /// every doc lands in exactly one section — matches the historical
    /// behaviour before this trait method existed.
    async fn assign_hub_sections(
        &self,
        _subtype: &str,
        sections: &[&str],
        docs: &[(i64, String, String)], // (doc_id, title, content_preview)
    ) -> anyhow::Result<Vec<Vec<usize>>> {
        let n = sections.len().max(1);
        Ok(docs
            .iter()
            .map(|(id, _, _)| vec![id.unsigned_abs() as usize % n])
            .collect())
    }

    /// Detect contradictions among a set of claims about the same entity.
    /// Default: empty — only LlmAIEngine produces real detections.
    async fn detect_contradictions(
        &self,
        _entity: &str,
        _claims: &[ContentClaim],
    ) -> anyhow::Result<Vec<Contradiction>> {
        Ok(Vec::new())
    }

    /// Qualitative gate for mid-length texts (200 ≤ len < 2000).
    /// Return whether the text reads as **reference knowledge** worth
    /// storing in the second brain, vs everyday conversation that
    /// should NOT be indexed.
    ///
    /// Default implementation is rule-based and runs offline:
    /// - knowledge signals: markdown headers, compound-token citations
    ///   (statute/case/precedent), multi-sentence density, numeric/date
    ///   enumerations, formal-paragraph structure.
    /// - conversation signals: greetings, one-sentence asks, emoji /
    ///   chat particles, imperative fragments.
    async fn classify_as_knowledge(
        &self,
        text: &str,
    ) -> anyhow::Result<KnowledgeVerdict> {
        Ok(heuristic_knowledge_classify(text))
    }
}

#[derive(Debug, Clone)]
pub struct KnowledgeVerdict {
    pub is_knowledge: bool,
    /// 0.0–1.0
    pub confidence: f32,
    /// Short human-readable rationale (ko).
    pub reason: String,
}

/// Rule-based "is this knowledge" classifier used by HeuristicAIEngine
/// and as the safe fallback for any `AIEngine` that can't reach its
/// provider. Conservative on purpose: ambiguous cases are treated as
/// conversation so the vault doesn't fill up with greetings.
pub fn heuristic_knowledge_classify(text: &str) -> KnowledgeVerdict {
    let trimmed = text.trim();
    let char_count = trimmed.chars().count();
    if char_count < 50 {
        return KnowledgeVerdict {
            is_knowledge: false,
            confidence: 0.95,
            reason: "매우 짧은 텍스트 (< 50자) — 잡담으로 간주".into(),
        };
    }

    let compounds =
        super::tokens::detect_compound_tokens(trimmed);
    let has_markdown_header = trimmed
        .lines()
        .any(|l| l.trim_start().starts_with("# ") || l.trim_start().starts_with("## "));
    let sentence_terminators = trimmed
        .chars()
        .filter(|c| matches!(c, '.' | '?' | '!' | '。' | '！' | '？' | '…'))
        .count();
    let newline_paragraphs = trimmed.matches("\n\n").count();

    // Conversation markers (Korean).
    const CHAT_OPENERS: &[&str] = &[
        "안녕", "반가", "고마", "고맙", "감사", "미안", "죄송", "잠깐", "잠시만",
        "저기", "ㅎㅎ", "ㅋㅋ", "ㅠㅠ", "ㅜㅜ",
    ];
    const CHAT_PARTICLES: &[&str] = &[
        "요?", "까?", "죠?", "지?", "해?", "하셨어?", "줘", "주세요",
    ];
    let mut convo_score = 0i32;
    for marker in CHAT_OPENERS {
        if trimmed.starts_with(marker) || trimmed.contains(marker) {
            convo_score += 1;
        }
    }
    for p in CHAT_PARTICLES {
        if trimmed.contains(p) {
            convo_score += 1;
        }
    }
    // Single sentence + a question mark → very likely a direct question.
    if sentence_terminators <= 1 && trimmed.contains('?') {
        convo_score += 2;
    }

    // Knowledge markers.
    let mut knowledge_score = 0i32;
    if has_markdown_header {
        knowledge_score += 3;
    }
    if !compounds.is_empty() {
        knowledge_score += 2 + (compounds.len().min(3) as i32);
    }
    if sentence_terminators >= 4 {
        knowledge_score += 2;
    }
    if sentence_terminators >= 8 {
        knowledge_score += 1;
    }
    if newline_paragraphs >= 2 {
        knowledge_score += 2;
    }
    // Dense numeric / date enumerations (e.g., amounts, dates).
    let digit_runs = trimmed
        .split(|c: char| !c.is_ascii_digit())
        .filter(|s| s.len() >= 3)
        .count();
    if digit_runs >= 3 {
        knowledge_score += 1;
    }

    let is_knowledge = knowledge_score > convo_score && knowledge_score >= 3;
    let net = (knowledge_score - convo_score) as f32;
    let confidence = ((net.abs() / 8.0) + 0.4).clamp(0.4, 0.95);
    let reason = if is_knowledge {
        format!(
            "지식 점수 {knowledge_score} ≥ 잡담 점수 {convo_score} (헤더/복합토큰/문장 밀도로 판단)"
        )
    } else {
        format!(
            "잡담 점수 {convo_score} · 지식 점수 {knowledge_score} — 세컨드브레인 편입 부적합"
        )
    };

    KnowledgeVerdict {
        is_knowledge,
        confidence,
        reason,
    }
}

/// A single factual statement extracted from a vault document, used as
/// input to `AIEngine::detect_contradictions`.
#[derive(Debug, Clone)]
pub struct ContentClaim {
    pub doc_id: i64,
    pub title: String,
    /// ≤500 char snippet centred on the entity mention.
    pub statement: String,
}

/// A detected contradiction between two documents about the same entity.
#[derive(Debug, Clone)]
pub struct Contradiction {
    pub left_doc_id: i64,
    pub right_doc_id: i64,
    /// Short human-readable summary (e.g. "A says 2024-01-01, B says 2026-04-01").
    pub description: String,
    /// 1 (minor) – 10 (fundamental / case-altering)
    pub severity: u8,
}

/// Provider-free default. Strategy:
/// - Every compound token becomes a key concept at importance 9.
/// - The first H1 title (if any) is extracted as importance 10.
/// - Gatekeeper passes all candidates through; synonym pairs derived
///   from regex-detectable statute short-forms (e.g. "750조" ↔ "민법 제750조").
pub struct HeuristicAIEngine;

#[async_trait]
impl AIEngine for HeuristicAIEngine {
    async fn extract_key_concepts(
        &self,
        markdown: &str,
        compounds: &[CompoundToken],
    ) -> anyhow::Result<Vec<KeyConcept>> {
        let mut concepts = Vec::new();

        // Compound tokens → high importance.
        for c in compounds {
            let imp = match c.kind {
                CompoundTokenKind::StatuteArticle
                | CompoundTokenKind::PrecedentCitation
                | CompoundTokenKind::CaseNumber => 9,
                CompoundTokenKind::Organization => 8,
            };
            concepts.push(KeyConcept {
                term: c.canonical.clone(),
                importance: imp,
            });
        }

        // H1 title → importance 10 (first one only).
        for line in markdown.lines() {
            if let Some(rest) = line.trim_start().strip_prefix("# ") {
                let title = rest.trim();
                if !title.is_empty() {
                    concepts.push(KeyConcept {
                        term: title.to_string(),
                        importance: 10,
                    });
                    break;
                }
            }
        }

        Ok(concepts)
    }

    async fn gatekeep(
        &self,
        candidates: &[String],
        _doc_preview: &str,
    ) -> anyhow::Result<GatekeepVerdict> {
        let mut kept: Vec<String> = candidates.to_vec();
        kept.sort();
        kept.dedup();

        // Detect synonym pairs: "제NNN조" ↔ "민법 제NNN조" etc. (structural).
        let mut synonym_pairs: Vec<(String, String)> = Vec::new();
        for cand in &kept {
            if let Some((rep, alias)) = detect_statute_short_form(cand, &kept) {
                synonym_pairs.push((rep, alias));
            }
        }

        Ok(GatekeepVerdict {
            kept,
            synonym_pairs,
        })
    }

    async fn narrate_briefing(
        &self,
        case_number: &str,
        primary_docs: &[(i64, String, String)],
        related_docs: &[(i64, String)],
    ) -> anyhow::Result<BriefingNarrative> {
        // Structured template: deterministic, no LLM. Lists docs
        // per-section and leaves a note requesting LLM fill-in.
        let mut timeline = String::from("이 사건 **관련 문서 시계열**:\n\n");
        for (id, title, _) in primary_docs {
            timeline.push_str(&format!("- [Doc-{id}] {title}\n"));
        }

        let mut evidence = String::from("이 사건과 직접 매핑된 문서:\n\n");
        for (id, title, _) in primary_docs {
            evidence.push_str(&format!("- [Doc-{id}] {title}\n"));
        }
        if primary_docs.is_empty() {
            evidence.push_str("(사건 프론트매터 매칭 없음 — 문서에 `case_number` 필드 기재 필요)\n");
        }

        let precedents = if related_docs.is_empty() {
            "관련 판례·자료 매핑 없음.".to_string()
        } else {
            let mut s = String::from("1-depth 그래프 확장으로 식별된 관련 자료:\n\n");
            for (id, title) in related_docs {
                s.push_str(&format!("- [Doc-{id}] {title}\n"));
            }
            s
        };

        Ok(BriefingNarrative {
            timeline,
            contentions: format!(
                "사건번호 {case_number}에 대한 양측 주장 대비는 LLM 서사 합성에서 제공됩니다 (Heuristic 엔진은 구조만 채움)."
            ),
            issues: "핵심 쟁점은 LLM 서사 합성에서 제공됩니다.".to_string(),
            evidence,
            precedents,
            checklist: "- [ ] 쟁점 정리\n- [ ] 증거 현황\n- [ ] 다음 기일 준비사항\n".to_string(),
            strategy: "전략 제안은 LLM 서사 합성에서 제공됩니다.".to_string(),
        })
    }
}

/// If `candidate` is "민법 제750조" and "제750조" is also in the set,
/// return (rep="민법 제750조", alias="제750조"). Heuristic helper.
fn detect_statute_short_form(candidate: &str, all: &[String]) -> Option<(String, String)> {
    if let Some((_law, rest)) = candidate.split_once(' ') {
        if rest.starts_with("제") && rest.contains("조") && all.iter().any(|c| c == rest) {
            return Some((candidate.to_string(), rest.to_string()));
        }
    }
    None
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::vault::wikilink::tokens::detect_compound_tokens;

    #[tokio::test]
    async fn heuristic_promotes_compound_tokens() {
        let md = "본문에 민법 제750조가 등장한다.";
        let compounds = detect_compound_tokens(md);
        let concepts = HeuristicAIEngine
            .extract_key_concepts(md, &compounds)
            .await
            .unwrap();
        assert!(concepts.iter().any(|k| k.term == "민법 제750조" && k.importance >= 9));
    }

    #[tokio::test]
    async fn heuristic_extracts_h1_as_top_concept() {
        let md = "# 불법행위 손해배상 분석\n\n본문";
        let concepts = HeuristicAIEngine
            .extract_key_concepts(md, &[])
            .await
            .unwrap();
        assert!(concepts
            .iter()
            .any(|k| k.term == "불법행위 손해배상 분석" && k.importance == 10));
    }

    #[tokio::test]
    async fn gatekeeper_detects_statute_short_form() {
        let candidates = vec!["민법 제750조".to_string(), "제750조".to_string()];
        let verdict = HeuristicAIEngine
            .gatekeep(&candidates, "ignored preview")
            .await
            .unwrap();
        assert_eq!(verdict.synonym_pairs.len(), 1);
        assert_eq!(verdict.synonym_pairs[0].0, "민법 제750조");
        assert_eq!(verdict.synonym_pairs[0].1, "제750조");
    }

    #[tokio::test]
    async fn gatekeeper_deduplicates() {
        let candidates = vec!["A".into(), "A".into(), "B".into()];
        let verdict = HeuristicAIEngine
            .gatekeep(&candidates, "")
            .await
            .unwrap();
        assert_eq!(verdict.kept.len(), 2);
    }

    // ── Q1 heuristic classifier tests ─────────────────────────────

    #[test]
    fn classify_tiny_text_is_conversation() {
        let v = heuristic_knowledge_classify("안녕하세요");
        assert!(!v.is_knowledge);
    }

    #[test]
    fn classify_formal_legal_note_is_knowledge() {
        let text = "# 민법 제750조 요건 요약\n\n\
민법 제750조는 불법행위의 일반 조항이다. \
요건은 고의/과실, 위법성, 손해, 인과관계 4가지다. \
대법원 2026. 2. 2. 선고 2025다12345 판결은 입증 책임을 정리했다. \
재산상 + 정신상 손해 모두 청구 가능하다.";
        let v = heuristic_knowledge_classify(text);
        assert!(v.is_knowledge, "expected knowledge, got {v:?}");
    }

    #[test]
    fn classify_chatty_mid_length_is_conversation() {
        let text = "안녕하세요 변호사님. 오늘 시간 되시나요? \
잠시만 여쭤봐도 괜찮을까요? 고마워요. \
저기 저희 일정 조율이 필요해서요. 혹시 오후 3시에 가능한가요? \
답장 주세요. 감사합니다.";
        let v = heuristic_knowledge_classify(text);
        assert!(!v.is_knowledge, "expected conversation, got {v:?}");
    }

    #[tokio::test]
    async fn heuristic_engine_classify_as_knowledge_wraps_rule() {
        let text = "# 법률 요약\n\n\
민법 제750조는 불법행위의 일반 조항이다. \
요건은 고의/과실, 위법성, 손해 발생, 인과관계 4가지다. \
대법원 2026. 2. 2. 선고 2025다12345 판결이 이를 확인했다.";
        let v = HeuristicAIEngine
            .classify_as_knowledge(text)
            .await
            .unwrap();
        assert!(v.is_knowledge, "expected knowledge, got {v:?}");
    }
}
