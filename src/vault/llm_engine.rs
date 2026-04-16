// @Ref: SUMMARY §6D-9 — production AIEngine backed by providers::Provider.
//
// LlmAIEngine calls a provider (Haiku/Opus) for Step 2a (key concepts)
// and Step 4 (gatekeeper). JSON-shaped prompts + defensive parsing —
// malformed responses fall back to `HeuristicAIEngine` output.
//
// Usage:
//   let engine = LlmAIEngine::new(provider, "claude-haiku-4-5-20251001");
//   let vault = VaultStore::with_shared_connection(conn)?.with_ai_engine(Arc::new(engine));

use crate::providers::traits::Provider;
use crate::vault::wikilink::{
    ai_stub::{
        heuristic_knowledge_classify, BriefingNarrative, ContentClaim, Contradiction,
        HeuristicAIEngine, KnowledgeVerdict,
    },
    AIEngine, CompoundToken, GatekeepVerdict, KeyConcept,
};
use async_trait::async_trait;
use serde::Deserialize;
use std::sync::Arc;

const DEFAULT_TEMPERATURE: f64 = 0.1;

pub struct LlmAIEngine {
    provider: Arc<dyn Provider>,
    model: String,
    /// Used when the provider call fails or returns malformed output.
    fallback: HeuristicAIEngine,
}

impl LlmAIEngine {
    pub fn new(provider: Arc<dyn Provider>, model: impl Into<String>) -> Self {
        Self {
            provider,
            model: model.into(),
            fallback: HeuristicAIEngine,
        }
    }
}

#[derive(Debug, Deserialize)]
struct AiConcept {
    term: String,
    importance: u8,
}

#[derive(Debug, Deserialize)]
struct AiGatekeepResponse {
    #[serde(default)]
    kept: Vec<String>,
    #[serde(default)]
    synonyms: Vec<(String, String)>,
}

#[derive(Debug, Deserialize, Default)]
struct AiBriefingResponse {
    #[serde(default)]
    timeline: String,
    #[serde(default)]
    contentions: String,
    #[serde(default)]
    issues: String,
    #[serde(default)]
    evidence: String,
    #[serde(default)]
    precedents: String,
    #[serde(default)]
    checklist: String,
    #[serde(default)]
    strategy: String,
}

#[async_trait]
impl AIEngine for LlmAIEngine {
    async fn extract_key_concepts(
        &self,
        markdown: &str,
        compounds: &[CompoundToken],
    ) -> anyhow::Result<Vec<KeyConcept>> {
        let preview = markdown.chars().take(3000).collect::<String>();
        let compound_hint = compounds
            .iter()
            .take(20)
            .map(|c| format!("- {}", c.canonical))
            .collect::<Vec<_>>()
            .join("\n");
        let prompt = format!(
            "다음 한국어 마크다운 문서의 **핵심 키워드**를 JSON으로 추출하세요. \
규칙: (a) 도메인 보편 상용구('원고','피고','청구' 등)는 제외, (b) 고유명사· 법조문·사건번호·기관명을 우선, \
(c) 각 키워드에 1~10 중요도 점수(10=문서 존재 이유). 최대 20개. \
오직 JSON 배열만 출력 — 설명 금지. 예: [{{\"term\":\"민법 제750조\",\"importance\":9}}].\n\n\
이미 감지된 복합 토큰:\n{compound_hint}\n\n문서 본문:\n---\n{preview}\n---"
        );
        let raw = self
            .provider
            .simple_chat(&prompt, &self.model, DEFAULT_TEMPERATURE)
            .await;
        let txt = match raw {
            Ok(t) => t,
            Err(e) => {
                tracing::warn!("LlmAIEngine extract failed, falling back: {e}");
                return self.fallback.extract_key_concepts(markdown, compounds).await;
            }
        };
        match extract_json_array::<AiConcept>(&txt) {
            Ok(parsed) => Ok(parsed
                .into_iter()
                .filter(|c| c.importance >= 1 && c.importance <= 10)
                .map(|c| KeyConcept {
                    term: c.term,
                    importance: c.importance,
                })
                .collect()),
            Err(e) => {
                tracing::warn!("LlmAIEngine extract parse failed: {e}; falling back");
                self.fallback.extract_key_concepts(markdown, compounds).await
            }
        }
    }

