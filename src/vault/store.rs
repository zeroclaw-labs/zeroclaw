// @Ref: SUMMARY §4 — VaultStore + ingest_markdown (+ search_fts stub).
//
// Wraps a Connection around brain.db (the same SQLite file first-brain
// uses). Adds v6 vault tables on construction via `schema::init_schema`.
// All mutations are idempotent via checksum / UUID uniqueness.

use super::ingest::{IngestInput, IngestOutput, SourceType};
use super::schema;
use super::wikilink::{AIEngine, HeuristicAIEngine, WikilinkPipeline};
use crate::memory::embeddings::{EmbeddingProvider, NoopEmbedding};
use crate::memory::sync::SyncEngine;
use anyhow::Result;
use parking_lot::Mutex;
use rusqlite::{params, Connection};
use sha2::{Digest, Sha256};
use std::collections::{HashMap, HashSet, VecDeque};
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
    /// Embedding provider for vector search. Defaults to Noop (returns
    /// empty vec) so pure-FTS mode still works. Production callers swap
    /// in an OpenAI/custom provider via `with_embedder`.
    embedder: Arc<dyn EmbeddingProvider>,
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
            embedder: Arc::new(NoopEmbedding),
        })
    }

    /// Swap in a different AI engine (e.g. LLM-backed in production).
    pub fn with_ai_engine(mut self, engine: Arc<dyn AIEngine>) -> Self {
        self.ai = engine;
        self
    }

    /// Swap in an embedding provider (OpenAI / custom HTTP / ...).
    /// Required to enable vector search on the vault.
    pub fn with_embedder(mut self, embedder: Arc<dyn EmbeddingProvider>) -> Self {
        self.embedder = embedder;
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
        // §4.0 guard: chat_paste uses a three-tier rule:
        //   char_count >= DOCUMENT_MIN_CHARS (2000) → always ingest (quantitative)
        //   DOCUMENT_QUALITATIVE_MIN_CHARS <= char_count < DOCUMENT_MIN_CHARS →
        //       ingest only if AIEngine classifies the text as knowledge
        //       (qualitative: formal document, citations, multi-sentence
        //       density, not everyday conversation)
        //   char_count < DOCUMENT_QUALITATIVE_MIN_CHARS (200) → reject
        let char_count = input.markdown.chars().count();
        if input.source_type == SourceType::ChatPaste {
            if char_count < super::DOCUMENT_QUALITATIVE_MIN_CHARS {
                anyhow::bail!(
                    "chat_paste below qualitative floor ({} chars, need ≥ {}); not ingesting",
                    char_count,
                    super::DOCUMENT_QUALITATIVE_MIN_CHARS
                );
            }
            if char_count < DOCUMENT_MIN_CHARS {
                // Mid-range: consult the AI engine.
                let verdict = self.ai.classify_as_knowledge(input.markdown).await?;
                if !verdict.is_knowledge {
                    anyhow::bail!(
                        "chat_paste in qualitative range ({char_count} chars) classified as \
non-knowledge (confidence {:.2}): {}",
                        verdict.confidence,
                        verdict.reason
                    );
                }
                tracing::debug!(
                    char_count,
                    confidence = verdict.confidence,
                    reason = %verdict.reason,
                    "chat_paste qualitatively accepted as knowledge"
                );
            }
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

        // §4.5a embedding (computed BEFORE taking the DB lock; async)
        let embedding = match self.embedder.embed_one(body).await {
            Ok(v) if !v.is_empty() => Some(v),
            _ => None,
        };

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

            // Embedding row (if provider produced one).
            if let Some(ref emb) = embedding {
                let bytes = crate::memory::vector::vec_to_bytes(emb);
                conn.execute(
                    "INSERT INTO vault_embeddings (doc_id, embedding, dim)
                     VALUES (?1, ?2, ?3)",
                    params![id, bytes, emb.len() as i64],
                )?;
            }

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
                        i32::from(target_doc_id.is_some()),
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
    /// Query is normalised (fullwidth→halfwidth, whitespace squeeze) and
    /// FTS-escaped before the MATCH clause — see `vault::normalize`.
    pub fn search_fts(&self, query: &str, limit: usize) -> Result<Vec<(i64, String, f32)>> {
        let normalised = super::normalize::normalize_text(query);
        let conn = self.conn.lock();
        let mut stmt = conn.prepare(
            "SELECT d.id, COALESCE(d.title, ''), bm25(vault_docs_fts)
             FROM vault_docs_fts f JOIN vault_documents d ON d.id = f.rowid
             WHERE vault_docs_fts MATCH ?1
             ORDER BY bm25(vault_docs_fts)
             LIMIT ?2",
        )?;
        let rows = stmt.query_map(params![fts_escape(&normalised), limit as i64], |r| {
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

    /// Cosine-similarity vector search. Requires `with_embedder` to have
    /// installed a non-noop provider; otherwise returns empty.
    pub async fn search_vector(
        &self,
        query: &str,
        limit: usize,
    ) -> Result<Vec<(i64, String, f32)>> {
        let q_emb = self.embedder.embed_one(query).await?;
        if q_emb.is_empty() {
            return Ok(Vec::new());
        }
        let conn = self.conn.lock();
        let mut stmt = conn.prepare(
            "SELECT d.id, COALESCE(d.title,''), e.embedding
             FROM vault_embeddings e JOIN vault_documents d ON d.id = e.doc_id",
        )?;
        let rows = stmt.query_map([], |r| {
            Ok((r.get::<_, i64>(0)?, r.get::<_, String>(1)?, r.get::<_, Vec<u8>>(2)?))
        })?;

        let mut scored: Vec<(i64, String, f32)> = Vec::new();
        for row in rows {
            let (id, title, bytes) = row?;
            let emb = crate::memory::vector::bytes_to_vec(&bytes);
            if emb.is_empty() {
                continue;
            }
            let score = crate::memory::vector::cosine_similarity(&q_emb, &emb);
            scored.push((id, title, score));
        }
        scored.sort_by(|a, b| b.2.partial_cmp(&a.2).unwrap_or(std::cmp::Ordering::Equal));
        scored.truncate(limit);
        Ok(scored)
    }

    /// Graph BFS over `vault_links`. Seeds: docs whose title or any
    /// existing wikilink `target_raw` matches `query`. Expands both
    /// inbound (backlinks) and outbound (linked-to) edges up to `depth`.
    ///
    /// Returns (doc_id, title, distance) where distance 0 = seed, 1 =
    /// direct neighbour, etc. Score = 1.0 / (1.0 + distance).
    pub fn search_graph(
        &self,
        query: &str,
        depth: u32,
        limit: usize,
    ) -> Result<Vec<(i64, String, f32)>> {
        if depth == 0 {
            return Ok(Vec::new());
        }
        let conn = self.conn.lock();

        // Seeds: docs whose title matches query exactly, OR docs that
        // have any link targeting a matching `target_raw`.
        let mut seeds: HashSet<i64> = HashSet::new();
        {
            let mut stmt = conn.prepare(
                "SELECT id FROM vault_documents
                 WHERE title = ?1 OR instr(title, ?1) > 0",
            )?;
            for row in stmt.query_map(params![query], |r| r.get::<_, i64>(0))? {
                seeds.insert(row?);
            }
        }
        {
            let mut stmt = conn.prepare(
                "SELECT DISTINCT source_doc_id FROM vault_links
                 WHERE target_raw = ?1 OR instr(target_raw, ?1) > 0",
            )?;
            for row in stmt.query_map(params![query], |r| r.get::<_, i64>(0))? {
                seeds.insert(row?);
            }
            let mut stmt2 = conn.prepare(
                "SELECT DISTINCT target_doc_id FROM vault_links
                 WHERE (target_raw = ?1 OR instr(target_raw, ?1) > 0)
                   AND target_doc_id IS NOT NULL",
            )?;
            for row in stmt2.query_map(params![query], |r| r.get::<_, Option<i64>>(0))? {
                if let Some(id) = row? {
                    seeds.insert(id);
                }
            }
        }

        if seeds.is_empty() {
            return Ok(Vec::new());
        }

        // BFS.
        let mut visited: HashMap<i64, u32> = seeds.iter().map(|id| (*id, 0u32)).collect();
        let mut queue: VecDeque<(i64, u32)> = seeds.iter().map(|id| (*id, 0u32)).collect();
        while let Some((id, dist)) = queue.pop_front() {
            if dist >= depth {
                continue;
            }
            let next_dist = dist + 1;

            // outbound: docs linked FROM this id
            let mut out = conn.prepare(
                "SELECT DISTINCT target_doc_id FROM vault_links
                 WHERE source_doc_id = ?1 AND target_doc_id IS NOT NULL",
            )?;
            for row in out.query_map(params![id], |r| r.get::<_, Option<i64>>(0))? {
                if let Some(next) = row? {
                    visited.entry(next).or_insert(next_dist);
                    if next_dist < depth {
                        queue.push_back((next, next_dist));
                    }
                }
            }
            // inbound: docs that link TO this id
            let mut inb = conn.prepare(
                "SELECT DISTINCT source_doc_id FROM vault_links
                 WHERE target_doc_id = ?1",
            )?;
            for row in inb.query_map(params![id], |r| r.get::<_, i64>(0))? {
                let next = row?;
                visited.entry(next).or_insert(next_dist);
                if next_dist < depth {
                    queue.push_back((next, next_dist));
                }
            }
        }

        // Fetch titles; score = 1/(1+distance).
        let mut result: Vec<(i64, String, f32)> = Vec::with_capacity(visited.len());
        for (id, dist) in visited {
            let title: String = conn
                .query_row(
                    "SELECT COALESCE(title,'') FROM vault_documents WHERE id = ?1",
                    params![id],
                    |r| r.get(0),
                )
                .unwrap_or_default();
            let score = 1.0 / (1.0 + dist as f32);
            result.push((id, title, score));
        }
        result.sort_by(|a, b| b.2.partial_cmp(&a.2).unwrap_or(std::cmp::Ordering::Equal));
        result.truncate(limit);
        Ok(result)
    }

    /// Metadata-filter search over `vault_frontmatter`.
    /// Filters: pairs of (key, value_substring). All must match (AND).
    /// Score = 1.0 (uniform; ordering by recency).
    pub fn search_meta(
        &self,
        filters: &[(String, String)],
        limit: usize,
    ) -> Result<Vec<(i64, String, f32)>> {
        if filters.is_empty() {
            return Ok(Vec::new());
        }
        let conn = self.conn.lock();
        // Build: intersect doc_ids matching each (k,v) filter.
        let mut matching: Option<HashSet<i64>> = None;
        for (k, v) in filters {
            let mut stmt = conn.prepare(
                "SELECT DISTINCT doc_id FROM vault_frontmatter
                 WHERE key = ?1 AND (value = ?2 OR instr(value, ?2) > 0)",
            )?;
            let set: HashSet<i64> = stmt
                .query_map(params![k, v], |r| r.get::<_, i64>(0))?
                .filter_map(|r| r.ok())
                .collect();
            matching = Some(match matching {
                Some(prev) => prev.intersection(&set).copied().collect(),
                None => set,
            });
            if let Some(ref s) = matching {
                if s.is_empty() {
                    return Ok(Vec::new());
                }
            }
        }
        let ids = matching.unwrap_or_default();
        let mut rows: Vec<(i64, String, f32)> = Vec::with_capacity(ids.len());
        for id in ids {
            let title: String = conn
                .query_row(
                    "SELECT COALESCE(title,'') FROM vault_documents WHERE id = ?1",
                    params![id],
                    |r| r.get(0),
                )
                .unwrap_or_default();
            rows.push((id, title, 1.0));
        }
        rows.sort_by_key(|(id, _, _)| std::cmp::Reverse(*id)); // recent first
        rows.truncate(limit);
        Ok(rows)
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

    /// Expose the embedder for vault modules that need to compute
    /// embeddings of auxiliary text (e.g. `health::semantic_tag_clusters`).
    pub fn embedder(&self) -> Arc<dyn EmbeddingProvider> {
        self.embedder.clone()
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

    // Async kept for signature symmetry with other vault test helpers
    // (vault/health.rs, vault/hub.rs, vault/briefing.rs) — avoids churn at
    // every `mem_store().await` call site in this file.
    #[allow(clippy::unused_async)]
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
                markdown: "짧은 텍스트", // < 200 chars — hard floor
                html_content: None,
                doc_type: None,
                domain: "legal",
            })
            .await
            .unwrap_err();
        assert!(err.to_string().contains("qualitative floor"));
    }

    // ── Q1: mid-range qualitative gate ──────────────────────────

    #[tokio::test]
    async fn chat_paste_in_mid_range_rejected_if_chatty() {
        let (store, _conn) = mem_store().await;
        // ~500 chars of pure conversation — no headers, no citations, lots
        // of chat particles. Heuristic default classifies as non-knowledge.
        let chatty = "안녕하세요 변호사님. 오늘 시간 되시나요? 잠시만 여쭤봐도 괜찮을까요? \
고마워요. 저기 저희 일정 조율이 필요해서요. 혹시 오후 3시에 가능한가요? ".repeat(6);
        let err = store
            .ingest_markdown(IngestInput {
                source_type: SourceType::ChatPaste,
                source_device_id: "dev",
                original_path: None,
                title: None,
                markdown: &chatty,
                html_content: None,
                doc_type: None,
                domain: "legal",
            })
            .await
            .unwrap_err();
        assert!(err.to_string().contains("non-knowledge"));
    }

    #[tokio::test]
    async fn chat_paste_in_mid_range_accepted_if_knowledge() {
        let (store, _conn) = mem_store().await;
        // ~500 chars formal note: markdown header + compound-token citation +
        // multi-sentence density → heuristic classifies as knowledge.
        let knowledge = "# 민법 제750조 요건 요약\n\n\
민법 제750조는 불법행위의 일반 조항이다. \
요건은 고의 또는 과실, 위법성, 손해 발생, 인과관계 4가지다. \
대법원 2026. 2. 2. 선고 2025다12345 판결은 네 요건의 개별 입증 책임을 확인했다. \
재산상 손해뿐 아니라 정신상 손해에 대해서도 위자료 청구가 가능하다. \
공동불법행위의 경우 부진정연대채무 관계가 성립한다.".to_string();
        let out = store
            .ingest_markdown(IngestInput {
                source_type: SourceType::ChatPaste,
                source_device_id: "dev",
                original_path: None,
                title: Some("민법 제750조 요약"),
                markdown: &knowledge,
                html_content: None,
                doc_type: None,
                domain: "legal",
            })
            .await
            .unwrap();
        assert!(!out.already_present);
        assert!(out.link_count >= 1);
    }

    #[tokio::test]
    async fn chat_paste_over_2000_still_accepted_without_gate() {
        let (store, _conn) = mem_store().await;
        // Pure conversation but > 2000 chars — still auto-ingested per the
        // quantitative threshold (users explicitly pasted long content).
        let long_chatty = "안녕하세요 반가워요 고마워요. ".repeat(400);
        let out = store
            .ingest_markdown(IngestInput {
                source_type: SourceType::ChatPaste,
                source_device_id: "dev",
                original_path: None,
                title: None,
                markdown: &long_chatty,
                html_content: None,
                doc_type: None,
                domain: "legal",
            })
            .await
            .unwrap();
        assert!(!out.already_present);
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
        // trigram tokenizer needs ≥3-char contiguous substring
        let hits = store.search_fts("불법행", 10).unwrap();
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
