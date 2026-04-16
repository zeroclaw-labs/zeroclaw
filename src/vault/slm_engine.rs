// @Ref: SUMMARY §6D-9 — on-device SLM AIEngine via Ollama HTTP.
//
// OllamaSlmEngine hits `http://<host>:<port>/api/chat` with a non-streaming
// JSON request. Works with any locally-installed Ollama model: gemma3:4b,
// qwen2.5:0.5b, phi4-mini:3.8b, etc. Perfect for idle-time hub compilation
// where latency matters less than cost/privacy.
//
// If the Ollama daemon is unreachable or the model isn't pulled, every
// call degrades to HeuristicAIEngine so the vault keeps functioning.

use crate::vault::wikilink::{
    ai_stub::{BriefingNarrative, HeuristicAIEngine},
    AIEngine, CompoundToken, GatekeepVerdict, KeyConcept,
};
use async_trait::async_trait;
use serde::{Deserialize, Serialize};
use std::time::Duration;

const DEFAULT_BASE: &str = "http://localhost:11434";
const REQUEST_TIMEOUT: Duration = Duration::from_secs(60);

pub struct OllamaSlmEngine {
    base_url: String,
    model: String,
    client: reqwest::Client,
    fallback: HeuristicAIEngine,
}

impl OllamaSlmEngine {
    /// Construct a new engine. `model` is the Ollama model tag, e.g.
    /// `"qwen2.5:0.5b"`, `"gemma3:4b"`, or `"phi4-mini:3.8b"`.
    pub fn new(model: impl Into<String>) -> Self {
        Self {
            base_url: DEFAULT_BASE.into(),
            model: model.into(),
            client: reqwest::Client::builder()
                .timeout(REQUEST_TIMEOUT)
                .build()
                .unwrap_or_else(|_| reqwest::Client::new()),
            fallback: HeuristicAIEngine,
        }
    }

    /// Override the Ollama base URL (e.g. remote Ollama server).
    pub fn with_base_url(mut self, url: impl Into<String>) -> Self {
        self.base_url = url.into();
        self
    }

    async fn chat(&self, prompt: &str) -> anyhow::Result<String> {
        #[derive(Serialize)]
        struct Req<'a> {
            model: &'a str,
            messages: Vec<Msg<'a>>,
            stream: bool,
            options: OllamaOptions,
        }
        #[derive(Serialize)]
        struct Msg<'a> {
            role: &'a str,
            content: &'a str,
        }
        #[derive(Serialize)]
        struct OllamaOptions {
            temperature: f32,
        }
        #[derive(Deserialize)]
        struct Resp {
            message: RespMsg,
        }
        #[derive(Deserialize)]
        struct RespMsg {
            content: String,
        }

        let url = format!("{}/api/chat", self.base_url);
        let body = Req {
            model: &self.model,
            messages: vec![Msg {
                role: "user",
                content: prompt,
            }],
            stream: false,
            options: OllamaOptions { temperature: 0.1 },
        };
        let resp = self
            .client
            .post(&url)
            .json(&body)
            .send()
            .await?
            .error_for_status()?;
        let parsed: Resp = resp.json().await?;
        Ok(parsed.message.content)
    }
}