    async fn gatekeep(
        &self,
        candidates: &[String],
        doc_preview: &str,
    ) -> anyhow::Result<GatekeepVerdict> {
        if candidates.is_empty() {
            return Ok(GatekeepVerdict::default());
        }
        let cand_list = candidates
            .iter()
            .enumerate()
            .map(|(i, c)| format!("{}. {}", i + 1, c))
            .collect::<Vec<_>>()
            .join("\n");
        let prompt = format!(
            "다음 후보 키워드 목록에 대해 (1) 이 문서 맥락에서 '진짜 핵심 키워드'만 남기고 (2) 같은 의미의 동의어 쌍을 식별하세요. \
JSON만 반환:\n{{\"kept\":[\"...\"],\"synonyms\":[[\"대표표현\",\"원문표현\"]]}}\n\n\
후보:\n{cand_list}\n\n문서 발췌:\n---\n{doc_preview}\n---"
        );
        let raw = self
            .provider
            .simple_chat(&prompt, &self.model, DEFAULT_TEMPERATURE)
            .await;
        let txt = match raw {
            Ok(t) => t,
            Err(e) => {
                tracing::warn!("LlmAIEngine gatekeep failed, falling back: {e}");
                return self.fallback.gatekeep(candidates, doc_preview).await;
            }
        };
        match extract_json_object::<AiGatekeepResponse>(&txt) {
            Ok(r) => {
                let kept: Vec<String> = if r.kept.is_empty() {
                    candidates.to_vec()
                } else {
                    r.kept
                };
                let synonym_pairs = r.synonyms;
                Ok(GatekeepVerdict {
                    kept,
                    synonym_pairs,
                })
            }
            Err(e) => {
                tracing::warn!("LlmAIEngine gatekeep parse failed: {e}; falling back");
                self.fallback.gatekeep(candidates, doc_preview).await
            }
        }
    }

    async fn assign_hub_sections(
        &self,
        subtype: &str,
        sections: &[&str],
        docs: &[(i64, String, String)],
    ) -> anyhow::Result<Vec<Vec<usize>>> {
        if docs.is_empty() || sections.is_empty() {
            return Ok(Vec::new());
        }
        let section_list = sections
            .iter()
            .enumerate()
            .map(|(i, s)| format!("  {i}: {s}"))
            .collect::<Vec<_>>()
            .join("\n");
        let doc_block = docs
            .iter()
            .map(|(id, title, preview)| {
                format!(
                    "[Doc-{id}] {title}\n발췌: {}",
                    preview.chars().take(350).collect::<String>()
                )
            })
            .collect::<Vec<_>>()
            .join("\n\n");
        let prompt = format!(
            "당신은 법률 허브노트 편집자. 아래 {subtype} 허브의 각 섹션에 어떤 \
백링크 문서를 매핑할지 결정하세요. 각 문서는 **1개 이상의** 섹션에 속할 수 \
있습니다. 관련 없으면 빈 배열.\n\n\
반환: JSON 객체 {{\"assignments\":[[섹션번호,...], ...]}} — 배열 길이는 \
입력 문서 수와 같아야 합니다. 오직 JSON만.\n\n\
섹션:\n{section_list}\n\n문서:\n{doc_block}"
        );
        let raw = match self
            .provider
            .simple_chat(&prompt, &self.model, DEFAULT_TEMPERATURE)
            .await
        {
            Ok(t) => t,
            Err(e) => {
                tracing::warn!("assign_hub_sections call failed, falling back: {e}");
                return self.fallback.assign_hub_sections(subtype, sections, docs).await;
            }
        };
        #[derive(Deserialize)]
        struct Resp {
            #[serde(default)]
            assignments: Vec<Vec<usize>>,
        }
        match extract_json_object::<Resp>(&raw) {
            Ok(r) if r.assignments.len() == docs.len() => Ok(r.assignments),
            Ok(_) => {
                tracing::warn!("assign_hub_sections length mismatch; falling back");
                self.fallback.assign_hub_sections(subtype, sections, docs).await
            }
            Err(e) => {
                tracing::warn!("assign_hub_sections parse failed: {e}; falling back");
                self.fallback.assign_hub_sections(subtype, sections, docs).await
            }
        }
    }

