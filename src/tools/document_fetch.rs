//! Document fetch tool — retrieves original document content from the LLM Wiki.
//!
//! When memory recall returns a document summary with a `content_id`, the LLM
//! can call this tool to fetch the full original text — but ONLY if truly
//! needed. This is the core of the LLM Wiki pattern: summaries first, full
//! content on demand.

use super::traits::{Tool, ToolResult};
use crate::memory::document_store::DocumentStore;
use async_trait::async_trait;
use serde_json::json;
use std::path::PathBuf;
use std::sync::Arc;

/// Tool that returns the full original content of a stored document
/// identified by its `content_id`.
pub struct DocumentFetchTool {
    workspace_dir: Arc<PathBuf>,
}

impl DocumentFetchTool {
    pub fn new(workspace_dir: Arc<PathBuf>) -> Self {
        Self { workspace_dir }
    }

    fn store(&self) -> anyhow::Result<DocumentStore> {
        let db_path = self.workspace_dir.join("memory").join("brain.db");
        DocumentStore::open(&db_path)
    }
}

#[async_trait]
impl Tool for DocumentFetchTool {
    fn name(&self) -> &str {
        "document_fetch"
    }

    fn description(&self) -> &str {
        "Fetch the full original text of a document from the LLM Wiki by content_id. \
         Use this ONLY when the summary returned by memory_recall or document_search \
         is insufficient and you need the complete original content. \
         Token-expensive — prefer searching summaries first."
    }

    fn parameters_schema(&self) -> serde_json::Value {
        json!({
            "type": "object",
            "properties": {
                "content_id": {
                    "type": "string",
                    "description": "The document content_id (e.g. 'doc_abc123') returned by a previous search"
                }
            },
            "required": ["content_id"]
        })
    }

    async fn execute(&self, args: serde_json::Value) -> anyhow::Result<ToolResult> {
        let content_id = args
            .get("content_id")
            .and_then(|v| v.as_str())
            .ok_or_else(|| anyhow::anyhow!("content_id가 필요합니다."))?;

        let store = match self.store() {
            Ok(s) => s,
            Err(e) => {
                return Ok(ToolResult {
                    success: false,
                    output: String::new(),
                    error: Some(format!("문서 저장소를 열 수 없습니다: {e}")),
                });
            }
        };

        match store.fetch(content_id) {
            Ok(Some(doc)) => {
                let output = format!(
                    "📄 문서 원문 (ID: {content_id})\n\
                     제목: {title}\n\
                     분류: {category}\n\
                     원문 길이: {char_count}자\n\
                     엔티티: {entities}\n\
                     생성일: {created_at}\n\
                     \n\
                     === 원문 시작 ===\n\
                     {content}\n\
                     === 원문 끝 ===",
                    title = doc.title,
                    category = doc.category,
                    char_count = doc.char_count,
                    entities = doc.entities.join(", "),
                    created_at = doc.created_at,
                    content = doc.content,
                );
                Ok(ToolResult {
                    success: true,
                    output,
                    error: None,
                })
            }
            Ok(None) => Ok(ToolResult {
                success: false,
                output: String::new(),
                error: Some(format!(
                    "content_id '{content_id}'에 해당하는 문서를 찾을 수 없습니다."
                )),
            }),
            Err(e) => Ok(ToolResult {
                success: false,
                output: String::new(),
                error: Some(format!("문서 조회 중 문제: {e}")),
            }),
        }
    }
}

/// Tool that searches the document store by keyword, returning summaries only.
///
/// This is the preferred search path for the LLM Wiki pattern — cheap context,
/// fast lookup. If the summary is insufficient, the LLM calls `document_fetch`.
pub struct DocumentSearchTool {
    workspace_dir: Arc<PathBuf>,
}

impl DocumentSearchTool {
    pub fn new(workspace_dir: Arc<PathBuf>) -> Self {
        Self { workspace_dir }
    }

    fn store(&self) -> anyhow::Result<DocumentStore> {
        let db_path = self.workspace_dir.join("memory").join("brain.db");
        DocumentStore::open(&db_path)
    }
}

#[async_trait]
impl Tool for DocumentSearchTool {
    fn name(&self) -> &str {
        "document_search"
    }

    fn description(&self) -> &str {
        "Search the LLM Wiki document store for relevant documents. Returns SUMMARIES only \
         (not full content) to save context. Each result includes a content_id that can be \
         passed to document_fetch if the full original is needed. \
         Useful for: finding relevant legal cases, prior meeting notes, coding docs, uploaded files."
    }

    fn parameters_schema(&self) -> serde_json::Value {
        json!({
            "type": "object",
            "properties": {
                "query": {
                    "type": "string",
                    "description": "Keywords to search for (matches title, summary, entities)"
                },
                "category": {
                    "type": "string",
                    "description": "Optional category filter: legal, coding, meeting, document, general"
                },
                "limit": {
                    "type": "integer",
                    "description": "Max results (default: 5)"
                }
            },
            "required": ["query"]
        })
    }

    async fn execute(&self, args: serde_json::Value) -> anyhow::Result<ToolResult> {
        let query = args
            .get("query")
            .and_then(|v| v.as_str())
            .ok_or_else(|| anyhow::anyhow!("query가 필요합니다."))?;

        #[allow(clippy::cast_possible_truncation)]
        let limit = args
            .get("limit")
            .and_then(serde_json::Value::as_u64)
            .map_or(5, |v| v as usize);

        let category = args.get("category").and_then(|v| v.as_str());

        let store = match self.store() {
            Ok(s) => s,
            Err(e) => {
                return Ok(ToolResult {
                    success: false,
                    output: String::new(),
                    error: Some(format!("문서 저장소를 열 수 없습니다: {e}")),
                });
            }
        };

        match store.search(query, limit, category) {
            Ok(hits) if hits.is_empty() => Ok(ToolResult {
                success: true,
                output: "관련 문서를 찾지 못했습니다.".into(),
                error: None,
            }),
            Ok(hits) => {
                let mut output = format!("📚 LLM Wiki에서 {}개 문서 발견:\n\n", hits.len());
                for (i, hit) in hits.iter().enumerate() {
                    output.push_str(&format!(
                        "[{idx}] {title}\n\
                         ID: {id}\n\
                         분류: {cat}\n\
                         원문 길이: {chars}자\n\
                         엔티티: {entities}\n\
                         \n\
                         {summary}\n\
                         \n\
                         ---\n\n",
                        idx = i + 1,
                        title = hit.title,
                        id = hit.content_id,
                        cat = hit.category,
                        chars = hit.char_count,
                        entities = hit.entities.join(", "),
                        summary = hit.summary,
                    ));
                }
                output.push_str(
                    "💡 원문이 필요한 문서가 있다면 `document_fetch`를 `content_id`로 호출하세요.",
                );
                Ok(ToolResult {
                    success: true,
                    output,
                    error: None,
                })
            }
            Err(e) => Ok(ToolResult {
                success: false,
                output: String::new(),
                error: Some(format!("문서 검색 중 문제: {e}")),
            }),
        }
    }
}
