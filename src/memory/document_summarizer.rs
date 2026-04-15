//! Document summarizer using Claude Haiku for cost-efficient one-time summarization.
//!
//! When a user uploads a document or pastes 2000+ chars, this module calls
//! Claude Haiku (cheap model) to generate a structured summary that includes:
//! - Title
//! - 3-5 key points
//! - Extracted entities (law names, case IDs, function names, etc.)
//! - Themes (topic labels for categorization)
//!
//! The full original is stored in the `documents` table. The summary goes
//! into chat context. The LLM can fetch the original via `document_fetch`
//! if needed.
//!
//! ## Why Haiku?
//!
//! - ~1/10th the cost of Sonnet/Opus
//! - Runs once per document (amortized over all future recalls)
//! - Summarization is a well-suited task for smaller models
//! - Claude Haiku 4.5 is recent and capable

use crate::memory::document_store::{
    build_summary_prompt, parse_summary_output, DocumentStore, DOCUMENT_MIN_CHARS,
};
use crate::providers::anthropic::AnthropicProvider;
use crate::providers::traits::{ChatMessage, Provider};
use anyhow::Result;

/// Claude Haiku model ID for summarization (cheap + fast).
pub const HAIKU_MODEL_ID: &str = "claude-haiku-4-5-20251001";

/// Result of summarizing and storing a document.
#[derive(Debug, Clone)]
pub struct SummarizedDocument {
    pub content_id: String,
    pub title: String,
    pub summary: String,
    pub entities: Vec<String>,
    pub themes: Vec<String>,
    pub char_count: usize,
}

/// Check if content is long enough to warrant summarization and storage.
pub fn should_summarize(content: &str) -> bool {
    content.chars().count() >= DOCUMENT_MIN_CHARS
}

/// Summarize content with Claude Haiku and store in the document table.
///
/// This is the one-time cost — afterwards the summary is reused forever
/// via document_search. Returns the stored document metadata.
pub async fn summarize_and_store(
    content: &str,
    category: &str,
    source_type: &str,
    store: &DocumentStore,
    anthropic_key: Option<&str>,
) -> Result<SummarizedDocument> {
    let char_count = content.chars().count();
    if char_count < DOCUMENT_MIN_CHARS {
        anyhow::bail!(
            "Content too short to summarize ({}자 < {DOCUMENT_MIN_CHARS}자)",
            char_count
        );
    }

    let prompt = build_summary_prompt(content, category);

    // Call Claude Haiku for summarization
    let provider = AnthropicProvider::new(anthropic_key);
    let messages = vec![ChatMessage::user(&prompt)];

    let raw_summary = provider
        .chat_with_history(&messages, HAIKU_MODEL_ID, 0.2)
        .await
        .map_err(|e| anyhow::anyhow!("Haiku summarization failed: {e}"))?;

    let raw_summary = raw_summary.trim().to_string();
    if raw_summary.is_empty() {
        anyhow::bail!("Haiku returned empty summary");
    }

    let (title, summary_body, mut entities) = parse_summary_output(&raw_summary);

    // Extract themes: use entities that look like topic labels (not specific IDs)
    // For MoA's categories (legal, coding), themes are typically action verbs
    // or subject matter labels. We derive them from entity analysis.
    let themes = extract_themes(&entities, &title, category);

    // Deduplicate entities (themes may overlap with entities)
    entities.sort();
    entities.dedup();

    let content_id = store.store(
        &title,
        &summary_body,
        content,
        category,
        &entities,
        source_type,
    )?;

    tracing::info!(
        content_id,
        char_count,
        title = title.as_str(),
        category,
        themes_count = themes.len(),
        "Document summarized via Haiku and stored"
    );

    Ok(SummarizedDocument {
        content_id,
        title,
        summary: summary_body,
        entities,
        themes,
        char_count,
    })
}

/// Extract themes (topic labels) from entities based on category.
///
/// Themes are coarser than entities — they represent categorizable topics.
/// For legal documents: "차용금반환청구", "물품대금청구", "소장" etc.
/// For coding: "API design", "database migration", "bug fix" etc.
fn extract_themes(entities: &[String], title: &str, category: &str) -> Vec<String> {
    let mut themes = Vec::new();

    // Legal theme indicators (Korean legal terminology)
    let legal_themes = [
        "차용금", "물품대금", "대여금", "매매계약", "임대차",
        "손해배상", "부당이득", "양수금", "구상금", "공사대금",
        "소장", "답변서", "준비서면", "판결", "항소", "상고",
        "청구", "반환", "이행", "배상", "해지", "취소",
    ];

    // Coding theme indicators
    let coding_themes = [
        "API", "database", "migration", "refactor", "bug", "fix",
        "feature", "test", "deployment", "security", "performance",
    ];

    let source_text = format!("{} {}", title, entities.join(" "));

    let indicators: &[&str] = match category {
        "legal" | "law" => &legal_themes,
        "code" | "coding" => &coding_themes,
        _ => &[],
    };

    for indicator in indicators {
        if source_text.contains(indicator) && !themes.contains(&indicator.to_string()) {
            themes.push(indicator.to_string());
        }
    }

    // Add entities that look like themes (contain "청구", "계약", etc.)
    for entity in entities {
        if entity.ends_with("청구")
            || entity.ends_with("계약")
            || entity.ends_with("소송")
            || entity.ends_with("심판")
        {
            if !themes.contains(entity) {
                themes.push(entity.clone());
            }
        }
    }

    themes
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn should_summarize_threshold() {
        assert!(!should_summarize("short text"));
        let long = "a".repeat(DOCUMENT_MIN_CHARS - 1);
        assert!(!should_summarize(&long));
        let longer = "a".repeat(DOCUMENT_MIN_CHARS + 1);
        assert!(should_summarize(&longer));
    }

    #[test]
    fn extract_legal_themes() {
        let entities = vec![
            "민법".to_string(),
            "차용금반환청구".to_string(),
            "물품대금청구".to_string(),
            "김철수".to_string(),
        ];
        let themes = extract_themes(&entities, "소장 제출 — 차용금반환청구", "legal");
        assert!(themes.contains(&"차용금".to_string()));
        assert!(themes.contains(&"물품대금".to_string()));
        assert!(themes.contains(&"소장".to_string()));
        // Also includes entity-derived themes
        assert!(themes.contains(&"차용금반환청구".to_string()));
    }

    #[test]
    fn extract_coding_themes() {
        let entities = vec![
            "REST API".to_string(),
            "user authentication".to_string(),
        ];
        let themes = extract_themes(
            &entities,
            "Add JWT authentication to REST API",
            "coding",
        );
        assert!(themes.contains(&"API".to_string()));
    }
}
