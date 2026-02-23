//! Fact extraction from conversation text via LLM (OpenRouter) or mock.
//!
//! [`FactExtractor`] defines the async trait for extracting structured
//! [`FactEntryDraft`] items from free-text prompts.  Two implementations are
//! provided:
//!
//! * [`OpenRouterFactExtractor`] — calls an OpenAI-compatible chat completions
//!   endpoint (e.g. OpenRouter) over HTTP.
//! * [`MockFactExtractor`] — returns a fixed set of drafts; useful for tests.

use anyhow::{anyhow, Result};
use async_trait::async_trait;
use serde::{Deserialize, Serialize};
use std::time::Duration;

// ── FactEntryDraft ───────────────────────────────────────────────────────────

/// Lightweight draft returned by the LLM before enrichment into a full
/// [`super::facts::FactEntry`].
#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct FactEntryDraft {
    pub category: String,
    pub subject: String,
    pub attribute: String,
    pub value: String,
    pub context_narrative: String,
    /// One of `"low"`, `"medium"`, `"high"`.
    pub confidence: String,
    pub related_facts: Vec<String>,
    /// One of `"stable"`, `"semi_stable"`, `"volatile"`.
    pub volatility_class: String,
}

// ── FactExtractor trait ──────────────────────────────────────────────────────

/// Async trait for extracting structured facts from free-text.
#[async_trait]
pub trait FactExtractor: Send + Sync {
    /// Extract zero or more [`FactEntryDraft`] items from the given prompt text.
    async fn extract(&self, prompt: &str) -> Result<Vec<FactEntryDraft>>;
}

// ── JSON parsing helpers ─────────────────────────────────────────────────────

/// Parse an LLM response string into a `Vec<FactEntryDraft>`.
///
/// Tries, in order:
/// 1. Raw JSON array (`[{...}, ...]`).
/// 2. Markdown-fenced JSON (` ```json ... ``` `).
/// 3. Locating a JSON array anywhere in the text (first `[` to last `]`).
///
/// Returns an error if none of the strategies succeed.
pub fn parse_extraction_response(response: &str) -> Result<Vec<FactEntryDraft>> {
    let trimmed = response.trim();

    // 1. Try raw JSON array.
    if let Ok(facts) = serde_json::from_str::<Vec<FactEntryDraft>>(trimmed) {
        return Ok(facts);
    }

    // 2. Try markdown-fenced ```json ... ```.
    if let Some(inner) = extract_fenced_json(trimmed) {
        if let Ok(facts) = serde_json::from_str::<Vec<FactEntryDraft>>(inner.trim()) {
            return Ok(facts);
        }
    }

    // 3. Try to find a JSON array anywhere in the text.
    if let Some(start) = trimmed.find('[') {
        if let Some(end) = trimmed.rfind(']') {
            if end > start {
                let slice = &trimmed[start..=end];
                if let Ok(facts) = serde_json::from_str::<Vec<FactEntryDraft>>(slice) {
                    return Ok(facts);
                }
            }
        }
    }

    Err(anyhow!(
        "failed to parse fact extraction response as JSON array"
    ))
}

/// Extract the content inside a ` ```json ... ``` ` fence, if present.
fn extract_fenced_json(text: &str) -> Option<&str> {
    let fence_start = text.find("```json")?;
    let content_start = fence_start + "```json".len();
    let rest = &text[content_start..];
    let fence_end = rest.find("```")?;
    Some(&rest[..fence_end])
}

// ── MockFactExtractor ────────────────────────────────────────────────────────

/// A test-only extractor that returns a fixed set of drafts.
pub struct MockFactExtractor {
    response: Vec<FactEntryDraft>,
}

impl MockFactExtractor {
    /// Create a mock that always returns the given drafts.
    pub fn new(response: Vec<FactEntryDraft>) -> Self {
        Self { response }
    }
}

#[async_trait]
impl FactExtractor for MockFactExtractor {
    async fn extract(&self, _prompt: &str) -> Result<Vec<FactEntryDraft>> {
        Ok(self.response.clone())
    }
}

// ── OpenRouter chat-completions DTOs ─────────────────────────────────────────

#[derive(Debug, Serialize)]
struct ChatMessage {
    role: String,
    content: String,
}

#[derive(Debug, Serialize)]
struct ChatRequest {
    model: String,
    messages: Vec<ChatMessage>,
    max_tokens: usize,
    temperature: f32,
}

#[derive(Debug, Deserialize)]
struct ChatResponse {
    choices: Vec<ChatChoice>,
}

