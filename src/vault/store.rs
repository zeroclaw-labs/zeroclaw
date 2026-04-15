// @Ref: SUMMARY §4 — VaultStore + ingest_markdown (+ search_fts stub).
//
// Wraps a Connection around brain.db (the same SQLite file first-brain
// uses). Adds v6 vault tables on construction via `schema::init_schema`.
// All mutations are idempotent via checksum / UUID uniqueness.

use super::ingest::{IngestInput, IngestOutput, SourceType};
use super::schema;
use super::wikilink::{AIEngine, HeuristicAIEngine, WikilinkPipeline};
use crate::memory::sync::SyncEngine;
use anyhow::Result;
use parking_lot::Mutex;
use rusqlite::{params, Connection};
use sha2::{Digest, Sha256};
use std::collections::HashMap;
use std::sync::Arc;

pub const DOCUMENT_MIN_CHARS: usize = super::DOCUMENT_MIN_CHARS;

/// Second-brain document store.
///
/// Owns a shared SQLite connection with first-brain (through an `Arc<Mutex<Connection>>`)
/// so cross-brain JOINs are possible and sync can atomically span both tables.
pub struct VaultStore {
    conn: Arc<Mutex<Connection>>,
    sync: Mutex<Option<Arc<Mutex<SyncEngine>>>>,
    ai: Arc<dyn AIEngine>,
}

impl VaultStore {
    /// Build a vault over an existing shared connection (e.g. the one owned
    /// by `SqliteMemory`). Idempotent — running `init_schema` multiple times
    /// is safe.
    pub fn with_shared_connection(conn: Arc<Mutex<Connection>>) -> Result<Self> {
        {
            let c = conn.lock();
            schema::init_schema(&c)?;
        }
        Ok(Self {
            conn,
            sync: Mutex::new(None),
            ai: Arc::new(HeuristicAIEngine),
        })
    }

    /// Swap in a different AI engine (e.g. LLM-backed in production).
    pub fn with_ai_engine(mut self, engine: Arc<dyn AIEngine>) -> Self {
        self.ai = engine;
        self
    }

    /// Attach a sync engine so vault mutations push `VaultDocUpsert` deltas.
    pub fn attach_sync(&self, engine: Arc<Mutex<SyncEngine>>) {
        *self.sync.lock() = Some(engine);
    }

    fn with_sync<F: FnOnce(&mut SyncEngine)>(&self, f: F) {
        let guard = self.sync.lock();
        if let Some(arc) = guard.as_ref().cloned() {
            drop(guard);
            let mut eng = arc.lock();
            f(&mut eng);
        }
    }

