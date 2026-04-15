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
}