#[derive(Debug, Deserialize)]
struct ChatChoice {
    message: ChatChoiceMessage,
}

#[derive(Debug, Deserialize)]
struct ChatChoiceMessage {
    content: String,
}

// ── OpenRouterFactExtractor ──────────────────────────────────────────────────

/// Fact extractor that calls an OpenAI-compatible chat completions endpoint.
pub struct OpenRouterFactExtractor {
    client: reqwest::Client,
    base_url: String,
    api_key: String,
    model: String,
    max_tokens: usize,
    temperature: f32,
}

impl OpenRouterFactExtractor {
    /// Create a new extractor targeting the given OpenRouter (or compatible) API.
    pub fn new(
        base_url: &str,
        api_key: &str,
        model: &str,
        max_tokens: usize,
        temperature: f32,
        timeout_secs: u64,
    ) -> Self {
        let client = reqwest::Client::builder()
            .timeout(Duration::from_secs(timeout_secs))
            .build()
            .expect("failed to build reqwest client");

        Self {
            client,
            base_url: base_url.trim_end_matches('/').to_string(),
            api_key: api_key.to_string(),
            model: model.to_string(),
            max_tokens,
            temperature,
        }
    }
}

#[async_trait]
impl FactExtractor for OpenRouterFactExtractor {
    async fn extract(&self, prompt: &str) -> Result<Vec<FactEntryDraft>> {
        let url = format!("{}/chat/completions", self.base_url);

        let body = ChatRequest {
            model: self.model.clone(),
            messages: vec![ChatMessage {
                role: "user".to_string(),
                content: prompt.to_string(),
            }],
            max_tokens: self.max_tokens,
            temperature: self.temperature,
        };

        let resp = self
            .client
            .post(&url)
            .header("Authorization", format!("Bearer {}", self.api_key))
            .json(&body)
            .send()
            .await?;

        if !resp.status().is_success() {
            let status = resp.status();
            let text = resp.text().await.unwrap_or_default();
            return Err(anyhow!("OpenRouter API error (HTTP {}): {}", status, text));
        }

        let chat_resp: ChatResponse = resp.json().await?;

        let content = chat_resp
            .choices
            .first()
            .map(|c| c.message.content.as_str())
            .unwrap_or("[]");

        parse_extraction_response(content)
    }
}

// ── Tests ────────────────────────────────────────────────────────────────────

#[cfg(test)]
mod tests {
    use super::*;

    /// Helper: build a sample FactEntryDraft for testing.
    fn sample_draft() -> FactEntryDraft {
        FactEntryDraft {
            category: "personal".to_string(),
            subject: "user".to_string(),
            attribute: "favorite_color".to_string(),
            value: "blue".to_string(),
            context_narrative: "User said they love blue.".to_string(),
            confidence: "high".to_string(),
            related_facts: vec![],
            volatility_class: "stable".to_string(),
        }
    }

    #[tokio::test]
    async fn mock_extractor_returns_facts() {
        let draft = sample_draft();
        let extractor = MockFactExtractor::new(vec![draft.clone()]);
        let results = extractor.extract("tell me about the user").await.unwrap();
        assert_eq!(results.len(), 1);
        assert_eq!(results[0].category, "personal");
        assert_eq!(results[0].subject, "user");
        assert_eq!(results[0].attribute, "favorite_color");
        assert_eq!(results[0].value, "blue");
    }

    #[tokio::test]
    async fn mock_extractor_empty_input() {
        let extractor = MockFactExtractor::new(vec![]);
        let results = extractor.extract("").await.unwrap();
        assert!(results.is_empty());
    }

    #[test]
    fn parse_extraction_response_valid_json() {
        let json = serde_json::to_string(&vec![sample_draft()]).unwrap();
        let results = parse_extraction_response(&json).unwrap();
        assert_eq!(results.len(), 1);
        assert_eq!(results[0].value, "blue");
    }

    #[test]
    fn parse_extraction_response_invalid_json() {
        let result = parse_extraction_response("this is not json at all");
        assert!(result.is_err());
    }

    #[test]
    fn parse_extraction_response_extracts_from_markdown_fence() {
        let json = serde_json::to_string(&vec![sample_draft()]).unwrap();
        let markdown = format!("Here are the facts:\n```json\n{}\n```\nDone.", json);
        let results = parse_extraction_response(&markdown).unwrap();
        assert_eq!(results.len(), 1);
        assert_eq!(results[0].attribute, "favorite_color");
    }
}