    /// Core ingest path. Runs the full 7-step pipeline and writes rows
    /// for document, links, frontmatter, tags, aliases. Returns early with
    /// `already_present=true` if the checksum is already in the vault.
    pub async fn ingest_markdown(&self, input: IngestInput<'_>) -> Result<IngestOutput> {
        // §4.0 guard: chat_paste < threshold is rejected (caller should have
        // filtered, but we double-check).
        let char_count = input.markdown.chars().count();
        if input.source_type == SourceType::ChatPaste
            && char_count < DOCUMENT_MIN_CHARS
        {
            anyhow::bail!(
                "chat_paste below {DOCUMENT_MIN_CHARS}-char threshold (got {char_count}); not ingesting"
            );
        }

        // §4.1 checksum
        let checksum = hex::encode(Sha256::digest(input.markdown.as_bytes()));

        // §4.2 existing check
        if let Some(existing) = self.lookup_by_checksum(&checksum)? {
            return Ok(IngestOutput {
                vault_doc_id: existing.0,
                uuid: existing.1,
                already_present: true,
                link_count: 0,
                keywords: vec![],
            });
        }

        // §4.3 frontmatter parse
        let (frontmatter, body) = parse_frontmatter(input.markdown);

        // §4.4 pipeline run
        let pipeline = WikilinkPipeline::new(&*self.ai, &self.conn, input.domain);
        let out = pipeline.run(body).await?;

        // §4.5 write rows
        let uuid = uuid::Uuid::new_v4().to_string();
        let doc_id = {
            let conn = self.conn.lock();
            let now = unix_epoch();
            conn.execute(
                "INSERT INTO vault_documents
                    (uuid, title, content, html_content, source_type, source_device_id,
                     original_path, checksum, doc_type, char_count, created_at, updated_at)
                 VALUES (?1, ?2, ?3, ?4, ?5, ?6, ?7, ?8, ?9, ?10, ?11, ?11)",
                params![
                    uuid,
                    input.title,
                    out.annotated_content,
                    input.html_content,
                    input.source_type.as_str(),
                    input.source_device_id,
                    input.original_path,
                    checksum,
                    input.doc_type,
                    char_count as i64,
                    now as i64,
                ],
            )?;
            let id = conn.last_insert_rowid();

            // Frontmatter rows.
            for (k, v) in &frontmatter {
                conn.execute(
                    "INSERT OR IGNORE INTO vault_frontmatter (doc_id, key, value)
                     VALUES (?1, ?2, ?3)",
                    params![id, k, v],
                )?;
            }

            // Tags (§2.3): from frontmatter key "tags" (comma-separated) if present.
            if let Some(tag_list) = frontmatter.get("tags") {
                for t in tag_list.split(',').map(str::trim).filter(|t| !t.is_empty()) {
                    conn.execute(
                        "INSERT OR IGNORE INTO vault_tags (doc_id, tag_name, tag_type)
                         VALUES (?1, ?2, 'frontmatter')",
                        params![id, t],
                    )?;
                }
            }

            // Aliases (from frontmatter `aliases`, comma-separated).
            if let Some(alias_list) = frontmatter.get("aliases") {
                for a in alias_list.split(',').map(str::trim).filter(|a| !a.is_empty()) {
                    conn.execute(
                        "INSERT OR IGNORE INTO vault_aliases (doc_id, alias)
                         VALUES (?1, ?2)",
                        params![id, a],
                    )?;
                }
            }

            // Links.
            for link in &out.links {
                let target_doc_id: Option<i64> = conn
                    .query_row(
                        "SELECT id FROM vault_documents WHERE title = ?1",
                        params![link.target_raw],
                        |r| r.get::<_, i64>(0),
                    )
                    .ok();
                conn.execute(
                    "INSERT INTO vault_links
                        (source_doc_id, target_raw, target_doc_id, display_text,
                         link_type, context, line_number, is_resolved)
                     VALUES (?1, ?2, ?3, ?4, ?5, ?6, ?7, ?8)",
                    params![
                        id,
                        link.target_raw,
                        target_doc_id,
                        link.display_text,
                        link.link_type,
                        link.context,
                        link.line_number,
                        target_doc_id.is_some() as i32,
                    ],
                )?;
            }
            id
        };

        // §4.6 sync delta
        let links_json = serde_json::to_string(
            &out.links
                .iter()
                .map(|l| {
                    serde_json::json!({
                        "target": l.target_raw,
                        "display": l.display_text,
                        "type": l.link_type,
                    })
                })
                .collect::<Vec<_>>(),
        )
        .ok();
        let fm_json = if frontmatter.is_empty() {
            None
        } else {
            serde_json::to_string(&frontmatter).ok()
        };
        let content_sha256 = hex::encode(Sha256::digest(out.annotated_content.as_bytes()));
        let source_type_str = input.source_type.as_str().to_string();
        let title_owned = input.title.map(String::from);
        let checksum_owned = checksum.clone();
        let uuid_for_sync = uuid.clone();

        self.with_sync(move |engine| {
            engine.record_vault_doc_upsert(
                &uuid_for_sync,
                &source_type_str,
                title_owned.as_deref(),
                &checksum_owned,
                &content_sha256,
                fm_json.as_deref(),
                links_json.as_deref(),
            );
        });

        Ok(IngestOutput {
            vault_doc_id: doc_id,
            uuid,
            already_present: false,
            link_count: out.links.len(),
            keywords: out.keywords,
        })
    }

