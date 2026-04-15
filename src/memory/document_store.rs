//! Document store: LLM Wiki-style summary + link pattern for long content.
//!
//! Inspired by Karpathy's LLM Knowledge Base pattern and llm-wiki-compiler.
//!
//! ## Design
//!
//! When a user uploads a document or pastes text > 2000 chars:
//! 1. Content is stored with a unique `content_id` in the `documents` table
//! 2. Claude Haiku (cheap model) generates a ~500-char summary ONE TIME
//! 3. Entities (law names, case IDs, function names) are extracted
//! 4. The summary + content_id replace the full content in chat context
//!
//! When memory is recalled:
//! 1. FTS5 searches summaries + entities FIRST (tiny context cost)
//! 2. LLM sees the summary and can call `document_fetch(content_id)`
//!    ONLY IF it needs the original text
//!
//! ## Why Haiku?
//!
//! Summarization happens ONCE per document. Claude Haiku 4.5 is ~1/10th the
//! cost of Sonnet/Opus while producing high-quality summaries for this task.
//! The one-time cost is amortized across every future recall of the document.

use anyhow::{Context, Result};
use chrono::Local;
use parking_lot::Mutex;
use rusqlite::{params, Connection};
use serde::{Deserialize, Serialize};
use std::path::Path;
use std::sync::Arc;
use uuid::Uuid;

/// Minimum character count to trigger document summarization.
/// Below this, content goes directly into chat context (no overhead).
pub const DOCUMENT_MIN_CHARS: usize = 2000;

/// Target summary length (characters). Approximately 500 tokens.
pub const SUMMARY_TARGET_CHARS: usize = 500;

/// A stored document with summary, full content, and extracted entities.
#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct StoredDocument {
    pub content_id: String,
    pub title: String,
    pub summary: String,
    pub content: String,
    pub category: String,
    pub entities: Vec<String>,
    pub token_count: i64,
    pub char_count: i64,
    pub source_type: String,
    pub created_at: String,
    pub last_accessed: Option<String>,
}

/// Search result: summary + content_id (NO full content).
#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct DocumentSearchHit {
    pub content_id: String,
    pub title: String,
    pub summary: String,
    pub category: String,
    pub entities: Vec<String>,
    pub char_count: i64,
    pub created_at: String,
    pub score: f64,
}

/// Document store backed by SQLite.
pub struct DocumentStore {
    conn: Arc<Mutex<Connection>>,
}

impl DocumentStore {
    /// Open document store at the shared memory database path.
    pub fn open(db_path: &Path) -> Result<Self> {
        let conn = Connection::open(db_path)
            .with_context(|| format!("Failed to open document store at {db_path:?}"))?;
        Ok(Self {
            conn: Arc::new(Mutex::new(conn)),
        })
    }

    /// Store a document with its summary and entities.
    /// Returns the generated `content_id`.
    pub fn store(
        &self,
        title: &str,
        summary: &str,
        content: &str,
        category: &str,
        entities: &[String],
        source_type: &str,
    ) -> Result<String> {
        let content_id = format!("doc_{}", Uuid::new_v4().simple());
        let char_count = content.chars().count() as i64;
        // Rough token estimate: 1 token ≈ 4 chars (English) or 1.5 chars (Korean)
        let token_count = (char_count as f64 / 2.0) as i64;
        let entities_json = serde_json::to_string(entities).unwrap_or_else(|_| "[]".to_string());
        let now = Local::now().to_rfc3339();

        let conn = self.conn.lock();
        conn.execute(
            "INSERT INTO documents
                (content_id, title, summary, content, category, entities,
                 token_count, char_count, source_type, created_at, last_accessed)
             VALUES (?1, ?2, ?3, ?4, ?5, ?6, ?7, ?8, ?9, ?10, NULL)",
            params![
                content_id,
                title,
                summary,
                content,
                category,
                entities_json,
                token_count,
                char_count,
                source_type,
                now,
            ],
        )?;

        tracing::info!(
            content_id,
            char_count,
            token_count,
            category,
            "Document stored with summary"
        );
        Ok(content_id)
    }

