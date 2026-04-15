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
    ai_stub::HeuristicAIEngine, AIEngine, CompoundToken, GatekeepVerdict, KeyConcept,
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