    /// FTS5 keyword search over `vault_documents`.
    /// Returns (doc_id, title, snippet_score).
    pub fn search_fts(&self, query: &str, limit: usize) -> Result<Vec<(i64, String, f32)>> {
        let conn = self.conn.lock();
        let mut stmt = conn.prepare(
            "SELECT d.id, COALESCE(d.title, ''), bm25(vault_docs_fts)
             FROM vault_docs_fts f JOIN vault_documents d ON d.id = f.rowid
             WHERE vault_docs_fts MATCH ?1
             ORDER BY bm25(vault_docs_fts)
             LIMIT ?2",
        )?;
        let rows = stmt.query_map(params![fts_escape(query), limit as i64], |r| {
            Ok((r.get::<_, i64>(0)?, r.get::<_, String>(1)?, r.get::<_, f64>(2)? as f32))
        })?;
        let mut result = Vec::new();
        for row in rows {
            let (id, title, bm25) = row?;
            // FTS5 returns lower = better; invert for consistency with vector scores.
            let score = 1.0 / (1.0 + bm25);
            result.push((id, title, score));
        }
        Ok(result)
    }

    /// Count total vault documents.
    pub fn doc_count(&self) -> Result<i64> {
        let conn = self.conn.lock();
        Ok(conn.query_row("SELECT COUNT(*) FROM vault_documents", [], |r| r.get(0))?)
    }

    fn lookup_by_checksum(&self, checksum: &str) -> Result<Option<(i64, String)>> {
        let conn = self.conn.lock();
        let result = conn.query_row(
            "SELECT id, uuid FROM vault_documents WHERE checksum = ?1",
            params![checksum],
            |r| Ok((r.get::<_, i64>(0)?, r.get::<_, String>(1)?)),
        );
        match result {
            Ok(v) => Ok(Some(v)),
            Err(rusqlite::Error::QueryReturnedNoRows) => Ok(None),
            Err(e) => Err(e.into()),
        }
    }

    /// Expose connection for integration tests / unified_search.
    pub fn connection(&self) -> &Arc<Mutex<Connection>> {
        &self.conn
    }
}

// ── helpers ────────────────────────────────────────────────────────

fn unix_epoch() -> u64 {
    std::time::SystemTime::now()
        .duration_since(std::time::UNIX_EPOCH)
        .unwrap_or_default()
        .as_secs()
}

/// Parse a front-matter YAML block (`--- … ---`) at the start of the
/// markdown. Returns (key_value_map, remaining_body). Falls back to
/// (empty, full_text) if no front-matter.
fn parse_frontmatter(markdown: &str) -> (HashMap<String, String>, &str) {
    let trimmed = markdown.trim_start_matches(['\u{feff}']);
    if let Some(body) = trimmed.strip_prefix("---\n") {
        if let Some(end) = body.find("\n---\n") {
            let (fm_block, rest) = body.split_at(end);
            let mut map = HashMap::new();
            for line in fm_block.lines() {
                if let Some((k, v)) = line.split_once(':') {
                    let key = k.trim();
                    let val = v.trim().trim_matches('"').trim_matches('\'');
                    if !key.is_empty() {
                        map.insert(key.to_string(), val.to_string());
                    }
                }
            }
            // rest starts with "\n---\n"; skip the closing delimiter.
            return (map, rest.trim_start_matches("\n---\n"));
        }
    }
    (HashMap::new(), markdown)
}