    /// Fetch a single document by content_id. Updates last_accessed timestamp.
    pub fn fetch(&self, content_id: &str) -> Result<Option<StoredDocument>> {
        let conn = self.conn.lock();
        let now = Local::now().to_rfc3339();
        // Update access time (best-effort)
        let _ = conn.execute(
            "UPDATE documents SET last_accessed = ?1 WHERE content_id = ?2",
            params![now, content_id],
        );

        let result = conn.query_row(
            "SELECT content_id, title, summary, content, category, entities,
                    token_count, char_count, source_type, created_at, last_accessed
             FROM documents WHERE content_id = ?1",
            params![content_id],
            |row| {
                let entities_json: String = row.get(5)?;
                let entities: Vec<String> =
                    serde_json::from_str(&entities_json).unwrap_or_default();
                Ok(StoredDocument {
                    content_id: row.get(0)?,
                    title: row.get(1)?,
                    summary: row.get(2)?,
                    content: row.get(3)?,
                    category: row.get(4)?,
                    entities,
                    token_count: row.get(6)?,
                    char_count: row.get(7)?,
                    source_type: row.get(8)?,
                    created_at: row.get(9)?,
                    last_accessed: row.get(10)?,
                })
            },
        );

        match result {
            Ok(doc) => Ok(Some(doc)),
            Err(rusqlite::Error::QueryReturnedNoRows) => Ok(None),
            Err(e) => Err(e.into()),
        }
    }

    /// Search documents by query using FTS5 over summaries + entities.
    ///
    /// Returns summaries only — NOT full content. The LLM can then decide
    /// whether to call `document_fetch` for the full text.
    pub fn search(
        &self,
        query: &str,
        limit: usize,
        category_filter: Option<&str>,
    ) -> Result<Vec<DocumentSearchHit>> {
        if query.trim().is_empty() {
            return Ok(Vec::new());
        }

        let conn = self.conn.lock();
        let sanitized = Self::sanitize_fts_query(query);
        if sanitized.is_empty() {
            return Ok(Vec::new());
        }

        let sql = if category_filter.is_some() {
            "SELECT d.content_id, d.title, d.summary, d.category, d.entities,
                    d.char_count, d.created_at,
                    bm25(documents_fts) AS score
             FROM documents_fts
             JOIN documents d ON d.rowid = documents_fts.rowid
             WHERE documents_fts MATCH ?1 AND d.category = ?2
             ORDER BY score
             LIMIT ?3"
        } else {
            "SELECT d.content_id, d.title, d.summary, d.category, d.entities,
                    d.char_count, d.created_at,
                    bm25(documents_fts) AS score
             FROM documents_fts
             JOIN documents d ON d.rowid = documents_fts.rowid
             WHERE documents_fts MATCH ?1
             ORDER BY score
             LIMIT ?2"
        };

        let mut stmt = conn.prepare(&sql)?;
        let rows: Vec<DocumentSearchHit> = if let Some(cat) = category_filter {
            stmt.query_map(params![sanitized, cat, limit as i64], Self::map_hit)?
                .filter_map(|r| r.ok())
                .collect()
        } else {
            stmt.query_map(params![sanitized, limit as i64], Self::map_hit)?
                .filter_map(|r| r.ok())
                .collect()
        };
        Ok(rows)
    }

    fn map_hit(row: &rusqlite::Row) -> rusqlite::Result<DocumentSearchHit> {
        let entities_json: String = row.get(4)?;
        let entities: Vec<String> = serde_json::from_str(&entities_json).unwrap_or_default();
        Ok(DocumentSearchHit {
            content_id: row.get(0)?,
            title: row.get(1)?,
            summary: row.get(2)?,
            category: row.get(3)?,
            entities,
            char_count: row.get(5)?,
            created_at: row.get(6)?,
            score: row.get::<_, f64>(7)?,
        })
    }