#[async_trait]
impl AIEngine for OllamaSlmEngine {
    async fn extract_key_concepts(
        &self,
        markdown: &str,
        compounds: &[CompoundToken],
    ) -> anyhow::Result<Vec<KeyConcept>> {
        let preview = markdown.chars().take(2400).collect::<String>();
        let compound_hint = compounds
            .iter()
            .take(20)
            .map(|c| format!("- {}", c.canonical))
            .collect::<Vec<_>>()
            .join("\n");
        let prompt = format!(
            "다음 한국어 문서에서 **핵심 키워드** JSON 배열만 출력 (설명 금지). \
형식: [{{\"term\":\"...\",\"importance\":1-10}}]. 최대 20개. \
복합 토큰 힌트:\n{compound_hint}\n\n본문:\n{preview}"
        );
        let txt = match self.chat(&prompt).await {
            Ok(t) => t,
            Err(e) => {
                tracing::warn!("OllamaSlmEngine unavailable ({e}); falling back");
                return self.fallback.extract_key_concepts(markdown, compounds).await;
            }
        };
        match parse_concepts_json(&txt) {
            Some(v) => Ok(v),
            None => {
                tracing::warn!("OllamaSlmEngine returned non-JSON; falling back");
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
            "후보 중 '진짜 핵심'만 남기고 동의어 쌍을 식별. JSON만: \
{{\"kept\":[\"...\"],\"synonyms\":[[\"rep\",\"alias\"]]}}\n\n\
후보:\n{cand_list}\n\n본문:\n{}",
            doc_preview.chars().take(600).collect::<String>()
        );
        let txt = match self.chat(&prompt).await {
            Ok(t) => t,
            Err(e) => {
                tracing::warn!("OllamaSlmEngine gatekeep unavailable ({e}); falling back");
                return self.fallback.gatekeep(candidates, doc_preview).await;
            }
        };
        match parse_gatekeep_json(&txt) {
            Some((kept, synonyms)) => {
                let kept = if kept.is_empty() { candidates.to_vec() } else { kept };
                Ok(GatekeepVerdict {
                    kept,
                    synonym_pairs: synonyms,
                })
            }
            None => self.fallback.gatekeep(candidates, doc_preview).await,
        }
    }

    async fn narrate_briefing(
        &self,
        case_number: &str,
        primary_docs: &[(i64, String, String)],
        related_docs: &[(i64, String)],
    ) -> anyhow::Result<BriefingNarrative> {
        // Delegate to fallback for narrative; small SLMs are unreliable on
        // 7-section structured output. Deliberate design choice — let
        // LlmAIEngine (cloud) handle this path.
        self.fallback
            .narrate_briefing(case_number, primary_docs, related_docs)
            .await
    }
}

fn parse_concepts_json(text: &str) -> Option<Vec<KeyConcept>> {
    let start = text.find('[')?;
    let end = text.rfind(']')?;
    if end <= start {
        return None;
    }
    #[derive(Deserialize)]
    struct Raw {
        term: String,
        importance: u8,
    }
    let raw: Vec<Raw> = serde_json::from_str(&text[start..=end]).ok()?;
    Some(
        raw.into_iter()
            .filter(|r| r.importance >= 1 && r.importance <= 10)
            .map(|r| KeyConcept {
                term: r.term,
                importance: r.importance,
            })
            .collect(),
    )
}

fn parse_gatekeep_json(text: &str) -> Option<(Vec<String>, Vec<(String, String)>)> {
    let start = text.find('{')?;
    let end = text.rfind('}')?;
    if end <= start {
        return None;
    }
    #[derive(Deserialize, Default)]
    struct Raw {
        #[serde(default)]
        kept: Vec<String>,
        #[serde(default)]
        synonyms: Vec<(String, String)>,
    }
    let raw: Raw = serde_json::from_str(&text[start..=end]).ok()?;
    Some((raw.kept, raw.synonyms))
}

#[cfg(test)]
mod tests {
    use super::*;

    /// When Ollama is unreachable (as in the test environment), every
    /// method should degrade to the Heuristic fallback without erroring.
    #[tokio::test]
    async fn unreachable_ollama_falls_back_cleanly() {
        let engine = OllamaSlmEngine::new("qwen2.5:0.5b")
            .with_base_url("http://127.0.0.1:1"); // guaranteed closed port

        // extract: heuristic returns compound tokens + H1 title.
        let md = "# 테스트\n\n민법 제750조에 근거한 사건";
        let compounds = crate::vault::wikilink::tokens::detect_compound_tokens(md);
        let concepts = engine
            .extract_key_concepts(md, &compounds)
            .await
            .expect("fallback should succeed");
        assert!(concepts.iter().any(|c| c.term == "민법 제750조"));

        // gatekeep: heuristic passes everything through + detects statute short form.
        let verdict = engine
            .gatekeep(&["민법 제750조".into(), "제750조".into()], "preview")
            .await
            .expect("fallback should succeed");
        assert!(verdict.kept.len() >= 2);
        assert!(verdict
            .synonym_pairs
            .iter()
            .any(|(rep, _)| rep == "민법 제750조"));
    }

    #[test]
    fn parse_concepts_json_handles_clean_input() {
        let txt = r#"Here: [{"term":"민법 제750조","importance":9}]"#;
        let out = parse_concepts_json(txt).unwrap();
        assert_eq!(out.len(), 1);
        assert_eq!(out[0].term, "민법 제750조");
    }

    #[test]
    fn parse_gatekeep_json_accepts_partial_payload() {
        let txt = r#"{"kept":["A","B"]}"#;
        let (kept, syn) = parse_gatekeep_json(txt).unwrap();
        assert_eq!(kept.len(), 2);
        assert!(syn.is_empty());
    }

    #[test]
    fn parse_concepts_rejects_invalid_importance() {
        let txt = r#"[{"term":"X","importance":99}]"#;
        let out = parse_concepts_json(txt).unwrap_or_default();
        assert!(out.is_empty());
    }
}