    async fn detect_contradictions(
        &self,
        entity: &str,
        claims: &[ContentClaim],
    ) -> anyhow::Result<Vec<Contradiction>> {
        if claims.len() < 2 {
            return Ok(Vec::new());
        }
        let claim_block = claims
            .iter()
            .enumerate()
            .map(|(i, c)| {
                format!(
                    "[{i}] Doc-{} {title}\n  주장: {statement}",
                    c.doc_id,
                    title = c.title,
                    statement = c.statement.chars().take(400).collect::<String>(),
                )
            })
            .collect::<Vec<_>>()
            .join("\n\n");
        let prompt = format!(
            "엔티티 '{entity}'에 대한 아래 주장들을 비교해 **사실이 충돌하는 \
쌍**을 찾으세요. 사실 진술이 아닌 의견 차이는 제외. 충돌 없으면 빈 배열. \
JSON만: {{\"contradictions\":[{{\"left\":<idx>,\"right\":<idx>,\
\"description\":\"짧은 요약\",\"severity\":1-10}}]}}\n\n주장:\n{claim_block}"
        );
        let raw = match self
            .provider
            .simple_chat(&prompt, &self.model, DEFAULT_TEMPERATURE)
            .await
        {
            Ok(t) => t,
            Err(e) => {
                tracing::warn!("detect_contradictions call failed, falling back: {e}");
                return Ok(Vec::new());
            }
        };
        #[derive(Deserialize)]
        struct Item {
            left: usize,
            right: usize,
            #[serde(default)]
            description: String,
            #[serde(default)]
            severity: u8,
        }
        #[derive(Deserialize)]
        struct Resp {
            #[serde(default)]
            contradictions: Vec<Item>,
        }
        match extract_json_object::<Resp>(&raw) {
            Ok(r) => Ok(r
                .contradictions
                .into_iter()
                .filter_map(|it| {
                    let (l, r) = (claims.get(it.left)?, claims.get(it.right)?);
                    Some(Contradiction {
                        left_doc_id: l.doc_id,
                        right_doc_id: r.doc_id,
                        description: it.description,
                        severity: it.severity.clamp(1, 10),
                    })
                })
                .collect()),
            Err(e) => {
                tracing::warn!("detect_contradictions parse failed: {e}");
                Ok(Vec::new())
            }
        }
    }