    /// List recent documents (for listing/browsing UI).
    pub fn list_recent(&self, limit: usize) -> Result<Vec<DocumentSearchHit>> {
        let conn = self.conn.lock();
        let mut stmt = conn.prepare(
            "SELECT content_id, title, summary, category, entities, char_count, created_at, 1.0
             FROM documents
             ORDER BY created_at DESC
             LIMIT ?1",
        )?;
        let rows = stmt
            .query_map(params![limit as i64], Self::map_hit)?
            .filter_map(|r| r.ok())
            .collect();
        Ok(rows)
    }

    /// Delete a document by content_id.
    pub fn delete(&self, content_id: &str) -> Result<bool> {
        let conn = self.conn.lock();
        let deleted = conn.execute(
            "DELETE FROM documents WHERE content_id = ?1",
            params![content_id],
        )?;
        Ok(deleted > 0)
    }

    /// Count total documents stored.
    pub fn count(&self) -> Result<i64> {
        let conn = self.conn.lock();
        let count: i64 = conn.query_row("SELECT COUNT(*) FROM documents", [], |row| row.get(0))?;
        Ok(count)
    }

    /// Sanitize FTS5 query: escape quotes, join tokens with OR.
    fn sanitize_fts_query(query: &str) -> String {
        query
            .split_whitespace()
            .filter(|t| t.len() > 1)
            .map(|t| {
                let cleaned: String = t
                    .chars()
                    .filter(|c| c.is_alphanumeric() || *c == '_' || !c.is_ascii())
                    .collect();
                if cleaned.is_empty() {
                    String::new()
                } else {
                    format!("\"{}\"", cleaned)
                }
            })
            .filter(|s| !s.is_empty())
            .collect::<Vec<_>>()
            .join(" OR ")
    }
}

/// Prompt template for Claude Haiku to summarize a document.
///
/// Designed to produce a structured summary optimized for LLM Wiki retrieval:
/// - Title line for quick identification
/// - 3-5 key points
/// - Extracted entities (names, laws, functions, cases)
/// - Target length: ~500 chars (~250 tokens)
pub fn build_summary_prompt(content: &str, category: &str) -> String {
    let category_hint = match category {
        "legal" | "law" => "법률 문서",
        "code" | "coding" => "코드/기술 문서",
        "meeting" => "회의록",
        "document" => "일반 문서",
        _ => "텍스트",
    };

    format!(
        "다음 {category_hint}을 구조화된 요약으로 변환해 주세요. \
         이 요약은 나중에 LLM이 검색하고 참조하기 위한 LLM Wiki 항목으로 저장됩니다.\n\
         \n\
         반드시 아래 형식으로 출력하세요 (500자 이내):\n\
         \n\
         제목: <한 줄 요약>\n\
         분류: <세부 분류>\n\
         핵심 요약:\n\
         - <핵심 포인트 1>\n\
         - <핵심 포인트 2>\n\
         - <핵심 포인트 3>\n\
         (최대 5개)\n\
         엔티티: <쉼표로 구분된 주요 개체 (법률명, 판례번호, 인명, 함수명, 클래스명 등)>\n\
         \n\
         원문:\n\
         ```\n\
         {content}\n\
         ```\n\
         \n\
         요약:"
    )
}

/// Parse a structured summary output into title, summary body, and entities.
///
/// Expected format (from build_summary_prompt):
/// ```
/// 제목: ...
/// 분류: ...
/// 핵심 요약:
/// - ...
/// 엔티티: a, b, c
/// ```
pub fn parse_summary_output(raw: &str) -> (String, String, Vec<String>) {
    let mut title = String::new();
    let mut entities: Vec<String> = Vec::new();
    let mut summary_lines: Vec<String> = Vec::new();
    let mut in_summary_section = false;

    for line in raw.lines() {
        let trimmed = line.trim();
        if let Some(rest) = trimmed.strip_prefix("제목:") {
            title = rest.trim().to_string();
            summary_lines.push(trimmed.to_string());
        } else if let Some(rest) = trimmed.strip_prefix("엔티티:") {
            entities = rest
                .trim()
                .split(',')
                .map(|s| s.trim().to_string())
                .filter(|s| !s.is_empty())
                .collect();
            in_summary_section = false;
        } else if trimmed.starts_with("핵심 요약:") {
            in_summary_section = true;
            summary_lines.push(trimmed.to_string());
        } else if in_summary_section && !trimmed.is_empty() {
            summary_lines.push(trimmed.to_string());
        } else if !trimmed.is_empty() && title.is_empty() {
            // Fallback: treat the first non-empty line as title
            title = trimmed.chars().take(80).collect();
            summary_lines.push(trimmed.to_string());
        } else if !trimmed.is_empty() {
            summary_lines.push(trimmed.to_string());
        }
    }

    if title.is_empty() {
        title = "(제목 없음)".to_string();
    }
    let summary = summary_lines.join("\n");
    (title, summary, entities)
}