/// Escape an FTS5 query string by wrapping non-alphanumeric tokens in
/// double quotes. Prevents syntax errors on punctuation-heavy Korean
/// legal text.
fn fts_escape(q: &str) -> String {
    let tokens: Vec<String> = q
        .split_whitespace()
        .filter(|t| !t.is_empty())
        .map(|t| {
            if t.chars().all(|c| c.is_alphanumeric()) {
                t.to_string()
            } else {
                format!("\"{}\"", t.replace('"', "\"\""))
            }
        })
        .collect();
    if tokens.is_empty() {
        q.to_string()
    } else {
        tokens.join(" OR ")
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    async fn mem_store() -> (VaultStore, Arc<Mutex<Connection>>) {
        let conn = Arc::new(Mutex::new(Connection::open_in_memory().unwrap()));
        let store = VaultStore::with_shared_connection(conn.clone()).unwrap();
        (store, conn)
    }

    #[tokio::test]
    async fn ingest_short_chat_paste_rejected() {
        let (store, _conn) = mem_store().await;
        let err = store
            .ingest_markdown(IngestInput {
                source_type: SourceType::ChatPaste,
                source_device_id: "dev",
                original_path: None,
                title: None,
                markdown: "짧은 텍스트",
                html_content: None,
                doc_type: None,
                domain: "legal",
            })
            .await
            .unwrap_err();
        assert!(err.to_string().contains("threshold"));
    }

    #[tokio::test]
    async fn ingest_and_idempotency() {
        let (store, _conn) = mem_store().await;
        let md = sample_doc();
        let a = store
            .ingest_markdown(IngestInput {
                source_type: SourceType::LocalFile,
                source_device_id: "dev_a",
                original_path: Some("/x.hwp"),
                title: Some("민법 제750조 해설"),
                markdown: &md,
                html_content: None,
                doc_type: Some("analysis"),
                domain: "legal",
            })
            .await
            .unwrap();
        assert!(!a.already_present);
        assert!(a.link_count >= 1);

        // Re-ingest same checksum
        let b = store
            .ingest_markdown(IngestInput {
                source_type: SourceType::LocalFile,
                source_device_id: "dev_a",
                original_path: Some("/x.hwp"),
                title: Some("민법 제750조 해설"),
                markdown: &md,
                html_content: None,
                doc_type: Some("analysis"),
                domain: "legal",
            })
            .await
            .unwrap();
        assert!(b.already_present);
        assert_eq!(a.vault_doc_id, b.vault_doc_id);
    }

    #[tokio::test]
    async fn fts_finds_wikilinked_content() {
        let (store, _conn) = mem_store().await;
        store
            .ingest_markdown(IngestInput {
                source_type: SourceType::LocalFile,
                source_device_id: "dev",
                original_path: None,
                title: Some("분석"),
                markdown: &sample_doc(),
                html_content: None,
                doc_type: None,
                domain: "legal",
            })
            .await
            .unwrap();
        let hits = store.search_fts("민법", 10).unwrap();
        assert!(!hits.is_empty());
    }

    #[tokio::test]
    async fn frontmatter_tags_and_aliases_stored() {
        let (store, conn) = mem_store().await;
        let md = "---\ntitle: 테스트\ntags: 민사, 손해배상\naliases: 750조, 민750\n---\n# 민법 제750조\n\n내용이 길고 의미 있게 작성되어야 한다. 민법 제750조는 불법행위를 규정한다.\n".to_string() + &"x".repeat(500);
        store
            .ingest_markdown(IngestInput {
                source_type: SourceType::LocalFile,
                source_device_id: "dev",
                original_path: None,
                title: Some("민법 제750조"),
                markdown: &md,
                html_content: None,
                doc_type: None,
                domain: "legal",
            })
            .await
            .unwrap();
        let tag_count: i64 = {
            let c = conn.lock();
            c.query_row("SELECT COUNT(*) FROM vault_tags", [], |r| r.get(0))
                .unwrap()
        };
        assert_eq!(tag_count, 2);
        let alias_count: i64 = {
            let c = conn.lock();
            c.query_row("SELECT COUNT(*) FROM vault_aliases", [], |r| r.get(0))
                .unwrap()
        };
        assert_eq!(alias_count, 2);
    }

    fn sample_doc() -> String {
        format!(
            "# 민법 제750조 해설\n\n이 문서는 민법 제750조(불법행위의 내용)에 관한 \
해설이다. 민법 제750조는 고의 또는 과실로 인한 위법행위에 대한 손해배상 책임을 규정한다. \
참조: 대법원 2026. 2. 2. 선고 2025다12345 판결.\n\n{body}",
            body = "이 규정은 근대 민법의 핵심 조항이다. ".repeat(40)
        )
    }
}