    async fn classify_as_knowledge(
        &self,
        text: &str,
    ) -> anyhow::Result<KnowledgeVerdict> {
        let preview = text.chars().take(1800).collect::<String>();
        let prompt = format!(
            "아래 텍스트가 '참조 지식'(세컨드브레인에 저장할 가치가 있는 문서)인지 \
'일상 대화'인지 분류하세요. 법률·의료·금융 등 전문 도메인의 요약·해설·정의·\
메모·판례 요약 등은 지식. 인사·안부·간단 질문·감탄·답장 단문은 대화. \
JSON만 반환: {{\"is_knowledge\":true|false,\"confidence\":0.0-1.0,\
\"reason\":\"짧은 설명\"}}\n\n텍스트:\n---\n{preview}\n---"
        );
        let raw = match self
            .provider
            .simple_chat(&prompt, &self.model, DEFAULT_TEMPERATURE)
            .await
        {
            Ok(t) => t,
            Err(e) => {
                tracing::warn!("classify_as_knowledge call failed, falling back: {e}");
                return Ok(heuristic_knowledge_classify(text));
            }
        };
        #[derive(Deserialize)]
        struct Resp {
            #[serde(default)]
            is_knowledge: bool,
            #[serde(default = "default_conf")]
            confidence: f32,
            #[serde(default)]
            reason: String,
        }
        fn default_conf() -> f32 { 0.7 }
        match extract_json_object::<Resp>(&raw) {
            Ok(r) => Ok(KnowledgeVerdict {
                is_knowledge: r.is_knowledge,
                confidence: r.confidence.clamp(0.0, 1.0),
                reason: if r.reason.is_empty() {
                    "LLM 판정".into()
                } else {
                    r.reason
                },
            }),
            Err(e) => {
                tracing::warn!("classify_as_knowledge parse failed: {e}; falling back");
                Ok(heuristic_knowledge_classify(text))
            }
        }
    }

    async fn narrate_briefing(
        &self,
        case_number: &str,
        primary_docs: &[(i64, String, String)],
        related_docs: &[(i64, String)],
    ) -> anyhow::Result<BriefingNarrative> {
        let primary_block = primary_docs
            .iter()
            .map(|(id, title, preview)| {
                format!(
                    "[Doc-{id}] {title}\n  발췌: {}\n",
                    preview.chars().take(400).collect::<String>()
                )
            })
            .collect::<Vec<_>>()
            .join("\n");
        let related_block = related_docs
            .iter()
            .map(|(id, title)| format!("[Doc-{id}] {title}"))
            .collect::<Vec<_>>()
            .join("\n");

        let prompt = format!(
            "당신은 한국 변호사의 사건 브리핑 보조자다. 아래 사건번호의 **브리핑**을 \
7개 섹션으로 JSON 객체로 작성하라. 각 섹션은 마크다운 문자열이며, \
출처가 있는 주장은 [Doc-N] 형태로 각주 인용하라. 근거가 부족한 섹션은 \
'증거 부족' 플래그로 짧게 명시. 오직 JSON만 출력 — 다른 텍스트 금지.\n\n\
JSON 스키마:\n\
{{\"timeline\":\"\",\"contentions\":\"\",\"issues\":\"\",\"evidence\":\"\",\
\"precedents\":\"\",\"checklist\":\"\",\"strategy\":\"\"}}\n\n\
사건번호: {case_number}\n\n\
### 이 사건 직접 매핑 문서\n{primary_block}\n\n\
### 1-depth 그래프 확장 자료\n{related_block}\n"
        );
        let raw = self
            .provider
            .simple_chat(&prompt, &self.model, DEFAULT_TEMPERATURE)
            .await;
        let txt = match raw {
            Ok(t) => t,
            Err(e) => {
                tracing::warn!("LlmAIEngine narrate_briefing failed, falling back: {e}");
                return self
                    .fallback
                    .narrate_briefing(case_number, primary_docs, related_docs)
                    .await;
            }
        };
        match extract_json_object::<AiBriefingResponse>(&txt) {
            Ok(r) => Ok(BriefingNarrative {
                timeline: r.timeline,
                contentions: r.contentions,
                issues: r.issues,
                evidence: r.evidence,
                precedents: r.precedents,
                checklist: r.checklist,
                strategy: r.strategy,
            }),
            Err(e) => {
                tracing::warn!("LlmAIEngine narrate_briefing parse failed: {e}; falling back");
                self.fallback
                    .narrate_briefing(case_number, primary_docs, related_docs)
                    .await
            }
        }
    }
}