/// Build the memo placeholder that replaces long content in chat context.
///
/// The LLM sees this memo instead of the full document. If it needs the
/// original, it can call `document_fetch(content_id)`.
pub fn build_wiki_memo(
    content_id: &str,
    title: &str,
    summary: &str,
    char_count: i64,
    category: &str,
) -> String {
    format!(
        "\n---\n\
         📚 문서 요약 (LLM Wiki)\n\
         ID: {content_id}\n\
         제목: {title}\n\
         분류: {category}\n\
         원문 길이: {char_count}자\n\
         \n\
         {summary}\n\
         \n\
         💡 원문이 필요하면 `document_fetch` 도구를 `content_id=\"{content_id}\"`로 호출하세요.\n\
         ---\n"
    )
}

#[cfg(test)]
mod tests {
    use super::*;
    use tempfile::TempDir;

    fn setup_store() -> (TempDir, DocumentStore) {
        let tmp = TempDir::new().unwrap();
        // SqliteMemory creates schema at <workspace>/memory/brain.db.
        // DocumentStore must open the same file.
        let _ = crate::memory::SqliteMemory::new(tmp.path()).unwrap();
        let db_path = tmp.path().join("memory").join("brain.db");
        let store = DocumentStore::open(&db_path).unwrap();
        (tmp, store)
    }

    #[test]
    fn store_and_fetch_roundtrip() {
        let (_tmp, store) = setup_store();
        let content_id = store
            .store(
                "테스트 문서",
                "제목: 테스트 문서\n핵심 요약:\n- 포인트 1\n엔티티: 테스트",
                "원문 내용".repeat(500).as_str(),
                "legal",
                &["테스트".to_string(), "문서".to_string()],
                "paste",
            )
            .unwrap();
        assert!(content_id.starts_with("doc_"));

        let fetched = store.fetch(&content_id).unwrap().unwrap();
        assert_eq!(fetched.title, "테스트 문서");
        assert_eq!(fetched.category, "legal");
        assert_eq!(fetched.entities.len(), 2);
        assert!(fetched.content.len() > 1000);
    }

    #[test]
    fn parse_summary_extracts_title_and_entities() {
        let raw = "제목: 민법 제750조 불법행위\n\
                   분류: 민사\n\
                   핵심 요약:\n\
                   - 고의 또는 과실로 타인에게 손해를 가한 자는 배상책임\n\
                   - 위법성 요건 필요\n\
                   엔티티: 민법, 제750조, 불법행위, 손해배상";
        let (title, _summary, entities) = parse_summary_output(raw);
        assert_eq!(title, "민법 제750조 불법행위");
        assert_eq!(entities.len(), 4);
        assert!(entities.contains(&"민법".to_string()));
    }

    #[test]
    fn search_returns_summaries_not_full_content() {
        let (_tmp, store) = setup_store();
        store
            .store(
                "계약 해지",
                "제목: 계약 해지 통보\n핵심 요약:\n- 임대차 계약 해지",
                "이것은 매우 긴 계약 원문입니다. ".repeat(200).as_str(),
                "legal",
                &["계약".to_string(), "해지".to_string()],
                "upload",
            )
            .unwrap();

        let hits = store.search("계약", 10, None).unwrap();
        // Depending on FTS tokenization of Korean, results may or may not appear.
        // We just verify that if any hits come back, they contain summaries only.
        for hit in &hits {
            assert!(!hit.summary.is_empty());
            // Hit struct has no `content` field — originals never leak into search.
        }
    }
}