/// Find the first `[ … ]` span in `text` and deserialize as Vec<T>.
fn extract_json_array<T: for<'de> serde::Deserialize<'de>>(text: &str) -> anyhow::Result<Vec<T>> {
    let bytes = text.as_bytes();
    let start = bytes.iter().position(|b| *b == b'[').ok_or_else(|| anyhow::anyhow!("no [ found"))?;
    let end = bytes.iter().rposition(|b| *b == b']').ok_or_else(|| anyhow::anyhow!("no ] found"))?;
    if end <= start {
        anyhow::bail!("invalid bracket span");
    }
    let slice = &text[start..=end];
    Ok(serde_json::from_str::<Vec<T>>(slice)?)
}

/// Find the first `{ … }` span and deserialize as T.
fn extract_json_object<T: for<'de> serde::Deserialize<'de>>(text: &str) -> anyhow::Result<T> {
    let bytes = text.as_bytes();
    let start = bytes.iter().position(|b| *b == b'{').ok_or_else(|| anyhow::anyhow!("no opening brace"))?;
    let end = bytes.iter().rposition(|b| *b == b'}').ok_or_else(|| anyhow::anyhow!("no closing brace"))?;
    if end <= start {
        anyhow::bail!("invalid brace span");
    }
    let slice = &text[start..=end];
    Ok(serde_json::from_str::<T>(slice)?)
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::providers::traits::Provider;
    use async_trait::async_trait;
    use std::sync::Mutex as StdMutex;

    /// Mock provider that returns a preprogrammed response string.
    /// Only implements the single required `chat_with_system` method;
    /// default impls cover `chat_with_history`, `chat`, `simple_chat`.
    struct MockProvider {
        response: StdMutex<String>,
        calls: StdMutex<usize>,
    }
    impl MockProvider {
        fn new(response: &str) -> Self {
            Self {
                response: StdMutex::new(response.into()),
                calls: StdMutex::new(0),
            }
        }
    }

    #[async_trait]
    impl Provider for MockProvider {
        async fn chat_with_system(
            &self,
            _system_prompt: Option<&str>,
            _message: &str,
            _model: &str,
            _temperature: f64,
        ) -> anyhow::Result<String> {
            *self.calls.lock().unwrap() += 1;
            Ok(self.response.lock().unwrap().clone())
        }
    }

    #[tokio::test]
    async fn extract_key_concepts_parses_valid_json() {
        let provider: Arc<dyn Provider> = Arc::new(MockProvider::new(
            r#"Here you go:
            [{"term":"민법 제750조","importance":9},{"term":"불법행위","importance":7}]"#,
        ));
        let engine = LlmAIEngine::new(provider, "claude-haiku-4-5-20251001");
        let out = engine.extract_key_concepts("본문", &[]).await.unwrap();
        assert_eq!(out.len(), 2);
        assert_eq!(out[0].term, "민법 제750조");
        assert_eq!(out[0].importance, 9);
    }

    #[tokio::test]
    async fn extract_falls_back_on_garbage_response() {
        let provider: Arc<dyn Provider> = Arc::new(MockProvider::new("not json at all"));
        let engine = LlmAIEngine::new(provider, "claude-haiku-4-5-20251001");
        // Fallback returns empty vec for plain body with no compound tokens or H1.
        let out = engine.extract_key_concepts("짧은 본문", &[]).await.unwrap();
        assert!(out.is_empty() || out.iter().all(|c| c.importance <= 10));
    }

    #[tokio::test]
    async fn gatekeep_parses_object() {
        let provider: Arc<dyn Provider> = Arc::new(MockProvider::new(
            r#"{"kept":["민법 제750조","손해배상"],"synonyms":[["민법 제750조","750조"]]}"#,
        ));
        let engine = LlmAIEngine::new(provider, "m");
        let verdict = engine
            .gatekeep(&["민법 제750조".into(), "손해배상".into(), "750조".into()], "preview")
            .await
            .unwrap();
        assert_eq!(verdict.kept, vec!["민법 제750조", "손해배상"]);
        assert_eq!(verdict.synonym_pairs.len(), 1);
    }
}
